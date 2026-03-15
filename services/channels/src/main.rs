//! Omnibus Channels Service — MCP-lite daemon.
//!
//! Boot sequence:
//!   1. Load `.env` via dotenvy (best-effort)
//!   2. Init OTEL
//!   3. Load `config/channels.toml` (env interpolated)
//!   4. Build `ChannelRegistry` from enabled platforms
//!   5. Obtain `event_tx` from `McpLiteServer`
//!   6. Spawn per-channel listener tasks → push `message.received` events
//!   7. Register MCP-lite tool handlers
//!   8. `server.serve(socket_path).await`
//!
//! Tool surface:
//!   - `channel.send`          — send a message to a ChannelAddress
//!   - `channel.update_draft`  — update a streaming draft
//!   - `channel.finalize_draft`— finalize a draft with Markdown
//!   - `channel.cancel_draft`  — cancel a draft
//!   - `channel.react`         — add an emoji reaction
//!   - `channel.typing_start`  — send typing indicator
//!   - `channel.typing_stop`   — stop typing indicator
//!   - `channel.list`          — list enabled channels

mod adapter;
mod address;
mod config;
mod registry;

use std::sync::Arc;

use sdk_rust::{MetricsWriter, OutboundEvent};
use tracing::{error, info, warn};
use zeroclaw::channels::SendMessage;

use address::ChannelAddress;
use registry::ChannelRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 0. Install rustls crypto provider (ring) before any TLS connection is made.
    // rustls 0.23 requires an explicit provider; without this, Discord/Telegram TLS panics.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ok() — safe to ignore if already installed by another crate

    // 1. Load .env (best-effort — missing file is not an error)
    dotenvy::dotenv().ok();

    // 2. OTEL
    let logs_dir = std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    let _otel_guard = sdk_rust::setup_otel("channels", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"msg\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();

    let socket_path = std::env::var("OPENAGENT_SOCKET_PATH")
        .unwrap_or_else(|_| "data/sockets/channels.sock".to_string());

    info!(socket = %socket_path, "channels.start");

    // 3. Load config
    let config_path = std::env::var("OPENAGENT_CHANNELS_CONFIG")
        .unwrap_or_else(|_| "config/channels.toml".to_string());

    let cfg = match config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "channels.config.fallback: using defaults");
            config::ChannelsConfig::default()
        }
    };

    // 4. Build registry
    let metrics = Arc::new(MetricsWriter::new(&logs_dir, "channels")
        .unwrap_or_else(|e| { warn!(error=%e, "metrics.init.failed"); panic!("metrics init failed") }));
    let registry = Arc::new(ChannelRegistry::build(&cfg, Arc::clone(&metrics))?);

    info!(count = registry.len(), "channels.registry.built");

    // 5. Build MCP-lite server
    let mut server = sdk_rust::McpLiteServer::new(make_tools(), "ready");

    // 6. Spawn listeners before serve()
    let event_tx = server.event_sender();
    registry.spawn_listeners(event_tx.clone());

    // 7. Register tool handlers
    // sdk-rust register_tool expects sync Fn. We bridge to async via Handle::current().block_on().

    let reg = Arc::clone(&registry);
    server.register_tool("channel.send", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = params
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            let msg = SendMessage::new(content, addr.chat_id()).in_thread(addr.thread_id());
            ch.send(&msg).await?;
            Ok(format!("sent to {address_str}"))
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.update_draft", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let message_id = str_param(&params, "message_id")?;
            let content = str_param(&params, "content")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.update_draft(addr.chat_id(), &message_id, &content).await?;
            Ok(format!("draft updated on {address_str}"))
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.finalize_draft", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let message_id = str_param(&params, "message_id")?;
            let content = str_param(&params, "content")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.finalize_draft(addr.chat_id(), &message_id, &content).await?;
            Ok(format!("draft finalized on {address_str}"))
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.cancel_draft", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let message_id = str_param(&params, "message_id")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.cancel_draft(addr.chat_id(), &message_id).await?;
            Ok(format!("draft cancelled on {address_str}"))
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.react", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let message_id = str_param(&params, "message_id")?;
            let emoji = str_param(&params, "emoji")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.add_reaction(addr.chat_id(), &message_id, &emoji).await?;
            Ok(format!("reaction {emoji} added on {address_str}"))
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.typing_start", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.start_typing(addr.chat_id()).await?;
            Ok("typing started".to_string())
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.typing_stop", move |params| {
        let reg = Arc::clone(&reg);
        tokio::runtime::Handle::current().block_on(async move {
            let address_str = str_param(&params, "address")?;
            let addr = ChannelAddress::parse(&address_str)
                .map_err(|e| anyhow::anyhow!("invalid address: {e}"))?;
            let ch = reg
                .get(addr.platform())
                .ok_or_else(|| anyhow::anyhow!("no channel: {}", addr.platform()))?;
            ch.stop_typing(addr.chat_id()).await?;
            Ok("typing stopped".to_string())
        })
    });

    let reg = Arc::clone(&registry);
    server.register_tool("channel.list", move |_params| {
        let names: Vec<String> = reg.all().iter().map(|c| c.name().to_string()).collect();
        Ok(serde_json::to_string(&names).unwrap_or_else(|_| "[]".into()))
    });

    // Push initial channel.status event
    let names: Vec<String> = registry.all().iter().map(|c| c.name().to_string()).collect();
    let _ = event_tx.send(OutboundEvent::new(
        "channel.status",
        serde_json::json!({ "channels": names, "status": "ready" }),
    ));

    // 8. Serve
    if let Err(e) = server.serve(&socket_path).await {
        error!(error = %e, "channels.serve.exit");
    }

    Ok(())
}

