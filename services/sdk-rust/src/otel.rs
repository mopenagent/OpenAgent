//! OTEL tracing for Rust MCP-lite services — dual export: file + optional Jaeger.
//!
//! Each service calls [`setup_otel`] at startup to initialise a TracerProvider
//! that writes spans as OTLP-compatible JSON to:
//!
//!   `<logs_dir>/<service_name>-traces-YYYY-MM-DD.jsonl`
//!
//! If `OTEL_EXPORTER_OTLP_ENDPOINT` is set (e.g. `http://localhost:4318`),
//! spans are also exported to a Jaeger/collector via `opentelemetry-otlp`
//! (standard OTLP/HTTP with protobuf, retry, and proper headers).
//! File export always runs regardless of whether the collector is reachable.
//!
//! Daily rotation and 1-day retention are managed by [`DailyFileWriter`].
//!
//! # Usage
//! ```ignore
//! use sdk_rust::otel::setup_otel;
//!
//! #[tokio::main]
//! async fn main() {
//!     let _guard = setup_otel("browser", "logs").expect("otel init");
//!     // spans from tracing! macros are forwarded to OTEL + file (+ Jaeger if configured)
//! }
//! ```

use futures_util::future::BoxFuture;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::trace::Status as TraceStatus;
use opentelemetry_sdk::{
    export::trace::{ExportResult, SpanData, SpanExporter},
    runtime,
    trace::TracerProvider,
    Resource,
};
use opentelemetry::KeyValue;
use serde_json::{json, Value};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// ---------------------------------------------------------------------------
// Daily rotating file writer
// ---------------------------------------------------------------------------

/// Thread-safe file writer that rotates daily and keeps 1 day of logs.
///
/// `Clone` is a shallow clone — the underlying file handle is shared.
#[derive(Clone)]
pub struct DailyFileWriter {
    logs_dir: PathBuf,
    prefix: String,
    inner: Arc<Mutex<DailyWriterInner>>,
}

impl std::fmt::Debug for DailyFileWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DailyFileWriter")
            .field("logs_dir", &self.logs_dir)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

struct DailyWriterInner {
    file: File,
    current_date: String,
}

impl DailyFileWriter {
    /// Create a new daily-rotating file writer under `logs_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Io`] if the directory cannot be created or the
    /// initial log file cannot be opened.
    pub fn new(logs_dir: impl Into<PathBuf>, prefix: impl Into<String>) -> crate::Result<Self> {
        let logs_dir = logs_dir.into();
        let prefix = prefix.into();
        fs::create_dir_all(&logs_dir)?;
        let today = today_str();
        let file = open_file(&logs_dir, &prefix, &today)?;
        Ok(Self {
            logs_dir,
            prefix,
            inner: Arc::new(Mutex::new(DailyWriterInner {
                file,
                current_date: today,
            })),
        })
    }

    /// Append a line to today's log file, rotating if the date has changed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Io`] if the file cannot be opened, written, or flushed.
    pub fn write_line(&self, line: &str) -> crate::Result<()> {
        let mut guard = self.inner.lock().expect("log file mutex poisoned");
        let today = today_str();
        if guard.current_date != today {
            let new_file = open_file(&self.logs_dir, &self.prefix, &today)?;
            guard.file = new_file;
            guard.current_date = today.clone();
            self.purge_old(&today);
        }
        writeln!(guard.file, "{line}")?;
        guard.file.flush()?;
        Ok(())
    }

