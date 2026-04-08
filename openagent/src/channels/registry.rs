//! Channel registry — instantiates platforms from config and routes outbound calls.
//!
//! Each enabled platform is built via its module's `build()` factory, wrapped
//! in [`super::adapter::ZeroClawChannel`] for uniform OTEL spans + metrics,
//! and stored as `Arc<dyn Channel>` keyed by platform name.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use zeroclaw::channels::traits::ChannelMessage;
use zeroclaw::channels::Channel;

use crate::observability::telemetry::MetricsWriter;

use super::config::ChannelsConfig;
use super::{cli, discord, imessage, irc, mattermost, signal, slack, telegram, whatsapp, whatsapp_web};

/// Stores enabled channels keyed by platform name (e.g. `"telegram"`).
pub struct ChannelRegistry {
    channels: HashMap<String, Arc<dyn Channel>>,
}

impl ChannelRegistry {
    /// Return an empty registry (no platforms enabled).
    pub fn empty() -> Self {
        Self { channels: HashMap::new() }
    }

    /// Instantiate all enabled channels from config.
    pub fn build(cfg: &ChannelsConfig, metrics: Arc<MetricsWriter>) -> anyhow::Result<Self> {
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();

        macro_rules! register {
            ($enabled:expr, $name:literal, $build:expr) => {
                if $enabled {
                    channels.insert($name.into(), $build);
                    info!("channels.registry: {} enabled", $name);
                }
            };
        }

        register!(cfg.telegram.enabled,    "telegram",    telegram::build(&cfg.telegram,    Arc::clone(&metrics)));
        register!(cfg.discord.enabled,     "discord",     discord::build(&cfg.discord,      Arc::clone(&metrics)));
        register!(cfg.slack.enabled,       "slack",       slack::build(&cfg.slack,          Arc::clone(&metrics)));
        register!(cfg.signal.enabled,      "signal",      signal::build(&cfg.signal,        Arc::clone(&metrics)));
        register!(cfg.irc.enabled,         "irc",         irc::build(&cfg.irc,              Arc::clone(&metrics)));
        register!(cfg.mattermost.enabled,  "mattermost",  mattermost::build(&cfg.mattermost,Arc::clone(&metrics)));
        register!(cfg.imessage.enabled,    "imessage",    imessage::build(&cfg.imessage,    Arc::clone(&metrics)));
        register!(cfg.cli.enabled,         "cli",         cli::build(                        Arc::clone(&metrics)));

        // WhatsApp Cloud API — outbound sends work immediately;
        // inbound requires the webhook route at POST /webhook/whatsapp.
        // The Go service (services/whatsapp/) handles inbound until Meta
        // Business approval is obtained.
        register!(cfg.whatsapp.enabled, "whatsapp",
            whatsapp::build(&cfg.whatsapp, Arc::clone(&metrics)));

        // WhatsApp Web (wa-rs) — unofficial protocol, QR-code auth.
        // Requires zeroclaw `whatsapp-web` feature.
        register!(cfg.whatsapp_web.enabled, "whatsapp_web",
            whatsapp_web::build(&cfg.whatsapp_web, Arc::clone(&metrics)));

        // Stubs not yet implemented (reddit, twitter, mqtt) are intentionally
        // omitted from the registry — they have no Channel impl yet.

        Ok(Self { channels })
    }

    /// Look up a channel by platform name.
    pub fn get(&self, platform: &str) -> Option<Arc<dyn Channel>> {
        self.channels.get(platform).cloned()
    }

    /// Return all enabled channels.
    pub fn all(&self) -> Vec<Arc<dyn Channel>> {
        self.channels.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Spawn a background listener task for each enabled channel.
    ///
    /// Messages are forwarded as `message.received` events via `event_tx`
    /// (same broadcast bus used by the ServiceManager).
    pub fn spawn_listeners(&self, event_tx: broadcast::Sender<Value>) {
        for (platform, channel) in &self.channels {
            let channel = Arc::clone(channel);
            let event_tx = event_tx.clone();
            let platform = platform.clone();

            tokio::spawn(async move {
                loop {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(64);

                    let listen_ch = Arc::clone(&channel);
                    let platform_inner = platform.clone();
                    let listen_handle = tokio::spawn(async move {
                        if let Err(e) = listen_ch.listen(tx).await {
                            warn!(platform = %platform_inner, error = %e, "channel.listen.error");
                        }
                    });

                    while let Some(msg) = rx.recv().await {
                        let event = serde_json::json!({
                            "type":  "event",
                            "event": "message.received",
                            "data": {
                                "id":           msg.id,
                                "sender":       msg.sender,
                                "reply_target": msg.reply_target,
                                "content":      msg.content,
                                "channel":      msg.channel,
                                "timestamp":    msg.timestamp,
                                "thread_ts":    msg.thread_ts,
                            }
                        });
                        let _ = event_tx.send(event);
                    }

                    listen_handle.abort();
                    error!(platform = %platform, "channel.listener.restart");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
    }
}
