use crate::config::ProviderConfig;
use anyhow::{anyhow, Result};
use autoagents_llm::backends::anthropic::Anthropic;
use autoagents_llm::backends::openai::OpenAI;
use autoagents_llm::builder::LLMBuilder;
use autoagents_llm::chat::{
    ChatMessage, ChatMessageBuilder, ChatProvider, ChatRole, StructuredOutputFormat,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone)]
pub struct StepPrompt {
    pub system_prompt: String,
    pub user_input: String,
    pub action_context: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StepOutput {
    pub content: String,
    pub provider_kind: String,
    pub model: String,
}

/// Build a boxed `LLMProvider` from a `ProviderConfig`.
///
/// Used by `handle_step` to construct the `Arc<dyn LLMProvider>` required by
/// `BaseAgent::new()`.  The resulting provider is used by the framework for memory
/// context population; the raw ReAct loop continues to use `dispatch_llm` directly.
pub fn build_llm_provider(config: &ProviderConfig) -> Result<Arc<dyn autoagents_llm::LLMProvider>> {
    match config.kind.trim() {
        "anthropic" => {
            let p = LLMBuilder::<Anthropic>::new()
                .api_key(&config.api_key)
                .base_url(&config.base_url)
                .model(&config.model)
                .timeout_seconds(config.timeout as u64)
                .max_tokens(config.max_tokens)
                .build()
                .map_err(|e| anyhow!("anthropic provider build failed: {e}"))?;
            Ok(p)
        }
        _ => {
            let api_key = if config.api_key.is_empty() { "none" } else { &config.api_key };
            let p = LLMBuilder::<OpenAI>::new()
                .api_key(api_key)
                .base_url(&config.base_url)
                .model(&config.model)
                .timeout_seconds(config.timeout as u64)
                .max_tokens(config.max_tokens)
                .build()
                .map_err(|e| anyhow!("openai provider build failed: {e}"))?;
            Ok(p)
        }
    }
}

/// Single-prompt entry point used by `CortexAgent::step()`.
pub async fn complete(provider: &ProviderConfig, prompt: &StepPrompt) -> Result<StepOutput> {
    let model_label = requested_model(provider)?;
    let messages = build_messages(prompt);
    if provider.debug_llm {
        info!(
            provider_kind = %provider.kind,
            model = %model_label,
            llm_http_request = %build_debug_request(provider, prompt),
            "cortex.llm.http.request"
        );
    }
    let content = dispatch_llm(provider, &messages, &model_label).await?;
    Ok(StepOutput {
        content,
        provider_kind: provider.kind.clone(),
        model: model_label,
    })
}

/// Multi-turn entry point used by the ReAct loop in `CortexAgent::run()`.
///
/// Accepts a pre-built message list so the loop can accumulate tool results between
/// iterations without rebuilding from a `StepPrompt` each time.
pub async fn complete_messages(
    provider: &ProviderConfig,
    messages: &[ChatMessage],
) -> Result<StepOutput> {
    let model_label = requested_model(provider)?;
    let content = dispatch_llm(provider, messages, &model_label).await?;
    Ok(StepOutput {
        content,
        provider_kind: provider.kind.clone(),
        model: model_label,
    })
}

/// Shared LLM dispatch — called by both `complete` and `complete_messages`.
///
/// Dispatches on provider kind at compile time. All OpenAI-compatible endpoints
/// (LM Studio, Ollama /v1, local servers) use the OpenAI builder.
async fn dispatch_llm(
    provider: &ProviderConfig,
    messages: &[ChatMessage],
    model_label: &str,
) -> Result<String> {
    match provider.kind.trim() {
        "anthropic" => {
            let p = LLMBuilder::<Anthropic>::new()
                .api_key(&provider.api_key)
                .base_url(&provider.base_url)
                .model(&provider.model)
                .timeout_seconds(provider.timeout as u64)
                .max_tokens(provider.max_tokens)
                .build()
                .map_err(|e| anyhow!("anthropic provider build failed: {e}"))?;
            if provider.debug_llm {
                info!(provider_kind = "anthropic", model = %model_label, "cortex.llm.call");
            }
            let mut stream = p
                .chat_stream(messages, None::<StructuredOutputFormat>)
                .await
                .map_err(|e| anyhow!("anthropic stream open failed: {e}"))?;
            let text = accumulate_stream(&mut stream).await?;
            if provider.debug_llm {
                info!(
                    provider_kind = "anthropic",
                    model = %model_label,
                    response_len = text.len(),
                    llm_response_text = %text,
                    "cortex.llm.http.response"
                );
            }
            Ok(text)
        }
        _ => {
            let api_key = if provider.api_key.is_empty() {
                "none"
            } else {
                &provider.api_key
            };
            let p = LLMBuilder::<OpenAI>::new()
                .api_key(api_key)
                .base_url(&provider.base_url)
                .model(&provider.model)
                .timeout_seconds(provider.timeout as u64)
                .max_tokens(provider.max_tokens)
                .build()
                .map_err(|e| anyhow!("openai provider build failed: {e}"))?;
            if provider.debug_llm {
                info!(provider_kind = %provider.kind, model = %model_label, "cortex.llm.call");
            }
            let mut stream = p
                .chat_stream(messages, None::<StructuredOutputFormat>)
                .await
                .map_err(|e| anyhow!("openai stream open failed: {e}"))?;
            let text = accumulate_stream(&mut stream).await?;
            if provider.debug_llm {
                info!(
                    provider_kind = %provider.kind,
                    model = %model_label,
                    response_len = text.len(),
                    llm_response_text = %text,
                    "cortex.llm.http.response"
                );
            }
            Ok(text)
        }
    }
}

fn build_messages(prompt: &StepPrompt) -> Vec<ChatMessage> {
    // ChatMessage has no system() shortcut — use ChatMessageBuilder with ChatRole::System.
    vec![
        ChatMessageBuilder::new(ChatRole::System)
            .content(&prompt.system_prompt)
            .build(),
        ChatMessage::user().content(&prompt.user_input).build(),
    ]
}

async fn accumulate_stream<S>(stream: &mut S) -> Result<String>
where
    S: futures::Stream<Item = Result<String, autoagents_llm::error::LLMError>> + Unpin,
{
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        let delta = chunk.map_err(|e| anyhow!("stream chunk error: {e}"))?;
        buf.push_str(&delta);
    }
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("provider returned empty response"));
    }
    Ok(trimmed.to_string())
}

