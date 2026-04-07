mod action;
mod agent;
mod classifier;
mod config;
mod diary;
mod prompt;
mod handlers;
mod llm;
mod memory_adapter;
mod metrics;
mod response;
mod tool_router;
mod tools;
mod validator;

use anyhow::Result;
use handlers::AppContext;
use metrics::CortexTelemetry;
use sdk_rust::{setup_otel, McpLiteServer};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tool_router::ToolRouter;
use tracing::info;

const DEFAULT_LOGS_DIR: &str = "logs";

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());

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
    // Build the tool→socket map from service.json declarations so ToolRouter can
    // route tools like web.search → browser.sock without any prefix hacks.
    let tool_sockets = action_catalog
        .tool_socket_map()
        .into_iter()
        .map(|(tool, sock)| {
            let path = if std::path::Path::new(&sock).is_absolute() {
                PathBuf::from(&sock)
            } else {
                root.join(&sock)
            };
            (tool, path)
        })
        .collect();
    let tool_router = Arc::new(ToolRouter::new(tool_sockets, socket_dir));
    let ctx = Arc::new(AppContext::new(Arc::clone(&tel), action_catalog, tool_router, root.clone()));

    let mut server = McpLiteServer::new(tools::make_tools(), "phase1");
    tools::register_handlers(&mut server, ctx);

    info!(addr = "0.0.0.0:9003", phase = "phase1", "cortex.start");
    server.serve_auto("0.0.0.0:9003").await?;
    Ok(())
}
