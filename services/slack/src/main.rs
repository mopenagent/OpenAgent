//! Slack MCP-lite service.
//!
//! Connects to Slack via slack-morphism (Socket Mode + Web API) and exposes tools
//! plus event streams over a Unix Domain Socket using the MCP-lite protocol.
//!
//! # Tools
//! - `slack.status`       — service health snapshot
//! - `slack.link_state`   — connection/auth state
//! - `slack.send_message` — post a message to a channel (chat.postMessage)
//!
//! # Events (pushed to Python on change)
//! - `slack.connection.status`  — emitted on connect / disconnect / error
//! - `slack.message.received`   — emitted for every inbound user message
//!
//! # Environment variables
//! - `SLACK_BOT_TOKEN` / `OPENAGENT_SLACK_BOT_TOKEN` — required; xoxb-… token
//! - `SLACK_APP_TOKEN` / `OPENAGENT_SLACK_APP_TOKEN` — required for Socket Mode; xapp-… token
//! - `OPENAGENT_SOCKET_PATH` (default: `data/sockets/slack.sock`)
//! - `OPENAGENT_LOGS_DIR`    (default: `logs`)

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod dispatch;
mod handlers;
mod metrics;
mod state;
mod tools;

use anyhow::Context as _;
use metrics::SlackTelemetry;
use rvstruct::ValueStruct;
use sdk_rust::{setup_otel, McpLiteServer};
use slack_morphism::prelude::*;
use state::SlackState;
use std::sync::{atomic::Ordering, Arc};
use tracing::{error, info, warn};

fn tokens_from_env() -> (Option<String>, Option<String>) {
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
    let (bot_token, app_token) = tokens_from_env();
    let bot_token = bot_token.ok_or_else(|| {
        anyhow::anyhow!("missing SLACK_BOT_TOKEN or OPENAGENT_SLACK_BOT_TOKEN")
    })?;

    let socket_path = std::env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| "data/sockets/slack.sock".to_string());
    let logs_dir =
        std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    if let Err(e) = setup_otel("slack", &logs_dir) {
        eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}");
    }

    let tel = Arc::new(
        SlackTelemetry::new(&logs_dir).context("failed to init slack telemetry")?,
    );

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    let event_tx = server.event_sender();

    let state = SlackState::new(event_tx);
    tools::register_handlers(&mut server, Arc::clone(&state), tel);

    // Build the Hyper-backed Slack client and verify the bot token
    let client: Arc<SlackHyperClient> =
        Arc::new(SlackClient::new(SlackClientHyperConnector::new()?));
    let token = SlackApiToken::new(bot_token.clone().into());

    match client.open_session(&token).auth_test().await {
        Ok(auth) => {
            *state.bot_user_id.lock().expect("bot_user_id poisoned") =
                auth.user_id.value().to_string();
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

    info!(socket = %socket_path, "slack.start");

    // Start Socket Mode listener if app token is available
    let slack_handle = if let Some(app_tok) = app_token {
        let listener_env: Arc<SlackHyperListenerEnvironment> = Arc::new(
            SlackClientEventsListenerEnvironment::new(client.clone())
                .with_user_state(Arc::clone(&state)),
        );

        let callbacks =
            SlackSocketModeListenerCallbacks::new().with_push_events(dispatch::push_events_handler);

        let app_token = SlackApiToken::new(app_tok.into());

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
        warn!("SLACK_APP_TOKEN not set — service will not receive inbound messages");
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
