/// Axum route handlers for the openagent control plane.
///
/// POST /step   — reads AgentResult set by AgentLayer (guard-checked + agent ran in middleware)
/// GET  /health — liveness + registered service names
/// GET  /tools  — all tools registered from all running services
/// POST /tool/:name — raw tool call (internal / debug use)
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;
use tracing::info;

use crate::observability::metrics::{step_metric, tool_metric};
use super::middleware::AgentResult;
use super::state::AppState;

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

    let services: Vec<Value> = live
        .iter()
        .map(|svc| json!({"name": svc.name, "address": svc.address, "status": "connected"}))
        .collect();

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

/// POST /step — AgentLayer has already run the ReAct loop and stored the result
/// in `AgentResult`.  This handler simply reads that extension and returns it.
pub async fn step(
    State(state): State<AppState>,
    Extension(AgentResult(result)): Extension<AgentResult>,
    Json(req): Json<StepRequest>,
) -> impl IntoResponse {
    let started = Instant::now();
    info!(
        platform   = %req.platform,
        channel_id = %req.channel_id,
        session_id = %req.session_id,
        "openagent.step.ok"
    );
    state.metrics.record(&step_metric(&req.platform, &req.channel_id, &req.session_id, "ok", started));
    (StatusCode::OK, Json(result)).into_response()
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
// GET /webhook/whatsapp  — Meta webhook challenge verification
// POST /webhook/whatsapp — Meta inbound message webhook
// ---------------------------------------------------------------------------
//
// Meta sends a GET with hub.challenge to verify the endpoint.
// Subsequent POSTs carry message payloads; we parse them and inject
// message.received events onto the dispatch bus via ChannelHandle.
//
// The Go-based services/whatsapp/ (whatsmeow) remains the active inbound
// handler for the personal WhatsApp number. This endpoint is for the
// Cloud API number once Meta Business approval is obtained.

#[derive(serde::Deserialize)]
pub struct WhatsAppChallenge {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
}

pub async fn whatsapp_webhook_verify(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<WhatsAppChallenge>,
) -> impl IntoResponse {
    let cfg = &state.channel_handle.whatsapp_config();
    if q.mode.as_deref() == Some("subscribe")
        && q.verify_token.as_deref() == Some(cfg.verify_token.as_str())
    {
        let challenge = q.challenge.unwrap_or_default();
        info!("whatsapp.webhook.verified");
        (StatusCode::OK, challenge).into_response()
    } else {
        info!("whatsapp.webhook.verify.rejected");
        StatusCode::FORBIDDEN.into_response()
    }
}

pub async fn whatsapp_webhook_receive(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let injected = state.channel_handle.inject_whatsapp_webhook(&payload);
    info!(messages = injected, "whatsapp.webhook.received");
    StatusCode::OK.into_response()
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
                .map(|s| json!({"name": s.name, "address": s.address, "status": "connected"}))
                .collect();
            (StatusCode::OK, Json(json!({"services": services, "count": services.len()}))).into_response()
        }

        "service.restart" | "service.start" | "service.stop" => {
            // openagent no longer manages service lifecycles.
            // Use: systemctl restart openagent-<name>  (production)
            //      ./services.sh restart <name>         (dev)
            let svc_name = params.get("name").and_then(Value::as_str).unwrap_or("?");
            (StatusCode::OK, Json(json!({
                "ok": false,
                "error": format!(
                    "openagent does not manage service processes. \
                     Use: systemctl restart openagent-{svc_name}"
                )
            }))).into_response()
        }

        _ => (StatusCode::NOT_FOUND, Json(json!({"error": format!("unknown service tool: {tool}")}))).into_response(),
    }
}
