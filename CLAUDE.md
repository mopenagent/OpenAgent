# CLAUDE.md — OpenAgent Development Guide

## What We're Building

**OpenAgent** is a deterministic, extension-first agent platform with a **hybrid Python + Rust architecture**:

- **Python Control Plane (the Brain)** — LLM interfacing, multi-agent orchestration, state management, and high-level reasoning. Stateless asyncio core loop. Python is the babysitter.
- **Rust Services (the Hands)** — Long-lived service daemons for CPU/IO-intensive work: platform connectors (discord; telegram, slack in transition), compute (sandbox, stt, tts), automation (browser), memory. **Rust-first** — all new services are Rust.
- **Go** — Only WhatsApp (`services/whatsapp/`) remains in Go (whatsmeow). No new Go services. Telegram and Slack are still Go but will migrate to Rust.

The two planes communicate via a **MCP-lite wire protocol** (tagged JSON frames over Unix Domain Sockets). Services run as persistent daemons; the agent's `ServiceManager` owns the full lifecycle: spawn, health-check, restart, graceful shutdown.

Primary deployment target: **Raspberry Pi / low-power hardware** (Rust compiles to arm64; Python core stays lean).

## Communication Protocol (Rule #1)
Whenever the user sends an input where their intention needs clarification or the context needs expansion, **do not assume the correct path.** Ask clarifying questions **one by one** (1-by-1) and provide possible **options/paths** for the user to choose from. Apply this explicitly in every conversation.

## Agentic Layer

OpenAgent uses a **custom ReAct loop** and thin httpx-based provider layer — no framework dependency. This gives full control over tool schema format, retry logic, and iteration limits for sub-30B models. Session/memory uses a `SessionBackend` protocol (SQLite now; Go/Rust service later). See `roadmap.md` for rationale and build order.

## Reference Implementations

Read these before implementing anything non-trivial. Prefer patterns from these codebases.

| Reference | Language | Role | Path |
|-----------|----------|------|------|
| **OpenClaw** | TypeScript | Functionality — agent logic, orchestration, tool/extension patterns | `inspire/openclaw/` |
| **Nanobot** | Python | Structure — project layout, agent loop, provider registry, config schema | `inspire/nanobot/` |
| **Picoclaw** | Go | Multi-agent registry, service/tool patterns | `inspire/picoclaw/` |

Key files to consult:
- Agent loop → `inspire/nanobot/nanobot/agent/loop.py`
- Tool ABC → `inspire/nanobot/nanobot/agent/tools/base.py`
- Provider registry → `inspire/nanobot/nanobot/providers/registry.py`
- Config schema → `inspire/nanobot/nanobot/config/schema.py`
- Multi-agent registry (Go) → `inspire/picoclaw/pkg/agent/registry.go`
- Agent instance (Go) → `inspire/picoclaw/pkg/agent/instance.go`

## Quick Commands

```bash
# Install core + all Python extensions
pip install -r requirements.txt

# Run
openagent

# Verify extension registration
python -c "import importlib.metadata as m; print(m.entry_points(group='openagent.extensions'))"

# Run all tests
pytest

# Build all services (cross-compile)
make all

# Build for current host only (faster dev loop)
make local

# Build Rust services (primary)
make local    # Current host
make all      # All targets (Pi, etc.)

# Build WhatsApp (Go) — only Go service
cd services/whatsapp && go build -o bin/whatsapp .
cd services/whatsapp && GOOS=linux GOARCH=arm64 go build -o bin/whatsapp-linux-arm64 .

# Start microsandbox server (required at runtime for sandbox service)
msb server start --dev
```

## Repository Layout

