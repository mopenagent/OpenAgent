# AGENTS.md

This file defines how coding agents should work in this repository.

## Mission

Build OpenAgent as a deterministic, extension-first Python + Go hybrid agent platform that orchestrates multi-agent pipelines on offline 14B-class models. Primary deployment target: Raspberry Pi / low-power hardware.

## Agentic Layer

- **No framework dependency** — custom ReAct loop and thin httpx-based provider layer. This gives full control over tool schema format, retry logic, and iteration limits for sub-30B models (14B Llama, Qwen, Mistral).
- **Session/memory** — `SessionBackend` protocol (SQLite now; Go/Rust service later). Optional summarisation hooks. Agno is inspiration only — we do not use it as a dependency.

## Source Of Truth

- Full development guide: [`CLAUDE.md`](./CLAUDE.md)
- Project context and intent: [`CURSOR.md`](./CURSOR.md)
- Build order and gaps: [`roadmap.md`](./roadmap.md)
- Reference implementations: `inspire/openclaw/` (TypeScript), `inspire/nanobot/` (Python), `inspire/picoclaw/` (Go)

When in doubt: `CLAUDE.md` > `CURSOR.md` > `roadmap.md` > reference implementations.

## Two-Plane Architecture

OpenAgent has two distinct planes. Do not mix their responsibilities.

| Plane | Language | Location | Responsibility |
|---|---|---|---|
| Control Plane (Brain) | Python | `openagent/`, `extensions/` | LLM interfacing, orchestration, platform integrations |
| Data Plane (Hands) | Go (or compiled) | `services/` | Long-lived service daemons for compute/data-intensive work |

The two planes communicate via **MCP-lite**: tagged JSON frames over Unix Domain Sockets (`data/sockets/<name>.sock`).

## Architecture Rules

### 1. Communication Protocol (User Preference)
- Whenever the user sends an input where their intention needs clarification or the context needs expansion, **do not assume the correct path.**
- Ask clarifying questions **one by one** (1-by-1).
- Provide possible **options/paths** for the user to choose from.
- Record and apply this explicitly in every conversation.

### 2. Keep core minimal
- Core lives in `openagent/`.
- Core is responsible for: extension discovery, service discovery, lifecycle management, shared interfaces, message bus, and agent loop orchestration.
- Do not add domain-specific logic or heavy third-party dependencies to core.

### 2. Python extensions = platform integrations only
- Extensions under `extensions/<name>/` are for **platforms and media** (WhatsApp, Discord, TTS, STT).
- Extensions must be independently installable and register via entry points in `openagent.extensions`.
- Extensions must be first-class async and event-driven.
- Do not put CPU/IO-heavy compute in Python extensions — that goes in Go services.
- **Workflow Orchestrator:** Python acts as a workflow orchestrator. A deterministic chain (e.g. WhatsApp -> STT -> LLM) saves tokens, memory, and latency. The LLM is just one node in Python's workflow graph.

### 3. Go services = compute and data
- New compute-heavy or data-intensive capabilities go in `services/<name>/`.
- Each service is a self-contained Go module with a `service.json` manifest.
- Services communicate with the Python core via MCP-lite (not REST, not gRPC).
- The Python `ServiceManager` owns the lifecycle: spawn, health-check, restart, shutdown.
- Services never call back into Python — they only respond to requests and push events.

### 4. Service manifest is the only contract
- `service.json` is the schema-first contract between core and service.
- Core must not depend on service internals — only on the manifest and the wire protocol.
- A service can be rewritten in any language without changing core, as long as the manifest and protocol are honoured.
- **Zero-Copy Artifact Passing:** Services write raw binary data directly to disk (`data/artifacts/`). They pass only lightweight JSON strings with the file path back to Python over the MCP-lite socket (`{"path": "/data/artifacts/xxx.mp3"}`). No heavy binary data over sockets. Python routes this artifact between services.

### 5. AgentLoop Middleware (Hooks)
- Use `AgentMiddleware` to intercept `InboundMessage`s before or after LLM processing.
- Middleware hooks are **manually wired** when instantiating the `AgentLoop`.
- Perfect for cross-cutting processing like auto-transcribing audio (STT), computer vision parsing, or strict logging without polluting the ReAct core.

### 5. Tool-oriented design
- Expose capabilities as tools the LLM can call.
- Keep tool schemas stable, clear, and deterministic — write descriptions for a 14B model to understand.
- Python tools: in-process Python callables registered with the agent loop.
- Go service tools: declared in `service.json`, proxied through `ServiceManager`.

### 6. Deterministic behavior
- Prefer explicit control flow over hidden side effects.
- Keep initialization and execution paths reproducible and testable.
- Max agent loop iterations: 40. Truncate large tool results to 500 chars. Both configurable.

## Python and Packaging

- Minimum supported Python: `>=3.11`
- Core package name: `openagent-core`
- Editable installs for local development:
  - `pip install -e .`
  - `pip install -e extensions/<name>`
- `aiohttp` for all external HTTP/API calls — no `requests`, no OpenAI SDK
- `asyncio.to_thread()` when integrating sync libraries inside async flows
- Pydantic for config and data models

## Go and Services

