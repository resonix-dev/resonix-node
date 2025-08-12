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
            timeout_ms: default_resolve_timeout(),
            preferred_format: default_preferred_format(),
            allow_spotify_title_search: default_allow_spotify_title_search(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SpotifyConfig {
    // Values can be either direct secrets or names of environment variables to resolve from .env/ENV
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
    pub resolve_timeout_ms: u64,
    pub preferred_format: String,
    pub allow_spotify_title_search: bool,
    pub allow_patterns: Vec<Regex>,
    pub block_patterns: Vec<Regex>,
    pub password: Option<String>,
    pub spotify_client_id: Option<String>,
    pub spotify_client_secret: Option<String>,
}

pub fn load_config() -> EffectiveConfig {
    // Load .env first so env lookups can find user-provided values
    let _ = dotenvy::dotenv();

    let mut raw: RawConfig = RawConfig {
        server: Default::default(),
        logging: Default::default(),
        resolver: Default::default(),
        spotify: Default::default(),
        sources: Default::default(),
    };
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
    let timeout_env = std::env::var("RESOLVE_TIMEOUT_MS").ok().and_then(|s| s.parse().ok());

    let allow_patterns = raw.sources.allowed.iter().filter_map(|p| Regex::new(p).ok()).collect::<Vec<_>>();
    let block_patterns = raw.sources.blocked.iter().filter_map(|p| Regex::new(p).ok()).collect::<Vec<_>>();

    // Helper: if the provided string matches an existing env var name, use its value; otherwise treat it as a literal secret.
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