fn requested_model(provider: &ProviderConfig) -> Result<String> {
    let model = provider.model.trim().to_string();
    if model.is_empty() {
        return Err(anyhow!("provider.model is required for Cortex Phase 1"));
    }
    // Display label normalises "openai_compat" → "openai" for logs/metrics.
    Ok(format!("{}::{}", kind_display_label(&provider.kind), model))
}

fn kind_display_label(kind: &str) -> &str {
    match kind.trim() {
        "openai" | "openai_compat" => "openai",
        "anthropic" => "anthropic",
        "ollama" => "ollama",
        other => other,
    }
}

fn build_debug_request(provider: &ProviderConfig, prompt: &StepPrompt) -> Value {
    let base_url = normalize_base_url(&provider.base_url);
    let path = match provider.kind.trim() {
        "anthropic" => "messages",
        _ => "chat/completions",
    };
    let url = if base_url.is_empty() {
        path.to_string()
    } else {
        format!("{base_url}{path}")
    };
    json!({
        "method": "POST",
        "url": url,
        "payload": {
            "model": provider.model.trim(),
            "messages": [
                {"role": "system", "content": prompt.system_prompt},
                {"role": "user", "content": prompt.user_input}
            ],
            "stream": false,
            "max_tokens": provider.max_tokens,
        }
    })
}

fn normalize_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    }
}

pub fn build_prompt_with_action_context(
    system_prompt: &str,
    user_input: &str,
    action_context: Option<String>,
) -> StepPrompt {
    StepPrompt {
        system_prompt: append_action_context(system_prompt.trim(), action_context.as_deref()),
        user_input: user_input.trim().to_string(),
        action_context,
    }
}

