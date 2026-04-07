use crate::codec::{Decoder, Encoder};
use crate::error::{Error, Result};
use crate::types::{Frame, OutboundEvent, ToolDefinition};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Async handler return type.
type BoxFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;

/// Handler closures stored as trait objects.
type ToolHandler = Box<dyn Fn(serde_json::Value) -> BoxFuture + Send + Sync>;

/// MCP-lite server — accepts connections and dispatches tool calls.
///
/// Supports two transports selectable at runtime:
///   - Unix Domain Socket (default, same host)
///   - TCP (LAN / multi-machine deployment)
///
/// Transport is selected by `serve_auto(default_socket)`:
///   - If `OPENAGENT_TCP_ADDRESS` is set (e.g. `0.0.0.0:9001`) → TCP
///   - Otherwise reads `OPENAGENT_SOCKET_PATH` (or uses `default_socket`) → UDS
pub struct McpLiteServer {
    tools: Vec<ToolDefinition>,
    handlers: HashMap<String, ToolHandler>,
    status: String,
    event_tx: broadcast::Sender<OutboundEvent>,
}

impl std::fmt::Debug for McpLiteServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpLiteServer")
            .field("tools", &self.tools)
            .field("handlers", &self.handlers.keys().collect::<Vec<_>>())
            .field("status", &self.status)
            .finish()
    }
}

impl McpLiteServer {
    pub fn new(tools: Vec<ToolDefinition>, status: &str) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            tools,
            handlers: HashMap::new(),
            status: status.to_string(),
            event_tx,
        }
    }

    /// Return a sender that broadcasts [`OutboundEvent`] frames to every active connection.
    pub fn event_sender(&self) -> broadcast::Sender<OutboundEvent> {
        self.event_tx.clone()
    }

    /// Register an async handler for a named tool.
    pub fn register_tool<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.handlers
            .insert(name.to_string(), Box::new(move |p| Box::pin(handler(p))));
    }

    /// Dispatch a single frame and return the response frame.
    pub async fn handle_request(&self, frame: Frame) -> Result<Frame> {
        match frame {
            Frame::PingRequest { id } => Ok(Frame::PingResponse {
                id,
                status: self.status.clone(),
            }),
            Frame::ToolListRequest { id } => Ok(Frame::ToolListResponse {
                id,
                tools: self.tools.clone(),
            }),
            Frame::ToolCallRequest { id, tool, params, trace_id, span_id } => {
                let span = tracing::info_span!(
                    "tool.call",
                    otel.kind = "SERVER",
                    tool = %tool,
                    request_id = %id,
                    status = tracing::field::Empty,
                    duration_ms = tracing::field::Empty,
                );
                if let (Some(tid), Some(sid)) = (trace_id.as_deref(), span_id.as_deref()) {
                    if let Some(parent_cx) = crate::otel::context_from_ids(tid, sid) {
                        span.set_parent(parent_cx);
                    }
                }
                let _enter = span.enter();
                let start = Instant::now();

                let response = if let Some(handler) = self.handlers.get(&tool) {
                    match handler(params).await {
                        Ok(res) => {
                            span.record("status", "ok");
                            Frame::ToolCallResponse { id, result: Some(res), error: None }
                        }
                        Err(err) => {
                            span.record("status", "error");
                            error!(tool = %tool, error = %err, "tool.handler.error");
                            Frame::ToolCallResponse { id, result: None, error: Some(err.to_string()) }
                        }
                    }
                } else {
                    span.record("status", "not_found");
                    warn!(tool = %tool, "tool.not_found");
                    Frame::ErrorResponse {
                        id,
                        code: "TOOL_NOT_FOUND".to_string(),
                        message: format!("tool {tool} not found"),
                    }
                };

                span.record("duration_ms", start.elapsed().as_millis() as i64);
                Ok(response)
            }
            _ => Err(Error::UnsupportedFrame),
        }
    }

    // -------------------------------------------------------------------------
    // Transport selection
    // -------------------------------------------------------------------------

    /// Bind and serve on TCP.
    ///
    /// Address is resolved in this priority:
    /// 1. `OPENAGENT_TCP_ADDRESS` env var
    /// 2. `default_addr` argument (compile-time default from each service)
    ///
    /// Sets `TCP_NODELAY` on every connection.
    pub async fn serve_auto(self, default_addr: &str) -> Result<()> {
        let addr = std::env::var("OPENAGENT_TCP_ADDRESS")
            .unwrap_or_else(|_| default_addr.to_string());
        self.serve_tcp(&addr).await
    }

    /// Bind to a TCP address and serve.
    pub async fn serve_tcp(self, addr: &str) -> Result<()> {
        let listener = TcpListener::bind(addr).await
            .map_err(|e| Error::Io(e))?;
        info!(addr = %addr, "service.listen.tcp");

        let server = Arc::new(self);
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    // Disable Nagle — tool calls are small request/response pairs,
                    // not bulk streams. TCP_NODELAY cuts ~40ms on most LANs.
                    if let Err(e) = stream.set_nodelay(true) {
                        warn!(peer = %peer, error = %e, "tcp.nodelay.failed");
                    }
                    let srv = Arc::clone(&server);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, srv).await {
                            error!(peer = %peer, error = %e, "connection.error");
                        }
                    });
                }
                Err(e) => error!(error = %e, "accept.error"),
            }
        }
    }

}

// -----------------------------------------------------------------------------
// Generic connection handler — works for both UnixStream and TcpStream
// -----------------------------------------------------------------------------

async fn handle_connection<S>(stream: S, server: Arc<McpLiteServer>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut decoder = Decoder::new(read_half);
    let encoder = Arc::new(Mutex::new(Encoder::new(write_half)));

    let mut event_rx = server.event_tx.subscribe();
    let enc_pump = Arc::clone(&encoder);
    let pump = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    let mut enc = enc_pump.lock().await;
                    if let Err(e) = enc.write_event(&event).await {
                        warn!(error = %e, "event.write.error");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(dropped = n, "event.queue.lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    while let Ok(Some(frame)) = decoder.next_frame().await {
        let srv = Arc::clone(&server);
        let enc = Arc::clone(&encoder);
        tokio::spawn(async move {
            let id = frame.id().to_string();
            match srv.handle_request(frame).await {
                Ok(response) => {
                    let mut e = enc.lock().await;
                    if let Err(err) = e.write_frame(&response).await {
                        error!(request_id = %id, error = %err, "response.write.error");
                    }
                }
                Err(e) => {
                    warn!(request_id = %id, error = %e, "request.handle.error");
                    let err_response = Frame::ErrorResponse {
                        id,
                        code: "INTERNAL_ERROR".to_string(),
                        message: e.to_string(),
                    };
                    let mut enc = enc.lock().await;
                    let _ = enc.write_frame(&err_response).await;
                }
            }
        });
    }

    pump.abort();
    Ok(())
}
