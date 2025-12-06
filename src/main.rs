use anyhow::{bail, Result};
use axum::{
    routing::{delete, get, patch, post},
    Router,
};
use sysinfo::System;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use tracing_appender::rolling;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod api;
mod audio;
mod cli;
mod config;
mod middleware;
mod resolver;
mod state;
mod utils;

use crate::api::handlers::{
    create_player, decode_track, decode_tracks, delete_player, enqueue, get_queue, info, list_players,
    load_tracks, pause, play, resolve_http, set_loop_mode, skip, update_filters, update_metadata, ws_events,
    ws_stream,
};
use crate::config::load_config;
use crate::middleware::auth::auth_middleware;
use crate::state::AppState;
use crate::utils::{ffmpeg, stdu::format_ram_mb};

#[tokio::main]
async fn main() -> Result<()> {
    match crate::cli::parse_args() {
        crate::cli::CliAction::PrintVersion => {
            crate::cli::print_version();
            return Ok(());
        }
        crate::cli::CliAction::InitConfig => {
            crate::cli::init_config_file();
            return Ok(());
        }
        crate::cli::CliAction::RunServer => { /* continue */ }
    }
    let mut cfg = load_config();
    let logs_dir_str = std::env::var("RESONIX_LOG_DIR").unwrap_or_else(|_| ".logs".into());
    let logs_dir = std::path::Path::new(&logs_dir_str);

    let stdout_layer = fmt::layer().with_target(false).compact();
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(env_filter).with(stdout_layer);

    match std::fs::create_dir_all(logs_dir) {
        Ok(()) => {
            if cfg.clean_log_on_start {
                let log_path = logs_dir.join("latest.log");
                if let Ok(f) =
                    std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&log_path)
                {
                    drop(f);
                }
            }
            let file_appender = rolling::never(logs_dir, "latest.log");
            let (file_nb, _guard_file) = tracing_appender::non_blocking(file_appender);
            let file_layer = fmt::layer().with_ansi(false).with_target(false).with_writer(file_nb).compact();
            registry.with(file_layer).init();
        }
        Err(e) => {
            eprintln!("File logging disabled (cannot create {}): {}", logs_dir.display(), e);
            registry.init();
        }
    }

    if let Err(e) = ensure_ffmpeg_available(&mut cfg).await {
        error!(?e, path = %cfg.ffmpeg_path, "ffmpeg missing or unusable");
        std::process::exit(1);
    }

    let mut sys = System::new_all();
    sys.refresh_all();
    let version = env!("CARGO_PKG_VERSION");
    let os = System::name().unwrap_or_else(|| "Unknown OS".into());
    let os_ver = System::os_version().unwrap_or_default();
    let total_mem_mb = (sys.total_memory() / (1024 * 1024)) as u64;
    let cpu_brand = sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_else(|| "Unknown CPU".into());
    if total_mem_mb == 0 {
        warn!("Unable to determine RAM size");
    }

    info!(
        version,
        os = %format!("{} {}", os, os_ver),
        cpu = %cpu_brand,
        ram_mb = %format_ram_mb(total_mem_mb),
        "Resonix server starting"
    );

    let state = AppState::new(cfg.clone());

    let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);
    ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    })
    .ok();

    let app = Router::new()
        .route("/v0/players", post(create_player))
        .route("/v0/players", get(list_players))
        .route("/v0/players/{id}/play", post(play))
        .route("/v0/players/{id}/pause", post(pause))
        .route("/v0/players/{id}", delete(delete_player))
        .route("/v0/players/{id}/filters", patch(update_filters))
        .route("/v0/players/{id}/metadata", patch(update_metadata))
        .route("/v0/players/{id}/ws", get(ws_stream))
        .route("/v0/players/{id}/events", get(ws_events))
        .route("/v0/players/{id}/queue", post(enqueue))
        .route("/v0/players/{id}/queue", get(get_queue))
        .route("/v0/players/{id}/loop", patch(set_loop_mode))
        .route("/v0/players/{id}/skip", post(skip))
        .route("/v0/resolve", get(resolve_http))
        .route("/v0/loadtracks", get(load_tracks))
        .route("/v0/decodetrack", get(decode_track))
        .route("/v0/decodetracks", post(decode_tracks))
        .route("/info", get(info))
        .route("/version", get(version))
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth_middleware));

    let bind_addr = (state.cfg.host.as_str(), state.cfg.port);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    info!(addr = %format!("{}:{}", state.cfg.host, state.cfg.port), "Listening");

    tokio::select! {
        res = axum::serve(listener, app) => {
            if let Err(e) = res { tracing::error!(?e, "server error"); }
        }
        _ = shutdown_rx.recv() => { info!("Shutdown signal received"); }
    }

    crate::audio::source::cleanup_resonix_temp_files();

    Ok(())
}

async fn ensure_ffmpeg_available(cfg: &mut crate::config::EffectiveConfig) -> Result<()> {
    if check_ffmpeg(&cfg.ffmpeg_path).await.is_ok() {
        return Ok(());
    }

    warn!(path = %cfg.ffmpeg_path, "Configured ffmpeg binary is not available; attempting automatic install");

    let fallback_path = ffmpeg::default_ffmpeg_binary_path()?;
    if std::path::Path::new(&cfg.ffmpeg_path) != fallback_path.as_path() {
        let fallback_path_str = fallback_path.to_string_lossy().into_owned();
        if check_ffmpeg(&fallback_path_str).await.is_ok() {
            cfg.ffmpeg_path = fallback_path_str;
            info!(path = %cfg.ffmpeg_path, "Using bundled ffmpeg binary");
            return Ok(());
        }
    }

    let downloaded_path = ffmpeg::download_latest_ffmpeg().await?;
    cfg.ffmpeg_path = downloaded_path.to_string_lossy().into_owned();
    check_ffmpeg(&cfg.ffmpeg_path).await?;
    info!(path = %cfg.ffmpeg_path, "Downloaded ffmpeg binary");

    Ok(())
}

async fn check_ffmpeg(path: &str) -> Result<()> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    let status = timeout(
        Duration::from_secs(5),
        Command::new(path)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("ffmpeg check timed out"))??;

    if !status.success() {
        bail!("ffmpeg command '{}' exited with status {}", path, status);
    }

    Ok(())
}
