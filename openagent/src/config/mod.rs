//! OpenAgent runtime configuration.
//!
//! Loaded from `config/openagent.toml` at startup. Missing file or missing
//! sections fall back to built-in defaults so the binary starts without any
//! config file present.
//!
//! Values containing `${VAR}` are resolved from environment variables at load
//! time. Environment variables set directly also override any field via the
//! `OPENAGENT_` prefix convention where noted.
//!
//! # File layout
//! - `channel_types.rs` — shared channel config types: `StreamMode`,
//!   `TranscriptionConfig`, proxy helpers, WS helper. Used by channel
//!   implementations in `crate::channels`.

pub mod channel_types;

// Re-export channel types so `crate::config::StreamMode` etc. resolve.
pub use channel_types::{
    apply_channel_proxy_to_builder, build_channel_proxy_client,
    build_channel_proxy_client_with_timeouts, build_runtime_proxy_client,
    ws_connect_with_proxy, AssemblyAiSttConfig, DeepgramSttConfig, GoogleSttConfig,
    LocalWhisperConfig, OpenAiSttConfig, StreamMode, TranscriptionConfig,
};

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Env-var resolution
// ---------------------------------------------------------------------------

/// Resolve `${VAR}` tokens in a string from the process environment.
pub fn resolve_env(s: &str) -> String {
    let mut result = s.to_string();
    let mut start = 0;
    while let Some(open) = result[start..].find("${") {
        let open = start + open;
        if let Some(close) = result[open..].find('}') {
            let close = open + close;
            let var = &result[open + 2..close];
            let value = std::env::var(var).unwrap_or_default();
            result.replace_range(open..=close, &value);
            start = open + value.len();
        } else {
            break;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// "openai_compat" | "anthropic" | "openai"
    #[serde(default = "default_provider_kind")]
    pub kind: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_timeout")]
    pub timeout: f64,
    #[serde(default)]
    pub debug_llm: bool,
}

fn default_provider_kind() -> String {
    "openai_compat".to_string()
}
fn default_timeout() -> f64 {
    120.0
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: default_provider_kind(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            timeout: default_timeout(),
            debug_llm: false,
        }
    }
}

impl ProviderConfig {
    fn resolve(mut self) -> Self {
        self.kind = std::env::var("OPENAGENT_PROVIDER_KIND")
            .unwrap_or_else(|_| resolve_env(&self.kind));
        self.base_url = std::env::var("OPENAGENT_LLM_BASE_URL")
            .unwrap_or_else(|_| resolve_env(&self.base_url));
        self.api_key = std::env::var("OPENAGENT_API_KEY")
            .unwrap_or_else(|_| resolve_env(&self.api_key));
        self.model = std::env::var("OPENAGENT_MODEL")
            .unwrap_or_else(|_| resolve_env(&self.model));
        if let Ok(t) = std::env::var("OPENAGENT_LLM_TIMEOUT") {
            if let Ok(v) = t.parse() {
                self.timeout = v;
            }
        }
        if let Ok(d) = std::env::var("OPENAGENT_DEBUG_LLM") {
            self.debug_llm = matches!(d.as_str(), "1" | "true" | "yes");
        }
        self
    }

    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if !self.kind.is_empty() {
            m.insert("OPENAGENT_PROVIDER_KIND".into(), self.kind.clone());
        }
        if !self.base_url.is_empty() {
            m.insert("OPENAGENT_LLM_BASE_URL".into(), self.base_url.clone());
        }
        if !self.api_key.is_empty() {
            m.insert("OPENAGENT_API_KEY".into(), self.api_key.clone());
        }
        if !self.model.is_empty() {
            m.insert("OPENAGENT_MODEL".into(), self.model.clone());
        }
        m.insert("OPENAGENT_LLM_TIMEOUT".into(), self.timeout.to_string());
        m.insert(
            "OPENAGENT_DEBUG_LLM".into(),
            if self.debug_llm { "1" } else { "0" }.into(),
        );
        m
    }
}

// ---------------------------------------------------------------------------
// Guard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct GuardConfig {
    #[serde(default = "default_guard_enabled")]
    pub enabled: bool,
    #[serde(default = "default_guard_db_path")]
    pub db_path: String,
}

fn default_guard_enabled() -> bool {
    true
}
fn default_guard_db_path() -> String {
    "data/guard.db".to_string()
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            enabled: default_guard_enabled(),
            db_path: default_guard_db_path(),
        }
    }
}

