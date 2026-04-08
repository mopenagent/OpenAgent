//! TTS adapter for in-channel voice synthesis.
//!
//! `TtsManager` wraps the TTS service (port 9004, MCP-lite) so channel
//! implementations can synthesize audio without knowing the wire protocol.

use anyhow::Result;

use crate::config::TtsConfig;

/// Calls the TTS service to synthesize text into audio bytes.
pub struct TtsManager {
    config: TtsConfig,
}

impl TtsManager {
    pub fn new(config: &TtsConfig) -> Result<Self> {
        if !config.enabled {
            anyhow::bail!("TTS is not enabled in config");
        }
        Ok(Self { config: config.clone() })
    }

    /// Synthesize `text` and return raw audio bytes (WAV/OGG).
    ///
    /// Calls `tts.synthesize` on the TTS service (TCP :9004) via MCP-lite.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        use serde_json::json;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpStream;

        let mut stream = TcpStream::connect("127.0.0.1:9004").await
            .map_err(|e| anyhow::anyhow!("TTS service unreachable: {e}"))?;

        let req = json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "type": "tool.call",
            "tool": "tts.synthesize",
            "params": {
                "text": text,
                "voice": self.config.voice,
                "speed": self.config.speed,
                "language": self.config.language,
            }
        });

        let mut frame = serde_json::to_string(&req)?;
        frame.push('\n');
        stream.write_all(frame.as_bytes()).await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let resp: serde_json::Value = serde_json::from_str(line.trim())?;

        if let Some(err) = resp.get("error").filter(|v| !v.is_null()) {
            anyhow::bail!("TTS error: {err}");
        }

        // Result is base64-encoded audio bytes.
        let b64 = resp["result"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("TTS: missing result field"))?;

        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64)
            .map_err(|e| anyhow::anyhow!("TTS base64 decode: {e}"))?;
        Ok(bytes)
    }
}
