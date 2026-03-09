//! Telegram MCP-lite service.
//!
//! Connects to Telegram via Teloxide (Bot API, long polling) and exposes three tools
//! plus two event streams over a Unix Domain Socket using the MCP-lite protocol.
//!
//! # Tools
//! - `telegram.status`       — service health snapshot
//! - `telegram.link_state`   — connection/auth state
//! - `telegram.send_message` — send a message to a user (chat_id = user_id for DMs)
//!
//! # Events (pushed to Python on change)
//! - `telegram.connection.status`  — emitted on connect / disconnect / error
//! - `telegram.message.received`   — emitted for every inbound private message
//!
//! Bot API uses chat_id (no access_hash). Adapter compatibility: we accept user_id + access_hash
//! but use user_id as chat_id; access_hash is ignored.

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Context as _;
use sdk_rust::{McpLiteServer, OutboundEvent, ToolDefinition};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use teloxide::prelude::*;
use tokio::runtime::Handle;
use tracing::{info, warn};

const BACKEND: &str = "teloxide";

// ---------------------------------------------------------------------------
// Shared runtime state
// ---------------------------------------------------------------------------

struct TelegramState {
    started: AtomicBool,
    connected: AtomicBool,
    authorized: AtomicBool,
    last_error: Mutex<String>,
    bot: Mutex<Option<Bot>>,
    event_tx: tokio::sync::broadcast::Sender<OutboundEvent>,
}

impl std::fmt::Debug for TelegramState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramState")
            .field("started", &self.started)
            .field("connected", &self.connected)
            .field("authorized", &self.authorized)
            .finish()
    }
}

impl TelegramState {
    fn new(event_tx: tokio::sync::broadcast::Sender<OutboundEvent>) -> Self {
        Self {
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            authorized: AtomicBool::new(false),
            last_error: Mutex::new(String::new()),
            bot: Mutex::new(None),
            event_tx,
        }
    }

    fn set_error(&self, msg: &str) {
        *self.last_error.lock().expect("last_error poisoned") = msg.to_string();
    }

    fn error_text(&self) -> String {
        self.last_error.lock().expect("last_error poisoned").clone()
    }

    fn emit_connection_status(&self) {
        let mut data = serde_json::json!({
            "connected":  self.connected.load(Ordering::Acquire),
            "authorized": self.authorized.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        let err = self.error_text();
        if !err.is_empty() {
            data["last_error"] = serde_json::Value::String(err);
        }
        let _ = self.event_tx.send(OutboundEvent::new("telegram.connection.status", data));
    }

    fn status_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "running":    self.started.load(Ordering::Acquire),
            "connected":  self.connected.load(Ordering::Acquire),
            "authorized": self.authorized.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        let err = self.error_text();
        if !err.is_empty() {
            v["last_error"] = serde_json::Value::String(err);
        }
        v
    }

    fn link_state_json(&self) -> serde_json::Value {
        serde_json::json!({
            "authorized": self.authorized.load(Ordering::Acquire),
            "connected":  self.connected.load(Ordering::Acquire),
            "backend":    BACKEND,
        })
    }
}

// ---------------------------------------------------------------------------
// Message handler — forward private text messages to event_tx
// ---------------------------------------------------------------------------

async fn handle_message(
    _bot: Bot,
    msg: Message,
    event_tx: tokio::sync::broadcast::Sender<OutboundEvent>,
) -> Result<(), teloxide::RequestError> {
    // Only private (DM) messages
    if !matches!(msg.chat.kind, teloxide::types::ChatKind::Private(_)) {
        return Ok(());
    }

    let text = match msg.text() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return Ok(()),
    };

    let from_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);
    let from_name = msg
        .from
        .as_ref()
        .map(|u| {
            let mut n = u.first_name.clone();
            if let Some(ref last) = u.last_name {
                if !last.is_empty() {
                    n.push(' ');
                    n.push_str(last);
                }
            }
            n
        })
        .unwrap_or_default();
    let username = msg
        .from
        .as_ref()
        .and_then(|u| u.username.as_deref())
        .unwrap_or("")
        .to_string();

    // Bot API has no access_hash; use 0 for adapter compatibility
    let data = serde_json::json!({
        "message_id":  msg.id.0,
        "from_id":     from_id,
        "access_hash": 0i64,
        "from_name":   from_name,
        "username":    username,
        "text":        text,
    });
    let _ = event_tx.send(OutboundEvent::new("telegram.message.received", data));

    // Don't auto-reply — agent handles responses via send_message tool
    Ok(())
}

