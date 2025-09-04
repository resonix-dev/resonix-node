use crate::utils::enc::{is_encrypted_file, read_decrypted_file};
use anyhow::{anyhow, Result};
use symphonia::core::audio::Signal;
use tracing::{debug, warn};

pub struct PcmBlock {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

pub struct SymphoniaDecoder {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    chan_count: usize,
    resampler: Option<LinearResampler>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    temp_plain_path: Option<std::path::PathBuf>,
}

impl SymphoniaDecoder {
    pub fn open(path: &std::path::PathBuf) -> Result<Self> {
    use std::fs::File;
        use symphonia::core::{
            codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions,
            probe::Hint,
        };

        let mut temp_plain_path: Option<std::path::PathBuf> = None;
            let mss = if is_encrypted_file(path) {
                let data = read_decrypted_file(path)?;
                let mut t = tempfile::Builder::new().prefix("resonix_dec_").tempfile()?;
                use std::io::Write;
                t.as_file_mut().write_all(&data)?;
                let p = t.into_temp_path().keep()?;
                temp_plain_path = Some(p.clone());
                let file = File::open(&p).map_err(|e| anyhow!("open tmp source: {e}"))?;
                MediaSourceStream::new(Box::new(file), Default::default())
            } else {
            let file = File::open(path).map_err(|e| anyhow!("open source: {e}"))?;
            MediaSourceStream::new(Box::new(file), Default::default())
        };

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| anyhow!("probe error: {e}"))?;
        let format = probed.format;

        let track = format.default_track().ok_or_else(|| anyhow!("no default track"))?;
        let track_id = track.id;

        let dec = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions { verify: false })
            .map_err(|e| anyhow!("decoder error: {e}"))?;
        let decoder = dec;

        let src_rate = track.codec_params.sample_rate.ok_or_else(|| anyhow!("unknown sample rate"))?;
        let chan_count = track.codec_params.channels.map(|c| c.count()).unwrap_or(2) as usize;

        let resampler = LinearResampler::new(src_rate as f64, 48_000.0);

        Ok(Self {
            format,
            decoder,
            track_id,
            chan_count,
            resampler,
            out_l: Vec::with_capacity(48_000),
            out_r: Vec::with_capacity(48_000),
            temp_plain_path,
        })
    }

    pub fn next_pcm_block(&mut self) -> Result<Option<PcmBlock>> {
        use symphonia::core::audio::AudioBufferRef;

        if self.out_l.is_empty() || self.out_l.len() < 960 {
            loop {
                let packet = match self.format.next_packet() {
                    Ok(p) => p,
                    Err(symphonia::core::errors::Error::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        if !self.out_l.is_empty() {
                            let l = std::mem::take(&mut self.out_l);
                            let r = std::mem::take(&mut self.out_r);
                            return Ok(Some(PcmBlock { l, r }));
                        }
                        return Ok(None);
                    }
                    Err(e) => return Err(anyhow!("demux err: {e}")),
                };
                trace_packet(&packet);
                if packet.track_id() != self.track_id {
                    continue;
                }

                let decoded = match self.decoder.decode(&packet) {
                    Ok(d) => d,
                    Err(symphonia::core::errors::Error::DecodeError(_)) => {
                        warn!("decoder recoverable error; skipping packet");
                        continue;
                    }
                    Err(e) => return Err(anyhow!("decode err: {e}")),
                };

                let (mut ch_data, frames, _rate) = match decoded {
                    AudioBufferRef::F32(buf) => {
                        let b = buf.as_ref();
                        let chn = b.spec().channels.count();
                        debug!(chn, frames = b.frames(), rate = b.spec().rate, "decoded f32 buffer");
                        let chs: Vec<Vec<f32>> = (0..chn).map(|c| b.chan(c).to_vec()).collect();
                        (chs, b.frames(), b.spec().rate)
                    }
                    AudioBufferRef::S16(buf) => {
                        let b = buf.as_ref();
                        let chn = b.spec().channels.count();
                        debug!(chn, frames = b.frames(), rate = b.spec().rate, "decoded s16 buffer");
                        let chs: Vec<Vec<f32>> = (0..chn)
                            .map(|c| b.chan(c).iter().map(|&s| s as f32 / 32768.0).collect())
                            .collect();
                        (chs, b.frames(), b.spec().rate)
                    }
                    AudioBufferRef::U8(buf) => {
                        let b = buf.as_ref();
                        let chn = b.spec().channels.count();
                        debug!(chn, frames = b.frames(), rate = b.spec().rate, "decoded u8 buffer");
                        let chs: Vec<Vec<f32>> = (0..chn)
                            .map(|c| b.chan(c).iter().map(|&s| (s as f32 - 128.0) / 128.0).collect())
                            .collect();
                        (chs, b.frames(), b.spec().rate)
                    }
                    other => {
                        debug!(
                            frames = other.frames(),
                            rate = other.spec().rate,
                            "decoded other buffer; converting to f32"
                        );
                        let mut tmp = symphonia::core::audio::AudioBuffer::<f32>::new(
                            other.frames() as u64,
                            other.spec().clone(),
                        );
                        other.convert(&mut tmp);
                        let chs: Vec<Vec<f32>> =
                            (0..tmp.spec().channels.count()).map(|c| tmp.chan(c).to_vec()).collect();
                        (chs, tmp.frames(), tmp.spec().rate)
                    }
                };

                let src_ch_count = ch_data.len();
                let (mut l, mut r) = downmix_to_stereo(&mut ch_data, frames, src_ch_count);
                debug!(in_frames = frames, out_l = l.len(), out_r = r.len(), "downmixed to stereo");

                if let Some(res) = &mut self.resampler {
                    let (ol, or) = res.process(&l, &r);
                    l = ol;
                    r = or;
                    debug!(resampled_l = l.len(), resampled_r = r.len(), "resampled to 48kHz");
                }

                self.out_l.extend_from_slice(&l);
                self.out_r.extend_from_slice(&r);
                debug!(accum_l = self.out_l.len(), accum_r = self.out_r.len(), "accumulated samples");

                if self.out_l.len() >= 960 {
                    break;
                }
            }
        }

        let l = std::mem::take(&mut self.out_l);
        let r = std::mem::take(&mut self.out_r);
        Ok(Some(PcmBlock { l, r }))
    }
}

