pub mod manifest;
pub mod mcplite;

/// ServiceManager — connects to and health-monitors MCP-lite service daemons.
///
/// openagent does NOT spawn or restart services. That is the job of systemd
/// (production) or services.sh (dev). This manager:
///   - Connects to each service over TCP at the address from service.json
///   - Calls tools.list and registers discovered tools
///   - Runs a ping health loop; reconnects automatically if a service restarts
///   - Forwards service events to the dispatch loop
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use self::manifest::ServiceManifest;
use self::mcplite::McpLiteClient;

/// A registered tool discovered via `tools.list`.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub service: String,
    pub definition: Value,
}

/// Live state of a connected service.
#[derive(Debug)]
struct ServiceState {
    name: String,
    client: Arc<McpLiteClient>,
}

/// Public snapshot of a connected service.
#[derive(Debug, Clone)]
pub struct LiveServiceInfo {
    pub name: String,
    pub address: String,
}

/// ServiceManager — connects to externally-managed service daemons.
#[derive(Debug)]
pub struct ServiceManager {
    /// All tools registered from all connected services.
    tools: Arc<RwLock<Vec<RegisteredTool>>>,
    /// Service name → live connection state.
    services: Arc<RwLock<HashMap<String, ServiceState>>>,
    /// Broadcast channel for events arriving from any service.
    event_tx: broadcast::Sender<Value>,
    /// JoinHandles for all connection-loop tasks.
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl ServiceManager {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            tools: Arc::new(RwLock::new(Vec::new())),
            services: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            task_handles: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe to events pushed by any connected service.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.event_tx.subscribe()
    }

    /// Return a snapshot of all currently connected services.
    pub async fn live_services(&self) -> Vec<LiveServiceInfo> {
        self.services
            .read()
            .await
            .values()
            .map(|s| LiveServiceInfo { name: s.name.clone(), address: String::new() })
            .collect()
    }

    /// Return all registered tools.
    pub async fn tools(&self) -> Vec<RegisteredTool> {
        self.tools.read().await.clone()
    }

    /// Call a tool on the appropriate service.
    pub async fn call_tool(&self, tool: &str, params: Value, timeout_ms: u64) -> Result<String> {
        let prefix = tool
            .split('.')
            .next()
            .ok_or_else(|| anyhow!("invalid tool name: {tool}"))?;

        let client = {
            let guard = self.services.read().await;
            guard
                .get(prefix)
                .map(|s| Arc::clone(&s.client))
                .ok_or_else(|| anyhow!("service not connected: {prefix}"))?
        };

        client.call_tool(tool, params, timeout_ms).await
    }

    /// Spawn a connection-loop task for each enabled manifest.
    ///
    /// Each loop connects to the service TCP address, registers tools, and
    /// re-connects automatically when the connection drops (the service
    /// restarted). This is intentionally lightweight — openagent never kills
    /// or restarts service processes.
    pub async fn start_all(&self, manifests: Vec<ServiceManifest>, config_disabled: &[String]) {
        for manifest in manifests {
            if !manifest.enabled {
                info!(service = %manifest.name, "service.disabled — skipping");
                continue;
            }
            if config_disabled.iter().any(|d| d == &manifest.name) {
                info!(service = %manifest.name, "service.disabled — skipping (config)");
                continue;
            }

            let services = Arc::clone(&self.services);
            let tools = Arc::clone(&self.tools);
            let event_tx = self.event_tx.clone();

            let handle = tokio::spawn(async move {
                connection_loop(manifest, services, tools, event_tx).await;
            });

            self.task_handles
                .lock()
                .expect("task_handles poisoned")
                .push(handle);
        }
    }

    /// Abort all connection-loop tasks and clear state.
    pub async fn stop_all(&self) {
        let handles: Vec<JoinHandle<()>> = self
            .task_handles
            .lock()
            .expect("task_handles poisoned")
            .drain(..)
            .collect();

        for h in &handles {
            h.abort();
        }
        for h in handles {
            let _ = h.await;
        }

        self.services.write().await.clear();
        info!("service_manager.disconnected_all");
    }
}

/// Long-running loop that keeps one service connection alive.
///
/// Waits for the service to become reachable, connects, registers tools,
/// runs a health loop, and reconnects when the connection drops.
async fn connection_loop(
    manifest: ServiceManifest,
    services: Arc<RwLock<HashMap<String, ServiceState>>>,
    tools: Arc<RwLock<Vec<RegisteredTool>>>,
    event_tx: broadcast::Sender<Value>,
) {
    let name = manifest.name.clone();
    let addr = manifest.connect_addr();
    let health_interval = Duration::from_millis(manifest.health.interval_ms);
    let timeout_ms = manifest.health.timeout_ms;

    loop {
        // ---- wait for TCP port to accept connections ------------------------
        info!(service = %name, addr = %addr, "service.connecting");
        if let Err(e) = wait_for_tcp(&addr, manifest.health.startup_timeout_ms).await {
            warn!(service = %name, addr = %addr, error = %e, "service.unreachable — retrying in 5s");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        // ---- connect --------------------------------------------------------
        let client = match McpLiteClient::connect(&addr).await {
            Ok(c) => Arc::new(c),
            Err(e) => {
                error!(service = %name, addr = %addr, error = %e, "service.connect.failed");
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        // ---- tools.list -----------------------------------------------------
        match client.tools_list(timeout_ms * 5).await {
            Ok(tool_defs) => {
                let mut t = tools.write().await;
                t.retain(|rt| rt.service != name);
                for def in &tool_defs {
                    t.push(RegisteredTool {
                        service: name.clone(),
                        definition: def.clone(),
                    });
                }
                info!(service = %name, addr = %addr, count = tool_defs.len(), "service.connected");
            }
            Err(e) => {
                warn!(service = %name, error = %e, "service.tools_list.failed");
            }
        }

        // ---- forward events -------------------------------------------------
        {
            let mut event_rx = client.subscribe_events();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                while let Ok(evt) = event_rx.recv().await {
                    let _ = tx.send(evt);
                }
            });
        }

        // ---- register live state --------------------------------------------
        services.write().await.insert(
            name.clone(),
            ServiceState { name: name.clone(), client: Arc::clone(&client) },
        );

        // ---- health loop ----------------------------------------------------
        loop {
            sleep(health_interval).await;

            if !client.ping(timeout_ms).await {
                error!(service = %name, addr = %addr, "service.ping.timeout — reconnecting");
                break;
            }
            debug!(service = %name, "service.health.ok");
        }

        // Remove from live state; tools remain stale until we reconnect.
        services.write().await.remove(&name);
        tools.write().await.retain(|rt| rt.service != name);

        info!(service = %name, addr = %addr, "service.disconnected — reconnecting in 2s");
        sleep(Duration::from_secs(2)).await;
    }
}

/// Poll `addr` every 200ms until TCP accepts a connection or timeout expires.
async fn wait_for_tcp(addr: &str, timeout_ms: u64) -> Result<()> {
    let deadline = Duration::from_millis(timeout_ms);
    let start = tokio::time::Instant::now();
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(anyhow!("TCP {addr} not reachable within {timeout_ms}ms"));
        }
        sleep(Duration::from_millis(200)).await;
    }
}
