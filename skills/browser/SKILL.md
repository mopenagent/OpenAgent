---
name: browser
description: Two-turn web access via reqwest + dom_smoothie. Turn 1 searches SearXNG, Turn 2 fetches and extracts clean Markdown. No JavaScript execution.
hint: Call skill.read(name="browser") for the two-turn search→fetch workflow, caching details, and escalation guidance.
allowed-tools: web.search, web.fetch
enforce: false
---

# Browser Service

Pure Rust headless web access. No JavaScript — static pages only.

## Two-Turn Workflow

**This is the required pattern. Always use both turns.**

### Turn 1 — Search
```
web.search(query="...", max_results=5)
```
Returns a JSON array of `{url, title, snippet}`. Inspect the list and pick the most relevant URL.

### Turn 2 — Fetch
```
web.fetch(url="<url from Turn 1>")
```
Returns the page as clean Markdown extracted by Mozilla Readability. Feed directly to the LLM.

## When to Search vs Fetch Directly

- **Known URL** → skip `web.search`, call `web.fetch` directly
- **Unknown topic** → `web.search` first to discover relevant URLs, then `web.fetch` the best result
- **Multiple candidates** → call `web.fetch` on the top 1–2 results; compare and synthesise

## Sparse or Empty Content

If `web.fetch` returns very little content, the page likely requires JavaScript rendering.

Call `cortex.discover` to find a remote browser tool (e.g. `agent-browser` service) that can execute JavaScript. Do not retry `web.fetch` on the same URL.

## Caching

| Tool        | Cache TTL |
|-------------|-----------|
| `web.search` | 5 minutes |
| `web.fetch`  | 1 hour    |

Identical calls within the TTL return the cached result instantly.

## Limitations

- No JavaScript execution — SPAs, login-gated pages, and lazy-loaded content will be sparse
- No cookie/session persistence between calls
- Rate limiting by target site is possible; cache mitigates repeat hits
- Binary files (PDF, images) are not extracted — fetch the page that links to them instead

## Environment

| Variable              | Default                        | Purpose                  |
|-----------------------|--------------------------------|--------------------------|
| `SEARXNG_URL`         | `http://100.96.81.109:8888`    | SearXNG instance base URL |
| `OPENAGENT_SOCKET_PATH` | `data/sockets/browser.sock` | Unix socket path          |
| `OPENAGENT_LOGS_DIR`  | `logs`                         | OTEL output directory     |
