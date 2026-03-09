//! STT service — MCP-lite wrapper for Whisper via whisper-rs (whisper.cpp).
//!
//! Tool: `stt.transcribe` — transcribes any audio file to text.
//!
//! # Environment variables
//!
//! - `OPENAGENT_SOCKET_PATH` — Unix socket (default: `data/sockets/stt.sock`)
//! - `OPENAGENT_STT_MODEL`   — GGML model path (default: `data/models/whisper-ggml-small.bin`)
//! - `OPENAGENT_LOGS_DIR`    — Directory for OTLP log/trace files (default: `logs`)
//!
//! # Runtime dependencies
//!
//! `ffmpeg` must be on `$PATH`. Install with:
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

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod audio;
mod handlers;
mod metrics;
mod tools;

use anyhow::Context as _;
use metrics::SttTelemetry;
use sdk_rust::{setup_otel, McpLiteServer};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};
use tracing::{info, warn};
use whisper_rs::{WhisperContext, WhisperContextParameters};

const DEFAULT_SOCKET_PATH: &str = "data/sockets/stt.sock";
const DEFAULT_MODEL_PATH: &str = "data/models/whisper-ggml-small.bin";
const DEFAULT_LOGS_DIR: &str = "logs";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logs_dir = std::env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());

    if let Err(e) = setup_otel("stt", &logs_dir) {
        eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}");
    }

    let socket_path =
        std::env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let model_path =
        std::env::var("OPENAGENT_STT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());

    // Metrics writer (Pillar: Metrics)
    let tel = Arc::new(SttTelemetry::new(&logs_dir).context("failed to init stt telemetry")?);

    // Warn if ffmpeg is not on PATH — transcription will fail at runtime otherwise.
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        warn!("ffmpeg not found on PATH — stt.transcribe will fail until installed");
    }

    info!(socket = %socket_path, model = %model_path, "stt.start");

    // Load Whisper model once and keep warm (~244 MB RSS for small).
    let ctx = WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
        .map_err(|e| anyhow::anyhow!("failed to load whisper model {model_path:?}: {e:?}"))?;

    let ctx = Arc::new(Mutex::new(ctx));

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, ctx, tel);

    server.serve(&socket_path).await?;
    Ok(())
}
