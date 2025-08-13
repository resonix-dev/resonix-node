use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RawConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub resolver: ResolverConfig,
    #[serde(default)]
    pub spotify: SpotifyConfig,
    #[serde(default)]
    pub sources: SourcesConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub password: Option<String>,
}
fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    2333
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { host: default_host(), port: default_port(), password: None }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_clean_log")]
    pub clean_log_on_start: bool,
}
fn default_clean_log() -> bool {
    true
}
impl Default for LoggingConfig {
    fn default() -> Self {
        Self { clean_log_on_start: default_clean_log() }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolverConfig {
    #[serde(default = "default_resolver_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub ytdlp_path: Option<String>,
    #[serde(default)]
    pub ffmpeg_path: Option<String>,
    #[serde(default = "default_resolve_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_preferred_format")]
    pub preferred_format: String,
    #[serde(default = "default_allow_spotify_title_search")]
    pub allow_spotify_title_search: bool,
}
fn default_resolver_enabled() -> bool {
    false
}
fn default_resolve_timeout() -> u64 {
    20_000
}
fn default_preferred_format() -> String {
    "140".into()
}
fn default_allow_spotify_title_search() -> bool {
    true
}
impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            enabled: default_resolver_enabled(),
            ytdlp_path: None,
            ffmpeg_path: None,
            timeout_ms: default_resolve_timeout(),
            preferred_format: default_preferred_format(),
            allow_spotify_title_search: default_allow_spotify_title_search(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SpotifyConfig {
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SourcesConfig {
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub blocked: Vec<String>,
}

#[derive(Clone)]
pub struct EffectiveConfig {
    pub host: String,
    pub port: u16,
    pub clean_log_on_start: bool,
    pub resolver_enabled: bool,
    pub ytdlp_path: String,
    pub ffmpeg_path: String,
    pub resolve_timeout_ms: u64,
    pub preferred_format: String,
    pub allow_spotify_title_search: bool,
    pub allow_patterns: Vec<Regex>,
    pub block_patterns: Vec<Regex>,
    pub password: Option<String>,
    pub spotify_client_id: Option<String>,
    pub spotify_client_secret: Option<String>,
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# Resonix Node Configuration

[server]
# Host/IP to bind. Default: 0.0.0.0
host = "0.0.0.0"
# Port to bind. Default: 2333
port = 2333
# Optional password required in the Authorization header for all requests. Default: unset (no auth)
# password = "supersecret"

[logging]
# Truncate .logs/latest.log on startup. Default: true
clean_log_on_start = true

[resolver]
# Enable resolver/downloader for non-direct sources (YouTube/Spotify). Default: false
enabled = true
# Optional custom path to yt-dlp executable. Default: "yt-dlp"
ytdlp_path = "yt-dlp"
# Timeout for resolve/download operations in milliseconds. Default: 20000
timeout_ms = 20000
# Preferred format code for yt-dlp (e.g. 140 = m4a). Default: "140"
preferred_format = "140"
# If true, Spotify URLs are resolved by title via yt-dlp's YouTube search. Default: true
allow_spotify_title_search = true

[spotify]
# --- Spotify Credentials ---
# To enable Spotify source support, you must set these to valid values from your Spotify Developer Portal.
# - You can set environment variables and reference them here or set the values directly.
# See: https://developer.spotify.com/dashboard
client_id = "SPOTIFY_CLIENT_ID"
client_secret = "SPOTIFY_CLIENT_SECRET"

[sources]
# Regex patterns that are allowed. If empty, all are allowed unless blocked.
# Match is tested against both the full URI and the hostname.
# Example: only allow YouTube and local files
# allowed = ["(^|.*)(youtube\\.com|youtu\\.be)(/|$)"]
allowed = []

# Regex patterns that are blocked. These take priority over allowed.
# Example: block SoundCloud completely
# blocked = ["(^|.*)soundcloud\\.com(/|$)"]
blocked = []"#;

pub fn load_config() -> EffectiveConfig {
    let _ = dotenvy::dotenv();

    let mut raw: RawConfig = RawConfig {
        server: Default::default(),
        logging: Default::default(),
        resolver: Default::default(),
        spotify: Default::default(),
        sources: Default::default(),
    };

    let config_paths = ["resonix.toml", "Resonix.toml"];
    let config_exists = config_paths.iter().any(|path| std::path::Path::new(path).exists());

    if !config_exists {
        if let Err(e) = std::fs::write("resonix.toml", DEFAULT_CONFIG_TEMPLATE) {
            tracing::warn!(?e, "Failed to create default config file");
        } else {
            tracing::info!("Created default config file at resonix.toml");
        }
    }

    // Try to load existing or newly created config
    if let Ok(contents) =
        std::fs::read_to_string("resonix.toml").or_else(|_| std::fs::read_to_string("Resonix.toml"))
    {
        match toml::from_str::<RawConfig>(&contents) {
            Ok(parsed) => raw = parsed,
            Err(e) => tracing::warn!(?e, "Failed to parse resonix config; using defaults"),
        }
    }

    let resolver_env =
        std::env::var("RESONIX_RESOLVE").ok().map(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let ytdlp_env = std::env::var("YTDLP_PATH").ok();
    let ffmpeg_env = std::env::var("FFMPEG_PATH").ok();
    let timeout_env = std::env::var("RESOLVE_TIMEOUT_MS").ok().and_then(|s| s.parse().ok());

    let allow_patterns = raw.sources.allowed.iter().filter_map(|p| Regex::new(p).ok()).collect::<Vec<_>>();
    let block_patterns = raw.sources.blocked.iter().filter_map(|p| Regex::new(p).ok()).collect::<Vec<_>>();

    fn env_or_literal(val: &Option<String>, fallback_env: &str) -> Option<String> {
        if let Some(s) = val {
            if let Ok(v) = std::env::var(s) {
                return Some(v);
            }
            return Some(s.clone());
        }
        std::env::var(fallback_env).ok()
    }

    let spotify_client_id = env_or_literal(&raw.spotify.client_id, "SPOTIFY_CLIENT_ID");
    let spotify_client_secret = env_or_literal(&raw.spotify.client_secret, "SPOTIFY_CLIENT_SECRET");

    EffectiveConfig {
        host: raw.server.host,
        port: raw.server.port,
        clean_log_on_start: raw.logging.clean_log_on_start,
        resolver_enabled: resolver_env.unwrap_or(raw.resolver.enabled),
        ytdlp_path: ytdlp_env.unwrap_or_else(|| raw.resolver.ytdlp_path.unwrap_or_else(|| "yt-dlp".into())),
        ffmpeg_path: ffmpeg_env
            .unwrap_or_else(|| raw.resolver.ffmpeg_path.clone().unwrap_or_else(|| "ffmpeg".into())),
        resolve_timeout_ms: timeout_env.unwrap_or(raw.resolver.timeout_ms),
        preferred_format: raw.resolver.preferred_format,
        allow_spotify_title_search: raw.resolver.allow_spotify_title_search,
        allow_patterns,
        block_patterns,
        password: raw.server.password,
        spotify_client_id,
        spotify_client_secret,
    }
}

pub fn resolver_enabled(cfg: &EffectiveConfig) -> bool {
    cfg.resolver_enabled
}
