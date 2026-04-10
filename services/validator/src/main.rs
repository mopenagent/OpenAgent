use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod handlers;
mod metrics;
mod repair;
mod tools;

use anyhow::Result;
use metrics::{elapsed_ms, repair_metric, ValidatorTelemetry};
use sdk_rust::{setup_otel, McpLiteServer};
use serde_json::Value;
use std::env;
use std::time::Instant;
use tracing::{error, info, info_span};

const DEFAULT_LOGS_DIR: &str = "logs";

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());
    let _otel_guard = setup_otel("validator", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let telemetry = ValidatorTelemetry::new(&logs_dir)?;
    let mut server = McpLiteServer::new(tools::make_tools(), "ready");

    let tel = telemetry.clone();
    server.register_tool("validator.repair_json", move |params: Value| {
        let tel = tel.clone();
        async move {
        let mode = params
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_string();
        let input_len = params.get("text").and_then(Value::as_str).map_or(0, str::len);
        // ContextGuard is !Send — create span inside its scope then drop before any await.
        let span = {
            let _cx_guard = ValidatorTelemetry::attach_context(
                &params,
                vec![opentelemetry::KeyValue::new("tool", "validator.repair_json")],
            );
            info_span!(
                "validator.repair_json",
                backend = "llm_json",
                mode = mode.as_str(),
                input_len = input_len,
                status = tracing::field::Empty,
                was_repaired = tracing::field::Empty,
                changed = tracing::field::Empty,
                output_len = tracing::field::Empty,
                duration_ms = tracing::field::Empty
            )
        };
        let _enter = span.enter();
        let started = Instant::now();

        let result = handlers::handle_repair_json(params);
        let duration_ms = elapsed_ms(started);

        match &result {
            Ok(payload) => {
                let parsed: Value = serde_json::from_str(payload).unwrap_or_else(|_| {
                    serde_json::json!({"ok": false, "error": "invalid_handler_payload"})
                });
                let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let was_repaired = parsed.get("was_repaired").and_then(Value::as_bool).unwrap_or(false);
                let changed = parsed.get("changed").and_then(Value::as_bool).unwrap_or(false);
                let output_len = parsed.get("json").and_then(Value::as_str).map_or(0, str::len);
                let status = if ok { "ok" } else { "unable_to_repair" };

                span.record("status", status);
                span.record("was_repaired", was_repaired);
                span.record("changed", changed);
                span.record("output_len", output_len as i64);
                span.record("duration_ms", duration_ms);

                if ok {
                    info!(
                        backend = "llm_json",
                        mode = mode.as_str(),
                        input_len,
                        output_len,
                        was_repaired,
                        changed,
                        duration_ms,
                        "validator.repair.ok"
                    );
                } else {
                    info!(
                        backend = "llm_json",
                        mode = mode.as_str(),
                        input_len,
                        output_len,
                        was_repaired,
                        changed,
                        duration_ms,
                        error = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("unable_to_repair"),
                        message = parsed.get("message").and_then(|v| v.as_str()).unwrap_or("repair failed"),
                        "validator.repair.unable_to_repair"
                    );
                }

                tel.record(&repair_metric(
                    mode.as_str(),
                    status,
                    was_repaired,
                    changed,
                    input_len,
                    output_len,
                    duration_ms,
                ));
            }
            Err(err) => {
                span.record("status", "error");
                span.record("was_repaired", false);
                span.record("changed", false);
                span.record("output_len", 0_i64);
                span.record("duration_ms", duration_ms);

                error!(
                    backend = "llm_json",
                    mode = mode.as_str(),
                    input_len,
                    duration_ms,
                    error = %err,
                    "validator.repair.error"
                );

                tel.record(&repair_metric(
                    mode.as_str(),
                    "error",
                    false,
                    false,
                    input_len,
                    0,
                    duration_ms,
                ));
            }
        }

        result
        }
    });

    info!(addr = "0.0.0.0:9005", backend = "llm_json", "validator.start");
    server.serve_auto("0.0.0.0:9005").await?;
    Ok(())
}
