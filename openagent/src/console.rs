/// Interactive CLI console — runs in the terminal after openagent starts.
///
/// Commands:
///   help                              List commands
///   health                            Show service health
///   services                          List running services
///   service restart <name>            Restart a service (SIGTERM → auto-restart)
///   tools [<filter>]                  List registered tools
///   guard list                        List guard contacts
///   guard allow <platform> <id> [name]
///   guard block  <platform> <id> [note]
///   guard name   <platform> <id> <name>
///   guard remove <platform> <id>
///   tool <name> [<json>]              Call any tool with optional JSON params
///   logs [<service>]                  Stream logs; press Enter to stop
///   quit / shutdown                   Graceful shutdown
///
/// Shutdown behaviour:
/// - Ctrl-C (SIGINT) always triggers shutdown (OS signal).
/// - Typing `quit` / `shutdown` triggers shutdown via Notify.
/// - stdin EOF (daemon / no TTY) exits the console loop silently — the
///   process keeps running and waits for OS signals only.
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::{json, Value};
use tokio::sync::Notify;
use tracing::info;

use crate::manager::ServiceManager;

const HELP: &str = "\
OpenAgent console commands:
  health                              Control plane + service health
  services                            List running services
  service restart <name>              Restart a service
  tools [<filter>]                    List registered tools
  guard list                          List all guard contacts
  guard allow <platform> <id> [name]  Allow a contact
  guard block <platform> <id> [note]  Block a contact
  guard name  <platform> <id> <name>  Rename a contact
  guard remove <platform> <id>        Remove a contact
  tool <name> [<json>]                Call any tool directly
  logs [<service>]                    Stream logs (Enter to stop)
  quit / shutdown                     Shut down openagent
  help                                Show this help";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Spawn the interactive console.  Returns a `Notify` that fires only when
/// the user explicitly types `quit`/`shutdown`.  stdin EOF (daemon mode)
/// does NOT fire the notify — the process keeps running, waiting for signals.
pub async fn run(manager: Arc<ServiceManager>, logs_dir: PathBuf) -> Arc<Notify> {
    let quit = Arc::new(Notify::new());
    let quit2 = Arc::clone(&quit);

    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        let wants_quit = console_loop(&rt, &manager, &logs_dir);
        if wants_quit {
            quit2.notify_one();
        }
    });

    quit
}

// ---------------------------------------------------------------------------
// Blocking console loop — returns true iff the user typed quit/shutdown
// ---------------------------------------------------------------------------

fn console_loop(
    rt: &tokio::runtime::Handle,
    manager: &Arc<ServiceManager>,
    logs_dir: &PathBuf,
) -> bool {
    let stdin = io::stdin();
    let mut lock = stdin.lock();

    // If stdin is not a TTY (running as a daemon, pipe, or Docker without -it),
    // skip the interactive console entirely.  The process keeps running, waiting
    // for OS signals only.
    #[cfg(unix)]
    if !is_tty() {
        return false;
    }

    // Wait for the Tokio runtime and services to start accepting connections
    // before dropping into the console.  Services startup is async; this gives
    // them time to emit their initial "service.spawned" log so the console
    // banner lands at the bottom rather than in the middle of startup noise.
    // With service stdout/stderr redirected to /dev/null and tracing going to
    // files only, the terminal should already be quiet by this point.
    std::thread::sleep(Duration::from_secs(2));

    // Clear the current line and print the banner so it's always visible even
    // if any stray output arrived during the sleep.
    print!("\r\x1b[2K");  // CR + erase line
    println!();
    println!("  ┌──────────────────────────────────────────────┐");
    println!("  │   OpenAgent  —  type 'help' for commands      │");
    println!("  └──────────────────────────────────────────────┘");
    println!();
    prompt();

    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) | Err(_) => {
                // stdin closed (not a TTY at runtime, or piped input ended).
                // Exit console but do NOT signal shutdown.
                break;
            }
            Ok(_) => {}
        }

        let cmd = line.trim().to_string();
        if cmd.is_empty() {
            prompt();
            continue;
        }

        let parts: Vec<&str> = cmd.splitn(16, ' ').filter(|s| !s.is_empty()).collect();
        let verb = parts[0].to_lowercase();

        match verb.as_str() {
            "quit" | "exit" | "shutdown" => {
                println!("Shutting down…");
                info!("console.shutdown_requested");
                return true; // signal shutdown
            }

            "help" => println!("{HELP}"),

            "health" => {
                let out = rt.block_on(cmd_health(manager));
                println!("{out}");
            }

            "services" | "service" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                match sub.as_str() {
                    "list" | "" => {
                        let out = rt.block_on(cmd_service_list(manager));
                        println!("{out}");
                    }
                    "restart" => {
                        let name = parts.get(2).copied().unwrap_or("");
                        if name.is_empty() {
                            eprintln!("Usage: service restart <name>");
                        } else {
                            let out = rt.block_on(cmd_service_restart(manager, name));
                            println!("{out}");
                        }
                    }
                    _ => eprintln!("Usage: services | service restart <name>"),
                }
            }

            "tools" => {
                let filter = parts.get(1).copied().unwrap_or("");
                let out = rt.block_on(cmd_tools(manager, filter));
                println!("{out}");
            }

            "guard" => {
                let sub = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
                let out = rt.block_on(cmd_guard(manager, &sub, &parts));
                println!("{out}");
            }

            "tool" => {
                if parts.len() < 2 {
                    eprintln!("Usage: tool <name> [<json params>]");
                } else {
                    let name = parts[1];
                    let raw = if parts.len() > 2 { parts[2..].join(" ") } else { "{}".into() };
                    let out = rt.block_on(cmd_raw_tool(manager, name, &raw));
                    println!("{out}");
                }
            }

            "logs" => {
                let service = parts.get(1).copied().unwrap_or("openagent");
                stream_logs(service, logs_dir, &mut lock, &mut line);
            }

            _ => eprintln!("Unknown command: {verb:?} — type 'help'"),
        }

        prompt();
    }

    false // stdin closed, not an explicit quit
}

