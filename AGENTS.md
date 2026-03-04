# AGENTS.md

This file defines how coding agents should work in this repository.

## Mission

Build OpenAgent as a deterministic, extension-first Python + Go hybrid agent platform that orchestrates multi-agent pipelines on offline 14B-class models. Primary deployment target: Raspberry Pi / low-power hardware.

## Source Of Truth

- Full development guide: [`CLAUDE.md`](./CLAUDE.md)
- Project context and intent: [`CURSOR.md`](./CURSOR.md)
- Reference implementations: `inspire/openclaw/` (TypeScript), `inspire/nanobot/` (Python), `inspire/picoclaw/` (Go)

When in doubt: `CLAUDE.md` > `CURSOR.md` > reference implementations.

## Two-Plane Architecture

OpenAgent has two distinct planes. Do not mix their responsibilities.

| Plane | Language | Location | Responsibility |
|---|---|---|---|
| Control Plane (Brain) | Python | `openagent/`, `extensions/` | LLM interfacing, orchestration, channel integrations |
| Data Plane (Hands) | Go (or compiled) | `services/` | Long-lived service daemons for compute/data-intensive work |

The two planes communicate via **MCP-lite**: tagged JSON frames over Unix Domain Sockets (`data/sockets/<name>.sock`).

## Architecture Rules

### 1. Keep core minimal
- Core lives in `openagent/`.
- Core is responsible for: extension discovery, service discovery, lifecycle management, shared interfaces, message bus, and agent loop orchestration.
- Do not add domain-specific logic or heavy third-party dependencies to core.

### 2. Python extensions = channel integrations only
- Extensions under `extensions/<name>/` are for **channels and media** (WhatsApp, Discord, TTS, STT).
- Extensions must be independently installable and register via entry points in `openagent.extensions`.
- Extensions must be first-class async and event-driven.
- Do not put CPU/IO-heavy compute in Python extensions — that goes in Go services.

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
- Compiled binaries in `services/<name>/bin/` (gitignored)

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
extensions/         # Python channel integrations (independently installable)
services/           # Go service daemons (each with go.mod + service.json)
app/                # Minimalist web UI (FastAPI + HTMX, no auth — POC only)
tests/              # Python tests (mirrors openagent/ and extensions/ structure)
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

- **extension** — Python channel/media integration (`extensions/`)
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

- Core tests: `tests/openagent/`
- Extension tests: `extensions/<name>/tests/` and `tests/extensions/<name>/`
- Go service tests: `services/<name>/` (Go `_test.go` files)
- Mock Go services in Python tests with a minimal asyncio socket stub that speaks MCP-lite
- No real network calls in tests, no real LLM calls in tests
- `pytest-asyncio` for async Python tests
- Every new core behaviour: tests covering discovery/loading, initialization, key execution paths

## Change Discipline

- Do not break entry-point based extension discovery
- Do not hard-code extension names or service names in core
- `service.json` is the only contract — core must not know service internals
- Prefer backward-compatible interface evolution
- If deviating from OpenClaw/Nanobot patterns, document why in comments or PR notes

## Agent Workflow

1. Read `CLAUDE.md` first, then `CURSOR.md` before substantial changes.
2. Determine: is this a channel/media concern (Python extension) or a compute concern (Go service)?
3. Keep core changes minimal; push feature logic to extensions or services.
4. Update/add tests in the appropriate `tests/` tree (Python) or `services/<name>/` (Go).
5. Keep docs in sync: `README.md`, `CLAUDE.md`, `CURSOR.md`, extension/service metadata.