```
openagent/              # Core Python — orchestration, discovery, interfaces
  __init__.py
  interfaces.py             # AsyncExtension protocol + BaseAsyncExtension ABC
  manager.py                # Extension discovery via entry points
  platforms/                 # MCP-lite platform/service adapters (discord, telegram, ...)
  main.py                   # Entry point: asyncio.run(load_extensions())
  agent/                    # Agent loop (ReAct), tool registry
  providers/                # LLM provider registry (Anthropic, OpenAI, OpenAI-compat)
  services/                 # ServiceManager — Go daemon lifecycle
  bus/                      # Message bus (InboundMessage, OutboundMessage)
  session/                  # SessionManager, SessionBackend, SQLite impl
  heartbeat/                # Periodic health/summary polling
  observability/            # Logging, metrics, context
  tests/                    # Core tests (including platforms/)

services/                   # Rust (primary) or Go (WhatsApp only) service daemons
  <name>/                   # Rust: Cargo.toml, src/main.rs; Go: main.go, go.mod
    service.json            # Service manifest — schema-first declaration
    bin/                    # Compiled binaries (gitignored)
  sandbox/                  # Rust — VM-isolated code/shell execution
  discord/                  # Rust — Discord connector
  telegram/                 # Go — Telegram (migration to Rust planned)
  slack/                    # Go — Slack (migration to Rust planned)
  whatsapp/                 # Go — WhatsApp (whatsmeow; only Go service retained)
  stt/                      # Rust — Speech-to-text
  tts/                      # Rust — Text-to-speech
  browser/                  # Rust — Headless browser automation
  memory/                   # Rust — Vector memory

app/                        # Minimalist web UI (FastAPI + HTMX, no auth — POC only)
  main.py                   # FastAPI app, mounts routes and static files
  routes/                   # Route modules (dashboard, chat, services)
  templates/                # Jinja2 HTML templates
  static/                   # CSS and vanilla JS (no build step)
  pyproject.toml            # Package: openagent-app
  tests/                    # Web UI tests

data/                       # Runtime storage (gitignored)
  openagent.db              # SQLite session history, settings, whitelist
  memory/                   # LanceDB vector store
  sockets/                  # Unix domain socket files — <name>.sock
  artifacts/                # Media, downloads, outputs

config/
  openagent.yaml            # Primary config file

inspire/                    # Reference implementations (gitignored)
  openclaw/
  nanobot/
  picoclaw/
```

## Architecture

### Two Planes, One Agent

```
                    ┌─────────────────────────────────────────┐
                    │         Python Control Plane             │
                    │                                          │
  platform ext ─────►  Message Bus ──► AgentLoop ──► LLM API  │
  (WhatsApp/Discord)│                     │                    │
                    │             tool calls│                  │
                    │               ┌──────▼───────┐          │
                    │               │ ServiceManager│          │
                    │               └──────┬───────┘          │
                    └──────────────────────┼──────────────────┘
                                           │ JSON/UDS
                    ┌──────────────────────┼──────────────────┐
                    │    Rust/Go Services Layer │              │
                    │   ┌──────────────────▼──────────────┐   │
                    │   │  services/<name>/ (Rust or Go)  │   │
                    │   │  UDS daemon, MCP-lite protocol  │   │
                    │   └─────────────────────────────────┘   │
                    └─────────────────────────────────────────┘
```

### Python Control Plane (Brain)

Follows Nanobot's agent loop. Core loop:

```
InboundMessage (from platform extension)
  → AgentLoop.process()
    → Execute Middleware Chain (Hooks like STT transcription)
    → Build context (history + memory + system prompt)
    → Call LLM (OpenAI-compatible /v1/chat/completions via aiohttp)
    → If tool call:
        → Python tool? execute directly
        → Service tool? ServiceManager.call(service, tool, params)
        → Append result, loop (max 40 iterations)
    → Final answer → OutboundMessage → platform extension delivers it
```

**Agent registry:** Multiple named agent instances (follow Picoclaw `AgentRegistry`). Each agent has its own model, workspace, session, and tool set. A supervisor agent dispatches to workers.

