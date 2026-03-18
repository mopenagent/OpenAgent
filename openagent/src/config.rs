/// Openagent Rust runtime configuration.
///
/// Loaded from `config/openagent.toml` at startup.  Missing file or missing
/// sections fall back to built-in defaults so the binary starts without any
/// config file present.
///
/// Values containing `${VAR}` are resolved from environment variables at load
/// time.  Environment variables set directly also override any field via the
/// `OPENAGENT_` prefix convention where noted.
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Env-var resolution
// ---------------------------------------------------------------------------

/// Resolve `${VAR}` tokens in a string from the process environment.
/// Returns the original string unchanged if no tokens are found.
fn resolve_env(s: &str) -> String {
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
    /// Apply env var overrides and resolve ${VAR} tokens.
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

    /// Build a map of env vars to inject into the Cortex service process.
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
        m.insert(
            "OPENAGENT_LLM_TIMEOUT".into(),
            self.timeout.to_string(),
        );
        m.insert(
            "OPENAGENT_DEBUG_LLM".into(),
            if self.debug_llm { "1" } else { "0" }.into(),
        );
        m
    }
}

// ---------------------------------------------------------------------------
// Platforms
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DiscordPlatformConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TelegramPlatformConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_hash: String,
    #[serde(default)]
    pub bot_token: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SlackPlatformConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub app_token: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WhatsAppPlatformConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub phone_number: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PlatformsConfig {
    #[serde(default)]
    pub discord: DiscordPlatformConfig,
    #[serde(default)]
    pub telegram: TelegramPlatformConfig,
    #[serde(default)]
    pub slack: SlackPlatformConfig,
    #[serde(default)]
    pub whatsapp: WhatsAppPlatformConfig,
}

impl PlatformsConfig {
    /// Resolve ${VAR} tokens and env var overrides for all platform tokens.
    fn resolve(mut self) -> Self {
        self.discord.token = std::env::var("DISCORD_TOKEN")
            .unwrap_or_else(|_| resolve_env(&self.discord.token));
        self.telegram.app_id = std::env::var("TELEGRAM_APP_ID")
            .unwrap_or_else(|_| resolve_env(&self.telegram.app_id));
        self.telegram.app_hash = std::env::var("TELEGRAM_APP_HASH")
            .unwrap_or_else(|_| resolve_env(&self.telegram.app_hash));
        self.telegram.bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .unwrap_or_else(|_| resolve_env(&self.telegram.bot_token));
        self.slack.bot_token = std::env::var("SLACK_BOT_TOKEN")
            .unwrap_or_else(|_| resolve_env(&self.slack.bot_token));
        self.slack.app_token = std::env::var("SLACK_APP_TOKEN")
            .unwrap_or_else(|_| resolve_env(&self.slack.app_token));
        self.whatsapp.phone_number = std::env::var("WHATSAPP_PHONE")
            .unwrap_or_else(|_| resolve_env(&self.whatsapp.phone_number));
        self
    }

    /// Build per-service env var maps to inject into service processes.
    pub fn discord_env(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if !self.discord.token.is_empty() {
            m.insert("DISCORD_TOKEN".into(), self.discord.token.clone());
        }
        m
    }

    pub fn telegram_env(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if !self.telegram.app_id.is_empty() {
            m.insert("TELEGRAM_APP_ID".into(), self.telegram.app_id.clone());
        }
        if !self.telegram.app_hash.is_empty() {
            m.insert("TELEGRAM_APP_HASH".into(), self.telegram.app_hash.clone());
        }
        if !self.telegram.bot_token.is_empty() {
            m.insert("TELEGRAM_BOT_TOKEN".into(), self.telegram.bot_token.clone());
        }
        m
    }

    pub fn slack_env(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if !self.slack.bot_token.is_empty() {
            m.insert("SLACK_BOT_TOKEN".into(), self.slack.bot_token.clone());
        }
        if !self.slack.app_token.is_empty() {
            m.insert("SLACK_APP_TOKEN".into(), self.slack.app_token.clone());
        }
        m
    }

    pub fn whatsapp_env(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if !self.whatsapp.phone_number.is_empty() {
            m.insert(
                "WHATSAPP_PHONE".into(),
                self.whatsapp.phone_number.clone(),
            );
        }
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
    /// Enable the STT middleware. When false the layer is a no-op.
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
    /// Enable the TTS middleware. When false the layer is a no-op.
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
    /// Max concurrent in-flight requests allowed at once.
    /// Excess requests are backpressured; combined with TimeoutLayer they fail
    /// after 130s — prevents connector floods from overwhelming Cortex.
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

/// `[services]` block — operator-level service enable/disable list.
///
/// ```toml
/// [services]
/// disabled = ["tts", "stt"]   # services to skip entirely on startup
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServicesConfig {
    /// Service names that should not be started even if their binary exists.
    #[serde(default)]
    pub disabled: Vec<String>,
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAgentConfig {
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub platforms: PlatformsConfig,
    #[serde(default)]
    pub guard: GuardConfig,
    #[serde(default)]
    pub middleware: MiddlewareConfig,
    #[serde(default)]
    pub services: ServicesConfig,
}

/// Load `config/openagent.toml` relative to `project_root`, apply env var
/// overrides, and resolve `${VAR}` tokens throughout.
///
/// Missing file → all defaults (binary runs without any config file).
/// Parse errors are returned as `Err`.
pub fn load(project_root: &Path) -> Result<OpenAgentConfig> {
    let path = project_root.join("config").join("openagent.toml");
    let mut cfg = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str::<OpenAgentConfig>(&raw)?
    } else {
        OpenAgentConfig::default()
    };
    // Resolve env vars in sections that carry secrets.
    cfg.provider = cfg.provider.resolve();
    cfg.platforms = cfg.platforms.resolve();
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
        assert_eq!(cfg.middleware.tts.language, "en-us");
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
            language = "fr-fr"
        "#;
        let cfg: OpenAgentConfig = toml::from_str(raw).expect("should parse");
        assert!(cfg.middleware.stt.enabled);
        assert!(cfg.middleware.tts.enabled);
        assert_eq!(cfg.middleware.tts.voice, "af_nicole");
        assert_eq!(cfg.middleware.tts.language, "fr-fr");
    }

    #[test]
    fn parses_provider() {
        let raw = r#"
            [provider]
            kind = "anthropic"
            model = "claude-sonnet-4-6"
            timeout = 60.0
        "#;
        let cfg: OpenAgentConfig = toml::from_str(raw).expect("should parse");
        assert_eq!(cfg.provider.kind, "anthropic");
        assert_eq!(cfg.provider.model, "claude-sonnet-4-6");
    }

    #[test]
    fn parses_platforms() {
        let raw = r#"
            [platforms.discord]
            enabled = true
            token = "test-token"

            [platforms.slack]
            enabled = true
            bot_token = "xoxb-test"
            app_token = "xapp-test"
        "#;
        let cfg: OpenAgentConfig = toml::from_str(raw).expect("should parse");
        assert!(cfg.platforms.discord.enabled);
        assert_eq!(cfg.platforms.discord.token, "test-token");
        assert_eq!(cfg.platforms.slack.bot_token, "xoxb-test");
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
        // Env var almost certainly not set in test environment.
        let result = resolve_env("${OPENAGENT_TEST_VAR_UNLIKELY_XYZ}");
        assert_eq!(result, "");
    }
}
