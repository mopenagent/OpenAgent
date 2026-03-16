/// Dispatch loop — bridges inbound channel events to Cortex and back.
///
/// Subscribes to the `ServiceManager` event bus.  For each `message.received`
/// event it:
///   1. Extracts `content`, `channel` (address URI), and `sender` from the event.
///   2. Derives a stable `session_id = "{channel}:{sender}"`.
///   3. Calls `guard.check` — blocked messages are dropped silently; guard down → fail open.
///   4. Fires `channel.typing_start` (best-effort, no await on result).
///   5. Calls `cortex.step` with the message and session id.
///   6. Sends the response text back via `channel.send`.
///
/// A semaphore caps concurrent in-flight steps at `MAX_CONCURRENT` to keep
/// memory and CPU pressure manageable on low-power hardware (Pi).
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::manager::ServiceManager;
use crate::scrub;

/// Max concurrent cortex.step calls — keeps the Pi from thrashing.
const MAX_CONCURRENT: usize = 4;

/// Cortex tool call timeout (ms).  Cortex itself has a 300 s LLM timeout;
/// we give an extra 30 s of headroom on top.
const STEP_TIMEOUT_MS: u64 = 330_000;

/// channel.send / typing_start timeouts (ms).
const SEND_TIMEOUT_MS: u64 = 10_000;

pub async fn run(manager: Arc<ServiceManager>) {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let mut event_rx = manager.subscribe_events();

    info!("dispatch.start");

    loop {
        let event = match event_rx.recv().await {
            Ok(e) => e,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "dispatch.events.lagged");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                info!("dispatch.events.closed — exiting");
                break;
            }
        };

        // Only handle message.received events.
        if event.get("type").and_then(Value::as_str) != Some("event") {
            continue;
        }
        if event.get("event").and_then(Value::as_str) != Some("message.received") {
            debug!(event_type = ?event.get("event"), "dispatch.event.ignored");
            continue;
        }

        let data = match event.get("data") {
            Some(d) => d.clone(),
            None => {
                warn!("dispatch.event.no_data");
                continue;
            }
        };

        let content = match data.get("content").and_then(Value::as_str) {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => {
                debug!("dispatch.event.empty_content — skipping");
                continue;
            }
        };

        let channel = match data.get("channel").and_then(Value::as_str) {
            Some(c) => c.to_string(),
            None => {
                warn!("dispatch.event.no_channel — skipping");
                continue;
            }
        };

        let sender = data
            .get("sender")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let session_id = format!("{channel}:{sender}");

        info!(
            session_id,
            channel,
            sender,
            content_len = content.len(),
            "dispatch.message.received"
        );

        let mgr = Arc::clone(&manager);
        let sem = Arc::clone(&semaphore);

        tokio::spawn(async move {
            handle_message(mgr, sem, session_id, channel, sender, content).await;
        });
    }
}

/// Extract the platform name from a channel URI scheme.
/// `discord://guild/channel` → `"discord"`, `slack://team/channel` → `"slack"`.
/// Falls back to `"unknown"` if the URI has no scheme.
fn platform_from_channel(channel: &str) -> &str {
    channel.split("://").next().unwrap_or("unknown")
}

async fn handle_message(
    manager: Arc<ServiceManager>,
    semaphore: Arc<Semaphore>,
    session_id: String,
    channel: String,
    sender: String,
    content: String,
) {
    // Acquire slot — back-pressures at MAX_CONCURRENT.
    let _permit = match semaphore.acquire().await {
        Ok(p) => p,
        Err(_) => return, // semaphore closed (shutdown)
    };

    // ---- Guard check --------------------------------------------------------
    // Mirrors the GuardLayer in the HTTP stack — same guard.check call,
    // same fail-open behaviour when the guard service is unavailable.
    let platform = platform_from_channel(&channel);
    match manager
        .call_tool(
            "guard.check",
            json!({"platform": platform, "channel_id": sender}),
            2_000,
        )
        .await
    {
        Ok(payload) => {
            let v: Value = serde_json::from_str(&payload).unwrap_or_default();
            let allowed = v.get("allowed").and_then(Value::as_bool).unwrap_or(false);
            let reason = v
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if allowed {
                info!(session_id, platform, sender, reason, "dispatch.guard.allowed");
            } else {
                info!(session_id, platform, sender, reason, "dispatch.guard.blocked — dropping");
                return;
            }
        }
        Err(e) => {
            warn!(session_id, error = %e, "dispatch.guard.unavailable — failing open");
        }
    }

    // ---- Scrub credentials + detect injection ----------------------------------
    let ctx = format!("platform:{platform} sender:{sender}");
    let content = scrub::process(&content, &ctx);

    // Fire typing_start best-effort (don't block on it).
    {
        let mgr = Arc::clone(&manager);
        let addr = channel.clone();
        tokio::spawn(async move {
            let _ = mgr
                .call_tool(
                    "channel.typing_start",
                    json!({"address": addr}),
                    SEND_TIMEOUT_MS,
                )
                .await;
        });
    }

    // Call cortex.step.
    let step_params = json!({
        "session_id": session_id,
        "user_input": content,
    });

    let response_text = match manager
        .call_tool("cortex.step", step_params, STEP_TIMEOUT_MS)
        .await
    {
        Ok(raw) => {
            // cortex.step returns a JSON string with a `response_text` field.
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            v.get("response_text")
                .and_then(Value::as_str)
                .unwrap_or(&raw)
                .to_string()
        }
        Err(e) => {
            error!(session_id, error = %e, "dispatch.cortex.step.error");
            return;
        }
    };

    if response_text.trim().is_empty() {
        debug!(session_id, "dispatch.cortex.empty_response — not sending");
        return;
    }

    info!(
        session_id,
        channel,
        response_len = response_text.len(),
        "dispatch.sending"
    );

    // Route the reply to the appropriate send tool based on platform.
    let platform = platform_from_channel(&channel);
    let send_result = if platform == "whatsapp" {
        // WhatsApp uses its own send tool with chat_id extracted from the URI.
        let chat_id = channel.trim_start_matches("whatsapp://");
        manager
            .call_tool(
                "whatsapp.send_text",
                json!({"chat_id": chat_id, "text": response_text}),
                SEND_TIMEOUT_MS,
            )
            .await
    } else {
        // All other platforms go through the channels service.
        manager
            .call_tool(
                "channel.send",
                json!({"address": channel, "content": response_text}),
                SEND_TIMEOUT_MS,
            )
            .await
    };

    if let Err(e) = send_result {
        error!(session_id, channel, error = %e, "dispatch.channel.send.error");
    }
}
