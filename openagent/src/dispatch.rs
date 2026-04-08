/// Dispatch loop — bridges inbound channel events to the in-process agent and back.
///
/// Subscribes to the `ServiceManager` event bus.  For each `message.received`
/// event it:
///   1. Extracts `content`, `channel` (address URI), and `sender` from the event.
///   2. Derives a stable `session_id = "{channel}:{sender}"`.
///   3. Calls `guard.check` — blocked messages are dropped silently; guard down → fail open.
///   4. Fires `channel.typing_start` (best-effort, no await on result).
///   5. Calls `agent::handlers::handle_step` in-process (no TCP hop).
///   6. Sends the response text back via `channel.send`.
///
/// A semaphore caps concurrent in-flight steps at `MAX_CONCURRENT` to keep
/// memory and CPU pressure manageable on low-power hardware (Pi).
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::agent::handlers::{handle_step, AgentContext};
use crate::guard::GuardDb;
use crate::service::ServiceManager;
use crate::guard::scrub;

/// Max concurrent agent.step calls — keeps the Pi from thrashing.
const MAX_CONCURRENT: usize = 4;

/// channel.send / typing_start timeouts (ms).
const SEND_TIMEOUT_MS: u64 = 10_000;

pub async fn run(manager: Arc<ServiceManager>, guard_db: GuardDb, agent_ctx: Arc<AgentContext>) {
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

        let content_raw = data
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let artifact_path = data
            .get("artifact_path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Drop events that have neither text content nor an audio artifact.
        if content_raw.is_none() && artifact_path.is_none() {
            debug!("dispatch.event.empty_content — skipping");
            continue;
        }

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

        // Session key = the channel URI (platform://chatID).
        // For 1:1 chats the channel already uniquely identifies the conversation —
        // appending the sender would duplicate the chatID (e.g. "wa://id:id").
        // For group chats the channel is the group ID, which is also correct.
        let session_id = channel.clone();

        info!(
            session_id,
            channel,
            sender,
            content_len = content_raw.as_deref().map(str::len).unwrap_or(0),
            has_audio = artifact_path.is_some(),
            "dispatch.message.received"
        );

        let mgr = Arc::clone(&manager);
        let sem = Arc::clone(&semaphore);
        let gdb = guard_db.clone();
        let ctx = Arc::clone(&agent_ctx);

        tokio::spawn(async move {
            handle_message(mgr, sem, gdb, ctx, session_id, channel, sender, content_raw, artifact_path).await;
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
    guard_db: GuardDb,
    agent_ctx: Arc<AgentContext>,
    session_id: String,
    channel: String,
    sender: String,
    content_raw: Option<String>,
    artifact_path: Option<String>,
) {
    // Acquire slot — back-pressures at MAX_CONCURRENT.
    let _permit = match semaphore.acquire().await {
        Ok(p) => p,
        Err(_) => return, // semaphore closed (shutdown)
    };

    // ---- STT: transcribe audio artifact if present --------------------------
    // Audio messages from WhatsApp arrive with artifact_path set and content="".
    // Call stt.transcribe inline here; the HTTP-path SttLayer is separate.
    let content = if let Some(ref path) = artifact_path {
        match manager
            .call_tool("stt.transcribe", json!({"audio_path": path}), 120_000)
            .await
        {
            Ok(payload) => {
                let v: Value = serde_json::from_str(&payload).unwrap_or_default();
                let transcript = v
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if transcript.is_empty() {
                    warn!(session_id, audio_path = path, "dispatch.stt.empty_transcript — skipping");
                    return;
                }
                info!(session_id, audio_path = path, transcript_len = transcript.len(), "dispatch.stt.transcribed");
                transcript
            }
            Err(e) => {
                warn!(session_id, audio_path = path, error = %e, "dispatch.stt.unavailable — skipping audio message");
                return;
            }
        }
    } else {
        // Plain text — content_raw is guaranteed Some here (checked before spawn).
        content_raw.unwrap_or_default()
    };

    // ---- Guard check --------------------------------------------------------
    // Inline GuardDb lookup (direct SQLite, no TCP).
    // Same fail-open policy as GuardLayer: a DB error logs a warning but
    // does not drop the message.
    let platform = platform_from_channel(&channel);
    {
        let gdb      = guard_db.clone();
        let plat_s   = platform.to_string();
        let sender_s = sender.clone();
        match tokio::task::spawn_blocking(move || gdb.check(&plat_s, &sender_s)).await {
            Ok(Ok((allowed, reason))) => {
                if allowed {
                    info!(session_id, platform, sender, reason, "dispatch.guard.allowed");
                } else {
                    info!(session_id, platform, sender, reason, "dispatch.guard.blocked — dropping");
                    return;
                }
            }
            Ok(Err(e)) => {
                warn!(session_id, error = %e, "dispatch.guard.db_error — failing open");
            }
            Err(e) => {
                warn!(session_id, error = %e, "dispatch.guard.spawn_error — failing open");
            }
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

    // Call handle_step in-process (no TCP hop — agent logic lives here now).
    let step_params = json!({
        "session_id": session_id,
        "user_input": content,
    });

    let ctx_clone = Arc::clone(&agent_ctx);
    let params_clone = step_params.clone();
    let response_text = match tokio::task::spawn_blocking(move || handle_step(params_clone, ctx_clone)).await {
        Ok(Ok(raw)) => {
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            v.get("response_text")
                .and_then(Value::as_str)
                .unwrap_or(&raw)
                .to_string()
        }
        Ok(Err(e)) => {
            error!(session_id, error = %e, "dispatch.agent.step.error");
            return;
        }
        Err(e) => {
            error!(session_id, error = %e, "dispatch.agent.step.panic");
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
