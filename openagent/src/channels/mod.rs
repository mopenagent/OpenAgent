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
pub mod media_pipeline;
pub mod mqtt;
pub mod reddit;
pub mod registry;
pub mod signal;
pub mod slack;
pub mod stall_watchdog;
pub mod telegram;
pub mod transcription;
pub mod tts;
pub mod twitter;
pub mod whatsapp;
pub mod whatsapp_storage;
pub mod whatsapp_web;

/// Prefix used by `SlackChannel::send` to detect Block Kit JSON payloads.
/// Messages starting with this prefix are parsed as Slack Block Kit JSON.
pub const BLOCK_KIT_PREFIX: &str = "BLOCK_KIT_JSON:";

pub mod traits;

// Re-export core channel types from local traits module.
pub use traits::Channel;
pub use traits::ChannelMessage;
pub use traits::SendMessage;

// Re-export concrete channel types.
pub use discord::DiscordChannel;
pub use imessage::IMessageChannel;
pub use irc::IrcChannel;
pub use mattermost::MattermostChannel;
pub use signal::SignalChannel;
pub use slack::SlackChannel;
pub use telegram::TelegramChannel;

/// Strip XML-style tool-call tags from agent output before forwarding to users.
///
/// Removes `<function_calls>…</function_calls>` and related tag pairs that are
/// internal protocol and must not be forwarded to end users on any channel.
pub(crate) fn strip_tool_call_tags(message: &str) -> String {
    const TOOL_CALL_OPEN_TAGS: [&str; 7] = [
        "<function_calls>",
        "<function_call>",
        "<tool_call>",
        "<toolcall>",
        "<tool-call>",
        "<tool>",
        "<invoke>",
    ];

    fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
        tags.iter()
            .filter_map(|tag| haystack.find(tag).map(|idx| (idx, *tag)))
            .min_by_key(|(idx, _)| *idx)
    }

    fn matching_close_tag(open_tag: &str) -> Option<&'static str> {
        match open_tag {
            "<function_calls>" => Some("</function_calls>"),
            "<function_call>" => Some("</function_call>"),
            "<tool_call>" => Some("</tool_call>"),
            "<toolcall>" => Some("</toolcall>"),
            "<tool-call>" => Some("</tool-call>"),
            "<tool>" => Some("</tool>"),
            "<invoke>" => Some("</invoke>"),
            _ => None,
        }
    }

    fn extract_first_json_end(input: &str) -> Option<usize> {
        let trimmed = input.trim_start();
        let trim_offset = input.len().saturating_sub(trimmed.len());

        for (byte_idx, ch) in trimmed.char_indices() {
            if ch != '{' && ch != '[' {
                continue;
            }

            let slice = &trimmed[byte_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            if let Some(Ok(_value)) = stream.next() {
                let consumed = stream.byte_offset();
                if consumed > 0 {
                    return Some(trim_offset + byte_idx + consumed);
                }
            }
        }

        None
    }

    fn strip_leading_close_tags(mut input: &str) -> &str {
        loop {
            let trimmed = input.trim_start();
            if !trimmed.starts_with("</") {
                return trimmed;
            }

            let Some(close_end) = trimmed.find('>') else {
                return "";
            };
            input = &trimmed[close_end + 1..];
        }
    }

    let mut kept_segments = Vec::new();
    let mut remaining = message;

    while let Some((start, open_tag)) = find_first_tag(remaining, &TOOL_CALL_OPEN_TAGS) {
        let before = &remaining[..start];
        if !before.is_empty() {
            kept_segments.push(before.to_string());
        }

        let Some(close_tag) = matching_close_tag(open_tag) else {
            break;
        };
        let after_open = &remaining[start + open_tag.len()..];

        if let Some(close_idx) = after_open.find(close_tag) {
            remaining = &after_open[close_idx + close_tag.len()..];
            continue;
        }

        if let Some(consumed_end) = extract_first_json_end(after_open) {
            remaining = strip_leading_close_tags(&after_open[consumed_end..]);
            continue;
        }

        kept_segments.push(remaining[start..].to_string());
        remaining = "";
        break;
    }

    if !remaining.is_empty() {
        kept_segments.push(remaining.to_string());
    }

    let mut result = kept_segments.concat();

    // Clean up any resulting blank lines (but preserve paragraphs)
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
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
