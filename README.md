# OpenAgent

A deterministic, open-claw-style agent platform with a **Python + Go hybrid architecture**. The Python control plane orchestrates multi-agent pipelines via a local LLM. Go services handle compute-intensive work as managed daemons. Built to run on **Raspberry Pi** and other low-power hardware with offline 14B-parameter models.

## Architecture

```
Python Control Plane (Brain)          Go Services (Hands)
─────────────────────────────         ───────────────────
 Channel extensions                    Long-lived daemons
  WhatsApp, Discord        ──JSON──►   Heavy compute/data
 Agent loop + LLM calls    ◄──UDS──    Managed by Python
 Session + memory
```

Two clear planes, one socket each, no REST overhead:
- **Python extensions** → channels and media (WhatsApp, Discord, TTS, STT)
- **Go services** → compute and data-intensive tools
- **MCP-lite protocol** → tagged JSON frames over Unix Domain Sockets

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

## Project Structure

```
OpenAgent/
├── openagent/          # Core: orchestration, discovery, interfaces
│   ├── interfaces.py       # AsyncExtension protocol
│   ├── manager.py          # Extension discovery (entry points)
│   ├── agent/              # Agent loop, context, session, memory
│   ├── providers/          # LLM provider registry
│   ├── services/           # ServiceManager — Go daemon lifecycle
│   └── bus/                # Message bus (channel → agent → response)
│
├── extensions/             # Python channel/media integrations
│   ├── whatsapp/           # WhatsApp (Neonize)
│   ├── discord/            # Discord bot
│   ├── tts/                # Text-to-speech (EdgeTTS, MiniMax)
│   └── stt/                # Speech-to-text (faster-whisper, Deepgram)
│
├── services/               # Go service daemons
│   └── <name>/             # Self-contained Go module
│       ├── main.go         # UDS server + MCP-lite handler
│       ├── service.json    # Service manifest (tool schemas, binary paths)
│       └── go.mod
│
├── app/                    # Minimalist web UI (FastAPI + HTMX)
│   ├── main.py             # FastAPI app
│   ├── routes/             # One module per page
│   ├── templates/          # Jinja2 HTML templates
│   └── static/             # CSS + vendored HTMX
│
├── tests/                  # Python tests
├── config/                 # openagent.yaml
├── data/                   # Runtime: sessions.db, memory/, sockets/
└── inspire/                # Reference implementations (gitignored)
```

## Python Extensions

Extensions handle channels and media. Each is independently installable.

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
| *(first service — establishes pattern)* | Simple compute tool | Planned |
| **whatsapp** | Native WhatsApp via whatsmeow | Planned (migrating from Python extension) |

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
pytest                              # all tests
pytest tests/openagent/             # core only
pytest extensions/whatsapp/tests/   # extension tests
```

**Adding a new Python extension (channel/media):**
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
| Dashboard | `/` | Agent status, extension health, service health |
| Chat | `/chat` | Send messages, stream responses in real time |
| Logs | `/logs` | Live log stream |
| Extensions | `/extensions` | Loaded Python extensions and status |
| Services | `/services` | Go services with status and restart controls |
| Config | `/config` | Read-only view of `openagent.yaml` |

Stack: FastAPI 3.x · Jinja2 · HTMX · Tailwind CSS (CDN) · WebSockets · SSE

## License

See [LICENSE](LICENSE) in this repository.
