use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a helpful assistant. Use tools only when necessary. Be concise.";

#[derive(Debug, Clone, Deserialize)]
pub struct CortexConfig {
    #[serde(default)]
    pub provider: ProviderConfig,
    /// Optional fast-path provider for simple turns.
    /// When set, the query classifier routes short/tool_call turns here instead of
    /// the main (strong) provider. Omit to send all turns to the main provider.
    #[serde(default)]
    pub fast_provider: Option<ProviderConfig>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    /// Root directory for per-session diary markdown files.
    #[serde(default = "default_diary_path")]
    pub diary_path: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { diary_path: default_diary_path() }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_provider_kind")]
    pub kind: String,
    #[serde(default = "default_provider_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_provider_timeout")]
    pub timeout: f64,
    #[serde(default = "default_provider_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub debug_llm: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_agent_name")]
    pub name: String,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedStepConfig {
    pub provider: ProviderConfig,
    /// Fast-path provider, if configured. When present, the classifier may select
    /// this instead of `provider` for simple/tool_call turns.
    pub fast_provider: Option<ProviderConfig>,
    pub agent_name: String,
    pub system_prompt: String,
    pub source_path: PathBuf,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            fast_provider: None,
            agents: vec![AgentConfig::default()],
            memory: MemoryConfig::default(),
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: default_provider_kind(),
            base_url: default_provider_base_url(),
            api_key: String::new(),
            model: String::new(),
            timeout: default_provider_timeout(),
            max_tokens: default_provider_max_tokens(),
            debug_llm: false,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: default_agent_name(),
            system_prompt: default_system_prompt(),
        }
    }
}

impl CortexConfig {
    pub fn load() -> Result<ResolvedConfigFile> {
        let path = resolve_config_path();
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;

        apply_provider_env_overrides(&mut cfg.provider);

        if cfg.agents.is_empty() {
            cfg.agents.push(AgentConfig::default());
        }

        Ok(ResolvedConfigFile { cfg, path })
    }

    pub fn resolve_step_config(
        &self,
        path: PathBuf,
        agent_name: Option<&str>,
    ) -> ResolvedStepConfig {
        let agent = agent_name
            .and_then(|name| self.agents.iter().find(|agent| agent.name == name))
            .or_else(|| self.agents.first())
            .cloned()
            .unwrap_or_default();

        ResolvedStepConfig {
            provider: self.provider.clone(),
            fast_provider: self.fast_provider.clone(),
            agent_name: agent.name,
            system_prompt: agent.system_prompt,
            source_path: path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedConfigFile {
    pub cfg: CortexConfig,
    pub path: PathBuf,
}

fn resolve_config_path() -> PathBuf {
    if let Ok(path) = env::var("OPENAGENT_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    let config = Path::new("config/openagent.toml");
    if config.exists() {
        return config.to_path_buf();
    }

    PathBuf::from("config/openagent.toml")
}

fn apply_provider_env_overrides(provider: &mut ProviderConfig) {
    if let Ok(value) = env::var("OPENAGENT_PROVIDER_KIND") {
        provider.kind = value;
    }
    if let Ok(value) = env::var("OPENAGENT_LLM_BASE_URL") {
        provider.base_url = value;
    }
    if let Ok(value) = env::var("OPENAGENT_API_KEY") {
        provider.api_key = value;
    }
    if let Ok(value) = env::var("OPENAGENT_MODEL") {
        provider.model = value;
    }
    if let Ok(value) = env::var("OPENAGENT_LLM_TIMEOUT") {
        if let Ok(parsed) = value.parse::<f64>() {
            provider.timeout = parsed;
        }
    }
    if let Ok(value) = env::var("OPENAGENT_DEBUG_LLM") {
        provider.debug_llm = matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES");
    }
}

fn default_provider_kind() -> String {
    "openai_compat".to_string()
}

fn default_provider_base_url() -> String {
    "http://100.74.210.70:1234/v1".to_string()
}

fn default_provider_timeout() -> f64 {
    60.0
}

fn default_provider_max_tokens() -> u32 {
    2048
}

fn default_diary_path() -> String {
    "data/diary".to_string()
}

fn default_agent_name() -> String {
    "default".to_string()
}

fn default_system_prompt() -> String {
    DEFAULT_SYSTEM_PROMPT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_step_config_uses_named_agent_when_present() {
        let cfg = CortexConfig {
            provider: ProviderConfig::default(),
            fast_provider: None,
            agents: vec![
                AgentConfig {
                    name: "primary".to_string(),
                    system_prompt: "Primary prompt".to_string(),
                },
                AgentConfig {
                    name: "secondary".to_string(),
                    system_prompt: "Secondary prompt".to_string(),
                },
            ],
            memory: MemoryConfig::default(),
        };

        let resolved =
            cfg.resolve_step_config(PathBuf::from("config/openagent.toml"), Some("secondary"));
        assert_eq!(resolved.agent_name, "secondary");
        assert_eq!(resolved.system_prompt, "Secondary prompt");
    }
}
