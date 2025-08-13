use crate::audio::{
    decoder::SymphoniaDecoder,
    dsp::{biquad_eq_in_place, update_eq_filters, Filters},
    source::{prepare_local_source, transcode_to_mp3},
    track::{LoopMode, TrackItem},
};
use anyhow::Result;
use bytes::Bytes;
use std::{sync::Arc, time::Duration};
use tokio::sync::{broadcast, Mutex};
use tracing::warn;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct EqBandParam {
    pub band: u8,
    pub gain_db: f32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op")]
pub enum PlayerEvent {
    TrackStart { id: String, uri: String },
    TrackEnd { id: String },
    QueueUpdate,
    LoopModeChange(LoopMode),
}

#[derive(Clone)]
struct PlayerCtrl {
    pause_tx: broadcast::Sender<bool>,
    stop_tx: broadcast::Sender<()>,
    skip_tx: broadcast::Sender<()>,
    filters: Arc<Mutex<Filters>>,
}

pub struct Player {
    id: String,
    uri: String,
    ctrl: PlayerCtrl,
    out_tx: broadcast::Sender<Bytes>,
    metadata: Arc<Mutex<serde_json::Value>>,
    track_info: Arc<Mutex<InternalTrackInfo>>,
    queue: Arc<Mutex<Vec<TrackItem>>>,
    loop_mode: Arc<Mutex<LoopMode>>,
    event_tx: broadcast::Sender<PlayerEvent>,
}

impl Player {
    pub fn new(id: &str, uri: &str) -> Result<Self> {
        let (pause_tx, _) = broadcast::channel(8);
        let (stop_tx, _) = broadcast::channel(1);
        let (skip_tx, _) = broadcast::channel(8);
        let filters = Arc::new(Mutex::new(Filters::default()));
        {
            let mut f = futures::executor::block_on(filters.lock());
            update_eq_filters(&mut f);
        }
        let (out_tx, _) = broadcast::channel(1024);
        let (event_tx, _) = broadcast::channel(128);
        Ok(Self {
            id: id.into(),
            uri: uri.into(),
            ctrl: PlayerCtrl { pause_tx, stop_tx, skip_tx, filters },
            out_tx,
            metadata: Arc::new(Mutex::new(serde_json::json!({}))),
            track_info: Arc::new(Mutex::new(InternalTrackInfo::new(id, uri))),
            queue: Arc::new(Mutex::new(Vec::new())),
            loop_mode: Arc::new(Mutex::new(LoopMode::None)),
            event_tx,
        })
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut current_uri = self.uri.clone();
        'session: loop {
            let source_path = prepare_local_source(&current_uri).await?;
            {
                let mut ti = self.track_info.lock().await;
                ti.title =
                    source_path.file_stem().and_then(|s| s.to_str()).unwrap_or(&current_uri).to_string();
                ti.uri = current_uri.clone();
                ti.identifier = current_uri.clone();
                ti.source_name = if current_uri.starts_with("http") { "http".into() } else { "file".into() };
                ti.position_ms = 0;
            }
            let _ =
                self.event_tx.send(PlayerEvent::TrackStart { id: self.id.clone(), uri: current_uri.clone() });
            let mut decoder = match SymphoniaDecoder::open(&source_path) {
                Ok(d) => d,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("unsupported codec") || msg.contains("unsupported feature") {
                        warn!(%msg, "ffmpeg fallback");
                        let mp3 = transcode_to_mp3(&source_path).await?;
                        SymphoniaDecoder::open(&mp3)?
                    } else {
                        return Err(e);
                    }
                }
            };
            {
                let mut ti = self.track_info.lock().await;
                ti.is_seekable = ti.length_ms > 0;
                ti.is_stream = ti.length_ms == 0;
            }
            const FRAME_SAMPLES: usize = 960;
            const CHANNELS: usize = 2;
            const SAMPLES_PER_FRAME: usize = FRAME_SAMPLES * CHANNELS;
            let mut buf: Vec<i16> = Vec::with_capacity(SAMPLES_PER_FRAME * 8);
            let (mut pause_rx, mut stop_rx, mut skip_rx) = self.ctrl_channels();
            let mut paused = false;
            let mut sent: u64 = 0;
            let mut head = 0usize;
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut eos = false;
            let mut skipped = false;
            loop {
                tick.tick().await;
                if let Ok(_) = skip_rx.try_recv() {
                    skipped = true;
                    break;
                }
                match stop_rx.try_recv() {
                    Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                        break 'session;
                    }
                    Err(_) => {}
                }
                if let Ok(p) = pause_rx.try_recv() {
                    paused = p;
                }
                if paused {
                    continue;
                }
                while buf.len().saturating_sub(head) < SAMPLES_PER_FRAME * 4 && !eos {
                    match decoder.next_pcm_block()? {
                        Some(mut block) => {
                            if block.l.is_empty() {
                                break;
                            }
                            let vol = {
                                let mut f = self.ctrl.filters.lock().await;
                                biquad_eq_in_place(&mut block.l, &mut block.r, &mut *f);
                                f.volume
                            };
                            buf.reserve(block.l.len() * 2);
                            for i in 0..block.l.len() {
                                buf.push((block.l[i] * vol * 32767.0).clamp(-32768.0, 32767.0) as i16);
                                buf.push((block.r[i] * vol * 32767.0).clamp(-32768.0, 32767.0) as i16);
                            }
                        }
                        None => {
                            eos = true;
                        }
                    }
                }
                if buf.len().saturating_sub(head) >= SAMPLES_PER_FRAME {
                    let frame = &buf[head..head + SAMPLES_PER_FRAME];
                    let bytes = bytemuck::cast_slice(frame);
                    let _ = self.out_tx.send(Bytes::copy_from_slice(bytes));
                    sent += 1;
                    head += SAMPLES_PER_FRAME;
                    if sent % 5 == 0 {
                        let mut ti = self.track_info.lock().await;
                        ti.position_ms = sent * 20;
                    }
                    if head >= SAMPLES_PER_FRAME * 8 && head > buf.len() / 2 {
                        buf.drain(0..head);
                        head = 0;
                    }
                } else if eos {
                    break;
                }
            }
            let _ = self.event_tx.send(PlayerEvent::TrackEnd { id: self.id.clone() });
            if let Some(next) = self.next_track_uri(skipped).await {
                current_uri = next;
                continue;
            } else {
                break 'session;
            }
        }
        Ok(())
    }

    fn ctrl_channels(&self) -> (broadcast::Receiver<bool>, broadcast::Receiver<()>, broadcast::Receiver<()>) {
        (self.ctrl.pause_tx.subscribe(), self.ctrl.stop_tx.subscribe(), self.ctrl.skip_tx.subscribe())
    }
    pub fn play(&self) -> Result<()> {
        let _ = self.ctrl.pause_tx.send(false);
        Ok(())
    }
    pub fn pause(&self) -> Result<()> {
        let _ = self.ctrl.pause_tx.send(true);
        Ok(())
    }
    pub fn stop(&self) {
        let _ = self.ctrl.stop_tx.send(());
    }
    pub fn skip(&self) {
        let _ = self.ctrl.skip_tx.send(());
    }
    pub fn set_volume(&self, v: f32) {
        let f = self.ctrl.filters.clone();
        tokio::spawn(async move {
            f.lock().await.volume = v.max(0.0);
        });
    }
    pub fn set_eq(&self, bands: Vec<EqBandParam>) {
        let f = self.ctrl.filters.clone();
        tokio::spawn(async move {
            let mut fl = f.lock().await;
            for b in bands {
                if let Some(slot) = fl.eq.get_mut(b.band as usize) {
                    *slot = b.gain_db;
                }
            }
            update_eq_filters(&mut fl);
        });
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.out_tx.subscribe()
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.event_tx.subscribe()
    }
    pub async fn set_metadata(&self, v: serde_json::Value) {
        *self.metadata.lock().await = v;
    }
    pub async fn merge_metadata(&self, v: serde_json::Value) {
        use serde_json::Value;
        let mut m = self.metadata.lock().await;
        match (&mut *m, v) {
            (Value::Object(base), Value::Object(n)) => {
                for (k, val) in n {
                    base.insert(k, val);
                }
            }
            (_, o) => *m = o,
        }
    }
    pub async fn metadata(&self) -> serde_json::Value {
        self.metadata.lock().await.clone()
    }
    pub fn track_identifier(&self) -> String {
        futures::executor::block_on(async { self.track_info.lock().await.identifier.clone() })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub async fn track_info_snapshot(&self) -> InternalTrackInfo {
        self.track_info.lock().await.clone()
    }
    pub async fn enqueue(&self, uri: String, metadata: serde_json::Value) -> String {
        let mut q = self.queue.lock().await;
        let item = TrackItem::new(&uri, metadata);
        let id = item.id.clone();
        q.push(item);
        let _ = self.event_tx.send(PlayerEvent::QueueUpdate);
        id
    }
    pub async fn set_loop_mode(&self, mode: LoopMode) {
        *self.loop_mode.lock().await = mode;
        let _ = self.event_tx.send(PlayerEvent::LoopModeChange(mode));
    }
    async fn next_track_uri(&self, skipped: bool) -> Option<String> {
        let mut q = self.queue.lock().await;
        let mode = *self.loop_mode.lock().await;
        if mode == LoopMode::Track && !skipped {
            return Some(self.track_identifier());
        }
        if q.is_empty() {
            return None;
        }
        match mode {
            LoopMode::Track => Some(self.track_identifier()),
            LoopMode::Queue => {
                let item = q.remove(0);
                let uri = item.uri.clone();
                q.push(item);
                Some(uri)
            }
            LoopMode::None => {
                let item = q.remove(0);
                Some(item.uri)
            }
        }
    }
    pub async fn queue_snapshot(&self) -> Vec<TrackItem> {
        self.queue.lock().await.clone()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InternalTrackInfo {
    pub id: String,
    pub identifier: String,
    pub uri: String,
    pub title: String,
    pub author: String,
    pub length_ms: u64,
    pub position_ms: u64,
    pub is_stream: bool,
    pub is_seekable: bool,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
    pub source_name: String,
}
impl InternalTrackInfo {
    fn new(id: &str, uri: &str) -> Self {
        Self {
            id: id.into(),
            identifier: uri.into(),
            uri: uri.into(),
            title: uri.into(),
            author: String::new(),
            length_ms: 0,
            position_ms: 0,
            is_stream: true,
            is_seekable: false,
            artwork_url: None,
            isrc: None,
            source_name: "direct".into(),
        }
    }
}
