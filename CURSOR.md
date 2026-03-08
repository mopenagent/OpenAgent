# OpenAgent — Cursor Project Context

## What We're Building

**OpenAgent** is a **deterministic**, **extension-first** hybrid Python + Rust agent platform. It orchestrates multi-agent pipelines using offline 14B-parameter models on low-power hardware (Raspberry Pi primary target).

The architecture has two planes:
- **Python Control Plane (Brain)** — LLM interfacing, multi-agent orchestration, platform integrations, stateless async core loop.
- **Rust Services (Hands)** — Long-lived daemon processes for platform connectors (discord), compute (sandbox, stt, tts), automation (browser), memory. **Rust-first** — all new services are Rust.
- **Go** — Only WhatsApp remains in Go (whatsmeow). Telegram and Slack are still Go but will migrate to Rust.

OpenAgent uses a **custom ReAct loop** and thin provider layer (no framework dependency). Session/memory uses a `SessionBackend` protocol. OpenAgent remains responsible for extension/service orchestration, MCP-lite lifecycle, and deployment constraints for low-power hardware. See `roadmap.md` for rationale.

## Design Principles

1. **Communication Protocol** — Whenever the user sends an input where their intention needs clarification or context needs expansion, do not assume the correct path. Ask clarifying questions **one by one**, provide possible options/paths, and record/apply this explicitly in every conversation.
2. **Deterministic behavior** — Explicit control flow, reproducible execution paths. Aligns with smaller local models where reliability matters more than flexibility.
3. **Two planes, clear boundary** — Python extensions handle platforms (WhatsApp, Discord) and media (TTS, STT). Go services handle compute and data-intensive work. Never mix them.
4. **Service-first for compute** — New heavy capabilities go in `services/<name>/` as Rust daemons (Rust-first). Only WhatsApp stays in Go.
5. **First-class async** — Python core and all extensions are async-first. No blocking I/O in Python extension code.
6. **Tool-oriented** — Capabilities are exposed as tools the LLM can call. Python tools run in-process; Go service tools are declared in `service.json` and proxied through `ServiceManager`.
7. **Offline and low-power friendly** — Designed for a 14B local model on Raspberry Pi. Keep core lean, keep context concise, lazy-load everything heavy. Vector search (LanceDB) executes directly via Python to leverage Rust core without JSON IPC serialization tax.
8. **Workflow Orchestrator (Zero-Copy Artifacts)** — Python acts as a workflow router. Services dump heavy binaries to `data/artifacts/` and return a file path. Python pipes that path as an argument to the next step. LLMs are treated as non-deterministic nodes in a larger deterministic workflow graph. No direct service-to-service (East-West) communication.

## Reference Implementations

| Reference | Language | Role | Path |
|-----------|----------|------|------|
| **OpenClaw** | TypeScript | Functionality — agent logic, orchestration, tool/extension patterns | `inspire/openclaw/` |
| **Nanobot** | Python | Structure — project layout, agent loop, provider registry, config schema | `inspire/nanobot/` |
| **Picoclaw** | Go | Multi-agent registry, service daemon patterns | `inspire/picoclaw/` |

Key files:
- Agent loop: `inspire/nanobot/nanobot/agent/loop.py`
- Tool ABC: `inspire/nanobot/nanobot/agent/tools/base.py`
- Provider registry: `inspire/nanobot/nanobot/providers/registry.py`
- Config schema: `inspire/nanobot/nanobot/config/schema.py`
- Multi-agent registry: `inspire/picoclaw/pkg/agent/registry.go`

## Repository Layout

```
openagent/      # Core Python — orchestration, discovery, interfaces ONLY
  tests/         # Core tests (including platform adapters)
services/       # Rust (primary) + Go (whatsapp only; telegram, slack in transition)
app/            # Minimalist web UI — FastAPI 3.x + HTMX, no auth (POC/Pi only)
  routes/       # dashboard, chat, services, config, settings, llm, provider, browser
  tests/        # Web UI tests
data/           # Runtime: openagent.db, memory/, sockets/, artifacts/
config/         # openagent.yaml
inspire/        # Reference implementations (gitignored)
```

## What Lives Where

