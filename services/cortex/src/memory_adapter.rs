//! HybridMemoryAdapter — STM + LTM MemoryProvider for Cortex.
//!
//! # Architecture
//!
//! ```text
//! HybridMemoryAdapter
//!   ├── STM: SlidingWindowMemory (autoagents-core, Drop strategy)
//!   │         window_size messages in-process; eviction → markdown file
//!   └── LTM: memory.sock via ToolRouter
//!             memory.search (ltm) on recall
//! ```
//!
//! # Hooks on SlidingWindowMemory
//!
//! AutoAgents `SlidingWindowMemory` does not expose an eviction callback.
//! We intercept it manually: before calling `stm.remember()`, if
//! `stm.size() >= window_size` the oldest message (first in `export()`) is
//! captured and appended to a markdown file before AutoAgents pops it.
//!
//! `clear()` dumps the entire in-process window to markdown then delegates.
//!
//! # STM markdown format
//!
//! Files land at `{stm_dir}/{unix_ms}_{reason}.md`:
//!
//! ```markdown
//! # STM Dump — {session_id}
//! ts_unix: {unix_ms}
//! reason: eviction | clear
//! count: {n}
//!
//! ## Turn 1
//! role: user
//! ---
//! {content}
//!
//! ## Turn 2
//! role: assistant
//! ---
//! {content}
//! ```
//!
//! # LTM recall
//!
//! `recall(query, _)` queries `memory.search` with `store=ltm` via the ToolRouter.
//! Results are parsed into `ChatMessage`s (role from stored metadata, content as-is)
//! and prepended to the STM window so context reads chronologically.
//! If memory.sock is unavailable the LTM step is silently skipped.
//!
//! An empty query skips LTM (no cost when loading a fresh turn with no user input yet).

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

use crate::tool_router::ToolRouter;

/// Default STM window: 40 messages (~20 user/assistant turn pairs).
pub const DEFAULT_STM_WINDOW: usize = 40;

/// Maximum number of LTM hits merged into a single recall.
const LTM_RECALL_LIMIT: usize = 5;

// ── HybridMemoryAdapter ───────────────────────────────────────────────────────

/// STM + LTM MemoryProvider.
///
/// STM is AutoAgents `SlidingWindowMemory` with `TrimStrategy::Drop`.
/// LTM recall hits `memory.search` on `memory.sock`; writes are handled
/// by the offline compaction pipeline (not on every `remember()`).
#[derive(Clone)]
pub struct HybridMemoryAdapter {
    stm: SlidingWindowMemory,
    /// Cached window_size so we can detect the eviction boundary ourselves.
    window_size: usize,
    session_id: String,
    /// Where to write STM markdown dumps: `data/stm/{session_id}/`.
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
    /// Store a message in the STM window.
    ///
    /// Intercepts SlidingWindowMemory eviction: when the window is full the
    /// oldest message is written to a markdown file before AutoAgents pops it.
    async fn remember(&mut self, message: &ChatMessage) -> Result<(), LLMError> {
        if self.stm.size() >= self.window_size {
            // Capture the message about to be evicted (index 0 = oldest).
            let oldest = self.stm.export().into_iter().next();
            if let Some(evicted) = oldest {
                if let Err(e) =
                    dump_messages_to_md(&self.stm_dir, &self.session_id, &[evicted], "eviction")
                        .await
                {
                    warn!(
                        session_id = %self.session_id,
                        error = %e,
                        "stm eviction dump failed"
                    );
                }
            }
        }
        self.stm.remember(message).await
    }

