//! Socket Mode push-event dispatcher — routes inbound Slack messages to the event bus.
//!
//! Only user-authored messages are forwarded; bot messages, edits, and deletions
//! (events with a `subtype`) are silently dropped — matching the Go implementation.

use crate::state::SlackState;
use rvstruct::ValueStruct;
use sdk_rust::OutboundEvent;
use slack_morphism::prelude::*;
use std::sync::Arc;
use tracing::info;

/// Socket Mode push-event handler registered with `SlackSocketModeListenerCallbacks`.
///
/// Extracts `channel_id`, `user_id`, `text`, and `ts` from `Message` events,
/// then publishes a `slack.message.received` event to the MCP-lite bus.
pub async fn push_events_handler(
    event: SlackPushEventCallback,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = {
        let guard = states.read().await;
        guard
            .get_user_state::<Arc<SlackState>>()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("missing SlackState in user state"))?
    };

    if let SlackEventCallbackBody::Message(msg) = event.event {
        // Skip bot messages and edits/deletions (subtype present means non-user-post)
        if msg.subtype.is_some() || msg.sender.bot_id.is_some() {
            return Ok(());
        }

        let channel_id = msg
            .origin
            .channel
            .as_ref()
            .map(|c| c.value().to_string())
            .unwrap_or_default();

        let user_id = msg
            .sender
            .user
            .as_ref()
            .map(|u| u.value().to_string())
            .unwrap_or_default();

        let text = msg
            .content
            .as_ref()
            .and_then(|c| c.text.clone())
            .unwrap_or_default();

        if channel_id.is_empty() || text.is_empty() {
            return Ok(());
        }

        let ts = msg
            .message
            .as_ref()
            .map(|m| m.ts.value().to_string())
            .or_else(|| msg.previous_message.as_ref().map(|m| m.ts.value().to_string()))
            .unwrap_or_else(|| msg.origin.ts.value().to_string());

        info!(channel_id = %channel_id, user_id = %user_id, "slack.message.received");

        let data = serde_json::json!({
            "channel_id": channel_id,
            "user_id":    user_id,
            "text":       text,
            "ts":         ts,
            "team_id":    event.team_id.value(),
        });
        let _ = state.event_tx.send(OutboundEvent::new("slack.message.received", data));
    }

    Ok(())
}