pub fn prompt_preview(prompt: &StepPrompt) -> Value {
    json!({
        "system_prompt_len": prompt.system_prompt.len(),
        "user_input_len": prompt.user_input.len(),
        "action_context_len": prompt.action_context.as_ref().map_or(0, String::len),
    })
}

fn append_action_context(system_prompt: &str, action_context: Option<&str>) -> String {
    let Some(action_context) = action_context.map(str::trim).filter(|v| !v.is_empty()) else {
        return system_prompt.to_string();
    };
    format!(
        concat!(
            "{system_prompt}\n\n",
            "## Available tools\n\n",
            "{action_context}\n\n",
            "To call a tool: {{\"type\":\"tool_call\",\"tool\":\"<name>\",\"arguments\":{{...}}}}\n",
            "To answer directly: {{\"type\":\"final\",\"content\":\"<answer>\"}}\n",
            "Only use tools listed above. Start your response with `{{`."
        ),
        system_prompt = system_prompt,
        action_context = action_context,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_trims_both_parts() {
        let prompt = build_prompt_with_action_context("  system  ", "  user  ", None);
        assert_eq!(prompt.system_prompt, "system");
        assert_eq!(prompt.user_input, "user");
    }

    #[test]
    fn build_prompt_appends_action_context() {
        let prompt = build_prompt_with_action_context(
            "system",
            "user",
            Some("- browser.open [browser] - Open a URL".to_string()),
        );
        assert!(prompt.system_prompt.contains("## Available tools"));
        assert!(prompt.system_prompt.contains("browser.open"));
        assert!(prompt.system_prompt.contains("Start your response with"));
    }

    #[test]
    fn requested_model_normalises_openai_compat_kind() {
        let provider = ProviderConfig {
            kind: "openai_compat".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
            model: "qwen2.5-7b-instruct".to_string(),
            timeout: 60.0,
            max_tokens: 2048,
            debug_llm: false,
        };
        assert_eq!(
            requested_model(&provider).expect("model should resolve"),
            "openai::qwen2.5-7b-instruct"
        );
    }

    #[test]
    fn requested_model_rejects_empty_model() {
        let provider = ProviderConfig {
            kind: "openai_compat".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
            model: String::new(),
            timeout: 60.0,
            max_tokens: 2048,
            debug_llm: false,
        };
        assert!(requested_model(&provider).is_err());
    }

    #[test]
    fn normalize_base_url_preserves_v1_path() {
        assert_eq!(
            normalize_base_url("http://localhost:1234/v1"),
            "http://localhost:1234/v1/"
        );
        assert_eq!(
            normalize_base_url("http://localhost:1234/v1/"),
            "http://localhost:1234/v1/"
        );
    }

    #[test]
    fn build_debug_request_uses_openai_chat_completions_endpoint() {
        let provider = ProviderConfig {
            kind: "openai_compat".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
            model: "qwen2.5-7b-instruct".to_string(),
            timeout: 60.0,
            max_tokens: 2048,
            debug_llm: true,
        };
        let prompt = StepPrompt {
            system_prompt: "system".to_string(),
            user_input: "user".to_string(),
            action_context: None,
        };
        let request = build_debug_request(&provider, &prompt);
        assert_eq!(
            request.get("url").and_then(Value::as_str),
            Some("http://localhost:1234/v1/chat/completions")
        );
    }

    #[test]
    fn build_debug_request_uses_anthropic_messages_endpoint() {
        let provider = ProviderConfig {
            kind: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            timeout: 60.0,
            max_tokens: 2048,
            debug_llm: true,
        };
        let prompt = StepPrompt {
            system_prompt: "system".to_string(),
            user_input: "user".to_string(),
            action_context: None,
        };
        let request = build_debug_request(&provider, &prompt);
        assert_eq!(
            request.get("url").and_then(Value::as_str),
            Some("https://api.anthropic.com/v1/messages")
        );
    }
}
