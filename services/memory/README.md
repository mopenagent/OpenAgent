# Memory Service

MCP-lite daemon that gives OpenAgent agents **persistent, structured memory** — not just retrieval, but genuine learning over time. Built on **LanceDB** (embedded vector engine) and **FastEmbed-rs** (local ONNX embeddings), with no external services or daemons required.

---

## Architecture Overview

The memory service implements a **5-layer Hindsight-Nash hybrid** architecture. Memory is treated as a versioned knowledge structure rather than a flat database: raw session events are extracted into typed facts, reinforced over time, and periodically synthesised into higher-order knowledge by a background Dreaming job.

### The Five Layers

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 5 — Peer Cards                                        │
│  Stable biographical profiles, always injected into context  │
├─────────────────────────────────────────────────────────────┤
│  Layer 4 — Dreaming Job                                      │
│  Background compaction: consolidation, promotion, deduction  │
├─────────────────────────────────────────────────────────────┤
│  Layer 3 — Semantic Deduplication                            │
│  remember_or_reinforce(): reinforce existing vs insert new   │
├─────────────────────────────────────────────────────────────┤
│  Layer 2 — Extraction                                        │
│  Batch LLM pass: typed facts from raw diary entries          │
├─────────────────────────────────────────────────────────────┤
│  Layer 1 — Atomic Memories (LanceDB)                         │
│  World / Experience / Observation facts with full metadata   │
└─────────────────────────────────────────────────────────────┘
```

---

## Fact Types and Store Mapping

Three typed fact categories map directly to the existing LanceDB stores:

| Fact Type | Current Store | Description |
|---|---|---|
| **Experience** | `diary` | Raw turn-by-turn session records — what happened each turn. Append-only JSONL. Immutable by nature — the natural audit trail. |
| **Observation** | `memory` | Generalised patterns abstracted from sessions. Promoted from Experience after 3+ reinforcements by the Dreaming job. |
| **World** | `knowledge` | External curated objective facts. API endpoints, project decisions, business rules — things true regardless of session context. |

Each LanceDB record carries:

| Field | Description |
|---|---|
| `id` | UUID |
| `content` | Text content |
| `vector` | Embedding (384-dim, BAAI/bge-small-en-v1.5) |
| `type` | `experience` / `observation` / `world` |
| `bank_id` | Owning bank identifier |
| `tier` | `user` / `account` / `platform` |
| `anchor_id` | Block ID linking to a specific line in `KB.md` (e.g. `fact-8821`) |
| `confidence` | Float 0–1, boosted on reinforcement |
| `occurrences` | Count of how many times this fact has been reinforced |
| `timestamp` | Unix epoch of last write |
| `metadata` | Arbitrary JSON (session_id, source, entity_labels, etc.) |

---

## Bank Structure (Isolation Model)

Every agent context is scoped to a **Bank** — a privacy boundary that prevents memory from leaking across users or accounts.

### User Tier — Physical Isolation

Each user gets a dedicated directory:

```
data/memory/banks/<user_id>/
  diary.jsonl          ← Experience: append-only session records
  KB.md                ← Observation/World: current compacted knowledge
  KB_<timestamp>.md    ← KB snapshots — last N dreams kept for rollback
  peer_card.md         ← Stable biographical profile
```

User-tier LanceDB records are filtered by `bank_id=<user_id>` and `tier=user`. No user's memories are visible to another user's agent.

### Account Tier — Logical Isolation

Shared workspace knowledge lives in the shared LanceDB instance, filtered by `bank_id=<account_id>` and `tier=account`. Business facts, project context, and team patterns that benefit every agent in the workspace.

### Platform Tier — Logical Isolation

Anonymised cross-account patterns. Filtered by `tier=platform`. Read-only from any individual agent's perspective — written only by the Dreaming job when a pattern reaches sufficient confidence across multiple accounts.

---

## Layer 1 — Atomic Memories (Write Path)

### Step 1: Diary Append (Experience)

Every conversation turn is appended to `diary.jsonl` — structured, timestamped, immutable:

```jsonl
{"turn": 42, "session_id": "...", "user": "...", "assistant": "...", "ts": 1743500000}
```

The diary is the raw source of truth. It is never modified — only appended.

### Step 2: Batch Extraction

After a cycle completes (not after every turn), a single LLM call reviews all new diary entries together and extracts typed facts. Batching across the cycle identifies cross-turn patterns that per-turn extraction would miss, and reduces extraction cost by 60–70%.

Each extracted fact carries:
- `type`: World / Experience / Observation
- `content`: the fact statement
- `entity_labels`: structured tags (e.g. `tech_stack: [rust, lancedb]`)
- `anchor_id`: block ID for linking to KB.md

### Step 3: `remember_or_reinforce()`

Before writing any fact to LanceDB, the service searches for semantic duplicates:

```
search LanceDB for cosine similarity >= 0.9
  match found  → increment occurrences, boost confidence (no new record)
  no match     → insert new atomic fact
