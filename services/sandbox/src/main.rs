//! Sandbox service — MCP-lite wrapper for microsandbox.
//!
//! Provides sandboxed code execution (Python, Node.js) and shell commands via a
//! microsandbox server (VM-level OCI isolation).  Supersedes the Go shell service.
//!
//! Tools exposed:
//!   sandbox.execute  — run Python or Node.js code via sandbox.repl.run
//!   sandbox.shell    — run a shell command via sandbox.command.run
//!
//! Environment variables:
//!   OPENAGENT_SOCKET_PATH — Unix socket path (default data/sockets/sandbox.sock)
//!   MSB_SERVER_URL        — microsandbox server URL (default http://127.0.0.1:5555)
//!   MSB_API_KEY           — API key (required; run: msb server keygen)
//!   MSB_MEMORY_MB         — VM memory in MB (default 512)
//!
//! # Abort
//!
//! Panics if the log-level env filter directive is invalid, or if microsandbox
//! returns malformed JSON that violates the expected schema.

use anyhow::{Context, Result};
use mimalloc::MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer, ToolDefinition};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use serde_json::{json, Value};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

const DEFAULT_SOCKET_PATH: &str = "data/sockets/sandbox.sock";
const DEFAULT_MSB_URL: &str = "http://127.0.0.1:5555";
const DEFAULT_MEMORY_MB: u64 = 512;
const NAMESPACE: &str = "default";

// ---------------------------------------------------------------------------
// MSB HTTP client
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct MsbClient {
    rpc_url: String,
    api_key: String,
    memory_mb: u64,
}

impl MsbClient {
    fn from_env() -> Result<Self> {
        let base_url = env::var("MSB_SERVER_URL")
            .unwrap_or_else(|_| DEFAULT_MSB_URL.to_string());
        let api_key = env::var("MSB_API_KEY")
            .context("MSB_API_KEY required — run: msb server keygen")?;
        let memory_mb = env::var("MSB_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MEMORY_MB);
        Ok(Self {
            rpc_url: format!("{}/api/v1/rpc", base_url.trim_end_matches('/')),
            api_key,
            memory_mb,
        })
    }

    fn post(&self, body: &Value) -> Result<Value> {
        let body_str = serde_json::to_string(body).context("Serialize request")?;
        let resp = minreq::post(&self.rpc_url)
            .with_header("Content-Type", "application/json")
            .with_header("Authorization", format!("Bearer {}", self.api_key))
            .with_body(body_str)
            .send()
            .context("HTTP request to microsandbox server failed")?;
        if resp.status_code != 200 {
            let body_text = resp.as_str().unwrap_or("").to_string();
            return Err(anyhow::anyhow!(
                "MSB returned {}: {}",
                resp.status_code,
                body_text
            ));
        }
        let val: Value = resp.json().map_err(|e| anyhow::anyhow!("JSON parse: {}", e))?;
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(anyhow::anyhow!("MSB RPC error: {}", msg));
        }
        Ok(val)
    }

    fn start(&self, name: &str, image: &str) -> Result<()> {
        self.post(&json!({
            "jsonrpc": "2.0",
            "method": "sandbox.start",
            "params": {
                "sandbox": name,
                "namespace": NAMESPACE,
                "config": { "image": image, "memory": self.memory_mb }
            },
            "id": "1"
        }))?;
        Ok(())
    }

    fn stop(&self, name: &str) {
        let _ = self.post(&json!({
            "jsonrpc": "2.0",
            "method": "sandbox.stop",
            "params": { "sandbox": name, "namespace": NAMESPACE },
            "id": "4"
        }));
    }

    fn repl_run(&self, name: &str, language: &str, code: &str) -> Result<String> {
        let resp = self.post(&json!({
            "jsonrpc": "2.0",
            "method": "sandbox.repl.run",
            "params": {
                "sandbox": name,
                "namespace": NAMESPACE,
                "language": language,
                "code": code
            },
            "id": "2"
        }))?;
        Ok(extract_output(&resp))
    }

