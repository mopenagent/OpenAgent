//! Tool handler implementations: memory.index_trace and memory.search_memory.

use crate::db::{batch_stream, err_json, make_batch, LTS_TABLE, STS_TABLE, TOP_K};
use crate::metrics::{ts_ms, MetricsWriter};
use anyhow::Result;
use arrow_array::{ArrayRef, Float32Array, StringArray};
use fastembed::TextEmbedding;
use futures::TryStreamExt as _;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery as _, QueryBase as _};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use uuid::Uuid;

pub fn handle_index_trace(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    metrics: Arc<MetricsWriter>,
) -> Result<String> {
    let p = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;
    let content = p.get("content").and_then(|v| v.as_str()).unwrap_or("").trim();
    let store = p.get("store").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
    let metadata_str =
        p.get("metadata").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());

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

    // Span: child of tool.call span set by sdk-rust server.rs, parented to the
    // Python AgentLoop span via propagated trace_id/span_id.
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
            // ── Embed ──────────────────────────────────────────────────────────
            let t_embed = Instant::now();
            let embeddings = model
                .lock()
                .expect("embedding model mutex poisoned")
                .embed(&[content_owned.clone()], None)
                .map_err(|e| anyhow::anyhow!("{}", err_json(&format!("embedding failed: {e}"))))?;
            let embed_ms = t_embed.elapsed().as_secs_f64() * 1000.0;
            let vec = embeddings
                .first()
                .ok_or_else(|| anyhow::anyhow!("{}", err_json("no embedding returned")))?;

            op_span.record("embed_ms", embed_ms);
            info!(embed_ms = embed_ms, content_len = content_len, "embedded content");

            // ── Insert ─────────────────────────────────────────────────────────
            let t_store = Instant::now();
            let id = Uuid::new_v4().to_string();
            let created_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string());
            let batch = make_batch(&id, &content_owned, &metadata_str, vec, &created_at)?;
            let tbl = db.open_table(table_name).execute().await?;
            tbl.add(Box::new(batch_stream(batch))).execute().await?;
            let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;

            op_span.record("store_ms", store_ms);
            op_span.record("doc_id", id.as_str());
            info!(
                index = %table_name, doc_id = %id,
                embed_ms = embed_ms, store_ms = store_ms,
                "document stored"
            );

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

pub fn handle_search_memory(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    metrics: Arc<MetricsWriter>,
) -> Result<String> {
    let p = params
        .as_object()
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
        _ => {
            return Err(anyhow::anyhow!(
                "{}",
                err_json("store must be 'lts', 'sts', or 'all'")
            ))
        }
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
            // ── Embed query ────────────────────────────────────────────────────
            let t_embed = Instant::now();
            let embeddings = model
                .lock()
                .expect("embedding model mutex poisoned")
                .embed(&[query_owned.clone()], None)
                .map_err(|e| anyhow::anyhow!("{}", err_json(&format!("embedding failed: {e}"))))?;
            let embed_ms = t_embed.elapsed().as_secs_f64() * 1000.0;
            let query_vec = embeddings
                .first()
                .ok_or_else(|| anyhow::anyhow!("{}", err_json("no embedding returned")))?;

            op_span.record("embed_ms", embed_ms);
            info!(embed_ms = embed_ms, query_len = query_len, "embedded query");

            // ── Search per index, collect hits for global ranking ──────────────
            let t_search = Instant::now();

            struct Hit {
                distance: f32,
                id: String,
                content: String,
                metadata: String,
                created_at: String,
                index: String,
            }

            let mut hits: Vec<Hit> = Vec::new();

            for table_name in &tables {
                let tbl = match db.open_table(table_name.to_string()).execute().await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!(index = table_name, error = %e, "open table failed");
                        continue;
                    }
                };
                let stream = match tbl
                    .query()
                    .nearest_to(query_vec.as_slice())
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .limit(TOP_K)
                    .execute()
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(index = table_name, error = %e, "search failed");
                        continue;
                    }
                };

                let batches: Vec<arrow_array::RecordBatch> =
                    stream.try_collect().await.map_err(|e| anyhow::anyhow!("{e}"))?;

                for batch in &batches {
                    let id_col = batch.column_by_name("id");
                    let content_col = batch.column_by_name("content");
                    let meta_col = batch.column_by_name("metadata");
                    let ts_col = batch.column_by_name("created_at");
                    let dist_col = batch.column_by_name("_distance");

                    for i in 0..id_col.map(|c| c.len()).unwrap_or(0) {
                        let get_str = |col: Option<&ArrayRef>, fallback: &str| -> String {
                            col.and_then(|c| c.as_any().downcast_ref::<StringArray>())
                                .map(|a| a.value(i).to_string())
                                .unwrap_or_else(|| fallback.to_string())
                        };
                        // _distance: lower = more similar (L2 on normalised vecs)
                        let distance = dist_col
                            .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                            .map(|a| a.value(i))
                            .unwrap_or(f32::MAX);

                        hits.push(Hit {
                            distance,
                            id: get_str(id_col, ""),
                            content: get_str(content_col, ""),
                            metadata: get_str(meta_col, "{}"),
                            created_at: get_str(ts_col, ""),
                            index: table_name.to_string(),
                        });
                    }
                }
            }

            let search_ms = t_search.elapsed().as_secs_f64() * 1000.0;

            // Global top-K: sort by distance ascending (closest = most relevant)
            hits.sort_by(|a, b| {
                a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(TOP_K);

            let top_score = hits.first().map(|h| (1.0_f32 - h.distance).clamp(0.0, 1.0));
            let result_count = hits.len();

            op_span.record("search_ms", search_ms);
            op_span.record("result_count", result_count as i64);
            if let Some(s) = top_score {
                op_span.record("top_score", f64::from(s));
            }

            info!(
                index = %store_owned, embed_ms = embed_ms, search_ms = search_ms,
                result_count = result_count, top_score = top_score.unwrap_or(0.0),
                "search complete"
            );

            let results: Vec<Value> = hits
                .iter()
                .map(|h| {
                    let score = (1.0_f32 - h.distance).clamp(0.0, 1.0);
                    json!({
                        "id": h.id, "content": h.content, "metadata": h.metadata,
                        "created_at": h.created_at, "store": h.index,
                        "score":    round3(f64::from(score)),
                        "distance": round3(f64::from(h.distance)),
                    })
                })
                .collect();

            metrics.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "search", "status": "ok",
                "index": store_owned, "query_len": query_len,
                "embed_ms": round1(embed_ms), "search_ms": round1(search_ms),
                "result_count": result_count,
                "top_score": top_score.map(|s| round3(f64::from(s))),
            }));

            Ok::<_, anyhow::Error>(
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string()),
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

/// Round to 1 decimal place for timing metrics.
pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Round to 3 decimal places for similarity scores.
pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
