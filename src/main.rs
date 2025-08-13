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

fn embedded_bin_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("RESONIX_EMBED_EXTRACT_DIR") {
        return std::path::PathBuf::from(p);
    }
    std::env::temp_dir().join("resonix-embedded")
}

fn ensure_extracted(bin_name: &str, bytes: &[u8]) -> Option<std::path::PathBuf> {
    if bytes.is_empty() {
        return None;
    }
    let dir = embedded_bin_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut path = dir.join(bin_name);
    #[cfg(windows)]
    {
        if path.extension().is_none() {
            path.set_extension("exe");
        }
    }
    let write = match std::fs::metadata(&path) {
        Ok(m) => (m.len() as usize) != bytes.len() || m.len() == 0,
        Err(_) => true,
    };
    if write {
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!(?e, path=%path.display(), "failed to write embedded binary");
            return None;
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
    }
    Some(path)
}

async fn check_startup_dependencies(cfg: &crate::config::EffectiveConfig) -> Result<()> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};
    let mut ytdlp_path = cfg.ytdlp_path.clone();
    let mut ffmpeg_path = cfg.ffmpeg_path.clone();

    #[allow(unused)]
    fn validate(cmd_path: &str, arg: &str) -> bool {
        let mut c = std::process::Command::new(cmd_path);
        c.arg(arg).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        c.status().map(|s| s.success()).unwrap_or(false)
    }

    #[cfg(has_embedded_bins)]
    {
        mod embedded_bins {
            include!(env!("RESONIX_EMBED_BINS_RS"));
        }
        use embedded_bins::*;
        if (!validate(&ytdlp_path, "--version")) && !YT_DLP.is_empty() {
            if let Some(p) = ensure_extracted(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" }, YT_DLP) {
                ytdlp_path = p.to_string_lossy().to_string();
            }
        }
        if (!validate(&ffmpeg_path, "-version")) && !FFMPEG.is_empty() {
            if let Some(files) = std::option::Option::Some(EMBEDDED_FILES) {
                let dir = embedded_bin_dir();
                let _ = std::fs::create_dir_all(&dir);
                for ef in files {
                    if ef.name.eq_ignore_ascii_case("yt-dlp") || ef.name.eq_ignore_ascii_case("yt-dlp.exe") {
                        continue;
                    }
                    let out_path = dir.join(ef.name);
                    #[cfg(windows)]
                    {
                        // Keep provided extension; do not auto add .exe here.
                    }
                    let write = match std::fs::metadata(&out_path) {
                        Ok(m) => (m.len() as usize) != ef.bytes.len() || m.len() == 0,
                        Err(_) => true,
                    };
                    if write {
                        if let Err(e) = std::fs::write(&out_path, ef.bytes) {
                            tracing::warn!(?e, path=%out_path.display(), "failed to write embedded support file");
                        } else {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                if let Ok(meta) = std::fs::metadata(&out_path) {
                                    let mut perm = meta.permissions();
                                    perm.set_mode(0o755);
                                    let _ = std::fs::set_permissions(&out_path, perm);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(p) = ensure_extracted(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }, FFMPEG) {
                ffmpeg_path = p.to_string_lossy().to_string();
            }
        }
    }

    let ytdlp_ok = {
        let mut cmd = Command::new(&ytdlp_path);
        cmd.arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match timeout(Duration::from_secs(5), cmd.status()).await {
            Ok(Ok(st)) => st.success(),
            _ => false,
        }
    };
    if !ytdlp_ok {
        log_install_help("yt-dlp");
        anyhow::bail!("yt-dlp not found or not working (path: {})", ytdlp_path);
    }

    let ffmpeg_ok = {
        let mut cmd = Command::new(&ffmpeg_path);
        cmd.arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        match timeout(Duration::from_secs(5), cmd.status()).await {
            Ok(Ok(st)) => st.success(),
            _ => false,
        }
    };
    if !ffmpeg_ok {
        log_install_help("ffmpeg");
        anyhow::bail!("ffmpeg not found or not working (path: {})", ffmpeg_path);
    }

    std::env::set_var("RESONIX_FFMPEG_BIN", &ffmpeg_path);
    std::env::set_var("RESONIX_YTDLP_BIN", &ytdlp_path);

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
