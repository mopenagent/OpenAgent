//! Channel registry — instantiates platforms from config and routes outbound calls.

use std::collections::HashMap;
use std::sync::Arc;

use sdk_rust::{MetricsWriter, OutboundEvent};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use zeroclaw::channels::irc::IrcChannelConfig;
use zeroclaw::channels::traits::ChannelMessage;
use zeroclaw::channels::Channel;

use crate::adapter::ZeroClawChannel;
use crate::config::ChannelsConfig;

/// Stores enabled channels keyed by platform name (e.g. "telegram").
pub struct ChannelRegistry {
    channels: HashMap<String, Arc<dyn Channel>>,
}

impl ChannelRegistry {
    /// Instantiate enabled channels from config.
    pub fn build(cfg: &ChannelsConfig, metrics: Arc<MetricsWriter>) -> anyhow::Result<Self> {
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();

        if cfg.telegram.enabled {
            let ch = zeroclaw::channels::TelegramChannel::new(
                cfg.telegram.token.clone(),
                cfg.telegram.allowed_users.clone(),
                cfg.telegram.mention_only,
            );
            channels.insert(
                "telegram".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: telegram enabled");
        }

        if cfg.discord.enabled {
            let guild_id = if cfg.discord.guild_id.is_empty() {
                None
            } else {
                Some(cfg.discord.guild_id.clone())
            };
            let ch = zeroclaw::channels::DiscordChannel::new(
                cfg.discord.token.clone(),
                guild_id,
                cfg.discord.allowed_users.clone(),
                cfg.discord.listen_to_bots,
                cfg.discord.mention_only,
            );
            channels.insert(
                "discord".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: discord enabled");
        }

        if cfg.slack.enabled {
            let app_token = if cfg.slack.app_token.is_empty() {
                None
            } else {
                Some(cfg.slack.app_token.clone())
            };
            let channel_id = if cfg.slack.channel_id.is_empty() {
                None
            } else {
                Some(cfg.slack.channel_id.clone())
            };
            let ch = zeroclaw::channels::SlackChannel::new(
                cfg.slack.bot_token.clone(),
                app_token,
                channel_id,
                vec![],
                cfg.slack.allowed_users.clone(),
            );
            channels.insert(
                "slack".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: slack enabled");
        }

        if cfg.irc.enabled {
            let ch = zeroclaw::channels::IrcChannel::new(IrcChannelConfig {
                server: cfg.irc.server.clone(),
                port: cfg.irc.port,
                nickname: cfg.irc.nickname.clone(),
                username: None,
                channels: vec![cfg.irc.channel.clone()],
                allowed_users: vec![],
                server_password: if cfg.irc.password.is_empty() {
                    None
                } else {
                    Some(cfg.irc.password.clone())
                },
                nickserv_password: None,
                sasl_password: None,
                verify_tls: true,
            });
            channels.insert(
                "irc".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: irc enabled");
        }

        if cfg.mattermost.enabled {
            let ch = zeroclaw::channels::MattermostChannel::new(
                cfg.mattermost.url.clone(),
                cfg.mattermost.token.clone(),
                None,  // channel_id
                vec![], // allowed_users
                false, // thread_replies
                false, // mention_only
            );
            channels.insert(
                "mattermost".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: mattermost enabled");
        }

        if cfg.signal.enabled {
            let ch = zeroclaw::channels::SignalChannel::new(
                cfg.signal.cli_url.clone(),
                cfg.signal.number.clone(),
                None,   // group_id
                vec![], // allowed_from
                false,  // ignore_attachments
                false,  // ignore_stories
            );
            channels.insert(
                "signal".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: signal enabled");
        }

        if cfg.imessage.enabled {
            let ch = zeroclaw::channels::IMessageChannel::new(vec![]);
            channels.insert(
                "imessage".into(),
                Arc::new(ZeroClawChannel::new(ch, Arc::clone(&metrics))),
            );
            info!("channels.registry: imessage enabled");
        }

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

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Spawn a background listener task for each enabled channel.
    ///
    /// Messages are forwarded as `message.received` MCP-lite events via `event_tx`.
    /// Each listener restarts on error with a 5s delay.
    pub fn spawn_listeners(&self, event_tx: broadcast::Sender<OutboundEvent>) {
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
                        let data = serde_json::json!({
                            "id": msg.id,
                            "sender": msg.sender,
                            "reply_target": msg.reply_target,
                            "content": msg.content,
                            "channel": msg.channel,
                            "timestamp": msg.timestamp,
                            "thread_ts": msg.thread_ts,
                        });
                        let _ = event_tx.send(OutboundEvent::new("message.received", data));
                    }

                    listen_handle.abort();
                    error!(platform = %platform, "channel.listener.restart");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });
        }
    }
}