    /// Return STM (most recent window) merged with LTM results.
    ///
    /// LTM hits are prepended (older semantic context) so the combined list
    /// reads chronologically: `[ltm…, stm…]`.
    ///
    /// Pass the current `user_input` as `query` for semantic LTM retrieval.
    /// An empty `query` skips LTM entirely.
    async fn recall(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<ChatMessage>, LLMError> {
        let stm_messages = self.stm.recall(query, limit).await?;

        let ltm_messages = if !query.is_empty() {
            recall_ltm(&self.router, query, LTM_RECALL_LIMIT)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // LTM first (background context), STM last (recent window).
        let mut combined = ltm_messages;
        combined.extend(stm_messages);
        Ok(combined)
    }

    /// Dump entire STM to markdown, then clear the window.
    async fn clear(&mut self) -> Result<(), LLMError> {
        let messages = self.stm.export();
        if !messages.is_empty() {
            if let Err(e) =
                dump_messages_to_md(&self.stm_dir, &self.session_id, &messages, "clear").await
            {
                warn!(
                    session_id = %self.session_id,
                    error = %e,
                    "stm clear dump failed"
                );
            }
        }
        self.stm.clear().await
    }

    // Delegate summary lifecycle hooks to SlidingWindowMemory.
    fn needs_summary(&self) -> bool {
        self.stm.needs_summary()
    }
    fn mark_for_summary(&mut self) {
        self.stm.mark_for_summary();
    }
    fn replace_with_summary(&mut self, summary: String) {
        self.stm.replace_with_summary(summary);
    }

    fn memory_type(&self) -> MemoryType {
        MemoryType::Custom
    }

    fn size(&self) -> usize {
        self.stm.size()
    }

    fn clone_box(&self) -> Box<dyn MemoryProvider> {
        Box::new(self.clone())
    }

    fn id(&self) -> Option<String> {
        Some(self.session_id.clone())
    }

    /// Export the full STM window (for diary/compaction hooks).
    fn export(&self) -> Vec<ChatMessage> {
        self.stm.export()
    }

    /// Hydrate the STM window from persisted history (session resume).
    fn preload(&mut self, data: Vec<ChatMessage>) -> bool {
        self.stm.preload(data)
    }
}

// ── LTM retrieval via memory.sock ─────────────────────────────────────────────

/// Query the memory service for semantically related LTM context.
///
/// Calls `memory.search` with `store=ltm` via the ToolRouter.
/// Gracefully returns empty if memory.sock is down or returns an error.
async fn recall_ltm(
    router: &ToolRouter,
    query: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>, LLMError> {
    let params = json!({ "query": query, "store": "ltm" });

    let raw = match router.call("memory.search", &params).await {
        Ok(r) => r,
        Err(e) => {
            debug!(error = %e, "ltm recall skipped — memory.sock unavailable or errored");
            return Ok(Vec::new());
        }
    };

    let items: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
    let messages = items
        .into_iter()
        .take(limit)
        .filter_map(|item| {
            let content = item.get("content")?.as_str()?.to_string();
            if content.is_empty() {
                return None;
            }
            let role = parse_role_from_metadata(item.get("metadata"));
            Some(ChatMessageBuilder::new(role).content(&content).build())
        })
        .collect();

    Ok(messages)
}

/// Extract a `ChatRole` from the JSON metadata string stored alongside an LTM document.
///
/// Memory service stores metadata as a JSON *string* (e.g. `"{\"role\":\"user\"}"`).
/// Falls back to `ChatRole::User` if the field is absent or unparseable.
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

// ── STM markdown persistence ───────────────────────────────────────────────────

/// Write `messages` to a timestamped markdown file under `stm_dir`.
///
/// File name: `{unix_ms}_{reason}.md` (eviction | clear).
/// Creates `stm_dir` on first write.
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
        md.push_str(&format!(
            "\n## Turn {}\nrole: {role}\n---\n{}\n",
            i + 1,
            msg.content
        ));
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessageBuilder::new(ChatRole::User).content(content).build()
    }

    fn assistant_msg(content: &str) -> ChatMessage {
        ChatMessageBuilder::new(ChatRole::Assistant).content(content).build()
    }

    fn make_adapter(window: usize) -> HybridMemoryAdapter {
        let router = Arc::new(ToolRouter::new(PathBuf::from("data/sockets")));
        HybridMemoryAdapter::new("test-sess", window, PathBuf::from("/tmp/stm-test"), router)
    }

    #[tokio::test]
    async fn remember_and_recall_within_window() {
        let mut mem = make_adapter(10);
        mem.remember(&user_msg("hello")).await.unwrap();
        mem.remember(&assistant_msg("hi there")).await.unwrap();
        // empty query → no LTM, just STM
        let recalled = mem.recall("", None).await.unwrap();
        assert_eq!(recalled.len(), 2);
        assert_eq!(recalled[0].content, "hello");
        assert_eq!(recalled[1].content, "hi there");
    }

    #[tokio::test]
    async fn evicts_when_window_full() {
        let mut mem = make_adapter(3);
        mem.remember(&user_msg("a")).await.unwrap();
        mem.remember(&user_msg("b")).await.unwrap();
        mem.remember(&user_msg("c")).await.unwrap();
        // "a" will be evicted (drop written to /tmp — may fail on perms, test still passes)
        mem.remember(&user_msg("d")).await.unwrap();
        let recalled = mem.recall("", None).await.unwrap();
        assert_eq!(recalled.len(), 3);
        assert_eq!(recalled[0].content, "b");
        assert_eq!(recalled[2].content, "d");
    }

    #[tokio::test]
    async fn clear_empties_stm() {
        let mut mem = make_adapter(10);
        mem.remember(&user_msg("keep this")).await.unwrap();
        assert_eq!(mem.size(), 1);
        mem.clear().await.unwrap();
        assert_eq!(mem.size(), 0);
        assert!(mem.is_empty());
    }

    #[test]
    fn role_parsing_falls_back_to_user() {
        assert!(matches!(parse_role_from_metadata(None), ChatRole::User));
        let bad = serde_json::json!("not-json-object");
        assert!(matches!(parse_role_from_metadata(Some(&bad)), ChatRole::User));
    }

    #[test]
    fn role_parsing_extracts_assistant() {
        let meta = serde_json::json!("{\"role\":\"assistant\",\"session_id\":\"s1\"}");
        assert!(matches!(
            parse_role_from_metadata(Some(&meta)),
            ChatRole::Assistant
        ));
    }

    #[test]
    fn role_from_str_mapping() {
        assert!(matches!(role_from_str("assistant"), ChatRole::Assistant));
        assert!(matches!(role_from_str("system"), ChatRole::System));
        assert!(matches!(role_from_str("user"), ChatRole::User));
        assert!(matches!(role_from_str("other"), ChatRole::User));
    }

    #[test]
    fn size_and_is_empty() {
        let mem = make_adapter(10);
        assert!(mem.is_empty());
        assert_eq!(mem.size(), 0);
        assert_eq!(mem.id(), Some("test-sess".to_string()));
    }

    #[test]
    fn clone_box_produces_independent_clone() {
        let mem = make_adapter(5);
        let boxed = mem.clone_box();
        assert_eq!(boxed.size(), 0);
        assert_eq!(boxed.id(), Some("test-sess".to_string()));
    }

    #[test]
    fn preload_and_export_round_trip() {
        let mut mem = make_adapter(10);
        mem.preload(vec![user_msg("loaded"), assistant_msg("yes")]);
        assert_eq!(mem.size(), 2);
        let exported = mem.export();
        assert_eq!(exported[0].content, "loaded");
        assert_eq!(exported[1].content, "yes");
    }
}