**Key constraints:**
- Max iterations: 40 (configurable)
- Truncate large tool results to 500 chars (configurable)
- Strip context tags before saving to history
- Core loop is stateless — all state lives in `SessionManager`
- **Zero-Copy Artifact Passing:** When dense data is generated or received by a service (e.g. media), the service writes the raw binary to disk (`data/artifacts/`). Python routes the small JSON artifact path payload, maintaining decoupling without IPC serialization taxes. Python is the absolute central router for all inter-service workflows (no east-west mesh between Go daemons).

### Services Layer — Rust (primary) and Go (WhatsApp only)

Services are **long-lived daemon processes** managed by `ServiceManager`. One socket per service handles both directions. **Rust-first** — all new services are Rust. Only WhatsApp remains in Go.

**ServiceManager responsibilities:**
1. Read `service.json` manifests from `services/*/service.json` on startup
2. Detect platform (`GOOS`/`GOARCH` or Rust target), select correct binary
3. Spawn binary (`asyncio.create_subprocess_exec`)
4. Connect async Unix socket client (`data/sockets/<name>.sock`)
5. Send `{"id":"...","type":"tools.list"}` — register returned tools into agent loop
6. Run health-check loop (ping/pong every 5s); restart on timeout (exponential backoff)
7. Subscribe to event frames — route inbound events to message bus
8. Expose `start(name)`, `stop(name)`, `restart(name)`, `status(name)` API

**Rust service structure (primary):**
```
services/<name>/
  Cargo.toml     # package + dependencies (sdk-rust, tokio, serde_json)
  src/main.rs    # McpLiteServer + tool handlers
  service.json   # manifest
  bin/           # cross-compiled binaries (gitignored)
```

**Rust service internals (main.rs pattern):**
```rust
// 1. Build McpLiteServer from sdk-rust (reads OPENAGENT_SOCKET_PATH)
// 2. Register tool handlers (closures or fns)
// 3. server.run() — owns accept loop, dispatches tools.list / tool.call / ping
// 4. Handlers call MsbClient (sync minreq HTTP) — one sandbox per invocation
//    start → execute/run → stop
// 5. Return tool result string to server; server sends tool.result frame
// 6. SIGTERM handled by tokio runtime shutdown
```

**Rust sandbox: microsandbox (MSB) dependency**

The `sandbox` service requires a running microsandbox server:
```bash
# Install MSB CLI
cargo install msb   # or brew install microsandbox/tap/msb

# Start the server (dev mode — no API key required)
msb server start --dev

# Generate an API key (production)
msb server keygen
```

Set env vars (or config `tools.sandbox` in `openagent.yaml`):
```
MSB_SERVER_URL=http://127.0.0.1:5555   # default
MSB_API_KEY=<key>                      # required unless --dev
MSB_MEMORY_MB=512                      # memory per sandbox VM
```

MSB JSON-RPC 2.0 methods used (POST `/api/v1/rpc`, Bearer auth):
- `sandbox.start` — create named sandbox with OCI image + resource limits
- `sandbox.repl.run` — execute Python/Node code snippet (for `sandbox.execute` tool)
- `sandbox.command.run` — run a shell command (for `sandbox.shell` tool)
- `sandbox.stop` — destroy sandbox after each invocation

### MCP-lite Wire Protocol

One Unix socket per service. Newline-delimited JSON frames in both directions.

**Agent → Service (requests):**
```json
{"id":"<uuid>","type":"tools.list"}
{"id":"<uuid>","type":"tool.call","tool":"<name>","params":{...}}
{"id":"<uuid>","type":"ping"}
```

**Service → Agent (responses — always include same `id`):**
```json
{"id":"<uuid>","type":"tools.list.ok","tools":[{"name":"...","description":"...","params":{...}}]}
{"id":"<uuid>","type":"tool.result","result":"<string>","error":null}
{"id":"<uuid>","type":"pong","status":"ready"}
{"id":"<uuid>","type":"error","code":"SERVICE_ERROR","message":"..."}
```

**Service → Agent (events — no `id`, unprompted push):**
```json
{"type":"event","event":"message.received","data":{...}}
{"type":"event","event":"connection.status","data":{"connected":true}}
```

