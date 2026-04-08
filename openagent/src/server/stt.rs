/// SttLayer — Axum middleware that transcribes audio before the step handler.
///
/// If the request body contains an `audio_path` field the middleware:
///   1. Calls `stt.transcribe` via ServiceManager (60 s timeout — Whisper on Pi is slow).
///   2. Injects the transcript as `user_input` in the body.
///   3. Removes `audio_path` (and `language` if present) so the handler never sees them.
///
/// If `audio_path` is absent the request passes through unchanged — most requests
/// are plain text and never touch this middleware.
///
/// Error policy (unlike GuardLayer, STT failures are not recoverable):
///   - STT service unavailable → HTTP 503
///   - Empty transcript returned → HTTP 422
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::time::Instant;
use tracing::{info, warn};

use crate::observability::metrics::stt_metric;
use super::state::AppState;

pub async fn stt_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !state.config.stt.enabled {
        return next.run(req).await;
    }

    let (parts, body) = req.into_parts();

    let bytes = match axum::body::to_bytes(body, 4 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "stt.body.read.error");
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "body_read_error"})),
            )
                .into_response();
        }
    };

    // Fast-path: no JSON body or no audio_path — pass through without buffering cost.
    let Ok(mut body_json) = serde_json::from_slice::<Value>(&bytes) else {
        let req = Request::from_parts(parts, Body::from(bytes));
        return next.run(req).await;
    };

    let audio_path = match body_json.get("audio_path").and_then(Value::as_str) {
        Some(p) => p.to_string(),
        None => {
            let req = Request::from_parts(parts, Body::from(bytes));
            return next.run(req).await;
        }
    };

    let started = Instant::now();
    let language = body_json
        .get("language")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut params = json!({"audio_path": audio_path});
    if let Some(lang) = &language {
        params["language"] = Value::String(lang.clone());
    }

    match state.manager.call_tool("stt.transcribe", params, 60_000).await {
        Ok(payload) => {
            let v: Value = serde_json::from_str(&payload).unwrap_or_default();
            let transcript = v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();

            if transcript.is_empty() {
                warn!(audio_path, "stt.transcript.empty");
                state
                    .metrics
                    .record(&stt_metric(&audio_path, "empty", started));
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    axum::Json(json!({
                        "error": "stt_empty_transcript",
                        "audio_path": audio_path,
                    })),
                )
                    .into_response();
            }

            info!(
                audio_path,
                transcript_len = transcript.len(),
                "stt.transcribed"
            );
            state
                .metrics
                .record(&stt_metric(&audio_path, "ok", started));

            // Inject transcript; strip STT-only fields before the handler sees them.
            body_json["user_input"] = Value::String(transcript);
            if let Some(obj) = body_json.as_object_mut() {
                obj.remove("audio_path");
                obj.remove("language");
            }

            let modified = serde_json::to_vec(&body_json)
                .map(axum::body::Bytes::from)
                .unwrap_or(bytes);

            let req = Request::from_parts(parts, Body::from(modified));
            next.run(req).await
        }
        Err(e) => {
            warn!(audio_path, error = %e, "stt.transcribe.unavailable");
            state
                .metrics
                .record(&stt_metric(&audio_path, "error", started));
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({
                    "error": "stt_unavailable",
                    "detail": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}
