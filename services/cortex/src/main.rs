//! Cortex Phase 0 service boundary.
//!
//! Phase 0 does not execute session steps or call the LLM. It establishes that
//! Cortex is a standalone MCP-lite service with a narrow, explicit boundary.
//!
//! Exposed tool:
//!   cortex.describe_boundary
//!
//! Environment variables:
//!   OPENAGENT_SOCKET_PATH - Unix socket path (default: data/sockets/cortex.sock)
//!   OPENAGENT_LOGS_DIR    - traces + metrics   (default: logs)

use anyhow::Result;
use sdk_rust::{setup_otel, McpLiteServer};
use serde_json::json;
use std::env;
use tracing::info;

const DEFAULT_LOGS_DIR: &str = "logs";
const DEFAULT_SOCKET_PATH: &str = "data/sockets/cortex.sock";

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir =
        env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());
    let socket_path =
        env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    let _otel_guard = setup_otel("cortex", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let mut server = McpLiteServer::new(make_tools(), "phase0");
    register_handlers(&mut server);

    info!(socket = %socket_path, phase = "phase0", "cortex.start");
    server.serve(&socket_path).await?;
    Ok(())
}

fn make_tools() -> Vec<sdk_rust::ToolDefinition> {
    vec![sdk_rust::ToolDefinition {
        name: "cortex.describe_boundary".to_string(),
        description: "Describe Cortex Phase 0 boundaries, ownership, and the planned library set for the first LLM step.".to_string(),
        params: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }]
}

fn register_handlers(server: &mut McpLiteServer) {
    server.register_tool("cortex.describe_boundary", |_params| {
        Ok(json!({
            "phase": "phase0",
            "status": "boundary-captured",
            "service_boundary": {
                "is_service": true,
                "transport": "mcp-lite-json-uds",
                "python_shell_role": "temporary pre-cortex shell",
                "llm_calling_rule": "cortex-only in target architecture"
            },
            "owns_now": [
                "service identity",
                "mcp-lite socket boundary",
                "boundary documentation",
                "phase planning for session-step execution"
            ],
            "does_not_own_yet": [
                "llm invocation",
                "tool routing",
                "memory retrieval",
                "plan store",
                "segmented stm"
            ],
            "phase1_library_set": {
                "keep": [
                    "sdk-rust",
                    "tokio",
                    "serde",
                    "serde_json",
                    "anyhow",
                    "tracing",
                    "opentelemetry",
                    "tracing-opentelemetry"
                ],
                "add_next": [
                    "reqwest (rustls-tls) for the LLM HTTP client",
                    "uuid for request and session correlation"
                ],
                "avoid": [
                    "agent frameworks",
                    "embedded memory/vector stores inside cortex",
                    "direct service-to-service shortcuts"
                ]
            }
        })
        .to_string())
    });
}
