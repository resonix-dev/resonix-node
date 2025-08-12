use std::{sync::Arc, time::Duration};

use anyhow::Result;
use bytes::Bytes;
use bytemuck;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

use crate::audio::decoder::SymphoniaDecoder;
use crate::audio::dsp::{Filters, biquad_eq_in_place, update_eq_filters};
use crate::audio::source::prepare_local_source;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct EqBandParam { pub band: u8, pub gain_db: f32 }

pub struct Player {
    id: String,
    uri: String,
    ctrl: PlayerCtrl,
    out_tx: broadcast::Sender<Bytes>,
}

#[derive(Clone)]
struct PlayerCtrl {
    pause_tx: broadcast::Sender<bool>,
    stop_tx: broadcast::Sender<()>,
    filters: Arc<Mutex<Filters>>,
}

impl Player {
    pub fn new(id: &str, uri: &str) -> Result<Self> {
        let (pause_tx, _) = broadcast::channel::<bool>(8);
        let (stop_tx, _) = broadcast::channel::<()>(1);
        let filters = Arc::new(Mutex::new(Filters::default()));
        {
            let mut f = futures::executor::block_on(filters.lock());
            update_eq_filters(&mut f);
        }
        let (out_tx, _rx) = broadcast::channel::<Bytes>(1024);

        Ok(Self { id: id.into(), uri: uri.into(), ctrl: PlayerCtrl { pause_tx, stop_tx, filters }, out_tx })
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!("Starting player {} for {}", self.id, self.uri);
        let source_path = prepare_local_source(&self.uri).await?;
        info!("Using local source: {}", source_path.display());

        let mut waited_ms: u64 = 0;
        while self.out_tx.receiver_count() == 0 {
            if waited_ms % 500 == 0 { info!(id=%self.id, "waiting for WS subscriber..."); }
            tokio::time::sleep(Duration::from_millis(50)).await;
            waited_ms += 50;
            if waited_ms >= 5_000 { info!(id=%self.id, "no subscriber after 5s; starting anyway"); break; }
        }

        let mut decoder = SymphoniaDecoder::open(&source_path)?;

        const FRAME_SAMPLES: usize = 960;
        const CHANNELS: usize = 2;
        const SAMPLES_PER_FRAME: usize = FRAME_SAMPLES * CHANNELS;

        let mut interleaved_i16: Vec<i16> = Vec::with_capacity(SAMPLES_PER_FRAME * 8);
        let (mut pause_rx, mut stop_rx) = self.ctrl_channels();
        let mut paused = false;
        let mut sent_frames: u64 = 0;
        let mut head: usize = 0;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut underruns_logged = 0u32;
        let mut eos = false;

        loop {
            tick.tick().await;

            match stop_rx.try_recv() {
                Ok(_) => { info!("Stop received for {}", self.id); break; }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => { info!("Stop channel closed for {}", self.id); break; }
                Err(_) => {}
            }
            if let Ok(p) = pause_rx.try_recv() { paused = p; info!("{} paused={}", self.id, paused); }
            if paused { continue; }

            while !eos && interleaved_i16.len().saturating_sub(head) < SAMPLES_PER_FRAME * 4 {
                debug!(id=%self.id, "decoding next block");
                match decoder.next_pcm_block()? {
                    Some(mut block) => {
                        if block.l.is_empty() { break; }
                        let vol = {
                            let mut f = self.ctrl.filters.lock().await;
                            biquad_eq_in_place(&mut block.l, &mut block.r, &mut *f);
                            f.volume
                        };
                        for i in 0..block.l.len() {
                            block.l[i] = (block.l[i] * vol).tanh();
                            block.r[i] = (block.r[i] * vol).tanh();
                        }
                        interleaved_i16.reserve(block.l.len() * 2);
                        for i in 0..block.l.len() {
                            interleaved_i16.push((block.l[i] * 32767.0).clamp(-32768.0, 32767.0) as i16);
                            interleaved_i16.push((block.r[i] * 32767.0).clamp(-32768.0, 32767.0) as i16);
                        }
                    }
                    None => { eos = true; break; }
                }
                if interleaved_i16.len().saturating_sub(head) >= SAMPLES_PER_FRAME * 2 { break; }
            }

            if interleaved_i16.len().saturating_sub(head) >= SAMPLES_PER_FRAME {
                let frame_slice = &interleaved_i16[head..head + SAMPLES_PER_FRAME];
                let bytes = bytemuck::cast_slice(frame_slice);
                let _ = self.out_tx.send(Bytes::copy_from_slice(bytes));
                sent_frames += 1;
                if sent_frames % 2000 == 0 { info!(id=%self.id, sent_frames, "PCM sent (summary)"); }
                head += SAMPLES_PER_FRAME;
                if head >= SAMPLES_PER_FRAME * 8 && head > interleaved_i16.len() / 2 { interleaved_i16.drain(0..head); head = 0; }
                underruns_logged = 0;
            } else {
                if underruns_logged < 5 { warn!(id=%self.id, "audio underrun (buffer empty)"); }
                underruns_logged = underruns_logged.saturating_add(1);
                if eos { info!("End of stream for {}", self.id); break; }
            }
        }

        info!("Player {} stopped", self.id);
        Ok(())
    }

    fn ctrl_channels(&self) -> (broadcast::Receiver<bool>, broadcast::Receiver<()>) {
        (self.ctrl.pause_tx.subscribe(), self.ctrl.stop_tx.subscribe())
    }

    pub fn play(&self) -> Result<()> { let _ = self.ctrl.pause_tx.send(false); Ok(()) }
    pub fn pause(&self) -> Result<()> { let _ = self.ctrl.pause_tx.send(true); Ok(()) }
    pub fn stop(&self) { let _ = self.ctrl.stop_tx.send(()); }

    pub fn set_volume(&self, v: f32) {
        let filters = self.ctrl.filters.clone();
        tokio::spawn(async move { filters.lock().await.volume = v.max(0.0); });
    }
    pub fn set_eq(&self, bands: Vec<EqBandParam>) {
        let filters = self.ctrl.filters.clone();
        tokio::spawn(async move {
            let mut f = filters.lock().await;
            for b in bands { if let Some(slot) = f.eq.get_mut(b.band as usize) { *slot = b.gain_db; } }
            update_eq_filters(&mut f);
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> { self.out_tx.subscribe() }
}
