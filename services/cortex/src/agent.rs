//! CortexAgent — implements AutoAgents AgentDeriveT + AgentExecutor + AgentHooks.
//!
//! # Framework integration
//!
//! `CortexAgent` is wired as a full AutoAgents citizen:
//! - `BaseAgent::<CortexAgent, DirectAgent>::new()` in `handle_step` owns the LLM +
//!   memory.  `base_agent.run(task)` fires `on_run_start` → `execute()` →
//!   `on_run_complete`, giving us the full hook lifecycle.
//! - `AgentExecutor::execute()` IS the multi-turn ReAct loop.  It uses
//!   `context.llm()` (the provider built once at `BaseAgent::new()`) to avoid
//!   per-iteration provider rebuilds, and `context.memory()` for history.
//! - Per-iteration hooks (`on_turn_start`, `on_turn_complete`) and tool hooks
//!   (`on_tool_call`, `on_tool_start`, `on_tool_result`, `on_tool_error`) are
//!   called from inside `execute()`.  Phase 3+ will override these for diary
//!   writes and telemetry.
//!
//! # Why not the framework's TurnEngine / native tool calling
//!
//! AutoAgents' built-in `ReActAgent` executor dispatches tools through
//! `context.tools()` using the LLM's native `function_call` / `tool_use` API.
//! That requires models that reliably emit structured tool-call responses.
//! Local sub-30B models (Qwen, Llama, Mistral) do not.
//!
//! Cortex instead instructs the model to output exactly one JSON object per turn
//! (`{"type":"tool_call",...}` or `{"type":"final",...}`) and dispatches tools
//! through `ToolRouter` over UDS sockets.  This is the correct tradeoff for the
//! target hardware.  The framework's execution machinery (`TurnEngine`,
//! `ToolProcessor`) is intentionally bypassed; everything else — `BaseAgent`,
//! `MemoryProvider`, `AgentHooks` — is used as designed.

use crate::config::ProviderConfig;
use crate::diary::write_diary_entry;
use crate::llm::build_prompt_with_action_context;
use crate::response::parse_step_model_output;
use crate::tool_router::ToolRouter;
use crate::validator::maybe_validate_response;
use std::path::PathBuf;
use async_trait::async_trait;
use autoagents_core::agent::task::Task;
use autoagents_core::agent::{
    AgentDeriveT, AgentExecutor, AgentHooks, AgentOutputT, Context, ExecutorConfig, HookOutcome,
};
use autoagents_core::tool::ToolCallResult;
use autoagents_llm::{FunctionCall, ToolCall as LlmToolCall};
use autoagents_llm::chat::{ChatMessageBuilder, ChatRole, StructuredOutputFormat};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use tracing::{info, warn};

/// Discriminant for `CortexAgentError` — enables typed recovery at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CortexAgentErrorKind {
    /// LLM provider failed to open a stream, returned an error chunk, or yielded empty output.
    LlmStream,
    /// Memory recall or persist failed (`MemoryProvider::remember` / `recall`).
    Memory,
    /// Validator or JSON parser returned an error while processing model output.
    Validation,
    /// ReAct loop reached `MAX_REACT_ITERATIONS` without producing a final answer.
    IterationLimit,
    /// Model returned a response type other than `"final"` or `"tool_call"`.
    UnsupportedResponse,
    /// Catch-all for errors converted from `anyhow` context chains.
    Other,
}

/// Typed error returned by `AgentExecutor::execute()`.
///
/// Carries a `kind` discriminant for recovery logic, a human-readable `message`,
/// and an optional `cause` string preserving the underlying error chain.
///
/// `anyhow::Error` does not implement `std::error::Error`, so the cause is stored
/// as a formatted string rather than a `Box<dyn Error>` source chain.
#[derive(Debug)]
pub struct CortexAgentError {
    pub kind: CortexAgentErrorKind,
    message: String,
    cause: Option<String>,
}

