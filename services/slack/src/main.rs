//! Slack MCP-lite service.
//!
//! Connects to Slack via slack-morphism (Socket Mode + Web API) and exposes three tools
//! plus two event streams over a Unix Domain Socket using the MCP-lite protocol.
//!
//! # Tools
//! - `slack.status`       — service health snapshot
//! - `slack.link_state`   — connection/auth state
//! - `slack.send_message` — send a message to a channel
//!
//! # Events (pushed to Python on change)
//! - `slack.connection.status`  — emitted on connect / disconnect / error
//! - `slack.message.received`  — emitted for every inbound message

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Context as _;
use rvstruct::ValueStruct;
use sdk_rust::{McpLiteServer, OutboundEvent, ToolDefinition};
use slack_morphism::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::runtime::Handle;
use tracing::{error, info, warn};

const BACKEND: &str = "slack-morphism";

// ---------------------------------------------------------------------------
// Shared runtime state
// ---------------------------------------------------------------------------

/// State shared between Socket Mode callbacks and MCP-lite tool handlers.
struct SlackState {
    started: AtomicBool,
    connected: AtomicBool,
    authorized: AtomicBool,
    last_error: Mutex<String>,
    bot_user_id: Mutex<String>,
    team_id: Mutex<String>,
    /// Client for chat_post_message; set after auth_test succeeds.
    client: Mutex<Option<Arc<SlackHyperClient>>>,
    bot_token: Mutex<Option<SlackApiToken>>,
    event_tx: tokio::sync::broadcast::Sender<OutboundEvent>,
}

impl std::fmt::Debug for SlackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackState")
            .field("started", &self.started)
            .field("connected", &self.connected)
            .field("authorized", &self.authorized)
            .finish()
    }
}

