//! Agentic Memory MCP-lite service — LanceDB + FastEmbed, LTS/STS vector stores.
//!
//! Tools: memory.index_trace, memory.search_memory.
//! Uses BAAI/bge-small-en-v1.5 (384-dim) via fastembed.
//!
//! Observability:
//!   Traces  → logs/memory-traces-YYYY-MM-DD.jsonl    (via sdk-rust setup_otel)
//!   Metrics → logs/memory-metrics-YYYY-MM-DD.jsonl   (one JSON line per operation)
//!   Logs    → structured tracing events bridged to OTEL spans
//!
//! Environment variables (all paths relative to the process working directory = project root):
//!   OPENAGENT_SOCKET_PATH      — Unix socket        (default: data/sockets/memory.sock)
//!   OPENAGENT_MEMORY_PATH      — LanceDB storage    (default: data/memory)
//!   OPENAGENT_LOGS_DIR         — traces + metrics   (default: logs)
//!   OPENAGENT_EMBED_CACHE_PATH — FastEmbed cache    (default: data/models)
//!   OPENAGENT_EMBED_OFFLINE    — "1" → error if model not in cache (no download)
//!
//! # Abort
//!
//! Panics if the log-level env filter directive is invalid, or if the embedding
//! model mutex is poisoned due to a prior panic in a tool handler.

mod db;
mod handlers;
mod metrics;
mod tools;

use anyhow::Result;
use db::{
    ensure_table, DEFAULT_EMBED_CACHE, DEFAULT_LOGS_DIR, DEFAULT_MEMORY_PATH,
    DEFAULT_SOCKET_PATH, LTS_TABLE, STS_TABLE,
};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use metrics::MetricsWriter;
use mimalloc::MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer};
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir =
        env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());

    // Traces → logs/memory-traces-YYYY-MM-DD.jsonl
    // Bridges tracing macros → OTEL spans → OTLP-JSON file exporter (sdk-rust)
    let _otel_guard = match setup_otel("memory", &logs_dir) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("otel init failed (traces disabled): {e}");
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("memory=info".parse().expect("valid log directive")),
                )
                .try_init()
                .ok();
            None
        }
    };

    // Metrics → logs/memory-metrics-YYYY-MM-DD.jsonl (one line per op)
    let metrics = Arc::new(MetricsWriter::new(&logs_dir)?);

    let memory_path =
        env::var("OPENAGENT_MEMORY_PATH").unwrap_or_else(|_| DEFAULT_MEMORY_PATH.to_string());
    let socket_path =
        env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let embed_cache =
        env::var("OPENAGENT_EMBED_CACHE_PATH").unwrap_or_else(|_| DEFAULT_EMBED_CACHE.to_string());
    let embed_offline = env::var("OPENAGENT_EMBED_OFFLINE")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    info!(
        memory_path = %memory_path,
        embed_cache = %embed_cache,
        offline = embed_offline,
        logs_dir = %logs_dir,
        "memory service starting"
    );

    let db = lancedb::connect(&memory_path).execute().await?;
    let db = Arc::new(db);
    ensure_table(db.as_ref(), LTS_TABLE).await?;
    ensure_table(db.as_ref(), STS_TABLE).await?;

    // Load embedding model — uses local cache; errors if absent + EMBED_OFFLINE=1
    let t_model = Instant::now();
    let model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(embed_cache.into())
            .with_show_download_progress(!embed_offline),
    )?;
    let model = Arc::new(Mutex::new(model));
    info!(load_ms = t_model.elapsed().as_millis(), "embedding model loaded");

    // Warm-up: force ONNX session init before first real request
    let t_warm = Instant::now();
    model
        .lock()
        .expect("embedding model mutex poisoned")
        .embed(&["warmup".to_string()], None)?;
    info!(warmup_ms = t_warm.elapsed().as_millis(), "model warmup complete");

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, Arc::clone(&db), Arc::clone(&model), Arc::clone(&metrics));

    info!(socket = %socket_path, "memory service ready");
    server.serve(&socket_path).await?;
    Ok(())
}
