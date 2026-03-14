# Cortex Service

Rust service target for OpenAgent's cognitive control plane. Cortex is the future agent loop, planner, retrieval orchestrator, and tool router. It does not store all knowledge itself. Instead, it coordinates:

- LLM service
- memory service
- tool services

OpenAgent's current Python outer loop is treated as a temporary shell that will call Cortex. The final architecture keeps Cortex as a separate service, not embedded in the Python app.

## Phase 1 Status

Phase 1 is implemented as a minimal single-step reasoning service.

What exists now:
- Cortex is a standalone Rust MCP-lite service.
- Transport is locked to JSON over Unix Domain Sockets.
- Service identity is declared in `service.json`.
- `cortex.describe_boundary` reports the current service boundary.
- `cortex.step` performs one LLM-backed response step.
- The system prompt is loaded from `config/openagent.yaml` or `config/openagent.yml`.
- Traces, metrics, and structured logs are emitted for each Cortex step.

What does not exist yet:
- tool routing
- memory retrieval
- planner / DAG store
- STM segmentation

This keeps Phase 1 aligned with [`TODO.md`](./TODO.md): one message in, one LLM response out, no tools or planning yet.

## Role

Cortex owns:
- session step execution
- prompt/context assembly
- tool discovery and tool routing
- plan-of-action execution
- reflection scheduling
- memory orchestration
- diary emission orchestration

Cortex does not own:
- vector storage internals
- markdown KB storage internals
- browser automation internals
- sandbox execution internals
- direct user interface concerns

Those remain in dedicated services.

## Final Topology

```text
OpenAgent Shell (Python, temporary)
        |
        v
   Cortex Service (Rust)
        |
  +-----+----------+------------------+
  |                |                  |
  v                v                  v
LLM Service    Memory Service     Tool Services
               (STM/LTM/KB)       (browser, sandbox, ...)
```

All service communication should use MCP-lite over JSON + UDS.

## Design Principles

- Cortex is a service boundary, not a library inside OpenAgent.
- Cortex is the single source of cognition. The shell must not call the LLM directly.
- Tools are called by Cortex, not by the outer Python loop.
- Memory remains a separate service. Cortex decides when to read and write memory, but does not own LanceDB or the KB vault directly.
- STM is working control state and should remain local to Cortex or Cortex-managed runtime state.
- LTM and KB remain persistent memory concerns behind the memory service boundary.

## Cognitive Stages

Cortex is organized as a subsystem with three major stages around the LLM.

### 1. Pre-LLM Cognition

Responsibilities:
- load active session
- load active plan
- decide current goal and current runnable task
- retrieve memory context
- load STM segments
- assemble final prompt package

Inputs:
- user input
- session state
- plan state
- STM state
- memory bundle

Outputs:
- prompt package for LLM
- candidate tools
- execution state snapshot

### 2. LLM Reasoning

The LLM is treated as a reasoning engine, not the system brain.

Expected outputs:
- answer
- tool call
- plan update suggestion
- reflection output

The LLM must produce structured output. It must not mutate state directly.

### 3. Post-LLM Cognition

Responsibilities:
- validate LLM output
- execute tool calls
- update plan DAG
- write episodic memory
- emit deterministic diary entry
- schedule reflection
- emit final response to caller

Outputs:
- user-visible response
- plan update
- tool execution log
- memory write events
- diary write event

## Planned Subsystems

### Planner

Persistent task graph stored in SQLite. Plans are not memory objects; they are control state.

Expected tables:
- `plans`
- `tasks`
- `task_dependencies`
- `tool_calls`
- `turns`
- `sessions`

Each session owns an active plan. Each task can depend on previous tasks.

### Retrieval

Unified search interface in Cortex, backed by the memory service.

Initial strategy:
- one unified memory search
- memory bundle return shape
- KB graph expansion handled by memory service or requested via memory API

Longer term:
- richer routing by task type and uncertainty
- tighter coupling between plan state and retrieval query construction

### Action Registry and Action Search

Because the number of tools and skills will grow, Cortex should not expose the full action set to the LLM every cycle.

Instead:
- maintain an action registry
- index service tools and local skills together
- search top-k candidate actions for the current task
- pass only a small candidate set to the LLM

Action discovery is the main abstraction, not service names. Browser and sandbox are important because they expose many tools each, while local skills provide guidance about how to use those tools. Cortex should discover and rank available actions across tool services and local skills rather than hardcode one-off integrations.