```

This prevents memory bloat. Repeated patterns naturally get stronger rather than noisier. Single observations stay as observations — a preference requires evidence from at least two independent data points before promotion.

---

## Layer 3 — Recall (Read Path)

Every recall operation follows this sequence:

1. **Peer Card pre-pend** — always injected first, no vector search needed
2. **RRF merge** — Reciprocal Rank Fusion of:
   - LanceDB dense vector search (semantic similarity)
   - BM25 keyword search over `KB.md`
3. **Temporal decay** — recent facts ranked higher within equal-scoring results
4. **Type filter** — caller can restrict results to `world`, `experience`, or `observation`

Returns up to 5 results ranked by combined RRF score, each including `id`, `content`, `metadata`, `type`, `confidence`, `occurrences`, `anchor_id`, `timestamp`.

---

## Layer 4 — The Dreaming Job

The Dreaming job is the learning engine. It runs as a background pass and transforms raw Experience into structured Observation, detects contradictions, and keeps the KB current.

### Trigger

- Nightly (time-based)
- After a session ends (event-based)

### What it does

**1. Consolidation**
Groups semantically similar facts (threshold 0.85 — looser than the 0.9 dedup threshold). Merges them via LLM into a single stronger statement. The originals are archived; the consolidated record inherits the sum of all occurrences.

**2. Promotion**
Experience facts reinforced 3+ times are promoted to Observation type with a confidence boost. The system recognises that something noticed repeatedly is a reliable signal, not noise.

**3. Induction Specialist**
Scans for recurring behavioural patterns across multiple observations. A preference must have evidence from at least two independent data points before it is promoted — single observations stay as observations.

**4. Deduction Specialist**
Detects contradictions in the KB. If a user changes role, location, or preference, the specialist catches conflicting facts, resolves them, and deprecates the stale version.

**5. KB.md Update**
Writes the Dreaming output to `KB.md` with block anchors that link every line back to its LanceDB record:

```markdown
- User consistently prefers async communication over meetings. ^fact-8821
- Primary tech stack: Rust, LanceDB, FastEmbed. ^fact-9104
```

The `anchor_id` (`fact-8821`) is stored in LanceDB. If the KB is reorganised, the anchor survives — vector results always link back to the exact KB line.

**6. KB Snapshot**
Before overwriting `KB.md`, the previous version is saved as `KB_<timestamp>.md`. The last N snapshots are retained, providing rollback without Git overhead.

**7. Peer Card update**
The `## Learned` section of `peer_card.md` is updated with newly promoted stable facts.

### Dreaming LLM Config

The Dreaming job uses a dedicated provider — typically a lighter, cheaper model than the main agent:

```toml
[memory.dreaming]
enabled  = true
provider = "openai_compat"
base_url = "http://localhost:11434/v1"
model    = "llama3:8b"
api_key  = ""
trigger  = "nightly"          # nightly | session_end | both
kb_snapshots_kept = 5         # how many KB_<timestamp>.md files to retain
```

---

## Layer 5 — Peer Cards

A Peer Card is a stable biographical profile for each user. It is always injected into the system prompt at session start — no vector search, no relevance scoring, 100% hit rate for critical context.

```markdown
---
user_id: "telegram:916356737267"
last_updated: "2026-04-01"
---

## Static
<!-- Human-maintained. Never modified by the agent. -->
NAME: Keith
TIMEZONE: US/Pacific
ROLE: Founder, technical product lead
INSTRUCTION: Do not create documents unless explicitly asked

## Learned
<!-- Written by the Dreaming job only. Auditable via KB snapshots. -->
PREFERENCE: Outcome-oriented execution without repeated approval-seeking
PREFERENCE: Async communication over synchronous meetings
TRAIT: Delegates broadly to trusted agents
INTEREST: AI agent architectures, autonomous systems
```

