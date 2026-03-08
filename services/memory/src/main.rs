//! Agentic Memory MCP-lite service — LanceDB + FastEmbed, LTS/STS vector stores.
//!
//! Tools: memory.index_trace, memory.search_memory.
//! Uses BAAI/bge-small-en-v1.5 (384-dim) via fastembed, local LanceDB at ./data/memory.
//!
//! Observability:
//!   Traces  → logs/memory-traces-YYYY-MM-DD.jsonl    (via sdk-rust setup_otel)
//!   Metrics → logs/memory-metrics-YYYY-MM-DD.jsonl   (one JSON line per operation)
//!   Logs    → structured tracing events bridged to OTEL spans
//!
//! Environment variables:
//!   OPENAGENT_SOCKET_PATH      — Unix socket (default: data/sockets/memory.sock)
//!   OPENAGENT_MEMORY_PATH      — LanceDB storage dir (default: ./data/memory)
//!   OPENAGENT_LOGS_DIR         — Log/metrics output dir (default: logs)
//!   OPENAGENT_EMBED_CACHE_PATH — FastEmbed model cache (default: ./data/models)
//!   OPENAGENT_EMBED_OFFLINE    — "1" to error if model not cached (no download)
//!
//! # Abort
//!
//! Panics if the log-level env filter directive is invalid, or if the embedding
//! model mutex is poisoned due to a prior panic in a tool handler.

use anyhow::Result;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use arrow_array::{
    types::Float32Type, Array, FixedSizeListArray, Float32Array, RecordBatch,
    RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use futures::TryStreamExt;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use sdk_rust::{setup_otel, McpLiteServer, ToolDefinition};
use serde_json::{json, Value};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use arrow_array::ArrayRef;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use uuid::Uuid;

const EMBED_DIM: usize = 384;
const TOP_K: usize = 5;
const DEFAULT_MEMORY_PATH: &str = "./data/memory";
const DEFAULT_SOCKET_PATH: &str = "data/sockets/memory.sock";
const DEFAULT_EMBED_CACHE: &str = "./data/models";
const LTS_TABLE: &str = "lts";
const STS_TABLE: &str = "sts";

// ---------------------------------------------------------------------------
// Metrics writer — one JSONL line per operation
// ---------------------------------------------------------------------------

/// Appends one JSON line per memory operation to a daily-rotating JSONL file:
///   logs/memory-metrics-YYYY-MM-DD.jsonl
///
/// Each line: {"ts_ms":…,"service":"memory","op":"store|search","status":"ok|error",
///             "index":"sts","embed_ms":22.1,"op_ms":8.4,"result_count":5,…}
#[derive(Debug)]
struct MetricsWriter {
    inner: Arc<Mutex<MetricsInner>>,
    logs_dir: PathBuf,
}

#[derive(Debug)]
struct MetricsInner {
    file: File,
    current_date: String,
}

impl MetricsWriter {
    fn new(logs_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(logs_dir);
        fs::create_dir_all(&dir)?;
        let today = today_date();
        let file = open_metrics_file(&dir, &today)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(MetricsInner { file, current_date: today })),
            logs_dir: dir,
        })
    }

    fn record(&self, record: &Value) {
        let mut guard = self.inner.lock().expect("metrics mutex poisoned");
        let today = today_date();
        if guard.current_date != today {
            match open_metrics_file(&self.logs_dir, &today) {
                Ok(f) => { guard.file = f; guard.current_date = today; }
                Err(e) => { eprintln!("metrics rotate error: {e}"); return; }
            }
        }
        if let Ok(line) = serde_json::to_string(record) {
            let _ = writeln!(guard.file, "{}", line);
            let _ = guard.file.flush();
        }
    }
}

fn open_metrics_file(dir: &PathBuf, date: &str) -> anyhow::Result<File> {
    let path = dir.join(format!("memory-metrics-{}.jsonl", date));
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

fn today_date() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, m, d) = days_to_ymd(secs / 86400);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let y = 1970 + days / 365;
    let rem = days % 365;
    ((y), (1 + rem / 30).min(12), (1 + rem % 30).min(28))
}

fn ts_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Schema / table helpers
// ---------------------------------------------------------------------------

