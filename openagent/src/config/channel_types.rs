//! Channel-specific config types shared across all channel implementations.
//!
//! These are inlined from zeroclaw 0.6.8's `config/schema.rs` — the subset
//! used by our channel implementations (StreamMode, TranscriptionConfig,
//! proxy helpers, WS connection helper).
//!
//! Proxy support is simplified for our Pi use case: per-channel `proxy_url`
//! overrides are supported; global proxy state is not. `ws_connect_with_proxy`
//! falls back to a plain `connect_async` when no proxy is configured.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::warn;

// ---------------------------------------------------------------------------
// StreamMode
// ---------------------------------------------------------------------------

/// Controls how the bot delivers long responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    /// Send the complete response as a single message (default).
    #[default]
    Off,
    /// Update a draft message with every flush interval.
    Partial,
    /// Split the response into separate messages at paragraph boundaries.
    #[serde(rename = "multi_message")]
    MultiMessage,
}

// ---------------------------------------------------------------------------
// STT provider sub-configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAiSttConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    /// Override base URL (e.g. for a local Whisper-compatible endpoint).
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_openai_stt_model")]
    pub model: String,
    #[serde(default)]
    pub language: Option<String>,
}

fn default_openai_stt_model() -> String {
    "whisper-1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeepgramSttConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub smart_format: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssemblyAiSttConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub language_code: Option<String>,
    #[serde(default)]
    pub speech_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoogleSttConfig {
    /// Google Cloud API key (alternative to service account credentials).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Path to Google service account credentials JSON file.
    #[serde(default)]
    pub credentials_path: Option<String>,
    #[serde(default)]
    pub language_code: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalWhisperConfig {
    /// HTTP/HTTPS endpoint URL, e.g. `"http://10.0.0.1:8001/v1/transcribe"`.
    pub url: String,
    /// Bearer token for endpoint auth. Omit for unauthenticated local endpoints.
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Max audio file size in bytes (default 25 MB).
    #[serde(default = "default_local_whisper_max_audio_bytes")]
    pub max_audio_bytes: usize,
    /// Request timeout in seconds (default 300).
    #[serde(default = "default_local_whisper_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_local_whisper_max_audio_bytes() -> usize {
    25 * 1024 * 1024
}
fn default_local_whisper_timeout_secs() -> u64 {
    300
}

// ---------------------------------------------------------------------------
// TranscriptionConfig
// ---------------------------------------------------------------------------

/// In-channel voice transcription configuration.
/// When enabled, audio messages are transcribed to text before the agent
/// processes them. OpenAgent's own `stt` service is an alternative — configure
/// that instead for a local Pi deployment to avoid cloud STT costs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Enable voice transcription within this channel.
    #[serde(default)]
    pub enabled: bool,
    /// STT provider: "groq" | "openai" | "deepgram" | "assemblyai" | "google" | "local_whisper".
    #[serde(default = "default_transcription_provider")]
    pub default_provider: String,
    /// API key for the Groq provider.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whisper API endpoint URL (Groq provider).
    #[serde(default = "default_transcription_api_url")]
    pub api_url: String,
    /// Whisper model name (Groq provider).
    #[serde(default = "default_transcription_model")]
    pub model: String,
    /// ISO-639-1 language hint (e.g. "en", "ru").
    #[serde(default)]
    pub language: Option<String>,
    /// Prompt to bias transcription toward expected vocabulary.
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Skip messages longer than this many seconds (default 300).
    #[serde(default = "default_transcription_max_duration_secs")]
    pub max_duration_secs: u64,
    #[serde(default)]
    pub openai: Option<OpenAiSttConfig>,
    #[serde(default)]
    pub deepgram: Option<DeepgramSttConfig>,
    #[serde(default)]
    pub assemblyai: Option<AssemblyAiSttConfig>,
    #[serde(default)]
    pub google: Option<GoogleSttConfig>,
    /// Local / self-hosted Whisper-compatible endpoint.
    #[serde(default)]
    pub local_whisper: Option<LocalWhisperConfig>,
    /// Also transcribe non-PTT (forwarded/regular) audio on WhatsApp.
    #[serde(default)]
    pub transcribe_non_ptt_audio: bool,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_provider: default_transcription_provider(),
            api_key: None,
            api_url: default_transcription_api_url(),
            model: default_transcription_model(),
            language: None,
            initial_prompt: None,
            max_duration_secs: default_transcription_max_duration_secs(),
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        }
    }
}

