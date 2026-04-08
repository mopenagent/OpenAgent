//! Telegram channel — Telegram Bot API via zeroclaw.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [telegram]
//! enabled      = true
//! token        = "${TELEGRAM_BOT_TOKEN}"
//! allowed_users = []         # empty = pairing-code flow
//! mention_only = false
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::TelegramChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    /// Allowed Telegram user IDs or usernames. Empty = pairing-code flow.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Only respond when the bot is @mentioned.
    #[serde(default)]
    pub mention_only: bool,
}

/// Build a Telegram channel wrapped in the observability adapter.
pub fn build(cfg: &TelegramConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(
        Inner::new(cfg.token.clone(), cfg.allowed_users.clone(), cfg.mention_only),
        metrics,
    ))
}
