mod config;
mod handlers;
mod llm;
mod metrics;
mod tools;

use anyhow::Result;
use metrics::CortexTelemetry;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::sync::Arc;
use tracing::info;

const DEFAULT_LOGS_DIR: &str = "logs";
const DEFAULT_SOCKET_PATH: &str = "data/sockets/cortex.sock";

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());
    let socket_path =
        env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    let _otel_guard = setup_otel("cortex", &logs_dir)
        .inspect_err(|e| {
            eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}")
        })
        .ok();
    let tel = Arc::new(CortexTelemetry::new(&logs_dir)?);

    let mut server = McpLiteServer::new(tools::make_tools(), "phase1");
    tools::register_handlers(&mut server, Arc::clone(&tel));

    info!(socket = %socket_path, phase = "phase1", "cortex.start");
    server.serve(&socket_path).await?;
    Ok(())
}