Current design:
- discover service tools from `services/*/service.json` at boot
- discover local skills from `skills/*/SKILL.md` at boot
- keep the catalog only in transient Cortex memory for now
- inject top candidate action summaries only on generation turns
- inject nothing on deterministic tool-call turns

Each action record should include:
- name
- kind
- summary
- owner
- input schema summary
- tags
- embedding later

Skills are guidance-first in the current phase. Later they should move to a hybrid model where some skills become executable workflows.

### Tool Router

Routes tool calls to owning services over MCP-lite.

Initial services:
- `memory`
- tool services such as `browser`
- tool services such as `sandbox`

Later:
- additional platform and compute services

### STM Manager

Segmented STM design currently intended:
- system core
- active objective
- active plan snapshot
- conversation context
- tool interaction log
- reasoning scratchpad
- observation buffer
- curiosity queue

Only selected segments should be compacted.

### Reflection

Future reflection responsibilities:
- cross-thread synthesis
- well-supported hypothesis generation
- contradiction detection handoff
- research digests
- curiosity queue generation

## Memory Relationship

Cortex should use the memory service, not absorb it.

Memory service logical layers:
- STM support data if needed
- LTM in LanceDB
- KB in markdown vault with graph links
- Diary as markdown on disk plus metadata/index support

Cortex responsibilities toward memory:
- request retrieval
- write episodes
- trigger promotion/compaction workflows
- consume memory bundles during reasoning
- hand off deterministic diary writes after each completed tool cycle

Memory service responsibilities:
- storage
- indexing
- graph parsing
- clustering support
- knowledge persistence
- diary persistence and diary indexing

## AutoAgents Integration Architecture

