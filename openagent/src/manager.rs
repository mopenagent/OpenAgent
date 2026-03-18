/// ServiceManager — spawns, monitors, and restarts MCP-lite service daemons.
///
/// Each managed service runs as a child process. A health loop sends `ping`
/// every `health.interval_ms`; if no `pong` arrives within `health.timeout_ms`,
/// the process is killed and restarted with exponential backoff.
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use crate::manifest::ServiceManifest;
use crate::mcplite::McpLiteClient;
use crate::platform::host_platform_key;

/// A registered tool discovered via `tools.list`.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    pub service: String,
    pub definition: Value,
}

/// Live state of a running service.
#[derive(Debug)]
struct ServiceState {
    name: String,
    client: Arc<McpLiteClient>,
    pid: Option<u32>,
}

/// Public snapshot of a running service — returned by `live_services()`.
#[derive(Debug, Clone)]
pub struct LiveServiceInfo {
    pub name: String,
    pub pid: Option<u32>,
}

/// ServiceManager — the central orchestrator.
#[derive(Debug)]
pub struct ServiceManager {
    project_root: PathBuf,
    /// Per-service env var overrides injected from config (e.g. tokens, DB paths).
    /// Key: service name (e.g. "cortex"), Value: env var map.
    service_env: HashMap<String, HashMap<String, String>>,
    /// Service name → live state (None if not yet started or crashed).
    services: Arc<RwLock<HashMap<String, ServiceState>>>,
    /// All tools registered from all services.
    tools: Arc<RwLock<Vec<RegisteredTool>>>,
    /// Broadcast channel for events arriving from any service.
    event_tx: broadcast::Sender<Value>,
    /// JoinHandles for all spawned service loop tasks — used by stop_all() to abort them.
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl ServiceManager {
    pub fn new(
        project_root: PathBuf,
        service_env: HashMap<String, HashMap<String, String>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            project_root,
            service_env,
            services: Arc::new(RwLock::new(HashMap::new())),
            tools: Arc::new(RwLock::new(Vec::new())),
            event_tx,
            task_handles: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe to events pushed by any managed service.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.event_tx.subscribe()
    }

    /// Return a snapshot of all registered tools.
    /// Snapshot of all currently running services (name + PID).
    pub async fn live_services(&self) -> Vec<LiveServiceInfo> {
        self.services
            .read()
            .await
            .values()
            .map(|s| LiveServiceInfo { name: s.name.clone(), pid: s.pid })
            .collect()
    }

    pub async fn tools(&self) -> Vec<RegisteredTool> {
        self.tools.read().await.clone()
    }

    /// Call a tool on the appropriate service.
    pub async fn call_tool(&self, tool: &str, params: Value, timeout_ms: u64) -> Result<String> {
        let service_name = tool
            .split('.')
            .next()
            .ok_or_else(|| anyhow!("invalid tool name: {tool}"))?;

        let state = {
            let guard = self.services.read().await;
            guard
                .get(service_name)
                .map(|s| Arc::clone(&s.client))
                .ok_or_else(|| anyhow!("service not running: {service_name}"))?
        };

        state.call_tool(tool, params, timeout_ms).await
    }

    /// Start all services described by `manifests`, skipping any that are disabled.
    ///
    /// A service is skipped if:
    /// - `manifest.enabled == false` (set in `service.json`)
    /// - its name appears in `config_disabled` (set in `openagent.toml [services] disabled`)
    pub async fn start_all(&self, manifests: Vec<ServiceManifest>, config_disabled: &[String]) {
        for manifest in manifests {
            if !manifest.enabled {
                info!(service = %manifest.name, "service.disabled — skipping (service.json enabled=false)");
                continue;
            }
            if config_disabled.iter().any(|d| d == &manifest.name) {
                info!(service = %manifest.name, "service.disabled — skipping (openagent.toml services.disabled)");
                continue;
            }
            let root = self.project_root.clone();
            let services = Arc::clone(&self.services);
            let tools = Arc::clone(&self.tools);
            let event_tx = self.event_tx.clone();
            let extra_env = self
                .service_env
                .get(&manifest.name)
                .cloned()
                .unwrap_or_default();
            let handle = tokio::spawn(async move {
                run_service_loop(manifest, root, extra_env, services, tools, event_tx).await;
            });
            self.task_handles
                .lock()
                .expect("task_handles mutex poisoned")
                .push(handle);
        }
    }

    /// Stop all services: abort service loop tasks (which triggers kill_on_drop on each
    /// child process), then clear live state.
    pub async fn stop_all(&self) {
        let handles: Vec<JoinHandle<()>> = self
            .task_handles
            .lock()
            .expect("task_handles mutex poisoned")
            .drain(..)
            .collect();

        // Abort every service loop task. Aborting drops the task's Future, which drops
        // `child` (kill_on_drop = true), sending SIGKILL to each child process.
        for h in &handles {
            h.abort();
        }
        // Wait for all tasks to finish dropping so kills are delivered before we return.
        for h in handles {
            let _ = h.await; // Returns JoinError::Cancelled — expected, ignore it.
        }

        self.services.write().await.clear();
        info!("service_manager.stopped_all");
    }
}

/// Long-running loop that keeps one service alive: spawn → connect → health → restart.
async fn run_service_loop(
    manifest: ServiceManifest,
    project_root: PathBuf,
    config_env: HashMap<String, String>,
    services: Arc<RwLock<HashMap<String, ServiceState>>>,
    tools: Arc<RwLock<Vec<RegisteredTool>>>,
    event_tx: broadcast::Sender<Value>,
) {
    let name = manifest.name.clone();
    let platform = host_platform_key();
    let backoff = &manifest.health.restart_backoff_ms;
    let mut attempt: usize = 0;

    loop {
        // ---- resolve binary -------------------------------------------------
        let binary = match manifest.binary_path(platform) {
            Some(p) => p,
            None => {
                error!(service = %name, platform, "service.binary.not_found — skipping");
                return;
            }
        };

        if !binary.exists() {
            warn!(
                service = %name,
                path = %binary.display(),
                "service.binary.missing — run `make local` first; retrying in 10s"
            );
            sleep(Duration::from_secs(10)).await;
            continue;
        }

        let socket_path = manifest.socket_path();

        // Ensure the socket directory exists.
        if let Some(dir) = socket_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Remove stale socket file if present.
        let _ = std::fs::remove_file(&socket_path);

        // ---- spawn child ----------------------------------------------------
        // Merge: manifest env (service.json) ← config env (openagent.toml).
        // Config env wins so operator overrides take effect.
        let mut env_extras: Vec<(String, String)> = manifest
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (k, v) in &config_env {
            if let Some(entry) = env_extras.iter_mut().find(|(ek, _)| ek == k) {
                entry.1 = v.clone();
            } else {
                env_extras.push((k.clone(), v.clone()));
            }
        }

        // Resolve relative data/ paths against project_root.
        for (_, val) in &mut env_extras {
            if val.starts_with("data/") {
                *val = project_root.join(&*val).to_string_lossy().to_string();
            }
        }

        let socket_str = socket_path.to_string_lossy().to_string();

        let child = Command::new(&binary)
            // stdin  → /dev/null: prevents child from racing on parent's stdin fd
            //          (would cause console read_line to get spurious EOF).
            // stdout → /dev/null: service logs go to their own OTEL files;
            //          suppress here so they don't flood the interactive console.
            // stderr → /dev/null: same reason — services write structured logs
            //          to files, not to the terminal.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Put each child in its own process group so terminal Ctrl-C (SIGINT
            // to the foreground group) only hits openagent.  openagent then calls
            // stop_all() which aborts the task and kill_on_drop delivers SIGKILL.
            // Without this, children receive SIGINT at the same instant as the
            // parent, causing a restart race before stop_all() can run.
            .process_group(0)
            .env("OPENAGENT_SOCKET_PATH", &socket_str)
            .env("OPENAGENT_LOGS_DIR", project_root.join("logs").to_string_lossy().as_ref())
            .envs(env_extras)
            .kill_on_drop(true)
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                error!(service = %name, error = %e, "service.spawn.failed");
                backoff_sleep(backoff, &mut attempt).await;
                continue;
            }
        };

