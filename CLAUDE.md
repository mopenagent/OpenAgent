# CLAUDE.md — OpenAgent Development Guide

## What We're Building

**OpenAgent** is a deterministic, extension-first agent platform on a **progressive Rust migration path**:

- **Python Control Plane (temporary shell)** — LLM interfacing, multi-agent orchestration, state management. Stateless asyncio core loop. Python is a shrinking babysitter — it owns less with each phase.
- **Rust Services (the Hands, permanent)** — Long-lived `tokio` daemon services for all CPU/IO-intensive work: platform connectors (discord, telegram, slack), compute (sandbox, stt, tts), automation (browser), memory. **Rust-first** — all new services are Rust.
- **Cortex (the growing Brain)** — Rust service that owns the full ReAct loop, tool routing, memory, and action search. `openagent` (Rust binary) wraps it with Tower middleware and exposes Axum on :8080 for external callers.
- **Go** — Only WhatsApp (`services/whatsapp/`) remains in Go (whatsmeow). No new Go services.

All services communicate via **MCP-lite wire protocol** (tagged JSON frames over TCP). Services run as persistent daemons started by `services.sh` (dev) or systemd (production); `ServiceManager` in `openagent` connects to them, registers their tools, and health-monitors them.

Primary deployment target: **Raspberry Pi / low-power hardware** (Rust compiles to arm64; Python core stays lean while it lasts).

### Migration Trajectory

This is an evolution, not a rewrite. Each phase is independently shippable. Python shrinks; Cortex grows.

| Phase | Control plane | Python role | Tower/Axum role |
|---|---|---|---|
| **Phase 1** ✅ | Python `AgentLoop` calls `cortex.step` via MCP-lite. Cortex does one LLM turn. | Full control plane | None |
| **Phase 2** ✅ | Rust `openagent` binary is the control plane. Cortex owns full ReAct loop + tool routing + memory. Tower middleware (Guard, STT, TTS) and dispatch loop live in `openagent`. Python middleware deleted. | Web UI only (optional Docker container) | Full Tower stack in `openagent` (GuardLayer → SttLayer → TtsLayer) |
| **Phase 3 (now)** ✅ | `openagent` Axum serves control plane API on :8080. Platform connectors (channels service) connect directly. Python web UI is a separate container. | Retired as control plane; web UI only | Axum in `openagent` is the control plane |

**Permanent protocol decision:** MCP-lite JSON over TCP is the permanent internal protocol between `openagent` and all services. Axum is external-facing only — it speaks JSON to clients on :8080. Services never change their protocol because the control plane is being replaced above them.

## Instruction Scope

Project-specific Claude guidance belongs in this file at the repository root. Treat `CLAUDE.md` as the canonical source for OpenAgent architecture, workflow, coding standards, and decision history.

Use `.claude/settings.local.json` only for machine-local permission overrides or temporary tooling allowances needed during development on this checkout. Do not treat it as durable project guidance.

Keep `~/.claude/settings.json` generic or empty. Do not place OpenAgent-specific architecture rules, coding conventions, or workflow policy in home-level Claude settings; those belong in this repository so they version with the codebase.

## Communication Protocol (Rule #1)
Whenever the user sends an input where their intention needs clarification or the context needs expansion, **do not assume the correct path.** Ask clarifying questions **one by one** (1-by-1) and provide possible **options/paths** for the user to choose from. Apply this explicitly in every conversation.

## Agentic Layer

OpenAgent uses a **custom ReAct loop** and thin httpx-based provider layer — no framework dependency. This gives full control over tool schema format, retry logic, and iteration limits for sub-30B models. Session/memory uses a `SessionBackend` protocol (SQLite now; Go/Rust service later). See `roadmap.md` for rationale and build order.

**LLM deployment note:** The primary LLM is an **external model with a 36K token context window** (served via OpenAI-compatible API). Context overhead per prompt is minimal: 3 fixed Capability schemas + top-k skill summaries (one line each). Tools are not injected — the LLM discovers them via `cortex.discover` on demand. Token pressure is not a constraint at current scale — do not add token-reduction complexity unless profiling proves otherwise.

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

