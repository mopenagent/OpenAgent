//! STT service — MCP-lite wrapper for Whisper via whisper-rs (whisper.cpp).
//!
//! Tool: `stt.transcribe` — transcribes any audio file to text.
//!
//! Audio decoding is delegated to the `ffmpeg` system binary so every format
//! WhatsApp, Discord, or Telegram can produce (OGG/Opus, MP3, M4A, WebM, WAV)
//! is handled without bundling Rust codec crates.  `ffmpeg` converts the input
//! to raw 16 kHz mono f32 PCM which Whisper requires.
//!
//! # Environment variables
//!
//! - `OPENAGENT_SOCKET_PATH` — Unix socket (default: `data/sockets/stt.sock`)
//! - `OPENAGENT_STT_MODEL`   — GGML model path (default: `data/models/whisper-ggml-small.bin`)
//! - `OPENAGENT_LOGS_DIR`    — Directory for OTLP trace files (default: `logs`)
//!
//! # Runtime dependencies
//!
//! `ffmpeg` must be on `$PATH`.  Install with:
//!   - macOS: `brew install ffmpeg`
//!   - Pi/Debian: `sudo apt install ffmpeg`
//!
//! # Model download
//!
//! ```sh
//! mkdir -p data/models
//! curl -L -o data/models/whisper-ggml-small.bin \
//!   https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin
//! ```
//!
//! # Abort
//!
//! Panics if the WhisperContext mutex is poisoned — indicates a bug in
//! inference code.  All other errors are returned as tool.result errors.

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Context as _;
use sdk_rust::{setup_otel, McpLiteServer, ToolDefinition};
use std::{
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};
use tracing::{info, info_span, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const DEFAULT_SOCKET_PATH: &str = "data/sockets/stt.sock";
const DEFAULT_MODEL_PATH: &str = "data/models/whisper-ggml-small.bin";
const WHISPER_SAMPLE_RATE: u32 = 16_000;

// ---------------------------------------------------------------------------
// Audio decoding via ffmpeg subprocess
// ---------------------------------------------------------------------------

/// Decode any audio file to raw 16 kHz mono f32 PCM using `ffmpeg`.
///
/// # Errors
///
/// Returns an error if `ffmpeg` is not found, decoding fails, or the output
/// byte count is not a multiple of 4.
fn decode_audio_ffmpeg(path: &str) -> anyhow::Result<Vec<f32>> {
    let output = Command::new("ffmpeg")
        .args([
            "-i",
            path,
            "-ar",
            &WHISPER_SAMPLE_RATE.to_string(),
            "-ac",
            "1",
            "-f",
            "f32le",
            "-", // stdout
        ])
        // suppress ffmpeg progress to stderr of the parent process
        .stderr(std::process::Stdio::null())
        .output()
        .context("ffmpeg not found — install with: brew install ffmpeg / apt install ffmpeg")?;

    if !output.status.success() {
        anyhow::bail!("ffmpeg exited with status {}", output.status);
    }

    let bytes = output.stdout;
    if bytes.len() % 4 != 0 {
        anyhow::bail!(
            "ffmpeg output length {} is not a multiple of 4",
            bytes.len()
        );
    }

    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    Ok(samples)
}

// ---------------------------------------------------------------------------
// Tool handler
// ---------------------------------------------------------------------------

/// Transcribe an audio file with Whisper.
///
/// Runs entirely inside `block_in_place` (ffmpeg subprocess + sync inference).
fn handle_transcribe(
    params: serde_json::Value,
    ctx: Arc<Mutex<WhisperContext>>,
) -> anyhow::Result<String> {
    let audio_path = params["audio_path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("audio_path is required"))?
        .to_string();

    let language = params["language"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("en")
        .to_string();

    let span = info_span!("stt.transcribe", path = %audio_path, lang = %language);
    let _enter = span.enter();
    let wall = Instant::now();

    let transcript = tokio::task::block_in_place(|| -> anyhow::Result<String> {
        // Step 1: decode audio to f32 PCM via ffmpeg
        let samples = decode_audio_ffmpeg(&audio_path)
            .with_context(|| format!("failed to decode {audio_path}"))?;

        if samples.is_empty() {
            return Ok(String::new());
        }

        // Step 2: create per-call inference state from the shared context
        let state = {
            let guard = ctx.lock().expect("whisper ctx poisoned");
            guard.create_state().map_err(|e| anyhow::anyhow!("whisper state: {e:?}"))?
        };

        // Step 3: configure and run Whisper inference
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some(&language));
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);
        p.set_print_special(false);

        // state.full takes ownership of params — reborrow on each call
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
    })?;

    let elapsed_ms = wall.elapsed().as_millis();
    info!(
        chars = transcript.len(),
        duration_ms = elapsed_ms,
        "stt.transcribe.ok"
    );

    Ok(serde_json::json!({
        "text":     transcript,
        "model":    "ggml-small",
        "duration_ms": elapsed_ms,
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logs_dir =
        std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());

    if let Err(e) = setup_otel("stt", &logs_dir) {
        eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}");
    }

    let socket_path =
        std::env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    let model_path =
        std::env::var("OPENAGENT_STT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());

    // Warn if ffmpeg is not on PATH — transcription will fail at runtime otherwise.
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        warn!("ffmpeg not found on PATH — stt.transcribe will fail until installed");
    }

    info!(socket = %socket_path, model = %model_path, "stt.start");

    // Load Whisper model once and keep warm (WhisperContext is ~244 MB RSS for small).
    let ctx = WhisperContext::new_with_params(
        &model_path,
        WhisperContextParameters::default(),
    )
    .map_err(|e| anyhow::anyhow!("failed to load whisper model {model_path:?}: {e:?}"))?;

    let ctx = Arc::new(Mutex::new(ctx));

    let tools = vec![ToolDefinition {
        name: "stt.transcribe".to_string(),
        description: "Transcribe an audio file to text using Whisper (ggml-small). \
                      Accepts any format ffmpeg can decode: WAV, OGG/Opus, MP3, M4A, WebM, FLAC. \
                      Returns the transcript text."
            .to_string(),
        params: serde_json::json!({
            "type": "object",
            "properties": {
                "audio_path": {
                    "type": "string",
                    "description": "Path to the audio file on disk."
                },
                "language": {
                    "type": "string",
                    "description": "ISO 639-1 code (e.g. 'en', 'es'). Omit for auto-detect."
                }
            },
            "required": ["audio_path"]
        }),
    }];

    let mut server = McpLiteServer::new(tools, "ready");

    let ctx_for_tool = Arc::clone(&ctx);
    server.register_tool("stt.transcribe", move |params| {
        handle_transcribe(params, Arc::clone(&ctx_for_tool))
    });

    server.serve(&socket_path).await?;
    Ok(())
}
