# OpenAgent — Cursor Project Context

## What We're Building

**OpenAgent** is a **deterministic**, **extension-first** hybrid Python + Go agent platform. It orchestrates multi-agent pipelines using offline 14B-parameter models on low-power hardware (Raspberry Pi primary target).

The architecture has two planes:
- **Python Control Plane (Brain)** — LLM interfacing, multi-agent orchestration, channel integrations, stateless async core loop.
- **Go Services (Hands)** — Long-lived daemon processes for CPU/IO-intensive work. Python spawns, monitors, and manages them. They communicate via MCP-lite (tagged JSON over Unix sockets).

## Design Principles

1. **Deterministic behavior** — Explicit control flow, reproducible execution paths. Aligns with smaller local models where reliability matters more than flexibility.
2. **Two planes, clear boundary** — Python extensions handle channels (WhatsApp, Discord) and media (TTS, STT). Go services handle compute and data-intensive work. Never mix them.
3. **Service-first for compute** — New heavy capabilities go in `services/<name>/` as Go daemons, not in Python extensions.
4. **First-class async** — Python core and all extensions are async-first. No blocking I/O in Python extension code.
5. **Tool-oriented** — Capabilities are exposed as tools the LLM can call. Python tools run in-process; Go service tools are declared in `service.json` and proxied through `ServiceManager`.
6. **Offline and low-power friendly** — Designed for a 14B local model on Raspberry Pi. Keep core lean, keep context concise, lazy-load everything heavy.

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
src/openagent/      # Core Python — orchestration, discovery, interfaces ONLY
extensions/         # Python channel/media integrations (independently installable)
services/           # Go service daemons (compute/data tools)
tests/              # Python tests
data/               # Runtime: sessions.db, memory/, sockets/, artifacts/
config/             # openagent.yaml
inspire/            # Reference implementations (gitignored)
```

## What Lives Where

| Component | Location | Language | Pattern |
|---|---|---|---|
| Agent loop, orchestration | `src/openagent/agent/` | Python | Nanobot loop.py |
| LLM provider registry | `src/openagent/providers/` | Python | Nanobot ProviderRegistry |
| Service lifecycle manager | `src/openagent/services/` | Python | ServiceManager |
| Message bus | `src/openagent/bus/` | Python | Nanobot bus pattern |
| Channel integrations | `extensions/` | Python | AsyncExtension + entry points |
| Media (TTS, STT) | `extensions/` | Python | Provider pattern |
| Compute/data tools | `services/` | Go | MCP-lite daemon + service.json |

## Extension Contract

Python extensions implement `AsyncExtension` from `openagent.interfaces`:

```python
async def initialize(self) -> None: ...   # startup — no blocking
async def shutdown(self) -> None: ...     # graceful stop
def get_status(self) -> dict[str, Any]: ...
```

Extend `BaseAsyncExtension`. Register via `pyproject.toml` entry point:

```toml
[project.entry-points."openagent.extensions"]
my-ext = "plugin:MyExtension"
```

Extension source layout: flat at `extensions/<name>/src/` — no nested package folders.

## Service Contract

Go services implement the **MCP-lite** wire protocol over a Unix Domain Socket:

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

## Files to Change: Extensions

When editing a Python extension, change only files under `extensions/<name>/`:

- `pyproject.toml` — package metadata, dependencies, entry point
- `src/plugin.py` — extension entry point, implements `AsyncExtension`
- `src/<component>.py` — component logic (connector, bridge, schema, etc.)
- `tests/` — extension tests

Do not change `src/openagent/` or other extensions.

## Files to Change: Go Services

When editing a Go service, change only files under `services/<name>/`:

- `main.go` — UDS server, MCP-lite protocol handler
- `service.json` — service manifest (the only contract with Python core)
- `go.mod` / `go.sum` — Go module definition
- `bin/` — compiled binaries (gitignored)
- `*_test.go` — Go unit tests

Do not change `src/openagent/` or any extension when working on a service.

## Development Conventions

**Python:**
- Python ≥ 3.11
- `pip install -r requirements.txt` for all extensions
- Run: `python -m openagent.main` or `openagent`
- Verify extensions: `python -c "import importlib.metadata as m; print(m.entry_points(group='openagent.extensions'))"`
- `aiohttp` for HTTP — never `requests` or OpenAI SDK
- `asyncio.to_thread()` for sync libs in async context

**Go:**
- Go ≥ 1.21
- Build: `cd services/<name> && go build -o bin/<name> .`
- Cross-compile for Pi: `GOOS=linux GOARCH=arm64 go build -o bin/<name>-linux-arm64 .`
- Run tests: `cd services/<name> && go test ./...`

**Config:** `config/openagent.yaml` — primary config. Env vars override with `OPENAGENT_` prefix.

## When Editing This Project

- **Core** — Keep it minimal. Add orchestration and interfaces; avoid domain logic and heavy dependencies.
- **New channel/media feature** — New Python extension under `extensions/`.
- **New compute/data feature** — New Go service under `services/` with `service.json`.
- **Async only (Python)** — All extension lifecycle and handlers must be `async def`. No blocking.
- **Goroutine per request (Go)** — Never block the accept loop. Graceful SIGTERM handling.
- **Determinism** — Explicit, reproducible flows. Stable tool schemas. Clear LLM-readable descriptions.
- **14B / Pi target** — Lean context, lazy loading, no heavy deps in core.
