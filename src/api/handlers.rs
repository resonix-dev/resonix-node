use anyhow::Result;
use axum::{extract::{Path, State, WebSocketUpgrade}, http::StatusCode, response::IntoResponse, Json};
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::audio::player::{Player, EqBandParam};
use crate::config::{EffectiveConfig, resolver_enabled};
use crate::resolver::{is_uri_allowed, needs_resolve, resolve_to_direct};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreatePlayerReq {
    pub id: String,
    pub uri: String,
}

#[derive(Debug, Serialize)]
pub struct CreatePlayerRes { pub id: String }

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
    if state.players.contains_key(&req.id) { return Err(StatusCode::CONFLICT); }

    if !is_uri_allowed(&state.cfg, &req.uri) {
        warn!(uri=%req.uri, "URI blocked by config patterns");
        return Err(StatusCode::FORBIDDEN);
    }

    let mut uri = req.uri.clone();
    if (needs_resolve(&uri) && resolver_enabled(&state.cfg)) || resolver_enabled(&state.cfg) {
        match resolve_to_direct(&state.cfg, &uri).await {
            Ok(direct) => { info!(%uri, %direct, "resolved page URL to direct stream"); uri = direct; }
            Err(e) => { warn!(%uri, ?e, "resolver failed; using original URI"); }
        }
    }

    let player = Player::new(&req.id, &uri).map_err(|_| StatusCode::BAD_REQUEST)?;
    let player = std::sync::Arc::new(player);
    state.players.insert(req.id.clone(), player.clone());

    tokio::spawn(async move {
        if let Err(e) = player.run().await { error!(?e, "player run error"); }
    });

    Ok((StatusCode::CREATED, Json(CreatePlayerRes { id: req.id })))
}

pub async fn play(State(state): State<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.play().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn pause(State(state): State<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    p.pause().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_player(State(state): State<AppState>, Path(id): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let Some((_, p)) = state.players.remove(&id) else { return Err(StatusCode::NOT_FOUND); };
    p.stop();
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_filters(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<FiltersReq>,
) -> Result<impl IntoResponse, StatusCode> {
    let p = state.players.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    if let Some(v) = req.volume { p.set_volume(v.clamp(0.0, 5.0)); }
    if let Some(bands) = req.eq { p.set_eq(bands); }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resolve_http(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cfg: &std::sync::Arc<EffectiveConfig> = &state.cfg;
    if !resolver_enabled(cfg) { return (StatusCode::BAD_REQUEST, "resolver disabled".to_string()); }
    if let Some(u) = q.get("url") {
        match resolve_to_direct(&cfg, u).await {
            Ok(d) => (StatusCode::OK, d),
            Err(e) => (StatusCode::BAD_REQUEST, format!("error: {}", e)),
        }
    } else { (StatusCode::BAD_REQUEST, "missing url param".to_string()) }
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

async fn ws_task(
    mut socket: axum::extract::ws::WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<Bytes>,
) {
    if socket
        .send(axum::extract::ws::Message::Binary(Bytes::from(vec![0u8; 3840])))
        .await
        .is_err()
    { return; }
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
