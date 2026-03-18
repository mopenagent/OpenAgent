use crate::action::catalog::ActionCatalog;
use crate::action::search::{search_catalog, SearchQuery, SearchResult};
use crate::agent::CortexAgent;
use crate::classifier;
use crate::config::CortexConfig;
use crate::llm::build_llm_provider;
use crate::memory_adapter::{HybridMemoryAdapter, DEFAULT_STM_WINDOW};
use crate::metrics::{elapsed_ms, step_err, step_ok, CortexTelemetry};
use crate::tool_router::ToolRouter;
use anyhow::{anyhow, Result};
use autoagents_core::agent::task::Task;
use autoagents_core::agent::{BaseAgent, DirectAgent};
use autoagents_protocol::Event;
use opentelemetry::KeyValue;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// How many tools the semantic search returns per step.
/// Keep this tight — every extra tool adds ~80 tokens to the context window.
const ACTION_SEARCH_LIMIT: usize = 8;

/// Tools always included regardless of search results.
/// - memory.search: LTM recall happens on every generation turn.
/// - cortex.step: supervisor uses this to dispatch tasks to named worker agents.
/// - browser.open/navigate/snapshot: primary interaction tools for web tasks.
///
/// NOTE: research.status is NOT pinned here — it is only added when the input
/// matches RESEARCH_KEYWORDS or when active research already exists (see
/// search_tools_for_step). This prevents the LLM from launching research DAGs
/// for ordinary conversational turns.
const ALWAYS_INCLUDE: &[&str] = &[
    "memory.search",
    "cortex.step",
    "browser.open",
    "browser.navigate",
    "browser.snapshot",
];

/// Keywords that indicate the user explicitly wants research/investigation work.
/// When matched, research.status and research.start are added to the tool context.
const RESEARCH_KEYWORDS: &[&str] = &[
    // Core research intent
    "research", "investigate", "investigation",
    // Analysis terms
    "analyse", "analyze", "analysis", "analytical",
    // Study / review
    "study", "review", "audit", "examine", "assessment",
    // Exploration
    "survey", "explore", "exploration", "find out", "look into",
    "deep dive", "deep-dive", "dig into", "dive into",
    // Reports and synthesis
    "report", "summarise", "summarize", "synthesis", "synthesize", "synthesise",
    "compile", "compare", "comparison",
    // Scientific / academic
    "hypothesis", "evaluate", "evaluation", "benchmark",
    // Common phrasings
    "what do we know about", "tell me about", "gather information",
    "collect data", "track", "monitor", "follow up",
];

#[derive(Clone, Debug)]
pub struct AppContext {
    tel: Arc<CortexTelemetry>,
    action_catalog: Arc<ActionCatalog>,
    tool_router: Arc<ToolRouter>,
}

impl AppContext {
    pub fn new(
        tel: Arc<CortexTelemetry>,
        action_catalog: Arc<ActionCatalog>,
        tool_router: Arc<ToolRouter>,
    ) -> Self {
        Self { tel, action_catalog, tool_router }
    }

    pub fn tel(&self) -> Arc<CortexTelemetry> {
        Arc::clone(&self.tel)
    }

    pub fn action_catalog(&self) -> Arc<ActionCatalog> {
        Arc::clone(&self.action_catalog)
    }

    pub fn tool_router(&self) -> Arc<ToolRouter> {
        Arc::clone(&self.tool_router)
    }
}

pub fn handle_describe_boundary() -> String {
    json!({
        "phase": "phase1",
        "status": "step-ready",
        "service_boundary": {
            "is_service": true,
            "transport": "mcp-lite-json-uds",
            "python_shell_role": "temporary pre-cortex shell",
            "llm_calling_rule": "cortex-only in target architecture"
        },
        "owns_now": [
            "service identity",
            "mcp-lite socket boundary",
            "config-backed system prompt loading",
            "single-step llm execution",
            "step observability"
        ],
        "does_not_own_yet": [
            "tool routing",
            "memory retrieval",
            "plan store",
            "segmented stm"
        ]
    })
    .to_string()
}

