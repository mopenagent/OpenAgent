/// Async MCP-lite client over TCP.
///
/// Sends newline-delimited JSON frames; matches responses by `id`;
/// routes unsolicited event frames to a broadcast channel.
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::time::{timeout, Duration};
use tracing::{debug, error, warn};
use uuid::Uuid;

/// Sender half of the pending-request registry.
type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

/// A connected MCP-lite client.
#[derive(Debug, Clone)]
pub struct McpLiteClient {
    /// Channel for sending raw JSON frames to the write task.
    tx: tokio::sync::mpsc::Sender<String>,
    /// Pending request registry — maps request id → oneshot sender.
    pending: PendingMap,
    /// Broadcast channel for unsolicited event frames.
    events: broadcast::Sender<Value>,
}

impl McpLiteClient {
    /// Connect to `addr` (e.g. `"127.0.0.1:9001"`) and start background read/write tasks.
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("connect to {addr}"))?;
        // Disable Nagle — tool calls are small request/response pairs.
        stream.set_nodelay(true).ok();

        let (read_half, mut write_half) = stream.into_split();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(64);
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<String>(64);

        // Write task — drains the mpsc channel and sends frames.
        tokio::spawn(async move {
            while let Some(frame) = write_rx.recv().await {
                let line = format!("{frame}\n");
                if let Err(e) = write_half.write_all(line.as_bytes()).await {
                    error!(error = %e, "mcplite.write.error");
                    break;
                }
            }
        });

        // Read task — receives frames, routes to pending or event broadcast.
        {
            let pending = Arc::clone(&pending);
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => {
                            debug!("mcplite.eof");
                            break;
                        }
                        Err(e) => {
                            error!(error = %e, "mcplite.read.error");
                            break;
                        }
                        Ok(_) => {}
                    }

                    let frame: Value = match serde_json::from_str(line.trim()) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, raw = %line.trim(), "mcplite.parse.error");
                            continue;
                        }
                    };

                    let id = frame.get("id").and_then(Value::as_str).map(str::to_string);

                    if let Some(id) = id {
                        // Matched response — complete the pending oneshot.
                        let sender = pending.lock().await.remove(&id);
                        if let Some(tx) = sender {
                            let _ = tx.send(frame);
                        }
                    } else {
                        // Unsolicited event — broadcast to all subscribers.
                        let _ = event_tx.send(frame);
                    }
                }
            });
        }

        Ok(Self {
            tx: write_tx,
            pending,
            events: event_tx,
        })
    }

    /// Send a request and await the response, with a `timeout_ms` deadline.
    pub async fn request(&self, mut frame: Value, timeout_ms: u64) -> Result<Value> {
        let id = Uuid::new_v4().to_string();
        frame["id"] = Value::String(id.clone());

        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), resp_tx);

        self.tx
            .send(serde_json::to_string(&frame)?)
            .await
            .map_err(|_| anyhow!("write channel closed"))?;

        timeout(Duration::from_millis(timeout_ms), resp_rx)
            .await
            .map_err(|_| {
                // Clean up the pending entry on timeout.
                let pending = Arc::clone(&self.pending);
                let id2 = id.clone();
                tokio::spawn(async move { pending.lock().await.remove(&id2); });
                anyhow!("mcplite request timed out after {timeout_ms}ms")
            })?
            .map_err(|_| anyhow!("response sender dropped"))
    }

    /// Subscribe to unsolicited event frames.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Send `tools.list` and return the tools array.
    pub async fn tools_list(&self, timeout_ms: u64) -> Result<Vec<Value>> {
        let resp = self
            .request(json!({"type": "tools.list"}), timeout_ms)
            .await?;
        resp.get("tools")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| anyhow!("tools.list response missing `tools` array"))
    }

    /// Send a `ping` and return `true` if a `pong` arrives within `timeout_ms`.
    pub async fn ping(&self, timeout_ms: u64) -> bool {
        self.request(json!({"type": "ping"}), timeout_ms)
            .await
            .map(|r| r.get("type").and_then(Value::as_str) == Some("pong"))
            .unwrap_or(false)
    }

    /// Call a tool by name with `params` and return the result string.
    pub async fn call_tool(
        &self,
        tool: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<String> {
        let resp = self
            .request(
                json!({"type": "tool.call", "tool": tool, "params": params}),
                timeout_ms,
            )
            .await?;

        if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
            return Err(anyhow!("tool error: {err}"));
        }

        resp.get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("tool.result missing `result` field"))
    }
}