# Build WhatsApp (Go) — only Go service (outputs to project-root bin/)
make whatsapp
# or manually:
cd services/whatsapp && GOOS=darwin GOARCH=arm64 go build -ldflags="-s -w" -o ../../bin/whatsapp-darwin-arm64 .

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
  services/                 # ServiceManager — Rust/Go daemon lifecycle
  bus/                      # Message bus (InboundMessage, OutboundMessage)
  session/                  # SessionManager, SessionBackend, SQLite impl
  heartbeat/                # Periodic health/summary polling
  observability/            # Logging, metrics, context
  tests/                    # Core tests (including platforms/)

services/                   # Rust (primary) or Go (WhatsApp only) service daemons
  <name>/                   # Rust: Cargo.toml, src/main.rs; Go: main.go, go.mod
    service.json            # Service manifest — schema-first declaration
  sandbox/                  # Rust — VM-isolated code/shell execution
  whatsapp/                 # Go — WhatsApp (whatsmeow; only Go service retained)
  stt/                      # Rust — Speech-to-text
  tts/                      # Rust — Text-to-speech
  browser/                  # Rust — Headless browser/web service
  memory/                   # Rust — Vector memory
  validator/                # Rust — Tool output validator
  sdk-rust/                 # Rust — Shared MCP-lite server library (used by all Rust services)
  sdk-go/                   # Go — Shared MCP-lite library (used by WhatsApp)

bin/                        # Cross-compiled service binaries (gitignored)
  <name>-<os>-<arch>        # e.g. browser-darwin-arm64, whatsapp-linux-arm64

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
  artifacts/                # Media, downloads, outputs

config/
  openagent.toml            # Primary config file (provider, agents, channels, middleware)

skills/                     # Domain knowledge for the agent (human-authored, agent-enriched)
  <name>/
    SKILL.md              # Entry point — frontmatter (name, description, hint, allowed-tools, enforce) + body
    references/           # Deep-dive docs — loaded on demand via skill.read(reference=...)
    templates/            # Ready-to-run scripts
    drafts/               # Agent-generated learning candidates — pending human review (gitignored)

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
                                           │ JSON/TCP
                    ┌──────────────────────┼──────────────────┐
                    │    Rust/Go Services Layer │              │
                    │   ┌──────────────────▼──────────────┐   │
                    │   │  services/<name>/ (Rust or Go)  │   │
                    │   │  TCP daemon, MCP-lite protocol  │   │
                    │   └─────────────────────────────────┘   │
                    └─────────────────────────────────────────┘
```

### Python Control Plane (Temporary Shell — shrinking)

Follows Nanobot's agent loop. Current loop (Phase 1):

```
InboundMessage (from platform extension)
  → AgentLoop.process()
    → Execute Middleware Chain (STT, whitelist — Python-side, temporary)
    → Build context (history + memory + system prompt)
    → cortex.step via MCP-lite  ← Cortex does LLM reasoning
    → If tool call (Phase 2+): Cortex routes tools directly
    → Final answer → OutboundMessage → platform extension delivers it