| Component | Location | Language | Pattern |
|---|---|---|---|
| Agent loop, tool registry | `openagent/agent/` | Python | Custom ReAct loop (no framework) |
| LLM provider registry | `openagent/providers/` | Python | httpx-based, Anthropic/OpenAI/OpenAI-compat |
| Service lifecycle manager | `openagent/services/` | Python | ServiceManager — spawn, health-check, restart |
| Session manager | `openagent/session/` | Python | SessionBackend protocol, SQLite impl |
| Message bus | `openagent/bus/` | Python | InboundMessage, OutboundMessage, SenderInfo |
| Service platform adapters (MCP-lite clients) | `openagent/platforms/` | Python | Shared `mcplite.py` + per-service adapter |
| Platform connectors | `services/` | Rust + Go | discord (Rust), telegram/slack/whatsapp (Go; only whatsapp retained) |
| Compute/automation | `services/` | Rust | sandbox, stt, tts, browser, memory |

See `roadmap.md` for consolidated Nanobot/Picoclaw comparison and build order.

## Service Contract

Services (Rust or Go) implement the **MCP-lite** wire protocol over a Unix Domain Socket:

```
Socket:  data/sockets/<name>.sock
Frames:  newline-delimited JSON, bidirectional

Agent → Service:
  {"id":"<uuid>","type":"tools.list"}
  {"id":"<uuid>","type":"tool.call","tool":"<name>","params":{...}}
  {"id":"<uuid>","type":"ping"}

Service → Agent:
  {"id":"<uuid>","type":"tools.list.ok","tools":[...]}
  {"id":"<uuid>","type":"tool.result","result":"...","error":null}
  {"id":"<uuid>","type":"pong","status":"ready"}
  {"type":"event","event":"<name>","data":{...}}   ← unprompted push, no id
```

Service manifest (`service.json`) declares: name, binary paths per arch, socket path, health config, tool schemas, event types.

`ServiceManager` in core: reads manifests → spawns binary → connects socket → registers tools → health-checks → restarts on crash.

## Files to Change: Web UI

When editing the web UI, change only files under `app/`:

- `app/main.py` — FastAPI app instance, route registration, lifespan
- `app/routes/<page>.py` — Route handler for each page
- `app/templates/<page>.html` — Jinja2 template for each page
- `app/templates/base.html` — Shared layout (nav, sidebar, content slot)
- `app/static/` — CSS and vendored HTMX JS

Do not add FastAPI routes or UI logic to `openagent/`. The UI is a consumer of core, not part of it.

## Files to Change: Services (Rust or Go)

When editing a service, change only files under `services/<name>/`:

- **Rust:** `Cargo.toml`, `src/main.rs`, `service.json`, `bin/` (gitignored)
- **Go (WhatsApp only):** `main.go`, `service.json`, `go.mod`/`go.sum`, `bin/`, `*_test.go`

Do not change `openagent/` when working on a service.

## Development Conventions

**Python:**
- Python ≥ 3.11
- `pip install -r requirements.txt` for core
- Run: `python -m openagent.main` or `openagent`
- `httpx` for HTTP — never `requests` or OpenAI SDK
- `asyncio.to_thread()` for sync libs in async context

**Rust (primary):**
- Build: `make local` or `make all`
- Run tests: `cd services/<name> && cargo test`

**Go (WhatsApp only):**
- Go ≥ 1.21
- Build: `cd services/whatsapp && go build -o bin/whatsapp .`
- Cross-compile for Pi: `GOOS=linux GOARCH=arm64 go build -o bin/whatsapp-linux-arm64 .`
- Run tests: `cd services/whatsapp && go test ./...`

**Config:** `config/openagent.yaml` — primary config. Env vars override with `OPENAGENT_` prefix.

**Web UI:**
```bash
pip install -e app/
uvicorn app.main:app --host 0.0.0.0 --port 8080 --reload
# visit http://<pi-ip>:8080
```

## When Editing This Project

- **Core** — Keep it minimal. Add orchestration and interfaces; avoid domain logic and heavy dependencies.
- **New compute/data feature** — New Rust service under `services/` with `service.json` (Rust-first).
- **Async only (Python)** — All extension lifecycle and handlers must be `async def`. No blocking.
- **Rust-first for services** — New services are Rust. Only WhatsApp stays in Go.
- **Determinism** — Explicit, reproducible flows. Stable tool schemas. Clear LLM-readable descriptions.
- **14B / Pi target** — Lean context, lazy loading, no heavy deps in core.

## Observability Baseline (Agreed)

- Add shared Python observability helpers under `openagent/observability/`.
- Use structured logs with correlation ids for extension lifecycle, provider calls, and MCP-lite traffic.
- Expose Prometheus metrics at `GET /metrics` from `app/main.py`.
- Track operation latency/error for STT/TTS and MCP-lite request-response paths.
- Keep logs privacy-safe: never log full message bodies or secrets.
- Mirror structured request observability in Rust/Go services for parity.
