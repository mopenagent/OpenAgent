//! WhatsApp Web channel — native Rust client via wa-rs (zeroclaw).
//!
//! Uses the unofficial WhatsApp Web protocol (same as whatsmeow in Go).
//! Full E2E encryption, QR-code or pair-code linking, no Meta approval needed.
//!
//! **Status:** Requires the `whatsapp-web` feature in zeroclaw's Cargo.toml.
//! The feature is not enabled in the current vendor build — stubs are compiled
//! so the module is always present. Enable the feature to activate.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [whatsapp_web]
//! enabled          = true
//! session_path     = "data/whatsapp_web.db"   # rusqlite session store
//! pair_phone       = "+15551234567"            # phone to pair via QR
//! allowed_numbers  = []
//! ```

use std::sync::Arc;

use serde::Deserialize;
use super::traits::Channel;

use crate::observability::telemetry::MetricsWriter;

#[derive(Debug, Default, Deserialize)]
pub struct WhatsAppWebConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Path to the rusqlite session database.
    #[serde(default = "default_session_path")]
    pub session_path: String,
    /// Phone number to pair (E.164). Triggers QR flow on first run.
    #[serde(default)]
    pub pair_phone: String,
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

fn default_session_path() -> String {
    "data/whatsapp_web.db".to_string()
}

/// Build a WhatsApp Web channel wrapped in the observability adapter.
///
/// Requires enabling `features = ["whatsapp-web"]` in vendor/zeroclaw/Cargo.toml
/// (pulls in the optional `wa-rs` crates). Returns an error until then.
pub fn build(_cfg: &WhatsAppWebConfig, _metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    // `WhatsAppWebChannel` is behind the `whatsapp-web` feature flag.
    // Attempting to build it without the feature panics at compile time.
    // This stub keeps the config path alive for when the feature is enabled.
    panic!(
        "WhatsApp Web channel requires the `whatsapp-web` feature in vendor/zeroclaw. \
         Disable [whatsapp_web] enabled = true in config/channels.toml until then."
    )
}
