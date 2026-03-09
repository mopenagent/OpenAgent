//! Tool handler implementations: slack.send_message.
//!
//! Wires all four OTEL pillars via SlackTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — SlackTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent + tool/channel tags

use crate::metrics::{elapsed_ms, send_err, send_ok, SlackTelemetry};
use crate::state::SlackState;
use anyhow::Result;
use opentelemetry::KeyValue;
use rvstruct::ValueStruct;
use serde_json::Value;
use slack_morphism::prelude::*;
use std::sync::{atomic::Ordering, Arc};
use std::time::Instant;
use tokio::runtime::Handle;
use tracing::{error, info, info_span};

pub fn handle_send_message(
    params: Value,
    state: Arc<SlackState>,
    tel: Arc<SlackTelemetry>,
) -> Result<String> {
    let channel_id = params["channel_id"].as_str().unwrap_or("").to_string();
    let text = params["text"].as_str().unwrap_or("").to_string();

    if channel_id.is_empty() {
        anyhow::bail!("channel_id is required");
    }
    if text.is_empty() {
        anyhow::bail!("text is required");
    }
    if !state.started.load(Ordering::Acquire) {
        anyhow::bail!("slack runtime is not started");
    }

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = SlackTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "slack.send_message"),
            KeyValue::new("channel_id", channel_id.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "slack.send_message",
        channel_id = %channel_id,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(channel_id = %channel_id, "slack.send_message start");

    let client = state.client.lock().expect("client poisoned").clone();
    let token = state.bot_token.lock().expect("bot_token poisoned").clone();
    let (client, token) = match (client, token) {
        (Some(c), Some(t)) => (c, t),
        _ => anyhow::bail!("slack not connected"),
    };

    let req = SlackApiChatPostMessageRequest::new(
        channel_id.clone().into(),
        SlackMessageContent::new().with_text(text.into()),
    );

    let t_start = Instant::now();
    let result: Result<SlackApiChatPostMessageResponse, slack_morphism::errors::SlackClientError> =
        tokio::task::block_in_place(|| {
            Handle::current().block_on(client.open_session(&token).chat_post_message(&req))
        });
    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics ──────────────────────────────────────
    match result {
        Ok(resp) => {
            let ts_str = resp.ts.value().to_string();
            span.record("duration_ms", duration_ms);
            span.record("status", "ok");
            info!(channel_id = %channel_id, duration_ms, ts = %ts_str, "slack.send_message ok");
            tel.record(&send_ok(&channel_id, duration_ms));
            Ok(serde_json::json!({
                "ok":         true,
                "channel_id": channel_id,
                "ts":         ts_str,
            })
            .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            error!(channel_id = %channel_id, duration_ms, error = %e, "slack.send_message error");
            state.set_error(&e.to_string());
            state.emit_connection_status();
            tel.record(&send_err(&channel_id, duration_ms));
            Err(anyhow::anyhow!("{e}"))
        }
    }
}
