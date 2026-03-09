//! OTEL observability — all four pillars — for the Memory service.
//!
//! ```text
//! Pillar   │ Mechanism                              │ Output
//! ─────────┼────────────────────────────────────────┼──────────────────────────────────────
//! Traces   │ tracing::info_span! → OTEL bridge      │ logs/memory-traces-YYYY-MM-DD.jsonl
//! Metrics  │ MemoryTelemetry::record()              │ logs/memory-metrics-YYYY-MM-DD.jsonl
//! Logs     │ tracing::{info!,warn!,error!}          │ OTEL span events (same trace file)
//! Baggage  │ opentelemetry::Context + BaggageExt    │ propagated in-process via Context
//! ```

use opentelemetry::{baggage::BaggageExt as _, Context, ContextGuard, KeyValue};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// OTEL observability facade for the Memory service.
#[derive(Debug, Clone)]
pub struct MemoryTelemetry {
    inner: Arc<TelemetryInner>,
}

#[derive(Debug)]
struct TelemetryInner {
    logs_dir: PathBuf,
    state: Mutex<MetricsState>,
}

#[derive(Debug)]
struct MetricsState {
    file: File,
    current_date: String,
}

impl MemoryTelemetry {
    pub fn new(logs_dir: &str) -> anyhow::Result<Self> {
        let dir = PathBuf::from(logs_dir);
        fs::create_dir_all(&dir)?;
        let today = today_date();
        let file = open_metrics_file(&dir, &today)?;
        Ok(Self {
            inner: Arc::new(TelemetryInner {
                logs_dir: dir,
                state: Mutex::new(MetricsState { file, current_date: today }),
            }),
        })
    }

    /// Append one metrics data point to the daily JSONL file.
    pub fn record(&self, data: &Value) {
        let mut guard = self.inner.state.lock().expect("metrics mutex poisoned");
        let today = today_date();
        if guard.current_date != today {
            match open_metrics_file(&self.inner.logs_dir, &today) {
                Ok(f) => {
                    guard.file = f;
                    guard.current_date = today;
                }
                Err(e) => {
                    eprintln!("memory metrics rotate error: {e}");
                    return;
                }
            }
        }
        if let Ok(line) = serde_json::to_string(data) {
            let _ = writeln!(guard.file, "{line}");
            let _ = guard.file.flush();
        }
    }

    /// Attach remote trace context from MCP-lite `_trace_id`/`_span_id` params
    /// and baggage key-values to the current OTEL context.
    ///
    /// Keep the returned [`ContextGuard`] alive for the duration of the span.
    pub fn attach_context(params: &Value, baggage_kvs: Vec<KeyValue>) -> ContextGuard {
        let mut cx = Context::current();

        if let (Some(tid), Some(sid)) = (
            params.get("_trace_id").and_then(|v| v.as_str()),
            params.get("_span_id").and_then(|v| v.as_str()),
        ) {
            if let Some(remote_cx) = remote_context_from_ids(tid, sid) {
                cx = remote_cx;
            }
        }

        if !baggage_kvs.is_empty() {
            cx = cx.with_baggage(baggage_kvs);
        }

        cx.attach()
    }
}

fn open_metrics_file(dir: &PathBuf, date: &str) -> anyhow::Result<File> {
    let path = dir.join(format!("memory-metrics-{date}.jsonl"));
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

pub fn ts_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn today_date() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, m, d) = days_to_ymd(secs / 86400);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let y = 1970 + days / 365;
    let rem = days % 365;
    (y, (1 + rem / 30).min(12), (1 + rem % 30).min(28))
}

fn remote_context_from_ids(trace_id_hex: &str, span_id_hex: &str) -> Option<Context> {
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    if trace_id_hex.len() != 32 || span_id_hex.len() != 16 {
        return None;
    }
    let tid_bytes = hex::decode(trace_id_hex).ok()?;
    let sid_bytes = hex::decode(span_id_hex).ok()?;
    if tid_bytes.len() != 16 || sid_bytes.len() != 8 {
        return None;
    }
    let mut tid = [0u8; 16];
    let mut sid = [0u8; 8];
    tid.copy_from_slice(&tid_bytes);
    sid.copy_from_slice(&sid_bytes);
    let sc = SpanContext::new(
        TraceId::from_bytes(tid),
        SpanId::from_bytes(sid),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    Some(Context::new().with_remote_span_context(sc))
}

/// Round to 1 decimal place for timing metrics.
pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Round to 3 decimal places for similarity scores.
pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}
