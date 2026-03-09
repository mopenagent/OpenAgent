//! Tool handler implementations: tts.synthesize and tts.synthesize_bytes.
//!
//! Each handler wires all four OTEL pillars via TtsTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — TtsTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent + tool/voice tags

use crate::metrics::{
    elapsed_ms, synthesize_bytes_err, synthesize_bytes_ok, synthesize_err, synthesize_ok,
    TtsTelemetry,
};
use crate::params::{TtsParams, SAMPLE_RATE};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use kokoros::tts::koko::{TTSKoko, TTSOpts};
use opentelemetry::KeyValue;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, info_span};

pub fn handle_synthesize(
    params: Value,
    tts: Arc<Mutex<TTSKoko>>,
    tel: Arc<TtsTelemetry>,
    out_dir: String,
) -> Result<String> {
    let p = TtsParams::from_value(&params)?;

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = TtsTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "tts.synthesize"),
            KeyValue::new("voice", p.voice.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "tts.synthesize",
        voice = %p.voice,
        text_len = p.text.len(),
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(voice = %p.voice, text_len = p.text.len(), "tts.synthesize start");

    let save_path = {
        let id = uuid::Uuid::new_v4();
        format!("{}/{}.wav", out_dir.trim_end_matches('/'), id)
    };

    let t_start = Instant::now();
    let result = tokio::task::block_in_place(|| {
        let tts = tts.lock().expect("tts mutex poisoned");
        tts.tts(TTSOpts {
            txt: &p.text,
            lan: &p.lan,
            style_name: &p.voice,
            save_path: &save_path,
            mono: true,
            speed: p.speed,
            initial_silence: None,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))
    });
    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics ──────────────────────────────────────
    match result {
        Ok(()) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "ok");
            info!(voice = %p.voice, duration_ms, path = %save_path, "tts.synthesize ok");
            tel.record(&synthesize_ok(&p.voice, p.text.len(), duration_ms));
            Ok(json!({ "path": save_path, "sample_rate": SAMPLE_RATE, "format": "wav" })
                .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            error!(voice = %p.voice, duration_ms, error = %e, "tts.synthesize error");
            tel.record(&synthesize_err(&p.voice, p.text.len(), duration_ms));
            Err(e)
        }
    }
}

pub fn handle_synthesize_bytes(
    params: Value,
    tts: Arc<Mutex<TTSKoko>>,
    tel: Arc<TtsTelemetry>,
) -> Result<String> {
    let p = TtsParams::from_value(&params)?;

    // ── Pillar: Baggage ───────────────────────────────────────────────────────
    let _cx_guard = TtsTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "tts.synthesize_bytes"),
            KeyValue::new("voice", p.voice.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "tts.synthesize_bytes",
        voice = %p.voice,
        text_len = p.text.len(),
        byte_len = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs ─────────────────────────────────────────────────────────
    info!(voice = %p.voice, text_len = p.text.len(), "tts.synthesize_bytes start");

    let t_start = Instant::now();
    let result = tokio::task::block_in_place(|| {
        let tts = tts.lock().expect("tts mutex poisoned");
        tts.tts_raw_audio(&p.text, &p.lan, &p.voice, p.speed, None, None, None, None)
            .map_err(|e| anyhow::anyhow!("{e}"))
    });
    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics ──────────────────────────────────────
    match result {
        Ok(audio) => {
            let bytes: Vec<u8> = audio.iter().flat_map(|s| s.to_le_bytes()).collect();
            let byte_len = bytes.len();
            let encoded = BASE64.encode(&bytes);
            span.record("duration_ms", duration_ms);
            span.record("byte_len", byte_len as i64);
            span.record("status", "ok");
            info!(voice = %p.voice, duration_ms, byte_len, "tts.synthesize_bytes ok");
            tel.record(&synthesize_bytes_ok(&p.voice, p.text.len(), byte_len, duration_ms));
            Ok(json!({
                "audio_base64": encoded,
                "sample_rate":  SAMPLE_RATE,
                "format":       "f32_le",
                "channels":     1
            })
            .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            error!(voice = %p.voice, duration_ms, error = %e, "tts.synthesize_bytes error");
            tel.record(&synthesize_bytes_err(&p.voice, p.text.len(), duration_ms));
            Err(e)
        }
    }
}
