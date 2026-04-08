//! Top-level channels configuration — aggregates all per-channel config structs.
//!
//! Loaded as `[channels]` within `config/openagent.toml` via `OpenAgentConfig`.
//! Every `${VAR}` placeholder in the TOML is resolved by `crate::config::load()`.
//!
//! Per-channel config structs live in their own modules and are re-exported here
//! so `registry.rs` can `use crate::channels::config::*`.

use serde::Deserialize;

// Per-channel config structs live in their own modules; re-exported here
// so registry.rs can `use crate::channels::config::*` to get everything.
pub use super::cli::CliConfig;
pub use super::discord::DiscordConfig;
pub use super::imessage::IMessageConfig;
pub use super::mqtt::MqttConfig;
pub use super::reddit::RedditConfig;
pub use super::signal::SignalConfig;
pub use super::slack::SlackConfig;
pub use super::telegram::TelegramConfig;
pub use super::twitter::TwitterConfig;
pub use super::whatsapp::WhatsAppConfig;
pub use super::whatsapp_web::WhatsAppWebConfig;

// Configs that don't yet have dedicated channel files stay inline here.
pub use super::irc::IrcConfig;
pub use super::mattermost::MattermostConfig;

/// Top-level config for the channels module.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub signal: SignalConfig,
    #[serde(default)]
    pub imessage: IMessageConfig,
    #[serde(default)]
    pub cli: CliConfig,
    #[serde(default)]
    pub irc: IrcConfig,
    #[serde(default)]
    pub mattermost: MattermostConfig,
    #[serde(default)]
    pub whatsapp: WhatsAppConfig,
    #[serde(default)]
    pub whatsapp_web: WhatsAppWebConfig,
    // Stubs — not yet implemented
    #[serde(default)]
    pub mqtt: MqttConfig,
    #[serde(default)]
    pub reddit: RedditConfig,
    #[serde(default)]
    pub twitter: TwitterConfig,
}

/// Load and return [`ChannelsConfig`] from a TOML file.
pub fn load(path: &str) -> anyhow::Result<ChannelsConfig> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read channels config {path}: {e}"))?;
    let interpolated = interpolate_env(&raw);
    let cfg: ChannelsConfig = toml::from_str(&interpolated)
        .map_err(|e| anyhow::anyhow!("invalid channels config {path}: {e}"))?;
    Ok(cfg)
}

/// Replace every `${VAR}` with the env value. Unset vars are left as-is.
pub fn interpolate_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        if let Some(end) = rest.find('}') {
            let var = &rest[..end];
            match std::env::var(var) {
                Ok(val) => out.push_str(&val),
                Err(_) => {
                    out.push_str("${");
                    out.push_str(var);
                    out.push('}');
                }
            }
            rest = &rest[end + 1..];
        } else {
            out.push_str("${");
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_resolves_set_vars() {
        std::env::set_var("_TEST_TOKEN_CH2", "abc123");
        let result = interpolate_env("token = \"${_TEST_TOKEN_CH2}\"");
        assert_eq!(result, "token = \"abc123\"");
        std::env::remove_var("_TEST_TOKEN_CH2");
    }

    #[test]
    fn interpolate_leaves_unset_vars_unchanged() {
        let result = interpolate_env("token = \"${_UNSET_VAR_99}\"");
        assert_eq!(result, "token = \"${_UNSET_VAR_99}\"");
    }

    #[test]
    fn default_config_all_disabled() {
        let cfg: ChannelsConfig = toml::from_str("").unwrap();
        assert!(!cfg.telegram.enabled);
        assert!(!cfg.discord.enabled);
        assert!(!cfg.slack.enabled);
        assert!(!cfg.whatsapp.enabled);
        assert!(!cfg.cli.enabled);
    }
}