```

**Middleware migration:** ✅ Complete. Python middleware (STT, whitelist/guard, TTS) has been replaced by Tower layers (`SttLayer`, `GuardLayer`, `TtsLayer`) in the `openagent` Rust binary. Do not add new Python middleware — add Tower layers in `openagent/src/` instead.

**Agent registry:** Multiple named agent instances (follow Picoclaw `AgentRegistry`). Each agent has its own model, workspace, session, and tool set. A supervisor agent dispatches to workers. In Phase 3+, this registry moves into Cortex.

**Key constraints (Python side, until migrated):**
- Max iterations: 40 (configurable)
- Truncate large tool results to 500 chars (configurable)
- Strip context tags before saving to history
- Core loop is stateless — all state lives in `SessionManager`
- **Zero-Copy Artifact Passing:** Services write raw binary to disk (`data/artifacts/`). Python routes the small JSON artifact path payload. In Phase 4, Cortex/Axum takes over this routing role.
- Python is the central router for inter-service workflows until Phase 4 — no east-west mesh between Rust/Go daemons at any phase.

### Services Layer — Rust (primary) and Go (WhatsApp only)

Services are **long-lived daemon processes** started by `services.sh` (dev) or systemd (production). `ServiceManager` in `openagent` connects to them over TCP — it does **not** spawn or restart them. **Rust-first** — all new services are Rust. Only WhatsApp remains in Go.

**ServiceManager responsibilities:**
1. Read `service.json` manifests from `services/*/service.json` on startup
2. For each enabled service, spawn a `connection_loop` task that connects over TCP to `address` from the manifest
3. Send `{"id":"...","type":"tools.list"}` — register returned tools into agent loop
4. Run health-check loop (ping/pong every 5s); reconnect automatically if the service restarts
5. Subscribe to event frames — route inbound events to the dispatch loop

**Starting services (dev):**
```bash
./services.sh start        # start all services
./services.sh start browser  # start one
./services.sh status       # check running state
```

**Rust service structure (primary):**
```
services/<name>/
  Cargo.toml     # package + dependencies (sdk-rust, tokio, serde_json)
  src/main.rs    # McpLiteServer + tool handlers
  service.json   # manifest
```

**Binaries live in the project-root `bin/` directory (not inside the service dir):**
```
bin/
  browser-darwin-arm64
  browser-linux-arm64
  ...
```

**Rust service internals (main.rs pattern):**
```rust
// 1. Load .env (dotenvy, best-effort)
// 2. Init OTEL: sdk_rust::setup_otel("service-name", logs_dir)
// 3. Build McpLiteServer from sdk-rust
// 4. Register tool handlers (closures or fns)
// 5. server.serve_auto("0.0.0.0:<port>").await
//    — reads OPENAGENT_TCP_ADDRESS env var; falls back to the hardcoded default
// 6. SIGTERM handled automatically by tokio runtime shutdown
```

**Transport:** `serve_auto(default_addr)` in sdk-rust reads `OPENAGENT_TCP_ADDRESS` from env. `services.sh` sets this before spawning each binary.

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

One TCP connection per service (loopback). Newline-delimited JSON frames in both directions.

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

**Why TCP instead of UDS?** TCP works transparently across localhost, LAN, and Docker networks without socket-file lifecycle management. `TCP_NODELAY` is set on every connection to eliminate Nagle latency (~40 ms on loopback). All services bind on `0.0.0.0:<port>` so the port can be forwarded or firewall-restricted without changing service code.

### Service Manifest (`service.json`)

Schema-first: the manifest is the only contract between Python core and the service. Core must not depend on implementation details of the binary.

```json
{
  "name": "my-service",
  "description": "What this service does for the agent",
  "version": "0.1.0",
  "address": "0.0.0.0:9006",
  "binary": {
    "linux/arm64":  "bin/my-service-linux-arm64",
    "linux/amd64":  "bin/my-service-linux-amd64",
    "darwin/arm64": "bin/my-service-darwin-arm64"
  },
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

**Port allocation:**
| Service  | Port |
|----------|------|
| memory   | 9000 |
| browser  | 9001 |
| sandbox  | 9002 |
| stt      | 9003 |
| tts      | 9004 |
| validator| 9005 |
| whatsapp | 9010 |

Assign the next available port to new services. Never reuse a port.

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

- **Primary:** External model with **36K context window** via OpenAI-compatible `/v1/chat/completions`
- Fast/cheap model (e.g. Qwen2.5:7B) → routing and simple tasks
- Capable model (e.g. Qwen2.5:14B) → complex reasoning
- All HTTP via `httpx` — no sync HTTP, no OpenAI SDK

### Configuration

**TOML config file** (`config/openagent.toml`) + **env var overrides** (prefix: `OPENAGENT_`). All `${VAR}` tokens in the TOML are resolved from the environment at load time.

```toml
[provider]
kind     = "openai_compat"
base_url = "http://localhost:1234/v1"
api_key  = ""
model    = "qwen/qwen3.5-9b"
timeout  = 300.0

[[agents]]
name           = "AgentM"
system_prompt  = "..."
max_iterations = 40

[session]
backend = "sqlite"
db_path = "data/openagent.db"

[channels.telegram]
enabled = false
token   = "${TELEGRAM_BOT_TOKEN}"

[channels.discord]
enabled = true
token   = "${DISCORD_BOT_TOKEN}"

[channels.slack]
enabled   = true
bot_token = "${SLACK_BOT_TOKEN}"
app_token = "${SLACK_APP_TOKEN}"

[guard]
enabled = true
db_path = "data/openagent.db"

[middleware.stt]
enabled = false

[middleware.tts]
enabled = false
voice   = "af_sarah"

[services]
disabled = []   # service names to skip even if binary exists
```

### Memory & Storage

| Store | Purpose | Path |
|---|---|---|
| SQLite | Session history, settings, whitelist | `data/openagent.db` |
| LanceDB | Semantic memory (vector search) | `data/memory/` |

**LanceDB Note:** Vector search uses a direct Python client wrapper to access LanceDB's fast native Rust core. This avoids JSON IPC serialization overhead on massive vector arrays and keeps the single-node setup lean. We only consider decoupling LanceDB into a Go service if rigorous profiling shows it aggressively blocking the `asyncio` event loop.

| Filesystem | Artifacts, media | `data/artifacts/` |
| Unix sockets | Service IPC files | `data/sockets/*.sock` |

## Skills (`skills/`)

### Three-Tier Model: Capabilities → Skills → Tools

The action context has three distinct tiers with different injection rules:

| Tier | What | Always in context? | How LLM gets more |
|---|---|---|---|
| **Capability** | Fixed meta-tools for discovery and recall | ✅ Always, every turn | N/A — always present |
| **Skill** | Domain knowledge, patterns, guidance | Summary only (top-k match) | `skill.read(name=...)` |
| **Tool** | Service integrations (browser, sandbox, …) | ❌ Never injected | `cortex.discover` → read schema → call |

**The five Capabilities (always injected every turn — full schema, no discovery needed):**
- `memory.search` — recall from long-term memory (LTM)
- `web.search` — search the web via SearXNG (step 1 of 2-turn web workflow)
- `web.fetch` — fetch a URL and return clean Markdown (step 2 of 2-turn web workflow)
- `cortex.discover` — search the action catalog for tools and skill summaries
- `skill.read` — load a skill's full body or deep-dive reference on demand

`memory.search`, `web.search`, and `web.fetch` are sourced from the ActionCatalog (service.json). `cortex.discover` and `skill.read` are internal Cortex tools injected via hardcoded builders.

**Skills** appear as one-line summaries in the top-k action search results. The LLM reads the summary and calls `skill.read` to get the full body. Skills never auto-inject their full content.

**Tools** are never injected. The LLM calls `cortex.discover` to find a tool, reads its schema in the result, then calls it. This keeps the initial context small and forces intentional tool selection.

### What a Skill Is

**Tools execute. Skills carry knowledge.**

A tool is an integration with an external system (browser, sandbox, memory). Its callables (`browser.open`, `sandbox.shell`) are its API surface, defined in `service.json`.

A skill is domain knowledge — it teaches the LLM **what** to do and **how** to think when using one or more tools together. The LLM uses skills to know the patterns, gotchas, and sequences; it uses tools to actually execute them.

Skills are not planned one-per-tool. They emerge from real repeatable workflows that span one or more tools. A skill is born when a pattern recurs enough to be worth capturing.

### Skill File Structure

```
skills/
  <skill-name>/
    SKILL.md              ← entry point (required)
    references/           ← deep-dive docs (optional)
      authentication.md
      commands.md
    templates/            ← ready-to-run scripts (optional)
      form-automation.sh
```

### SKILL.md Format

```markdown
---
name: agent-browser
description: Browser automation CLI for AI agents.
hint: Call skill.read(name="agent-browser") for commands, patterns, and auth workflows.
allowed-tools: browser.open, browser.navigate, browser.snapshot
enforce: false
---

# Full skill body here...
## Essential Commands
...
## Authentication
...
```

**Frontmatter fields:**

| Field | Required | Purpose |
|---|---|---|
| `name` | yes | Unique skill identifier — used by `skill.read` |
| `description` | yes | One-line summary injected in semantic search |
| `hint` | yes | Call-to-action appended to description in search context — tells LLM exactly how to get more |
| `allowed-tools` | no | Tools this skill uses. Enforcement depends on `enforce` flag |
| `enforce` | no | `true` = Cortex rejects tool calls outside `allowed-tools`. `false` (default) = soft guidance only |

### Progressive Disclosure

Skills use three-level progressive disclosure. Full bodies are never auto-injected.

**Level 1 — Semantic search (automatic, summary only)**
Every `cortex.step`, Cortex scores the user input against all catalog entries. If a skill matches the top-k, the LLM sees one line:
```
skill: agent-browser
description: Browser automation CLI for AI agents.
```
The body of SKILL.md is never injected at this level.

**Level 2 — Full skill on-demand**
LLM calls `skill.read(name="agent-browser")` → receives the full SKILL.md body + a table of contents of available references:
```
## Available References
- authentication — Login flows, OAuth, 2FA
- commands — Full command reference
- session-management — Parallel sessions, state persistence
```

**Level 3 — Reference on-demand**
LLM calls `skill.read(name="agent-browser", reference="authentication")` → receives that reference file's content.

`skill.read` is a **Capability** — always present, no discovery needed.

### Knowledge Lifecycle

Skills grow through agent experience, audited by a human before assimilation:

```
1. Human writes seed SKILL.md
       ↓
2. Agent runs tasks → diary entries written per session
       ↓
3. Phase 8 Reflection scans diary → extracts skill-relevant learnings → draft files
       (drafts live in skills/<name>/drafts/ — never auto-promoted)
       ↓
4. Human reviews drafts in editor → edits, approves, or discards
       ↓
5. Approved content merged into live SKILL.md manually
```

Agent-generated learnings **never automatically modify a live skill**. They sit in `drafts/` until a human promotes them. This is the knowledge assimilation output of Phase 8 Reflection.

### Management

Skills are managed as files. No web UI. Your editor is the interface.

```
skills/<name>/SKILL.md        ← live skill (human-maintained)
skills/<name>/drafts/         ← agent-generated candidates (pending human review)
skills/<name>/references/     ← deep-dive reference docs
skills/<name>/templates/      ← ready-to-run scripts
```

### Authoring Rules

- Every skill **must** have `name`, `description`, and `hint` in frontmatter
- `hint` must name the exact tool call: `Call skill.read(name="<name>") for ...`
- Keep `description` to one sentence — it appears in the 8-tool context block
- `enforce: true` only for critical, non-negotiable workflows — use sparingly
- Skills are born from real usage — do not pre-create skills for tools that haven't been used in multi-step patterns yet

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

### Rust Patterns (apply to all Rust code in this repo)

**1. Typestate Pattern — encode validity into the type system**

Never allow invalid state transitions at runtime. Encode states as distinct types so the compiler rejects illegal operations. Example: a hardware actuator should not accept `move()` when in `Error` or `Charging` state — make those states separate types that don't implement the `move` method.

```rust
struct Ready;
struct Charging;

struct Actuator<S> { _state: std::marker::PhantomData<S> }

impl Actuator<Ready> {
    pub fn move_to(&self, x: f32, y: f32, z: f32) -> Result<(), ActuatorError> { ... }
}
// Actuator<Charging>::move_to does not exist — invalid call won't compile.
```

Use this wherever a resource has a lifecycle (connections, hardware sessions, agent states).

**2. Trait-Based Tooling — extensibility without hardcoding**

Tools and actuators must be defined as traits so they can be swapped (real hardware, mock for tests, alternative implementations) without changing call sites.

```rust
pub trait RobotActuator {
    fn move_to(&self, x: f32, y: f32, z: f32) -> Result<(), RobotError>;
}
// Implement for Unitree G1, a robotic arm, or a test mock — same call site.
```

Apply this to all service integrations, hardware drivers, and any pluggable component.

**3. Actor Model — concurrency via message passing**

For concurrent workloads (sensor reads, motor commands, LLM streaming), each independent concern is an actor that communicates via bounded `tokio` channels. No shared mutable state between actors — only messages. This prevents data races when e.g. a camera feed is being read while motor commands are being sent.

Use `tokio::sync::mpsc` with an explicit capacity. Never use `unbounded_channel` — an unbounded queue between a fast producer and a slow consumer will silently leak memory until the process hangs.

### Rust Anti-Patterns (never do these)

| Anti-pattern | Why it's banned | What to do instead |
|---|---|---|
| `unwrap()` / `expect()` in non-test code | A panic in a service loop is a hard crash — no recovery, no error propagation | Return `Result<_, E>` with `thiserror`-derived error types |
| `static mut` for shared state | Undefined behaviour under concurrent access | `Arc<Mutex<T>>` or `Arc<RwLock<T>>` for shared state; `Atomic*` types for counters/flags |
| Blocking inside `async fn` | Starves the tokio executor — all other tasks on that thread freeze | `tokio::task::spawn_blocking` for CPU-heavy or blocking I/O work |
| `unbounded_channel` | Fast producer + slow consumer = unbounded memory growth | Always set a capacity: `mpsc::channel(N)` or `broadcast::channel(N)` |
| `clone()` on large buffers in hot paths | Copies data on every iteration | Pass `Arc<T>` or `&T`; only clone at ownership boundaries |



Cortex is the only service that uses [AutoAgents](https://github.com/liquidos-ai/AutoAgents) as its agent execution framework, `tower` for middleware, and eventually `axum` for the control plane. Other services remain plain `tokio` + `sdk-rust` daemons — do not add any of these to non-Cortex services.

**AutoAgents integration (selective — see `services/cortex/README.md` for full detail):**

| Adopted | Excluded |
|---|---|
| `autoagents-llm` — unified `LLMProvider` trait | `autoagents-core::memory` — own `MemoryProvider` impl |
| `autoagents-core` — `BaseAgent`, `ToolT`, `ActorAgent` | `autoagents-protocol` — own MCP-lite protocol |
| `autoagents-derive` — tool input/output proc macros | `autoagents-telemetry` — own OTEL via `sdk-rust` |

Key patterns:
- **`CortexAgent`**: single generic struct implementing `AgentDeriveT` manually; fully driven by `config/openagent.yaml`. No `#[agent]` macro — adding an agent is a YAML edit, not a Rust recompile.
- **`HybridMemoryAdapter`**: implements AutoAgents' `MemoryProvider` trait with sliding-window STM (40 messages, permanent) + LTM via `memory.search` over UDS.
- **Tool dispatch**: `ToolRouter` prefix-routes to owning service sockets; `cortex.*` self-routes back to `cortex.sock` (worker dispatch). Three Capabilities are always pinned: `memory.search`, `cortex.discover`, `skill.read`. All other tools are discovered via `cortex.discover` — never pre-injected.
- **Research context injection**: each generation turn calls `research.status` proactively, formats runnable tasks into the system prompt (`## Active Research` block) so the supervisor selects the next task without a round-trip tool call.
- **Worker dispatch**: supervisor calls `cortex.step` with `agent_name` to spawn a worker; same handler, same process, fresh `CortexAgent` with worker config. Workers are stateless — full context in the request. Actor dispatch (`ractor`) deferred to Phase 9+.

**Tower conventions (`openagent` binary):**
- Tower/Axum lives in `openagent/` (the control plane binary), NOT in Cortex or any other service
- Layer order (outermost → innermost): `ConcurrencyLimitLayer` → `HandleErrorLayer` → `TimeoutLayer` → `TraceLayer` → `CorsLayer` → `GuardLayer` → `SttLayer` → `TtsLayer` → Router
- Use `tower::ServiceBuilder` to compose timeout + error handling; use `axum::middleware::from_fn_with_state` for stateful layers
- Timeout and retry (`tower::timeout::TimeoutLayer`, `tower::retry::RetryLayer`) replace Python's manual retry logic

**Axum scope (permanent):**
- Axum in `openagent` is external-facing only — it speaks JSON on :8080 to platform connectors and the web UI
- Axum does NOT replace MCP-lite between `openagent` and services — that protocol is JSON over UDS, permanent
- Do NOT add `axum` or `tower` to any service other than `openagent`

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
- **ServiceManager** — spawn/monitor/restart Rust/Go daemons, MCP-lite health loop
- **Message bus** — `InboundMessage`, `OutboundMessage`, `SenderInfo`, per-session fanout
- **Agent loop** — custom ReAct loop (no framework), tool iteration, 40 max iters, 500-char truncation
- **Session manager** — `SessionBackend` protocol, SQLite impl, optional summarisation
- **Tool registry** — dispatches to Rust/Go services via MCP-lite
- **Provider layer** — Anthropic, OpenAI, OpenAI-compat (httpx)
- Rust services: sandbox (VM execution), discord, stt, tts, browser, memory
- Go services: whatsapp (only). Rust services: discord, telegram, slack, sandbox, stt, tts, browser, memory
- **Rust services: sandbox** — VM-isolated Python/Node/shell execution via microsandbox (v0.2.0; tools: `sandbox.execute`, `sandbox.shell`)
- **Rust service: research** — cross-session Research DAG (SQLite + markdown snapshots); tools: `research.start/list/switch/status/complete`, `research.task_add/done/fail`; `assigned_agent` field for multi-agent dispatch; web UI at `/research`
- Config schema extended: `agents`, `session`, `platforms`, `tools.sandbox` + env overrides
- Cross-platform build: `make all` / `make local` / `make sandbox` / `make browser` / `make research`

### Next (in order)

**Cortex evolution (see `services/cortex/TODO.md` for full phase breakdown):**
1. ~~**Cortex Phase 1B**~~ ✅ — AutoAgents core integration, `CortexAgent`, `HybridMemoryAdapter`, static tools, `autoagents-llm`
2. ~~**Cortex Phase 2**~~ ✅ — tool routing baseline; Cortex calls memory/browser/sandbox directly over MCP-lite
3. ~~**Cortex Phase 3**~~ ✅ — memory retrieval + episode writes; STM eviction, diary writes
4. ~~**Cortex Phase 4**~~ ✅ — prompt system: MiniJinja embedded templates
5. ~~**Tower middleware (full)**~~ ✅ — `GuardLayer`, `SttLayer`, `TtsLayer` in `openagent`; Python middleware deleted; dispatch loop added
6. ~~**Cortex Phase 5**~~ ✅ — action search: `ActionCatalog` keyword-ranked top-k per step; five Capabilities always pinned (`memory.search`, `web.search`, `web.fetch`, `cortex.discover`, `skill.read`); skills injected as summary-only; other tools not injected (LLM discovers via `cortex.discover`)
7. ~~**Provider fallback chain**~~ ✅ — `dispatch_with_fallback()` in `llm.rs`; `fallbacks: Vec<FallbackProvider>` in config
8. ~~**Rate limiting middleware**~~ ✅ — `ConcurrencyLimitLayer` (max 50) as outermost Tower layer in `openagent`
9. ~~**Web UI diary + chat refactor**~~ ✅ — `/diary` read-only past session browser; `/chat` simplified to live web session only
10. ~~**Cortex Phase 7: Segmented STM**~~ ❌ CANCELLED — sliding window (40 messages) is permanent
11. ~~**Cortex Phase 6: Research DAG + Supervisor task selection + Worker dispatch**~~ ✅ — `services/research/` Rust service (SQLite + markdown snapshots, 8 tools); research context injected into system prompt each generation turn (`fetch_research_context` via ToolRouter); `cortex.step` self-call worker dispatch (ToolRouter routes `cortex.*` → `cortex.sock`); `cortex.step` always pinned alongside `memory.search` and `research.status`; `user_key` param for cross-channel research ownership
12. **Skills — `skill.read` in Cortex** — `hint` + `enforce` fields in `SkillFrontmatter`; `handle_skill_read()` in `handlers.rs`; registered in `service.json`; progressive disclosure: summary in semantic search, full body + references TOC on `skill.read(name=...)`, reference content on `skill.read(name=..., reference=...)`; existing SKILL.md files updated with `hint` + `enforce`
13. **Cortex Phase 8: Reflection** — background synthesis, hypothesis generation, contradiction detection after research tasks complete; **also triggers skill knowledge assimilation** — scans diary entries for skill-relevant learnings, writes draft files to `skills/<name>/drafts/` for human review
14. **Cortex Phase 9: Curiosity queue** — research leads surfaced as non-intrusive suggestions

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
