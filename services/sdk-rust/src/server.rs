use crate::codec::{Decoder, Encoder};
use crate::error::{Error, Result};
use crate::types::{Frame, OutboundEvent, ToolDefinition};
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Async handler return type — a pinned boxed future so handlers can be stored
/// as trait objects without knowing the concrete future type.
type BoxFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;

/// Handler closures are application code and may use `anyhow::Result<String>`.
/// The server converts any error to a `tool.result` error frame — handler
/// errors never propagate as [`Error`] variants.
type ToolHandler = Box<dyn Fn(serde_json::Value) -> BoxFuture + Send + Sync>;

/// MCP-lite server — accepts Unix socket connections and dispatches tool calls.
///
/// Also carries a [`broadcast::Sender<OutboundEvent>`] so service code can push
/// unprompted events to every connected Python client.  Retrieve it with
/// [`McpLiteServer::event_sender`] before calling [`McpLiteServer::serve`].
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
        // capacity 256: matches the Go event channel buffer
        let (event_tx, _) = broadcast::channel(256);
        Self {
            tools,
            handlers: HashMap::new(),
            status: status.to_string(),
            event_tx,
        }
    }

    /// Return a sender that broadcasts [`OutboundEvent`] frames to every active connection.
    ///
    /// Call this before [`McpLiteServer::serve`] and keep the sender alive for the
    /// lifetime of the service.  Events sent when no client is connected are
    /// silently dropped.
    pub fn event_sender(&self) -> broadcast::Sender<OutboundEvent> {
        self.event_tx.clone()
    }

    /// Register an async handler for a named tool.
    ///
    /// The handler receives the raw JSON params and returns a JSON result
    /// string.  Returning `Err` sends a `tool.result` error frame to the
    /// caller — it does not terminate the connection.
    pub fn register_tool<F, Fut>(&mut self, name: &str, handler: F)
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.handlers
            .insert(name.to_string(), Box::new(move |p| Box::pin(handler(p))));
    }

    /// Dispatch a single frame and return the response frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFrame`] for frame types the server cannot
    /// handle (e.g. an inbound `tool.result`).
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
                // Build a tracing span parented under the Python AgentLoop trace context
                // propagated via trace_id/span_id MCP-lite fields.
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

    /// Bind to `socket_path` and serve requests until the process exits.
    ///
    /// Spawns one task per connection; each connection runs until the remote
    /// side closes or an I/O error occurs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the socket directory cannot be created, the
    /// stale socket file cannot be removed, or the listener cannot bind.
    pub async fn serve(self, socket_path: &str) -> Result<()> {
        let path = Path::new(socket_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if path.exists() {
            fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        info!(socket = %socket_path, "service.listen");

        let server = Arc::new(self);
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let server_clone = Arc::clone(&server);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, server_clone).await {
                            error!(error = %e, "connection.error");
                        }
                    });
                }
                Err(e) => {
                    error!(error = %e, "accept.error");
                }
            }
        }
    }
}

async fn handle_connection(stream: tokio::net::UnixStream, server: Arc<McpLiteServer>) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut decoder = Decoder::new(read_half);
    let encoder = Arc::new(Mutex::new(Encoder::new(write_half)));

    // Subscribe before spawning so no events are missed from this point on.
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
        let server = Arc::clone(&server);
        let encoder = Arc::clone(&encoder);
        tokio::spawn(async move {
            let id = frame.id().to_string();
            match server.handle_request(frame).await {
                Ok(response) => {
                    let mut enc = encoder.lock().await;
                    if let Err(e) = enc.write_frame(&response).await {
                        error!(request_id = %id, error = %e, "response.write.error");
                    }
                }
                Err(e) => {
                    warn!(request_id = %id, error = %e, "request.handle.error");
                    let err_response = Frame::ErrorResponse {
                        id,
                        code: "INTERNAL_ERROR".to_string(),
                        message: e.to_string(),
                    };
                    let mut enc = encoder.lock().await;
                    let _ = enc.write_frame(&err_response).await;
                }
            }
        });
    }

    pump.abort();
    Ok(())
}
