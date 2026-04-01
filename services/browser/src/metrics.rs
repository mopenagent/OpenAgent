//! OTEL observability facade for the Browser service.
//!
//! ```text
//! Pillar   │ Mechanism                              │ Output
//! ─────────┼────────────────────────────────────────┼──────────────────────────────────────
//! Traces   │ tracing::info_span! → OTEL bridge      │ logs/browser-traces-YYYY-MM-DD.jsonl
//! Metrics  │ BrowserTelemetry::record()             │ logs/browser-metrics-YYYY-MM-DD.jsonl
//! Logs     │ tracing::{info!,warn!,error!}          │ OTEL span events (same trace file)
//! Baggage  │ BrowserTelemetry::attach_context()     │ propagated in-process via Context
//! ```

use opentelemetry::{ContextGuard, KeyValue};
pub use sdk_rust::elapsed_ms;
use sdk_rust::{ts_ms, MetricsWriter};
use serde_json::{json, Value};

/// OTEL observability facade for the Browser service.
#[derive(Debug, Clone)]
pub struct BrowserTelemetry {
    writer: MetricsWriter,
}

impl BrowserTelemetry {
    pub fn new(logs_dir: &str) -> anyhow::Result<Self> {
        Ok(Self {
            writer: MetricsWriter::new(logs_dir, "browser").map_err(|e| anyhow::anyhow!("{e}"))?,
        })
    }

    /// Append one metrics data point to today's JSONL file (best-effort).
    pub fn record(&self, data: &Value) {
        self.writer.record(data);
    }

    /// Attach remote trace context from MCP-lite params + baggage key-values.
    ///
    /// Call at the top of every handler; keep the returned guard alive for the
    /// duration of the span so the child span is linked to the caller's trace.
    pub fn attach_context(params: &Value, baggage_kvs: Vec<KeyValue>) -> ContextGuard {
        sdk_rust::attach_context(params, baggage_kvs)
    }
}

// ── Metric record builders ────────────────────────────────────────────────────

pub fn tool_metric(tool: &str, status: &str, duration_ms: f64) -> Value {
    json!({
        "ts_ms":       ts_ms(),
        "service":     "browser",
        "op":          tool,
        "status":      status,
        "duration_ms": duration_ms,
    })
}
