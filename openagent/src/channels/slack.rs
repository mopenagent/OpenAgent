//! Slack channel — Slack Web API + Socket Mode via zeroclaw.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [slack]
//! enabled      = true
//! bot_token    = "${SLACK_BOT_TOKEN}"    # xoxb-…
//! app_token    = "${SLACK_APP_TOKEN}"    # xapp-… (Socket Mode)
//! channel_id   = ""                      # optional: restrict to one channel
//! allowed_users = []
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::SlackChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    /// Socket Mode app-level token (xapp-…). Required for real-time events.
    #[serde(default)]
    pub app_token: String,
    /// Optional single-channel restriction.
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

/// Build a Slack channel wrapped in the observability adapter.
pub fn build(cfg: &SlackConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    let app_token = if cfg.app_token.is_empty() { None } else { Some(cfg.app_token.clone()) };
    let channel_id = if cfg.channel_id.is_empty() { None } else { Some(cfg.channel_id.clone()) };
    Arc::new(ZeroClawChannel::new(
        Inner::new(cfg.bot_token.clone(), app_token, channel_id, vec![], cfg.allowed_users.clone()),
        metrics,
    ))
}
