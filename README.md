# OpenAgent

A deterministic Python + Go hybrid agent platform for low-power/offline deployments (including Raspberry Pi).  
Python is the control plane (extensions, orchestration, routing). Go services are the data plane (long-lived tools and integrations) over MCP-lite on Unix sockets.

## Current Status

**Implemented:**
- **ServiceManager** — spawns, health-checks, restarts Go daemons (`openagent/services/manager.py`)
- **Message bus** — `InboundMessage`, `OutboundMessage`, `SenderInfo`, per-session fanout (`openagent/bus/`)
- **Agent loop** — custom ReAct loop, no framework dependency (`openagent/agent/loop.py`)
- **Session manager** — `SessionBackend` protocol, SQLite impl, optional summarisation (`openagent/session/`)
- **Tool registry** — dispatches to Go services via MCP-lite (`openagent/agent/tools.py`)
- **Provider layer** — Anthropic, OpenAI, OpenAI-compat (httpx-based, no SDK)
- **MCP-lite** — Python client + Go SDK (`openagent/platforms/mcplite.py`, `services/sdk-go/mcplite/`)
- **Heartbeat** — periodic health/summary polling (`openagent/heartbeat/`)
- **platform adapters** — Discord, Telegram, WhatsApp, Slack (Python MCP-lite clients)
- **Go services** — `hello`, `filesystem`, `shell`, `discord`, `telegram`, `slack`, `whatsapp`
- **Web UI** — FastAPI + HTMX (dashboard, chat, logs, extensions, services, config)

**In progress:**
- Full chat path wiring (agent loop ↔ platform events ↔ web UI)
- Config schema extension (agents, bindings, session, tools)

## Architecture

```
Python Control Plane (Brain)          Go Services (Hands)
─────────────────────────────         ───────────────────
 platform/media extensions              Long-lived daemons
 Provider calls + orchestration ──JSON──► Compute/data integrations
 Message bus + health/heartbeat ◄─UDS──  Managed by ServiceManager
```

Two clear planes, one socket each, no REST overhead:
- **Python extensions** for platform/media integration
- **Go services** for compute/data-heavy and long-lived connectors
- **MCP-lite** newline-delimited JSON frames over Unix Domain Sockets

## Requirements