impl CortexAgentError {
    pub fn new(kind: CortexAgentErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), cause: None }
    }

    pub fn with_cause(
        kind: CortexAgentErrorKind,
        message: impl Into<String>,
        cause: impl fmt::Display,
    ) -> Self {
        Self { kind, message: message.into(), cause: Some(cause.to_string()) }
    }

    // ── Constructors ──────────────────────────────────────────────────────────

    pub fn llm_stream(cause: impl fmt::Display) -> Self {
        Self::with_cause(CortexAgentErrorKind::LlmStream, "LLM stream error", cause)
    }

    pub fn memory(cause: impl fmt::Display) -> Self {
        Self::with_cause(CortexAgentErrorKind::Memory, "memory operation failed", cause)
    }

    pub fn iteration_limit(max: usize) -> Self {
        Self::new(
            CortexAgentErrorKind::IterationLimit,
            format!("react loop reached {max} iterations without a final response"),
        )
    }

    pub fn unsupported_response(kind: &str) -> Self {
        Self::new(
            CortexAgentErrorKind::UnsupportedResponse,
            format!("unsupported response type in react loop: {kind}"),
        )
    }

    // ── Predicate helpers ─────────────────────────────────────────────────────

    pub fn is_llm_error(&self) -> bool {
        self.kind == CortexAgentErrorKind::LlmStream
    }

    pub fn is_memory_error(&self) -> bool {
        self.kind == CortexAgentErrorKind::Memory
    }

    pub fn is_iteration_limit(&self) -> bool {
        self.kind == CortexAgentErrorKind::IterationLimit
    }

    pub fn is_unsupported_response(&self) -> bool {
        self.kind == CortexAgentErrorKind::UnsupportedResponse
    }
}

impl fmt::Display for CortexAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(ref cause) = self.cause {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CortexAgentError {}

impl From<anyhow::Error> for CortexAgentError {
    fn from(e: anyhow::Error) -> Self {
        Self::with_cause(CortexAgentErrorKind::Other, "internal error", format!("{e:#}"))
    }
}

/// `CortexAgentError` → `RunnableAgentError` via the `ExecutorError` variant.
impl From<CortexAgentError> for autoagents_core::agent::error::RunnableAgentError {
    fn from(e: CortexAgentError) -> Self {
        autoagents_core::agent::error::RunnableAgentError::ExecutorError(e.to_string())
    }
}

// ── ReActOutput ───────────────────────────────────────────────────────────────

/// Output from a completed `CortexAgent` ReAct loop.
///
/// Returned by both `AgentExecutor::execute()` and `BaseAgent::run()`.
/// Implements `AgentOutputT` so it can serve as `AgentDeriveT::Output`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActOutput {
    /// The model's final answer text (extracted from `{"type":"final","content":"..."}`).
    pub response_text: String,
    /// Provider kind label (e.g. "openai", "anthropic") for telemetry.
    pub provider_kind: String,
    /// Full model label (e.g. "openai::qwen2.5-7b-instruct") for telemetry.
    pub model: String,
    /// Number of LLM turns used (1 = direct final answer, >1 = tool calls made).
    pub iterations: usize,
    /// Ordered list of tool names called during the loop, for telemetry.
    pub tool_calls_made: Vec<String>,
}

impl AgentOutputT for ReActOutput {
    fn output_schema() -> &'static str {
        r#"{"type":"object","properties":{"response_text":{"type":"string"},"provider_kind":{"type":"string"},"model":{"type":"string"},"iterations":{"type":"integer"},"tool_calls_made":{"type":"array","items":{"type":"string"}}},"required":["response_text","provider_kind","model","iterations","tool_calls_made"]}"#
    }

    fn structured_output_format() -> serde_json::Value {
        serde_json::json!({
            "name": "ReActOutput",
            "description": "Output from a completed Cortex ReAct loop",
            "schema": {
                "type": "object",
                "properties": {
                    "response_text": {"type": "string"},
                    "provider_kind": {"type": "string"},
                    "model": {"type": "string"},
                    "iterations": {"type": "integer"},
                    "tool_calls_made": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["response_text", "provider_kind", "model", "iterations", "tool_calls_made"]
            },
            "strict": true
        })
    }
}

