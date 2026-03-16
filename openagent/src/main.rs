use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod config;
mod console;
mod dispatch;
mod scrub;
mod manifest;
mod manager;
mod mcplite;
mod metrics;
mod middleware;
mod otel;
mod stt;
mod tts;
mod platform;
mod routes;
mod server;
mod state;
mod telemetry;

use anyhow::Result;
use manager::ServiceManager;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use telemetry::MetricsWriter;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    // ---- OTEL (traces + logs + metrics) ------------------------------------
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    let _otel_guard = otel::setup_otel("openagent", &logs_dir)
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
    let manifests = manifest::discover(&services_dir, &project_root)?;

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
        agent_count       = cfg.agents.len(),
        services_disabled = ?cfg.services.disabled,
        "openagent.config.loaded"
    );

    // Build per-service env var overrides from config.
    // ServiceManager injects these into each service's subprocess environment.
    let mut service_env: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    service_env.insert("cortex".into(), cfg.provider.to_env_vars());
    service_env.insert("guard".into(), cfg.guard.to_env_vars());
    if cfg.platforms.discord.enabled {
        service_env.insert("discord".into(), cfg.platforms.discord_env());
    }
    if cfg.platforms.telegram.enabled {
        service_env.insert("telegram".into(), cfg.platforms.telegram_env());
    }
    if cfg.platforms.slack.enabled {
        service_env.insert("slack".into(), cfg.platforms.slack_env());
    }
    if cfg.platforms.whatsapp.enabled {
        service_env.insert("whatsapp".into(), cfg.platforms.whatsapp_env());
    }

    let manager = Arc::new(ServiceManager::new(project_root, service_env));
    manager.start_all(manifests, &cfg.services.disabled).await;

    // ---- Dispatch loop — events → cortex → channel.send --------------------
    {
        let dispatch_manager = Arc::clone(&manager);
        tokio::spawn(async move {
            dispatch::run(dispatch_manager).await;
        });
    }

    // ---- Axum control plane (TCP :8000) -------------------------------------
    // Spawn server in background — shutdown is handled by Ctrl-C below.
    let server_manager = Arc::clone(&manager);
    let server_metrics = metrics.clone();
    let server_cfg = cfg.middleware.clone();
    tokio::spawn(async move {
        if let Err(e) = server::start_default(server_manager, server_metrics, server_cfg).await {
            tracing::error!(error = %e, "openagent.server.error");
        }
    });

    // ---- Interactive console -------------------------------------------------
    // Spawns a blocking thread that reads stdin.  The returned Notify fires
    // ONLY when the user types `quit`/`shutdown`.  stdin EOF (no TTY / daemon)
    // exits the console loop silently and the Notify is never triggered —
    // the process keeps running until a signal arrives.
    let logs_path = PathBuf::from(&logs_dir);
    let quit_notify = console::run(Arc::clone(&manager), logs_path).await;

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