- **Python 3.11+**
- **Go 1.21+** (for building Go services)
- A local LLM via an OpenAI-compatible endpoint (e.g. [Ollama](https://ollama.com))

## Installation

```bash
git clone https://github.com/kmaneesh/OpenAgent.git
cd OpenAgent

# Install core and all Python extensions
pip install -r requirements.txt

# Or selectively
pip install -e .
pip install -e extensions/whatsapp
pip install -e extensions/discord
pip install -e extensions/tts
pip install -e extensions/stt
```

## Quick Start

```bash
# Copy and edit the config
cp config/openagent.yaml.example config/openagent.yaml

# Run
openagent
# or
python -m openagent.main

# Verify extensions are registered
python -c "import importlib.metadata as m; print(m.entry_points(group='openagent.extensions'))"
```

## Configuration

OpenAgent is configured via `config/openagent.yaml`. Environment variables with the `OPENAGENT_` prefix override file values.

```yaml
providers:
  fast:
    base_url: http://localhost:11434/v1   # Ollama default
    model: qwen2.5:7b
    api_key: ollama
  strong:
    base_url: http://localhost:11434/v1
    model: qwen2.5:14b
    api_key: ollama

agents:
  supervisor:
    model: strong
    max_iterations: 40

memory:
  sqlite_path: data/sessions.db
  lancedb_path: data/memory/
```

Provider kinds currently supported in core:
- `openai_compat`
- `openai`
- `anthropic`

## Project Structure

```
OpenAgent/
├── openagent/          # Core runtime
│   ├── interfaces.py       # AsyncExtension protocol
│   ├── manager.py          # Extension discovery (entry points)
│   ├── providers/          # LLM provider registry
│   ├── services/           # ServiceManager — Go daemon lifecycle
│   ├── bus/                # Message bus (platform → agent → response)
│   ├── heartbeat/          # Periodic health/summary polling
│   ├── observability/      # Logging, metrics helpers, context
│   └── tests/              # Core Python tests
│
├── extensions/             # Python platform/media integrations
│   ├── whatsapp/           # WhatsApp (Neonize)
│   ├── discord/            # Discord bot
│   ├── tts/                # Text-to-speech (EdgeTTS, MiniMax)
│   └── stt/                # Speech-to-text (faster-whisper, Deepgram)
│
├── services/               # Go service daemons
│   ├── sdk-go/             # Shared MCP-lite Go SDK
│   ├── hello/              # Reference hello tool service
│   ├── filesystem/         # File system tools
│   ├── shell/              # Shell + python execution tools
│   ├── discord/            # Discord service
│   ├── telegram/           # Telegram service
│   ├── slack/              # Slack service
│   └── whatsapp/           # WhatsApp service
│
├── app/                    # Minimalist web UI (FastAPI + HTMX)
│   ├── main.py             # FastAPI app
│   ├── routes/             # One module per page
│   ├── templates/          # Jinja2 HTML templates
│   ├── static/             # CSS + vendored HTMX
│   └── tests/              # UI/backend route tests
│
├── config/                 # openagent.yaml
├── data/                   # Runtime: sessions.db, memory/, sockets/
└── inspire/                # Reference implementations
```

## Python Extensions

Extensions handle platforms and media. Each is independently installable.

| Extension | Description | Dependencies |
|-----------|-------------|--------------|
| **whatsapp** | WhatsApp messaging via Neonize | neonize |
| **discord** | Discord bot integration | discord.py, aiohttp |
| **tts** | Text-to-speech (EdgeTTS / MiniMax) | edge-tts, aiohttp |
| **stt** | Speech-to-text (faster-whisper / Deepgram) | faster-whisper, deepgram-sdk |

## Go Services

Services run as long-lived daemons managed by `ServiceManager`. Python spawns them, connects via Unix socket, and can start/stop/restart/inspect them at runtime.

| Service | Description | Status |
|---------|-------------|--------|
| **hello** | Reference MCP-lite service (`hello.reply`) | Implemented |
| **filesystem** | Local file system tools | Implemented |
| **shell** | Shell execution + Python execution tools | Implemented |
| **discord** | Discord connector service | Implemented |
| **telegram** | Telegram connector service (gotd/td) | Implemented |
| **slack** | Slack connector service | Implemented |
| **whatsapp** | WhatsApp service (service path in progress alongside Python extension) | Implemented |
| **sdk-go** | Shared MCP-lite server/client codec helpers | Implemented |

Build a service:
```bash
cd services/my-service
go build -o bin/my-service .

# Cross-compile for Raspberry Pi
GOOS=linux GOARCH=arm64 go build -o bin/my-service-linux-arm64 .
```

## Development

**Python tests:**
```bash
python -m pytest openagent/tests app/tests
python -m pytest extensions/discord/tests
pytest extensions/whatsapp/tests/   # extension tests
python -m pytest extensions/stt/tests
python -m pytest extensions/tts/tests
```

Note: avoid running `pytest` blindly at repository root if `inspire/` contains vendored/reference test trees.

**Go tests:**
```bash
# from repo root
for d in services/*; do
  if [ -f "$d/go.mod" ]; then
    (cd "$d" && GOCACHE=/tmp/go-build go test ./...)
  fi
done
```

**Adding a new Python extension (platform/media):**
1. Create `extensions/<name>/` with its own `pyproject.toml`
2. Implement `BaseAsyncExtension` in `src/plugin.py`
3. Register via entry point in `openagent.extensions` group
4. Install: `pip install -e extensions/<name>`

**Adding a new Go service (compute/data):**
1. Create `services/<name>/` with `go.mod` and `main.go`
2. Implement MCP-lite protocol: handle `tools.list`, `tool.call`, `ping` on a Unix socket
3. Write `service.json` manifest declaring tool schemas and binary paths
4. Build for all targets; `ServiceManager` picks up the manifest automatically

## Web UI

A minimalist admin interface for monitoring and interacting with the agent. No authentication — designed for an isolated Raspberry Pi on a private network.

```bash
pip install -e app/
uvicorn app.main:app --host 0.0.0.0 --port 8080
```

Visit `http://<pi-ip>:8080`.

| Page | URL | Description |
|------|-----|-------------|
| Dashboard | `/` | Extension status + service status |
| Chat | `/chat` | Chat surface (agent loop wiring still being finalized) |
| Logs | `/logs` | Live log stream |
| Extensions | `/extensions` | Loaded Python extensions and status |
| Services | `/services` | Go services with status and restart controls |
| Config | `/config` | Read-only view of `openagent.yaml` |

Stack: FastAPI 3.x · Jinja2 · HTMX · Tailwind CSS (CDN) · WebSockets · SSE

## Documentation

| Doc | Purpose |
|-----|---------|
| [CLAUDE.md](CLAUDE.md) | Full development guide — architecture, MCP-lite, config, build order |
| [AGENTS.md](AGENTS.md) | Agent workflow rules — two-plane architecture, naming, testing |
| [CURSOR.md](CURSOR.md) | Cursor project context — layout, contracts, conventions |
| [roadmap.md](roadmap.md) | Consolidated Nanobot + Picoclaw comparison, build order, gaps |

## License

See [LICENSE](LICENSE) in this repository.
