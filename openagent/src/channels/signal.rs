//! Signal channel — Signal-CLI JSON-RPC daemon via zeroclaw.
//!
//! Requires a running `signal-cli daemon --http` process.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [signal]
//! enabled             = true
//! http_url            = "http://127.0.0.1:8080"
//! account             = "+15551234567"   # E.164 registered number
//! group_id            = ""               # optional: restrict to one group
//! allowed_from        = []               # empty = all contacts
//! ignore_attachments  = false
//! ignore_stories      = true
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::SignalChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct SignalConfig {
    #[serde(default)]
    pub enabled: bool,
    /// signal-cli HTTP daemon base URL.
    #[serde(default)]
    pub http_url: String,
    /// Registered E.164 phone number (e.g. "+15551234567").
    #[serde(default)]
    pub account: String,
    /// Optional group ID to restrict listening to one Signal group.
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub allowed_from: Vec<String>,
    #[serde(default)]
    pub ignore_attachments: bool,
    #[serde(default = "default_true")]
    pub ignore_stories: bool,
}

fn default_true() -> bool { true }

/// Build a Signal channel wrapped in the observability adapter.
pub fn build(cfg: &SignalConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    let group_id = if cfg.group_id.is_empty() { None } else { Some(cfg.group_id.clone()) };
    Arc::new(ZeroClawChannel::new(
        Inner::new(
            cfg.http_url.clone(),
            cfg.account.clone(),
            group_id,
            cfg.allowed_from.clone(),
            cfg.ignore_attachments,
            cfg.ignore_stories,
        ),
        metrics,
    ))
}
