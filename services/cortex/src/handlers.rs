use crate::action::catalog::ActionCatalog;
use crate::action::search::{search_catalog, SearchQuery, SearchResult};
use crate::agent::CortexAgent;
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
use tracing::{error, info};

const DEFAULT_TOOL_NAMES: &[&str] = &[
    "browser.open",
    "browser.navigate",
    "browser.snapshot",
    "sandbox.execute",
    "sandbox.shell",
    "memory.search",
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
    let cfg_file = CortexConfig::load()?;
    let resolved = cfg_file
        .cfg
        .resolve_step_config(cfg_file.path.clone(), requested_agent);
    let default_tools = collect_default_tools(&catalog);
    let action_context = if turn_kind == "tool_call" {
        None
    } else {
        render_default_tool_context(&default_tools)
    };
    let structured_system_prompt = crate::prompt::render_step_system(&resolved.system_prompt)
        .map_err(|e| anyhow!("system prompt render failed: {e}"))?;

    let data_root = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let diary_dir = data_root
        .join(&cfg_file.cfg.memory.diary_path)
        .join(&session_id);

    let cortex_agent = CortexAgent::new(
        resolved.agent_name.clone(),
        structured_system_prompt,
        action_context,
        resolved.provider.clone(),
        crate::agent_tools::default_tools(),
        Arc::clone(&router),
        session_id.clone(),
        diary_dir,
    );

    span.record("agent_name", resolved.agent_name.as_str());
    span.record("provider_kind", resolved.provider.kind.as_str());
    span.record("model", resolved.provider.model.as_str());

    info!(
        agent_name = %resolved.agent_name,
        provider_kind = %resolved.provider.kind,
        config_path = %resolved.source_path.display(),
        turn_kind = %turn_kind,
        inject_default_tools = turn_kind != "tool_call",
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
    let llm_provider = build_llm_provider(&resolved.provider)
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
                provider_kind = %resolved.provider.kind,
                model = %resolved.provider.model,
                duration_ms,
                error = %err,
                "cortex.step.error"
            );
            tel.record(&step_err(
                &session_id,
                &resolved.agent_name,
                &resolved.provider.kind,
                &resolved.provider.model,
                &resolved.source_path.display().to_string(),
                duration_ms,
                user_input.len(),
            ));
            Err(err)
        }
    }
}

fn collect_default_tools(catalog: &ActionCatalog) -> Vec<SearchResult> {
    let by_name = DEFAULT_TOOL_NAMES
        .iter()
        .filter_map(|name| {
            catalog
                .entries()
                .iter()
                .find(|entry| entry.name == *name)
                .map(|entry| SearchResult {
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
                })
        })
        .collect::<Vec<_>>();
    // cortex.discover disabled — deterministic tool set only (Phase 2+ re-enables via ActionCatalog search)
    // by_name.push(discover_tool_result());
    by_name
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

pub fn handle_search_actions(params: Value, catalog: Arc<ActionCatalog>) -> Result<String> {
    let p = params
        .as_object()
        .ok_or_else(|| anyhow!("params must be an object"))?;
    let query = p
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("query is required"))?
        .to_string();

    let kind = p
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|value| {
            if value == "all" {
                String::new()
            } else {
                value.to_string()
            }
        })
        .filter(|v| !v.is_empty());
    let owner = p
        .get("owner")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    let include_params = p
        .get("include_params")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = p
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(8)
        .clamp(1, 25);

    let response = search_catalog(
        &catalog,
        SearchQuery {
            query,
            kind,
            owner,
            limit,
            include_params,
        },
    );
    Ok(serde_json::to_string(&response)?)
}

pub fn handle_search_tools(params: Value, catalog: Arc<ActionCatalog>) -> Result<String> {
    handle_search_actions(params, catalog)
}

pub fn handle_discover(params: Value, catalog: Arc<ActionCatalog>) -> Result<String> {
    handle_search_actions(params, catalog)
}

