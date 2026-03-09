//! Tool definitions and MCP-lite handler registration for the Slack service.

use crate::handlers::handle_send_message;
use crate::metrics::SlackTelemetry;
use crate::state::SlackState;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::Arc;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "slack.status".into(),
            description: "Return current Slack service status.".into(),
            params: json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "slack.link_state".into(),
            description: "Return Slack bot authorization and connection state.".into(),
            params: json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "slack.send_message".into(),
            description: concat!(
                "Send a message to a Slack channel via chat.postMessage. ",
                "Returns the message timestamp (ts) on success."
            )
            .into(),
            params: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "Slack channel ID (e.g. C01234ABCDE)." },
                    "text":       { "type": "string", "description": "Message text (plain text or mrkdwn)." }
                },
                "required": ["channel_id", "text"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    state: Arc<SlackState>,
    tel: Arc<SlackTelemetry>,
) {
    let s = Arc::clone(&state);
    server.register_tool("slack.status", move |_params| {
        Ok(s.status_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("slack.link_state", move |_params| {
        Ok(s.link_state_json().to_string())
    });

    let s = Arc::clone(&state);
    let t = Arc::clone(&tel);
    server.register_tool("slack.send_message", move |params| {
        handle_send_message(params, Arc::clone(&s), Arc::clone(&t))
    });
}