    fn command_run(&self, name: &str, command: &str) -> Result<String> {
        let resp = self.post(&json!({
            "jsonrpc": "2.0",
            "method": "sandbox.command.run",
            "params": {
                "sandbox": name,
                "namespace": NAMESPACE,
                "command": command
            },
            "id": "3"
        }))?;
        Ok(extract_output(&resp))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Combine stdout + stderr from a sandbox response into one string.
fn extract_output(resp: &Value) -> String {
    let result = resp.get("result");
    let stdout = result
        .and_then(|r| r.get("output"))
        .and_then(|o| o.as_str())
        .unwrap_or("");
    let stderr = result
        .and_then(|r| r.get("error"))
        .and_then(|e| e.as_str())
        .unwrap_or("");
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => format!("[stderr]\n{}", stderr),
        (false, false) => format!("{}\n[stderr]\n{}", stdout, stderr),
    }
}

/// Unique sandbox name per invocation (avoids concurrent name collisions).
fn sandbox_name(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("oa-{}-{}", prefix, ts)
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

fn handle_execute(params: Value) -> Result<String> {
    let params = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Params must be an object"))?;
    let lang = params
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let code = params
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if lang.is_empty() {
        return Err(anyhow::anyhow!("Missing 'language' parameter"));
    }
    if code.is_empty() {
        return Err(anyhow::anyhow!("Missing 'code' parameter"));
    }

    let (image, repl_lang) = match lang {
        "python" => ("microsandbox/python", "python"),
        "node" | "javascript" | "js" => ("microsandbox/node", "javascript"),
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported language '{}'. Supported: python, node",
                other
            ))
        }
    };

    let msb = MsbClient::from_env()?;
    let name = sandbox_name(lang);
    msb.start(&name, image)?;
    let result = msb.repl_run(&name, repl_lang, code);
    msb.stop(&name);
    result
}

fn handle_shell(params: Value) -> Result<String> {
    let params = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Params must be an object"))?;
    let command = params
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if command.is_empty() {
        return Err(anyhow::anyhow!("Missing 'command' parameter"));
    }

    let msb = MsbClient::from_env()?;
    let name = sandbox_name("shell");
    // Python image ships with bash, coreutils, and common Unix tools.
    msb.start(&name, "microsandbox/python")?;
    let result = msb.command_run(&name, command);
    msb.stop(&name);
    result
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    let _otel_guard = match setup_otel("sandbox", &logs_dir) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("otel init failed (continuing without file traces): {e}");
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("sandbox=info".parse().expect("valid log directive")),
                )
                .try_init()
                .ok();
            None
        }
    };

    let socket_path = env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    let tools = vec![
        ToolDefinition {
            name: "sandbox.execute".to_string(),
            description: concat!(
                "Execute Python or Node.js code in a secure OCI sandbox ",
                "(VM-level isolation via microsandbox). ",
                "Use for data processing, calculations, API calls, or scripting. ",
                "Each call starts a fresh sandbox — state is not preserved between calls."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python", "node"],
                        "description": "Runtime: 'python' (Python 3) or 'node' (Node.js)"
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute"
                    }
                },
                "required": ["language", "code"]
            }),
        },
        ToolDefinition {
            name: "sandbox.shell".to_string(),
            description: concat!(
                "Run a shell command in a secure OCI sandbox ",
                "(VM-level isolation via microsandbox). ",
                "Safe alternative to direct host execution — commands run inside a container. ",
                "Use for file operations, package inspection, or running binaries."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute (bash/sh). Example: 'ls -la /tmp && echo done'"
                    }
                },
                "required": ["command"]
            }),
        },
    ];

    let mut server = McpLiteServer::new(tools, "ready");

    server.register_tool("sandbox.execute", |params| handle_execute(params));
    server.register_tool("sandbox.shell", |params| handle_shell(params));

    info!("Sandbox service starting on {}", socket_path);
    server.serve(&socket_path).await?;
    Ok(())
}
