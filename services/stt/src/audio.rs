//! Audio decoding via ffmpeg subprocess.

use anyhow::Context as _;

pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Decode any audio file to raw 16 kHz mono f32 PCM using `ffmpeg`.
///
/// # Errors
///
/// Returns an error if `ffmpeg` is not found, decoding fails, or the output
/// byte count is not a multiple of 4.
pub fn decode_audio_ffmpeg(path: &str) -> anyhow::Result<Vec<f32>> {
    let output = std::process::Command::new("ffmpeg")
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
        // suppress ffmpeg progress output
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

    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}