fn err_json(msg: &str) -> String {
    json!({ "error": msg }).to_string()
}

fn table_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("metadata", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBED_DIM as i32,
            ),
            false,
        ),
        Field::new("created_at", DataType::Utf8, false),
    ]))
}

async fn ensure_table(db: &Connection, name: &str) -> Result<()> {
    let names = db.table_names().execute().await?;
    if !names.contains(&name.to_string()) {
        db.create_empty_table(name.to_string(), table_schema()).execute().await?;
        info!(table = name, "created memory table");
    }
    Ok(())
}

fn make_batch(id: &str, content: &str, metadata: &str, vector: &[f32], created_at: &str) -> Result<RecordBatch> {
    let list_values: Vec<Option<f32>> = vector.iter().map(|&x| Some(x)).collect();
    let vec_arr = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        vec![Some(list_values)],
        EMBED_DIM as i32,
    );
    RecordBatch::try_new(
        table_schema(),
        vec![
            Arc::new(StringArray::from(vec![id])),
            Arc::new(StringArray::from(vec![content])),
            Arc::new(StringArray::from(vec![Some(metadata)])),
            Arc::new(vec_arr),
            Arc::new(StringArray::from(vec![created_at])),
        ],
    ).map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

fn handle_index_trace(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    metrics: Arc<MetricsWriter>,
) -> Result<String> {
    let p = params.as_object()
        .ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;
    let content = p.get("content").and_then(|v| v.as_str()).unwrap_or("").trim();
    let store = p.get("store").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
    let metadata_str = p.get("metadata").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());

    if content.is_empty() {
        return Err(anyhow::anyhow!("{}", err_json("content is required")));
    }
    let table_name: &str = match store.as_str() {
        "lts" => LTS_TABLE,
        "sts" => STS_TABLE,
        _ => return Err(anyhow::anyhow!("{}", err_json("store must be 'lts' or 'sts'"))),
    };

    let content_len = content.len();
    let content_owned = content.to_owned();
    let store_owned = store.clone();

    // Span: child of tool.call span set by sdk-rust server.rs, which is itself
    // parented to the Python AgentLoop span via propagated trace_id/span_id.
    let op_span = tracing::info_span!(
        "memory.store",
        index = %store,
        content_len = content_len,
        embed_ms = tracing::field::Empty,
        store_ms = tracing::field::Empty,
        doc_id = tracing::field::Empty,
    );
    let _enter = op_span.enter();

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            // -- Embed --
            let t_embed = Instant::now();
            let embeddings = model.lock().expect("embedding model mutex poisoned").embed(&[content_owned.clone()], None)
                .map_err(|e| anyhow::anyhow!("{}", err_json(&format!("embedding failed: {e}"))))?;
            let embed_ms = t_embed.elapsed().as_secs_f64() * 1000.0;
            let vec = embeddings.first()
                .ok_or_else(|| anyhow::anyhow!("{}", err_json("no embedding returned")))?;

            op_span.record("embed_ms", embed_ms);
            tracing::info!(embed_ms = embed_ms, content_len = content_len, "embedded content");

            // -- Insert --
            let t_store = Instant::now();
            let id = Uuid::new_v4().to_string();
            let created_at = SystemTime::now().duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string()).unwrap_or_else(|_| "0".to_string());
            let batch = make_batch(&id, &content_owned, &metadata_str, vec, &created_at)?;
            let schema = batch.schema();
            let stream = RecordBatchIterator::new(vec![batch].into_iter().map(Ok), schema);
            let tbl = db.open_table(table_name).execute().await?;
            tbl.add(Box::new(stream)).execute().await?;
            let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;

            op_span.record("store_ms", store_ms);
            op_span.record("doc_id", id.as_str());
            info!(index = %table_name, doc_id = %id, embed_ms = embed_ms, store_ms = store_ms, "document stored");

            metrics.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "store", "status": "ok",
                "index": store_owned, "content_len": content_len,
                "embed_ms": round1(embed_ms), "store_ms": round1(store_ms),
            }));

            Ok::<_, anyhow::Error>(json!({ "id": id, "store": table_name }).to_string())
        })
    });

    if let Err(ref e) = result {
        error!(index = %store, error = %e, "store failed");
        metrics.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "store", "status": "error",
            "index": store, "content_len": content_len,
        }));
    }
    result
}

