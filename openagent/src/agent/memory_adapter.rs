//! HybridMemoryAdapter — STM + LTM MemoryProvider for the agent.
//!
//! STM: SlidingWindowMemory (autoagents-core, Drop strategy), window 40 messages.
//! LTM: memory.search via ToolRouter (gracefully skipped if unavailable).

use async_trait::async_trait;
use autoagents_core::agent::memory::{MemoryProvider, MemoryType, SlidingWindowMemory};
use autoagents_llm::{
    chat::{ChatMessage, ChatMessageBuilder, ChatRole},
    error::LLMError,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::agent::tool_router::ToolRouter;

pub const DEFAULT_STM_WINDOW: usize = 40;
const LTM_RECALL_LIMIT: usize = 5;

#[derive(Clone, Debug)]
pub struct HybridMemoryAdapter {
    stm: SlidingWindowMemory,
    window_size: usize,
    session_id: String,
    stm_dir: PathBuf,
    router: Arc<ToolRouter>,
}

impl HybridMemoryAdapter {
    pub fn new(
        session_id: &str,
        window_size: usize,
        stm_dir: PathBuf,
        router: Arc<ToolRouter>,
    ) -> Self {
        Self {
            stm: SlidingWindowMemory::new(window_size),
            window_size,
            session_id: session_id.to_string(),
            stm_dir,
            router,
        }
    }
}

#[async_trait]
impl MemoryProvider for HybridMemoryAdapter {
    async fn remember(&mut self, message: &ChatMessage) -> Result<(), LLMError> {
        if self.stm.size() >= self.window_size {
            let oldest = self.stm.export().into_iter().next();
            if let Some(evicted) = oldest {
                if let Err(e) =
                    dump_messages_to_md(&self.stm_dir, &self.session_id, &[evicted], "eviction")
                        .await
                {
                    warn!(session_id = %self.session_id, error = %e, "stm eviction dump failed");
                }
            }
        }
        self.stm.remember(message).await
    }

    async fn recall(&self, query: &str, limit: Option<usize>) -> Result<Vec<ChatMessage>, LLMError> {
        let stm_messages = self.stm.recall(query, limit).await?;

        let ltm_messages = if !query.is_empty() {
            recall_ltm(&self.router, query, LTM_RECALL_LIMIT).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut combined = ltm_messages;
        combined.extend(stm_messages);
        Ok(combined)
    }

    async fn clear(&mut self) -> Result<(), LLMError> {
        let messages = self.stm.export();
        if !messages.is_empty() {
            if let Err(e) =
                dump_messages_to_md(&self.stm_dir, &self.session_id, &messages, "clear").await
            {
                warn!(session_id = %self.session_id, error = %e, "stm clear dump failed");
            }
        }
        self.stm.clear().await
    }

    fn needs_summary(&self) -> bool { self.stm.needs_summary() }
    fn mark_for_summary(&mut self) { self.stm.mark_for_summary(); }
    fn replace_with_summary(&mut self, summary: String) { self.stm.replace_with_summary(summary); }
    fn memory_type(&self) -> MemoryType { MemoryType::Custom }
    fn size(&self) -> usize { self.stm.size() }

    fn clone_box(&self) -> Box<dyn MemoryProvider> {
        Box::new(self.clone())
    }

    fn id(&self) -> Option<String> { Some(self.session_id.clone()) }

    fn export(&self) -> Vec<ChatMessage> { self.stm.export() }

    fn preload(&mut self, data: Vec<ChatMessage>) -> bool { self.stm.preload(data) }
}

async fn recall_ltm(router: &ToolRouter, query: &str, limit: usize) -> Result<Vec<ChatMessage>, LLMError> {
    let params = json!({ "query": query, "store": "memory" });

    let raw = match router.call("memory.search", &params).await {
        Ok(r) => r,
        Err(e) => {
            debug!(error = %e, "ltm recall skipped — memory service unavailable");
            return Ok(Vec::new());
        }
    };

    let items: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
    let messages = items
        .into_iter()
        .take(limit)
        .filter_map(|item| {
            let content = item.get("content")?.as_str()?.to_string();
            if content.is_empty() { return None; }
            let role = parse_role_from_metadata(item.get("metadata"));
            Some(ChatMessageBuilder::new(role).content(&content).build())
        })
        .collect();

    Ok(messages)
}

fn parse_role_from_metadata(metadata: Option<&serde_json::Value>) -> ChatRole {
    metadata
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|m| m.get("role").and_then(|r| r.as_str()).map(str::to_string))
        .as_deref()
        .map(role_from_str)
        .unwrap_or(ChatRole::User)
}

fn role_from_str(s: &str) -> ChatRole {
    match s {
        "assistant" => ChatRole::Assistant,
        "system" => ChatRole::System,
        _ => ChatRole::User,
    }
}

async fn dump_messages_to_md(
    stm_dir: &PathBuf,
    session_id: &str,
    messages: &[ChatMessage],
    reason: &str,
) -> std::io::Result<()> {
    tokio::fs::create_dir_all(stm_dir).await?;

    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let file_path = stm_dir.join(format!("{ts_ms}_{reason}.md"));

    let mut md = format!(
        "# STM Dump — {session_id}\nts_unix_ms: {ts_ms}\nreason: {reason}\ncount: {}\n",
        messages.len()
    );

    for (i, msg) in messages.iter().enumerate() {
        let role = role_label(&msg.role);
        md.push_str(&format!("\n## Turn {}\nrole: {role}\n---\n{}\n", i + 1, msg.content));
    }

    tokio::fs::write(&file_path, md).await
}

fn role_label(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_adapter(window: usize) -> HybridMemoryAdapter {
        let router = Arc::new(ToolRouter::new(HashMap::new(), PathBuf::from("data")));
        HybridMemoryAdapter::new("test-sess", window, PathBuf::from("/tmp/stm-test"), router)
    }

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessageBuilder::new(ChatRole::User).content(content).build()
    }

    fn assistant_msg(content: &str) -> ChatMessage {
        ChatMessageBuilder::new(ChatRole::Assistant).content(content).build()
    }

    #[tokio::test]
    async fn remember_and_recall_within_window() {
        let mut mem = make_adapter(10);
        mem.remember(&user_msg("hello")).await.unwrap();
        mem.remember(&assistant_msg("hi there")).await.unwrap();
        let recalled = mem.recall("", None).await.unwrap();
        assert_eq!(recalled.len(), 2);
        assert_eq!(recalled[0].content, "hello");
    }

    #[tokio::test]
    async fn evicts_when_window_full() {
        let mut mem = make_adapter(3);
        mem.remember(&user_msg("a")).await.unwrap();
        mem.remember(&user_msg("b")).await.unwrap();
        mem.remember(&user_msg("c")).await.unwrap();
        mem.remember(&user_msg("d")).await.unwrap();
        let recalled = mem.recall("", None).await.unwrap();
        assert_eq!(recalled.len(), 3);
        assert_eq!(recalled[0].content, "b");
    }

    #[tokio::test]
    async fn clear_empties_stm() {
        let mut mem = make_adapter(10);
        mem.remember(&user_msg("keep this")).await.unwrap();
        assert_eq!(mem.size(), 1);
        mem.clear().await.unwrap();
        assert_eq!(mem.size(), 0);
    }

    #[test]
    fn role_parsing_falls_back_to_user() {
        assert!(matches!(parse_role_from_metadata(None), ChatRole::User));
    }

    #[test]
    fn role_parsing_extracts_assistant() {
        let meta = serde_json::json!("{\"role\":\"assistant\"}");
        assert!(matches!(parse_role_from_metadata(Some(&meta)), ChatRole::Assistant));
    }
}