fn prompt() {
    // \r\x1b[2K: carriage return + erase current line, so even if a stray newline
    // landed above, the prompt always appears cleanly on its own line.
    print!("\r\x1b[2K\x1b[1;33mopenagent❯\x1b[0m ");
    let _ = io::stdout().flush();
}

/// Returns true if file descriptor 0 (stdin) is an interactive terminal.
#[cfg(unix)]
fn is_tty() -> bool {
    // SAFETY: isatty is a pure query — no side effects, no memory unsafety.
    unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
}

// ---------------------------------------------------------------------------
// Command implementations (async, called via block_on)
// ---------------------------------------------------------------------------

async fn cmd_health(manager: &ServiceManager) -> String {
    let live = manager.live_services().await;
    let tool_count = manager.tools().await.len();

    let self_pid = std::process::id();
    let self_rss = rss_kb(self_pid)
        .map(|kb| format!("{:.1}MB", kb as f64 / 1024.0))
        .unwrap_or_else(|| "—".into());

    let mut lines = vec![
        format!("status:    ok"),
        format!("pid:       {self_pid}  rss: {self_rss}"),
        format!("tools:     {tool_count} registered"),
        format!("services:  {}", live.len()),
        String::new(),
    ];

    for svc in &live {
        lines.push(format!("  {:<22} addr={}  connected", svc.name, svc.address));
    }

    lines.join("\n")
}

async fn cmd_service_list(manager: &ServiceManager) -> String {
    let live = manager.live_services().await;
    if live.is_empty() {
        return "No services connected. Start them with: systemctl start openagent-<name> (or ./services.sh)".into();
    }
    let mut lines = vec![format!("{:<22}  status", "name")];
    lines.push("-".repeat(38));
    for s in &live {
        lines.push(format!("{:<22}  connected ({})", s.name, s.address));
    }
    lines.join("\n")
}

async fn cmd_service_restart(_manager: &ServiceManager, name: &str) -> String {
    // openagent no longer manages service processes.
    // Restart via systemd: systemctl restart openagent-<name>
    // Or via services.sh on dev machines.
    format!(
        "{name}: openagent does not manage service processes.\n\
         To restart: systemctl restart openagent-{name}\n\
         On dev:     ./services.sh restart {name}"
    )
}

async fn cmd_tools(manager: &ServiceManager, filter: &str) -> String {
    let tools = manager.tools().await;
    let matching: Vec<_> = tools
        .iter()
        .filter(|t| {
            let name = t.definition.get("name").and_then(Value::as_str).unwrap_or("");
            filter.is_empty() || name.contains(filter)
        })
        .collect();

    if matching.is_empty() {
        return if filter.is_empty() {
            "No tools registered".into()
        } else {
            format!("No tools matching '{filter}'")
        };
    }

    let mut lines = vec![format!("{:<20} {:<35} description", "service", "tool")];
    lines.push("-".repeat(80));
    for t in &matching {
        let svc = t.service.as_str();
        let name = t.definition.get("name").and_then(Value::as_str).unwrap_or("");
        let desc = t.definition.get("description").and_then(Value::as_str).unwrap_or("");
        let desc_short: String = desc.chars().take(40).collect();
        lines.push(format!("{svc:<20} {name:<35} {desc_short}"));
    }
    lines.join("\n")
}

