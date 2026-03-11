use crate::handlers::{handle_describe_boundary, handle_step};
use crate::metrics::CortexTelemetry;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::Arc;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "cortex.describe_boundary".to_string(),
            description: "Describe Cortex boundaries, ownership, and current implementation scope."
                .to_string(),
            params: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "cortex.step".to_string(),
            description: concat!(
                "Execute one Cortex Phase 1 reasoning step. ",
                "Loads the configured system prompt from OpenAgent config, ",
                "combines it with the user input, calls the configured LLM provider, ",
                "and returns plain response text without tool use or planning."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Stable session identifier for this turn"
                    },
                    "user_input": {
                        "type": "string",
                        "description": "Raw user message to send to Cortex"
                    },
                    "agent_name": {
                        "type": "string",
                        "description": "Optional configured agent name. Defaults to the first agent in openagent config."
                    }
                },
                "required": ["session_id", "user_input"]
            }),
        },
    ]
}

pub fn register_handlers(server: &mut McpLiteServer, tel: Arc<CortexTelemetry>) {
    server.register_tool("cortex.describe_boundary", |_params| {
        Ok(handle_describe_boundary())
    });

    let t = Arc::clone(&tel);
    server.register_tool("cortex.step", move |params| {
        handle_step(params, Arc::clone(&t))
    });
}
