//! CLI channel — stdin/stdout terminal bridge.
//!
//! Zero configuration required. Useful for local testing without any
//! platform credentials. Type `/quit` or `/exit` to stop the listener.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [cli]
//! enabled = true
//! ```

use serde::Deserialize;

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CliConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// Build a CLI (stdin/stdout) channel.
pub fn build(_cfg: &CliConfig) -> CliChannel {
    CliChannel::new()
}

/// CLI channel — stdin/stdout, always available, zero deps
#[derive(Debug)]
pub struct CliChannel;

impl CliChannel {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn name(&self) -> &str {
        "cli"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        println!("{}", message.content);
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if line == "/quit" || line == "/exit" {
                break;
            }

            let msg = ChannelMessage {
                id: Uuid::new_v4().to_string(),
                sender: "user".to_string(),
                reply_target: "user".to_string(),
                content: line,
                channel: "cli".to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            };

            if tx.send(msg).await.is_err() {
                break;
            }
        }
        Ok(())
    }
}
