use opentelemetry::{ContextGuard, KeyValue};
use sdk_rust::{attach_context, ts_ms, MetricsWriter};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct CortexTelemetry {
    writer: MetricsWriter,
}

impl CortexTelemetry {
    pub fn new(logs_dir: &str) -> anyhow::Result<Self> {
        Ok(Self {
            writer: MetricsWriter::new(logs_dir, "cortex").map_err(|e| anyhow::anyhow!("{e}"))?,
        })
    }

    pub fn record(&self, data: &Value) {
        self.writer.record(data);
    }

    pub fn attach_context(params: &Value, baggage_kvs: Vec<KeyValue>) -> ContextGuard {
        attach_context(params, baggage_kvs)
    }
}

pub fn step_ok(
    session_id: &str,
    agent_name: &str,
    provider_kind: &str,
    model: &str,
    config_path: &str,
    duration_ms: f64,
    user_input_len: usize,
    output_len: usize,
) -> Value {
    json!({
        "ts_ms": ts_ms(),
        "service": "cortex",
        "op": "step",
        "status": "ok",
        "session_id": session_id,
        "agent_name": agent_name,
        "provider_kind": provider_kind,
        "model": model,
        "config_path": config_path,
        "duration_ms": duration_ms,
        "user_input_len": user_input_len,
        "output_len": output_len,
    })
}

pub fn step_err(
    session_id: &str,
    agent_name: &str,
    provider_kind: &str,
    model: &str,
    config_path: &str,
    duration_ms: f64,
    user_input_len: usize,
) -> Value {
    json!({
        "ts_ms": ts_ms(),
        "service": "cortex",
        "op": "step",
        "status": "error",
        "session_id": session_id,
        "agent_name": agent_name,
        "provider_kind": provider_kind,
        "model": model,
        "config_path": config_path,
        "duration_ms": duration_ms,
        "user_input_len": user_input_len,
    })
}

pub use sdk_rust::elapsed_ms;