- Go minimum version: 1.21+
- Each service: standalone Go module (`go.mod`) in `services/<name>/`
- Socket path received via env var `OPENAGENT_SOCKET_PATH`
- Goroutine per request — never block the accept loop
- Graceful SIGTERM: drain in-flight requests, close socket, exit 0
- Cross-compile targets:
  - `GOOS=linux GOARCH=arm64` — Raspberry Pi (primary)
  - `GOOS=linux GOARCH=amd64` — Ubuntu server
  - `GOOS=darwin GOARCH=arm64` — M4 Mac (dev)
- Compiled binaries in `bin/` at project root (gitignored)

## MCP-lite Wire Protocol

One Unix socket per service. Newline-delimited JSON frames, bidirectional.

```
Agent → Service:  {"id":"<uuid>","type":"tools.list"}
                  {"id":"<uuid>","type":"tool.call","tool":"<name>","params":{...}}
                  {"id":"<uuid>","type":"ping"}

Service → Agent:  {"id":"<uuid>","type":"tools.list.ok","tools":[...]}
                  {"id":"<uuid>","type":"tool.result","result":"...","error":null}
                  {"id":"<uuid>","type":"pong","status":"ready"}
                  {"id":"<uuid>","type":"error","code":"...","message":"..."}
                  {"type":"event","event":"<name>","data":{...}}   ← no id, unprompted
```

## Repository Layout

```
openagent/      # Core Python (minimal)
  tests/             # Core Python tests (including platforms/)
extensions/         # Python platform integrations (independently installable)
  <name>/tests/      # Extension-local tests (self-contained verticals)
services/           # Go service daemons (each with go.mod + service.json)
app/                # Minimalist web UI (FastAPI + HTMX, no auth — POC only)
  tests/             # Web UI tests (route-level and app-level)
data/               # Runtime storage: sessions.db, memory/, sockets/, artifacts/
config/             # openagent.yaml (primary config)
inspire/            # Reference implementations (gitignored)
```

- Extension source layout: flat at `extensions/<name>/src/` — no nested package folders
- Service source layout: `services/<name>/main.go` + `services/<name>/service.json`
- `data/` is gitignored — created at runtime

## Web UI (`app/`)

- Lives in `app/` at repo root — a standalone FastAPI package (`openagent-app`)
- Stack: FastAPI 3.x, Jinja2 templates, HTMX, Tailwind CSS via CDN, WebSockets, SSE
- **No authentication** — POC for an isolated Raspberry Pi on a private network
- No JS framework, no build step — vendor HTMX as a static file
- `app/` imports from `openagent-core`; core never imports from `app/`
- Do not add UI logic or FastAPI routes to `openagent/`

## Naming Rules

- **extension** — Python platform/media integration (`extensions/`)
- **service** — Go (or compiled) long-lived daemon (`services/`)
- **tool** — Python in-process callable or Go service capability declared in `service.json`
- **worker** — Python async background task
- Do NOT use: sidecar, plugin (except `plugin.py` entrypoint convention), engine
- Keep `plugin.py` as the per-extension entrypoint filename convention
- In core: prefer `load_extensions`, `ServiceManager`, extension-oriented naming

## Agent File Rule

- `AGENTS.md` must remain present and non-empty.
- Do not replace this file with placeholders or empty sections.

## Coding Standards

- Small, composable modules with clear interfaces
- Type hints on all public APIs
- No global mutable state
- Logging concise and useful for debugging
- ASCII output by default
- Every I/O or network operation must be non-blocking

## Testing Standards

- Core tests: `openagent/tests/` (including `openagent/tests/platforms/`)
- App tests: `app/tests/`
- Extension tests: `extensions/<name>/tests/` only (self-contained per extension)
- Go service tests: `services/<name>/` (Go `_test.go` files)
- Mock Go services in Python tests with a minimal asyncio socket stub that speaks MCP-lite
- No real network calls in tests, no real LLM calls in tests
- `pytest-asyncio` for async Python tests
- Every new core behaviour: tests covering discovery/loading, initialization, key execution paths
- Do not add active test suites under project-root `tests/`; keep tests inside their owning vertical.

## Change Discipline

- Do not break entry-point based extension discovery
- Do not hard-code extension names or service names in core
- `service.json` is the only contract — core must not know service internals
- Prefer backward-compatible interface evolution
- If deviating from OpenClaw/Nanobot patterns, document why in comments or PR notes

## Agent Workflow

1. Read `CLAUDE.md` first, then `CURSOR.md` before substantial changes.
2. Consult `roadmap.md` for consolidated Nanobot/Picoclaw comparison and build order.
3. Determine: is this a platform/media concern (Python extension) or a compute concern (Go service)?
4. Keep core changes minimal; push feature logic to extensions or services.
5. Update/add tests in the appropriate `tests/` tree (Python) or `services/<name>/` (Go).
6. Keep docs in sync: `README.md`, `CLAUDE.md`, `CURSOR.md`, `roadmap.md`, extension/service metadata.

## Observability Standards

- Keep observability first-class across all verticals: `openagent/`, `extensions/`, `services/`, and `app/`.
- Python logs should use structured JSON via `openagent/observability/` helpers.
- Every MCP-lite request path should emit correlation id (`id`), operation, status, and duration.
- Prometheus metrics are exposed at `/metrics` from the web app and must include extension/provider and MCP-lite request latency/error counters.
- Avoid logging raw message text or sensitive credentials; log payload sizes, identifiers, and status instead.
- Go services should emit structured JSON logs per request with `service`, `request_id`, `tool`, `outcome`, and `duration_ms`.