    fn purge_old(&self, today: &str) {
        let Ok(entries) = fs::read_dir(&self.logs_dir) else {
            return;
        };
        let prefix_dash = format!("{}-", self.prefix);
        let today_dt = approx_date(today);
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(&prefix_dash) {
                continue;
            }
            let rest = &name[prefix_dash.len()..];
            if rest.len() < 10 {
                continue;
            }
            let date_str = &rest[..10];
            let file_dt = approx_date(date_str);
            if let (Some(t), Some(f)) = (today_dt, file_dt) {
                if t > f + 1 {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

}

fn open_file(dir: &PathBuf, prefix: &str, date: &str) -> std::io::Result<File> {
    let path = dir.join(format!("{prefix}-{date}.jsonl"));
    OpenOptions::new().create(true).append(true).open(path)
}

fn today_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Returns an approximate days-since-epoch value for comparison only.
fn approx_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: u64 = parts[0].parse().ok()?;
    let m: u64 = parts[1].parse().ok()?;
    let d: u64 = parts[2].parse().ok()?;
    Some(y * 365 + m * 30 + d)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let y = 1970 + days / 365;
    let remaining = days % 365;
    let m = 1 + remaining / 30;
    let d = 1 + remaining % 30;
    (y, m.min(12), d.min(28))
}

// ---------------------------------------------------------------------------
// Custom file span exporter
// ---------------------------------------------------------------------------

/// Exports OTEL spans to a daily-rotating JSONL file.
///
/// Jaeger / collector export is handled by a separate `opentelemetry-otlp`
/// exporter added alongside this one in [`setup_otel_inner`].
struct FileSpanExporter {
    inner: Arc<Mutex<DailyWriterInner>>,
    logs_dir: PathBuf,
    prefix: String,
}

impl fmt::Debug for FileSpanExporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSpanExporter")
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl SpanExporter for FileSpanExporter {
    fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        let lines: Vec<String> = batch.iter().map(serialize_span).collect();
        let inner = self.inner.clone();
        let logs_dir = self.logs_dir.clone();
        let prefix = self.prefix.clone();

        Box::pin(async move {
            let mut guard = inner.lock().expect("log file mutex poisoned");
            let today = today_str();
            if guard.current_date != today {
                match open_file(&logs_dir, &prefix, &today) {
                    Ok(f) => {
                        guard.file = f;
                        guard.current_date = today;
                    }
                    Err(e) => {
                        return Err(opentelemetry::trace::TraceError::from(e.to_string()))
                    }
                }
            }
            for line in &lines {
                if let Err(e) = writeln!(guard.file, "{}", line) {
                    return Err(opentelemetry::trace::TraceError::from(e.to_string()));
                }
            }
            let _ = guard.file.flush();
            Ok(())
        })
    }
}

/// Serialize a span's core fields into a JSON `Value` (no OTLP envelope).
/// Used by both [`serialize_span`] (file) and [`batch_to_otlp_json`] (Jaeger).
fn span_to_value(span: &SpanData) -> Value {
    let ctx = &span.span_context;
    let trace_id = format!("{:032x}", ctx.trace_id());
    let span_id = format!("{:016x}", ctx.span_id());
    let parent_span_id = if span.parent_span_id != opentelemetry::trace::SpanId::INVALID {
        format!("{:016x}", span.parent_span_id)
    } else {
        String::new()
    };

    let attrs: Vec<Value> = span
        .attributes
        .iter()
        .map(|kv| json!({"key": kv.key.as_str(), "value": kv_to_json(&kv.value)}))
        .collect();

    let events: Vec<Value> = span
        .events
        .iter()
        .map(|e| {
            let ev_attrs: Vec<Value> = e
                .attributes
                .iter()
                .map(|kv| json!({"key": kv.key.as_str(), "value": kv_to_json(&kv.value)}))
                .collect();
            json!({
                "timeUnixNano": e.timestamp.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_nanos().to_string(),
                "name": e.name.as_ref(),
                "attributes": ev_attrs,
            })
        })
        .collect();

    let start_ns = span
        .start_time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    let end_ns = span
        .end_time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();

    let (status_code, status_msg) = match &span.status {
        TraceStatus::Ok => (1i32, String::new()),
        TraceStatus::Error { description } => (2, description.to_string()),
        _ => (0, String::new()),
    };

    json!({
        "traceId": trace_id,
        "spanId": span_id,
        "parentSpanId": parent_span_id,
        "name": span.name.as_ref(),
        "kind": span.span_kind.clone() as i32,
        "startTimeUnixNano": start_ns,
        "endTimeUnixNano": end_ns,
        "attributes": attrs,
        "events": events,
        "status": { "code": status_code, "message": status_msg },
    })
}

/// Serialize a single span into an OTLP-envelope JSON string (for file export).
fn serialize_span(span: &SpanData) -> String {
    let svc = span.instrumentation_scope.name();
    let obj = json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [{"key": "service.name", "value": {"stringValue": svc}}]
            },
            "scopeSpans": [{
                "scope": { "name": svc },
                "spans": [span_to_value(span)],
            }]
        }]
    });
    obj.to_string()
}

fn kv_to_json(v: &opentelemetry::Value) -> Value {
    match v {
        opentelemetry::Value::String(s) => json!({ "stringValue": s.as_str() }),
        opentelemetry::Value::Bool(b) => json!({ "boolValue": b }),
        opentelemetry::Value::I64(i) => json!({ "intValue": i.to_string() }),
        opentelemetry::Value::F64(f) => json!({ "doubleValue": f }),
        opentelemetry::Value::Array(_) | &_ => json!({ "stringValue": v.to_string() }),
    }
}

// ---------------------------------------------------------------------------
// OTEL setup
// ---------------------------------------------------------------------------

