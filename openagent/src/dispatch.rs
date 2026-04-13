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
use crate::channels::ChannelHandle;
use crate::config::TtsConfig;
use crate::guard::GuardDb;
use crate::service::ServiceManager;
use crate::guard::scrub;

/// Max concurrent agent.step calls — keeps the Pi from thrashing.
const MAX_CONCURRENT: usize = 4;

/// channel.send / typing_start timeouts (ms).
const SEND_TIMEOUT_MS: u64 = 10_000;

pub async fn run(
    manager: Arc<ServiceManager>,
    guard_db: GuardDb,
    agent_ctx: Arc<AgentContext>,
    channel_handle: ChannelHandle,
    tts_cfg: TtsConfig,
) {
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
        let event_name = event.get("event").and_then(Value::as_str).unwrap_or("");

        // Handle incoming WhatsApp calls: reject + send a TTS voice note back.
        if event_name == "whatsapp.call.received" {
            if let Some(data) = event.get("data") {
                let call_id = data.get("call_id").and_then(Value::as_str).unwrap_or("").to_string();
                let chat_id = data.get("chat_id").and_then(Value::as_str).unwrap_or("").to_string();
                if !call_id.is_empty() && !chat_id.is_empty() {
                    let mgr = Arc::clone(&manager);
                    let tts = tts_cfg.clone();
                    tokio::spawn(async move {
                        handle_call(mgr, tts, call_id, chat_id).await;
                    });
                }
            }
            continue;
        }

        if event_name != "message.received" {
            debug!(event_name, "dispatch.event.ignored");
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
        let ch = channel_handle.clone();
        let tts = tts_cfg.clone();

        tokio::spawn(async move {
            handle_message(mgr, sem, gdb, ctx, ch, tts, session_id, channel, sender, content_raw, artifact_path).await;
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
    channel_handle: ChannelHandle,
    tts_cfg: TtsConfig,
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
    // WhatsApp uses the Go service tool; all other platforms use the in-process ChannelHandle.
    {
        let platform = platform_from_channel(&channel);
        if platform == "whatsapp" {
            let mgr = Arc::clone(&manager);
            let chat_id = channel.trim_start_matches("whatsapp://").to_string();
            tokio::spawn(async move {
                let _ = mgr
                    .call_tool("whatsapp.typing_start", json!({"chat_id": chat_id}), SEND_TIMEOUT_MS)
                    .await;
            });
        } else {
            let ch = channel_handle.clone();
            let addr = channel.clone();
            tokio::spawn(async move {
                let _ = ch.typing_start(&addr).await;
            });
        }
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
        debug!(session_id, "dispatch.agent.empty_response — not sending");
        return;
    }

    info!(
        session_id,
        channel,
        response_len = response_text.len(),
        "dispatch.sending"
    );

    // Route the reply to the appropriate platform.
    let platform = platform_from_channel(&channel);
    let send_result = if platform == "whatsapp" {
        // WhatsApp uses its own MCP-lite tool (Go service) — chat_id extracted from URI.
        let chat_id = channel.trim_start_matches("whatsapp://");

        // Use TTS only when the user explicitly says "speak" (works for both text and voice notes).
        // Default for all inputs — including voice notes — is plain text.
        // Falls back to plain text if synthesis fails or the tts service is unavailable.
        let wants_tts = tts_cfg.enabled
            && content.split_whitespace().any(|w| w.eq_ignore_ascii_case("speak"));
        if wants_tts {
            let tts_params = json!({
                "text":     response_text,
                "voice":    tts_cfg.voice,
                "speed":    tts_cfg.speed,
                "language": tts_cfg.language,
            });
            match manager.call_tool("tts.synthesize", tts_params, 60_000).await {
                Ok(payload) => {
                    let v: Value = serde_json::from_str(&payload).unwrap_or_default();
                    let audio_path = v.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                    if audio_path.is_empty() {
                        warn!(session_id, "dispatch.tts.empty_path — falling back to text");
                        manager
                            .call_tool(
                                "whatsapp.send_text",
                                json!({"chat_id": chat_id, "text": response_text}),
                                SEND_TIMEOUT_MS,
                            )
                            .await
                            .map(|_| ())
                    } else {
                        info!(session_id, audio_path, "dispatch.tts.synthesized — sending voice note");
                        manager
                            .call_tool(
                                "whatsapp.send_media",
                                json!({"chat_id": chat_id, "file_path": audio_path}),
                                SEND_TIMEOUT_MS,
                            )
                            .await
                            .map(|_| ())
                    }
                }
                Err(e) => {
                    warn!(session_id, error = %e, "dispatch.tts.unavailable — falling back to text");
                    manager
                        .call_tool(
                            "whatsapp.send_text",
                            json!({"chat_id": chat_id, "text": response_text}),
                            SEND_TIMEOUT_MS,
                        )
                        .await
                        .map(|_| ())
                }
            }
        } else {
            manager
                .call_tool(
                    "whatsapp.send_text",
                    json!({"chat_id": chat_id, "text": response_text}),
                    SEND_TIMEOUT_MS,
                )
                .await
                .map(|_| ())
        }
    } else {
        // All other platforms go through the in-process ChannelHandle (no TCP hop).
        channel_handle.send(&channel, &response_text).await
    };

    if let Err(e) = send_result {
        error!(session_id, channel, error = %e, "dispatch.channel.send.error");
    }
}

/// Handle an incoming WhatsApp voice/video call.
/// Synthesizes a short "unavailable" voice note via TTS and sends it back,
/// falling back to a plain text message if TTS is disabled or unavailable.
async fn handle_call(
    manager: Arc<ServiceManager>,
    tts_cfg: TtsConfig,
    _call_id: String,
    chat_id: String,
) {
    const UNAVAILABLE_MSG: &str =
        "Sorry, I can't take calls right now. Please send me a voice note or text instead.";

    if tts_cfg.enabled {
        let tts_params = json!({
            "text":     UNAVAILABLE_MSG,
            "voice":    tts_cfg.voice,
            "speed":    tts_cfg.speed,
            "language": tts_cfg.language,
        });
        if let Ok(payload) = manager.call_tool("tts.synthesize", tts_params, 60_000).await {
            let v: Value = serde_json::from_str(&payload).unwrap_or_default();
            let audio_path = v.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            if !audio_path.is_empty() {
                let _ = manager
                    .call_tool(
                        "whatsapp.send_media",
                        json!({"chat_id": chat_id, "file_path": audio_path}),
                        SEND_TIMEOUT_MS,
                    )
                    .await;
                return;
            }
        }
    }

    // TTS disabled or failed — send plain text fallback.
    let _ = manager
        .call_tool(
            "whatsapp.send_text",
            json!({"chat_id": chat_id, "text": UNAVAILABLE_MSG}),
            SEND_TIMEOUT_MS,
        )
        .await;
}
