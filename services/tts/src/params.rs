//! Parsed TTS parameters and service-wide constants.

use anyhow::Result;
use serde_json::Value;

pub const DEFAULT_SOCKET_PATH: &str = "data/sockets/tts.sock";
pub const DEFAULT_MODEL_PATH: &str = "data/models/kokoro-v1.0.onnx";
pub const DEFAULT_VOICES_PATH: &str = "data/models/voices-v1.0.bin";
pub const DEFAULT_ARTIFACTS_DIR: &str = "data/artifacts/tts";
pub const DEFAULT_LOGS_DIR: &str = "logs";
pub const DEFAULT_VOICE: &str = "af_sarah.4+af_nicole.6";
pub const SAMPLE_RATE: u32 = 24_000;

/// Parsed parameters common to all synthesis tools.
#[derive(Debug)]
pub struct TtsParams {
    pub text: String,
    pub voice: String,
    pub speed: f32,
    pub lan: String,
}

impl TtsParams {
    pub fn from_value(params: &Value) -> Result<Self> {
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
