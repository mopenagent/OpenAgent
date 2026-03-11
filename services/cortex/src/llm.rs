use crate::config::ProviderConfig;
use anyhow::{anyhow, Result};
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget, WebConfig};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct StepPrompt {
    pub system_prompt: String,
    pub user_input: String,
}

#[derive(Debug, Clone)]
pub struct StepOutput {
    pub content: String,
    pub provider_kind: String,
    pub model: String,
}

pub async fn complete(provider: &ProviderConfig, prompt: &StepPrompt) -> Result<StepOutput> {
    let requested_model = requested_model(provider)?;
    let client = build_client(provider);
    let chat_req = ChatRequest::new(vec![ChatMessage::user(prompt.user_input.clone())])
        .with_system(prompt.system_prompt.clone());
    let chat_options = ChatOptions::default().with_max_tokens(provider.max_tokens);
    let chat_res = client
        .exec_chat(&requested_model, chat_req, Some(&chat_options))
        .await?;

    Ok(StepOutput {
        content: chat_res.into_first_text().unwrap_or_default(),
        provider_kind: provider.kind.clone(),
        model: requested_model,
    })
}

fn build_client(provider: &ProviderConfig) -> Client {
    let cfg = provider.clone();
    let resolver_cfg = cfg.clone();
    let resolver = ServiceTargetResolver::from_resolver_fn(
        move |target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
            let adapter_kind = adapter_kind_for(&resolver_cfg);
            let model_name = raw_model_name(&resolver_cfg);
            let endpoint = if resolver_cfg.base_url.trim().is_empty() {
                target.endpoint
            } else {
                Endpoint::from_owned(resolver_cfg.base_url.trim_end_matches('/').to_string())
            };
            let auth = AuthData::from_single(resolver_cfg.api_key.clone());
            Ok(ServiceTarget {
                endpoint,
                auth,
                model: ModelIden::new(adapter_kind, model_name),
            })
        },
    );

    Client::builder()
        .with_web_config(WebConfig::default().with_timeout(Duration::from_secs_f64(cfg.timeout)))
        .with_service_target_resolver(resolver)
        .build()
}

fn requested_model(provider: &ProviderConfig) -> Result<String> {
    let model = raw_model_name(provider);
    if model.is_empty() {
        return Err(anyhow!("provider.model is required for Cortex Phase 1"));
    }
    Ok(format!(
        "{}::{}",
        adapter_kind_for(provider).as_lower_str(),
        model
    ))
}

fn raw_model_name(provider: &ProviderConfig) -> String {
    provider.model.trim().to_string()
}

fn adapter_kind_for(provider: &ProviderConfig) -> AdapterKind {
    match provider.kind.as_str() {
        "anthropic" => AdapterKind::Anthropic,
        "openai" | "openai_compat" => AdapterKind::OpenAI,
        _ => AdapterKind::OpenAI,
    }
}

pub fn build_prompt(system_prompt: &str, user_input: &str) -> StepPrompt {
    StepPrompt {
        system_prompt: system_prompt.trim().to_string(),
        user_input: user_input.trim().to_string(),
    }
}

pub fn prompt_preview(prompt: &StepPrompt) -> Value {
    json!({
        "system_prompt_len": prompt.system_prompt.len(),
        "user_input_len": prompt.user_input.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_trims_both_parts() {
        let prompt = build_prompt("  system  ", "  user  ");
        assert_eq!(prompt.system_prompt, "system");
        assert_eq!(prompt.user_input, "user");
    }

    #[test]
    fn requested_model_uses_provider_namespace() {
        let provider = ProviderConfig {
            kind: "openai_compat".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
            model: "qwen2.5-7b-instruct".to_string(),
            timeout: 60.0,
            max_tokens: 2048,
        };
        assert_eq!(
            requested_model(&provider).expect("model should resolve"),
            "openai::qwen2.5-7b-instruct"
        );
    }
}