// ── CortexAgent ───────────────────────────────────────────────────────────────

/// Cortex reasoning agent.
///
/// Constructed fresh per `cortex.step` request — stateless by design in Phase 1B.
/// Stores the pre-built structured system prompt, pre-computed action context, and
/// the tool router so `execute()` can drive the full ReAct loop autonomously.
#[derive(Debug)]
pub struct CortexAgent {
    /// Agent name from YAML config (e.g. "default", "researcher").
    agent_name: String,
    /// Human-readable description for AgentDeriveT introspection.
    description: String,
    /// Fully-assembled system prompt (includes JSON format instructions injected by
    /// `build_structured_system_prompt`).
    pub system_prompt: String,
    /// Pre-computed candidate action context injected on generation turns.
    /// `None` on tool_call turns.
    pub action_context: Option<String>,
    /// Provider config — used for `provider_kind`/`model` labels in telemetry
    /// and for `debug_llm` logging.  LLM calls go through `context.llm()`.
    pub provider_config: ProviderConfig,
    /// Tool set declared for AgentDeriveT — stubs wired in Phase 2+.
    tools: Vec<Box<dyn autoagents_core::tool::ToolT>>,
    /// Tool router — routes tool names from LLM output to UDS service sockets.
    router: Arc<ToolRouter>,
    /// Session identifier — written into diary entries for traceability.
    session_id: String,
    /// Directory for per-session diary markdown files (e.g. `data/diary/<session_id>/`).
    diary_dir: PathBuf,
}

impl CortexAgent {
    pub fn new(
        agent_name: String,
        system_prompt: String,
        action_context: Option<String>,
        provider_config: ProviderConfig,
        tools: Vec<Box<dyn autoagents_core::tool::ToolT>>,
        router: Arc<ToolRouter>,
        session_id: String,
        diary_dir: PathBuf,
    ) -> Self {
        Self {
            description: format!("Cortex reasoning agent: {agent_name}"),
            agent_name,
            system_prompt,
            action_context,
            provider_config,
            tools,
            router,
            session_id,
            diary_dir,
        }
    }
}

/// Maximum number of LLM→tool→LLM turns per `cortex.step` request.
const MAX_REACT_ITERATIONS: usize = 10;

// ── AgentDeriveT ─────────────────────────────────────────────────────────────

impl AgentDeriveT for CortexAgent {
    /// Rich output type carrying response text, telemetry labels, and loop stats.
    type Output = ReActOutput;

    fn name(&self) -> &str {
        &self.agent_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn output_schema(&self) -> Option<Value> {
        Some(ReActOutput::structured_output_format())
    }

    fn tools(&self) -> Vec<Box<dyn autoagents_core::tool::ToolT>> {
        // Tool dispatch goes through ToolRouter over UDS — not via framework tool stubs.
        // Stubs in self.tools are kept for AgentDeriveT compliance and future Phase 2+
        // integration; they are not called at runtime.
        vec![]
    }
}

// ── AgentExecutor ─────────────────────────────────────────────────────────────

#[async_trait]
impl AgentExecutor for CortexAgent {
    /// Rich output carrying response text + loop telemetry.
    type Output = ReActOutput;
    type Error = CortexAgentError;

    fn config(&self) -> ExecutorConfig {
        // Expose the full iteration budget so the framework knows this executor
        // is multi-turn (not single-turn as in Phase 1B).
        ExecutorConfig { max_turns: MAX_REACT_ITERATIONS }
    }

