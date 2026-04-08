//! IRC channel — IRC over TLS via zeroclaw.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [irc]
//! enabled   = true
//! server    = "irc.libera.chat"
//! port      = 6697
//! nickname  = "openagent"
//! channel   = "#your-channel"
//! password  = ""   # server password or NickServ password
//! ```

use std::sync::Arc;

use serde::Deserialize;
use zeroclaw::channels::irc::IrcChannelConfig;
use zeroclaw::channels::Channel;
use zeroclaw::channels::IrcChannel as Inner;

use crate::observability::telemetry::MetricsWriter;

use super::adapter::ZeroClawChannel;

#[derive(Debug, Default, Deserialize)]
pub struct IrcConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server: String,
    #[serde(default = "default_irc_port")]
    pub port: u16,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub password: String,
}

fn default_irc_port() -> u16 { 6667 }

/// Build an IRC channel wrapped in the observability adapter.
pub fn build(cfg: &IrcConfig, metrics: Arc<MetricsWriter>) -> Arc<dyn Channel> {
    Arc::new(ZeroClawChannel::new(
        Inner::new(IrcChannelConfig {
            server: cfg.server.clone(),
            port: cfg.port,
            nickname: cfg.nickname.clone(),
            username: None,
            channels: vec![cfg.channel.clone()],
            allowed_users: vec![],
            server_password: if cfg.password.is_empty() { None } else { Some(cfg.password.clone()) },
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        }),
        metrics,
    ))
}
