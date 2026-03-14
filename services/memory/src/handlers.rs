//! Tool handler implementations: memory.index_trace and memory.search_memory.
//!
//! Each handler wires all four OTEL pillars via MemoryTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — MemoryTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, warn!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent + tool/store tags

use crate::db::{batch_stream, err_json, make_batch, DIARY_TABLE, EMBED_DIM, KNOWLEDGE_TABLE, MEMORY_TABLE, TOP_K};
use crate::metrics::{round1, round3, ts_ms, MemoryTelemetry};
use anyhow::Result;
use arrow_array::{ArrayRef, Float32Array, StringArray};
use fastembed::TextEmbedding;
use futures::TryStreamExt as _;
use lancedb::connection::Connection;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery as _, QueryBase as _};
use opentelemetry::KeyValue;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use uuid::Uuid;

pub fn handle_index(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    tel: Arc<MemoryTelemetry>,
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
        "memory" => MEMORY_TABLE,
        _ => return Err(anyhow::anyhow!("{}", err_json("store must be 'memory'"))),
    };

    let content_len = content.len();
    let content_owned = content.to_owned();
    let store_owned = store.clone();

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = MemoryTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "memory.index_trace"),
            KeyValue::new("store", store.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let op_span = tracing::info_span!(
        "memory.store",
        index = %store,
        content_len = content_len,
        embed_ms = tracing::field::Empty,
        store_ms = tracing::field::Empty,
        doc_id = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = op_span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(index = %store, content_len, "memory.index start");

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
            op_span.record("status", "ok");
            info!(
                index = %table_name, doc_id = %id,
                embed_ms = embed_ms, store_ms = store_ms,
                "document stored"
            );

            // Metrics
            tel.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "store", "status": "ok",
                "index": store_owned, "content_len": content_len,
                "embed_ms": round1(embed_ms), "store_ms": round1(store_ms),
            }));

            Ok::<_, anyhow::Error>(json!({ "id": id, "store": table_name }).to_string())
        })
    });

    if let Err(ref e) = result {
        op_span.record("status", "error");
        error!(index = %store, error = %e, "store failed");
        tel.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "store", "status": "error",
            "index": store, "content_len": content_len,
        }));
    }
    result
}

