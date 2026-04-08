//! iMessage channel — macOS AppleScript bridge via zeroclaw.
//!
//! Reads the macOS Messages SQLite database and sends via AppleScript.
//! Only available on macOS with Messages.app running.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [imessage]
//! enabled           = true
//! allowed_contacts  = []   # empty = all contacts
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::IMessageChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct IMessageConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Allowed contact handles (phone/email). Empty = all contacts.
    #[serde(default)]
    pub allowed_contacts: Vec<String>,
}

/// Build an iMessage channel wrapped in the observability adapter.
pub fn build(cfg: &IMessageConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(
        Inner::new(cfg.allowed_contacts.clone()),
        metrics,
    ))
}
