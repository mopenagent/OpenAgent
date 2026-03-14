//! Tool router — dispatches tool calls from the Cortex ReAct loop to the
//! correct service socket using an inline MCP-lite client.
//!
//! The same JSON frame protocol that Python uses to call Cortex is used here by
//! Cortex to call downstream services.  No extra abstraction needed — just a plain
//! async connect → write ToolCallRequest → read ToolCallResponse.
//!
//! Socket routing is derived from the tool name prefix:
//!   `browser.*` → `<socket_dir>/browser.sock`
//!   `sandbox.*` → `<socket_dir>/sandbox.sock`
//!
//! Phase 3+: add `memory.*` → `memory.sock` here.

use anyhow::{anyhow, Result};
use sdk_rust::codec::{Decoder, Encoder};
use sdk_rust::types::Frame;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tracing::{info, warn};

/// Timeout for a single tool call (connect + write + read).
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Routes tool calls to the correct service socket.
///
/// Created once at startup and shared across request handlers.
#[derive(Debug, Clone)]
pub struct ToolRouter {
    socket_dir: PathBuf,
}

impl ToolRouter {
    /// Create a router that resolves sockets relative to `socket_dir`.
    /// Typical value: `data/sockets` (or `OPENAGENT_SOCKET_DIR` env var).
    pub fn new(socket_dir: PathBuf) -> Self {
        Self { socket_dir }
    }

    /// Dispatch `tool` with `arguments` to the owning service.
    ///
    /// Returns the raw result string from the service, or an error JSON string
    /// suitable for feeding back into the LLM context (caller decides policy).
    pub async fn call(&self, tool: &str, arguments: &Value) -> Result<String> {
        let socket_path = self.resolve_socket(tool)?;
        info!(
            tool = %tool,
            socket = %socket_path.display(),
            "cortex.tool_router.call"
        );
        call_service(&socket_path, tool, arguments).await
    }

    /// Map a tool name to its socket path via the tool name prefix.
    fn resolve_socket(&self, tool: &str) -> Result<PathBuf> {
        if !tool.contains('.') {
            return Err(anyhow!(
                "tool name must have a dot-separated owner prefix: {tool}"
            ));
        }
        let owner = tool
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("tool name must have a dot-separated owner prefix: {tool}"))?;
        Ok(self.socket_dir.join(format!("{owner}.sock")))
    }

    /// Returns true if the named service socket file exists on disk.
    /// Used to check availability without attempting a full connection.
    pub fn socket_exists(&self, tool: &str) -> bool {
        self.resolve_socket(tool)
            .map(|p| p.exists())
            .unwrap_or(false)
    }
}

/// Inline MCP-lite client — connects to a Unix socket, writes one ToolCallRequest
/// frame, reads the ToolCallResponse, and returns the result string.
///
/// Uses the same `sdk_rust::codec` and `sdk_rust::types::Frame` that the MCP-lite
/// server uses, so the wire format is identical to what Python sends to Cortex.
async fn call_service(socket_path: &Path, tool: &str, params: &Value) -> Result<String> {
    let stream = timeout(TOOL_CALL_TIMEOUT, UnixStream::connect(socket_path))
        .await
        .map_err(|_| {
            anyhow!(
                "connect to {} timed out after {}s",
                socket_path.display(),
                TOOL_CALL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow!("connect to {}: {e}", socket_path.display()))?;

    let (read_half, write_half) = stream.into_split();
    let mut decoder = Decoder::new(read_half);
    let mut encoder = Encoder::new(write_half);
    let id = request_id();

    encoder
        .write_frame(&Frame::ToolCallRequest {
            id: id.clone(),
            tool: tool.to_string(),
            params: params.clone(),
            trace_id: None,
            span_id: None,
        })
        .await
        .map_err(|e| anyhow!("write tool call frame to {}: {e}", socket_path.display()))?;

    let frame = timeout(TOOL_CALL_TIMEOUT, decoder.next_frame())
        .await
        .map_err(|_| {
            anyhow!(
                "tool result from {} timed out after {}s",
                socket_path.display(),
                TOOL_CALL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow!("read tool result from {}: {e}", socket_path.display()))?;

    let Some(frame) = frame else {
        return Err(anyhow!(
            "service at {} closed connection without responding",
            socket_path.display()
        ));
    };

    match frame {
        Frame::ToolCallResponse { id: resp_id, result, error } if resp_id == id => {
            if let Some(err) = error {
                warn!(tool = %tool, error = %err, "cortex.tool_router.service_error");
                return Err(anyhow!("tool {tool} returned error: {err}"));
            }
            Ok(result.unwrap_or_default())
        }
        Frame::ErrorResponse { id: resp_id, code, message } if resp_id == id => {
            Err(anyhow!("tool {tool} protocol error {code}: {message}"))
        }
        other => Err(anyhow!(
            "unexpected frame from {tool}: {other:?}"
        )),
    }
}

fn request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("cortex-tool-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> ToolRouter {
        ToolRouter::new(PathBuf::from("data/sockets"))
    }

    #[test]
    fn browser_tool_maps_to_browser_sock() {
        let r = router();
        let path = r.resolve_socket("browser.open").unwrap();
        assert_eq!(path, PathBuf::from("data/sockets/browser.sock"));
    }

    #[test]
    fn sandbox_tool_maps_to_sandbox_sock() {
        let r = router();
        let path = r.resolve_socket("sandbox.execute").unwrap();
        assert_eq!(path, PathBuf::from("data/sockets/sandbox.sock"));
    }

    #[test]
    fn tool_without_prefix_returns_error() {
        let r = router();
        assert!(r.resolve_socket("notool").is_err());
    }

    #[test]
    fn empty_tool_name_returns_error() {
        let r = router();
        assert!(r.resolve_socket("").is_err());
    }
}