impl GuardConfig {
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("GUARD_DB_PATH".into(), self.db_path.clone());
        m
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SttConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TtsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_voice")]
    pub voice: String,
    #[serde(default = "default_speed")]
    pub speed: f64,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_voice() -> String {
    "af_sarah".to_string()
}
fn default_speed() -> f64 {
    1.0
}
fn default_language() -> String {
    "en-us".to_string()
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: default_voice(),
            speed: default_speed(),
            language: default_language(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_concurrency_limit")]
    pub max_concurrent: usize,
}

fn default_concurrency_limit() -> usize {
    50
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_concurrency_limit(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MiddlewareConfig {
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub tts: TtsConfig,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServicesConfig {
    /// Service names that should not be started even if their binary exists.
    #[serde(default)]
    pub disabled: Vec<String>,
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CronConfig {
    /// Whether the cron scheduler is active.
    #[serde(default)]
    pub enabled: bool,
    /// How often (in seconds) the scheduler polls for due jobs.
    #[serde(default = "default_cron_poll_secs")]
    pub poll_secs: u64,
    /// SQLite database path (relative to project root).
    /// Defaults to the same DB as sessions so cron_jobs and cron_runs
    /// live alongside the rest of OpenAgent's persistent state.
    #[serde(default = "default_cron_db_path")]
    pub db_path: String,
}

fn default_cron_poll_secs() -> u64 {
    30
}
fn default_cron_db_path() -> String {
    "data/openagent.db".to_string()
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_secs: default_cron_poll_secs(),
            db_path: default_cron_db_path(),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAgentConfig {
    /// Enable debug-level log output. When `true` the log filter is set to
    /// `debug` unless `RUST_LOG` overrides it. Defaults to `false` (info).
    #[serde(default)]
    pub debug: bool,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub guard: GuardConfig,
    #[serde(default)]
    pub middleware: MiddlewareConfig,
    #[serde(default)]
    pub services: ServicesConfig,
    /// All channel configurations. Replaces the old `[platforms]` section.
    /// Each sub-key maps to a channel (e.g. `[channels.telegram]`).
    #[serde(default)]
    pub channels: crate::channels::config::ChannelsConfig,
    #[serde(default)]
    pub cron: CronConfig,
}

/// Load `config/openagent.toml` relative to `project_root`, apply env var
/// overrides, and resolve `${VAR}` tokens.
///
/// Missing file → all defaults.
pub fn load(project_root: &Path) -> Result<OpenAgentConfig> {
    let path = project_root.join("config").join("openagent.toml");
    let mut cfg = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str::<OpenAgentConfig>(&raw)?
    } else {
        OpenAgentConfig::default()
    };
    cfg.provider = cfg.provider.resolve();
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty_toml() {
        let cfg: OpenAgentConfig = toml::from_str("").expect("empty toml should parse");
        assert!(!cfg.middleware.stt.enabled);
        assert!(!cfg.middleware.tts.enabled);
        assert_eq!(cfg.middleware.tts.voice, "af_sarah");
        assert!((cfg.middleware.tts.speed - 1.0).abs() < f64::EPSILON);
        assert_eq!(cfg.provider.kind, "openai_compat");
        assert!(cfg.guard.enabled);
    }

    #[test]
    fn parses_middleware_flags() {
        let raw = r#"
            [middleware.stt]
            enabled = true

            [middleware.tts]
            enabled = true
            voice = "af_nicole"
            speed = 1.2
        "#;
        let cfg: OpenAgentConfig = toml::from_str(raw).expect("should parse");
        assert!(cfg.middleware.stt.enabled);
        assert!(cfg.middleware.tts.enabled);
        assert_eq!(cfg.middleware.tts.voice, "af_nicole");
    }

    #[test]
    fn missing_file_returns_defaults() {
        let tmp = std::env::temp_dir().join("nonexistent_openagent_cfg_xyz");
        let cfg = load(&tmp).expect("missing file should not error");
        assert!(!cfg.middleware.stt.enabled);
        assert!(cfg.guard.enabled);
    }

    #[test]
    fn resolve_env_no_tokens() {
        assert_eq!(resolve_env("hello world"), "hello world");
    }

    #[test]
    fn resolve_env_missing_var_becomes_empty() {
        let result = resolve_env("${OPENAGENT_TEST_VAR_UNLIKELY_XYZ}");
        assert_eq!(result, "");
    }
}