**`## Static` section** — human-only. The agent never writes here.

**`## Learned` section** — written exclusively by the Dreaming job. Changes are captured in KB snapshots for auditability.

Cards are bootstrapped on first access from existing LanceDB data. Up to 40 atomic facts, prefixed by category.

---

## Current Tools (Phase 1)

| Tool | Description |
|---|---|
| `memory.index` | Embed and store content into a store (`memory` / `diary` / `knowledge`) |
| `memory.search` | Hybrid RRF search (dense + BM25) across stores |
| `memory.delete` | Delete a document by id, or purge an entire store |
| `memory.prune` | Remove diary entries older than `max_age_secs` |
| `memory.diary_write` | Write a stub diary row (zero-vector placeholder) after each ReAct turn; compaction back-fills embeddings |

---

## Tech Stack

| Component | Choice | Why |
|---|---|---|
| Protocol | MCP-lite (sdk-rust, Unix socket) | Standard internal protocol |
| Vector engine | `lancedb` | Embedded, file-based, no daemon, fast Lance format |
| Embeddings | `fastembed` | Pure Rust, local ONNX, no API calls |
| BM25 search | Built into LanceDB | Hybrid search without a second engine |
| Dreaming LLM | `reqwest` → configured provider | Lightweight, uses existing provider layer |
| Runtime | `tokio` | Async throughout |
| Tracing | OpenTelemetry (file-based) | Same pattern as all other services |

---

## Prerequisites

- **Rust** 1.70+
- **protoc** (required by LanceDB):
  ```bash
  brew install protobuf        # macOS
  apt install protobuf-compiler  # Linux
  ```

---

## Build

```bash
make memory    # cross-compile all targets
make local     # current host only (faster dev loop)
```

Or directly:

```bash
cd services/memory
cargo build --release
```

Binaries land in `bin/memory-<platform>` (e.g. `bin/memory-darwin-arm64`).

---

## Run

Managed by OpenAgent's **ServiceManager** — reads `service.json`, spawns the binary, connects via Unix socket.

Standalone (for testing):

```bash
OPENAGENT_SOCKET_PATH=data/sockets/memory.sock \
OPENAGENT_MEMORY_PATH=./data/memory \
./bin/memory-darwin-arm64
```

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `OPENAGENT_SOCKET_PATH` | `data/sockets/memory.sock` | Unix socket path |
| `OPENAGENT_MEMORY_PATH` | `./data/memory` | LanceDB + bank storage root |
| `OPENAGENT_LOGS_DIR` | `logs` | OTEL trace output directory |
| `RUST_LOG` | `info` | Log level |

---

## Phased Roadmap

### Phase 1 — Current (Retrieval)
- `memory`, `diary`, `knowledge` stores in LanceDB
- Hybrid RRF search (dense + BM25)
- `memory.diary_write` stub rows; compaction back-fills embeddings
- `memory.prune` for diary TTL

### Phase 2 — Fact Extraction
- Introduce `type` field: `experience` / `observation` / `world`
- Migrate existing stores to typed fact model
- `remember_or_reinforce()` deduplication on write
- Bank directory structure (`data/memory/banks/<user_id>/`)
- Account + platform tier logical isolation

### Phase 3 — Dreaming Job
- Background compaction trigger (nightly + session end)
- Consolidation, promotion (3+ reinforcements → Observation)
- Induction specialist (2+ data points required)
- Deduction specialist (contradiction detection + resolution)
- KB.md generation with block anchors (`^fact-xxxx`)
- KB snapshot rotation (last N kept for rollback)
- `[memory.dreaming]` config block with dedicated LLM provider

### Phase 4 — Peer Cards
- `peer_card.md` per user bank
- `## Static` (human-only) + `## Learned` (Dreaming job) sections
- Auto-injection into system prompt at session start via Cortex
- Bootstrap from existing LanceDB data on first access

### Phase 5 — Platform Tier + Self-Improvement Loop
- Anonymised pattern promotion to platform tier
- Cycle quality scoring (1–10) injected into next planning context
- Rolling performance log (7-day) for agent self-correction
- Agent-driven memory: explicit `saveMemory` calls skip post-task extraction
