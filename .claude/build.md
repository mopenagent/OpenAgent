# Build Order — OpenAgent

## Completed ✅

- Core: extension discovery, lifecycle, async interfaces
- ServiceManager — MCP-lite health loop, TCP connection management
- Message bus — InboundMessage, OutboundMessage, per-session fanout
- Agent loop — custom ReAct, ToolRegistry, 40 max iters, 500-char truncation
- Session manager — SessionBackend protocol, SQLite impl, auto-summarise
- Provider layer — Anthropic, OpenAI, OpenAI-compat (httpx, no SDK)
- Channel adapters — Discord, Telegram, Slack, WhatsApp (Go/whatsmeow)
- Tower middleware — GuardLayer, SttLayer, TtsLayer; Python middleware deleted
- Rust services: sandbox, browser, memory, stt, tts, validator, discord, telegram, slack
- Go service: whatsapp (only Go service retained)
- Action catalog — keyword-ranked top-k; pinned capabilities; internal tool flag
- Provider fallback chain — `dispatch_with_fallback()`, `fallbacks: Vec<FallbackProvider>`
- Rate limiting — `ConcurrencyLimitLayer` (max 50)
- Research DAG — cross-session research (SQLite + markdown); multi-agent dispatch
- Agent Phase 5 — ActionCatalog, semantic search, `agent.discover`, `skill.read`
- Skills — `hint` + `enforce` frontmatter; progressive disclosure (3 levels)
- Per-service Docker layers — `local-<svc>` Makefile targets; memory first, sandbox last

## Next (in order)

12. **Skills — `skill.read` full implementation** — `handle_skill_read()` improvements; existing SKILL.md files updated with `hint` + `enforce`
13. **Agent Phase 8: Reflection** — background synthesis after research completes; scans diary → writes draft files to `skills/<name>/drafts/`
14. **Agent Phase 9: Curiosity queue** — research leads surfaced as non-intrusive suggestions

See `roadmap.md` for full Nanobot/Picoclaw comparison and detailed gaps.

## Build Commands

```bash
make local                  # all services + openagent, current host
make local-memory           # single service (for Docker layer caching)
make local-sandbox
make local-browser
make local-tts
make local-stt
make local-validator
make local-whatsapp
make openagent-local        # control plane binary only
make all                    # cross-compile all targets (linux-arm64, linux-amd64, darwin-arm64)
make whatsapp               # Go service, all targets
```