fn handle_search_memory(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    metrics: Arc<MetricsWriter>,
) -> Result<String> {
    let p = params.as_object()
        .ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;
    let query = p.get("query").and_then(|v| v.as_str()).unwrap_or("").trim();
    let store = p.get("store").and_then(|v| v.as_str()).unwrap_or("all").trim().to_lowercase();

    if query.is_empty() {
        return Err(anyhow::anyhow!("{}", err_json("query is required")));
    }
    let tables: Vec<&str> = match store.as_str() {
        "lts" => vec![LTS_TABLE],
        "sts" => vec![STS_TABLE],
        "all" => vec![LTS_TABLE, STS_TABLE],
        _ => return Err(anyhow::anyhow!("{}", err_json("store must be 'lts', 'sts', or 'all'"))),
    };

    let query_len = query.len();
    let query_owned = query.to_owned();
    let store_owned = store.clone();

    let op_span = tracing::info_span!(
        "memory.search",
        index = %store,
        query_len = query_len,
        embed_ms = tracing::field::Empty,
        search_ms = tracing::field::Empty,
        result_count = tracing::field::Empty,
        top_score = tracing::field::Empty,
    );
    let _enter = op_span.enter();

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            // -- Embed query --
            let t_embed = Instant::now();
            let embeddings = model.lock().expect("embedding model mutex poisoned").embed(&[query_owned.clone()], None)
                .map_err(|e| anyhow::anyhow!("{}", err_json(&format!("embedding failed: {e}"))))?;
            let embed_ms = t_embed.elapsed().as_secs_f64() * 1000.0;
            let query_vec = embeddings.first()
                .ok_or_else(|| anyhow::anyhow!("{}", err_json("no embedding returned")))?;

            op_span.record("embed_ms", embed_ms);
            tracing::info!(embed_ms = embed_ms, query_len = query_len, "embedded query");

            // -- Search per index, collect hits with _distance for global ranking --
            let t_search = Instant::now();

            struct Hit { distance: f32, id: String, content: String, metadata: String, created_at: String, index: String }

            let mut hits: Vec<Hit> = Vec::new();

            for table_name in &tables {
                let tbl = match db.open_table(table_name.to_string()).execute().await {
                    Ok(t) => t,
                    Err(e) => { warn!(index = table_name, error = %e, "open table failed"); continue; }
                };
                let stream = match tbl.query()
                    .nearest_to(query_vec.as_slice())
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .limit(TOP_K)
                    .execute()
                    .await
                {
                    Ok(s) => s,
                    Err(e) => { warn!(index = table_name, error = %e, "search failed"); continue; }
                };

                let batches: Vec<RecordBatch> = stream.try_collect().await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;

                for batch in &batches {
                    let id_col      = batch.column_by_name("id");
                    let content_col = batch.column_by_name("content");
                    let meta_col    = batch.column_by_name("metadata");
                    let ts_col      = batch.column_by_name("created_at");
                    let dist_col    = batch.column_by_name("_distance");

                    for i in 0..id_col.map(|c| c.len()).unwrap_or(0) {
                        let get_str = |col: Option<&ArrayRef>, fallback: &str| -> String {
                            col.and_then(|c| c.as_any().downcast_ref::<StringArray>())
                               .map(|a| a.value(i).to_string())
                               .unwrap_or_else(|| fallback.to_string())
                        };
                        // _distance: lower = more similar (L2/cosine on normalised vecs)
                        let distance = dist_col
                            .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                            .map(|a| a.value(i))
                            .unwrap_or(f32::MAX);

                        hits.push(Hit {
                            distance,
                            id:         get_str(id_col,      ""),
                            content:    get_str(content_col, ""),
                            metadata:   get_str(meta_col,    "{}"),
                            created_at: get_str(ts_col,      ""),
                            index:      table_name.to_string(),
                        });
                    }
                }
            }

            let search_ms = t_search.elapsed().as_secs_f64() * 1000.0;

            // Global top-K: sort by distance ascending (closest = most relevant), re-rank across indexes
            hits.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal));
            hits.truncate(TOP_K);

            let top_score = hits.first().map(|h| (1.0_f32 - h.distance).clamp(0.0, 1.0));
            let result_count = hits.len();

            op_span.record("search_ms", search_ms);
            op_span.record("result_count", result_count as i64);
            if let Some(s) = top_score { op_span.record("top_score", s as f64); }

            info!(
                index = %store_owned, embed_ms = embed_ms, search_ms = search_ms,
                result_count = result_count, top_score = top_score.unwrap_or(0.0),
                "search complete"
            );

            let results: Vec<Value> = hits.iter().map(|h| {
                let score = (1.0_f32 - h.distance).clamp(0.0, 1.0);
                json!({
                    "id": h.id, "content": h.content, "metadata": h.metadata,
                    "created_at": h.created_at, "store": h.index,
                    "score":    round3(score as f64),
                    "distance": round3(h.distance as f64),
                })
            }).collect();

            metrics.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "search", "status": "ok",
                "index": store_owned, "query_len": query_len,
                "embed_ms": round1(embed_ms), "search_ms": round1(search_ms),
                "result_count": result_count,
                "top_score": top_score.map(|s| round3(s as f64)),
            }));

            Ok::<_, anyhow::Error>(
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string())
            )
        })
    });

    if let Err(ref e) = result {
        error!(index = %store, error = %e, "search failed");
        metrics.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "search", "status": "error",
            "index": store, "query_len": query_len,
        }));
    }
    result
}