impl SlackState {
    fn new(event_tx: tokio::sync::broadcast::Sender<OutboundEvent>) -> Self {
        Self {
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            authorized: AtomicBool::new(false),
            last_error: Mutex::new(String::new()),
            bot_user_id: Mutex::new(String::new()),
            team_id: Mutex::new(String::new()),
            client: Mutex::new(None),
            bot_token: Mutex::new(None),
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
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        if !bid.is_empty() {
            data["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            data["team_id"] = serde_json::Value::String(tid);
        }
        let _ = self.event_tx.send(OutboundEvent::new("slack.connection.status", data));
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
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        if !bid.is_empty() {
            v["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            v["team_id"] = serde_json::Value::String(tid);
        }
        v
    }

    fn link_state_json(&self) -> serde_json::Value {
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        let mut v = serde_json::json!({
            "authorized": self.authorized.load(Ordering::Acquire),
            "connected":  self.connected.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        if !bid.is_empty() {
            v["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            v["team_id"] = serde_json::Value::String(tid);
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Socket Mode push events handler
// ---------------------------------------------------------------------------

async fn push_events_handler(
    event: SlackPushEventCallback,
    _client: Arc<SlackHyperClient>,
    states: SlackClientEventsUserState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {

    let state = {
        let guard = states.read().await;
        guard
            .get_user_state::<Arc<SlackState>>()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("missing SlackState in user state"))?
    };

    if let SlackEventCallbackBody::Message(msg) = event.event {
        // Skip bot messages and message edits/deletions (same as Go impl).
        if msg.subtype.is_some() || msg.sender.bot_id.is_some() {
            return Ok(());
        }

        let channel_id = msg
            .origin
            .channel
            .as_ref()
            .map(|c| c.value().to_string())
            .unwrap_or_default();

        let user_id = msg
            .sender
            .user
            .as_ref()
            .map(|u| u.value().to_string())
            .unwrap_or_default();

        let text = msg
            .content
            .as_ref()
            .and_then(|c| c.text.clone())
            .unwrap_or_default();

        if channel_id.is_empty() || text.is_empty() {
            return Ok(());
        }

        let ts = msg
            .message
            .as_ref()
            .map(|m| m.ts.value().to_string())
            .or_else(|| msg.previous_message.as_ref().map(|m| m.ts.value().to_string()))
            .unwrap_or_else(|| msg.origin.ts.value().to_string());

        let data = serde_json::json!({
            "channel_id": channel_id,
            "user_id":    user_id,
            "text":       text,
            "ts":         ts,
            "team_id":    event.team_id.value(),
        });
        let _ = state.event_tx.send(OutboundEvent::new("slack.message.received", data));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tool handlers (sync — use block_in_place for async Slack API calls)
// ---------------------------------------------------------------------------

fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "slack.status".into(),
            description: "Return current Slack service status.".into(),
            params: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "slack.link_state".into(),
            description: "Return Slack auth/connect state.".into(),
            params: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDefinition {
            name: "slack.send_message".into(),
            description: "Send a message to a Slack channel.".into(),
            params: serde_json::json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "Slack channel ID." },
                    "text":       { "type": "string", "description": "Message text." }
                },
                "required": ["channel_id", "text"]
            }),
        },
    ]
}

fn register_handlers(server: &mut McpLiteServer, state: Arc<SlackState>) {
    let s = Arc::clone(&state);
    server.register_tool("slack.status", move |_params| {
        Ok(s.status_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("slack.link_state", move |_params| {
        Ok(s.link_state_json().to_string())
    });

    let s = Arc::clone(&state);
    server.register_tool("slack.send_message", move |params| {
        let channel_id = params["channel_id"].as_str().unwrap_or("").to_string();
        let text = params["text"].as_str().unwrap_or("").to_string();

        if channel_id.is_empty() {
            anyhow::bail!("channel_id is required");
        }
        if text.is_empty() {
            anyhow::bail!("text is required");
        }
        if !s.started.load(Ordering::Acquire) {
            anyhow::bail!("slack runtime is not started");
        }

        let client = s.client.lock().expect("client poisoned").clone();
        let token = s.bot_token.lock().expect("bot_token poisoned").clone();
        let (client, token) = match (client, token) {
            (Some(c), Some(t)) => (c, t),
            _ => anyhow::bail!("slack not connected"),
        };

        let req = SlackApiChatPostMessageRequest::new(
            channel_id.clone().into(),
            SlackMessageContent::new().with_text(text.into()),
        );

        let session = client.open_session(&token);
        let resp = tokio::task::block_in_place(|| {
            Handle::current().block_on(session.chat_post_message(&req))
        })
        .map_err(|e: slack_morphism::errors::SlackClientError| {
            s.set_error(&e.to_string());
            s.emit_connection_status();
            anyhow::anyhow!("{e}")
        })?;

        let ts_str = resp.ts.value().to_string();

        Ok(serde_json::json!({
            "ok":         true,
            "channel_id": channel_id,
            "ts":         ts_str,
        })
        .to_string())
    });
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn token_from_env() -> (Option<String>, Option<String>) {
    let bot = ["SLACK_BOT_TOKEN", "OPENAGENT_SLACK_BOT_TOKEN"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty());
    let app = ["SLACK_APP_TOKEN", "OPENAGENT_SLACK_APP_TOKEN"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty());
    (bot, app)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (bot_token, app_token) = token_from_env();
    let bot_token = bot_token
        .ok_or_else(|| anyhow::anyhow!("missing SLACK_BOT_TOKEN or OPENAGENT_SLACK_BOT_TOKEN"))?;

    let socket_path = std::env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| "data/sockets/slack.sock".to_string());

    let logs_dir = std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    if let Err(e) = sdk_rust::setup_otel("slack", &logs_dir) {
        eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}");
    }

    let mut server = McpLiteServer::new(make_tools(), "ready");
    let event_tx = server.event_sender();

    let state = Arc::new(SlackState::new(event_tx));
    register_handlers(&mut server, Arc::clone(&state));

    let client: Arc<SlackHyperClient> = Arc::new(SlackClient::new(SlackClientHyperConnector::new()?));
    let bot_token_value: SlackApiTokenValue = bot_token.clone().into();
    let token = SlackApiToken::new(bot_token_value);

    let session = client.open_session(&token);
    match session.auth_test().await {
        Ok(auth) => {
            *state.bot_user_id.lock().expect("bot_user_id poisoned") = auth.user_id.value().to_string();
            *state.team_id.lock().expect("team_id poisoned") = auth.team_id.value().to_string();
            *state.client.lock().expect("client poisoned") = Some(client.clone());
            *state.bot_token.lock().expect("bot_token poisoned") = Some(token.clone());
            state.started.store(true, Ordering::Release);
            state.connected.store(true, Ordering::Release);
            state.authorized.store(true, Ordering::Release);
            state.set_error("");
            info!("slack.auth_ok");
        }
        Err(e) => {
            state.set_error(&e.to_string());
            state.emit_connection_status();
            return Err(e).context("slack auth_test failed");
        }
    }
    state.emit_connection_status();

    state.started.store(true, Ordering::Release);
    info!(socket = %socket_path, "slack.start");

    let slack_handle = if let Some(app_tok) = app_token {
        let listener_env: Arc<SlackHyperListenerEnvironment> = Arc::new(
            SlackClientEventsListenerEnvironment::new(client.clone())
                .with_user_state(Arc::clone(&state)),
        );

        let callbacks = SlackSocketModeListenerCallbacks::new()
            .with_push_events(push_events_handler);

        let app_token_value: SlackApiTokenValue = app_tok.into();
        let app_token = SlackApiToken::new(app_token_value);

        let socket_listener = SlackClientSocketModeListener::new(
            &SlackClientSocketModeConfig::new(),
            listener_env,
            callbacks,
        );

        Some(tokio::spawn(async move {
            if let Err(e) = socket_listener.listen_for(&app_token).await {
                error!(error = %e, "slack.socket_mode.listen_error");
                return;
            }
            let _exit_code = socket_listener.serve().await;
        }))
    } else {
        warn!("SLACK_APP_TOKEN not set — service will not receive messages");
        None
    };

    let serve_result = server.serve(&socket_path).await;

    if let Some(h) = slack_handle {
        h.abort();
    }

    if let Err(e) = serve_result {
        warn!(error = %e, "mcp.server.exit");
    }

    Ok(())
}
