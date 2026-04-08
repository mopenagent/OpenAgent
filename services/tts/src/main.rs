//! TTS service — MCP-lite wrapper for Kokoros (Kokoro TTS in Rust).
//!
//! Tools: `tts.synthesize`, `tts.synthesize_bytes`.
//!
//! # Environment variables
//!
//! - `OPENAGENT_TTS_MODEL`     — ONNX model path (default: `data/models/kokoro-v1.0.onnx`)
//! - `OPENAGENT_TTS_VOICES`    — Voices data path (default: `data/models/voices-v1.0.bin`)
//! - `OPENAGENT_ARTIFACTS_DIR` — Output dir for WAV files (default: `data/artifacts/tts`)
//! - `OPENAGENT_LOGS_DIR`      — Directory for OTLP log/trace files (default: `logs`)

use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod handlers;
mod metrics;
mod params;
mod tools;

use anyhow::{Context as _, Result};
use metrics::TtsTelemetry;
use params::{
    DEFAULT_ARTIFACTS_DIR, DEFAULT_LOGS_DIR, DEFAULT_MODEL_PATH,
    DEFAULT_VOICES_PATH,
};
use sdk_rust::{setup_otel, McpLiteServer};
use std::{env, fs, sync::{Arc, Mutex}};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir =
        env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| DEFAULT_LOGS_DIR.to_string());

    let _otel_guard = setup_otel("tts", &logs_dir)
        .inspect_err(|e| eprintln!("{{\"level\":\"WARN\",\"message\":\"otel init failed\",\"error\":\"{e}\"}}"))
        .ok();
    let model_path =
        env::var("OPENAGENT_TTS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());
    let voices_path =
        env::var("OPENAGENT_TTS_VOICES").unwrap_or_else(|_| DEFAULT_VOICES_PATH.to_string());
    let out_dir =
        env::var("OPENAGENT_ARTIFACTS_DIR").unwrap_or_else(|_| DEFAULT_ARTIFACTS_DIR.to_string());

    fs::create_dir_all(&out_dir).context("failed to create artifacts dir")?;

    let tel = Arc::new(TtsTelemetry::new(&logs_dir).context("failed to init tts telemetry")?);

    info!(addr = "0.0.0.0:9004", model = %model_path, voices = %voices_path, "tts.start");

    let tts = kokoros::tts::koko::TTSKoko::new(&model_path, &voices_path).await;
    let tts = Arc::new(Mutex::new(tts));

    let mut server = McpLiteServer::new(tools::make_tools(), "ready");
    tools::register_handlers(&mut server, tts, tel, out_dir);

    server.serve_auto("0.0.0.0:9004").await?;
    Ok(())
}