    /// Full multi-turn ReAct loop — the framework runtime entry point.
    ///
    /// Called by `BaseAgent::run(task)` after `on_run_start` fires.  Uses
    /// `context.llm()` (provider built once at `BaseAgent::new()`) for all LLM
    /// calls and `context.memory()` for session history.  Tool dispatch goes
    /// through `self.router` over UDS sockets.
    ///
    /// Per-iteration hooks fire before and after each LLM turn.  Tool hooks
    /// (`on_tool_call`, `on_tool_start`, `on_tool_result`, `on_tool_error`) fire
    /// around each tool dispatch.  Phase 3+ will override these for diary writes.
    async fn execute(
        &self,
        task: &Task,
        context: Arc<Context>,
    ) -> Result<ReActOutput, CortexAgentError> {
        let user_input = task.prompt.trim();

        let prompt = build_prompt_with_action_context(
            &self.system_prompt,
            user_input,
            self.action_context.clone(),
        );

        // Load prior conversation history.
        // Pass user_input as the recall query so HybridMemoryAdapter can run a
        // semantic LTM search alongside the STM window retrieval.
        let memory = context.memory();
        let history = if let Some(ref mem) = memory {
            mem.lock()
                .await
                .recall(user_input, None)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Build the initial message list: [system+context] + history + [current user turn].
        let user_msg = ChatMessageBuilder::new(ChatRole::User).content(user_input).build();

        let mut messages = Vec::with_capacity(2 + history.len());
        messages.push(
            ChatMessageBuilder::new(ChatRole::System)
                .content(&prompt.system_prompt)
                .build(),
        );
        messages.extend(history);
        messages.push(user_msg.clone());

        // Persist the current user turn in memory.
        if let Some(ref mem) = memory {
            mem.lock()
                .await
                .remember(&user_msg)
                .await
                .map_err(|e| CortexAgentError::memory(e))?;
        }

        let mut tool_calls_made: Vec<String> = Vec::new();

        for iteration in 0..MAX_REACT_ITERATIONS {
            self.on_turn_start(iteration, &context).await;

            // Call LLM via context.llm() — reuses the provider built once at
            // BaseAgent::new(), eliminating per-iteration provider reconstruction.
            let content = {
                let mut stream = context
                    .llm()
                    .chat_stream(&messages, None::<StructuredOutputFormat>)
                    .await
                    .map_err(|e| CortexAgentError::llm_stream(e))?;
                let mut buf = String::new();
                while let Some(chunk) = stream.next().await {
                    let delta = chunk.map_err(|e| CortexAgentError::llm_stream(e))?;
                    buf.push_str(&delta);
                }
                let trimmed = buf.trim().to_string();
                if trimmed.is_empty() {
                    return Err(CortexAgentError::llm_stream("provider returned empty response"));
                }
                if self.provider_config.debug_llm {
                    info!(
                        provider_kind = %self.provider_config.kind,
                        model = %self.provider_config.model,
                        response_len = trimmed.len(),
                        llm_response_text = %trimmed,
                        "cortex.llm.http.response"
                    );
                }
                trimmed
            };

            let validation = maybe_validate_response(&content)
                .await
                .map_err(CortexAgentError::from)?;
            let parsed = match parse_step_model_output(&validation.content) {
                Ok(p) => p,
                Err(e) => {
                    // Model returned prose or malformed JSON. Inject a
                    // correction prompt and continue — do not abort the step.
                    warn!(
                        iteration = iteration + 1,
                        error = %e,
                        raw_len = content.len(),
                        "cortex.react.parse_error — injecting correction prompt"
                    );
                    messages.push(
                        ChatMessageBuilder::new(ChatRole::User)
                            .content(concat!(
                                "Your previous response was not valid JSON. ",
                                "You must respond with exactly one JSON object and no surrounding text.\n",
                                "Allowed shapes:\n",
                                "1. {\"type\":\"final\",\"content\":\"your answer here\"}\n",
                                "2. {\"type\":\"tool_call\",\"tool\":\"tool.name\",\"arguments\":{}}"
                            ))
                            .build(),
                    );
                    self.on_turn_complete(iteration, &context).await;
                    continue;
                }
            };

            match parsed.response_type.as_str() {
                "final" => {
                    info!(
                        iterations = iteration + 1,
                        tool_calls = tool_calls_made.len(),
                        provider_kind = %self.provider_config.kind,
                        model = %self.provider_config.model,
                        "cortex.react.complete"
                    );

                    // Persist the final assistant response.
                    let final_msg = ChatMessageBuilder::new(ChatRole::Assistant)
                        .content(&validation.content)
                        .build();
                    if let Some(ref mem) = memory {
                        let _ = mem.lock().await.remember(&final_msg).await;
                    }

                    // Fire-and-forget diary write — does not block the step response.
                    tokio::spawn(write_diary_entry(
                        self.session_id.clone(),
                        self.diary_dir.clone(),
                        user_input.to_string(),
                        parsed.response_text.clone(),
                        tool_calls_made.clone(),
                        Arc::clone(&self.router),
                    ));

                    self.on_turn_complete(iteration, &context).await;

                    let (provider_kind, model) = telemetry_labels(&self.provider_config);
                    return Ok(ReActOutput {
                        response_text: parsed.response_text,
                        provider_kind,
                        model,
                        iterations: iteration + 1,
                        tool_calls_made,
                    });
                }

                "tool_call" => {
                    let tool_name =
                        parsed.tool_call["tool"].as_str().unwrap_or("").to_string();
                    let arguments = parsed.tool_call["arguments"].clone();

                    info!(
                        tool = %tool_name,
                        iteration = iteration + 1,
                        "cortex.react.tool_call"
                    );

                    // Construct an LlmToolCall for hook interop.
                    // Our JSON-format tools carry no native tool-call ID.
                    let llm_tool_call = LlmToolCall {
                        id: String::new(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: tool_name.clone(),
                            arguments: serde_json::to_string(&arguments).unwrap_or_default(),
                        },
                    };

                    // Hook: on_tool_call — allows aborting before dispatch.
                    if self.on_tool_call(&llm_tool_call, &context).await == HookOutcome::Abort {
                        warn!(tool = %tool_name, "cortex.react.tool_call.aborted_by_hook");
                        messages.push(
                            ChatMessageBuilder::new(ChatRole::User)
                                .content(&format!(
                                    "Tool call for {tool_name} was aborted."
                                ))
                                .build(),
                        );
                        self.on_turn_complete(iteration, &context).await;
                        continue;
                    }

                    // Persist the model's tool_call JSON as an assistant turn.
                    let assistant_msg = ChatMessageBuilder::new(ChatRole::Assistant)
                        .content(&validation.content)
                        .build();
                    if let Some(ref mem) = memory {
                        let _ = mem.lock().await.remember(&assistant_msg).await;
                    }
                    messages.push(assistant_msg);

                    self.on_tool_start(&llm_tool_call, &context).await;

                    // Dispatch via ToolRouter.  On failure, feed error text back so
                    // the model can decide how to recover.
                    let tool_result =
                        match self.router.call(&tool_name, &arguments).await {
                            Ok(result) => {
                                info!(
                                    tool = %tool_name,
                                    result_len = result.len(),
                                    "cortex.react.tool_result"
                                );
                                let call_result = ToolCallResult {
                                    tool_name: tool_name.clone(),
                                    success: true,
                                    arguments: arguments.clone(),
                                    result: serde_json::json!(result.clone()),
                                };
                                self.on_tool_result(&llm_tool_call, &call_result, &context)
                                    .await;
                                result
                            }
                            Err(e) => {
                                warn!(tool = %tool_name, error = %e, "cortex.react.tool_error");
                                let err_val = serde_json::json!(e.to_string());
                                self.on_tool_error(&llm_tool_call, err_val, &context).await;
                                format!("{{\"error\":\"{e}\",\"tool\":\"{tool_name}\"}}")
                            }
                        };

                    tool_calls_made.push(tool_name.clone());

                    // Append tool result as the next user turn.
                    messages.push(
                        ChatMessageBuilder::new(ChatRole::User)
                            .content(&format!(
                                "Tool result for {tool_name}:\n{tool_result}"
                            ))
                            .build(),
                    );
                }

                other => {
                    return Err(CortexAgentError::unsupported_response(other));
                }
            }

            self.on_turn_complete(iteration, &context).await;
        }

        Err(CortexAgentError::iteration_limit(MAX_REACT_ITERATIONS))
    }
}

// ── AgentHooks ────────────────────────────────────────────────────────────────

/// Lifecycle hooks — all default no-ops in Phase 1B.
///
/// Phase 3+ will override:
/// - `on_run_complete`: write diary markdown + stub LanceDB diary row.
/// - `on_turn_start` / `on_turn_complete`: per-turn telemetry.
/// - `on_tool_call`: whitelist check / rate limiting.
#[async_trait]
impl AgentHooks for CortexAgent {}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute `(provider_kind, model_display_label)` for telemetry from config.
///
/// `provider_kind` = raw config kind (e.g. "openai_compat").
/// `model` = `"<display_kind>::<model>"` (e.g. "openai::qwen2.5-7b-instruct").
fn telemetry_labels(config: &ProviderConfig) -> (String, String) {
    let provider_kind = config.kind.clone();
    let display_kind = match config.kind.trim() {
        "openai" | "openai_compat" => "openai",
        "anthropic" => "anthropic",
        "ollama" => "ollama",
        other => other,
    };
    let model = format!("{}::{}", display_kind, config.model.trim());
    (provider_kind, model)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_provider() -> ProviderConfig {
        ProviderConfig {
            kind: "openai_compat".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: String::new(),
            model: "test-model".to_string(),
            timeout: 10.0,
            max_tokens: 512,
            debug_llm: false,
        }
    }

    fn make_router() -> Arc<ToolRouter> {
        Arc::new(ToolRouter::new(PathBuf::from("data/sockets")))
    }

    fn make_agent(name: &str) -> CortexAgent {
        CortexAgent::new(
            name.to_string(),
            "System prompt".to_string(),
            None,
            dummy_provider(),
            vec![],
            make_router(),
            "test-session".to_string(),
            PathBuf::from("data/diary/test-session"),
        )
    }

    #[test]
    fn agent_name_and_description() {
        let agent = make_agent("researcher");
        assert_eq!(agent.name(), "researcher");
        assert!(agent.description().contains("researcher"));
    }

    #[test]
    fn output_schema_is_some_with_react_output_schema() {
        let agent = make_agent("default");
        let schema = agent.output_schema();
        assert!(schema.is_some());
        let v = schema.unwrap();
        assert_eq!(v["name"], "ReActOutput");
    }

    #[test]
    fn tools_returns_empty_for_framework_tool_dispatch() {
        let agent = make_agent("default");
        // Tools return empty — ToolRouter handles dispatch via UDS, not framework stubs.
        assert!(agent.tools().is_empty());
    }

    #[test]
    fn executor_config_exposes_max_react_iterations() {
        let agent = make_agent("default");
        assert_eq!(agent.config().max_turns, MAX_REACT_ITERATIONS);
    }

    #[test]
    fn react_output_implements_agent_output_t() {
        // Verify schema is valid JSON.
        let _: serde_json::Value =
            serde_json::from_str(ReActOutput::output_schema()).expect("schema must be valid JSON");
        let fmt = ReActOutput::structured_output_format();
        assert_eq!(fmt["name"], "ReActOutput");
    }

    #[test]
    fn react_output_serialization_round_trip() {
        let out = ReActOutput {
            response_text: "hello".to_string(),
            provider_kind: "openai_compat".to_string(),
            model: "openai::test-model".to_string(),
            iterations: 2,
            tool_calls_made: vec!["browser.open".to_string()],
        };
        let json = serde_json::to_string(&out).unwrap();
        let back: ReActOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(back.response_text, "hello");
        assert_eq!(back.iterations, 2);
        assert_eq!(back.tool_calls_made, vec!["browser.open"]);
    }

    #[test]
    fn telemetry_labels_normalises_openai_compat() {
        let config = dummy_provider();
        let (kind, model) = telemetry_labels(&config);
        assert_eq!(kind, "openai_compat");
        assert_eq!(model, "openai::test-model");
    }

    #[test]
    fn telemetry_labels_anthropic() {
        let config = ProviderConfig {
            kind: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            timeout: 60.0,
            max_tokens: 2048,
            debug_llm: false,
        };
        let (kind, model) = telemetry_labels(&config);
        assert_eq!(kind, "anthropic");
        assert_eq!(model, "anthropic::claude-3-5-sonnet-20241022");
    }
}