pub fn handle_search(
    params: Value,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    tel: Arc<MemoryTelemetry>,
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
        "memory" => vec![MEMORY_TABLE],
        "diary" => vec![DIARY_TABLE],
        "knowledge" => vec![KNOWLEDGE_TABLE],
        "all" => vec![MEMORY_TABLE, DIARY_TABLE, KNOWLEDGE_TABLE],
        _ => {
            return Err(anyhow::anyhow!(
                "{}",
                err_json("store must be 'memory', 'diary', 'knowledge', or 'all'")
            ))
        }
    };

    let query_len = query.len();
    let query_owned = query.to_owned();
    let store_owned = store.clone();

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = MemoryTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "memory.search"),
            KeyValue::new("store", store.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let op_span = tracing::info_span!(
        "memory.search",
        index = %store,
        query_len = query_len,
        embed_ms = tracing::field::Empty,
        search_ms = tracing::field::Empty,
        result_count = tracing::field::Empty,
        top_rrf = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = op_span.enter();
    info!(index = %store, query_len, "memory.search start (hybrid: dense + BM25 + RRF)");

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            // ── 1. Embed query for dense ANN ──────────────────────────────────
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

            // ── 2. Per-table: dense ANN + BM25, annotate with FTS rank ────────
            let t_search = Instant::now();

            struct RawHit {
                id:         String,
                content:    String,
                metadata:   String,
                created_at: String,
                index:      String,
                distance:   f32,
                fts_rank:   usize,
            }

            let get_str = |col: Option<&ArrayRef>, fallback: &str, i: usize| -> String {
                col.and_then(|c| c.as_any().downcast_ref::<StringArray>())
                    .map(|a| a.value(i).to_string())
                    .unwrap_or_else(|| fallback.to_string())
            };

            let mut all_hits: Vec<RawHit> = Vec::new();

            for table_name in &tables {
                let tbl = match db.open_table(table_name.to_string()).execute().await {
                    Ok(t)  => t,
                    Err(e) => { warn!(index = table_name, error = %e, "open table failed"); continue; }
                };

                // Dense ANN
                let dense_batches: Vec<arrow_array::RecordBatch> = match tbl
                    .query()
                    .nearest_to(query_vec.as_slice())
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .limit(TOP_K)
                    .execute()
                    .await
                {
                    Ok(s)  => s.try_collect().await.map_err(|e| anyhow::anyhow!("{e}"))?,
                    Err(e) => { warn!(index = table_name, error = %e, "dense search failed"); vec![] }
                };

                // BM25 FTS
                let fts_batches: Vec<arrow_array::RecordBatch> = match tbl
                    .query()
                    .full_text_search(FullTextSearchQuery::new(query_owned.clone()))
                    .limit(TOP_K)
                    .execute()
                    .await
                {
                    Ok(s)  => s.try_collect().await.map_err(|e| anyhow::anyhow!("{e}"))?,
                    Err(e) => { warn!(index = table_name, error = %e, "fts search failed"); vec![] }
                };

                // FTS id→rank map (1-based)
                let mut fts_rank_map: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                let mut fts_pos: usize = 1;
                for batch in &fts_batches {
                    if let Some(id_col) = batch.column_by_name("id") {
                        for i in 0..id_col.len() {
                            let id = id_col
                                .as_any()
                                .downcast_ref::<StringArray>()
                                .map(|a| a.value(i).to_string())
                                .unwrap_or_default();
                            fts_rank_map.entry(id).or_insert(fts_pos);
                            fts_pos += 1;
                        }
                    }
                }

                // Collect dense hits, annotate with FTS rank
                for batch in &dense_batches {
                    let id_col      = batch.column_by_name("id");
                    let content_col = batch.column_by_name("content");
                    let meta_col    = batch.column_by_name("metadata");
                    let ts_col      = batch.column_by_name("created_at");
                    let dist_col    = batch.column_by_name("_distance");

                    for i in 0..id_col.map(|c| c.len()).unwrap_or(0) {
                        let id = get_str(id_col, "", i);
                        let fts_rank = *fts_rank_map.get(&id).unwrap_or(&0);
                        let distance = dist_col
                            .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                            .map(|a| a.value(i))
                            .unwrap_or(f32::MAX);
                        all_hits.push(RawHit {
                            id, fts_rank, distance,
                            content:    get_str(content_col, "",   i),
                            metadata:   get_str(meta_col,    "{}", i),
                            created_at: get_str(ts_col,      "",   i),
                            index:      table_name.to_string(),
                        });
                    }
                }

                // FTS-only hits not in dense results
                let dense_ids: std::collections::HashSet<String> =
                    all_hits.iter().map(|h| h.id.clone()).collect();
                for batch in &fts_batches {
                    let id_col      = batch.column_by_name("id");
                    let content_col = batch.column_by_name("content");
                    let meta_col    = batch.column_by_name("metadata");
                    let ts_col      = batch.column_by_name("created_at");
                    for i in 0..id_col.map(|c| c.len()).unwrap_or(0) {
                        let id = get_str(id_col, "", i);
                        if !dense_ids.contains(&id) {
                            let fts_rank = *fts_rank_map.get(&id).unwrap_or(&(TOP_K + 1));
                            all_hits.push(RawHit {
                                id, fts_rank,
                                distance:   f32::MAX,
                                content:    get_str(content_col, "",   i),
                                metadata:   get_str(meta_col,    "{}", i),
                                created_at: get_str(ts_col,      "",   i),
                                index:      table_name.to_string(),
                            });
                        }
                    }
                }
            }

            let search_ms = t_search.elapsed().as_secs_f64() * 1000.0;

            // ── 3. RRF fusion: score = 1/(dr+k) + 1/(fr+k),  k=60 ────────────
            const K: f32 = 60.0;
            let n = all_hits.len();

            let mut dense_order: Vec<usize> = (0..n).collect();
            dense_order.sort_by(|&a, &b| {
                all_hits[a].distance
                    .partial_cmp(&all_hits[b].distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut dense_rank = vec![TOP_K + 1; n];
            for (rank, &idx) in dense_order.iter().enumerate() {
                if all_hits[idx].distance < f32::MAX {
                    dense_rank[idx] = rank + 1;
                }
            }

            let rrf_scores: Vec<f32> = (0..n)
                .map(|i| {
                    let dr = dense_rank[i] as f32;
                    let fr = if all_hits[i].fts_rank > 0 {
                        all_hits[i].fts_rank as f32
                    } else {
                        (TOP_K + 1) as f32
                    };
                    1.0 / (dr + K) + 1.0 / (fr + K)
                })
                .collect();

            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| {
                rrf_scores[b].partial_cmp(&rrf_scores[a]).unwrap_or(std::cmp::Ordering::Equal)
            });
            order.truncate(TOP_K);

            let top_rrf = order.first().map(|&i| rrf_scores[i]);
            let result_count = order.len();

            op_span.record("search_ms", search_ms);
            op_span.record("result_count", result_count as i64);
            op_span.record("status", "ok");
            if let Some(s) = top_rrf { op_span.record("top_rrf", f64::from(s)); }
            info!(
                index = %store_owned, embed_ms = embed_ms, search_ms = search_ms,
                result_count = result_count, top_rrf = top_rrf.unwrap_or(0.0),
                "hybrid search complete"
            );

            let results: Vec<Value> = order
                .iter()
                .map(|&i| {
                    let h = &all_hits[i];
                    let dense_score = if h.distance < f32::MAX {
                        (1.0_f32 - h.distance).clamp(0.0, 1.0)
                    } else { 0.0 };
                    json!({
                        "id":          h.id,
                        "content":     h.content,
                        "metadata":    h.metadata,
                        "created_at":  h.created_at,
                        "store":       h.index,
                        "rrf_score":   round3(f64::from(rrf_scores[i])),
                        "dense_score": round3(f64::from(dense_score)),
                        "dense_rank":  dense_rank[i],
                        "fts_rank":    h.fts_rank,
                    })
                })
                .collect();

            tel.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "search", "status": "ok",
                "index": store_owned, "query_len": query_len,
                "embed_ms": round1(embed_ms), "search_ms": round1(search_ms),
                "result_count": result_count,
                "top_rrf": top_rrf.map(|s| round3(f64::from(s))),
            }));

            Ok::<_, anyhow::Error>(
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string()),
            )
        })
    });

    if let Err(ref e) = result {
        op_span.record("status", "error");
        error!(index = %store, error = %e, "search failed");
        tel.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "search", "status": "error",
            "index": store, "query_len": query_len,
        }));
    }
    result
}