Cortex uses [AutoAgents](https://github.com/liquidos-ai/AutoAgents) (v0.3.6) as its agent execution framework. The integration uses the framework as the **runtime** — `BaseAgent::run()` is the entry point, `AgentExecutor::execute()` contains the ReAct loop, and all `AgentHooks` lifecycle methods fire. One part of the framework is intentionally bypassed: the built-in `TurnEngine`/`ReActAgent` executor (see below).

### Crates adopted

| Crate | Role in Cortex |
|---|---|
| `autoagents-llm` | Unified `LLMProvider` trait — replaces manual `reqwest` LLM calls. Provider built once at `BaseAgent::new()`, reused for all loop iterations via `context.llm().chat_stream()`. |
| `autoagents-core` | `BaseAgent`, `AgentDeriveT`, `AgentExecutor`, `AgentHooks`, `MemoryProvider`, `Context`. Full framework runtime — `base_agent.run(task)` is the step entry point. |
| `autoagents-protocol` | `Event` type used for `BaseAgent::new()` channel construction only. |

### Crates deliberately excluded

| Crate | Why excluded |
|---|---|
| `autoagents-core::prebuilt::ReActAgent` + `TurnEngine` | Requires native LLM tool-calling format (`function_call`/`tool_use`). Local sub-30B models don't reliably produce this. Cortex uses JSON text output format instead. |
| `autoagents-derive` | Tool inputs are `serde_json::Value` dispatched over UDS — proc macros add no safety at this boundary. |
| `autoagents-telemetry` | OpenAgent has its own OTEL pipeline via `sdk-rust` (file-based OTLP/JSON, daily rotation). |

---

### CortexAgent — framework runtime, custom ReAct loop

`BaseAgent::run(Task)` is the entry point. The framework fires hooks in order:

```
base_agent.run(task)
  → on_run_start(context)
  → AgentExecutor::execute(task, context)   ← our ReAct loop lives here
      for iteration in 0..MAX_REACT_ITERATIONS:
          on_turn_start(iteration, context)
          context.llm().chat_stream(messages, ...)  ← reuses pre-built provider
          parse JSON output → "final" | "tool_call"
          if tool_call:
              on_tool_call(llm_tool_call, context)  → HookOutcome::Continue | Abort
              on_tool_start(...)
              self.router.call(tool_name, args)      ← UDS dispatch, not ToolProcessor
              on_tool_result(...) | on_tool_error(...)
          on_turn_complete(iteration, context)
      return ReActOutput
  → on_run_complete(output, context)
```

Why NOT the framework's `TurnEngine`/`ReActAgent`: AutoAgents' built-in ReAct executor dispatches tools via `context.tools()` using native LLM function-calling API. Local sub-30B models (Qwen, Llama, Mistral) don't reliably emit `tool_use` responses. Cortex instructs the model to output exactly one JSON object per turn (`{"type":"tool_call",...}` or `{"type":"final",...}`) and dispatches via `ToolRouter` over UDS. Everything else — `BaseAgent`, `MemoryProvider`, `AgentHooks` — is used as designed.

```rust
pub struct CortexAgent {
    agent_name: String,
    system_prompt: String,          // pre-built with JSON format instructions
    action_context: Option<String>, // candidate tool summaries for generation turns
    provider_config: ProviderConfig, // for telemetry labels
    tools: Vec<Box<dyn ToolT>>,     // AgentDeriveT compliance; empty at runtime
    router: Arc<ToolRouter>,        // UDS dispatch: "browser.open" → browser.sock
}

impl AgentDeriveT for CortexAgent {
    type Output = ReActOutput;      // Serialize + DeserializeOwned + AgentOutputT
    fn output_schema(&self) -> Option<Value> { Some(ReActOutput::structured_output_format()) }
    fn tools(&self) -> Vec<Box<dyn ToolT>> { vec![] }  // ToolRouter handles dispatch
}

impl AgentExecutor for CortexAgent {
    type Output = ReActOutput;
    type Error = CortexAgentError;  // → RunnableAgentError::ExecutorError via From<>
    fn config(&self) -> ExecutorConfig { ExecutorConfig { max_turns: 10 } }
    async fn execute(&self, task: &Task, context: Arc<Context>) -> Result<ReActOutput, CortexAgentError> {
        // Full ReAct loop — see src/agent.rs
    }
}

impl AgentHooks for CortexAgent {}  // all no-ops in Phase 2; Phase 3 overrides on_run_complete
```

---

### HybridMemoryAdapter — MemoryProvider implementation

`HybridMemoryAdapter` (`src/memory_adapter.rs`) implements AutoAgents' `MemoryProvider` trait:

- **STM:** AutoAgents `SlidingWindowMemory` (`TrimStrategy::Drop`, `DEFAULT_STM_WINDOW = 40` messages). Eviction intercepted: when window full, oldest message is dumped to `data/stm/{session_id}/{unix_ms}_eviction.md` before `SlidingWindowMemory` pops it. `clear()` dumps full window to `{unix_ms}_clear.md`.
- **LTM:** `memory.search` via `ToolRouter` on `memory.sock`. Query is `user_input` (semantic signal). Gracefully no-ops when memory service is down.
- **Recall:** `[ltm_hits…, stm_window…]` — LTM prepended as background context, STM as recent window.

`SlidingWindowMemory` (40-message window) is the permanent STM implementation.

---

### Tool dispatch — string-keyed via ToolRouter, not ToolProcessor

The LLM sees a fixed candidate set injected as text in the system prompt (not as native tool schemas). When the model outputs `{"type":"tool_call","tool":"browser.open","arguments":{...}}`:

1. `parse_step_model_output()` extracts `tool` name and `arguments`
2. `on_tool_call()` hook fires — can abort
3. `self.router.call(tool_name, &arguments)` dispatches over UDS to the owning service
4. `on_tool_result()` or `on_tool_error()` fires
5. Result injected back as the next user message

`ToolRouter` uses prefix-based routing: `browser.*` → `browser.sock`, `sandbox.*` → `sandbox.sock`, `memory.*` → `memory.sock`. AutoAgents' `ToolProcessor::process_tool_calls()` is not used — tool names are strings at runtime, not compile-time types.

---

### Multi-agent — deferred to Phase 6

Named agents from `config/openagent.yaml` are config-selectable via `agent_name` in the step request. A single `CortexAgent` is constructed per-request from the selected config block. Actor dispatch (`ractor`) deferred until memory and tool layers are stable.

---

## Current Library Set

Libraries in use:
- `sdk-rust` — MCP-lite server and shared OTEL setup
- `tokio` — async service runtime
- `serde`, `serde_json`, `serde_yaml` — protocol payloads and config loading
- `anyhow` — process-level error handling
- `autoagents-llm` — unified LLM provider trait; streaming via `chat_stream()`
- `autoagents-core` — `BaseAgent`, `AgentDeriveT`, `AgentExecutor`, `AgentHooks`, `MemoryProvider`
- `autoagents-protocol` — `Event` type for `BaseAgent` channel construction
- `async-trait` — async trait support for AutoAgents impls
- `futures` — stream accumulation for LLM streaming
- `tower` — `CortexTraceLayer` + `TimeoutLayer` middleware stack
- `tracing`, `opentelemetry`, `tracing-opentelemetry` — observability

Libraries planned for later phases:
- `axum` — HTTP/UDS control plane transport (Phase 4 endgame only)

Libraries intentionally avoided:
- `autoagents-core::prebuilt::ReActAgent` / `TurnEngine` — requires native LLM tool-calling; local models don't support this reliably
- `autoagents-derive` — tool inputs are `Value` over UDS; proc macros add no benefit
- `autoagents-telemetry` — own OTEL via `sdk-rust`
- embedded vector storage inside Cortex
- direct browser/memory/sandbox implementation inside Cortex
- `axum` or `tower` in any service other than Cortex

## Phase 1 Tools

`cortex.describe_boundary`

Returns a JSON document describing:
- service ownership
- non-goals for Phase 0
- the transport contract
- the Phase 1 dependency set

`cortex.step`

Request:
- `session_id`
- `user_input`
- `agent_name` (optional)

Behavior:
- loads OpenAgent config from `OPENAGENT_CONFIG_PATH`, `config/openagent.yml`, or `config/openagent.yaml`
- selects the requested agent or falls back to the first configured agent
- reads `system_prompt` from config
- sends `system_prompt` + `user_input` to the configured provider
- returns plain response text plus provider metadata

Observability:
- traces: `logs/cortex-traces-YYYY-MM-DD.jsonl`
- metrics: `logs/cortex-metrics-YYYY-MM-DD.jsonl`
- logs: `logs/cortex-logs-YYYY-MM-DD.jsonl`

## Diary Layer

In addition to KB and episodic memory, the architecture includes a diary layer.

Purpose:
- human-readable audit trail
- request and response captured in English prose-like markdown
- searchable by HITL only
- never injected into normal agent reasoning context

Design:
- diary entry content stored as markdown on disk
- diary semantic index stored in LanceDB
- LanceDB diary index stores only reference-oriented summary fields, not full diary content

Recommended diary markdown contents:
- request
- response
- tool activity summary
- validator status
- optional flags

Recommended storage split:
- `md` is the human-readable source of truth
- `LanceDB` stores summary/index rows and file references for HITL semantic scan

Important rule:
- diary is excluded from normal Cortex retrieval and prompt hydration
- diary is only referenced by HITL scan/review workflows

Generation strategy:
- deterministic template only
- no extra LLM call for diary generation
- diary is dumped directly from request/response/tool state
- diary indexing can run asynchronously when the system is not under load

Suggested path shape:
- `data/diary/<session_id>/<timestamp>-<turn_id>.md`

## Prompt Management

Prompts should be runtime-loaded configuration, not compiled into Rust binaries.

Recommended pattern:
- YAML prompt files
- per-subsystem prompt folders
- template rendering at runtime
- versioned prompt metadata

Likely prompt families:
- planning
- memory compaction
- tool selection
- reflection
- contradiction review preparation

## Protocol

MCP-lite over JSON + UDS remains the preferred protocol.

Why:
- local machine
- low latency
- simple debugging
- language independence
- matches existing service direction in the repo

Recommended message shape:

```json
{
  "id": "uuid",
  "type": "tool.call",
  "tool": "browser.search",
  "params": {
    "query": "POLG mutation DNA repair"
  }
}
```

## Migration Strategy

This is an evolution, not a rewrite. Each phase is independently shippable.

**Phase 1 (now):**
- Keep Python outer loop
- Route each turn to Cortex via `cortex.step` MCP-lite call
- Python middleware (STT, whitelist) stays outside Cortex

**Phase 2 — Tower middleware begins:**
- Cortex owns tool routing (memory, browser, sandbox)
- Introduce `tower::ServiceBuilder` inside Cortex
- Begin porting Python middleware to `tower::Layer` (whitelist first, then STT/TTS)
- Python middleware removed as each Tower layer ships and passes integration tests

**Phase 3 — Cortex owns the loop:**
- Cortex owns the full ReAct loop (LLM → tool → LLM iterations)
- Python becomes a thin launcher: config load, service spawn, platform adapter glue
- Full Tower middleware stack active inside Cortex

**Phase 4 — Axum control plane (endgame):**
- `axum` over UDS replaces the Python process
- Platform connectors (Discord, Telegram, Slack) wire directly to Cortex/Axum
- `service.json` manifest and MCP-lite protocol unchanged for all other services
- Python retired

**Stability guarantee:** The MCP-lite UDS socket contract is stable across all phases. Downstream services never change protocol because the control plane above them is being replaced.

## Scope for MVP

Do not build the full cognition stack at once.

The first useful Cortex should only do:
- receive session step request
- retrieve memory context
- call LLM
- execute tool call
- return result

Planning, reflection, contradiction handling, curiosity, and advanced memory lifecycle should come later.
