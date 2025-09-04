use anyhow::Result;
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
use crate::utils::stdu::format_ram_mb;
use crate::utils::tools::{ensure_all, tools_home_dir};

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
    let cfg = load_config();
    let logs_dir = std::path::Path::new(".logs");
    if !logs_dir.exists() {
        let _ = std::fs::create_dir_all(logs_dir);
    }
    if cfg.clean_log_on_start {
        let log_path = logs_dir.join("latest.log");
        if let Ok(f) = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&log_path) {
            drop(f);
        }
    }
    let file_appender = rolling::never(".logs", "latest.log");
    let (file_nb, _guard_file) = tracing_appender::non_blocking(file_appender);
    let stdout_layer = fmt::layer().with_target(false).compact();
    let file_layer = fmt::layer().with_ansi(false).with_target(false).with_writer(file_nb).compact();
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry().with(env_filter).with(stdout_layer).with(file_layer).init();

    if let Err(e) = check_startup_dependencies(&cfg).await {
        error!(?e, "Dependency check failed");
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

    // Best-effort cleanup of temporary audio files created during runtime.
    crate::audio::source::cleanup_resonix_temp_files();

    Ok(())
}

async fn check_startup_dependencies(cfg: &crate::config::EffectiveConfig) -> Result<()> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};
    let mut ytdlp_path = cfg.ytdlp_path.clone();
    let mut ffmpeg_path = cfg.ffmpeg_path.clone();

    async fn validate(bin: &str, arg: &str) -> bool {
        let mut cmd = Command::new(bin);
        cmd.arg(arg).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match timeout(Duration::from_secs(5), cmd.status()).await {
            Ok(Ok(st)) => st.success(),
            _ => false,
        }
    }

    let need_ytdlp_download = !validate(&ytdlp_path, "--version").await;
    let need_ffmpeg_download = !validate(&ffmpeg_path, "-version").await;

    if need_ytdlp_download || need_ffmpeg_download {
        let (ytdlp_dl, ffmpeg_dl) = ensure_all(need_ytdlp_download, need_ffmpeg_download).await?;
        if let Some(p) = ytdlp_dl {
            ytdlp_path = p.to_string_lossy().to_string();
        }
        if let Some(p) = ffmpeg_dl {
            ffmpeg_path = p.to_string_lossy().to_string();
        }
    }

    let ytdlp_ok = validate(&ytdlp_path, "--version").await;
    if !ytdlp_ok {
        log_install_help("yt-dlp");
        anyhow::bail!("yt-dlp missing; attempted path {}", ytdlp_path);
    }
    let ffmpeg_ok = validate(&ffmpeg_path, "-version").await;
    if !ffmpeg_ok {
        log_install_help("ffmpeg");
        anyhow::bail!("ffmpeg missing; attempted path {}", ffmpeg_path);
    }

    std::env::set_var("RESONIX_FFMPEG_BIN", &ffmpeg_path);
    std::env::set_var("RESONIX_YTDLP_BIN", &ytdlp_path);
    std::env::set_var("RESONIX_TOOLS_DIR", tools_home_dir());
    Ok(())
}

fn log_install_help(tool: &str) {
    if cfg!(target_os = "windows") {
        error!(
            %tool,
            "Required dependency '{}' is missing. Install it with winget or Chocolatey and ensure it's in PATH.\nwinget (recommended): winget install -e --id {}\nChocolatey: choco install {}",
            tool,
            match tool { "yt-dlp" => "yt-dlp.yt-dlp", "ffmpeg" => "Gyan.FFmpeg", _ => tool },
            tool
        );
    } else if cfg!(target_os = "macos") {
        error!(
            %tool,
            "Required dependency '{}' is missing. Install it via Homebrew.\nbrew install {}",
            tool,
            tool
        );
    } else {
        error!(
            %tool,
            "Required dependency '{}' is missing. Install it via your package manager.\nDebian/Ubuntu: sudo apt update && sudo apt install -y {}\nArch: sudo pacman -S {}\nFedora: sudo dnf install -y {}",
            tool,
            tool,
            tool,
            tool
        );
    }
}
