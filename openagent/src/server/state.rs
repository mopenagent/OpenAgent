use crate::agent::handlers::AgentContext;
use crate::config::MiddlewareConfig;
use crate::guard::GuardDb;
use crate::service::ServiceManager;
use crate::observability::telemetry::MetricsWriter;
use std::sync::Arc;
use std::time::Instant;

/// Shared application state injected into every Axum route and middleware.
#[derive(Clone, Debug)]
pub struct AppState {
    pub manager:    Arc<ServiceManager>,
    pub metrics:    MetricsWriter,
    pub config:     MiddlewareConfig,
    /// Inline guard whitelist — direct SQLite, no network hop.
    pub guard_db:   GuardDb,
    /// Process start time — used to compute uptime in /health.
    pub started_at: Arc<Instant>,
    /// In-process agent context — AgentLayer and dispatch loop call handle_step directly.
    pub agent_ctx:  Arc<AgentContext>,
}

impl AppState {
    pub fn new(
        manager:   Arc<ServiceManager>,
        metrics:   MetricsWriter,
        config:    MiddlewareConfig,
        guard_db:  GuardDb,
        agent_ctx: Arc<AgentContext>,
    ) -> Self {
        Self {
            manager,
            metrics,
            config,
            guard_db,
            started_at: Arc::new(Instant::now()),
            agent_ctx,
        }
    }
}
