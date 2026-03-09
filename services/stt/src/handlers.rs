//! Tool handler implementations: stt.transcribe.
//!
//! Wires all four OTEL pillars via SttTelemetry:
//!   Traces  — tracing::info_span! with per-operation attributes
//!   Metrics — SttTelemetry::record() on success and error
//!   Logs    — structured tracing::{info!, warn!, error!} events on every path
//!   Baggage — attach_context() propagates remote parent + tool/language tags

use crate::audio::decode_audio_ffmpeg;
use crate::metrics::{elapsed_ms, transcribe_err, transcribe_ok, SttTelemetry};
use anyhow::Result;
use opentelemetry::KeyValue;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{error, info, info_span, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};

pub fn handle_transcribe(
    params: Value,
    ctx: Arc<Mutex<WhisperContext>>,
    tel: Arc<SttTelemetry>,
) -> Result<String> {
    let audio_path = params["audio_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("audio_path is required"))?
        .to_string();

    let language = params["language"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("en")
        .to_string();

    // ── Pillar: Baggage — attach context + remote parent span ────────────────
    let _cx_guard = SttTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("tool", "stt.transcribe"),
            KeyValue::new("language", language.clone()),
        ],
    );

    // ── Pillar: Traces ────────────────────────────────────────────────────────
    let span = info_span!(
        "stt.transcribe",
        path = %audio_path,
        lang = %language,
        duration_ms = tracing::field::Empty,
        chars = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let _enter = span.enter();

    // ── Pillar: Logs — structured event at invocation start ──────────────────
    info!(path = %audio_path, lang = %language, "stt.transcribe start");

    let t_start = Instant::now();

    let result = tokio::task::block_in_place(|| -> Result<String> {
        // Step 1: decode audio to f32 PCM via ffmpeg
        let samples = decode_audio_ffmpeg(&audio_path)
            .map_err(|e| anyhow::anyhow!("failed to decode {audio_path}: {e}"))?;

        if samples.is_empty() {
            warn!(path = %audio_path, "stt.transcribe: empty audio — returning empty transcript");
            return Ok(String::new());
        }

        // Step 2: create per-call inference state from the shared context
        let state = {
            let guard = ctx.lock().expect("whisper ctx poisoned");
            guard
                .create_state()
                .map_err(|e| anyhow::anyhow!("whisper state: {e:?}"))?
        };

        // Step 3: configure and run Whisper inference
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some(&language));
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);
        p.set_print_special(false);

        let mut state = state;
        state
            .full(p, &samples)
            .map_err(|e| anyhow::anyhow!("whisper inference: {e:?}"))?;

        // Step 4: collect segment text
        let n = state
            .full_n_segments()
            .map_err(|e| anyhow::anyhow!("full_n_segments: {e:?}"))?;

        let mut text = String::new();
        for i in 0..n {
            if let Ok(seg) = state.full_get_segment_text(i) {
                text.push_str(&seg);
            }
        }

        Ok(text.trim().to_string())
    });

    let duration_ms = elapsed_ms(t_start);

    // ── Pillar: Traces + Logs + Metrics — record outcome ─────────────────────
    match &result {
        Ok(transcript) => {
            let chars = transcript.len();
            span.record("duration_ms", duration_ms);
            span.record("chars", chars as i64);
            span.record("status", "ok");
            // Logs
            info!(
                path = %audio_path, lang = %language,
                duration_ms, chars,
                "stt.transcribe ok"
            );
            // Metrics
            tel.record(&transcribe_ok(&audio_path, &language, duration_ms, chars));

            Ok(serde_json::json!({
                "text":        transcript,
                "model":       "ggml-small",
                "duration_ms": duration_ms,
            })
            .to_string())
        }
        Err(e) => {
            span.record("duration_ms", duration_ms);
            span.record("status", "error");
            // Logs
            error!(
                path = %audio_path, lang = %language,
                duration_ms, error = %e,
                "stt.transcribe error"
            );
            // Metrics
            tel.record(&transcribe_err(&audio_path, &language, duration_ms));

            Err(anyhow::anyhow!("{e}"))
        }
    }
}