**Why not full MCP (JSON-RPC 2.0)?** Full MCP adds capability negotiation, resources, prompts, sampling, SSE transport — 80% of which we don't need. MCP-lite borrows MCP's vocabulary (`tools.list`, `tool.call`) so developers familiar with MCP read the protocol instantly, but strips it to only what runs deterministically on a Pi.

**Future path:** If ecosystem compatibility is needed, a thin MCP-to-MCP-lite bridge can be added without changing any service internals.

### Service Manifest (`service.json`)

Schema-first: the manifest is the only contract between Python core and the service. Core must not depend on implementation details of the binary.

```json
{
  "name": "my-service",
  "description": "What this service does for the agent",
  "version": "0.1.0",
  "binary": {
    "linux/arm64":  "bin/my-service-linux-arm64",
    "linux/amd64":  "bin/my-service-linux-amd64",
    "darwin/arm64": "bin/my-service-darwin-arm64"
  },
  "socket": "data/sockets/my-service.sock",
  "health": {
    "interval_ms": 5000,
    "timeout_ms": 1000,
    "restart_backoff_ms": [1000, 2000, 5000, 10000, 30000]
  },
  "tools": [
    {
      "name": "tool_name",
      "description": "What the tool does — write this for the LLM to understand",
      "params": {
        "type": "object",
        "properties": {
          "input": {"type": "string", "description": "..."}
        },
        "required": ["input"]
      }
    }
  ],
  "events": ["message.received", "connection.status"]
}
```

### Python Extensions = platform Integrations Only

Python extensions handle platforms and media. They do **not** do heavy CPU/IO work.

| What | Language | Location | Pattern |
|---|---|---|---|
| platform integrations (WhatsApp, Discord) | Python | `extensions/` | `AsyncExtension` + entry points |
| Service-backed platform connectors | Python | `openagent/platforms/` | Shared `mcplite.py` + per-service adapters |
| Media (TTS, STT) | Python | `extensions/` | Provider pattern, async wrappers |
| Heavy compute / data tools | Rust | `services/` | MCP-lite daemon + `service.json` (Rust-first) |
| VM-isolated code execution | Rust | `services/sandbox/` | MCP-lite daemon + microsandbox HTTP client |
| WhatsApp | Go | `services/whatsapp/` | Only Go service (whatsmeow) |

**WhatsApp:** Implemented as Go service (`services/whatsapp/`) using whatsmeow. No Python extension.

### LLM Provider Layer

Multiple configurable providers per agent. Follow Nanobot's `ProviderSpec` registry. Each agent in the registry can use a different model.

- Fast/cheap model (e.g. Qwen2.5:7B) → routing and simple tasks
- Capable model (e.g. Qwen2.5:14B) → complex reasoning
- All providers: OpenAI-compatible `/v1/chat/completions`
- All HTTP via `httpx` — no sync HTTP, no OpenAI SDK

### Configuration

**YAML config file** (`config/openagent.yaml`) + **env var overrides** (prefix: `OPENAGENT_`). Follow Nanobot's config schema with Pydantic models.

```yaml
providers:
  fast:
    base_url: http://localhost:11434/v1
    model: qwen2.5:7b
    api_key: ollama
  strong:
    base_url: http://localhost:11434/v1
    model: qwen2.5:14b
    api_key: ollama

agents:
  supervisor:
    model: strong
    system_prompt: "..."
    max_iterations: 40
  worker-search:
    model: fast
    system_prompt: "..."

memory:
  sqlite_path: data/sessions.db
  lancedb_path: data/memory/
  memory_window: 50

services:
  discovery: auto          # auto-discover from services/*/service.json
  socket_dir: data/sockets/
```

### Memory & Storage

| Store | Purpose | Path |
|---|---|---|
| SQLite | Session history, settings, whitelist | `data/openagent.db` |
| LanceDB | Semantic memory (vector search) | `data/memory/` |

