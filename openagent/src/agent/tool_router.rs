//! Tool router — dispatches tool calls from the agent ReAct loop to the
//! correct service over TCP using an inline MCP-lite client.
//!
//! All services bind on static TCP ports via the `address` field in each
//! `service.json`.  The ActionCatalog builds the `tool_name → address` map at
//! startup; ToolRouter does a direct lookup per call.
//!
//! `skill.read` is handled in-process without a TCP hop — ToolRouter intercepts
//! it and calls the file-system handler directly.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::agent::handlers::handle_skill_read;

/// Timeout for a single tool call (connect + write + read).
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Routes tool calls to the correct service over TCP, with in-process handling
/// for built-in capabilities (`skill.read`).
///
/// Created once at startup from the ActionCatalog's tool→address map.
#[derive(Debug, Clone)]
pub struct ToolRouter {
    /// Direct tool-name → TCP address lookup (e.g. "browser.open" → "127.0.0.1:9001").
    tool_addresses: HashMap<String, String>,
    /// Project root for in-process `skill.read` calls.
    project_root: PathBuf,
}

impl ToolRouter {
    pub fn new(tool_addresses: HashMap<String, String>, project_root: PathBuf) -> Self {
        Self { tool_addresses, project_root }
    }

    /// Dispatch `tool` with `arguments` to the owning service (or handle in-process).
    pub async fn call(&self, tool: &str, arguments: &Value) -> Result<String> {
        // skill.read is handled in-process — no TCP needed.
        if tool == "skill.read" {
            return Ok(handle_skill_read(arguments, &self.project_root));
        }

        let addr = self.resolve_address(tool)?;
        info!(tool = %tool, addr = %addr, "agent.tool_router.call");
        call_service(&addr, tool, arguments).await
    }

    fn resolve_address(&self, tool: &str) -> Result<String> {
        self.tool_addresses
            .get(tool)
            .map(|addr| normalize_addr(addr))
            .ok_or_else(|| anyhow!("no TCP address registered for tool: {tool}"))
    }

    /// Returns true if the named tool has a registered address or is a built-in.
    pub fn tool_registered(&self, tool: &str) -> bool {
        tool == "skill.read" || self.tool_addresses.contains_key(tool)
    }
}

fn normalize_addr(addr: &str) -> String {
    addr.replace("0.0.0.0", "127.0.0.1")
}

/// Inline MCP-lite client — connects to a TCP address, sends one tool call
/// frame (newline-delimited JSON), reads the response, and returns the result.
async fn call_service(addr: &str, tool: &str, params: &Value) -> Result<String> {
    let stream = timeout(TOOL_CALL_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("connect to {} timed out after {}s", addr, TOOL_CALL_TIMEOUT.as_secs()))?
        .map_err(|e| anyhow!("connect to {addr}: {e}"))?;

    stream.set_nodelay(true).unwrap_or_else(|e| {
        warn!(addr = %addr, error = %e, "TCP_NODELAY failed (non-fatal)");
    });

    let id = request_id();
    let request = serde_json::json!({
        "id": id,
        "type": "tool.call",
        "tool": tool,
        "params": params,
    });
    let request_line = format!("{}\n", serde_json::to_string(&request)?);

    let (read_half, mut write_half) = stream.into_split();
    timeout(TOOL_CALL_TIMEOUT, write_half.write_all(request_line.as_bytes()))
        .await
        .map_err(|_| anyhow!("write to {addr} timed out"))?
        .map_err(|e| anyhow!("write tool call to {addr}: {e}"))?;

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    timeout(TOOL_CALL_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| anyhow!("read from {addr} timed out after {}s", TOOL_CALL_TIMEOUT.as_secs()))?
        .map_err(|e| anyhow!("read tool result from {addr}: {e}"))?;

    if line.is_empty() {
        return Err(anyhow!("service at {addr} closed connection without responding"));
    }

    let response: Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow!("parse response from {addr}: {e}"))?;

    if let Some(err) = response.get("error").filter(|v| !v.is_null()) {
        warn!(tool = %tool, error = %err, "agent.tool_router.service_error");
        return Err(anyhow!("tool {tool} returned error: {err}"));
    }

    response
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("tool {tool} response missing 'result' field"))
}

fn request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("agent-tool-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_router(map: HashMap<String, String>) -> ToolRouter {
        ToolRouter::new(map, PathBuf::from("data"))
    }

    fn empty_router() -> ToolRouter {
        make_router(HashMap::new())
    }

    #[test]
    fn catalog_tool_resolves_to_registered_address() {
        let mut map = HashMap::new();
        map.insert("web.search".to_string(), "0.0.0.0:9001".to_string());
        map.insert("web.fetch".to_string(), "0.0.0.0:9001".to_string());
        let r = make_router(map);
        assert_eq!(r.resolve_address("web.search").unwrap(), "127.0.0.1:9001");
        assert_eq!(r.resolve_address("web.fetch").unwrap(), "127.0.0.1:9001");
    }

    #[test]
    fn unknown_tool_errors() {
        let r = empty_router();
        assert!(r.resolve_address("memory.search").is_err());
    }

    #[test]
    fn normalize_addr_replaces_unspecified_with_loopback() {
        assert_eq!(normalize_addr("0.0.0.0:9001"), "127.0.0.1:9001");
        assert_eq!(normalize_addr("127.0.0.1:9001"), "127.0.0.1:9001");
    }

    #[test]
    fn skill_read_is_always_registered() {
        let r = empty_router();
        assert!(r.tool_registered("skill.read"));
        assert!(!r.tool_registered("browser.open"));
    }

    #[test]
    fn tool_registered_returns_true_for_known_tools() {
        let mut map = HashMap::new();
        map.insert("memory.search".to_string(), "0.0.0.0:9000".to_string());
        let r = make_router(map);
        assert!(r.tool_registered("memory.search"));
        assert!(r.tool_registered("skill.read"));
        assert!(!r.tool_registered("browser.open"));
    }
}
