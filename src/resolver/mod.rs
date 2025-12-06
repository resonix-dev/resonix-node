use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use riva::soundcloud;
use riva::youtube;
use rspotify::{model::TrackId, prelude::BaseClient, ClientCredsSpotify, Credentials};
use serde::Deserialize;
use std::time::Duration;
use url::{form_urlencoded, Url};

use crate::config::EffectiveConfig;

const YT_SEARCH_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Resonix/0.3";
const MIN_RESOLVE_TIMEOUT_MS: u64 = 1_000;

static YT_VIDEO_ID_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\"videoId\":\"([A-Za-z0-9_-]{11})\""#).expect("valid video id regex"));

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
    if parse_ytsearch_query(input).is_some() {
        return true;
    }
    if let Some(h) = host(input) {
        return h.contains("youtube.com")
            || h == "youtu.be"
            || h.contains("spotify.com")
            || h.contains("soundcloud.com");
    }
    false
}

pub async fn resolve_to_direct(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    if let Some(query) = parse_ytsearch_query(input) {
        return resolve_youtube_search(cfg, &query).await;
    }
    if let Some(h) = host(input) {
        if h.contains("youtube.com") || h == "youtu.be" {
            return resolve_youtube_url(cfg, input).await;
        }
        if h.contains("soundcloud.com") {
            return resolve_soundcloud_url(cfg, input).await;
        }
        if h.contains("spotify.com") {
            return resolve_spotify_link(cfg, input).await;
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
                if em.contains("probe")
                    || em.contains("unsupported feature")
                    || em.contains("unsupported codec")
                {
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

async fn resolve_youtube_url(_cfg: &EffectiveConfig, url: &str) -> Result<String> {
    let streams =
        youtube::extract_streams(url).await.map_err(|e| anyhow!("youtube extraction failed: {e}"))?;
    let first = streams.first().ok_or_else(|| anyhow!("no playable youtube streams"))?;
    Ok(first.url.clone())
}

async fn resolve_youtube_search(cfg: &EffectiveConfig, query: &str) -> Result<String> {
    let video_id = search_youtube_video_id(cfg, query).await?;
    let url = format!("https://www.youtube.com/watch?v={video_id}");
    resolve_youtube_url(cfg, &url).await
}

async fn search_youtube_video_id(cfg: &EffectiveConfig, query: &str) -> Result<String> {
    let client = youtube_search_client(cfg)?;
    let encoded: String = form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let url = format!("https://www.youtube.com/results?search_query={encoded}");
    let body = client
        .get(&url)
        .send()
        .await
        .context("youtube search request failed")?
        .error_for_status()
        .context("youtube search returned error status")?
        .text()
        .await
        .context("youtube search body read failed")?;

    let caps = YT_VIDEO_ID_REGEX
        .captures(&body)
        .ok_or_else(|| anyhow!("youtube search did not return any video ids"))?;
    Ok(caps.get(1).map(|m| m.as_str()).unwrap_or_default().to_string())
}

async fn resolve_soundcloud_url(_cfg: &EffectiveConfig, url: &str) -> Result<String> {
    let streams =
        soundcloud::extract_streams(url).await.map_err(|e| anyhow!("soundcloud extraction failed: {e}"))?;
    let first = streams.first().ok_or_else(|| anyhow!("no playable soundcloud streams"))?;
    Ok(first.url.clone())
}

async fn resolve_spotify_link(cfg: &EffectiveConfig, input: &str) -> Result<String> {
    if cfg_spotify_creds(cfg).is_none() {
        anyhow::bail!(
            "Spotify URL provided but credentials are missing. Provide SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET or configure them in Resonix.toml."
        );
    }

    if let Some(track_id) = parse_spotify_track_id(input) {
        if let Some((client_id, client_secret)) = cfg_spotify_creds(cfg) {
            if let Ok((title, artists)) =
                fetch_spotify_track_metadata(&client_id, &client_secret, &track_id).await
            {
                let mut query = title.clone();
                if !artists.is_empty() {
                    query = format!("{} - {}", artists.join(", "), title);
                }
                if let Ok(url) = resolve_youtube_search(cfg, &query).await {
                    return Ok(url);
                }
            }
        }
    }

    if cfg.allow_spotify_title_search {
        if let Ok(title) = fetch_spotify_oembed_title(input).await {
            if let Ok(url) = resolve_youtube_search(cfg, &title).await {
                return Ok(url);
            }
        }
    }

    anyhow::bail!("Failed to resolve Spotify URL")
}

fn parse_ytsearch_query(input: &str) -> Option<String> {
    let idx = input.find(':')?;
    let prefix = &input[..idx];
    if !prefix.to_ascii_lowercase().starts_with("ytsearch") {
        return None;
    }
    let query = input[idx + 1..].trim();
    if query.is_empty() {
        None
    } else {
        Some(query.to_string())
    }
}

fn youtube_search_client(cfg: &EffectiveConfig) -> Result<Client> {
    Client::builder()
        .user_agent(YT_SEARCH_UA)
        .timeout(resolver_timeout(cfg))
        .build()
        .context("build youtube search client")
}

fn resolver_timeout(cfg: &EffectiveConfig) -> Duration {
    Duration::from_millis(cfg.resolve_timeout_ms.max(MIN_RESOLVE_TIMEOUT_MS))
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
