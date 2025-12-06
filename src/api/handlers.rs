use anyhow::Result;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::audio::player::{EqBandParam, Player};
use crate::audio::track::LoopMode;
use crate::config::{resolver_enabled, EffectiveConfig};
use crate::resolver::{is_uri_allowed, needs_resolve, resolve_to_direct, resolve_with_retry};
use crate::state::AppState;
use axum::extract::Query;
use base64::Engine;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
pub struct CreatePlayerReq {
    pub id: String,
    pub uri: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct CreatePlayerRes {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct FiltersReq {
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub eq: Option<Vec<EqBandParam>>,
}

pub async fn create_player(
    State(state): State<AppState>,
    Json(req): Json<CreatePlayerReq>,
) -> Result<impl IntoResponse, StatusCode> {
    if state.players.contains_key(&req.id) {
        let p = state.players.get(&req.id).ok_or(StatusCode::NOT_FOUND)?;
        if !is_uri_allowed(&state.cfg, &req.uri) {
            warn!(uri=%req.uri, "URI blocked by config patterns");
            return Err(StatusCode::FORBIDDEN);
        }
        let mut uri = req.uri.clone();
        let mut prepared_path: Option<String> = None;
        if (needs_resolve(&uri) && resolver_enabled(&state.cfg)) || resolver_enabled(&state.cfg) {
            match resolve_with_retry(&state.cfg, &uri).await {
                Ok(direct) => {
                    info!(%uri, %direct, "resolved page URL to direct stream");
                    if std::path::Path::new(&direct).exists() {
                        prepared_path = Some(direct.clone());
                    }
                    uri = direct;
                }
                Err(e) => {
                    warn!(%uri, ?e, "resolver failed; enqueuing original URI");
                }
            }
        }
        let md = req.metadata.unwrap_or_else(|| serde_json::json!({}));
        let _track_id = p.enqueue_prepared(uri.clone(), prepared_path, md).await;
        return Ok((StatusCode::OK, Json(CreatePlayerRes { id: req.id })));
    }

    if !is_uri_allowed(&state.cfg, &req.uri) {
        warn!(uri=%req.uri, "URI blocked by config patterns");
        return Err(StatusCode::FORBIDDEN);
    }

    if let Ok(u) = url::Url::parse(&req.uri) {
        if let Some(h) = u.host_str() {
            if h.to_lowercase().contains("spotify.com") {
                let has_creds = state.cfg.spotify_client_id.as_ref().filter(|s| !s.is_empty()).is_some()
                    && state.cfg.spotify_client_secret.as_ref().filter(|s| !s.is_empty()).is_some();
                if !has_creds {
                    warn!(
                        uri=%req.uri,
                        "Spotify URL provided but Spotify credentials are missing. Set SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET (or configure [spotify] in Resonix.toml)."
                    );
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
        }
    }

    let mut uri = req.uri.clone();
    if (needs_resolve(&uri) && resolver_enabled(&state.cfg)) || resolver_enabled(&state.cfg) {
        match resolve_with_retry(&state.cfg, &uri).await {
            Ok(direct) => {
                info!(%uri, %direct, "resolved page URL to direct stream");
                uri = direct;
            }
            Err(e) => {
                warn!(%uri, ?e, "resolver failed; using original URI");
            }
        }
    }

    let player = Player::new(&req.id, &uri, state.cfg.clone()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let player = std::sync::Arc::new(player);
    if let Some(md) = req.metadata {
        player.set_metadata(md).await;
    }
    state.players.insert(req.id.clone(), player.clone());

    tokio::spawn(async move {
        if let Err(e) = player.run().await {
            error!(?e, "player run error");
        }
    });

    Ok((StatusCode::CREATED, Json(CreatePlayerRes { id: req.id })))
}

pub async fn play(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.play().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.pause().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_player(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let Some((_, p)) = state.players.remove(&id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    p.stop();
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_filters(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<FiltersReq>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    if let Some(v) = req.volume {
        p.set_volume(v.clamp(0.0, 5.0));
    }
    if let Some(bands) = req.eq {
        p.set_eq(bands);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct TrackInfoOut {
    pub identifier: String,
    #[serde(rename = "isSeekable")]
    pub is_seekable: bool,
    pub author: String,
    pub length: i64,
    #[serde(rename = "isStream")]
    pub is_stream: bool,
    pub position: i64,
    pub title: String,
    pub uri: String,
    #[serde(rename = "artworkUrl")]
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
    #[serde(rename = "sourceName")]
    pub source_name: String,
}

#[derive(Debug, Serialize)]
pub struct TrackOut {
    pub encoded: String,
    pub info: TrackInfoOut,
    #[serde(rename = "pluginInfo")]
    pub plugin_info: serde_json::Value,
    #[serde(rename = "userData")]
    pub user_data: serde_json::Value,
}

pub async fn list_players(State(state): State<AppState>) -> impl IntoResponse {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let engine = base64::engine::general_purpose::STANDARD;
    for p in state.players.iter() {
        let md = p.metadata().await;
        let ti = p.track_info_snapshot().await;
        out.push(serde_json::json!({
            "id": p.id().to_string(),
            "track": TrackOut {
                encoded: engine.encode(p.track_identifier()),
                info: TrackInfoOut {
                    identifier: ti.identifier,
                    is_seekable: ti.is_seekable,
                    author: ti.author,
                    length: ti.length_ms as i64,
                    is_stream: ti.is_stream,
                    position: ti.position_ms as i64,
                    title: ti.title,
                    uri: ti.uri,
                    artwork_url: ti.artwork_url,
                    isrc: ti.isrc,
                    source_name: ti.source_name,
                },
                plugin_info: serde_json::json!({}),
                user_data: md,
            }
        }));
    }
    Json(out)
}

#[derive(Debug, Deserialize)]
pub struct MetadataUpdateReq {
    pub merge: bool,
    pub value: serde_json::Value,
}

pub async fn update_metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MetadataUpdateReq>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    if req.merge {
        p.merge_metadata(req.value).await;
    } else {
        p.set_metadata(req.value).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct EnqueueReq {
    pub uri: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

pub async fn enqueue(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EnqueueReq>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    if !is_uri_allowed(&state.cfg, &req.uri) {
        return Err(StatusCode::FORBIDDEN);
    }
    let md = req.metadata.unwrap_or_else(|| serde_json::json!({}));
    let mut uri = req.uri.clone();
    let mut prepared_path: Option<String> = None;
    if (needs_resolve(&uri) && resolver_enabled(&state.cfg)) || resolver_enabled(&state.cfg) {
        match resolve_with_retry(&state.cfg, &uri).await {
            Ok(direct) => {
                info!(original=%req.uri, %direct, "resolved queue URL to direct stream");
                if std::path::Path::new(&direct).exists() {
                    prepared_path = Some(direct.clone());
                }
                uri = direct;
            }
            Err(e) => {
                warn!(uri=%req.uri, ?e, "resolver failed; enqueued original URI");
            }
        }
    }
    let track_id = p.enqueue_prepared(uri, prepared_path, md).await;
    Ok((StatusCode::CREATED, Json(serde_json::json!({"trackId": track_id}))))
}

pub async fn get_queue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let q = p.queue_snapshot().await;
    Ok(Json(q))
}

#[derive(Debug, Deserialize)]
pub struct LoopModeReq {
    pub mode: LoopMode,
}

pub async fn set_loop_mode(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<LoopModeReq>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.set_loop_mode(req.mode).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn skip(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.skip();
    Ok(StatusCode::NO_CONTENT)
}

pub async fn ws_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let rx = p.subscribe_events();
    Ok(ws.on_upgrade(move |socket| async move { ws_events_task(socket, rx).await }))
}

async fn ws_events_task(
    mut socket: axum::extract::ws::WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<crate::audio::player::PlayerEvent>,
) {
    use axum::extract::ws::Message;
    use serde_json::json;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(ev) => {
                        if socket.send(Message::Text(json!(ev).to_string().into())).await.is_err() { break; }
                    }
                    Err(_) => break,
                }
            }
            else => break,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LoadTracksQuery {
    pub identifier: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "loadType", content = "data")]
pub enum LoadResult {
    #[serde(rename = "track")]
    Track(Box<TrackOut>),
    #[serde(rename = "empty")]
    Empty(serde_json::Value),
}

pub async fn load_tracks(Query(q): Query<LoadTracksQuery>) -> impl IntoResponse {
    let engine = base64::engine::general_purpose::STANDARD;
    if q.identifier.trim().is_empty() {
        return Json(LoadResult::Empty(serde_json::json!({})));
    }
    let info = TrackInfoOut {
        identifier: q.identifier.clone(),
        is_seekable: false,
        author: String::new(),
        length: 0,
        is_stream: true,
        position: 0,
        title: q.identifier.clone(),
        uri: q.identifier.clone(),
        artwork_url: None,
        isrc: None,
        source_name: "direct".into(),
    };
    let encoded = engine.encode(q.identifier.clone());
    Json(LoadResult::Track(Box::new(TrackOut {
        encoded,
        info,
        plugin_info: serde_json::json!({}),
        user_data: serde_json::json!({}),
    })))
}

#[derive(Debug, Deserialize)]
pub struct DecodeTrackQuery {
    #[serde(rename = "encodedTrack")]
    pub encoded_track: String,
}

pub async fn decode_track(Query(q): Query<DecodeTrackQuery>) -> impl IntoResponse {
    let engine = base64::engine::general_purpose::STANDARD;
    match engine.decode(q.encoded_track) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes).to_string();
            let info = TrackInfoOut {
                identifier: s.clone(),
                is_seekable: false,
                author: String::new(),
                length: 0,
                is_stream: true,
                position: 0,
                title: s.clone(),
                uri: s.clone(),
                artwork_url: None,
                isrc: None,
                source_name: "direct".into(),
            };
            Json(TrackOut {
                encoded: engine.encode(s.clone()),
                info,
                plugin_info: serde_json::json!({}),
                user_data: serde_json::json!({}),
            })
            .into_response()
        }
        Err(_) => (StatusCode::BAD_REQUEST, "invalid base64").into_response(),
    }
}

pub async fn decode_tracks(Json(arr): Json<Vec<String>>) -> impl IntoResponse {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    for enc in arr {
        if let Ok(bytes) = engine.decode(&enc) {
            if let Ok(s) = String::from_utf8(bytes.clone()) {
                out.push(TrackOut {
                    encoded: engine.encode(s.clone()),
                    info: TrackInfoOut {
                        identifier: s.clone(),
                        is_seekable: false,
                        author: String::new(),
                        length: 0,
                        is_stream: true,
                        position: 0,
                        title: s.clone(),
                        uri: s.clone(),
                        artwork_url: None,
                        isrc: None,
                        source_name: "direct".into(),
                    },
                    plugin_info: serde_json::json!({}),
                    user_data: serde_json::json!({}),
                });
            }
        }
    }
    Json(out)
}

#[derive(Debug, Serialize)]
pub struct InfoResponse {
    version: String,
    #[serde(rename = "buildTime")]
    build_time: u64,
}

pub async fn info() -> impl IntoResponse {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let build_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    Json(InfoResponse { version, build_time: build_time })
}

pub async fn resolve_http(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cfg: &std::sync::Arc<EffectiveConfig> = &state.cfg;
    if !resolver_enabled(cfg) {
        return (StatusCode::BAD_REQUEST, "resolver disabled".to_string());
    }
    if let Some(u) = q.get("url") {
        if let Ok(parsed) = url::Url::parse(u) {
            if let Some(h) = parsed.host_str() {
                if h.to_lowercase().contains("spotify.com") {
                    let has_creds = cfg.spotify_client_id.as_ref().filter(|s| !s.is_empty()).is_some()
                        && cfg.spotify_client_secret.as_ref().filter(|s| !s.is_empty()).is_some();
                    if !has_creds {
                        let msg = "Spotify URL provided but Spotify credentials are missing. Set SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET (or configure [spotify] in Resonix.toml).";
                        warn!(url=%u, "blocked spotify resolve due to missing credentials");
                        return (StatusCode::BAD_REQUEST, msg.to_string());
                    }
                }
            }
        }
        match resolve_to_direct(&cfg, u).await {
            Ok(d) => (StatusCode::OK, d),
            Err(e) => (StatusCode::BAD_REQUEST, format!("error: {}", e)),
        }
    } else {
        (StatusCode::BAD_REQUEST, "missing url param".to_string())
    }
}

pub async fn ws_stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let rx = p.subscribe();
    info!(player_id=%id, "WS subscriber connected");
    Ok(ws.on_upgrade(move |socket| async move { ws_task(socket, rx).await }))
}

async fn ws_task(mut socket: axum::extract::ws::WebSocket, mut rx: tokio::sync::broadcast::Receiver<Bytes>) {
    let mut ws_forwarded: u64 = 0;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(pkt) => {
                        if socket.send(axum::extract::ws::Message::Binary(pkt)).await.is_err() { break; }
                        ws_forwarded += 1;
                        if ws_forwarded % 1000 == 0 { info!(ws_forwarded, "WS forwarded frames (summary)"); }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(lost = n, "WS lagged; dropped packets");
                    }
                    Err(_) => break,
                }
            }
            Some(Ok(msg)) = socket.next() => {
                match msg { axum::extract::ws::Message::Close(_) => break, _ => {} }
            }
            else => break,
        }
    }
}
