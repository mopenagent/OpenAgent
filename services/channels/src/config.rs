//! Channels service configuration.
//!
//! Loads `config/channels.toml` (or the path in `OPENAGENT_CHANNELS_CONFIG`).
//! Before parsing, every `${VAR}` placeholder is replaced with the
//! corresponding environment variable.  `.env` is loaded by the caller before
//! [`load`] is called.

use serde::Deserialize;

/// Top-level config for the channels daemon.
#[derive(Debug, Default, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub irc: IrcConfig,
    #[serde(default)]
    pub mattermost: MattermostConfig,
    #[serde(default)]
    pub signal: SignalConfig,
    #[serde(default)]
    pub imessage: IMessageConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub mention_only: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub listen_to_bots: bool,
    #[serde(default)]
    pub mention_only: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub app_token: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct IrcConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server: String,
    #[serde(default = "default_irc_port")]
    pub port: u16,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub password: String,
}

fn default_irc_port() -> u16 {
    6667
}

#[derive(Debug, Default, Deserialize)]
pub struct MattermostConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct SignalConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cli_url: String,
    #[serde(default)]
    pub number: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct IMessageConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// Load and return [`ChannelsConfig`].
pub fn load(path: &str) -> anyhow::Result<ChannelsConfig> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read channels config {path}: {e}"))?;
    let interpolated = interpolate_env(&raw);
    let cfg: ChannelsConfig = toml::from_str(&interpolated)
        .map_err(|e| anyhow::anyhow!("invalid channels config {path}: {e}"))?;
    Ok(cfg)
}

/// Replace every `${VAR}` with the env value.  Unset vars are left as-is.
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
        std::env::set_var("_TEST_TOKEN_CH", "abc123");
        let result = interpolate_env("token = \"${_TEST_TOKEN_CH}\"");
        assert_eq!(result, "token = \"abc123\"");
        std::env::remove_var("_TEST_TOKEN_CH");
    }

    #[test]
    fn interpolate_leaves_unset_vars_unchanged() {
        let result = interpolate_env("token = \"${_UNSET_VAR_99}\"");
        assert_eq!(result, "token = \"${_UNSET_VAR_99}\"");
    }

    #[test]
    fn interpolate_handles_multiple_vars() {
        std::env::set_var("_CH_A", "hello");
        std::env::set_var("_CH_B", "world");
        let result = interpolate_env("${_CH_A} ${_CH_B}");
        assert_eq!(result, "hello world");
        std::env::remove_var("_CH_A");
        std::env::remove_var("_CH_B");
    }

    #[test]
    fn default_config_all_disabled() {
        let cfg: ChannelsConfig = toml::from_str("").unwrap();
        assert!(!cfg.telegram.enabled);
        assert!(!cfg.discord.enabled);
        assert!(!cfg.slack.enabled);
    }
}
