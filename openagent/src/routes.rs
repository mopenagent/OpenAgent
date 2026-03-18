/// Axum route handlers for the openagent control plane.
///
/// POST /step   — run one Cortex reasoning step (guard-checked by middleware)
/// GET  /health — liveness + registered service names
/// GET  /tools  — all tools registered from all running services
/// POST /tool/:name — raw tool call (internal / debug use)
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;
use tracing::{error, info};

use crate::metrics::{step_metric, tool_metric};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// GET /health
// ---------------------------------------------------------------------------

/// Read RSS memory (KB → MB) for a given PID via `ps`. Works on macOS + Linux.
/// Returns None if the process no longer exists or ps fails.
async fn rss_mb(pid: u32) -> Option<f64> {
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let kb: u64 = std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok()?;
        Some((kb as f64) / 1024.0)
    })
    .await
    .ok()
    .flatten()
}

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let uptime_s = state.started_at.elapsed().as_secs();
    let tool_count = state.manager.tools().await.len();
    let live = state.manager.live_services().await;

    // openagent runtime itself
    let self_pid = std::process::id();
    let self_rss = rss_mb(self_pid).await;

    // Per-service info — gather RSS concurrently
    let service_futs: Vec<_> = live
        .iter()
        .map(|svc| {
            let name = svc.name.clone();
            let pid = svc.pid;
            async move {
                let rss = if let Some(p) = pid { rss_mb(p).await } else { None };
                json!({
                    "name": name,
                    "pid": pid,
                    "rss_mb": rss.map(|v| (v * 10.0).round() / 10.0),
                    "status": "running",
                })
            }
        })
        .collect();
    let services: Vec<Value> = futures_util::future::join_all(service_futs).await;

    Json(json!({
        "status": "ok",
        "uptime_s": uptime_s,
        "tool_count": tool_count,
        "self": {
            "name": "openagent",
            "pid": self_pid,
            "rss_mb": self_rss.map(|v| (v * 10.0).round() / 10.0),
        },
        "services": services,
    }))
}

// ---------------------------------------------------------------------------
// GET /tools
// ---------------------------------------------------------------------------

pub async fn list_tools(State(state): State<AppState>) -> impl IntoResponse {
    let tools = state.manager.tools().await;
    let entries: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "service": t.service,
                "name": t.definition.get("name").cloned().unwrap_or_default(),
                "description": t.definition.get("description").cloned().unwrap_or_default(),
            })
        })
        .collect();
    Json(json!({"tools": entries, "count": entries.len()}))
}

// ---------------------------------------------------------------------------
// POST /step
// ---------------------------------------------------------------------------

/// Request body for `POST /step`.
///
/// `platform` + `channel_id` are consumed by the guard middleware before the
/// request reaches this handler; they are still part of the body so the
/// middleware can read them without a separate header convention.
#[derive(Debug, Deserialize)]
pub struct StepRequest {
    /// Platform of the originating message (e.g. "telegram", "discord").
    pub platform: String,
    /// Platform-specific sender/channel identifier.
    pub channel_id: String,
    /// Session identifier — passed through to Cortex for memory continuity.
    pub session_id: String,
    /// The user's message text.
    pub user_input: String,
    /// Optional agent name; Cortex resolves to `default` if omitted.
    pub agent_name: Option<String>,
    /// "generation" (default) or "tool_call".
    pub turn_kind: Option<String>,
}

pub async fn step(
    State(state): State<AppState>,
    Json(req): Json<StepRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    info!(
        platform = %req.platform,
        channel_id = %req.channel_id,
        session_id = %req.session_id,
        user_input_len = req.user_input.len(),
        "openagent.step.start"
    );

    let mut params = json!({
        "session_id": req.session_id,
        "user_input": req.user_input,
    });

    if let Some(name) = &req.agent_name {
        params["agent_name"] = Value::String(name.clone());
    }
    if let Some(kind) = &req.turn_kind {
        params["turn_kind"] = Value::String(kind.clone());
    }

    match state
        .manager
        .call_tool("cortex.step", params, 120_000)
        .await
    {
        Ok(payload) => {
            info!(session_id = %req.session_id, "openagent.step.ok");
            state.metrics.record(&step_metric(&req.platform, &req.channel_id, &req.session_id, "ok", started));
            match serde_json::from_str::<Value>(&payload) {
                Ok(v) => (StatusCode::OK, Json(v)).into_response(),
                Err(e) => {
                    error!(error = %e, "openagent.step.parse_error");
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "response_parse_error", "detail": e.to_string()}))).into_response()
                }
            }
        }
        Err(e) => {
            error!(session_id = %req.session_id, error = %e, "openagent.step.error");
            state.metrics.record(&step_metric(&req.platform, &req.channel_id, &req.session_id, "error", started));
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "step_failed", "detail": e.to_string()}))).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// POST /tool/:name  (internal / debug)
// ---------------------------------------------------------------------------

pub async fn call_tool(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let started = Instant::now();
    let params: Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_json", "detail": e.to_string()})),
                )
                    .into_response()
            }
        }
    };

    // Intercept service control tools — handled directly in the control plane.
    if name.starts_with("service.") {
        return service_control(&state, &name, params).await.into_response();
    }

    match state.manager.call_tool(&name, params, 30_000).await {
        Ok(result) => {
            state.metrics.record(&tool_metric(&name, "ok", started));
            let v: Value = serde_json::from_str(&result).unwrap_or(Value::String(result));
            (StatusCode::OK, Json(v)).into_response()
        }
        Err(e) => {
            state.metrics.record(&tool_metric(&name, "error", started));
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Service control (service.list / service.restart)
// ---------------------------------------------------------------------------

async fn service_control(state: &AppState, tool: &str, params: Value) -> impl IntoResponse {
    match tool {
        "service.list" => {
            let live = state.manager.live_services().await;
            let services: Vec<Value> = live
                .iter()
                .map(|s| json!({"name": s.name, "pid": s.pid, "status": "running"}))
                .collect();
            (StatusCode::OK, Json(json!({"services": services, "count": services.len()}))).into_response()
        }

        "service.restart" => {
            let svc_name = match params.get("name").and_then(Value::as_str) {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "name required"}))).into_response(),
            };

            let live = state.manager.live_services().await;
            match live.iter().find(|s| s.name == svc_name) {
                None => {
                    (StatusCode::NOT_FOUND, Json(json!({"error": format!("service not found: {svc_name}")}))).into_response()
                }
                Some(svc) => {
                    let pid = match svc.pid {
                        Some(p) => p,
                        None => return (StatusCode::OK, Json(json!({"ok": false, "error": "no pid — service may be starting"}))).into_response(),
                    };
                    // SIGTERM — the run_service_loop health check detects the exit and restarts.
                    let killed = std::process::Command::new("kill")
                        .args(["-TERM", &pid.to_string()])
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    info!(service = %svc_name, pid, "service.restart.requested");
                    (StatusCode::OK, Json(json!({"ok": killed, "action": "restart", "service": svc_name, "pid": pid}))).into_response()
                }
            }
        }

        _ => (StatusCode::NOT_FOUND, Json(json!({"error": format!("unknown service tool: {tool}")}))).into_response(),
    }
}