        info!(service = %name, pid = ?child.id(), binary = %binary.display(), "service.spawned");

        // ---- wait for socket ------------------------------------------------
        if let Err(e) = wait_for_socket(&socket_path, manifest.health.startup_timeout_ms).await {
            error!(service = %name, error = %e, "service.socket.timeout");
            let _ = child.kill().await;
            backoff_sleep(backoff, &mut attempt).await;
            continue;
        }

        // ---- connect --------------------------------------------------------
        let client = match McpLiteClient::connect(&socket_str).await {
            Ok(c) => Arc::new(c),
            Err(e) => {
                error!(service = %name, error = %e, "service.connect.failed");
                let _ = child.kill().await;
                backoff_sleep(backoff, &mut attempt).await;
                continue;
            }
        };

        // ---- tools.list -----------------------------------------------------
        match client.tools_list(manifest.health.timeout_ms * 5).await {
            Ok(tool_defs) => {
                let mut t = tools.write().await;
                // Remove any stale entries from a previous run of this service.
                t.retain(|rt| rt.service != name);
                for def in &tool_defs {
                    t.push(RegisteredTool {
                        service: name.clone(),
                        definition: def.clone(),
                    });
                }
                info!(service = %name, count = tool_defs.len(), "service.tools.registered");
            }
            Err(e) => {
                warn!(service = %name, error = %e, "service.tools_list.failed");
            }
        }

