use crate::utils::enc::encrypt_file_in_place;
use anyhow::{anyhow, Context, Result};
use rspotify::{model::TrackId, prelude::BaseClient, ClientCredsSpotify, Credentials};
use serde::Deserialize;
use tokio::process::Command;
use url::Url;

use crate::config::EffectiveConfig;

fn host(url: &str) -> Option<String> {
    Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_lowercase()))
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
            if let Ok(path) =
                download_with_ytdlp_to_temp(cfg, input, &cfg.preferred_format, Some(".m4a")).await
            {
                return Ok(path);
            }
            if let Ok(path) =
                download_with_ytdlp_to_temp(cfg, input, "bestaudio[ext=m4a]/bestaudio/best", Some(".m4a"))
                    .await
            {
                return Ok(path);
            }
            anyhow::bail!("Failed to download YouTube audio");
        }
        if h.contains("soundcloud.com") {
            if let Ok(path) = download_with_ytdlp_mp3(cfg, input).await {
                return Ok(path);
            }
            if let Ok(url) = run_yt_dlp(cfg, &["--no-playlist", "-g", input]).await {
                if !url.is_empty() {
                    return Ok(url);
                }
            }
            anyhow::bail!("Failed to resolve SoundCloud URL");
        }
        if h.contains("spotify.com") {
            if cfg_spotify_creds(cfg).is_none() {
                anyhow::bail!(
                    "Spotify URL provided but credentials are missing. Provide SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET or configure them in Resonix.toml."
                );
            }
            if let Some(url_track_id) = parse_spotify_track_id(input) {
                if let Some((client_id, client_secret)) = cfg_spotify_creds(cfg) {
                    if let Ok((title, artists)) =
                        fetch_spotify_track_metadata(&client_id, &client_secret, &url_track_id).await
                    {
                        let artist_joined =
                            if artists.is_empty() { String::new() } else { artists.join(", ") + " - " };
                        let query = format!("ytsearch1:{}{}", artist_joined, title);
                        if let Ok(path) =
                            download_with_ytdlp_to_temp(cfg, &query, &cfg.preferred_format, Some(".m4a"))
                                .await
                        {
                            return Ok(path);
                        }
                        if let Ok(path) = download_with_ytdlp_to_temp(
                            cfg,
                            &query,
                            "bestaudio[ext=m4a]/bestaudio/best",
                            Some(".m4a"),
                        )
                        .await
                        {
                            return Ok(path);
                        }
                    }
                }
            }

            if let Ok(title) = fetch_spotify_oembed_title(input).await {
                let query = format!("ytsearch1:{}", title);
                if let Ok(path) =
                    download_with_ytdlp_to_temp(cfg, &query, &cfg.preferred_format, Some(".m4a")).await
                {
                    return Ok(path);
                }
                if let Ok(path) = download_with_ytdlp_to_temp(
                    cfg,
                    &query,
                    "bestaudio[ext=m4a]/bestaudio/best",
                    Some(".m4a"),
                )
                .await
                {
                    return Ok(path);
                }
            }

            if cfg.allow_spotify_title_search {
                if let Ok(title) = run_yt_dlp(cfg, &["-e", input]).await {
                    let query = format!("ytsearch1:{}", title);
                    if let Ok(path) =
                        download_with_ytdlp_to_temp(cfg, &query, &cfg.preferred_format, Some(".m4a")).await
                    {
                        return Ok(path);
                    }
                    if let Ok(path) = download_with_ytdlp_to_temp(
                        cfg,
                        &query,
                        "bestaudio[ext=m4a]/bestaudio/best",
                        Some(".m4a"),
                    )
                    .await
                    {
                        return Ok(path);
                    }
                }
            }
            anyhow::bail!("Failed to resolve Spotify URL");
        }
    }

    anyhow::bail!("Failed to resolve URL to direct audio")
}

pub async fn resolve_with_retry(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=3 {
        match resolve_to_direct(cfg, input).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                let em = e.to_string();
                if em.contains("probe") || em.contains("ffmpeg") || em.contains("unsupported feature") {
                    last_err = Some(e);
                    let delay_ms = 250 * attempt;
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("resolve failed after retries")))
}

