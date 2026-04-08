//! Mattermost channel — Mattermost API v4 via zeroclaw.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [mattermost]
//! enabled  = true
//! url      = "https://your.mattermost.server"
//! token    = "${MATTERMOST_BOT_TOKEN}"
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::MattermostChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct MattermostConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

/// Build a Mattermost channel wrapped in the observability adapter.
pub fn build(cfg: &MattermostConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(
        Inner::new(cfg.url.clone(), cfg.token.clone(), None, vec![], false, false),
        metrics,
    ))
}