**LanceDB Note:** Vector search uses a direct Python client wrapper to access LanceDB's fast native Rust core. This avoids JSON IPC serialization overhead on massive vector arrays and keeps the single-node setup lean. We only consider decoupling LanceDB into a Go service if rigorous profiling shows it aggressively blocking the `asyncio` event loop.

| Filesystem | Artifacts, media | `data/artifacts/` |
| Unix sockets | Service IPC files | `data/sockets/*.sock` |

## Web UI (`app/`)

A minimalist admin/debug UI for the agent. POC only — **no authentication**, isolated network assumed (Raspberry Pi on private LAN).

**Stack:**
- FastAPI 3.x (async, same event loop as the agent core)
- Jinja2 templates (no frontend build step)
- HTMX for interactivity (no JS framework)
- Tailwind CSS via CDN
- WebSockets for real-time agent chat
- Server-Sent Events (SSE) for live log streaming

**Pages:**
- `/` Dashboard — agent status, Python packages, Go/Rust services, system stats
- `/chat` — Send messages to the agent, stream responses via WebSocket; sessions sidebar
- `/services` — Go and Rust services with status, restart button
- `/settings` — Connectors, provider, whitelist, WhatsApp QR
- `/settings` — Connectors (enable/disable), provider, whitelist, WhatsApp QR
- `/browser` — Headless browser sessions (screenshots, agent-driven automation)

**Layout:**
```
app/
  main.py           # FastAPI app instance, mounts all routes
  routes/
    dashboard.py    # GET /
    chat.py         # GET /chat, WS /ws/chat, /api/chat/sessions
    services.py     # GET /services, POST /services/{name}/restart
    settings.py     # GET /settings, connectors, whitelist, WhatsApp QR
    llm.py          # Provider/LLM config
    provider.py     # Provider API
    browser.py      # GET /browser, browser session API
  templates/
    base.html       # Layout shell (nav sidebar, content area)
    dashboard.html
    chat.html
    services.html
  settings.html
  browser.html
  static/
    app.css         # Minimal overrides on Tailwind
    htmx.min.js     # Vendored HTMX (no CDN dependency on Pi)
  pyproject.toml    # openagent-app, depends on openagent-core
```

**Rules:**
- Do not add authentication — this is a POC for an isolated Pi
- Do not use React, Vue, or any JS framework with a build step
- Do not add `app/` logic to `openagent/` core — the UI imports from core, not vice versa
- Keep templates simple: one template per page, shared base layout
- The FastAPI app runs alongside the agent (shared process or separate, configurable)

## Coding Standards

### Python
- Python ≥ 3.11, type hints on all public APIs
- `aiohttp` for all HTTP — no `requests`, no OpenAI SDK
- `asyncio.to_thread()` for sync libs in async context
- Pydantic for config and data models
- No global mutable state

### Go Services (WhatsApp only)
- WhatsApp: standalone Go module (`go.mod`) in `services/whatsapp/`
- Socket path received via env var `OPENAGENT_SOCKET_PATH` or CLI flag
- Goroutine per request — never block the accept loop
- Graceful SIGTERM: drain in-flight requests, close socket, exit 0
- Include `service.json` manifest
- Cross-compile targets: `linux/arm64`, `linux/amd64`, `darwin/arm64`
- Compiled binaries go in `bin/` (gitignored)

### Rust Services
- Each service: standalone Rust crate in `services/<name>/` with `Cargo.toml`
- Use `sdk-rust` local crate for MCP-lite server boilerplate
- Socket path read from `OPENAGENT_SOCKET_PATH` env var (same convention as Go)
- Use `tokio` async runtime; blocking I/O (e.g. HTTP) via `tokio::task::spawn_blocking` or sync crate (`minreq`)
- Graceful SIGTERM: tokio runtime shutdown signal
- Include `service.json` manifest — identical schema to Go services
- Cross-compile targets: `aarch64-apple-darwin` (native on Mac), `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl` (via `cross`)
- Compiled binaries go in `bin/` (gitignored)
- External runtime deps (e.g. MSB) must be documented in `service.json` or service README