pub fn handle_step(params: Value, ctx: Arc<AppContext>) -> Result<String> {
    let tel = ctx.tel();
    let catalog = ctx.action_catalog();
    let router = ctx.tool_router();
    let p = params
        .as_object()
        .ok_or_else(|| anyhow!("params must be an object"))?;
    let session_id = p
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("session_id is required"))?
        .to_string();
    let user_input = p
        .get("user_input")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("user_input is required"))?
        .to_string();
    let requested_agent = p.get("agent_name").and_then(|v| v.as_str()).map(str::trim);
    let turn_kind = p
        .get("turn_kind")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("generation")
        .to_string();
    // user_key is used to look up the active research for this user.
    // Falls back to session_id when omitted so single-user sessions work without extra params.
    let user_key = p
        .get("user_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(session_id.as_str())
        .to_string();

    let _cx_guard = CortexTelemetry::attach_context(
        &params,
        vec![
            KeyValue::new("service", "cortex"),
            KeyValue::new("op", "step"),
            KeyValue::new("session_id", session_id.clone()),
        ],
    );

    let span = tracing::info_span!(
        "cortex.step",
        session_id = %session_id,
        agent_name = tracing::field::Empty,
        provider_kind = tracing::field::Empty,
        model = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        status = tracing::field::Empty,
        user_input_len = user_input.len(),
        output_len = tracing::field::Empty,
    );
    let _enter = span.enter();

    let started = Instant::now();

    // Close any browser sessions left open from the previous step.
    // Fire-and-forget — never fails the current step.
    if turn_kind != "tool_call" {
        let close_router = Arc::clone(&router);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let _ = close_router.call("browser.close_all", &json!({})).await;
            })
        });
    }

    let cfg_file = CortexConfig::load()?;
    let resolved = cfg_file
        .cfg
        .resolve_step_config(cfg_file.path.clone(), requested_agent);
    let mut structured_system_prompt = crate::prompt::render_step_system(&resolved.system_prompt)
        .map_err(|e| anyhow!("system prompt render failed: {e}"))?;

    // Phase 6: Proactively inject active research context into the system prompt on
    // generation turns so the supervisor always knows what tasks are runnable without
    // needing an extra `research.status` tool call first.
    // Must be fetched before Phase 5 tool selection so research tools are gated correctly.
    let research_context_block = if turn_kind != "tool_call" {
        fetch_research_context(&router, &user_key)
    } else {
        None
    };
    if let Some(ref rc) = research_context_block {
        structured_system_prompt.push_str("\n\n");
        structured_system_prompt.push_str(rc);
    }

    // Phase 5: Action Search — select top-k tools relevant to the user's input
    // rather than exposing every tool on every step.  On tool-call turns the
    // model is already mid-ReAct; don't re-inject the candidate list.
    // Research tools are only pinned when the input mentions research keywords
    // OR active research already exists — prevents the LLM from launching
    // research DAGs on ordinary conversational turns.
    let default_tools = if turn_kind != "tool_call" {
        search_tools_for_step(&catalog, &user_input, research_context_block.is_some())
    } else {
        vec![]
    };
    let action_context = if turn_kind == "tool_call" {
        None
    } else {
        render_default_tool_context(&default_tools)
    };

    // Query classifier: select fast vs strong provider based on turn content.
    // When fast_provider is absent this is a no-op — all turns use the main provider.
    let selected_provider = match &resolved.fast_provider {
        Some(fast) => {
            let tier = classifier::classify(
                &user_input,
                research_context_block.is_some(),
                &turn_kind,
            );
            if tier == classifier::ProviderTier::Fast {
                info!(model = %fast.model, "cortex.classifier.fast");
                fast.clone()
            } else {
                resolved.provider.clone()
            }
        }
        None => resolved.provider.clone(),
    };

    let data_root = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let diary_dir = data_root
        .join(&cfg_file.cfg.memory.diary_path)
        .join(&session_id);

    let cortex_agent = CortexAgent::new(
        resolved.agent_name.clone(),
        structured_system_prompt,
        action_context,
        selected_provider.clone(),
        Arc::clone(&router),
        session_id.clone(),
        diary_dir,
    );

    span.record("agent_name", resolved.agent_name.as_str());
    span.record("provider_kind", selected_provider.kind.as_str());
    span.record("model", selected_provider.model.as_str());

    info!(
        agent_name = %resolved.agent_name,
        provider_kind = %selected_provider.kind,
        model = %selected_provider.model,
        config_path = %resolved.source_path.display(),
        turn_kind = %turn_kind,
        action_candidates = default_tools.len(),
        has_research_context = research_context_block.is_some(),
        "cortex.step.start"
    );

    // Construct BaseAgent with HybridMemoryAdapter — wires the AutoAgents memory contract.
    //   STM: AutoAgents SlidingWindowMemory (Drop strategy, DEFAULT_STM_WINDOW messages).
    //   LTM: memory.sock via ToolRouter (semantic recall at loop start).
    //   Eviction + clear hooks dump overflow messages to data/stm/{session_id}/.
    let stm_dir = data_root.join("data").join("stm").join(&session_id);
    let memory_adapter = HybridMemoryAdapter::new(
        &session_id,
        DEFAULT_STM_WINDOW,
        stm_dir,
        Arc::clone(&router),
    );

    let llm_provider = build_llm_provider(&selected_provider)
        .map_err(|e| anyhow!("llm provider build failed: {e}"))?;
    let (tx, _rx) = mpsc::channel::<Event>(32);

    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            let base_agent =
                BaseAgent::<CortexAgent, DirectAgent>::new(
                    cortex_agent,
                    llm_provider,
                    Some(Box::new(memory_adapter)),
                    tx,
                    false,
                )
                .await
                .map_err(|e| anyhow!("base agent construction failed: {e}"))?;

            base_agent
                .run(Task::new(&user_input))
                .await
                .map_err(|e| anyhow!("{e}"))
        })
    });

    match result {
        Ok(react_output) => {
            let duration_ms = elapsed_ms(started);
            span.record("status", "ok");
            span.record("duration_ms", duration_ms);
            span.record("output_len", react_output.response_text.len() as i64);
            info!(
                agent_name = %resolved.agent_name,
                provider_kind = %react_output.provider_kind,
                model = %react_output.model,
                duration_ms,
                output_len = react_output.response_text.len(),
                iterations = react_output.iterations,
                tool_calls = ?react_output.tool_calls_made,
                default_tool_count = default_tools.len(),
                "cortex.step.ok"
            );
            tel.record(&step_ok(
                &session_id,
                &resolved.agent_name,
                &react_output.provider_kind,
                &react_output.model,
                &resolved.source_path.display().to_string(),
                duration_ms,
                user_input.len(),
                react_output.response_text.len(),
            ));

            Ok(json!({
                "session_id": session_id,
                "agent_name": resolved.agent_name,
                "provider_kind": react_output.provider_kind,
                "model": react_output.model,
                "response_type": "final",
                "response_text": react_output.response_text,
                "tool_call": null,
                "react_summary": {
                    "iterations": react_output.iterations,
                    "tool_calls_made": react_output.tool_calls_made,
                    "default_tool_count": default_tools.len(),
                    "candidates": default_tools.iter().map(|v| v.name.clone()).collect::<Vec<_>>()
                }
            })
            .to_string())
        }
        Err(err) => {
            let duration_ms = elapsed_ms(started);
            span.record("status", "error");
            span.record("duration_ms", duration_ms);
            error!(
                agent_name = %resolved.agent_name,
                provider_kind = %selected_provider.kind,
                model = %selected_provider.model,
                duration_ms,
                error = %err,
                "cortex.step.error"
            );
            tel.record(&step_err(
                &session_id,
                &resolved.agent_name,
                &selected_provider.kind,
                &selected_provider.model,
                &resolved.source_path.display().to_string(),
                duration_ms,
                user_input.len(),
            ));
            Err(err)
        }
    }
}