fn round1(v: f64) -> f64 { (v * 10.0).round() / 10.0 }
fn round3(v: f64) -> f64 { (v * 1000.0).round() / 1000.0 }

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

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

    let memory_path = env::var("OPENAGENT_MEMORY_PATH")
        .unwrap_or_else(|_| DEFAULT_MEMORY_PATH.to_string());
    let socket_path = env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let embed_cache = env::var("OPENAGENT_EMBED_CACHE_PATH")
        .unwrap_or_else(|_| DEFAULT_EMBED_CACHE.to_string());
    let embed_offline = env::var("OPENAGENT_EMBED_OFFLINE")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    info!(
        memory_path = %memory_path, embed_cache = %embed_cache,
        offline = embed_offline, logs_dir = %logs_dir,
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
    let _ = model.lock().expect("embedding model mutex poisoned").embed(&["warmup".to_string()], None)?;
    info!(warmup_ms = t_warm.elapsed().as_millis(), "model warmup complete");

    let tools = vec![
        ToolDefinition {
            name: "memory.index_trace".to_string(),
            description: concat!(
                "Embed and store text (content + optional metadata) into ",
                "LTS (long-term summaries) or STS (short-term conversation chain). ",
                "Returns the generated document id."
            ).to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Text to embed and store" },
                    "metadata": { "type": "object", "description": "Optional metadata (session_id, source, etc.)" },
                    "store": { "type": "string", "enum": ["lts", "sts"], "description": "lts = long-term summaries; sts = short-term full chain" }
                },
                "required": ["content", "store"]
            }),
        },
        ToolDefinition {
            name: "memory.search_memory".to_string(),
            description: concat!(
                "Semantic vector search over LTS, STS, or both. ",
                "Returns up to 5 results ranked by similarity score (0–1, higher = more relevant). ",
                "Each result includes id, content, metadata, created_at, store, score, distance."
            ).to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query" },
                    "store": { "type": "string", "enum": ["lts", "sts", "all"], "description": "Store to search (default: all — merges and re-ranks by score)" }
                },
                "required": ["query"]
            }),
        },
    ];

    let mut server = McpLiteServer::new(tools, "ready");

    let (db1, m1, mx1) = (db.clone(), model.clone(), metrics.clone());
    server.register_tool("memory.index_trace", move |p| {
        handle_index_trace(p, db1.clone(), m1.clone(), mx1.clone())
    });

    let (db2, m2, mx2) = (db.clone(), model.clone(), metrics.clone());
    server.register_tool("memory.search_memory", move |p| {
        handle_search_memory(p, db2.clone(), m2.clone(), mx2.clone())
    });

    info!(socket = %socket_path, "memory service ready");
    server.serve(&socket_path).await?;
    Ok(())
}
