/// Axum HTTP control plane — TCP port 8080.
///
/// Tower middleware stack (outermost → innermost):
///   TimeoutLayer → HandleErrorLayer → TraceLayer → GuardLayer → SttLayer → Router
///
/// Routes:
///   GET  /health        liveness + tool count
///   GET  /tools         all registered tools
///   POST /step          run one Cortex reasoning step (guard-checked)
///   POST /tool/:name    raw tool call (internal / debug)
use anyhow::Result;
use axum::error_handling::HandleErrorLayer;
use axum::http::StatusCode;
use axum::middleware as axum_middleware;
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::timeout::TimeoutLayer;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::MiddlewareConfig;
use crate::manager::ServiceManager;
use crate::middleware::guard_middleware;
use crate::routes;
use crate::state::AppState;
use crate::stt::stt_middleware;
use crate::telemetry::MetricsWriter;
use crate::tts::tts_middleware;

const DEFAULT_PORT: u16 = 8080;

/// Per-request deadline covering the full Cortex ReAct loop + tool calls.
/// Individual LLM calls and MCP-lite tool calls have their own shorter deadlines;
/// this is the outer safety net that prevents zombie HTTP connections.
const STEP_TIMEOUT_SECS: u64 = 130;

/// Build the Axum router with the Tower middleware stack applied.
///
/// Stack (outermost → innermost):
///   TimeoutLayer(130s) → HandleErrorLayer(→ 408) → TraceLayer → GuardLayer → SttLayer → TtsLayer → Router
///
/// SttLayer and TtsLayer are always registered; they no-op immediately when
/// their `enabled` flag is false in `config/openagent.toml`.
pub fn build_router(
    manager: Arc<ServiceManager>,
    metrics: MetricsWriter,
    config: MiddlewareConfig,
) -> Router {
    let state = AppState::new(manager, metrics, config);

    Router::new()
        .route("/health", get(routes::health))
        .route("/tools", get(routes::list_tools))
        .route("/step", post(routes::step))
        .route("/tool/:name", post(routes::call_tool))
        // TtsLayer — innermost: synthesizes response_text → audio after the handler.
        // No-ops when tts.enabled = false in config/openagent.toml.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            tts_middleware,
        ))
        // SttLayer — transcribes audio_path → user_input before the handler.
        // No-ops when stt.enabled = false in config/openagent.toml.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            stt_middleware,
        ))
        // GuardLayer — runs before STT; no point transcribing a blocked request.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            guard_middleware,
        ))
        // TraceLayer — logs every request at INFO level with method, path, status.
        .layer(TraceLayer::new_for_http())
        // CorsLayer — allows the web UI (port 8000) to call this API (port 8080).
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        // TimeoutLayer + HandleErrorLayer composed via ServiceBuilder.
        // HandleErrorLayer must wrap TimeoutLayer so it can catch the BoxError
        // that TimeoutLayer produces when the deadline fires → returns HTTP 408.
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|e: BoxError| async move {
                    if e.is::<tower::timeout::error::Elapsed>() {
                        StatusCode::REQUEST_TIMEOUT
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                }))
                .layer(TimeoutLayer::new(Duration::from_secs(STEP_TIMEOUT_SECS))),
        )
        .with_state(state)
}

/// Start the Axum server on `0.0.0.0:{port}`.
pub async fn start(
    manager: Arc<ServiceManager>,
    metrics: MetricsWriter,
    config: MiddlewareConfig,
    port: u16,
) -> Result<()> {
    let app = build_router(manager, metrics, config);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(addr = %addr, "openagent.server.start");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Start with the default port (8000), or `OPENAGENT_PORT` env var override.
pub async fn start_default(
    manager: Arc<ServiceManager>,
    metrics: MetricsWriter,
    config: MiddlewareConfig,
) -> Result<()> {
    let port = std::env::var("OPENAGENT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    start(manager, metrics, config, port).await
}
