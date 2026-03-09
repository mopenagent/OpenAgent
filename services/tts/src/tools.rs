//! Tool definitions and MCP-lite handler registration for the TTS service.

use crate::handlers::{handle_synthesize, handle_synthesize_bytes};
use crate::metrics::TtsTelemetry;
use kokoros::tts::koko::TTSKoko;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "tts.synthesize".to_string(),
            description: concat!(
                "Synthesize speech from text using Kokoro TTS. ",
                "Writes WAV to data/artifacts/tts/. ",
                "Returns path, sample_rate, and format."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "text":     { "type": "string",  "description": "Text to speak" },
                    "voice":    { "type": "string",  "description": "Voice style (e.g. af_sarah, af_nicole). Blend: af_sarah.4+af_nicole.6" },
                    "speed":    { "type": "number",  "description": "Speech rate multiplier (default 1.0)" },
                    "language": { "type": "string",  "description": "Language code (default en-us)" }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "tts.synthesize_bytes".to_string(),
            description: concat!(
                "Synthesize speech to base64-encoded f32 little-endian PCM. ",
                "Use for streaming playback without writing a file."
            )
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
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    tts: Arc<Mutex<TTSKoko>>,
    tel: Arc<TtsTelemetry>,
    out_dir: String,
) {
    let tts_s = Arc::clone(&tts);
    let tel_s = Arc::clone(&tel);
    let dir_s = out_dir.clone();
    server.register_tool("tts.synthesize", move |params| {
        handle_synthesize(params, Arc::clone(&tts_s), Arc::clone(&tel_s), dir_s.clone())
    });

    let tts_b = Arc::clone(&tts);
    let tel_b = Arc::clone(&tel);
    server.register_tool("tts.synthesize_bytes", move |params| {
        handle_synthesize_bytes(params, Arc::clone(&tts_b), Arc::clone(&tel_b))
    });
}
