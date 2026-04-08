/// TtsLayer — Axum middleware that synthesizes speech after the step handler.
///
/// When `middleware.tts.enabled = true` in `config/openagent.toml`:
///   1. Lets the inner handler run and buffers its response body.
///   2. Parses `response_text` from the JSON response.
///   3. Calls `tts.synthesize` with the configured voice / speed / language.
///   4. Adds `audio_path` to the response JSON alongside `response_text`.
///
/// When disabled (default) the middleware is a zero-cost pass-through.
///
/// Error policy:
///   - TTS service unavailable → response passes through as-is with a warning
///     (caller still gets the text; audio is best-effort).
///   - Response body is not JSON or has no `response_text` → pass-through unchanged.
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::time::Instant;
use tracing::{info, warn};

use crate::observability::metrics::tts_metric;
use super::state::AppState;

pub async fn tts_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Fast-path: disabled in config.
    if !state.config.tts.enabled {
        return next.run(req).await;
    }

    // Run the inner handler — all owned values, no borrows across await.
    let response = next.run(req).await;

    // Only post-process successful JSON responses.
    if !response.status().is_success() {
        return response;
    }

    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "tts.response.read.error");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": "tts_response_read_error"})),
            )
                .into_response();
        }
    };

    // Must be parseable JSON with a `response_text` field — otherwise it's not
    // a step response (e.g. /health, /tools) and we pass through unchanged.
    let Ok(mut body_json) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    let Some(response_text) = body_json
        .get("response_text")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    // session_id is present in the cortex step response — use it for metrics.
    let session_id = body_json
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let started = Instant::now();
    let tts_cfg = &state.config.tts;

    let params = json!({
        "text":     response_text,
        "voice":    tts_cfg.voice,
        "speed":    tts_cfg.speed,
        "language": tts_cfg.language,
    });

    match state.manager.call_tool("tts.synthesize", params, 30_000).await {
        Ok(payload) => {
            let v: Value = serde_json::from_str(&payload).unwrap_or_default();
            let audio_path = v
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            if audio_path.is_empty() {
                warn!(session_id, "tts.synthesize.empty_path");
            } else {
                info!(session_id, audio_path, "tts.synthesized");
                body_json["audio_path"] = Value::String(audio_path);
            }
            state
                .metrics
                .record(&tts_metric(&session_id, "ok", started));
        }
        Err(e) => {
            // Best-effort: log and continue — caller still gets the text response.
            warn!(session_id, error = %e, "tts.synthesize.unavailable — passing text-only");
            state
                .metrics
                .record(&tts_metric(&session_id, "error", started));
        }
    }

    let modified = serde_json::to_vec(&body_json)
        .map(axum::body::Bytes::from)
        .unwrap_or(bytes);

    Response::from_parts(parts, Body::from(modified))
}