/// Extract a required string param from params map.
fn str_param(params: &serde_json::Value, key: &str) -> anyhow::Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing param: {key}"))
}

fn make_tools() -> Vec<sdk_rust::ToolDefinition> {
    use serde_json::json;
    vec![
        sdk_rust::ToolDefinition {
            name: "channel.send".into(),
            description: "Send a message to a platform address (e.g. telegram://bot/chat_id).".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address": { "type": "string", "description": "ChannelAddress URI" },
                    "content": { "type": "string", "description": "Message text" }
                },
                "required": ["address", "content"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.update_draft".into(),
            description: "Update an in-progress streaming draft message.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address":    { "type": "string" },
                    "message_id": { "type": "string" },
                    "content":    { "type": "string" }
                },
                "required": ["address", "message_id", "content"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.finalize_draft".into(),
            description: "Finalize a draft with the complete response (applies Markdown).".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address":    { "type": "string" },
                    "message_id": { "type": "string" },
                    "content":    { "type": "string" }
                },
                "required": ["address", "message_id", "content"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.cancel_draft".into(),
            description: "Cancel and remove a draft message.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address":    { "type": "string" },
                    "message_id": { "type": "string" }
                },
                "required": ["address", "message_id"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.react".into(),
            description: "Add an emoji reaction to a message.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address":    { "type": "string" },
                    "message_id": { "type": "string" },
                    "emoji":      { "type": "string", "description": "Unicode emoji, e.g. \"👀\"" }
                },
                "required": ["address", "message_id", "emoji"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.typing_start".into(),
            description: "Send a typing indicator on the given channel address.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address": { "type": "string" }
                },
                "required": ["address"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.typing_stop".into(),
            description: "Stop the typing indicator on the given channel address.".into(),
            params: json!({
                "type": "object",
                "properties": {
                    "address": { "type": "string" }
                },
                "required": ["address"]
            }),
        },
        sdk_rust::ToolDefinition {
            name: "channel.list".into(),
            description: "List enabled channel platform names.".into(),
            params: json!({ "type": "object", "properties": {}, "required": [] }),
        },
    ]
}
