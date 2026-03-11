# Cortex Service

Rust service target for OpenAgent's cognitive control plane. Cortex is the future agent loop, planner, retrieval orchestrator, and tool router. It does not store all knowledge itself. Instead, it coordinates:

- LLM service
- memory service
- tool services

OpenAgent's current Python outer loop is treated as a temporary shell that will call Cortex. The final architecture keeps Cortex as a separate service, not embedded in the Python app.

## Phase 0 Status

Phase 0 is implemented as a boundary capture, not a reasoning engine.

What exists now:
- Cortex is a standalone Rust MCP-lite service.
- Transport is locked to JSON over Unix Domain Sockets.
- Service identity is declared in `service.json`.
- A single introspection tool, `cortex.describe_boundary`, reports the frozen boundary.

What does not exist yet:
- session-step execution
- LLM HTTP client
- tool routing
- memory retrieval
- planner / DAG store
- STM segmentation

This keeps Phase 0 aligned with [`TODO.md`](./TODO.md): define the service boundary before adding cognition.

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

### Tool Registry and Tool Search

Because the number of tools will grow, Cortex should not expose the full tool set to the LLM every cycle.

Instead:
- maintain a tool registry
- embed tool descriptions
- search top-k candidate tools for the current task
- pass only a small candidate set to the LLM

Tool discovery is the main abstraction, not service names. Browser and sandbox are important because they expose many tools each. Cortex should discover and rank available tools across tool services rather than treat browser and sandbox as special hardcoded one-off integrations.

Each tool record should include:
- name
- description
- owning service
- input schema summary
- tags
- embedding

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

## Phase 0 Library Set

Libraries used now:
- `sdk-rust` for the MCP-lite server and shared OTEL setup
- `tokio` for the async service runtime
- `serde` and `serde_json` for protocol payloads
- `anyhow` for process-level error handling
- `tracing`, `opentelemetry`, and `tracing-opentelemetry` for observability

Libraries planned for Phase 1:
- `reqwest` with `rustls-tls` for async LLM HTTP calls
- `uuid` for request/session correlation where the service generates identifiers

Libraries intentionally avoided:
- agent frameworks
- embedded vector storage inside Cortex
- direct browser/memory/sandbox implementation inside Cortex

## Phase 0 Tool

`cortex.describe_boundary`

Returns a JSON document describing:
- service ownership
- non-goals for Phase 0
- the transport contract
- the Phase 1 dependency set

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

Short term:
- keep Python outer loop
- route each turn to Cortex
- keep existing Python middleware such as STT and whitelist outside Cortex

Medium term:
- Cortex becomes the true agent loop
- Python becomes shell/UI only

Long term:
- outer loop may also move to Rust
- Cortex remains the same service boundary

## Scope for MVP

Do not build the full cognition stack at once.

The first useful Cortex should only do:
- receive session step request
- retrieve memory context
- call LLM
- execute tool call
- return result

Planning, reflection, contradiction handling, curiosity, and advanced memory lifecycle should come later.
