use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod db;
mod handlers;
mod metrics;
mod tools;

use anyhow::Result;
use metrics::{check_metric, write_metric, GuardTelemetry};
use sdk_rust::{setup_otel, McpLiteServer};
use serde_json::Value;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, info_span};

const DEFAULT_LOGS_DIR: &str = "logs";
const DEFAULT_DB_PATH: &str = "data/guard.db";

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir =
        env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());
    let _otel_guard = setup_otel("guard", &logs_dir)
        .inspect_err(
            |e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"),
        )
        .ok();
    let db_path = env::var("GUARD_DB_PATH").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());

    // Open (or create) the SQLite database. Wrap in Arc<Mutex> for shared use
    // across tool handlers — all handlers are sync, so a std Mutex is fine.
    let conn = db::open(&db_path)?;
    let conn = Arc::new(Mutex::new(conn));

    let telemetry = GuardTelemetry::new(&logs_dir)?;
    let mut server = McpLiteServer::new(tools::make_tools(), "ready");

    // ---- guard.check --------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.check", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            // Sync work done before async block — ContextGuard is !Send
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.check")],
                );
                let platform = params
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                info_span!(
                    "guard.check",
                    platform = platform,
                    status = tracing::field::Empty,
                    duration_ms = tracing::field::Empty
                )
            };
            let platform = params
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_check(&c, params))
            };
            match &result {
                Ok(payload) => {
                    let v: Value = serde_json::from_str(payload).unwrap_or_default();
                    let reason = v
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let status = if reason == "blocked" { "blocked" } else { "allowed" };
                    span.record("status", status);
                    span.record("duration_ms", metrics::sdk_elapsed_ms(started));
                    info!(platform = platform.as_str(), status, reason, "guard.check.ok");
                    tel.record(&check_metric(&platform, status, started));
                }
                Err(err) => {
                    span.record("status", "error");
                    span.record("duration_ms", metrics::sdk_elapsed_ms(started));
                    error!(platform = platform.as_str(), error = %err, "guard.check.error");
                    tel.record(&check_metric(&platform, "error", started));
                }
            }
            async move { result }
        });
    }

    // ---- guard.allow --------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.allow", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.allow")],
                );
                let platform = params
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let channel_id = params
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                info_span!(
                    "guard.allow",
                    platform = platform,
                    channel_id = channel_id,
                    status = tracing::field::Empty
                )
            };
            let platform = params
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let channel_id = params
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_allow(&c, params))
            };
            match &result {
                Ok(_) => {
                    span.record("status", "ok");
                    info!(platform = platform.as_str(), channel_id = channel_id.as_str(), "guard.allow.ok");
                    tel.record(&write_metric("guard.allow", &platform, "ok", started));
                }
                Err(err) => {
                    span.record("status", "error");
                    error!(platform = platform.as_str(), error = %err, "guard.allow.error");
                    tel.record(&write_metric("guard.allow", &platform, "error", started));
                }
            }
            async move { result }
        });
    }

    // ---- guard.block --------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.block", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.block")],
                );
                let platform = params
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let channel_id = params
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                info_span!(
                    "guard.block",
                    platform = platform,
                    channel_id = channel_id,
                    status = tracing::field::Empty
                )
            };
            let platform = params
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let channel_id = params
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_block(&c, params))
            };
            match &result {
                Ok(_) => {
                    span.record("status", "ok");
                    info!(platform = platform.as_str(), channel_id = channel_id.as_str(), "guard.block.ok");
                    tel.record(&write_metric("guard.block", &platform, "ok", started));
                }
                Err(err) => {
                    span.record("status", "error");
                    error!(platform = platform.as_str(), error = %err, "guard.block.error");
                    tel.record(&write_metric("guard.block", &platform, "error", started));
                }
            }
            async move { result }
        });
    }

    // ---- guard.name ---------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.name", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.name")],
                );
                let platform = params
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let channel_id = params
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                info_span!(
                    "guard.name",
                    platform = platform,
                    channel_id = channel_id,
                    status = tracing::field::Empty
                )
            };
            let platform = params
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let channel_id = params
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_name(&c, params))
            };
            match &result {
                Ok(payload) => {
                    let v: Value = serde_json::from_str(payload).unwrap_or_default();
                    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    let s = if ok { "ok" } else { "not_found" };
                    span.record("status", s);
                    info!(platform = platform.as_str(), channel_id = channel_id.as_str(), updated = ok, "guard.name.ok");
                    tel.record(&write_metric("guard.name", &platform, s, started));
                }
                Err(err) => {
                    span.record("status", "error");
                    error!(platform = platform.as_str(), error = %err, "guard.name.error");
                    tel.record(&write_metric("guard.name", &platform, "error", started));
                }
            }
            async move { result }
        });
    }

    // ---- guard.remove -------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.remove", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.remove")],
                );
                let platform = params
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let channel_id = params
                    .get("channel_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                info_span!(
                    "guard.remove",
                    platform = platform,
                    channel_id = channel_id,
                    status = tracing::field::Empty
                )
            };
            let platform = params
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let channel_id = params
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_remove(&c, params))
            };
            let status = match &result {
                Ok(payload) => {
                    let v: Value = serde_json::from_str(payload).unwrap_or_default();
                    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    let s = if ok { "ok" } else { "not_found" };
                    span.record("status", s);
                    info!(platform = platform.as_str(), channel_id = channel_id.as_str(), removed = ok, "guard.remove.ok");
                    s
                }
                Err(err) => {
                    span.record("status", "error");
                    error!(platform = platform.as_str(), error = %err, "guard.remove.error");
                    "error"
                }
            };
            tel.record(&write_metric("guard.remove", &platform, status, started));
            async move { result }
        });
    }

    // ---- guard.list ---------------------------------------------------------
    {
        let conn = Arc::clone(&conn);
        let tel = telemetry.clone();
        server.register_tool("guard.list", move |params: Value| {
            let conn = Arc::clone(&conn);
            let tel = tel.clone();
            let span = {
                let _cx = GuardTelemetry::attach_context(
                    &params,
                    vec![opentelemetry::KeyValue::new("tool", "guard.list")],
                );
                info_span!("guard.list", count = tracing::field::Empty)
            };
            let started = Instant::now();
            let result = {
                let _enter = span.enter();
                conn.lock()
                    .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
                    .and_then(|c| handlers::handle_list(&c))
            };
            match &result {
                Ok(payload) => {
                    let v: Value = serde_json::from_str(payload).unwrap_or_default();
                    let count = v.get("count").and_then(Value::as_u64).unwrap_or(0);
                    span.record("count", count);
                    info!(count, "guard.list.ok");
                    tel.record(&write_metric("guard.list", "all", "ok", started));
                }
                Err(err) => {
                    error!(error = %err, "guard.list.error");
                    tel.record(&write_metric("guard.list", "all", "error", started));
                }
            }
            async move { result }
        });
    }

    info!(addr = "0.0.0.0:9004", db = %db_path, "guard.start");
    server.serve_auto("0.0.0.0:9004").await?;
    Ok(())
}
