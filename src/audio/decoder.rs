use anyhow::{anyhow, Context, Result};
use std::{
    io::{BufReader, Read},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

const SAMPLE_RATE: usize = 48_000;
const CHANNELS: usize = 2;
const FRAME_SAMPLES: usize = 960;
const BYTES_PER_SAMPLE: usize = 2;
const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * BYTES_PER_SAMPLE;

pub struct PcmBlock {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

pub struct FfmpegDecoder {
    child: Child,
    stdout: BufReader<ChildStdout>,
    pending: Vec<u8>,
}

impl FfmpegDecoder {
    pub fn open(path: &Path, ffmpeg_bin: &str) -> Result<Self> {
        let mut child = Command::new(ffmpeg_bin)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(path)
            .arg("-f")
            .arg("s16le")
            .arg("-ac")
            .arg(format!("{}", CHANNELS))
            .arg("-ar")
            .arg(format!("{}", SAMPLE_RATE))
            .arg("pipe:1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn ffmpeg using '{ffmpeg_bin}'"))?;

        let stdout = child.stdout.take().ok_or_else(|| anyhow!("ffmpeg stdout not captured"))?;

        Ok(Self { child, stdout: BufReader::new(stdout), pending: Vec::new() })
    }

    pub fn next_pcm_block(&mut self) -> Result<Option<PcmBlock>> {
        let mut raw = std::mem::take(&mut self.pending);
        raw.reserve(FRAME_BYTES);

        while raw.len() < FRAME_BYTES {
            let mut buf = vec![0u8; FRAME_BYTES - raw.len()];
            match self.stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(anyhow!("read ffmpeg stdout: {e}")),
            }
        }

        if raw.is_empty() {
            return Ok(None);
        }

        let aligned = raw.len() - (raw.len() % (CHANNELS * BYTES_PER_SAMPLE));
        if aligned == 0 {
            self.pending = raw;
            return Ok(None);
        }
        let remainder = raw.split_off(aligned);
        self.pending = remainder;

        let mut l = Vec::with_capacity(aligned / (CHANNELS * BYTES_PER_SAMPLE));
        let mut r = Vec::with_capacity(l.capacity());
        for chunk in raw.chunks_exact(CHANNELS * BYTES_PER_SAMPLE) {
            let left = i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0;
            let right = i16::from_le_bytes([chunk[2], chunk[3]]) as f32 / 32768.0;
            l.push(left);
            r.push(right);
        }

        Ok(Some(PcmBlock { l, r }))
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