/// Guard returned by [`setup_otel`]. Drop this to flush and shut down the tracer.
pub struct OTELGuard {
    provider: TracerProvider,
    _log_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl std::fmt::Debug for OTELGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OTELGuard").finish_non_exhaustive()
    }
}

impl Drop for OTELGuard {
    fn drop(&mut self) {
        for result in self.provider.force_flush() {
            if let Err(e) = result {
                eprintln!("otel flush error: {e}");
            }
        }
    }
}

/// Initialise a global `TracerProvider` and bridge `tracing` → OpenTelemetry.
///
/// Writes spans as OTLP-compatible JSON to
/// `<logs_dir>/<service_name>-traces-YYYY-MM-DD.jsonl`.
///
/// Returns a guard that flushes and shuts down the tracer on drop.
///
/// # Errors
///
/// Returns [`crate::Error::OtelSetup`] if the log directory cannot be created,
/// the initial trace file cannot be opened, or the subscriber cannot be
/// installed.
pub fn setup_otel(service_name: &str, logs_dir: &str) -> crate::Result<OTELGuard> {
    setup_otel_inner(service_name, logs_dir)
        .map_err(|e| crate::Error::OtelSetup(e.to_string()))
}

fn setup_otel_inner(service_name: &str, logs_dir: &str) -> anyhow::Result<OTELGuard> {
    let logs_path = PathBuf::from(logs_dir);
    fs::create_dir_all(&logs_path)?;
    let prefix = format!("{}-traces", service_name);
    let today = today_str();
    let file = open_file(&logs_path, &prefix, &today)?;
    let inner = Arc::new(Mutex::new(DailyWriterInner {
        file,
        current_date: today,
    }));

    let file_exporter = FileSpanExporter { inner, logs_dir: logs_path, prefix };

    let resource = Resource::new(vec![
        KeyValue::new("service.name", service_name.to_owned()),
        KeyValue::new("telemetry.sdk.language", "rust"),
    ]);

    // Always write spans to daily-rotating JSONL files.
    // If OTEL_EXPORTER_OTLP_ENDPOINT is set, also export via standard OTLP/HTTP
    // to Jaeger or any compatible collector (retry, headers, compression included).
    let mut builder = TracerProvider::builder()
        .with_batch_exporter(file_exporter, runtime::Tokio)
        .with_resource(resource);

    if let Ok(ep) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        use opentelemetry_otlp::WithExportConfig as _;
        let url = format!("{}/v1/traces", ep.trim_end_matches('/'));
        match opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(url)
            .build()
        {
            Ok(otlp_exporter) => {
                builder = builder.with_batch_exporter(otlp_exporter, runtime::Tokio);
            }
            Err(e) => eprintln!("OTLP span exporter init failed — Jaeger disabled: {e}"),
        }
    }

    let provider = builder.build();

    let tracer = provider.tracer(service_name.to_owned());

    // File appender for standard tracing logs
    let file_appender = tracing_appender::rolling::daily(logs_dir, format!("{}-logs", service_name));
    let (non_blocking_appender, log_guard) = tracing_appender::non_blocking(file_appender);

    // Bridge tracing → OTEL
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with(OpenTelemetryLayer::new(tracer))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(tracing_subscriber::fmt::layer().json().with_writer(non_blocking_appender))
        .try_init()
        .ok(); // ignore "already set" on repeated calls in tests

    Ok(OTELGuard { provider, _log_guard: log_guard })
}

// ---------------------------------------------------------------------------
// Trace context extraction from MCP-lite frames
// ---------------------------------------------------------------------------

/// Extract a remote span context from trace_id / span_id hex strings
/// propagated in a MCP-lite ToolCallRequest frame.
///
/// Returns an OTEL `Context` with the remote parent set if IDs are valid.
pub fn context_from_ids(trace_id_hex: &str, span_id_hex: &str) -> Option<opentelemetry::Context> {
    use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};

    if trace_id_hex.len() != 32 || span_id_hex.len() != 16 {
        return None;
    }
    let tid_bytes = hex::decode(trace_id_hex).ok()?;
    let sid_bytes = hex::decode(span_id_hex).ok()?;
    if tid_bytes.len() != 16 || sid_bytes.len() != 8 {
        return None;
    }
    let mut tid_arr = [0u8; 16];
    let mut sid_arr = [0u8; 8];
    tid_arr.copy_from_slice(&tid_bytes);
    sid_arr.copy_from_slice(&sid_bytes);

    let sc = SpanContext::new(
        TraceId::from_bytes(tid_arr),
        SpanId::from_bytes(sid_arr),
        TraceFlags::SAMPLED,
        true, // remote
        TraceState::default(),
    );
    Some(opentelemetry::Context::new().with_remote_span_context(sc))
}