/// Phase 5: select the top-k tools most relevant to this user input.
///
/// Algorithm (all keyword-based, no embedding needed for Phase 5):
///   1. Run scored search over the ActionCatalog using user_input as query.
///   2. Pin any ALWAYS_INCLUDE tools that didn't make the top-k naturally.
///   3. Conditionally pin research tools when input matches RESEARCH_KEYWORDS
///      or when active research already exists (`has_active_research`).
///   4. Append cortex.discover so the agent can fetch more tools mid-task.
fn search_tools_for_step(
    catalog: &ActionCatalog,
    user_input: &str,
    has_active_research: bool,
) -> Vec<SearchResult> {
    let mut results = search_catalog(
        catalog,
        SearchQuery {
            query: user_input.to_string(),
            kind: None,
            owner: None,
            limit: ACTION_SEARCH_LIMIT,
            include_params: true,
        },
    )
    .results;

    // Always include pinned tools (e.g. memory.search for LTM recall).
    for pinned_name in ALWAYS_INCLUDE {
        if !results.iter().any(|r| r.name == *pinned_name) {
            if let Some(entry) = catalog.entries().iter().find(|e| e.name == *pinned_name) {
                results.push(catalog_entry_to_result(entry));
            }
        }
    }

    // Conditionally pin research tools only when explicitly requested or active.
    // This prevents ordinary conversational turns from triggering the research DAG.
    if has_active_research || input_wants_research(user_input) {
        for research_tool in &["research.status", "research.start"] {
            if !results.iter().any(|r| r.name == *research_tool) {
                if let Some(entry) = catalog.entries().iter().find(|e| e.name == *research_tool) {
                    results.push(catalog_entry_to_result(entry));
                }
            }
        }
    }

    // Always expose cortex.discover so the agent can search for more tools
    // when the top-k candidates are insufficient for the task.
    results.push(discover_tool_result());

    results
}

