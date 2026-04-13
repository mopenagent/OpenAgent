# Skills System — OpenAgent

## Three-Tier Action Model

| Tier | What | Always in context? | How LLM gets more |
|---|---|---|---|
| **Capability** | Pinned meta-tools (memory.search, web.*, agent.discover, skill.read, sandbox.*) | Yes — every turn, full schema | N/A |
| **Skill** | Domain knowledge + patterns | Summary only (top-k semantic match) | `skill.read(name=...)` |
| **Tool** | Service integrations (browser, cron, ...) | Never auto-injected | `agent.discover` → schema → call |

## SKILL.md Format

```markdown
---
name: agent-browser
description: Browser automation CLI for AI agents.
hint: Call skill.read(name="agent-browser") for commands, patterns, and auth workflows.
allowed-tools: browser.open, browser.navigate, browser.snapshot
enforce: false
enabled: true
---

# Full skill body here...
```

**Frontmatter fields:**

| Field | Required | Purpose |
|---|---|---|
| `name` | yes | Unique identifier — used by `skill.read` |
| `description` | yes | One-line summary in semantic search results |
| `hint` | yes | Exact call-to-action: `Call skill.read(name="<name>") for ...` |
| `allowed-tools` | no | Tools this skill uses |
| `enforce` | no | `true` = agent rejects calls outside `allowed-tools` (use sparingly) |
| `enabled` | no | `false` = excluded from catalog (default: `true`) |

## Progressive Disclosure — Three Levels

**Level 1 — Semantic search (automatic)**
Every `agent.step`, catalog is scored against user input. Matching skills appear as one line:
```
skill: agent-browser  Browser automation CLI for AI agents.
hint: Call skill.read(name="agent-browser") for commands, patterns, and auth workflows.
```
Full body is never injected at this level.

**Level 2 — Full skill on-demand**
LLM calls `skill.read(name="agent-browser")` → receives full SKILL.md body + table of available references:
```
## Available References
- authentication — Login flows, OAuth, 2FA
- commands — Full command reference
```

**Level 3 — Reference on-demand**
LLM calls `skill.read(name="agent-browser", reference="authentication")` → receives that file's content.

## File Structure

```
skills/<name>/
  SKILL.md              entry point (required)
  references/           deep-dive docs loaded on demand
    authentication.md
    commands.md
  templates/            ready-to-run scripts
    form-automation.sh
  drafts/               agent-generated candidates — pending human review (gitignored)
```

## Knowledge Lifecycle

```
1. Human writes seed SKILL.md
2. Agent runs tasks → diary entries written per session
3. Phase 8 Reflection scans diary → extracts learnings → draft files in skills/<name>/drafts/
4. Human reviews drafts → edits, approves, or discards
5. Approved content merged into live SKILL.md manually
```

Agent-generated learnings **never automatically modify a live skill**.

## Authoring Rules

- Every skill must have `name`, `description`, and `hint` in frontmatter
- `hint` must name the exact call: `Call skill.read(name="<name>") for ...`
- Keep `description` to one sentence — it appears in the context block
- `enforce: true` only for critical, non-negotiable workflows
- Skills emerge from real usage — do not pre-create for tools that haven't been used in multi-step patterns
