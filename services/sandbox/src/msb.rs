//! microsandbox HTTP/JSON-RPC client and sandbox lifecycle helpers.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_MSB_URL: &str = "http://127.0.0.1:5555";
pub const DEFAULT_MEMORY_MB: u64 = 512;
const NAMESPACE: &str = "default";

/// Synchronous JSON-RPC 2.0 client for the microsandbox server.
#[derive(Debug)]
pub struct MsbClient {
    rpc_url: String,
    api_key: String,
    pub memory_mb: u64,
}

impl MsbClient {
    /// Construct from environment variables.
    ///
    /// - `MSB_SERVER_URL` — base URL (default: `http://127.0.0.1:5555`)
    /// - `MSB_API_KEY`    — required; obtain with `msb server keygen`
    /// - `MSB_MEMORY_MB`  — VM memory limit (default: 512)
    pub fn from_env() -> Result<Self> {
        let base_url =
            env::var("MSB_SERVER_URL").unwrap_or_else(|_| DEFAULT_MSB_URL.to_string());
        let api_key =
            env::var("MSB_API_KEY").context("MSB_API_KEY required — run: msb server keygen")?;
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

    /// POST a JSON-RPC request and return the parsed response.
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
            return Err(anyhow::anyhow!("MSB returned {}: {}", resp.status_code, body_text));
        }
        let val: Value = resp.json().map_err(|e| anyhow::anyhow!("JSON parse: {e}"))?;
        if let Some(err) = val.get("error") {
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
            return Err(anyhow::anyhow!("MSB RPC error: {msg}"));
        }
        Ok(val)
    }

    /// Start a named sandbox with the given OCI image.
    pub fn start(&self, name: &str, image: &str) -> Result<()> {
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

    /// Stop and destroy a sandbox (best-effort; errors are silently dropped).
    pub fn stop(&self, name: &str) {
        let _ = self.post(&json!({
            "jsonrpc": "2.0",
            "method": "sandbox.stop",
            "params": { "sandbox": name, "namespace": NAMESPACE },
            "id": "4"
        }));
    }

    /// Execute code in the REPL of a running sandbox.
    pub fn repl_run(&self, name: &str, language: &str, code: &str) -> Result<String> {
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

    /// Run a shell command in a running sandbox.
    pub fn command_run(&self, name: &str, command: &str) -> Result<String> {
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

/// Combine stdout + stderr from a sandbox JSON-RPC response into one string.
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
        (true, false) => format!("[stderr]\n{stderr}"),
        (false, false) => format!("{stdout}\n[stderr]\n{stderr}"),
    }
}

/// Generate a unique sandbox name for one invocation (avoids concurrent name collisions).
pub fn sandbox_name(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("oa-{prefix}-{ts}")
}