## Testing Standards

- Core tests: `openagent/tests/` (including `openagent/tests/platforms/`)
- App tests: `app/tests/`
- Extension tests: `extensions/<name>/tests/` only (self-contained per extension)
- Service tests: `services/<name>/` (Go `_test.go` files)
- Mock Go/Rust services in Python tests with a minimal asyncio socket stub that speaks MCP-lite
- No real network calls in tests, no real LLM calls in tests
- `pytest-asyncio` for async Python tests
- Do not keep active test suites under project-root `tests/`; tests belong to their owning vertical.

## Build Order (What Needs Building)

### Built
- Core: extension discovery, lifecycle, async interfaces
- **ServiceManager** — spawn/monitor/restart Go daemons, MCP-lite health loop
- **Message bus** — `InboundMessage`, `OutboundMessage`, `SenderInfo`, per-session fanout
- **Agent loop** — custom ReAct loop (no framework), tool iteration, 40 max iters, 500-char truncation
- **Session manager** — `SessionBackend` protocol, SQLite impl, optional summarisation
- **Tool registry** — dispatches to Go services via MCP-lite
- **Provider layer** — Anthropic, OpenAI, OpenAI-compat (httpx)
- Rust services: sandbox (VM execution), discord, stt, tts, browser, memory
- Go services: whatsapp (only), telegram, slack (migration to Rust planned)
- **Rust services: sandbox** — VM-isolated Python/Node/shell execution via microsandbox (v0.2.0; tools: `sandbox.execute`, `sandbox.shell`)
- Config schema extended: `agents`, `session`, `platforms`, `tools.sandbox` + env overrides
- Cross-platform build: `make all` / `make local` / `make sandbox` / `make browser`

### Next (in order)
1. **Agent registry** — optional multi-agent (follow `inspire/picoclaw/pkg/agent/registry.go`)
2. **platform manager** — config-driven init, outbound dispatch
3. **Optional** — memory consolidation, cron, slash commands, rate limiting
4. **Rust migration** — session store first, then channels when bottleneck proven

See `roadmap.md` for consolidated Nanobot/Picoclaw comparison and detailed gaps.

## Deployment Notes

**Raspberry Pi (primary):**
- Rust services compile to `linux/arm64`; Go (WhatsApp) to `linux/arm64`
- Keep Python deps minimal — no heavy ML libs in core
- Prefer EdgeTTS (no API key) for TTS, `faster-whisper int8 small` for STT
- SQLite + LanceDB — no Postgres, no Redis
- Lazy-load heavy providers and defer service startup until first use

**Ubuntu server / M4 Mac (dev):**
- Same codebase, different binary arch
- Docker optional: `ServiceManager` can spawn Go containers instead of local binaries

## Change Discipline

- Do not break entry-point-based Python extension discovery
- Do not hard-code extension or service names in core
- `service.json` is the only contract — core must not depend on service internals
- Borrow MCP vocabulary in the wire protocol but do not implement full MCP
- When deviating from OpenClaw/Nanobot patterns, document why in comments
- Keep `roadmap.md` updated when build order or status changes

## Observability Contract

- Observability is mandatory for new core, extension, app, and service work.
- Python side:
  - All logs are OTEL-compliant (OpenTelemetry). Traces, logs, and metrics are written to `logs/` as JSONL.
  - Use `openagent/observability/logging.py` for structured logs.
  - Use `openagent/observability/metrics.py` for counters/histograms.
  - Propagate request correlation ids using MCP-lite frame ids.
- App side:
  - Keep `/metrics` endpoint enabled for Prometheus scraping.
- Go/Rust services:
  - Emit one structured request log with request id, tool, outcome, duration.
  - Emit structured error logs for accept/decode/write failures.
- Keep instrumentation lightweight and deterministic for Raspberry Pi targets.
