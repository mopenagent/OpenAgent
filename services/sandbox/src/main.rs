//! Sandbox service — MCP-lite wrapper for microsandbox.
//!
//! Provides sandboxed code execution (Python, Node.js) and shell commands via a
//! microsandbox server (VM-level OCI isolation).  Supersedes the Go shell service.
//!
//! Tools exposed:
//!   sandbox.execute  — run Python or Node.js code via sandbox.repl.run
//!   sandbox.shell    — run a shell command via sandbox.command.run
//!
//! Environment variables:
//!   OPENAGENT_LOGS_DIR    — traces + metrics  (default: logs)
//!   MSB_SERVER_URL        — microsandbox URL  (default: http://127.0.0.1:5555)
//!   MSB_API_KEY           — API key (required; run: msb server keygen)
//!   MSB_MEMORY_MB         — VM memory in MB  (default: 512)
//!
//! # Abort
//!
//! Panics if the log-level env filter directive is invalid, or if microsandbox
//! returns malformed JSON that violates the expected schema.

mod handlers;
mod metrics;
mod msb;
mod tools;

use anyhow::Result;
use metrics::SandboxTelemetry;
use mimalloc::MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::sync::Arc;
use tracing::info;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    // Initialise all three OTEL pillars: traces, logs, metrics.
    // Writes sandbox-{traces,logs,metrics}-YYYY-MM-DD.jsonl; bridges tracing! macros
    // → OTEL spans (FileSpanExporter) and log records (FileLogExporter).
    // Guard must be held for the process lifetime — drop flushes all three providers.
    let _otel_guard = setup_otel("sandbox", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();
    let tel = Arc::new(SandboxTelemetry::new(&logs_dir)?);

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, Arc::clone(&tel));

    info!(addr = "0.0.0.0:9002", "sandbox.start");
    server.serve_auto("0.0.0.0:9002").await?;
    Ok(())
}
