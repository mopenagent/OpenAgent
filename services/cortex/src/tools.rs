use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::Arc;

use crate::handlers::{
    handle_describe_boundary, handle_discover, handle_search_tools, handle_step, AppContext,
};

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
                    },
                    "turn_kind": {
                        "type": "string",
                        "description": "Optional turn type. Use generation for normal LLM turns and tool_call for deterministic execution turns.",
                        "enum": ["generation", "tool_call"]
                    }
                },
                "required": ["session_id", "user_input"]
            }),
        },
        // cortex.discover and cortex.search_tools temporarily disabled for deterministic tool exposure only
        // ToolDefinition { ... cortex.discover ... },
        // ToolDefinition { ... cortex.search_tools ... },
    ]
}

pub fn register_handlers(server: &mut McpLiteServer, ctx: Arc<AppContext>) {
    server.register_tool("cortex.describe_boundary", |_params| {
        Ok(handle_describe_boundary())
    });

    let step_ctx = Arc::clone(&ctx);
    server.register_tool("cortex.step", move |params| {
        handle_step(params, Arc::clone(&step_ctx))
    });
    // cortex.discover and cortex.search_tools handler registration temporarily disabled
}