/// Returns true when the user input contains a research-intent keyword.
fn input_wants_research(user_input: &str) -> bool {
    let lower = user_input.to_lowercase();
    RESEARCH_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Convert a catalog entry into a SearchResult for pinning into the tool context.
fn catalog_entry_to_result(entry: &crate::action::catalog::ActionEntry) -> SearchResult {
    SearchResult {
        kind: entry.kind.as_str().to_string(),
        owner: entry.owner.clone(),
        runtime: entry.runtime.clone(),
        manifest_path: entry.manifest_path.display().to_string(),
        name: entry.name.clone(),
        summary: entry.summary.clone(),
        required: entry.required.clone(),
        param_names: entry.param_names.clone(),
        allowed_tools: entry.allowed_tools.clone(),
        steps: entry.steps.clone(),
        constraints: entry.constraints.clone(),
        completion_criteria: entry.completion_criteria.clone(),
        guidance: entry.guidance.clone(),
        params: Some(entry.params.clone()),
    }
}

fn render_default_tool_context(results: &[SearchResult]) -> Option<String> {
    if results.is_empty() {
        return None;
    }

    Some(
        results
            .iter()
            .map(render_tool_schema)
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn render_tool_schema(result: &SearchResult) -> String {
    let params = result
        .params
        .as_ref()
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}, "required": []}));
    format!(
        concat!(
            "tool: {}\n",
            "kind: {}\n",
            "owner: {}\n",
            "summary: {}\n",
            "params_schema: {}"
        ),
        result.name, result.kind, result.owner, result.summary, params
    )
}

fn discover_tool_result() -> SearchResult {
    SearchResult {
        kind: "tool".to_string(),
        owner: "cortex".to_string(),
        runtime: "rust".to_string(),
        manifest_path: "services/cortex/service.json".to_string(),
        name: "cortex.discover".to_string(),
        summary: "Discover additional tools and guidance skills beyond the default six. Use kind=tool|skill_guidance|all."
            .to_string(),
        required: vec!["query".to_string()],
        param_names: vec![
            "query".to_string(),
            "kind".to_string(),
            "owner".to_string(),
            "limit".to_string(),
            "include_params".to_string(),
        ],
        allowed_tools: Vec::new(),
        steps: Vec::new(),
        constraints: Vec::new(),
        completion_criteria: Vec::new(),
        guidance: "Use this only when the default six tools are insufficient.".to_string(),
        params: Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for tools and skills"
                },
                "kind": {
                    "type": "string",
                    "enum": ["tool", "skill_guidance", "all"],
                    "description": "Optional discovery mode. Default is all."
                },
                "owner": {
                    "type": "string",
                    "description": "Optional owner filter such as browser, sandbox, or skill folder"
                },
                "limit": {
                    "type": "number",
                    "description": "Max results to return"
                },
                "include_params": {
                    "type": "boolean",
                    "description": "Include full params schema for discovered tools"
                }
            },
            "required": ["query"]
        })),
    }
}

// ── Research context injection ─────────────────────────────────────────────────

/// Fetch the active research status for `user_key` via the ToolRouter and format
/// it as a system-prompt block.
///
/// Returns `None` when:
/// - research.sock does not exist (service not running)
/// - the user has no active research
/// - the research has no runnable tasks and no active research
/// - the call fails (logged as warning, never propagates)
fn fetch_research_context(router: &ToolRouter, user_key: &str) -> Option<String> {
    if !router.socket_exists("research.status") {
        return None;
    }
    let args = json!({ "user_key": user_key });
    let raw = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(router.call("research.status", &args))
    });
    match raw {
        Ok(json_str) => format_research_context(&json_str),
        Err(e) => {
            warn!(user_key = %user_key, error = %e, "research.status fetch failed (non-fatal)");
            None
        }
    }
}

