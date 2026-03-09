//! Tool definitions and MCP-lite handler registration for the Discord service.

use crate::handlers::{handle_edit_message, handle_send_message};
use crate::metrics::DiscordTelemetry;
use crate::state::DiscordState;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::Arc;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "discord.status".into(),
            description: "Return current Discord service status.".into(),
            params: json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "discord.link_state".into(),
            description: "Return current Discord connection and auth state.".into(),
            params: json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "discord.send_message".into(),
            description: "Send a message to a Discord channel.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "Discord channel ID." },
                    "text":       { "type": "string", "description": "Message text." }
                },
                "required": ["channel_id", "text"]
            }),
        },
        ToolDefinition {
            name: "discord.edit_message".into(),
            description: "Edit an existing Discord message.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "Discord channel ID." },
                    "message_id": { "type": "string", "description": "Discord message ID to edit." },
                    "text":       { "type": "string", "description": "New message text." }
                },
                "required": ["channel_id", "message_id", "text"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    state: Arc<DiscordState>,
    tel: Arc<DiscordTelemetry>,
) {
    let s = Arc::clone(&state);
    server.register_tool("discord.status", move |_params| {
        Ok(s.status_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("discord.link_state", move |_params| {
        Ok(s.link_state_json().to_string())
    });

    let s = Arc::clone(&state);
    let t = Arc::clone(&tel);
    server.register_tool("discord.send_message", move |params| {
        handle_send_message(params, Arc::clone(&s), Arc::clone(&t))
    });

    let s = Arc::clone(&state);
    let t = Arc::clone(&tel);
    server.register_tool("discord.edit_message", move |params| {
        handle_edit_message(params, Arc::clone(&s), Arc::clone(&t))
    });
}
