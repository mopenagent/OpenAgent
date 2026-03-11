# Cortex TODO

Phased implementation plan for Cortex as the future Rust orchestrator service.

## Phase 0: Capture the Boundary

- Finalize Cortex as a separate service, not an embedded OpenAgent module.
- Keep current Python loop as a temporary shell.
- Treat Python middleware such as STT and whitelist as pre-Cortex middleware for now.
- Lock Cortex transport to MCP-lite over JSON + UDS.
- Define Cortex as the only component allowed to call the LLM in the target architecture.

## Phase 1: Cortex Skeleton MVP

Goal: replace the current agent loop with a minimal Cortex step.

- Create `src/main.rs`
- Add MCP-lite server bootstrap using `sdk-rust`
- Expose a single step-style tool or request path for session execution
- Add request/response schemas for:
  - session id
  - user input
  - response text
  - optional tool activity summary
- Implement basic prompt builder
- Implement LLM client boundary
- Return plain answer without tools or planning first
- Add OTEL spans, metrics, and structured logs

Exit criteria:
- Python shell can send one message to Cortex and get one response back

## Phase 2: Tool Routing Baseline

Goal: let Cortex execute tools directly.

- Add `tool_router` module
- Add static tool registry first
- Add service client wrappers for:
  - memory
  - tool services such as `browser`
  - tool services such as `sandbox`
- Define structured LLM tool-call output contract
- Validate tool names and arguments before execution
- Append tool result back into the reasoning loop
- Record tool call telemetry

Exit criteria:
- Cortex can complete one LLM -> tool -> LLM round-trip

## Phase 3: Memory Retrieval and Episode Writes

Goal: make Cortex memory-aware.

- Add `memory_client` module
- Add unified memory search request contract
- Retrieve memory bundle before LLM reasoning
- Inject memory bundle into prompt assembly
- Add episodic memory write after significant results
- Capture LLM output after each completed cycle and run validator before downstream memory feedback
- Add deterministic diary write event after each completed tool cycle
- Add session-linked memory references in logs/telemetry

Exit criteria:
- Cortex can read from memory before reasoning, validate output, write an episode, and emit a diary event after execution

## Phase 4: Prompt System

Goal: externalize prompts and stop hardcoding cognitive instructions.

- Add prompt loader
- Use YAML prompt files
- Support runtime template rendering
- Add prompt version metadata
- Create initial prompt families:
  - step reasoning
  - tool selection
  - memory compaction handoff
  - plan update

Exit criteria:
- Cortex loads prompts from files without recompilation

## Phase 4A: Diary Store and Index

Goal: capture human-readable request/response history without polluting normal memory retrieval.

- Define diary markdown path convention
- Define deterministic diary template
- Persist request and response in markdown
- Add LanceDB diary index storing only:
  - entry id
  - session id
  - timestamp
  - short summary
  - keywords
  - file path
  - validator status
  - flags
- Ensure diary indexing is asynchronous and can be deferred when the system is under load
- Ensure diary search is only exposed to HITL/audit workflows

Exit criteria:
- Every completed cycle produces a deterministic markdown diary entry plus a LanceDB reference index row, and diary entries can be semantically scanned by HITL without being used in normal context injection

## Phase 5: Tool Search

Goal: avoid exposing every tool to the LLM at every step.

- Add `tool_registry` module
- Treat tool discovery as the main abstraction rather than direct service naming
- Define tool metadata schema:
  - name
  - description
  - tags
  - owning service
  - schema summary
  - embedding
- Add tool embedding/index build process
- Implement top-k tool search
- Ensure browser and sandbox register many tools through the same discovery path
- Pass only candidate tools into the LLM context

Exit criteria:
- Cortex can search tools semantically and expose only a limited candidate set

## Phase 6: Plan Store and DAG

Goal: give Cortex persistent control state.

- Add SQLite-backed plan store
- Add tables:
  - plans
  - tasks
  - task_dependencies
  - tool_calls
  - turns
  - sessions
- Add runnable-task selection
- Add plan snapshot injection into prompt
- Update plan after each tool call or step
- Keep a compact active plan summary in STM or step state

Exit criteria:
- Cortex can resume a multi-step task across turns

## Phase 7: Segmented STM

Goal: preserve working cognition shape instead of a flat buffer.

- Introduce segmented STM state:
  - system core
  - active objective
  - active plan snapshot
  - conversation context
  - tool interaction log
  - reasoning scratchpad
  - observation buffer
  - curiosity queue
- Add per-segment size budgets
- Define which segments compact and which never compact
- Keep STM local to Cortex-managed runtime state

Exit criteria:
- Cortex prompt assembly reads from segmented STM rather than one flat transcript

## Phase 8: Reflection

Goal: add background cognition after the main loop is stable.

- Add reflection scheduler
- Add cross-thread synthesis requests
- Add well-supported hypothesis generation
- Add research digest generation
- Add contradiction candidate generation for HITL

Exit criteria:
- Cortex can periodically synthesize research state without disrupting core task execution

## Phase 9: Curiosity and Investigation Queue

Goal: enable research collaborator behavior.

- Add curiosity queue generation
- Add confidence-gated autonomous exploration levels
- Keep suggestion output non-intrusive
- Present optional research leads rather than forcing direction changes

Exit criteria:
- Cortex can surface research leads as suggestions instead of direct interruptions

## Phase 10: Harden the Service Boundary

- Add retries/timeouts per dependent service
- Add degraded-mode behavior when memory or tool services are unavailable
- Add replay-friendly step logs
- Add trace correlation across LLM, tools, and memory
- Add protocol versioning notes

Exit criteria:
- Cortex survives partial subsystem failures without corrupting control state

## Deferred by Design

Not for early MVP:
- full contradiction arbitration
- concept canonicalization
- knowledge decay management inside Cortex
- moving the outer loop fully to Rust
- splitting memory into multiple services
- dynamic distributed scheduling

## Immediate Next Steps

1. Create `src/main.rs` and service bootstrap
2. Define session step request/response contract
3. Implement LLM client boundary
4. Add static tool router for memory and tool services such as browser/sandbox
5. Wire Python shell to call Cortex instead of the old loop
