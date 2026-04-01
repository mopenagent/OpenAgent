//! Browser service — MCP-lite web access daemon.
//!
//! Exposes two tools over a Unix Domain Socket using the sdk-rust McpLiteServer:
//!   web.search  — SearXNG full-text search (cached 5 min)
//!   web.fetch   — reqwest + dom_smoothie extraction (cached 1 hr)
//!
//! Environment variables:
//!   OPENAGENT_SOCKET_PATH — Unix socket path  (default: data/sockets/browser.sock)
//!   OPENAGENT_LOGS_DIR    — OTEL output dir   (default: logs)
//!   SEARXNG_URL           — SearXNG base URL  (default: http://100.96.81.109:8888)

mod handlers;
mod metrics;
mod tools;

pub mod cache;
pub mod extract;
pub mod fetch;
pub mod search;

use cache::Cache;
use metrics::BrowserTelemetry;
use mimalloc::MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::sync::{Arc, Mutex};
use tracing::info;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const DEFAULT_SOCKET_PATH: &str = "data/sockets/browser.sock";
const DEFAULT_SEARXNG_URL: &str = "http://100.96.81.109:8888";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls 0.23 (pulled in by hyper-rustls 0.27 / reqwest 0.12) requires an explicit
    // crypto provider. Install ring before any TLS connection is attempted.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ok() — safe to ignore if already installed

    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    let _otel_guard = setup_otel("browser", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"otel\":\"init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let tel = Arc::new(BrowserTelemetry::new(&logs_dir)?);

    let socket_path =
        env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let searxng_url =
        env::var("SEARXNG_URL").unwrap_or_else(|_| DEFAULT_SEARXNG_URL.to_string());

    let search_cache: Arc<Mutex<Cache>> = Arc::new(Mutex::new(Cache::new(300)));
    let fetch_cache: Arc<Mutex<Cache>> = Arc::new(Mutex::new(Cache::new(3_600)));

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, tel, search_cache, fetch_cache, searxng_url.clone());

    info!(socket = %socket_path, searxng = %searxng_url, "browser.start");
    server.serve(&socket_path).await?;
    Ok(())
}
