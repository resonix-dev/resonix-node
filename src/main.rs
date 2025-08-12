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
mod config;
mod middleware;
mod resolver;
mod state;
mod utils;

use crate::api::handlers::{
    create_player, delete_player, pause, play, resolve_http, update_filters, ws_stream,
};
use crate::config::load_config;
use crate::middleware::auth::auth_middleware;
use crate::state::AppState;
use crate::utils::format_ram_mb;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let cfg = load_config();
    // Ensure .logs directory exists and set up file logging to latest.log
    let logs_dir = std::path::Path::new(".logs");
    if !logs_dir.exists() {
        let _ = std::fs::create_dir_all(logs_dir);
    }
    // Clean logfile on startup if enabled
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

    // Verify required external dependencies before proceeding
    if let Err(e) = check_startup_dependencies(&cfg).await {
        error!(?e, "Dependency check failed");
        std::process::exit(1);
    }

    // Startup banner with system info
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
        .route("/players", post(create_player))
        .route("/players/{id}/play", post(play))
        .route("/players/{id}/pause", post(pause))
        .route("/players/{id}", delete(delete_player))
        .route("/players/{id}/filters", patch(update_filters))
        .route("/players/{id}/ws", get(ws_stream))
        .route("/resolve", get(resolve_http))
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

    Ok(())
}

async fn check_startup_dependencies(cfg: &crate::config::EffectiveConfig) -> Result<()> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    let ytdlp_ok = {
        let mut cmd = Command::new(&cfg.ytdlp_path);
        cmd.arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match timeout(Duration::from_secs(5), cmd.status()).await {
            Ok(Ok(status)) => status.success(),
            _ => false,
        }
    };
    if !ytdlp_ok {
        log_install_help("yt-dlp");
        anyhow::bail!("yt-dlp not found or not working (path: {})", cfg.ytdlp_path);
    }

    let ffmpeg_ok = {
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match timeout(Duration::from_secs(5), cmd.status()).await {
            Ok(Ok(status)) => status.success(),
            _ => false,
        }
    };
    if !ffmpeg_ok {
        log_install_help("ffmpeg");
        anyhow::bail!("ffmpeg not found or not working (expected in PATH)");
    }

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
