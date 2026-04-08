//! WhatsApp Cloud API channel — Meta Business API via zeroclaw.
//!
//! ## Mode: Cloud API (this file)
//! Uses the official Meta Cloud API. Outbound messages go via REST to
//! `graph.facebook.com`. Inbound messages arrive via Meta webhooks POSTed
//! to `POST /webhook/whatsapp` in the Axum server.
//!
//! **Current deployment:** The Go-based `services/whatsapp/` (whatsmeow) is
//! the active inbound handler. This channel is wired for outbound sends and
//! webhook parsing. Switch `dispatch.rs` platform routing once Meta Business
//! approval is obtained.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [whatsapp]
//! enabled          = true
//! access_token     = "${WHATSAPP_ACCESS_TOKEN}"    # Meta permanent token
//! phone_number_id  = "${WHATSAPP_PHONE_NUMBER_ID}" # from Meta dashboard
//! verify_token     = "${WHATSAPP_VERIFY_TOKEN}"    # webhook challenge secret
//! allowed_numbers  = []   # E.164 numbers, empty = all (use with caution)
//! ```
//!
//! ## Migration path from Go service
//! - Go service (whatsmeow): QR-code auth, unofficial WhatsApp Web protocol,
//!   full media support, no Meta approval needed.
//! - Cloud API (this): official Meta API, requires Business verification,
//!   webhook-based inbound, production-grade reliability.
//!
//! When ready to migrate:
//! 1. Enable this config block.
//! 2. Register the webhook URL with Meta (`POST /webhook/whatsapp`).
//! 3. In `dispatch.rs`, the `platform == "whatsapp"` branch already uses
//!    `channel_handle.send()` — no dispatch changes needed once this channel
//!    is registered.
//! 4. Disable the Go `services/whatsapp/` service in `config/openagent.toml`.

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::Channel;
use zeroclaw::channels::WhatsAppChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Meta permanent access token.
    #[serde(default)]
    pub access_token: String,
    /// Phone number ID from the Meta Business dashboard.
    #[serde(default)]
    pub phone_number_id: String,
    /// Secret token used to verify webhook challenge from Meta.
    #[serde(default)]
    pub verify_token: String,
    /// Allowed E.164 phone numbers. Empty or `["*"]` = allow all.
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

/// Build a WhatsApp Cloud API channel wrapped in the observability adapter.
///
/// Outbound sends work immediately. Inbound requires a running webhook
/// handler at `POST /webhook/whatsapp` that calls
/// [`parse_webhook`] and injects events onto the dispatch bus.
pub fn build(cfg: &WhatsAppConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(
        Inner::new(
            cfg.access_token.clone(),
            cfg.phone_number_id.clone(),
            cfg.verify_token.clone(),
            cfg.allowed_numbers.clone(),
        ),
        metrics,
    ))
}

/// Parse a raw Meta webhook POST body into `message.received` event payloads.
///
/// Called from the Axum webhook route. Each returned [`serde_json::Value`]
/// is ready to be sent on the `ServiceManager` broadcast bus so the dispatch
/// loop picks it up exactly like any other channel event.
pub fn parse_webhook(
    cfg: &WhatsAppConfig,
    payload: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let inner = Inner::new(
        cfg.access_token.clone(),
        cfg.phone_number_id.clone(),
        cfg.verify_token.clone(),
        cfg.allowed_numbers.clone(),
    );
    inner
        .parse_webhook_payload(payload)
        .into_iter()
        .map(|msg| {
            serde_json::json!({
                "type":  "event",
                "event": "message.received",
                "data": {
                    "id":      msg.id,
                    "sender":  msg.sender,
                    "content": msg.content,
                    "channel": format!("whatsapp://{}", msg.channel),
                    "timestamp": msg.timestamp,
                }
            })
        })
        .collect()
}