fn default_transcription_provider() -> String {
    "groq".to_string()
}
fn default_transcription_api_url() -> String {
    "https://api.groq.com/openai/v1/audio/transcriptions".to_string()
}
fn default_transcription_model() -> String {
    "whisper-large-v3-turbo".to_string()
}
fn default_transcription_max_duration_secs() -> u64 {
    300
}

// ---------------------------------------------------------------------------
// Proxy helpers
// ---------------------------------------------------------------------------
// Simplified versions of zeroclaw's proxy functions.
// No global proxy state — per-channel proxy_url is applied directly.
// On a Pi on a private LAN proxy is rarely needed; env-var proxies (HTTP_PROXY,
// HTTPS_PROXY) are still picked up by reqwest automatically.

/// Build an HTTP client for a channel.
/// If `proxy_url` is Some and non-empty, the proxy is applied to all requests.
/// Otherwise returns a plain `reqwest::Client` (which still respects HTTP_PROXY env vars).
pub fn build_channel_proxy_client(_service_key: &str, proxy_url: Option<&str>) -> reqwest::Client {
    build_client_inner(proxy_url, None, None)
}

/// Same as [`build_channel_proxy_client`] but with explicit timeout overrides.
pub fn build_channel_proxy_client_with_timeouts(
    _service_key: &str,
    proxy_url: Option<&str>,
    timeout_secs: u64,
    connect_timeout_secs: u64,
) -> reqwest::Client {
    build_client_inner(proxy_url, Some(timeout_secs), Some(connect_timeout_secs))
}

/// Apply an optional per-channel proxy URL to a `reqwest::ClientBuilder`.
pub fn apply_channel_proxy_to_builder(
    mut builder: reqwest::ClientBuilder,
    service_key: &str,
    proxy_url: Option<&str>,
) -> reqwest::ClientBuilder {
    let Some(url) = proxy_url.filter(|s| !s.is_empty()) else {
        return builder;
    };
    match reqwest::Proxy::all(url) {
        Ok(proxy) => builder = builder.proxy(proxy),
        Err(e) => warn!(service_key, proxy_url = url, "invalid proxy URL: {e}"),
    }
    builder
}

/// Build a plain runtime HTTP client (used by channels that don't have a
/// per-channel proxy but still want a managed client instance).
/// Respects `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` env vars automatically.
pub fn build_runtime_proxy_client(_service_key: &str) -> reqwest::Client {
    reqwest::Client::new()
}

fn build_client_inner(
    proxy_url: Option<&str>,
    timeout_secs: Option<u64>,
    connect_timeout_secs: Option<u64>,
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(url) = proxy_url.filter(|s| !s.is_empty()) {
        match reqwest::Proxy::all(url) {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(e) => warn!(proxy_url = url, "invalid channel proxy URL: {e}"),
        }
    }
    if let Some(t) = timeout_secs {
        builder = builder.timeout(Duration::from_secs(t));
    }
    if let Some(ct) = connect_timeout_secs {
        builder = builder.connect_timeout(Duration::from_secs(ct));
    }
    builder.build().unwrap_or_else(|e| {
        warn!("Failed to build proxy client: {e}");
        reqwest::Client::new()
    })
}

// ---------------------------------------------------------------------------
// WebSocket helper
// ---------------------------------------------------------------------------
// Simplified ws_connect_with_proxy: proxy tunnelling is not yet implemented
// for Pi deployments. Falls back to a plain connect_async; the signature
// matches zeroclaw's so the channel implementations compile unchanged.

pub type ProxiedWsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Connect a WebSocket, optionally through a proxy.
///
/// Per-channel proxy support for WS is not yet implemented (not needed on Pi).
/// The function always uses a direct connection and logs a warning if a proxy
/// URL was supplied. The return type matches zeroclaw's `ws_connect_with_proxy`
/// so channel implementations compile without changes.
pub async fn ws_connect_with_proxy(
    ws_url: &str,
    service_key: &str,
    channel_proxy_url: Option<&str>,
) -> anyhow::Result<(
    ProxiedWsStream,
    tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
)> {
    if channel_proxy_url.filter(|s| !s.is_empty()).is_some() {
        warn!(
            service_key,
            ws_url,
            "WebSocket proxy not yet implemented — connecting directly"
        );
    }
    let (stream, response) = tokio_tungstenite::connect_async(ws_url).await?;
    Ok((stream, response))
}
