//! Tool definitions and MCP-lite handler registration for the STT service.

use crate::handlers::handle_transcribe;
use crate::metrics::SttTelemetry;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::{Arc, Mutex};
use whisper_rs::WhisperContext;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "stt.transcribe".to_string(),
        description: concat!(
            "Transcribe an audio file to text using Whisper (ggml-small). ",
            "Accepts any format ffmpeg can decode: WAV, OGG/Opus, MP3, M4A, WebM, FLAC. ",
            "Returns the transcript text, model name, and processing duration."
        )
        .to_string(),
        params: json!({
            "type": "object",
            "properties": {
                "audio_path": {
                    "type": "string",
                    "description": "Path to the audio file on disk."
                },
                "language": {
                    "type": "string",
                    "description": "ISO 639-1 language code (e.g. 'en', 'es'). Omit for auto-detect."
                }
            },
            "required": ["audio_path"]
        }),
    }]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    ctx: Arc<Mutex<WhisperContext>>,
    tel: Arc<SttTelemetry>,
) {
    let ctx_clone = Arc::clone(&ctx);
    let tel_clone = Arc::clone(&tel);
    server.register_tool("stt.transcribe", move |params| {
        handle_transcribe(params, Arc::clone(&ctx_clone), Arc::clone(&tel_clone))
    });
}
