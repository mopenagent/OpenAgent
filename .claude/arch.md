# Architecture — OpenAgent

## Migration Trajectory

| Phase | Control plane | Python role | Tower/Axum role |
|---|---|---|---|
| Phase 1 ✅ | Python `AgentLoop` calls `agent.step` via MCP-lite | Full control plane | None |
| Phase 2 ✅ | Rust `openagent` binary. Agent owns ReAct loop + tool routing + memory. Tower middleware in `openagent`. | Web UI only | Full Tower stack (GuardLayer → SttLayer → TtsLayer) |
| Phase 3 ✅ (current) | `openagent` Axum :8080. Platform connectors connect directly. Python web UI is a separate container. | Retired as control plane | Axum is the control plane |

**Permanent decisions:**
- MCP-lite JSON/TCP is the permanent internal protocol — Axum never replaces it between `openagent` and services.
- New middleware goes into Tower layers in `openagent/src/` — never Python.

## MCP-lite Wire Protocol

One TCP connection per service (loopback). Newline-delimited JSON frames.

**Requests (agent → service):**
```json
{"id":"<uuid>","type":"tools.list"}
{"id":"<uuid>","type":"tool.call","tool":"<name>","params":{}}
{"id":"<uuid>","type":"ping"}
```

**Responses (service → agent, same `id`):**
```json
{"id":"<uuid>","type":"tools.list.ok","tools":[...]}
{"id":"<uuid>","type":"tool.result","result":"<string>","error":null}
{"id":"<uuid>","type":"pong","status":"ready"}
{"id":"<uuid>","type":"error","code":"SERVICE_ERROR","message":"..."}
```

**Events (service → agent, no `id`, unprompted):**
```json
{"type":"event","event":"message.received","data":{}}
{"type":"event","event":"connection.status","data":{"connected":true}}
```

Why TCP not UDS: works across localhost, LAN, and Docker without socket-file lifecycle management. `TCP_NODELAY` set to eliminate Nagle latency.

## Service Manifest (`service.json`)

Schema-first: the manifest is the only contract. Core must not depend on service internals.

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
      "description": "What the tool does — write this for the LLM",
      "internal": false,
      "params": {
        "type": "object",
        "properties": { "input": {"type":"string","description":"..."} },
        "required": ["input"]
      }
    }
  ],
  "events": ["message.received"]
}
```

`"internal": true` — tool is dispatch/maintenance plumbing; hidden from `agent.discover`.

## Port Allocation

| Service   | Port |
|-----------|------|
| memory    | 9000 |
| browser   | 9001 |
| sandbox   | 9002 |
| stt       | 9003 |
| tts       | 9004 |
| validator | 9005 |
| whatsapp  | 9010 |

Assign the next available port to new services. Never reuse a port.

## ServiceManager

`ServiceManager` in `openagent` connects to externally-managed services over TCP. It does **not** spawn or restart them — that is `services.sh` (dev) or systemd (prod).

Responsibilities:
1. Read `service.json` manifests from `services/*/service.json` on startup
2. Spawn a `connection_loop` task per enabled service — connects to `address` from manifest
3. Send `tools.list` → register tools into agent loop
4. Health-check loop (ping/pong every 5s); reconnect automatically on restart
5. Subscribe to event frames → route to dispatch loop

Tool routing is by name prefix: `browser.fetch` → `browser` service.

## Sandbox / MSB Setup

The `sandbox` service requires a running microsandbox server:

```bash
cargo install msb              # or brew install microsandbox/tap/msb
msb server start --dev         # dev mode — no API key required
msb server keygen              # generate key for production
```

Env vars:
```
MSB_SERVER_URL=http://127.0.0.1:5555
MSB_API_KEY=<key>              # required unless --dev
MSB_MEMORY_MB=512
```

MSB methods (POST `/api/v1/rpc`, Bearer auth):
- `sandbox.start` — create named sandbox with OCI image + resource limits
- `sandbox.repl.run` — execute Python/Node snippet (`sandbox.execute` tool)
- `sandbox.command.run` — run shell command (`sandbox.shell` tool)
- `sandbox.stop` — destroy sandbox after each invocation

## Rust Service main.rs Pattern

```rust
// 1. Load .env (dotenvy, best-effort)
// 2. Init OTEL: sdk_rust::setup_otel("service-name", logs_dir)
// 3. Build McpLiteServer from sdk-rust
// 4. Register tool handlers (closures or fns)
// 5. server.serve_auto("0.0.0.0:<port>").await
//    reads OPENAGENT_TCP_ADDRESS env var; falls back to hardcoded default
// 6. SIGTERM handled automatically by tokio runtime shutdown
```

## Action Catalog — Tool Visibility

Three tiers:
- **Pinned** — always injected every turn: `memory.search`, `web.search`, `web.fetch`, `agent.discover`, `skill.read`, `sandbox.execute`, `sandbox.shell`
- **Discoverable** — surfaced via `agent.discover` when the LLM searches; `"internal": false` in service.json
- **Internal** — never shown to LLM; `"internal": true` in service.json (stt, tts, validator, whatsapp plumbing, memory maintenance ops)

## Deployment

**Raspberry Pi (primary):** `linux/arm64` for all Rust binaries. `faster-whisper int8 small` for STT. EdgeTTS (no API key) for TTS. SQLite + LanceDB — no Postgres, no Redis.

**Dev (M4 Mac / Ubuntu):** Same codebase, different arch. Docker optional — `Dockerfile.debug` builds everything inside Linux container, mounts `./logs` and `./data` from host.

## Reference Implementations (inspire/ — gitignored)

Read before implementing anything non-trivial:
- Agent loop → `inspire/nanobot/nanobot/agent/loop.py`
- Provider registry → `inspire/nanobot/nanobot/providers/registry.py`
- Multi-agent registry → `inspire/picoclaw/pkg/agent/registry.go`
