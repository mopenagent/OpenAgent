//! Tool handler implementations: discord.send_message and discord.edit_message.
//!
//! Each handler wires all four OTEL pillars via DiscordTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — DiscordTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent + tool/channel tags

use crate::metrics::{edit_err, edit_ok, elapsed_ms, send_err, send_ok, DiscordTelemetry};
use crate::state::DiscordState;
use anyhow::{Context as _, Result};
use opentelemetry::KeyValue;
use serde_json::Value;
use serenity::{
    builder::EditMessage,
    model::id::{ChannelId, MessageId},
};
use std::{sync::Arc, time::Instant};
use tokio::runtime::Handle;
use tracing::{error, info, info_span};

pub fn handle_send_message(
    params: Value,
    state: Arc<DiscordState>,
    tel: Arc<DiscordTelemetry>,
) -> Result<String> {
    let channel_id = params["channel_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?
        .to_string();
    let text = params["text"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("text is required"))?
        .to_string();

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = DiscordTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "discord.send_message"),
            KeyValue::new("channel_id", channel_id.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "discord.send_message",
        channel_id = %channel_id,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(channel_id = %channel_id, "discord.send_message start");

    let http = state.http.lock().expect("http poisoned").clone();
    let http = http.ok_or_else(|| anyhow::anyhow!("discord not connected"))?;
    let cid: u64 = channel_id.parse().context("invalid channel_id")?;

    let t_start = Instant::now();
    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(ChannelId::new(cid).say(&*http, &text))
    });
    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics ──────────────────────────────────────
    match result {
        Ok(msg) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "ok");
            info!(channel_id = %channel_id, duration_ms, "discord.send_message ok");
            tel.record(&send_ok(&channel_id, duration_ms));
            Ok(serde_json::json!({
                "ok":         true,
                "id":         msg.id.to_string(),
                "channel_id": msg.channel_id.to_string(),
            })
            .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            error!(channel_id = %channel_id, duration_ms, error = %e, "discord.send_message error");
            state.set_error(&e.to_string());
            state.emit_connection_status();
            tel.record(&send_err(&channel_id, duration_ms));
            Err(anyhow::anyhow!("{e}"))
        }
    }
}

pub fn handle_edit_message(
    params: Value,
    state: Arc<DiscordState>,
    tel: Arc<DiscordTelemetry>,
) -> Result<String> {
    let channel_id = params["channel_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?
        .to_string();
    let message_id = params["message_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("message_id is required"))?
        .to_string();
    let text = params["text"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("text is required"))?
        .to_string();

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = DiscordTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "discord.edit_message"),
            KeyValue::new("channel_id", channel_id.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "discord.edit_message",
        channel_id = %channel_id,
        message_id = %message_id,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(channel_id = %channel_id, message_id = %message_id, "discord.edit_message start");

    let http = state.http.lock().expect("http poisoned").clone();
    let http = http.ok_or_else(|| anyhow::anyhow!("discord not connected"))?;
    let cid: u64 = channel_id.parse().context("invalid channel_id")?;
    let mid: u64 = message_id.parse().context("invalid message_id")?;

    let t_start = Instant::now();
    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(ChannelId::new(cid).edit_message(
            &*http,
            MessageId::new(mid),
            EditMessage::new().content(&text),
        ))
    });
    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics ──────────────────────────────────────
    match result {
        Ok(msg) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "ok");
            info!(channel_id = %channel_id, message_id = %message_id, duration_ms, "discord.edit_message ok");
            tel.record(&edit_ok(&channel_id, &message_id, duration_ms));
            Ok(serde_json::json!({
                "ok":         true,
                "id":         msg.id.to_string(),
                "channel_id": msg.channel_id.to_string(),
            })
            .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            error!(channel_id = %channel_id, message_id = %message_id, duration_ms, error = %e, "discord.edit_message error");
            state.set_error(&e.to_string());
            state.emit_connection_status();
            tel.record(&edit_err(&channel_id, &message_id, duration_ms));
            Err(anyhow::anyhow!("{e}"))
        }
    }
}
