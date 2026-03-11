use crate::config::CortexConfig;
use crate::llm::{build_prompt, complete, prompt_preview};
use crate::metrics::{elapsed_ms, step_err, step_ok, CortexTelemetry};
use anyhow::{anyhow, Result};
use opentelemetry::KeyValue;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info};

pub fn handle_describe_boundary() -> String {
    json!({
        "phase": "phase1",
        "status": "step-ready",
        "service_boundary": {
            "is_service": true,
            "transport": "mcp-lite-json-uds",
            "python_shell_role": "temporary pre-cortex shell",
            "llm_calling_rule": "cortex-only in target architecture"
        },
        "owns_now": [
            "service identity",
            "mcp-lite socket boundary",
            "config-backed system prompt loading",
            "single-step llm execution",
            "step observability"
        ],
        "does_not_own_yet": [
            "tool routing",
            "memory retrieval",
            "plan store",
            "segmented stm"
        ]
    })
    .to_string()
}

pub fn handle_step(params: Value, tel: Arc<CortexTelemetry>) -> Result<String> {
    let p = params
        .as_object()
        .ok_or_else(|| anyhow!("params must be an object"))?;
    let session_id = p
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let user_input = p
        .get("user_input")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let requested_agent = p.get("agent_name").and_then(|v| v.as_str()).map(str::trim);

    if session_id.is_empty() {
        return Err(anyhow!("session_id is required"));
    }
    if user_input.is_empty() {
        return Err(anyhow!("user_input is required"));
    }

    let _cx_guard = CortexTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("service", "cortex"),
            KeyValue::new("op", "step"),
            KeyValue::new("session_id", session_id.clone()),
        ],
    );

    let span = tracing::info_span!(
        "cortex.step",
        session_id = %session_id,
        agent_name = tracing::field::Empty,
        provider_kind = tracing::field::Empty,
        model = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
        user_input_len = user_input.len(),
        output_len = tracing::field::Empty,
    );
    let _enter = span.enter();

    let started = Instant::now();
    let cfg_file = CortexConfig::load()?;
    let resolved = cfg_file
        .cfg
        .resolve_step_config(cfg_file.path.clone(), requested_agent);
    let prompt = build_prompt(&resolved.system_prompt, &user_input);

    span.record("agent_name", resolved.agent_name.as_str());
    span.record("provider_kind", resolved.provider.kind.as_str());
    span.record("model", resolved.provider.model.as_str());

    info!(
        agent_name = %resolved.agent_name,
        provider_kind = %resolved.provider.kind,
        config_path = %resolved.source_path.display(),
        prompt_meta = %prompt_preview(&prompt).to_string(),
        "cortex.step.start"
    );

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { complete(&resolved.provider, &prompt).await })
    });

    match result {
        Ok(output) => {
            let duration_ms = elapsed_ms(started);
            span.record("status", "ok");
            span.record("duration_ms", duration_ms);
            span.record("output_len", output.content.len() as i64);
            info!(
                agent_name = %resolved.agent_name,
                provider_kind = %output.provider_kind,
                model = %output.model,
                duration_ms,
                output_len = output.content.len(),
                "cortex.step.ok"
            );
            tel.record(&step_ok(
                &session_id,
                &resolved.agent_name,
                &output.provider_kind,
                &output.model,
                &resolved.source_path.display().to_string(),
                duration_ms,
                user_input.len(),
                output.content.len(),
            ));

            Ok(json!({
                "session_id": session_id,
                "agent_name": resolved.agent_name,
                "provider_kind": output.provider_kind,
                "model": output.model,
                "response_text": output.content,
                "tool_activity_summary": null
            })
            .to_string())
        }
        Err(err) => {
            let duration_ms = elapsed_ms(started);
            span.record("status", "error");
            span.record("duration_ms", duration_ms);
            error!(
                agent_name = %resolved.agent_name,
                provider_kind = %resolved.provider.kind,
                model = %resolved.provider.model,
                duration_ms,
                error = %err,
                "cortex.step.error"
            );
            tel.record(&step_err(
                &session_id,
                &resolved.agent_name,
                &resolved.provider.kind,
                &resolved.provider.model,
                &resolved.source_path.display().to_string(),
                duration_ms,
                user_input.len(),
            ));
            Err(err)
        }
    }
}
