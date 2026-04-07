//! Research DAG MCP-lite service — SQLite-backed research tracking with task graph.
//!
//! Tools: research.start, research.list, research.switch, research.status,
//!        research.complete, research.task_add, research.task_done, research.task_fail.
//!
//! Observability:
//!   Traces  → logs/research-traces-YYYY-MM-DD.jsonl    (via sdk-rust setup_otel)
//!   Metrics → logs/research-metrics-YYYY-MM-DD.jsonl   (one JSON line per operation)
//!   Logs    → structured tracing events bridged to OTEL spans
//!
//! Environment variables (all paths relative to the process working directory = project root):
//!   OPENAGENT_RESEARCH_DB      — SQLite database    (default: data/research.db)
//!   OPENAGENT_RESEARCH_DIR     — Snapshot directory (default: data/research)
//!   OPENAGENT_LOGS_DIR         — traces + metrics   (default: logs)

mod db;
mod handlers;
mod metrics;
mod snapshot;
mod tools;

use anyhow::Result;
use db::{DEFAULT_DB_PATH, DEFAULT_LOGS_DIR, DEFAULT_RESEARCH_DIR};
use metrics::ResearchTelemetry;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir =
        env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());

    let _otel_guard = setup_otel("research", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let tel = Arc::new(ResearchTelemetry::new(&logs_dir)?);

    let db_path_str =
        env::var("OPENAGENT_RESEARCH_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
    let research_dir_str =
        env::var("OPENAGENT_RESEARCH_DIR").unwrap_or_else(|_| DEFAULT_RESEARCH_DIR.to_string());

    let db_path = Path::new(&db_path_str);
    let research_dir = Path::new(&research_dir_str);

    // Ensure research snapshot directory exists
    std::fs::create_dir_all(research_dir)?;

    info!(
        db = %db_path_str,
        research_dir = %research_dir_str,
        logs_dir = %logs_dir,
        "research service starting"
    );

    let store = Arc::new(db::ResearchStore::open(db_path, research_dir)?);
    let research_dir_arc = Arc::new(research_dir.to_path_buf());

    info!("research database ready");

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, Arc::clone(&store), Arc::clone(&research_dir_arc), Arc::clone(&tel));

    info!(addr = "0.0.0.0:9006", "research service ready");
    server.serve_auto("0.0.0.0:9006").await?;
    Ok(())
}