impl Drop for SymphoniaDecoder {
    fn drop(&mut self) {
        if let Some(p) = self.temp_plain_path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[inline]
fn trace_packet(packet: &symphonia::core::formats::Packet) {
    let ts = packet.ts;
    let d = packet.dur;
    let sz = packet.data.len();
    tracing::debug!(ts, dur = d, size = sz, track_id = packet.track_id(), "demuxed packet");
}

fn downmix_to_stereo(chs: &mut [Vec<f32>], frames: usize, src_ch_count: usize) -> (Vec<f32>, Vec<f32>) {
    if src_ch_count == 1 {
        let m = &chs[0];
        let mut l = Vec::with_capacity(frames);
        let mut r = Vec::with_capacity(frames);
        l.extend_from_slice(m);
        r.extend_from_slice(m);
        (l, r)
    } else {
        let mut l = vec![0.0f32; frames];
        let mut r = vec![0.0f32; frames];
        let mut count_l = 0.0f32;
        let mut count_r = 0.0f32;
        if !chs.is_empty() {
            let c0 = &chs[0];
            for i in 0..frames {
                l[i] += c0[i];
            }
            count_l += 1.0;
        }
        if chs.len() >= 2 {
            let c1 = &chs[1];
            for i in 0..frames {
                r[i] += c1[i];
            }
            count_r += 1.0;
        }
        if chs.len() > 2 {
            for c in &chs[2..] {
                for i in 0..frames {
                    l[i] += c[i] * 0.5;
                    r[i] += c[i] * 0.5;
                }
                count_l += 0.5;
                count_r += 0.5;
            }
        }
        if count_l > 1.0 {
            for v in &mut l {
                *v /= count_l;
            }
        }
        if count_r > 1.0 {
            for v in &mut r {
                *v /= count_r;
            }
        }
        (l, r)
    }
}

struct LinearResampler {
    step: f64,
    pos: f64,
    prev_l: f32,
    prev_r: f32,
    primed: bool,
}
impl LinearResampler {
    fn new(src_rate: f64, dst_rate: f64) -> Option<Self> {
        if (src_rate - dst_rate).abs() < f64::EPSILON {
            return None;
        }
        Some(Self { step: src_rate / dst_rate, pos: 0.0, prev_l: 0.0, prev_r: 0.0, primed: false })
    }
    fn process(&mut self, in_l: &[f32], in_r: &[f32]) -> (Vec<f32>, Vec<f32>) {
        if (self.step - 1.0).abs() < f64::EPSILON {
            return (in_l.to_vec(), in_r.to_vec());
        }
        let mut ext_l: Vec<f32> = Vec::with_capacity(in_l.len() + 1);
        let mut ext_r: Vec<f32> = Vec::with_capacity(in_r.len() + 1);
        if !self.primed {
            self.prev_l = in_l.first().copied().unwrap_or(0.0);
            self.prev_r = in_r.first().copied().unwrap_or(0.0);
            self.primed = true;
        }
        ext_l.push(self.prev_l);
        ext_l.extend_from_slice(in_l);
        ext_r.push(self.prev_r);
        ext_r.extend_from_slice(in_r);
        self.prev_l = *in_l.last().unwrap_or(&self.prev_l);
        self.prev_r = *in_r.last().unwrap_or(&self.prev_r);
        let mut out_l = Vec::with_capacity(((in_l.len() as f64) / self.step).ceil() as usize + 8);
        let mut out_r = Vec::with_capacity(out_l.capacity());
        let len = ext_l.len();
        while self.pos + 1.0 < len as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let xl0 = ext_l[i];
            let xl1 = ext_l[i + 1];
            let xr0 = ext_r[i];
            let xr1 = ext_r[i + 1];
            out_l.push(xl0 + (xl1 - xl0) * frac);
            out_r.push(xr0 + (xr1 - xr0) * frac);
            self.pos += self.step;
        }
        self.pos -= (len as f64 - 1.0).max(0.0);
        (out_l, out_r)
    }
}
