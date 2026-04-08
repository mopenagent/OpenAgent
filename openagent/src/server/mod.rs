pub mod middleware;
pub mod routes;
pub mod state;
pub mod stt;
pub mod tts;

/// Axum HTTP control plane — TCP port 8080.
///
/// Tower middleware stack (outermost → innermost):
///   ConcurrencyLimitLayer → HandleErrorLayer → TimeoutLayer → TraceLayer → GuardLayer → SttLayer → AgentLayer → TtsLayer → Router
///
/// Routes:
///   GET  /health        liveness + tool count
///   GET  /tools         all registered tools
///   POST /step          run one in-process agent reasoning step (guard-checked)
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
use tower::limit::ConcurrencyLimitLayer;
use tower::timeout::TimeoutLayer;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::agent::handlers::AgentContext;
use crate::channels::ChannelHandle;
use crate::config::MiddlewareConfig;
use crate::guard::GuardDb;
use crate::service::ServiceManager;
use crate::observability::telemetry::MetricsWriter;
use self::middleware::{agent_middleware, guard_middleware};
use self::state::AppState;
use self::stt::stt_middleware;
use self::tts::tts_middleware;

const DEFAULT_PORT: u16 = 8080;

/// Per-request deadline covering the full Cortex ReAct loop + tool calls.
/// Individual LLM calls and MCP-lite tool calls have their own shorter deadlines;
/// this is the outer safety net that prevents zombie HTTP connections.
const STEP_TIMEOUT_SECS: u64 = 130;

/// Build the Axum router with the Tower middleware stack applied.
///
/// Stack (outermost → innermost):
///   HandleErrorLayer(→ 408/503) → ConcurrencyLimitLayer → TimeoutLayer(130s)
///     → TraceLayer → CorsLayer → GuardLayer → SttLayer → AgentLayer → TtsLayer → Router
///
/// AgentLayer runs the in-process ReAct loop for POST /step; all other routes pass through.
/// SttLayer and TtsLayer are always registered; they no-op when disabled in config.
pub fn build_router(
    manager:        Arc<ServiceManager>,
    metrics:        MetricsWriter,
    config:         MiddlewareConfig,
    guard_db:       GuardDb,
    agent_ctx:      Arc<AgentContext>,
    channel_handle: ChannelHandle,
) -> Router {
    let max_concurrent = config.rate_limit.max_concurrent;
    let state = AppState::new(manager, metrics, config, guard_db, agent_ctx, channel_handle);

    Router::new()
        .route("/health", get(routes::health))
        .route("/tools", get(routes::list_tools))
        .route("/step", post(routes::step))
        .route("/tool/:name", post(routes::call_tool))
        // WhatsApp Cloud API webhook — GET verifies challenge, POST receives messages.
        .route("/webhook/whatsapp", get(routes::whatsapp_webhook_verify))
        .route("/webhook/whatsapp", post(routes::whatsapp_webhook_receive))
        // TtsLayer — innermost: synthesizes response_text → audio after the handler.
        // No-ops when tts.enabled = false in config/openagent.toml.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            tts_middleware,
        ))
        // AgentLayer — runs the in-process ReAct loop for POST /step.
        // Stores AgentResult in request extensions; route handler returns it as JSON.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            agent_middleware,
        ))
        // SttLayer — transcribes audio_path → user_input before AgentLayer.
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
        // HandleErrorLayer + TimeoutLayer: catches Elapsed → 408, other errors → 503.
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|e: BoxError| async move {
                    if e.is::<tower::timeout::error::Elapsed>() {
                        StatusCode::REQUEST_TIMEOUT
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }))
                .layer(TimeoutLayer::new(Duration::from_secs(STEP_TIMEOUT_SECS))),
        )
        // ConcurrencyLimitLayer (outermost) — caps simultaneous in-flight requests.
        // Excess requests are backpressured; the inner TimeoutLayer fires after 130s
        // so flooded requests self-terminate without exhausting server resources.
        .layer(ConcurrencyLimitLayer::new(max_concurrent))
        .with_state(state)
}

/// Start the Axum server on `0.0.0.0:{port}`.
pub async fn start(
    manager:        Arc<ServiceManager>,
    metrics:        MetricsWriter,
    config:         MiddlewareConfig,
    guard_db:       GuardDb,
    agent_ctx:      Arc<AgentContext>,
    channel_handle: ChannelHandle,
    port:           u16,
) -> Result<()> {
    let app = build_router(manager, metrics, config, guard_db, agent_ctx, channel_handle);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(addr = %addr, "openagent.server.start");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Start with the default port (8080), or `OPENAGENT_PORT` env var override.
pub async fn start_default(
    manager:        Arc<ServiceManager>,
    metrics:        MetricsWriter,
    config:         MiddlewareConfig,
    guard_db:       GuardDb,
    agent_ctx:      Arc<AgentContext>,
    channel_handle: ChannelHandle,
) -> Result<()> {
    let port = std::env::var("OPENAGENT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    start(manager, metrics, config, guard_db, agent_ctx, channel_handle, port).await
}