async fn cmd_guard(manager: &ServiceManager, sub: &str, parts: &[&str]) -> String {
    match sub {
        "list" => tool_call(manager, "guard.list", json!({})).await,

        "allow" => {
            if parts.len() < 4 {
                return "Usage: guard allow <platform> <channel_id> [<name>]".into();
            }
            let (platform, channel_id) = (parts[2], parts[3]);
            let name = if parts.len() > 4 { parts[4..].join(" ") } else { String::new() };
            tool_call(manager, "guard.allow", json!({"platform": platform, "channel_id": channel_id, "name": name})).await
        }

        "block" => {
            if parts.len() < 4 {
                return "Usage: guard block <platform> <channel_id> [<note>]".into();
            }
            let (platform, channel_id) = (parts[2], parts[3]);
            let note = if parts.len() > 4 { parts[4..].join(" ") } else { String::new() };
            tool_call(manager, "guard.block", json!({"platform": platform, "channel_id": channel_id, "note": note})).await
        }

        "name" => {
            if parts.len() < 5 {
                return "Usage: guard name <platform> <channel_id> <name>".into();
            }
            let (platform, channel_id) = (parts[2], parts[3]);
            let name = parts[4..].join(" ");
            tool_call(manager, "guard.name", json!({"platform": platform, "channel_id": channel_id, "name": name})).await
        }

        "remove" => {
            if parts.len() < 4 {
                return "Usage: guard remove <platform> <channel_id>".into();
            }
            let (platform, channel_id) = (parts[2], parts[3]);
            tool_call(manager, "guard.remove", json!({"platform": platform, "channel_id": channel_id})).await
        }

        _ => "Usage: guard list | allow | block | name | remove".into(),
    }
}

async fn cmd_raw_tool(manager: &ServiceManager, name: &str, raw: &str) -> String {
    let params: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return format!("Invalid JSON: {e}"),
    };
    tool_call(manager, name, params).await
}

async fn tool_call(manager: &ServiceManager, name: &str, params: Value) -> String {
    match manager.call_tool(name, params, 15_000).await {
        Ok(raw) => serde_json::from_str::<Value>(&raw)
            .map(|v| serde_json::to_string_pretty(&v).unwrap_or(raw.clone()))
            .unwrap_or(raw),
        Err(e) => format!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Logs streaming
// ---------------------------------------------------------------------------

/// Tail the log file for `service` until the user presses Enter.
fn stream_logs(service: &str, logs_dir: &PathBuf, lock: &mut io::StdinLock, line: &mut String) {
    let log_path = match find_latest_log(logs_dir, service) {
        Some(p) => p,
        None => {
            println!("[logs] No log file found for '{service}' in {}", logs_dir.display());
            return;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);

    let path_clone = log_path.clone();
    let log_thread = std::thread::spawn(move || {
        use std::io::{BufReader, Seek, SeekFrom};
        let Ok(f) = std::fs::File::open(&path_clone) else { return };
        let mut reader = BufReader::new(f);
        let _ = reader.seek(SeekFrom::End(0)); // tail from now

        let mut buf = String::new();
        while !stop2.load(Ordering::Relaxed) {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(100)),
                Ok(_) => {
                    print!("{}", format_log_line(buf.trim()));
                    println!();
                    let _ = io::stdout().flush();
                }
                Err(_) => break,
            }
        }
    });

    println!(
        "[logs] Streaming {} — press Enter to stop",
        log_path.display()
    );
    let _ = io::stdout().flush();

    line.clear();
    let _ = lock.read_line(line);

    stop.store(true, Ordering::Relaxed);
    let _ = log_thread.join();
    println!("[logs] stopped");
}

/// Find the most recently modified log file for a service.
/// Covers both `{svc}-logs.YYYY-MM-DD` (tracing_appender) and
/// `{svc}-logs-YYYY-MM-DD.jsonl` (sdk-rust FileLogExporter).
fn find_latest_log(logs_dir: &PathBuf, service: &str) -> Option<PathBuf> {
    let prefix = format!("{service}-logs");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            name.to_string_lossy().starts_with(&prefix)
        })
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();

    candidates.sort_by_key(|(t, _)| *t);
    candidates.into_iter().last().map(|(_, p)| p)
}

/// Format one JSON log line from `tracing_subscriber::fmt::Layer().json()`:
/// `{"timestamp":"…","level":"INFO","fields":{"message":"…",…},"target":"…"}`
///
/// Falls back to the raw line if parsing fails.
fn format_log_line(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };

    let ts = v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| s.get(11..19)) // HH:MM:SS
        .unwrap_or("??:??:??");

    let level = v.get("level").and_then(Value::as_str).unwrap_or("?");

    let fields = v.get("fields");
    let msg = fields
        .and_then(|f| f.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let extras: Vec<String> = fields
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| k.as_str() != "message")
                .map(|(k, v)| {
                    let val = v.as_str().map(|s| s.to_string())
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                        .unwrap_or_else(|| v.to_string());
                    format!("{k}={val}")
                })
                .collect()
        })
        .unwrap_or_default();

    let level_colored = match level {
        "ERROR" => "\x1b[31mERROR\x1b[0m",
        "WARN"  => "\x1b[33mWARN \x1b[0m",
        "INFO"  => "\x1b[32mINFO \x1b[0m",
        "DEBUG" => "\x1b[34mDEBUG\x1b[0m",
        "TRACE" => "\x1b[90mTRACE\x1b[0m",
        other   => other,
    };

    if extras.is_empty() {
        format!("\x1b[90m{ts}\x1b[0m {level_colored} {msg}")
    } else {
        format!(
            "\x1b[90m{ts}\x1b[0m {level_colored} {msg}  \x1b[90m{}\x1b[0m",
            extras.join(" ")
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rss_kb(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok()
}
