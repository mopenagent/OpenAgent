//! Tool definitions and MCP-lite handler registration for the sandbox service.

use crate::handlers::{handle_execute, handle_shell};
use crate::metrics::SandboxTelemetry;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::Arc;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "sandbox.execute".to_string(),
            description: concat!(
                "Execute Python or Node.js code in a secure OCI sandbox ",
                "(VM-level isolation via microsandbox). ",
                "Use for data processing, calculations, API calls, or scripting. ",
                "Each call starts a fresh sandbox — state is not preserved between calls."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python", "node"],
                        "description": "Runtime: 'python' (Python 3) or 'node' (Node.js)"
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute"
                    }
                },
                "required": ["language", "code"]
            }),
        },
        ToolDefinition {
            name: "sandbox.shell".to_string(),
            description: concat!(
                "Run a shell command in a secure OCI sandbox ",
                "(VM-level isolation via microsandbox). ",
                "Safe alternative to direct host execution — commands run inside a container. ",
                "Use for file operations, package inspection, or running binaries."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute (bash/sh). Example: 'ls -la /tmp && echo done'"
                    }
                },
                "required": ["command"]
            }),
        },
    ]
}

pub fn register_handlers(server: &mut McpLiteServer, tel: Arc<SandboxTelemetry>) {
    let t = Arc::clone(&tel);
    server.register_tool("sandbox.execute", move |params| {
        let t = Arc::clone(&t);
        async move { handle_execute(params, t) }
    });

    let t = Arc::clone(&tel);
    server.register_tool("sandbox.shell", move |params| {
        let t = Arc::clone(&t);
        async move { handle_shell(params, t) }
    });
}
