//! Provider stubs — satisfy channel implementations that reference zeroclaw's
//! provider types (`Provider` trait, `ChatMessage`, `sanitize_api_error`).
//!
//! OpenAgent uses `autoagents-llm` for LLM calls, not these types.
//! The stubs compile channel code without enabling the features.

/// A single chat message (role + content).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
}

/// Sanitize an API error string (remove auth keys, etc).
/// Stub — returns the string unchanged; no sensitive data to scrub in stubs.
pub fn sanitize_api_error(err: &str) -> String {
    err.to_string()
}

/// Provider trait — stub; not used at runtime in OpenAgent.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, messages: &[ChatMessage]) -> anyhow::Result<String>;
}

pub mod compatible {
    use super::{ChatMessage, Provider};

    pub enum AuthStyle {
        Bearer,
    }

    /// Minimal OpenAI-compatible provider stub.
    pub struct OpenAiCompatibleProvider {
        pub base_url: String,
        pub api_key: String,
        pub model: String,
    }

    impl OpenAiCompatibleProvider {
        pub fn new(
            base_url: impl Into<String>,
            api_key: impl Into<String>,
            model: impl Into<String>,
            _auth_style: AuthStyle,
        ) -> Self {
            Self {
                base_url: base_url.into(),
                api_key: api_key.into(),
                model: model.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for OpenAiCompatibleProvider {
        async fn complete(&self, _messages: &[ChatMessage]) -> anyhow::Result<String> {
            anyhow::bail!("OpenAiCompatibleProvider stub — not implemented in OpenAgent")
        }
    }
}
