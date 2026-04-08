//! In-process channels module — replaces the standalone `services/channels/` daemon.
//!
//! ## Architecture
//! Each platform lives in its own file (`telegram.rs`, `discord.rs`, …) with:
//! - A config struct (deserialised from `config/channels.toml`)
//! - A `build(cfg, metrics) -> Arc<dyn Channel>` factory
//!
//! All channels are wrapped in [`adapter::ZeroClawChannel<T>`] which adds
//! OTEL spans and metrics to every `send`, `listen`, and typing call.
//!
//! ## Startup
//! Call [`init`] once after the `ServiceManager` is ready.  It loads config,
//! builds the registry, spawns per-platform listener tasks (pushing
//! `message.received` events onto the shared broadcast bus), and returns a
//! cheap-to-clone [`ChannelHandle`] for outbound operations.
//!
//! ## WhatsApp Cloud API
//! Inbound webhook events (from Meta) are injected via [`ChannelHandle::inject_event`].
//! The Go-based `services/whatsapp/` remains the active inbound handler until
//! Meta Business approval is obtained; outbound sends via this module work
//! independently once `[whatsapp]` is configured.

pub mod adapter;
pub mod address;
pub mod cli;
pub mod config;
pub mod discord;
pub mod imessage;
pub mod irc;
pub mod mattermost;
pub mod mqtt;
pub mod reddit;
pub mod registry;
pub mod signal;
pub mod slack;
pub mod telegram;
pub mod twitter;
pub mod whatsapp;
pub mod whatsapp_storage;
pub mod whatsapp_web;

// Re-export zeroclaw channel types so ZeroClaw-merged code using
// `crate::channels::*` continues to resolve without modification.
pub use zeroclaw::channels::Channel;
pub use zeroclaw::channels::DiscordChannel;
pub use zeroclaw::channels::IMessageChannel;
pub use zeroclaw::channels::IrcChannel;
pub use zeroclaw::channels::MattermostChannel;
pub use zeroclaw::channels::SendMessage;
pub use zeroclaw::channels::SignalChannel;
pub use zeroclaw::channels::SlackChannel;
pub use zeroclaw::channels::TelegramChannel;
pub mod traits {
    pub use zeroclaw::channels::traits::*;
}

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::observability::telemetry::MetricsWriter;

use self::address::ChannelAddress;
use self::config::ChannelsConfig;
use self::registry::ChannelRegistry;

// ---- ChannelHandle ---------------------------------------------------------

/// Cheap-to-clone handle for outbound channel operations.
///
/// Backed by an `Arc<ChannelRegistry>` + a clone of the event broadcast sender
/// so webhook-sourced events (e.g. WhatsApp Cloud API) can be pushed onto the
/// same bus as polled channel events.
#[derive(Clone)]
pub struct ChannelHandle {
    registry: Arc<ChannelRegistry>,
    /// Shared event bus — same sender held by `ServiceManager`.
    event_tx: broadcast::Sender<Value>,
}

impl ChannelHandle {
    fn new(registry: Arc<ChannelRegistry>, event_tx: broadcast::Sender<Value>) -> Self {
        Self { registry, event_tx }
    }

