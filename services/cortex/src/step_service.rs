//! Tower middleware stack for the Cortex step handler.
//!
//! Phase 1: TraceLayer + TimeoutLayer wrapping the inner ReActService.
//! Phase 2: WhitelistLayer, SttLayer, TtsLayer slot in here.
//! Phase 3 (Axum): `axum::serve` replaces the raw UDS accept loop; this stack
//!                 is unchanged — only the transport in front of it changes.
//!
//! # Stack (outermost → innermost)
//!
//! ```text
//! CortexTraceLayer        — opens a tracing span for the full react loop
//!   TimeoutLayer          — hard deadline on the entire step (LLM + tools combined)
//!     ReActService        — runs BaseAgent::run(task) → AgentExecutor::execute()
//! ```
//!
//! # Error contract
//!
//! All layers and the inner service use `anyhow::Error`.  `TimeoutLayer` from
//! tower wraps errors as `tower::BoxError`; `build_step_service` maps that back
//! to `anyhow::Error` with a human-readable timeout message before it reaches
//! `CortexTraceLayer`, so callers always see `anyhow::Error`.

use crate::agent::{CortexAgent, ReActOutput};
use anyhow::{anyhow, Result};
use autoagents_core::agent::task::Task;
use autoagents_core::agent::{AgentDeriveT, BaseAgent, DirectAgent};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::timeout::TimeoutLayer;
use tower::{Layer, Service, ServiceBuilder};
use tracing::Instrument;

// ── Request type ──────────────────────────────────────────────────────────────

/// Input carried through the Tower stack for one Cortex step.
///
/// Built synchronously in `handle_step` after config loading and agent
/// construction; the async ReAct loop runs inside the Tower stack.
pub struct StepRequest {
    /// Fully-constructed BaseAgent — owns the LLM provider, memory, and inner agent.
    /// `base_agent.run(task)` fires on_run_start → execute() → on_run_complete.
    pub base_agent: BaseAgent<CortexAgent, DirectAgent>,
    /// Trimmed user message for this turn.
    pub user_input: String,
}

/// `BaseAgent` does not implement `Debug`, so we provide a manual impl that
/// shows the user input without trying to format the agent internals.
impl fmt::Debug for StepRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StepRequest")
            .field("user_input", &self.user_input)
            .field("base_agent", &"<BaseAgent>")
            .finish()
    }
}

// ── Inner service — ReActService ──────────────────────────────────────────────

/// Innermost Tower service: runs the full ReAct loop and returns `ReActOutput`.
///
/// Stateless — a fresh `ReActService` can be constructed per-request at zero cost.
#[derive(Debug, Clone)]
pub struct ReActService;

impl Service<StepRequest> for ReActService {
    type Response = ReActOutput;
    type Error = anyhow::Error;
    type Future = Pin<Box<dyn Future<Output = Result<ReActOutput>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: StepRequest) -> Self::Future {
        Box::pin(async move {
            req.base_agent
                .run(Task::new(&req.user_input))
                .await
                .map_err(|e| anyhow!("{e}"))
        })
    }
}

// ── CortexTraceLayer ──────────────────────────────────────────────────────────

/// Tower layer that instruments each step with a `cortex.react_loop` tracing span.
///
/// The span nests inside the `cortex.step` span already opened in `handle_step`,
/// so OTEL traces show: `cortex.step → cortex.react_loop → (individual turns)`.
///
/// Phase 2+: additional layers (WhitelistLayer, SttLayer, TtsLayer) wrap outside
/// or inside this layer depending on whether they affect the react loop itself.
#[derive(Debug, Clone)]
pub struct CortexTraceLayer;

impl<S> Layer<S> for CortexTraceLayer {
    type Service = CortexTraceService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        CortexTraceService { inner }
    }
}

#[derive(Debug, Clone)]
pub struct CortexTraceService<S> {
    inner: S,
}

impl<S, E> Service<StepRequest> for CortexTraceService<S>
where
    S: Service<StepRequest, Response = ReActOutput, Error = E>,
    S::Future: 'static,
    E: 'static,
{
    type Response = ReActOutput;
    type Error = E;
    type Future = Pin<Box<dyn Future<Output = Result<ReActOutput, E>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: StepRequest) -> Self::Future {
        let agent_name = req.base_agent.inner().name().to_string();
        let span = tracing::info_span!(
            "cortex.react_loop",
            agent = %agent_name,
        );
        Box::pin(self.inner.call(req).instrument(span))
    }
}

// ── Stack constructor ─────────────────────────────────────────────────────────

/// Build the Tower step service stack.
///
/// Stack: `CortexTraceLayer → map_err(timeout) → TimeoutLayer → ReActService`
///
/// `TimeoutLayer` converts errors to `tower::BoxError`.  The `map_err` adaptor
/// sits between the timeout and the trace layer so that `CortexTraceLayer` always
/// sees `anyhow::Error` — callers never deal with `BoxError`.
///
/// Typical timeout: 90 seconds (`DEFAULT_STEP_TIMEOUT_SECS`).
pub fn build_step_service(
    timeout: Duration,
) -> impl Service<StepRequest, Response = ReActOutput, Error = anyhow::Error> {
    ServiceBuilder::new()
        .layer(CortexTraceLayer)
        .map_err(move |e: tower::BoxError| {
            if e.is::<tower::timeout::error::Elapsed>() {
                anyhow!("step timed out after {}s — react loop exceeded deadline", timeout.as_secs())
            } else {
                anyhow!("{e}")
            }
        })
        .layer(TimeoutLayer::new(timeout))
        .service(ReActService)
}

/// Default per-step deadline.
///
/// Covers all LLM turns and tool calls in the ReAct loop combined.
/// Individual LLM calls and tool calls have their own shorter deadlines
/// (`ProviderConfig.timeout` and `TOOL_CALL_TIMEOUT` respectively).
/// This is the outer safety net for runaway loops.
pub const DEFAULT_STEP_TIMEOUT_SECS: u64 = 90;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_is_reasonable() {
        assert!(DEFAULT_STEP_TIMEOUT_SECS >= 30, "step timeout must allow at least one LLM call");
        assert!(DEFAULT_STEP_TIMEOUT_SECS <= 300, "step timeout above 5 min is probably misconfigured");
    }
}