async fn run_yt_dlp(cfg: &EffectiveConfig, args: &[&str]) -> Result<String> {
    let bin = std::env::var("RESONIX_YTDLP_BIN").unwrap_or_else(|_| cfg.ytdlp_path.clone());
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

pub async fn download_with_ytdlp_to_temp(
    cfg: &EffectiveConfig,
    input: &str,
    format: &str,
    suffix: Option<&str>,
) -> Result<String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("resonix_");
    if let Some(suf) = suffix {
        builder.suffix(suf);
    }
    let file = builder.tempfile()?;
    let path = file.path().to_path_buf();
    drop(file);

    let out_path = path.to_string_lossy().to_string();

    let ytdlp_bin = std::env::var("RESONIX_YTDLP_BIN").unwrap_or_else(|_| cfg.ytdlp_path.clone());
    let mut cmd = Command::new(&ytdlp_bin);
    cmd.arg("--no-playlist").arg("-f").arg(format).arg("-o").arg(&out_path).arg(input);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let status = cmd.status().await.context("run yt-dlp to download")?;
    if !status.success() {
        anyhow::bail!("yt-dlp download failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat downloaded file")?;
    if meta.len() == 0 {
        anyhow::bail!("yt-dlp created empty file");
    }
    encrypt_file_in_place(std::path::Path::new(&out_path)).context("encrypt ytdlp temp file")?;
    Ok(out_path)
}

async fn download_with_ytdlp_mp3(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("resonix_").suffix(".mp3");
    let file = builder.tempfile()?;
    let path = file.path().to_path_buf();
    drop(file);

    let out_path = path.to_string_lossy().to_string();

    let ytdlp_bin = std::env::var("RESONIX_YTDLP_BIN").unwrap_or_else(|_| cfg.ytdlp_path.clone());
    let mut cmd = Command::new(&ytdlp_bin);
    cmd.args(["--no-playlist", "-x", "--audio-format", "mp3", "-o"]).arg(&out_path).arg(input);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let status = cmd.status().await.context("run yt-dlp to download/extract mp3")?;
    if !status.success() {
        anyhow::bail!("yt-dlp mp3 extraction failed with status {status}");
    }

    let meta = tokio::fs::metadata(&out_path).await.context("stat downloaded mp3")?;
    if meta.len() == 0 {
        anyhow::bail!("yt-dlp created empty mp3 file");
    }
    encrypt_file_in_place(std::path::Path::new(&out_path)).context("encrypt ytdlp mp3 temp file")?;
    Ok(out_path)
}

fn cfg_spotify_creds(cfg: &EffectiveConfig) -> Option<(String, String)> {
    match (&cfg.spotify_client_id, &cfg.spotify_client_secret) {
        (Some(id), Some(sec)) if !id.is_empty() && !sec.is_empty() => Some((id.clone(), sec.clone())),
        _ => None,
    }
}

fn parse_spotify_track_id(input: &str) -> Option<String> {
    if let Some(u) = Url::parse(input).ok() {
        if let Some(h) = u.host_str() {
            if !h.contains("spotify.com") {
                return None;
            }
        }
        let mut prev: Option<String> = None;
        for seg in u.path_segments()? {
            if let Some(p) = &prev {
                if p == "track" && !seg.is_empty() {
                    let id = seg.split('?').next().unwrap_or(seg);
                    return Some(id.to_string());
                }
            }
            prev = Some(seg.to_string());
        }
        return None;
    }
    if let Some(rest) = input.strip_prefix("spotify:track:") {
        return Some(rest.to_string());
    }
    None
}

#[derive(Deserialize)]
struct SpotifyOEmbed {
    title: String,
}

async fn fetch_spotify_oembed_title(url: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://open.spotify.com/oembed")
        .query(&[("url", url)])
        .send()
        .await
        .context("spotify oembed get")?
        .error_for_status()
        .context("spotify oembed bad status")?;
    let bytes = resp.bytes().await.context("spotify oembed read body")?;
    let v: SpotifyOEmbed = serde_json::from_slice(&bytes).context("spotify oembed parse json")?;
    Ok(v.title)
}

async fn fetch_spotify_track_metadata(
    client_id: &str,
    client_secret: &str,
    track_id_b62: &str,
) -> Result<(String, Vec<String>)> {
    let creds = Credentials { id: client_id.to_string(), secret: Some(client_secret.to_string()) };
    let spotify = ClientCredsSpotify::new(creds);
    spotify.request_token().await.map_err(|e| anyhow!("spotify auth: {e}"))?;

    let tid = TrackId::from_id(track_id_b62).map_err(|e| anyhow!("invalid spotify track id: {e}"))?;
    let track = spotify.track(tid, None).await.map_err(|e| anyhow!("spotify track fetch: {e}"))?;
    let title = track.name.clone();
    let artists = track.artists.iter().map(|a| a.name.clone()).collect::<Vec<_>>();
    Ok((title, artists))
}
