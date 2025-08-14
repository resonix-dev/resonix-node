use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy)]
pub enum ToolKind {
    YtDlp,
    Ffmpeg,
}

impl ToolKind {
    pub fn filename(self) -> &'static str {
        match self {
            ToolKind::YtDlp => {
                if cfg!(windows) {
                    "yt-dlp.exe"
                } else {
                    "yt-dlp"
                }
            }
            ToolKind::Ffmpeg => {
                if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                }
            }
        }
    }
    pub fn url(self) -> &'static str {
        match self {
            ToolKind::YtDlp => {
                if cfg!(target_os = "windows") {
                    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
                } else if cfg!(target_os = "macos") {
                    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos"
                } else {
                    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp"
                }
            }
            ToolKind::Ffmpeg => {
                // Use BtbN static builds for now (GPL). Windows & Linux; macOS users should install via brew (we still attempt download for parity except Mac).
                if cfg!(target_os = "windows") {
                    // We pick win64 gpl build; for arm64 fallback also works via winarm64 but keep simple.
                    "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip"
                } else if cfg!(target_os = "linux") {
                    "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-linux64-gpl.tar.xz"
                } else {
                    // macOS: we do not auto download (brew preferred); return empty to skip.
                    ""
                }
            }
        }
    }
}

pub fn tools_home_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    home.join(".resonix").join("bin")
}

pub async fn ensure_tool(kind: ToolKind) -> Result<Option<PathBuf>> {
    let dir = tools_home_dir();
    tokio::fs::create_dir_all(&dir).await.ok();
    let path = dir.join(kind.filename());
    if path.exists() {
        debug!(tool=?kind, installed_path=%path.display(), "Tool already present; skipping download");
        return Ok(Some(path));
    }
    let url = kind.url();
    if url.is_empty() {
        debug!(tool=?kind, "No download URL defined for platform; skipping");
        return Ok(None);
    }
    info!(tool=?kind, %url, dest=%path.display(), "Downloading tool (first run)");
    let started = Instant::now();

    if matches!(kind, ToolKind::Ffmpeg) {
        let required_bins: &[&str] = if cfg!(windows) {
            &["ffmpeg.exe", "ffplay.exe", "ffprobe.exe"]
        } else {
            &["ffmpeg", "ffplay", "ffprobe"]
        };
        let mut extracted: Vec<String> = Vec::new();
        if url.ends_with(".zip") {
            let resp = reqwest::get(url).await.context("download ffmpeg zip")?;
            let status = resp.status();
            if !status.is_success() {
                anyhow::bail!("ffmpeg zip request failed {status}");
            }
            let bytes = resp.bytes().await?;
            info!(tool=?kind, size_bytes=bytes.len(), "Archive downloaded; extracting (zip)");
            let reader = std::io::Cursor::new(bytes);
            let mut zip = zip::ZipArchive::new(reader).context("open ffmpeg zip")?;
            let total = zip.len();
            debug!(entries=total, tool=?kind, "Scanning zip entries for binary");
            for i in 0..zip.len() {
                let mut file = zip.by_index(i).context("zip entry")?;
                let entry_name = file.name().to_string();
                if entry_name.ends_with('/') {
                    continue;
                }
                if let Some(fname) = entry_name.rsplit('/').next() {
                    if required_bins.contains(&fname) {
                        let out_path = dir.join(fname);
                        let mut out =
                            std::fs::File::create(&out_path).context("create ffmpeg related bin")?;
                        std::io::copy(&mut file, &mut out).context("write ffmpeg related bin")?;
                        extracted.push(fname.to_string());
                        debug!(tool=?kind, matched_entry=%entry_name, dest=%out_path.display(), "Extracted binary from zip");
                    }
                }
            }
        } else if url.ends_with(".tar.xz") {
            let resp = reqwest::get(url).await.context("download ffmpeg tar.xz")?;
            let status = resp.status();
            if !status.is_success() {
                anyhow::bail!("ffmpeg tar.xz request failed {status}");
            }
            let bytes = resp.bytes().await?;
            info!(tool=?kind, size_bytes=bytes.len(), "Archive downloaded; extracting (tar.xz)");
            let cursor = std::io::Cursor::new(bytes);
            let xz = xz2::read::XzDecoder::new(cursor);
            let mut archive = tar::Archive::new(xz);
            for entry in archive.entries().context("tar entries")? {
                let mut entry = entry.context("tar entry")?;
                let mut target: Option<String> = None;
                if let Ok(p) = entry.path() {
                    if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                        if required_bins.contains(&fname) {
                            target = Some(fname.to_string());
                        }
                    }
                }
                if let Some(fname) = target {
                    let out_path = dir.join(&fname);
                    entry.unpack(&out_path).context("unpack ffmpeg related bin")?;
                    extracted.push(fname.clone());
                    debug!(tool=?kind, matched_entry=%fname, dest=%out_path.display(), "Extracted binary from tar.xz");
                }
            }
        } else {
            warn!(%url, "Unsupported ffmpeg archive format; skipping");
            return Ok(None);
        }
        if !extracted.iter().any(|e| e.starts_with("ffmpeg")) {
            warn!(tool=?kind, extracted=?extracted, "Archive processed but 'ffmpeg' binary not found");
        }
    } else {
        let resp = reqwest::get(url).await.context("download yt-dlp")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("yt-dlp request failed {status}");
        }
        let bytes = resp.bytes().await?;
        info!(tool=?kind, size_bytes=bytes.len(), "Binary downloaded; writing to disk");
        tokio::fs::write(&path, &bytes).await.context("write yt-dlp")?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perm = meta.permissions();
            perm.set_mode(0o755);
            let _ = std::fs::set_permissions(&path, perm);
        }
    }

    let elapsed = started.elapsed();
    if path.exists() {
        if let Ok(meta) = std::fs::metadata(&path) {
            info!(tool=?kind, installed_path=%path.display(), size_bytes=meta.len(), took_ms=elapsed.as_millis(), "Tool installed successfully");
        } else {
            info!(tool=?kind, installed_path=%path.display(), took_ms=elapsed.as_millis(), "Tool installed (metadata unavailable)");
        }
        Ok(Some(path))
    } else {
        warn!(tool=?kind, took_ms=elapsed.as_millis(), "Download/extraction finished but file missing");
        Ok(None)
    }
}

pub async fn ensure_all(
    manage_ytdlp: bool,
    manage_ffmpeg: bool,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let mut ytdlp = None;
    let mut ffmpeg = None;
    if manage_ytdlp {
        ytdlp = ensure_tool(ToolKind::YtDlp).await?;
    }
    if manage_ffmpeg {
        ffmpeg = ensure_tool(ToolKind::Ffmpeg).await?;
    }
    Ok((ytdlp, ffmpeg))
}
