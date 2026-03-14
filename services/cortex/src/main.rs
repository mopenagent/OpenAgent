mod action;
mod agent;
mod agent_tools;
mod config;
mod handlers;
mod llm;
mod metrics;
mod response;
mod tool_router;
mod tools;
mod validator;

use anyhow::Result;
use handlers::AppContext;
use metrics::CortexTelemetry;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tool_router::ToolRouter;
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
    let root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let action_catalog = Arc::new(action::catalog::ActionCatalog::discover_from_root(&root)?);
    let socket_dir = env::var("OPENAGENT_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("data/sockets"));
    let tool_router = Arc::new(ToolRouter::new(socket_dir));
    let ctx = Arc::new(AppContext::new(Arc::clone(&tel), action_catalog, tool_router));

    let mut server = McpLiteServer::new(tools::make_tools(), "phase1");
    tools::register_handlers(&mut server, ctx);

    info!(socket = %socket_path, phase = "phase1", "cortex.start");
    server.serve(&socket_path).await?;
    Ok(())
}
