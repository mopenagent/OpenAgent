//! Discord channel — Discord Bot Gateway via zeroclaw.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [discord]
//! enabled       = true
//! token         = "${DISCORD_BOT_TOKEN}"
//! guild_id      = ""       # optional — restrict to one server
//! allowed_users = []       # empty = all users
//! listen_to_bots = false
//! mention_only   = false
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::DiscordChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    /// Restrict to a specific guild (server). Empty = all guilds.
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Whether to respond to messages from other bots.
    #[serde(default)]
    pub listen_to_bots: bool,
    /// Only respond when @mentioned.
    #[serde(default)]
    pub mention_only: bool,
}

/// Build a Discord channel wrapped in the observability adapter.
pub fn build(cfg: &DiscordConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    let guild_id = if cfg.guild_id.is_empty() { None } else { Some(cfg.guild_id.clone()) };
    Arc::new(ZeroClawChannel::new(
        Inner::new(cfg.token.clone(), guild_id, cfg.allowed_users.clone(), cfg.listen_to_bots, cfg.mention_only),
        metrics,
    ))
}
