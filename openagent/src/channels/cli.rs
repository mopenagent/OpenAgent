//! CLI channel — stdin/stdout terminal bridge via zeroclaw.
//!
//! Zero configuration required. Useful for local testing without any
//! platform credentials. Type `/quit` or `/exit` to stop the listener.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [cli]
//! enabled = true
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::CliChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct CliConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// Build a CLI (stdin/stdout) channel wrapped in the observability adapter.
pub fn build(metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(Inner, metrics))
}