pub fn handle_delete(
    params: Value,
    db: Arc<Connection>,
    tel: Arc<MemoryTelemetry>,
) -> Result<String> {
    let p = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;
    let store = p.get("store").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
    let id_opt: Option<String> = p.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());

    let table_name: &str = match store.as_str() {
        "memory" => MEMORY_TABLE,
        "diary" => DIARY_TABLE,
        "knowledge" => KNOWLEDGE_TABLE,
        _ => return Err(anyhow::anyhow!("{}", err_json("store must be 'memory', 'diary', or 'knowledge'"))),
    };

    let store_owned = store.clone();

    let op_span = tracing::info_span!(
        "memory.delete",
        index = %store,
        by_id = id_opt.is_some(),
        status = tracing::field::Empty,
    );
    let _enter = op_span.enter();

    match &id_opt {
        Some(id) => info!(index = %store, doc_id = %id, "memory.delete by id"),
        None      => info!(index = %store, "memory.delete purge all"),
    }

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let tbl = db.open_table(table_name).execute().await?;
            let predicate = match &id_opt {
                Some(id) => format!("id = '{}'", id.replace('\'', "''")),
                None     => "true".to_string(),   // matches every row — full purge
            };
            tbl.delete(&predicate).await?;
            op_span.record("status", "ok");

            match &id_opt {
                Some(id) => info!(index = %table_name, doc_id = %id, "document deleted"),
                None     => info!(index = %table_name, "store purged"),
            }

            tel.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "delete", "status": "ok",
                "index": store_owned,
                "by_id": id_opt.is_some(),
            }));

            let msg = match &id_opt {
                Some(id) => format!("deleted document {id} from {table_name}"),
                None     => format!("purged all documents from {table_name}"),
            };
            Ok::<_, anyhow::Error>(json!({ "ok": true, "message": msg }).to_string())
        })
    });

    if let Err(ref e) = result {
        op_span.record("status", "error");
        error!(index = %store, error = %e, "delete failed");
        tel.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "delete", "status": "error",
            "index": store,
        }));
    }
    result
}

