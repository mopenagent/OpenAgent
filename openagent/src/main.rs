use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

pub mod agent;
mod config;
mod console;
mod dispatch;
mod guard;
mod observability;
mod platform;
mod server;
mod service;

// Merged from ZeroClaw
pub mod hardware;
pub mod peripherals;
pub mod channels;
pub mod tunnel;
pub mod gateway;
pub mod cron;
pub mod sop;
pub mod doctor;
pub mod health;

// Stubs for zeroclaw channel deps (security pairing, provider trait, multimodal)
pub mod security;
pub mod providers;
pub mod multimodal;
use agent::action::catalog::ActionCatalog;
use agent::handlers::AgentContext;
use agent::metrics::AgentTelemetry;
use agent::tool_router::ToolRouter;
use anyhow::Result;
use service::ServiceManager;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use observability::telemetry::MetricsWriter;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // ---- OTEL (traces + logs + metrics) ------------------------------------
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    let _otel_guard = observability::otel::setup_otel("openagent", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let metrics = MetricsWriter::new(&logs_dir, "openagent")
        .unwrap_or_else(|e| {
            eprintln!("metrics writer init failed: {e}");
            panic!("cannot open metrics log dir");
        });

    // ---- project root -------------------------------------------------------
    let project_root = env::var("OPENAGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().expect("cannot determine current directory"));

    info!(
        root = %project_root.display(),
        platform = platform::host_platform_key(),
        "openagent.start"
    );

    // ---- discover + start services ------------------------------------------
    let services_dir = project_root.join("services");
    let manifests = service::manifest::discover(&services_dir, &project_root)?;

    info!(count = manifests.len(), "openagent.manifests.loaded");
    for m in &manifests {
        info!(
            service = %m.name,
            version = m.version.as_deref().unwrap_or("?"),
            "openagent.manifest"
        );
    }

    // ---- load config --------------------------------------------------------
    let cfg = config::load(&project_root).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "config.load.error — using defaults");
        config::OpenAgentConfig::default()
    });
    info!(
        provider_kind     = %cfg.provider.kind,
        provider_model    = %cfg.provider.model,
        guard_enabled     = cfg.guard.enabled,
        stt_enabled       = cfg.middleware.stt.enabled,
        tts_enabled       = cfg.middleware.tts.enabled,
        services_disabled = ?cfg.services.disabled,
        "openagent.config.loaded"
    );

    // ---- Open guard database ------------------------------------------------
    // Guard is now inline — no separate service process.
    // The same data/guard.db file is shared read-write with the FastAPI app
    // for management operations (list/allow/block).  WAL mode allows concurrent
    // readers and one writer without blocking.
    let guard_db_path = project_root.join(&cfg.guard.db_path);
    let guard_db_path_str = guard_db_path.to_string_lossy().to_string();
    let guard_db = guard::GuardDb::open(&guard_db_path_str, cfg.guard.enabled)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, path = %guard_db_path_str, "guard.db.open.error — guard disabled");
            // Construct a disabled stub so the rest of startup is unaffected.
            guard::GuardDb::open(":memory:", false).expect("in-memory guard db always works")
        });
    info!(
        path = %guard_db_path_str,
        enabled = cfg.guard.enabled,
        "guard.db.opened"
    );

    // Connect to externally-managed services (systemd / services.sh).
    // openagent does not spawn or restart services — that is the supervisor's job.
    let manager = Arc::new(ServiceManager::new());
    manager.start_all(manifests, &cfg.services.disabled).await;

    // ---- In-process channels (replaces services/channels/ daemon) ---------------
    // Listeners push message.received events onto the same broadcast bus used by
    // the ServiceManager, so dispatch.rs receives them transparently.
    let channel_handle = channels::init(
        &project_root,
        metrics.clone(),
        manager.event_sender(),
    ).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "channels.init.error — channel operations will fail");
        // Return a no-op handle backed by an empty registry.
        channels::ChannelHandle::disabled()
    });

    // ---- Build in-process AgentContext (action catalog + tool router + telemetry) ----
    let action_catalog = Arc::new(ActionCatalog::load(&project_root.join("services")));
    let tool_addresses = action_catalog.tool_address_map();
    let tool_router = Arc::new(ToolRouter::new(tool_addresses, project_root.clone()));
    let agent_tel = Arc::new(
        AgentTelemetry::new(&logs_dir).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "agent telemetry init failed — using no-op");
            AgentTelemetry::new("/dev/null").expect("fallback agent telemetry failed")
        }),
    );
    let agent_ctx = Arc::new(AgentContext::new(agent_tel, action_catalog, tool_router, project_root.clone()));

    info!(
        tool_count = agent_ctx.action_catalog().entries().len(),
        "agent.context.ready"
    );

    // ---- Dispatch loop — events → agent → channel.send --------------------
    {
        let dispatch_manager = Arc::clone(&manager);
        let dispatch_guard   = guard_db.clone();
        let dispatch_ctx     = Arc::clone(&agent_ctx);
        let dispatch_ch      = channel_handle.clone();
        tokio::spawn(async move {
            dispatch::run(dispatch_manager, dispatch_guard, dispatch_ctx, dispatch_ch).await;
        });
    }

    // ---- Axum control plane (TCP :8080) -------------------------------------
    // Spawn server in background — shutdown is handled by Ctrl-C below.
    let server_manager = Arc::clone(&manager);
    let server_metrics = metrics.clone();
    let server_cfg     = cfg.middleware.clone();
    let server_guard   = guard_db.clone();
    let server_ctx     = Arc::clone(&agent_ctx);
    let server_ch      = channel_handle.clone();
    tokio::spawn(async move {
        if let Err(e) = server::start_default(server_manager, server_metrics, server_cfg, server_guard, server_ctx, server_ch).await {
            tracing::error!(error = %e, "openagent.server.error");
        }
    });

    // ---- Interactive console -------------------------------------------------
    // Spawns a blocking thread that reads stdin.  The returned Notify fires
    // ONLY when the user types `quit`/`shutdown`.  stdin EOF (no TTY / daemon)
    // exits the console loop silently and the Notify is never triggered —
    // the process keeps running until a signal arrives.
    let logs_path    = PathBuf::from(&logs_dir);
    let quit_notify  = console::run(Arc::clone(&manager), guard_db.clone(), logs_path).await;

    // ---- SIGTERM / Ctrl-C shutdown ------------------------------------------
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c()  => { info!("openagent.sigint"); }
            _ = sigterm.recv()           => { info!("openagent.sigterm"); }
            _ = quit_notify.notified()   => { info!("openagent.console.quit"); }
        }
    }
    #[cfg(not(unix))]
    tokio::select! {
        _ = tokio::signal::ctrl_c()  => { info!("openagent.sigint"); }
        _ = quit_notify.notified()   => { info!("openagent.console.quit"); }
    }

    info!("openagent.shutdown");
    manager.stop_all().await;

    Ok(())
}
