# OpenAgent

A deterministic Python + Rust hybrid agent platform for low-power/offline deployments (including Raspberry Pi).  
Python is the control plane (orchestration, routing). Rust services are the data plane (long-lived tools and integrations) over MCP-lite on Unix sockets. Only WhatsApp remains implemented in Go.

## Current Status

**Implemented:**
- **ServiceManager** — spawns, health-checks, restarts service daemons (`openagent/services/manager.py`)
- **Message bus** — `InboundMessage`, `OutboundMessage`, `SenderInfo`, per-session fanout (`openagent/bus/`)
- **Agent loop** — custom ReAct loop, no framework dependency (`openagent/agent/loop.py`)
- **Session manager** — `SessionBackend` protocol, SQLite impl, optional summarisation (`openagent/session/`)
- **Tool registry** — dispatches to services via MCP-lite (`openagent/agent/tools.py`)
- **Provider layer** — Anthropic, OpenAI, OpenAI-compat (httpx-based, no SDK)
- **MCP-lite** — Python client + Rust SDK (`openagent/platforms/mcplite.py`, `services/sdk-rust/`)
- **Heartbeat** — periodic health/summary polling (`openagent/heartbeat/`)
- **Platform adapters** — Discord, Telegram, WhatsApp, Slack (Python MCP-lite clients)
- **Rust services** — `sandbox`, `discord`, `telegram`, `slack`, `stt`, `tts`, `browser`, `memory`
- **Go service** — `whatsapp` (only remaining Go service)
- **Web UI** — FastAPI + HTMX (dashboard, chat, services, config)

**In progress:**
- Config schema extension (agents, bindings, session, tools)

## Architecture

```
Python Control Plane (Brain)          Rust Services (Hands)
─────────────────────────────         ────────────────────
 Provider calls + orchestration ──JSON──► Long-lived daemons
 Message bus + health/heartbeat ◄─UDS──  Compute/data integrations
                                          Managed by ServiceManager
```

Two clear planes, one socket each, no REST overhead:
- **Python** — control plane, orchestration, platform adapters
- **Rust services** — compute, data, and platform connectors (Rust-first)
- **Go** — WhatsApp only (whatsmeow)
- **MCP-lite** newline-delimited JSON frames over Unix Domain Sockets

## Requirements

- **Python 3.11+**
- **Rust** (for building Rust services; `cargo` + `cross` for cross-compilation)
- **Go 1.21+** (only for WhatsApp service)
- A local LLM via an OpenAI-compatible endpoint (e.g. [Ollama](https://ollama.com))
- **agent-browser** (for browser service: `npm install -g agent-browser` then `agent-browser install`)

## Installation

```bash
git clone https://github.com/kmaneesh/OpenAgent.git
cd OpenAgent

# Install core
pip install -r requirements.txt
# or: pip install -e .
```

## Quick Start

```bash
# Copy and edit the config
cp config/openagent.yaml.example config/openagent.yaml

# Run
openagent
# or
python -m openagent.main

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
├── services/               # Rust (primary) + Go (WhatsApp, telegram, slack)
│   ├── sdk-rust/           # Shared MCP-lite Rust SDK
│   ├── sandbox/            # Rust — VM-isolated code/shell execution (microsandbox)
│   ├── discord/            # Rust — Discord connector
│   ├── telegram/           # Go — Telegram connector
│   ├── slack/              # Go — Slack connector
│   ├── whatsapp/           # Go — WhatsApp (whatsmeow; only Go service retained)
│   ├── stt/                # Rust — Speech-to-text
│   ├── tts/                # Rust — Text-to-speech
│   ├── browser/            # Rust — MCP-lite wrapper for agent-browser CLI
│   └── memory/             # Rust — Vector memory
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

## Services (Rust-first, WhatsApp in Go)

Services run as long-lived daemons managed by `ServiceManager`. Python spawns them, connects via Unix socket, and can start/stop/restart/inspect them at runtime.

| Service | Language | Description |
|---------|----------|-------------|
| **sandbox** | Rust | VM-isolated code/shell execution (microsandbox) |
| **discord** | Rust | Discord connector |
| **telegram** | Go | Telegram connector |
| **slack** | Go | Slack connector |
| **whatsapp** | Go | WhatsApp (whatsmeow) |
| **stt** | Rust | Speech-to-text |
| **tts** | Rust | Text-to-speech |
| **browser** | Rust | Headless browser automation via agent-browser CLI (`npm install -g agent-browser`) |
| **memory** | Rust | Vector memory (LanceDB) |

Build Rust services:
```bash
make local    # Build for current host
make all      # Cross-compile for all targets (Pi, etc.)
```

Build WhatsApp (Go):
```bash
cd services/whatsapp && go build -o bin/whatsapp .
# Cross-compile for Raspberry Pi:
GOOS=linux GOARCH=arm64 go build -o bin/whatsapp-linux-arm64 .
```

## Development

**Python tests:**
```bash
python -m pytest openagent/tests app/tests
```

Note: avoid running `pytest` blindly at repository root if `inspire/` contains vendored/reference test trees.

**Rust service tests:**
```bash
cd services/<name> && cargo test
```

**Go tests (WhatsApp only):**
```bash
cd services/whatsapp && go test ./...
```

**Adding a new Rust service:**
1. Create `services/<name>/` with `Cargo.toml` and `src/main.rs`
2. Use `sdk-rust` for MCP-lite server boilerplate
3. Write `service.json` manifest declaring tool schemas and binary paths
4. Add to Makefile; `ServiceManager` picks up the manifest automatically

**Adding a new Go service (rare; prefer Rust):**
1. Create `services/<name>/` with `go.mod` and `main.go`
2. Implement MCP-lite protocol: handle `tools.list`, `tool.call`, `ping` on a Unix socket
3. Write `service.json` manifest
4. Build for all targets

## Web UI

A minimalist admin interface for monitoring and interacting with the agent. No authentication — designed for an isolated Raspberry Pi on a private network.

```bash
pip install -e app/
uvicorn app.main:app --host 0.0.0.0 --port 8080
```

Visit `http://<pi-ip>:8080`.

| Page | URL | Description |
|------|-----|-------------|
| Dashboard | `/` | Service status + system stats |
| Chat | `/chat` | Chat surface |
| Services | `/services` | Rust/Go services with status and restart controls |
| Config | `/config` | Read-only view of `openagent.yaml` |

Logging is OTEL-compliant (OpenTelemetry); traces, logs, and metrics are written to `logs/` as JSONL.

Stack: FastAPI 3.x · Jinja2 · HTMX · Tailwind CSS (CDN) · WebSockets · SSE

## License

See [LICENSE](LICENSE) in this repository.