    /// Returns a handle backed by an empty registry (all operations return errors).
    /// Used as a fallback when [`init`] fails so startup is not blocked.
    pub fn disabled() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            registry: Arc::new(ChannelRegistry::empty()),
            event_tx: tx,
        }
    }

    // ---- outbound ops -------------------------------------------------------

    /// Send a text message to a `ChannelAddress` URI (e.g. `telegram://bot/chat_id`).
    pub async fn send(&self, address: &str, content: &str) -> Result<()> {
        let addr = ChannelAddress::parse(address)?;
        let ch = self
            .registry
            .get(addr.platform())
            .ok_or_else(|| anyhow::anyhow!("no channel for platform: {}", addr.platform()))?;
        let msg = SendMessage::new(content.to_string(), addr.chat_id()).in_thread(addr.thread_id());
        ch.send(&msg).await
    }

    /// Start a typing indicator on the given address.
    pub async fn typing_start(&self, address: &str) -> Result<()> {
        let addr = ChannelAddress::parse(address)?;
        let ch = self
            .registry
            .get(addr.platform())
            .ok_or_else(|| anyhow::anyhow!("no channel for platform: {}", addr.platform()))?;
        ch.start_typing(addr.chat_id()).await
    }

    /// Stop the typing indicator on the given address.
    pub async fn typing_stop(&self, address: &str) -> Result<()> {
        let addr = ChannelAddress::parse(address)?;
        let ch = self
            .registry
            .get(addr.platform())
            .ok_or_else(|| anyhow::anyhow!("no channel for platform: {}", addr.platform()))?;
        ch.stop_typing(addr.chat_id()).await
    }

    /// Return names of all enabled platforms.
    pub fn platform_names(&self) -> Vec<String> {
        self.registry.all().iter().map(|c| c.name().to_string()).collect()
    }

    // ---- inbound injection --------------------------------------------------

    /// Push a pre-formed `message.received` event onto the dispatch bus.
    ///
    /// Used by webhook routes (e.g. `POST /webhook/whatsapp`) to inject
    /// inbound events from push-based channels (WhatsApp Cloud API, Linq, WATI)
    /// into the same broadcast bus that polled channels use.
    ///
    /// The `event` must be a JSON object with shape:
    /// ```json
    /// { "type": "event", "event": "message.received", "data": { … } }
    /// ```
    pub fn inject_event(&self, event: Value) {
        let _ = self.event_tx.send(event);
    }

    /// Return the WhatsApp Cloud API config if the channel is registered.
    /// Used by the webhook verification route.
    pub fn whatsapp_config(&self) -> whatsapp::WhatsAppConfig {
        // Config is not stored in the handle — return a default so the
        // webhook route can still check the verify_token from env.
        // A proper implementation would store cfg in ChannelHandle.
        whatsapp::WhatsAppConfig {
            verify_token: std::env::var("WHATSAPP_VERIFY_TOKEN").unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Inject all events produced by parsing a WhatsApp Cloud API webhook body.
    ///
    /// Returns the number of messages extracted.
    pub fn inject_whatsapp_webhook(&self, payload: &Value) -> usize {
        let cfg = &whatsapp::WhatsAppConfig::default(); // read from registry if needed
        // Use the registry's whatsapp channel if configured; fall back to parse-only.
        let events = if let Some(ch) = self.registry.get("whatsapp") {
            // Downcast not possible via dyn Channel — use config-based parser.
            let _ = ch; // channel is registered (outbound works)
            // Parse via the config-free helper exposed in whatsapp module.
            whatsapp::parse_webhook(cfg, payload)
        } else {
            whatsapp::parse_webhook(cfg, payload)
        };
        let count = events.len();
        for ev in events {
            self.inject_event(ev);
        }
        count
    }
}

// ---- init ------------------------------------------------------------------

/// Initialise the channels module.
///
/// - Loads `config/channels.toml` (env-interpolated).
/// - Builds [`ChannelRegistry`] from enabled platforms.
/// - Spawns per-platform listener tasks that push `message.received` events
///   onto `event_tx` — the same broadcast bus used by the `ServiceManager`.
///
/// Returns a [`ChannelHandle`] for outbound operations and webhook injection.
pub fn init(
    project_root: &std::path::Path,
    metrics: MetricsWriter,
    event_tx: broadcast::Sender<Value>,
) -> Result<ChannelHandle> {
    // Install rustls crypto provider (ring) before any TLS connection.
    rustls::crypto::ring::default_provider().install_default().ok();

    let config_path = std::env::var("OPENAGENT_CHANNELS_CONFIG").unwrap_or_else(|_| {
        project_root
            .join("config/channels.toml")
            .to_string_lossy()
            .into_owned()
    });

    let cfg = match config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "channels.config.fallback: using defaults (all disabled)");
            ChannelsConfig::default()
        }
    };

    let metrics_arc = Arc::new(metrics);
    let registry = Arc::new(ChannelRegistry::build(&cfg, Arc::clone(&metrics_arc))?);

    info!(count = registry.len(), "channels.registry.built");

    registry.spawn_listeners(event_tx.clone());

    Ok(ChannelHandle::new(registry, event_tx))
}