        // ---- forward events -------------------------------------------------
        {
            let mut event_rx = client.subscribe_events();
            let event_tx2 = event_tx.clone();
            tokio::spawn(async move {
                while let Ok(evt) = event_rx.recv().await {
                    let _ = event_tx2.send(evt);
                }
            });
        }

        // ---- register live state --------------------------------------------
        let child_pid = child.id();
        services.write().await.insert(
            name.clone(),
            ServiceState {
                name: name.clone(),
                client: Arc::clone(&client),
                pid: child_pid,
            },
        );
        attempt = 0; // reset backoff on successful start

        // ---- health loop ----------------------------------------------------
        let interval = Duration::from_millis(manifest.health.interval_ms);
        let timeout_ms = manifest.health.timeout_ms;

        loop {
            sleep(interval).await;

            // Check if child process has exited.
            match child.try_wait() {
                Ok(Some(status)) => {
                    error!(service = %name, ?status, "service.exited");
                    break;
                }
                Err(e) => {
                    error!(service = %name, error = %e, "service.wait.error");
                    break;
                }
                Ok(None) => {} // still running
            }

            if !client.ping(timeout_ms).await {
                error!(service = %name, "service.ping.timeout — restarting");
                let _ = child.kill().await;
                break;
            }

            debug!(service = %name, "service.health.ok");
        }

        // Remove from live state before restarting.
        services.write().await.remove(&name);
        // Remove tools so they're not callable while service is down.
        tools.write().await.retain(|rt| rt.service != name);

        backoff_sleep(backoff, &mut attempt).await;
    }
}

/// Wait up to `timeout_ms` for the socket file to appear.
async fn wait_for_socket(path: &std::path::Path, timeout_ms: u64) -> Result<()> {
    let deadline = Duration::from_millis(timeout_ms);
    let start = tokio::time::Instant::now();
    loop {
        if path.exists() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(anyhow!("socket {} did not appear within {timeout_ms}ms", path.display()));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Sleep for the next backoff duration; increments attempt (capped at last entry).
async fn backoff_sleep(backoff: &[u64], attempt: &mut usize) {
    let ms = backoff
        .get(*attempt)
        .or_else(|| backoff.last())
        .copied()
        .unwrap_or(5000);
    *attempt = (*attempt + 1).min(backoff.len().saturating_sub(1) + 1);
    sleep(Duration::from_millis(ms)).await;
}
