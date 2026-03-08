//! TTS service — MCP-lite wrapper for Kokoros (Kokoro TTS in Rust).
//!
//! Tools: `tts.synthesize`, `tts.synthesize_bytes`.
//! Uses Kokoro-82M via Kokoros — local, fast, no API key.
//!
//! # Environment variables
//!
//! - `OPENAGENT_SOCKET_PATH`   — Unix socket (default: `data/sockets/tts.sock`)
//! - `OPENAGENT_TTS_MODEL`     — ONNX model path (default: `data/models/kokoro-v1.0.onnx`)
//! - `OPENAGENT_TTS_VOICES`    — Voices data path (default: `data/models/voices-v1.0.bin`)
//! - `OPENAGENT_ARTIFACTS_DIR` — Output dir for WAV files (default: `data/artifacts/tts`)
//! - `OPENAGENT_LOGS_DIR`      — Directory for OTLP trace files (default: `logs`)
//!
//! # Abort
//!
//! Panics if the TTS mutex is poisoned — indicates a bug in synthesis code.

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use kokoros::tts::koko::{TTSKoko, TTSOpts};
use mimalloc::MiMalloc;
use sdk_rust::{setup_otel, McpLiteServer, ToolDefinition};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use tracing::{info, info_span};
use uuid::Uuid;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const DEFAULT_SOCKET_PATH: &str = "data/sockets/tts.sock";
const DEFAULT_MODEL_PATH: &str = "data/models/kokoro-v1.0.onnx";
const DEFAULT_VOICES_PATH: &str = "data/models/voices-v1.0.bin";
const DEFAULT_ARTIFACTS_DIR: &str = "data/artifacts/tts";
const DEFAULT_VOICE: &str = "af_sarah.4+af_nicole.6";
const SAMPLE_RATE: u32 = 24_000;

fn model_path() -> String {
    env::var("OPENAGENT_TTS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string())
}

fn voices_path() -> String {
    env::var("OPENAGENT_TTS_VOICES").unwrap_or_else(|_| DEFAULT_VOICES_PATH.to_string())
}

fn artifacts_dir() -> String {
    env::var("OPENAGENT_ARTIFACTS_DIR").unwrap_or_else(|_| DEFAULT_ARTIFACTS_DIR.to_string())
}

/// Parsed parameters common to all synthesis tools.
struct TtsParams {
    text: String,
    voice: String,
    speed: f32,
    lan: String,
}

impl TtsParams {
    fn from_value(params: &Value) -> Result<Self> {
        let p = params
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("params must be an object"))?;

        let text = p
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        if text.is_empty() {
            return Err(anyhow::anyhow!("text is required"));
        }

        let voice = p
            .get("voice")
            .or_else(|| p.get("style"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_VOICE)
            .to_owned();
        let speed = p
            .get("speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;
        let lan = p
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("en-us")
            .to_owned();

        Ok(Self { text, voice, speed, lan })
    }
}

/// Synthesize speech to WAV file. Returns artifact path, sample rate, and format.
fn handle_synthesize(params: Value, tts: Arc<Mutex<TTSKoko>>) -> Result<String> {
    let p = TtsParams::from_value(&params)?;
    let span = info_span!("tts.synthesize", voice = %p.voice, text_len = p.text.len());
    let _enter = span.enter();

    let out_dir = artifacts_dir();
    let id = Uuid::new_v4();
    let save_path = format!("{}/{}.wav", out_dir.trim_end_matches('/'), id);

    tokio::task::block_in_place(|| {
        let mut tts = tts.lock().expect("tts mutex poisoned");
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
    })?;

    info!(path = %save_path, "tts.synthesize.ok");
    Ok(json!({
        "path": save_path,
        "sample_rate": SAMPLE_RATE,
        "format": "wav"
    })
    .to_string())
}

/// Synthesize speech to base64-encoded f32 PCM for streaming playback.
fn handle_synthesize_bytes(params: Value, tts: Arc<Mutex<TTSKoko>>) -> Result<String> {
    let p = TtsParams::from_value(&params)?;
    let span = info_span!("tts.synthesize_bytes", voice = %p.voice, text_len = p.text.len());
    let _enter = span.enter();

    let audio = tokio::task::block_in_place(|| {
        let mut tts = tts.lock().expect("tts mutex poisoned");
        tts.tts_raw_audio(&p.text, &p.lan, &p.voice, p.speed, None, None, None, None)
            .map_err(|e| anyhow::anyhow!("{e}"))
    })?;

    let bytes: Vec<u8> = audio.iter().flat_map(|s| s.to_le_bytes()).collect();
    let encoded = BASE64.encode(&bytes);

    info!(byte_len = bytes.len(), "tts.synthesize_bytes.ok");
    Ok(json!({
        "audio_base64": encoded,
        "sample_rate": SAMPLE_RATE,
        "format": "f32_le",
        "channels": 1
    })
    .to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let logs_dir = env::var("OPENAGENT_LOGS_DIR").unwrap_or_else(|_| "logs".to_string());
    let _otel_guard = match setup_otel("tts", &logs_dir) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("otel init failed (continuing without file traces): {e}");
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("tts=info".parse().unwrap()),
                )
                .try_init()
                .ok();
            None
        }
    };

    let socket_path =
        env::var("OPENAGENT_SOCKET_PATH").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let model = model_path();
    let voices = voices_path();
    fs::create_dir_all(artifacts_dir())?;

    info!(socket = %socket_path, model = %model, voices = %voices, "tts.start");

    let tts = TTSKoko::new(&model, &voices).await;
    let tts = Arc::new(Mutex::new(tts));

    let tools = vec![
        ToolDefinition {
            name: "tts.synthesize".to_string(),
            description: "Synthesize speech from text using Kokoro TTS. Writes WAV to \
                          data/artifacts/tts/. Returns path, sample_rate, and format."
                .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "text":     { "type": "string", "description": "Text to speak" },
                    "voice":    { "type": "string", "description": "Voice style (e.g. af_sarah, af_nicole, af_sky). Blend: af_sarah.4+af_nicole.6" },
                    "speed":    { "type": "number", "description": "Speech rate multiplier (default 1.0)" },
                    "language": { "type": "string", "description": "Language code (default en-us)" }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "tts.synthesize_bytes".to_string(),
            description: "Synthesize speech to base64-encoded f32 little-endian PCM. \
                          Use for streaming playback without writing a file."
                .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "text":     { "type": "string", "description": "Text to speak" },
                    "voice":    { "type": "string", "description": "Voice style" },
                    "speed":    { "type": "number", "description": "Speech rate multiplier (default 1.0)" },
                    "language": { "type": "string", "description": "Language code (default en-us)" }
                },
                "required": ["text"]
            }),
        },
    ];

    let mut server = McpLiteServer::new(tools, "ready");

    let tts_for_synthesize = Arc::clone(&tts);
    server.register_tool("tts.synthesize", move |params| {
        handle_synthesize(params, Arc::clone(&tts_for_synthesize))
    });

    let tts_for_bytes = Arc::clone(&tts);
    server.register_tool("tts.synthesize_bytes", move |params| {
        handle_synthesize_bytes(params, Arc::clone(&tts_for_bytes))
    });

    server.serve(&socket_path).await?;
    Ok(())
}