/// Prune stale entries from the diary table.
///
/// Deletes every row whose `created_at` Unix-second timestamp is older than
/// `max_age_secs` (default: 86400 s = 24 h).  Returns the number of rows removed.
pub fn handle_prune(
    params: Value,
    db: Arc<Connection>,
    tel: Arc<MemoryTelemetry>,
) -> Result<String> {
    let p = params.as_object().ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;

    // Optional override; defaults to 24 h
    let max_age_secs: u64 = p
        .get("max_age_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(86_400);

    let op_span = tracing::info_span!(
        "memory.prune",
        index = DIARY_TABLE,
        max_age_secs = max_age_secs,
        pruned = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = op_span.enter();
    info!(index = DIARY_TABLE, max_age_secs, "memory.prune start");

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let cutoff: u64 = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_sub(max_age_secs);

            // Count rows before deletion so we can report pruned count
            let tbl = db.open_table(DIARY_TABLE).execute().await?;
            let stale_predicate = format!("CAST(created_at AS BIGINT) < {cutoff}");
            let pruned: usize = tbl.count_rows(Some(stale_predicate.clone())).await?;

            // Delete the stale rows
            tbl.delete(&stale_predicate).await?;

            op_span.record("pruned", pruned as i64);
            op_span.record("status", "ok");
            info!(index = DIARY_TABLE, pruned, cutoff, "prune complete");

            tel.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "prune", "status": "ok",
                "index": DIARY_TABLE, "max_age_secs": max_age_secs,
                "pruned": pruned, "cutoff": cutoff,
            }));

            Ok::<_, anyhow::Error>(
                json!({ "ok": true, "pruned": pruned, "cutoff_unix": cutoff }).to_string(),
            )
        })
    });

    if let Err(ref e) = result {
        op_span.record("status", "error");
        error!(index = DIARY_TABLE, error = %e, "prune failed");
        tel.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "prune", "status": "error",
            "index": DIARY_TABLE, "max_age_secs": max_age_secs,
        }));
    }
    result
}

/// Write a stub diary row with a zero vector — no embedding is computed.
///
/// Called fire-and-forget from Cortex after each final answer.  The zero vector
/// is a placeholder; compaction will back-fill real embeddings in a future pass.
///
/// Params:
/// - `session_id`  — session identifier (stored in metadata JSON)
/// - `content`     — truncated response text (up to 500 chars from Cortex)
/// - `file_path`   — absolute path to the diary markdown file (stored in metadata)
pub fn handle_diary_write(
    params: Value,
    db: Arc<Connection>,
    tel: Arc<MemoryTelemetry>,
) -> Result<String> {
    let p = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{}", err_json("params must be an object")))?;
    let session_id = p.get("session_id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let content = p.get("content").and_then(|v| v.as_str()).unwrap_or("").trim();
    let file_path = p.get("file_path").and_then(|v| v.as_str()).unwrap_or("").trim();

    if content.is_empty() {
        return Err(anyhow::anyhow!("{}", err_json("content is required")));
    }

    let content_owned = content.to_owned();
    let session_owned = session_id.to_owned();
    let file_path_owned = file_path.to_owned();
    let content_len = content.len();

    let op_span = tracing::info_span!(
        "memory.diary_write",
        session_id = %session_owned,
        content_len = content_len,
        doc_id = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = op_span.enter();
    info!(session_id = %session_owned, content_len, "memory.diary_write start");

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let id = uuid::Uuid::new_v4().to_string();
            let created_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string());

            let metadata = serde_json::json!({
                "session_id": session_owned,
                "file_path": file_path_owned,
                "stub": true,
            })
            .to_string();

            // Zero vector placeholder — compaction will back-fill real embeddings.
            let zero_vec: Vec<f32> = vec![0.0; EMBED_DIM];
            let batch = make_batch(&id, &content_owned, &metadata, &zero_vec, &created_at)?;
            let tbl = db.open_table(DIARY_TABLE).execute().await?;
            tbl.add(Box::new(batch_stream(batch))).execute().await?;

            op_span.record("doc_id", id.as_str());
            op_span.record("status", "ok");
            info!(session_id = %session_owned, doc_id = %id, "diary row written");

            tel.record(&json!({
                "ts_ms": ts_ms(), "service": "memory", "op": "diary_write", "status": "ok",
                "session_id": session_owned, "content_len": content_len, "doc_id": id,
            }));

            Ok::<_, anyhow::Error>(json!({ "id": id, "store": DIARY_TABLE }).to_string())
        })
    });

    if let Err(ref e) = result {
        op_span.record("status", "error");
        error!(session_id = %session_id, error = %e, "diary_write failed");
        tel.record(&json!({
            "ts_ms": ts_ms(), "service": "memory", "op": "diary_write", "status": "error",
            "session_id": session_id, "content_len": content_len,
        }));
    }
    result
}
