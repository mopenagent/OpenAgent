//! Tool handler implementations: web.search and web.fetch.
//!
//! Each handler wires all four OTEL pillars via BrowserTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — BrowserTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent span from MCP-lite frame

use crate::cache::Cache;
use crate::metrics::{elapsed_ms, tool_metric, BrowserTelemetry};
use anyhow::Result;
use opentelemetry::KeyValue;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, info_span};

const DEFAULT_MAX_SEARCH_RESULTS: usize = 5;

// ── web.search ────────────────────────────────────────────────────────────────

pub async fn handle_search(
    params: Value,
    tel: Arc<BrowserTelemetry>,
    cache: Arc<Mutex<Cache>>,
    searxng_url: String,
) -> Result<String> {
    let query = params["query"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'query' param"))?
        .to_string();
    let max = params["max_results"]
        .as_u64()
        .map(|n| (n as usize).min(10))
        .unwrap_or(DEFAULT_MAX_SEARCH_RESULTS);

    // Baggage — create the span while the context guard is alive so the remote
    // parent trace ID is baked into the span. Drop the guard immediately after
    // (ContextGuard is !Send and cannot be held across an await point).
    let span = {
        let _cx_guard = BrowserTelemetry::attach_context(
            &params,
            vec![KeyValue::new("tool", "web.search")],
        );
        info_span!(
            "web.search",
            query = %query,
            status       = tracing::field::Empty,
            duration_ms  = tracing::field::Empty,
            result_count = tracing::field::Empty,
        )
    };
    let _enter = span.enter();

    info!(query = %query, max, "web.search start");
    let t_start = Instant::now();

    // Cache hit
    let cache_key = format!("{query}:{max}");
    if let Ok(c) = cache.lock() {
        if let Some(hit) = c.get(&cache_key) {
            let dur = elapsed_ms(t_start);
            span.record("status", "cache_hit");
            span.record("duration_ms", dur);
            info!(query = %query, duration_ms = dur, "web.search cache_hit");
            tel.record(&tool_metric("web.search", "cache_hit", dur));
            return Ok(hit.to_string());
        }
    }

    // Live query
    let result = crate::search::search(&searxng_url, &query, max).await;
    let dur = elapsed_ms(t_start);

    match &result {
        Ok(results) => {
            span.record("status", "ok");
            span.record("duration_ms", dur);
            span.record("result_count", results.len() as i64);
            info!(query = %query, count = results.len(), duration_ms = dur, "web.search ok");
            tel.record(&tool_metric("web.search", "ok", dur));
        }
        Err(e) => {
            span.record("status", "error");
            span.record("duration_ms", dur);
            error!(query = %query, duration_ms = dur, error = %e, "web.search error");
            tel.record(&tool_metric("web.search", "error", dur));
        }
    }

    let out = serde_json::to_string(&result?)?;
    if let Ok(mut c) = cache.lock() {
        c.set(cache_key, out.clone());
    }
    Ok(out)
}

// ── web.fetch ─────────────────────────────────────────────────────────────────

pub async fn handle_fetch(
    params: Value,
    tel: Arc<BrowserTelemetry>,
    cache: Arc<Mutex<Cache>>,
) -> Result<String> {
    let url = params["url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'url' param"))?
        .to_string();

    // Baggage — create the span while the context guard is alive so the remote
    // parent trace ID is baked into the span. Drop the guard immediately after.
    let span = {
        let _cx_guard = BrowserTelemetry::attach_context(
            &params,
            vec![KeyValue::new("tool", "web.fetch")],
        );
        info_span!(
            "web.fetch",
            url = %url,
            status      = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
            content_len = tracing::field::Empty,
        )
    };
    let _enter = span.enter();

    info!(url = %url, "web.fetch start");
    let t_start = Instant::now();

    // Cache hit
    if let Ok(c) = cache.lock() {
        if let Some(hit) = c.get(&url) {
            let dur = elapsed_ms(t_start);
            span.record("status", "cache_hit");
            span.record("duration_ms", dur);
            info!(url = %url, duration_ms = dur, "web.fetch cache_hit");
            tel.record(&tool_metric("web.fetch", "cache_hit", dur));
            return Ok(hit.to_string());
        }
    }

    // Fetch + extract
    let result = crate::fetch::fetch_html(&url)
        .await
        .map(|html| crate::extract::extract_text(&html));
    let dur = elapsed_ms(t_start);

    match &result {
        Ok(text) => {
            span.record("status", "ok");
            span.record("duration_ms", dur);
            span.record("content_len", text.len() as i64);
            info!(url = %url, content_len = text.len(), duration_ms = dur, "web.fetch ok");
            tel.record(&tool_metric("web.fetch", "ok", dur));
        }
        Err(e) => {
            span.record("status", "error");
            span.record("duration_ms", dur);
            error!(url = %url, duration_ms = dur, error = %e, "web.fetch error");
            tel.record(&tool_metric("web.fetch", "error", dur));
        }
    }

    let text = result?;
    if let Ok(mut c) = cache.lock() {
        c.set(url, text.clone());
    }
    Ok(text)
}
