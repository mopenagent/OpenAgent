//! `ZeroClawChannel<T>` — instrumentation adapter wrapping any [`Channel`].
//!
//! Adding OTEL spans and metrics here means every platform automatically gets
//! observability. New channel connectors require zero changes here.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{instrument, warn};
use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::observability::telemetry::{elapsed_ms, MetricsWriter};

/// Wraps a [`Channel`] with OTEL spans and metrics.
pub struct ZeroClawChannel<T: Channel> {
    inner: T,
    metrics: Arc<MetricsWriter>,
}

impl<T: Channel> ZeroClawChannel<T> {
    pub fn new(inner: T, metrics: Arc<MetricsWriter>) -> Self {
        Self { inner, metrics }
    }
}

#[async_trait]
impl<T: Channel + Send + Sync> Channel for ZeroClawChannel<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    #[instrument(skip(self, message), fields(channel = self.inner.name()))]
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let result = self.inner.send(message).await;
        let dur = elapsed_ms(start);
        let status = if result.is_ok() { "ok" } else { "error" };
        self.metrics.record(&serde_json::json!({
            "op": "channel.send",
            "channel": self.inner.name(),
            "status": status,
            "duration_ms": dur,
        }));
        if result.is_err() {
            warn!(channel = self.inner.name(), "channel.send.error");
        }
        result
    }

    #[instrument(skip(self, tx), fields(channel = self.inner.name()))]
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        self.inner.listen(tx).await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.inner.start_typing(recipient).await
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.inner.stop_typing(recipient).await
    }

    fn supports_draft_updates(&self) -> bool {
        self.inner.supports_draft_updates()
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        self.inner.send_draft(message).await
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.inner.update_draft(recipient, message_id, text).await
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.inner.finalize_draft(recipient, message_id, text).await
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        self.inner.cancel_draft(recipient, message_id).await
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.inner.add_reaction(channel_id, message_id, emoji).await
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .remove_reaction(channel_id, message_id, emoji)
            .await
    }

    async fn pin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        self.inner.pin_message(channel_id, message_id).await
    }

    async fn unpin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        self.inner.unpin_message(channel_id, message_id).await
    }
}
