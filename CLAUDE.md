# CLAUDE.md — OpenAgent

## What We're Building

Deterministic agent platform. **Rust-first** — `openagent` Rust binary owns the ReAct loop, Tower middleware, and Axum control plane (:8080). Rust services (`services/*/`) handle all CPU/IO work over MCP-lite (JSON/TCP). Go only for WhatsApp (`services/whatsapp/`). Python web UI is a separate container. Target: Raspberry Pi (arm64).

**Phase 3 (current):** `openagent` Axum is the control plane. Python retired as control plane.

## Hard Limits
- Functions: <= 100 lines
- Cyclomatic complexity: <= 8
- Positional parameters: <= 5
- Line length: 100 characters
- Files: <= 500 lines

## Python Tooling
- Package manager: uv
- Linting + formatting: ruff
- Type checking: ty
- Testing: pytest
- Run commands: Always uv run <tool>

## Behavioral Rules
- NEVER create files unless absolutely necessary
- ALWAYS read a file before editing it
- ALWAYS prefer editing existing files to creating new ones
- NEVER commit secrets, credentials, or .env files

## Error handling
- Fail fast with clear, actionable messages
- Never swallow exceptions silently
- Include context (what operation, what input, suggested fix)

## Approach
- Think before acting. Read existing files before writing code.
- Be concise in output but thorough in reasoning.
- Prefer editing over rewriting whole files.
- Do not re-read files you have already read unless the file may have changed.
- Test your code before declaring done.
- No sycophantic openers or closing fluff.
- Keep solutions simple and direct.
- User instructions always override this file.

## Code Rules
- Simplest working solution. No over-engineering.
- No abstractions for single-use operations.
- No speculative features or "you might also want..."
- Read the file before modifying it. Never edit blind.
- No docstrings or type annotations on code not being changed.
- No error handling for scenarios that cannot happen.
- Three similar lines is better than a premature abstraction.

## Review Rules
- State the bug. Show the fix. Stop.
- No suggestions beyond the scope of the review.
- No compliments on the code before or after the review.

## Debugging Rules
- Never speculate about a bug without reading the relevant code first.
- State what you found, where, and the fix. One pass.
- If cause is unclear: say so. Do not guess.

## Simple Formatting
- No em dashes, smart quotes, or decorative Unicode symbols.
- Plain hyphens and straight quotes only.
- Natural language characters (accented letters, CJK, etc.) are fine when the content requires them.
- Code output must be copy-paste safe.

## Output
- Return code first. Explanation after, only if non-obvious.
- No inline prose. Use comments sparingly - only where logic is unclear.
- No boilerplate unless explicitly requested.


## Hard Rules — Never Break

1. **No secrets in code** — `.env` holds secrets; never commit it. `.env.example` is the template.
2. **No new Go services** — all new services are Rust. WhatsApp is the only Go exception.
3. **No `axum`/`tower` in service crates** — Tower/Axum lives in `openagent/` only.
4. **`service.json` is the only contract** — core must not depend on service internals.
5. **No `unwrap()`/`expect()` in non-test Rust** — use `Result<_, E>` with `thiserror`.
6. **No `unbounded_channel`** — always `mpsc::channel(N)` with explicit capacity.
7. **No `static mut`** — use `Arc<Mutex<T>>` or `Arc<RwLock<T>>`.
8. **MCP-lite is permanent** — JSON/TCP between `openagent` and services; never replace with Axum.

## Communication Protocol

Ask clarifying questions **one at a time** when intent is unclear. Provide options. Do not assume the path.

## Git Commits

Format: `<type>: <short summary>` — no `Co-Authored-By` trailers, no metadata lines.

## Quick Commands

```bash
openagent                      # run the control plane
make local                     # build all services for current host
make local-<svc>               # build one service (memory, browser, sandbox, ...)
make whatsapp                  # build Go service
./services.sh start            # start all services (dev)
./services.sh start <svc>      # start one service
msb server start --dev         # start microsandbox (required for sandbox service)
pytest                         # Python tests
cargo test                     # Rust tests (run from service or openagent dir)
```

## Repo Layout

```
openagent/        Rust control plane — agent loop, Tower middleware, Axum :8080
services/<name>/  Rust (or Go for whatsapp) — MCP-lite daemons
  service.json    service manifest — schema-first contract
  src/main.rs     McpLiteServer + tool handlers
services/sdk-rust/ shared MCP-lite server library
services/sdk-go/  shared MCP-lite library (whatsapp only)
app/              Python web UI — FastAPI + HTMX, no auth, Pi-only POC
config/           openagent.toml — primary config
skills/           domain knowledge (SKILL.md + references/ + templates/)
bin/              compiled binaries (gitignored)
data/             runtime storage — SQLite, LanceDB, artifacts (gitignored)
logs/             OTEL JSONL output (gitignored)
```

## Coding Standards

**Rust:** `sdk-rust` for MCP-lite boilerplate. `tokio` features: `["rt-multi-thread","macros","sync","net"]` — never `"full"`. `mimalloc` as global allocator. `[profile.release]`: `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`. Typestate for lifecycle. Traits for pluggable components. Actor model (bounded channels) for concurrency. → full patterns: `.claude/rust.md`

**Go (whatsapp only):** Goroutine per request. `OPENAGENT_TCP_ADDRESS` env for TCP address. Graceful SIGTERM. Binaries → `bin/<name>-<os>-<arch>`.

**Python:** ≥3.11, type hints on public APIs. `aiohttp` for HTTP — no OpenAI SDK, no `requests`. `asyncio.to_thread()` for sync libs. Pydantic for config/models. No global mutable state.

**Tests:** No real network or LLM calls. Mock services with asyncio TCP stub. `pytest-asyncio` for async. Tests belong to their vertical (`openagent/tests/`, `app/tests/`, `services/<name>/`).

**Observability:** OTEL mandatory — traces, logs, metrics to `logs/` as JSONL via `sdk_rust::setup_otel` (Rust) or `openagent/observability/` (Python). One structured log per request: id, tool, outcome, duration.

## When You Need More

| Topic | File |
|---|---|
| Architecture, migration phases, MCP-lite wire protocol, port table | `.claude/arch.md` |
| Rust patterns, anti-patterns, Tower/Axum layer order | `.claude/rust.md` |
| Skills system — three-tier model, progressive disclosure, authoring | `.claude/skills.md` |
| Build order — phases done and next | `.claude/build.md` |
| Service manifest schema, ServiceManager, sandbox/MSB setup | `.claude/arch.md` |
| Roadmap and Nanobot/Picoclaw comparison | `roadmap.md` |