/// Parse the `research.status` JSON response and format it as a markdown block
/// suitable for injecting into the supervisor's system prompt.
fn format_research_context(json_str: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json_str).ok()?;

    // No active research for this user — nothing to inject.
    if v.get("research").map_or(true, |r| r.is_null()) {
        return None;
    }
    let research = v.get("research")?.as_object()?;
    let title = research.get("title")?.as_str()?;
    let goal = research.get("goal")?.as_str()?;
    let runnable_tasks = v.get("runnable_tasks")?.as_array()?;

    let mut out = format!(
        "## Active Research: \"{title}\"\n**Goal:** {goal}\n"
    );

    if runnable_tasks.is_empty() {
        out.push_str(
            "\nAll tasks are in progress or complete. \
             Use `research.status` to review the full task graph.\n"
        );
    } else {
        out.push_str("\n**Runnable tasks — pick one to work on next:**\n");
        for (i, task) in runnable_tasks.iter().enumerate() {
            let id = task.get("id").and_then(Value::as_str).unwrap_or("?");
            let desc = task.get("description").and_then(Value::as_str).unwrap_or("?");
            let agent = task.get("assigned_agent").and_then(Value::as_str);
            // Show first 8 chars of the UUID as a compact reference.
            let id_short = &id[..id.len().min(8)];
            match agent {
                Some(a) => out.push_str(&format!(
                    "{}. [{}] {} → delegate to `{}`\n", i + 1, id_short, desc, a
                )),
                None => out.push_str(&format!(
                    "{}. [{}] {}\n", i + 1, id_short, desc
                )),
            }
        }
        out.push_str(
            "\nCall `research.task_done` with the task_id when you finish a task. \
             Use `research.task_add` to add sub-tasks. \
             Delegate long-running tasks via `cortex.step` with `agent_name`.\n"
        );
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_status(title: &str, goal: &str, runnable: &[(&str, &str, Option<&str>)]) -> String {
        let tasks: Vec<Value> = runnable
            .iter()
            .map(|(id, desc, agent)| {
                json!({
                    "id": id,
                    "description": desc,
                    "assigned_agent": agent,
                    "status": "pending"
                })
            })
            .collect();
        json!({
            "research": { "title": title, "goal": goal },
            "tasks": tasks,
            "runnable_tasks": tasks
        })
        .to_string()
    }

    #[test]
    fn format_research_context_null_research_returns_none() {
        let json = json!({"research": null, "tasks": [], "runnable_tasks": []}).to_string();
        assert!(format_research_context(&json).is_none());
    }

    #[test]
    fn format_research_context_no_runnable_tasks_shows_all_complete_note() {
        let json = json!({
            "research": {"title": "AI Safety", "goal": "Study alignment"},
            "tasks": [],
            "runnable_tasks": []
        })
        .to_string();
        let out = format_research_context(&json).unwrap();
        assert!(out.contains("## Active Research: \"AI Safety\""));
        assert!(out.contains("Study alignment"));
        assert!(out.contains("All tasks are in progress or complete"));
    }

    #[test]
    fn format_research_context_shows_runnable_tasks() {
        let json = make_status(
            "Quantum Computing",
            "Survey recent advances",
            &[
                ("aaaaaaaa-1234-5678-abcd-ef0123456789", "Search papers", None),
                ("bbbbbbbb-1234-5678-abcd-ef0123456789", "Summarise papers", Some("summarise-agent")),
            ],
        );
        let out = format_research_context(&json).unwrap();
        assert!(out.contains("## Active Research: \"Quantum Computing\""));
        assert!(out.contains("Survey recent advances"));
        assert!(out.contains("1. [aaaaaaa") || out.contains("1. [aaaaaaaa"));
        assert!(out.contains("Search papers"));
        assert!(out.contains("summarise-agent"));
        assert!(out.contains("research.task_done"));
        assert!(out.contains("cortex.step"));
    }

    #[test]
    fn format_research_context_id_short_is_max_8_chars() {
        let json = make_status(
            "Test",
            "Goal",
            &[("a1b2c3d4e5f6", "Short task", None)],
        );
        let out = format_research_context(&json).unwrap();
        // Only first 8 chars of ID should appear in brackets
        assert!(out.contains("[a1b2c3d4]"));
    }
}