async fn message_handler(
    bot: Bot,
    msg: Message,
    event_tx: tokio::sync::broadcast::Sender<OutboundEvent>,
) -> Result<(), teloxide::RequestError> {
    handle_message(bot, msg, event_tx).await
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "telegram.status".into(),
            description: "Return current Telegram service status.".into(),
            params: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "telegram.link_state".into(),
            description: "Return Telegram bot authorization and connection state.".into(),
            params: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "telegram.send_message".into(),
            description: "Send Telegram message. user_id = chat_id for private chats; access_hash ignored (Bot API).".into(),
            params: serde_json::json!({
                "type": "object",
                "properties": {
                    "user_id":     { "type": "integer", "description": "Telegram user/chat ID." },
                    "access_hash": { "type": "integer", "description": "Ignored (Bot API)." },
                    "text":        { "type": "string", "description": "Message text." }
                },
                "required": ["user_id", "text"]
            }),
        },
    ]
}

fn parse_i64(value: &serde_json::Value) -> anyhow::Result<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64().ok_or_else(|| anyhow::anyhow!("invalid number")),
        serde_json::Value::String(s) => s.parse().map_err(|e| anyhow::anyhow!("invalid user_id: {e}")),
        _ => anyhow::bail!("user_id must be number or string"),
    }
}

fn register_handlers(server: &mut McpLiteServer, state: Arc<TelegramState>) {
    let s = Arc::clone(&state);
    server.register_tool("telegram.status", move |_params| {
        Ok(s.status_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("telegram.link_state", move |_params| {
        Ok(s.link_state_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("telegram.send_message", move |params| {
        let user_id = params
            .get("user_id")
            .ok_or_else(|| anyhow::anyhow!("user_id is required"))?;
        let user_id = parse_i64(user_id)?;
        let text = params["text"].as_str().unwrap_or("").to_string();

        if text.is_empty() {
            anyhow::bail!("text is required");
        }
        if !s.started.load(Ordering::Acquire) {
            anyhow::bail!("telegram runtime is not started");
        }

        let bot = s.bot.lock().expect("bot poisoned").clone();
        let bot = bot.ok_or_else(|| anyhow::anyhow!("telegram not connected"))?;

        let chat_id = teloxide::types::ChatId(user_id);

        let result = tokio::task::block_in_place(|| {
            Handle::current().block_on(async { bot.send_message(chat_id, &text).await })
        });

        result.map_err(|e: teloxide::RequestError| {
            s.set_error(&e.to_string());
            s.emit_connection_status();
            anyhow::anyhow!("{e}")
        })?;

        Ok(serde_json::json!({
            "ok":      true,
            "user_id": user_id,
        })
        .to_string())
    });
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn token_from_env() -> Option<String> {
    ["TELEGRAM_BOT_TOKEN", "OPENAGENT_TELEGRAM_BOT_TOKEN", "TELOXIDE_TOKEN"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = token_from_env()
        .ok_or_else(|| anyhow::anyhow!("missing TELEGRAM_BOT_TOKEN or TELOXIDE_TOKEN"))?;

    let socket_path = std::env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| "data/sockets/telegram.sock".to_string());

    let logs_dir = std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    if let Err(e) = sdk_rust::setup_otel("telegram", &logs_dir) {
        eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}");
    }

    let mut server = McpLiteServer::new(make_tools(), "ready");
    let event_tx = server.event_sender();

    let state = Arc::new(TelegramState::new(event_tx.clone()));
    register_handlers(&mut server, Arc::clone(&state));

    let bot = Bot::new(&token);

    // Verify bot token
    match bot.get_me().await {
        Ok(me) => {
            *state.bot.lock().expect("bot poisoned") = Some(bot.clone());
            state.started.store(true, Ordering::Release);
            state.connected.store(true, Ordering::Release);
            state.authorized.store(true, Ordering::Release);
            state.set_error("");
            info!(username = %me.username.as_deref().unwrap_or(""), "telegram.auth_ok");
        }
        Err(e) => {
            state.set_error(&e.to_string());
            state.emit_connection_status();
            return Err(e).context("telegram get_me failed");
        }
    }
    state.emit_connection_status();

    info!(socket = %socket_path, "telegram.start");

    let event_tx_for_handler = event_tx.clone();
    let telegram_handle = tokio::spawn(async move {
        use teloxide::dispatching::UpdateFilterExt;
        use teloxide::types::Update;

        let schema = Update::filter_message()
            .endpoint(move |bot: Bot, msg: Message| {
                let tx = event_tx_for_handler.clone();
                async move { message_handler(bot, msg, tx).await }
            });

        Dispatcher::builder(bot, schema)
            .build()
            .dispatch()
            .await;
    });

    let serve_result = server.serve(&socket_path).await;

    telegram_handle.abort();

    if let Err(e) = serve_result {
        warn!(error = %e, "mcp.server.exit");
    }

    Ok(())
}
