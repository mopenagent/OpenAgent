# Antigravity Context: OpenAgent Architecture

This file serves as a durable record of the architectural consensus reached regarding OpenAgent, specifically focusing on the deployment realities for Raspberry Pi and the hybrid Python/Rust/Go split (Rust-first; only WhatsApp remains in Go).

## 1. Communication Protocol (User Preference)
Whenever the user sends an input where their intention needs clarification or the context needs expansion, **do not assume the correct path.** 
- Ask clarifying questions **one by one** (1-by-1).
- Provide possible **options/paths** for the user to choose from.
- Record and apply this explicitly in every conversation regarding architecture and implementation.

## 2. The Workflow Engine Paradigm
OpenAgent is distinct from typical agentic loops because the LLM is **not** the center of the universe.
- Python acts as a deterministic **Workflow Orchestrator**.
- The LLM is just one non-deterministic node in the graph.
- A deterministic chain (e.g. `WhatsApp IN -> STT -> LLM -> TTS -> WhatsApp OUT`) is preferred over giving the LLM raw tools and asking it to figure out what to do. This saves latency and context window logic on local models.

**LLM context:** The primary LLM is an **external model with a 36K token context window**. Tool injection overhead is ~900 tokens/prompt (8 semantic candidates + 2 pinned tools + cortex.discover), leaving ~35K for conversation, STM, and system prompt. Token pressure is not a concern at this scale — do not add token-reduction complexity unless profiling proves otherwise.

## 3. Zero-Copy Artifact Routing
IPC (Inter-Process Communication) JSON serialization is a massive tax on low-power hardware. 
- **Rule:** Never send binary data, large text blobs, or heavy arrays over the MCP-lite Unix Sockets.
- **Solution:** Rust/Go services write data directly to the shared filesystem (`data/artifacts/`). They emit a tiny JSON payload containing only the pointer/path (`{"path": "/data/artifacts/audio_123.ogg"}`).
- Python receives this path and passes it as an argument to the next service in the workflow. 

## 4. No East-West Mesh
Rust and Go services **never** talk to each other directly.
- All routing goes strictly through the `openagent` control plane (Rust binary).
- This prevents microservice spaghetti and ensures the control plane remains the absolute Source of Truth for the state of an agent's workflow.
- Exception: in the multi-agent setup, worker Cortex instances call the Research service directly to update task state. This is the only sanctioned east-west call — Research is the shared blackboard, not a peer service.

## 5. Hardware Realism (The Pi-First Mindset)
The platform is designed to run entirely on a single 8GB Raspberry Pi (with the LLM/Vector DB potentially running on a dedicated local API/GPU).
- **Vector DB (LanceDB):** Uses a direct Python client wrapper to leverage LanceDB's fast, native Rust core. We do *not* isolate this into a Rust service initially, as that would introduce the JSON IPC serialization tax for massive vector arrays. We only shift it if profiling shows it aggressively blocking the `asyncio` event loop.
- The philosophy is: Build for a single node, monitor, profile, and optimize. Only distribute if absolutely necessary.

## 6. Multi-Agent Model: Supervisor/Worker (Anthropic Pattern)

OpenAgent adopts **Anthropic's Supervisor/Worker** as its canonical multi-agent pattern.

- **Cortex is the Supervisor.** It holds the Research DAG, picks the next runnable task, selects which worker agent handles it, and synthesises results.
- **Workers are stateless per invocation.** A worker Cortex receives: task description + prior task results + scoped tool set. It returns one result string. No session state carried between worker calls.
- **The supervisor handles simple tasks itself.** Cortex only launches a worker agent when the task genuinely benefits from specialisation — long-running retrieval, code execution, or analysis that warrants a dedicated context window. For short, self-contained steps it executes the tool calls directly in its own ReAct loop.
- **Workers are tools from the supervisor's perspective.** The supervisor calls `cortex.step` with `agent_name` the same way it calls `browser.search`. No specialised worker protocol.
- **Research service is the shared blackboard.** Both supervisor and workers call `research.task_done`, `research.task_add` over MCP-lite. This is the only sanctioned east-west call in the system.
- **Agent identity is config, not code.** Each named agent in `config/openagent.yaml` has its own `system_prompt`, `model`, and optional `assigned_agent` on research tasks. Adding an agent = editing YAML, not recompiling.

```
Cortex Supervisor (strong model)
  ├─ research.status → picks next runnable task
  ├─ cortex.step(agent_name="search-agent") → SearchAgent executes, returns result
  ├─ research.task_done(task_id, result)
  └─ cortex.step(agent_name="analysis-agent") → AnalysisAgent synthesises
```

## 7. Granular Edge Observability
- **Modular Data Plane:** All Rust service logic (the Hands) is decoupled into strictly separated `handlers`, `tools`, `state`, and `metrics` modules.
- **Native Telemetry:** Telemetry is explicitly wired up through the overarching `sdk-rust/otel.rs` implementation natively emitting granular tracing spans for all execution paths.
- **Trace Ingestion:** We embrace Jaeger UI to ingest these traces, giving the Orchestrator high visibility into sub-component latencies and success metrics without the overhead of heavy polling solutions.
