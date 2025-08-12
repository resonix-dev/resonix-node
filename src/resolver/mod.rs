use anyhow::{Context, Result};
use tokio::process::Command;
use url::Url;

use crate::config::EffectiveConfig;

fn host(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_lowercase()))
}

pub fn is_uri_allowed(cfg: &EffectiveConfig, uri: &str) -> bool {
    let h = host(uri).unwrap_or_default();
    if cfg.block_patterns.iter().any(|re| re.is_match(uri) || (!h.is_empty() && re.is_match(&h))) {
        return false;
    }
    if cfg.allow_patterns.is_empty() {
        true
    } else {
        cfg.allow_patterns.iter().any(|re| re.is_match(uri) || (!h.is_empty() && re.is_match(&h)))
    }
}

pub fn needs_resolve(input: &str) -> bool {
    if let Some(h) = host(input) {
        return h.contains("youtube.com")
            || h == "youtu.be"
            || h.contains("spotify.com")
            || h.contains("soundcloud.com");
    }
    false
}

pub async fn resolve_to_direct(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    if let Some(h) = host(input) {
        if h.contains("youtube.com") || h == "youtu.be" {
            if let Ok(path) = download_with_ytdlp_to_temp(cfg, input, &cfg.preferred_format, Some(".m4a")).await {
                return Ok(path);
            }
            if let Ok(path) = download_with_ytdlp_to_temp(cfg, input, "bestaudio[ext=m4a]/bestaudio/best", Some(".m4a")).await {
                return Ok(path);
            }
            anyhow::bail!("Failed to download YouTube audio");
        }
        if h.contains("soundcloud.com") {
            if let Ok(path) = download_with_ytdlp_mp3(cfg, input).await {
                return Ok(path);
            }
            if let Ok(url) = run_yt_dlp(cfg, &["--no-playlist", "-g", input]).await {
                if !url.is_empty() { return Ok(url); }
            }
            anyhow::bail!("Failed to resolve SoundCloud URL");
        }
        if h.contains("spotify.com") {
            if cfg.allow_spotify_title_search {
                if let Ok(title) = run_yt_dlp(cfg, &["-e", input]).await {
                    let query = format!("ytsearch1:{}", title);
                    if let Ok(path) = download_with_ytdlp_to_temp(cfg, &query, &cfg.preferred_format, Some(".m4a")).await { return Ok(path); }
                    if let Ok(path) = download_with_ytdlp_to_temp(cfg, &query, "bestaudio[ext=m4a]/bestaudio/best", Some(".m4a")).await { return Ok(path); }
                }
            }
            anyhow::bail!("Failed to resolve Spotify URL");
        }
    }

    anyhow::bail!("Failed to resolve URL to direct audio")
}

async fn run_yt_dlp(cfg: &EffectiveConfig, args: &[&str]) -> Result<String> {
    let bin = cfg.ytdlp_path.clone();
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.stderr(std::process::Stdio::null());
    let timeout_ms: u64 = cfg.resolve_timeout_ms;
    let out = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async move {
        let out = cmd.output().await?;
        anyhow::Ok(out)
    })
    .await??;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(s)
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("yt-dlp failed: {}", e)
    }
}

pub async fn download_with_ytdlp_to_temp(cfg: &EffectiveConfig, input: &str, format: &str, suffix: Option<&str>) -> Result<String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("resonix_");
    if let Some(suf) = suffix { builder.suffix(suf); }
    let file = builder.tempfile()?;
    let path = file.path().to_path_buf();
    drop(file);

    let out_path = path.to_string_lossy().to_string();

    let mut cmd = Command::new(&cfg.ytdlp_path);
    cmd.arg("--no-playlist").arg("-f").arg(format).arg("-o").arg(&out_path).arg(input);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let status = cmd.status().await.context("run yt-dlp to download")?;
    if !status.success() {
        anyhow::bail!("yt-dlp download failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat downloaded file")?;
    if meta.len() == 0 { anyhow::bail!("yt-dlp created empty file"); }
    Ok(out_path)
}

async fn download_with_ytdlp_mp3(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("resonix_").suffix(".mp3");
    let file = builder.tempfile()?;
    let path = file.path().to_path_buf();
    drop(file);

    let out_path = path.to_string_lossy().to_string();

    let mut cmd = Command::new(&cfg.ytdlp_path);
    cmd.args(["--no-playlist", "-x", "--audio-format", "mp3", "-o"]).arg(&out_path).arg(input);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let status = cmd.status().await.context("run yt-dlp to download/extract mp3")?;
    if !status.success() {
        anyhow::bail!("yt-dlp mp3 extraction failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat downloaded mp3")?;
    if meta.len() == 0 { anyhow::bail!("yt-dlp created empty mp3 file"); }
    Ok(out_path)
}
