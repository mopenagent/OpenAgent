---
name: rust-guidelines
description: Rust coding guidelines — pragmatic checklist, 179 rules across 14 categories, architecture patterns, and OpenAgent service conventions. Use when writing, reviewing, or architecting Rust code.
hint: Call skill.read(name="rust-guidelines") for the full checklist, architecture patterns, compiler-error design questions, and domain-specific patterns.
version: 2.0.0
enforce: false
---

# Rust Guidelines

Authoritative Rust reference combining: [Microsoft Rust Guidelines](https://microsoft.github.io/rust-guidelines/), 179 production rules, architecture patterns, and OpenAgent service conventions. Apply the spirit, not the letter. `must` = always hold; `should` = flexibility allowed.

## Golden Rule

Each guideline exists for a reason. Before working around one, understand why it exists.

## Quick Checklist

### Universal
- **M-LOG-STRUCTURED** — Structured logging with tracing/OTel. Redact sensitive data.
- **M-DOCUMENTED-MAGIC** — Document magic values; prefer named constants.
- **M-PANIC-ON-BUG** — Programming bugs → panic, not `Result`. Contract violations → panic.
- **M-PANIC-IS-STOP** — Panic means stop. No panics for flow control or error communication.
- **M-REGULAR-FN** — Prefer regular functions over associated functions for non-instance logic.
- **M-CONCISE-NAMES** — No weasel words: `Service`, `Manager`, `Factory`. Use `Bookings`, `BookingDispatcher`.
- **M-PUBLIC-DISPLAY** — Types meant to be read implement `Display`.
- **M-PUBLIC-DEBUG** — All public types implement `Debug`. Sensitive data → custom impl that redacts.
- **M-LINT-OVERRIDE-EXPECT** — Use `#[expect]` not `#[allow]`; add `reason`.
- **M-STATIC-VERIFICATION** — Use miri, clippy, rustfmt, cargo-audit, cargo-udeps, cargo-hack.

### API & Library Design
- **M-IMPL-IO** — Accept `impl Read`/`impl Write` for sans-IO flexibility.
- **M-IMPL-ASREF** — Accept `impl AsRef<Path>`, `impl AsRef<str>` where feasible.
- **M-SERVICES-CLONE** — Heavy services implement `Clone` (Arc-style, not fat copy).
- **M-INIT-BUILDER** — 4+ optional params → `FooBuilder` with chainable `.x()` and `.build()`.
- **M-ERRORS-CANONICAL-STRUCTS** — Errors: struct with cause, `is_xxx()` helpers. Implement `Display` + `Error`.
- **M-DI-HIERARCHY** — Prefer concrete types > generics > `dyn Trait`. Avoid `dyn` unless nesting forces it.
- **M-AVOID-WRAPPERS** — Don't expose `Rc`, `Arc`, `Box`, `RefCell` in public APIs.
- **M-TYPES-SEND** — Public types should be `Send` for Tokio/async compatibility.
- **M-STRONG-TYPES** — Use `PathBuf` not `String` for paths; proper type families.
- **M-NO-GLOB-REEXPORTS** — No `pub use foo::*`. Re-export items individually.
- **M-FEATURES-ADDITIVE** — Features are additive; any combination must work.
- **M-TEST-UTIL** — Test utilities, mocks, fake data behind `#[cfg(feature = "test-util")]`.

### Safety & Performance
- **M-UNSOUND** — Unsound code is never acceptable. Safe code must not cause UB.
- **M-UNSAFE** — `unsafe` only for: FFI, performance (after benchmark), novel abstractions.
- **M-YIELD-POINTS** — Long CPU-bound async tasks: `yield_now().await` every 10–100μs.
- **M-HOTPATH** — Profile first; benchmark with criterion/divan.
- **M-APP-ERROR** — Apps may use anyhow/eyre. Don't mix multiple app-level error crates.
- **M-MIMALLOC-APPS** — Use mimalloc as `#[global_allocator]` for apps.
- **M-DESIGN-FOR-AI** — Idiomatic APIs, thorough docs, strong types, testable APIs, good test coverage.

## Rule Categories (179 rules)

| Priority | Category | Impact | Prefix | Count |
|---|---|---|---|---|
| 1 | Ownership & Borrowing | CRITICAL | `own-` | 12 |
| 2 | Error Handling | CRITICAL | `err-` | 12 |
| 3 | Memory Optimization | CRITICAL | `mem-` | 15 |
| 4 | API Design | HIGH | `api-` | 15 |
| 5 | Async/Await | HIGH | `async-` | 15 |
| 6 | Compiler Optimization | HIGH | `opt-` | 12 |
| 7 | Naming Conventions | MEDIUM | `name-` | 16 |
| 8 | Type Safety | MEDIUM | `type-` | 10 |
| 9 | Testing | MEDIUM | `test-` | 13 |
| 10 | Documentation | MEDIUM | `doc-` | 11 |
| 11 | Performance Patterns | MEDIUM | `perf-` | 11 |
| 12 | Project Structure | LOW | `proj-` | 11 |
| 13 | Clippy & Linting | LOW | `lint-` | 11 |
| 14 | Anti-patterns | REFERENCE | `anti-` | 15 |

Load individual rules via `skill.read(name="rust-guidelines", reference="<prefix><rule>")` — e.g. `err-anyhow-app`, `async-no-lock-await`, `mem-smallvec`.

### Rule Application by Task

| Task | Primary Categories |
|---|---|
| New function | `own-`, `err-`, `name-` |
| New struct / API | `api-`, `type-`, `doc-` |
| Async code | `async-`, `own-` |
| Error handling | `err-`, `api-` |
| Memory optimization | `mem-`, `own-`, `perf-` |
| Performance tuning | `opt-`, `mem-`, `perf-` |
| Code review | `anti-`, `lint-` |
| New Rust project | `proj-`, `api-`, `err-` + see `arch-patterns` |

## Architecture Patterns

### Application Structure
- Keep `main.rs` minimal; logic in `lib.rs`. Single-responsibility crates: `_core` (pure domain), `_api` (axum), `_db` (sqlx), `_worker` (tokio tasks), `_cli` (clap).
- Workspace for large projects (>20K lines / 5+ devs). Use `[workspace.dependencies]` inheritance.
- Inject `Arc<AppState>` via Tower/axum `State` extractor. Never global mutable state.

### Ownership Strategy
- Borrow `&T` when you only read; take ownership when transforming; document why `.clone()` is needed.
- Shared read-only state: `Arc<T>`. Shared mutable: `Arc<Mutex<T>>` (std) unless holding across `.await` — then `Arc<tokio::sync::Mutex<T>>`.
- Prefer `AtomicT` for counters; `RwLock` for read-heavy maps; `mpsc`/`broadcast` channels over `Arc<Mutex<T>>`.

### Error Strategy
- Libraries: `thiserror` custom types with `#[from]` conversions. Applications: `anyhow`.
- Always add context: `.with_context(|| format!("failed to open {}", path.display()))`.
- No `.unwrap()` in production. `.expect()` only for programming invariants that must never fail.
- Error messages: lowercase, no trailing punctuation.

### Domain Modelling
- Parse, don't validate: construct valid types at system boundaries.
- Newtypes for all IDs: `UserId(Uuid)` not raw `Uuid`. Enums for mutually exclusive states.
- Never `f64` for money — use `rust_decimal::Decimal` or `i64` cents.
- `#[non_exhaustive]` on public enums/structs to allow adding variants without breaking changes.

### Async Patterns
- `tokio::select!` for racing/timeouts. `tokio::join!` for parallel. `try_join!` for fallible parallel.
- `JoinSet` for dynamic task groups. `CancellationToken` for graceful shutdown.
- `spawn_blocking` for CPU-intensive work. `tokio::fs` not `std::fs` in async contexts.

## OpenAgent Service Patterns

Conventions established across OpenAgent Rust services (`services/discord`, `services/sandbox`, etc.).

### Mutex Choice
- `std::sync::Mutex` — when lock is **not** held across `.await`
- `tokio::sync::Mutex` — only when lock must cross an `.await` point
- Never hold a `std::sync::Mutex` guard across `.await`; it deadlocks the executor thread

### Atomic Ordering for Connection Flags
- `Ordering::Acquire` on load / `Ordering::Release` on store for `connected`/`authorized` booleans
- Establishes happens-before so tool handlers see the latest gateway state

### Sync Tool Handlers Calling Async (block_in_place)
SDK tool handlers are `Fn(Value) -> anyhow::Result<String>` (sync). To call async from inside one:
```rust
// Handle::current().block_on() bridges sync handler → async.
tokio::task::block_in_place(|| Handle::current().block_on(some_async_fn()))
```
Requires `rt-multi-thread` in tokio features.

### Tokio Features — Minimal Set for Service Binaries
```toml
tokio = { version = "1", features = ["rt-multi-thread", "net", "sync", "macros"] }
```
Never use `features = ["full"]` — it pulls in test-util, io-std, and other unneeded weight.

### Graceful Shutdown Pattern
```rust
let shard_manager = Arc::clone(&client.shard_manager); // field, not method in serenity 0.12
let handle = tokio::spawn(async move { client.start().await });
server.serve(&socket_path).await;
shard_manager.shutdown_all().await;
handle.abort();
```

### Param Extraction in Tool Handlers
```rust
let channel_id = params["channel_id"]
    .as_str()
    .filter(|v| !v.is_empty())
    .ok_or_else(|| anyhow::anyhow!("channel_id is required"))?
    .to_string();
```

### Status Handler Shortcut
Don't store `started: AtomicBool`. If the handler is executing, the service is running. Hardcode `"running": true` in `status_json()`.

### Bot Message Filtering
Always filter `msg.author.bot` before emitting `message.received` events to prevent agent output loops.

## Compiler & Clippy Lints

```toml
[lints.rust]
ambiguous_negative_literals = "warn"
missing_debug_implementations = "warn"
redundant_imports = "warn"
redundant_lifetimes = "warn"
trivial_numeric_casts = "warn"
unsafe_op_in_unsafe_fn = "warn"
unused_lifetimes = "warn"

[lints.clippy]
cargo = { level = "warn", priority = -1 }
complexity = { level = "warn", priority = -1 }
correctness = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
perf = { level = "warn", priority = -1 }
style = { level = "warn", priority = -1 }
suspicious = { level = "warn", priority = -1 }
# nursery = { level = "warn", priority = -1 }  # optional

# Restriction group — consistency and quality
allow_attributes_without_reason = "warn"
clone_on_ref_ptr = "warn"
undocumented_unsafe_blocks = "warn"
map_err_ignore = "warn"
unused_result_ok = "warn"
string_to_string = "warn"
empty_drop = "warn"
empty_enum_variants_with_brackets = "warn"
renamed_function_params = "warn"

# Allow: structured logging uses literal strings with template syntax
literal_string_with_formatting_args = "allow"
```

## Release Profile

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true

[profile.bench]
inherits = "release"
debug = true
strip = false

[profile.dev.package."*"]
opt-level = 3  # Optimize dependencies in dev builds
```

## References

- [universal](references/universal.md) — Universal guidelines: logging, panic, names, static verification
- [libs](references/libs.md) — Library interop, UX, resilience, building
- [apps-safety-perf](references/apps-safety-perf.md) — Applications, FFI, safety, performance
- [docs-ai](references/docs-ai.md) — Documentation and AI-friendly design
- [arch-patterns](references/arch-patterns.md) — Architecture guardrails, NEVER/ALWAYS lists, workspace decision matrix, domain-specific adaptations

Individual rule references (160+ files — load by `reference="<prefix>-<name>"`):
- `own-*` — Ownership & borrowing: borrow-over-clone, arc-shared, mutex-interior, cow-conditional, lifetime-elision …
- `err-*` — Error handling: thiserror-lib, anyhow-app, context-chain, no-unwrap-prod, question-mark …
- `mem-*` — Memory: with-capacity, smallvec, arrayvec, zero-copy, compact-string, box-large-variant …
- `api-*` — API design: builder-pattern, newtype-safety, typestate, sealed-trait, impl-into, parse-dont-validate …
- `async-*` — Async: no-lock-await, spawn-blocking, cancellation-token, join-parallel, joinset-structured …
- `opt-*` — Compiler optimization: inline-small, lto-release, codegen-units, pgo-profile, simd-portable …
- `name-*` — Naming: types-camel, funcs-snake, as-free, to-expensive, into-ownership, no-get-prefix …
- `type-*` — Type safety: newtype-ids, enum-states, phantom-marker, never-diverge, repr-transparent …
- `test-*` — Testing: cfg-test-module, proptest-properties, mockall-mocking, tokio-async, criterion-bench …
- `doc-*` — Documentation: all-public, module-inner, examples-section, errors-section, intra-links …
- `perf-*` — Performance: iter-over-index, entry-api, drain-reuse, collect-once, black-box-bench …
- `proj-*` — Project structure: lib-main-split, mod-by-feature, workspace-large, workspace-deps …
- `lint-*` — Linting: deny-correctness, warn-suspicious, warn-style, workspace-lints, rustfmt-check …
- `anti-*` — Anti-patterns: unwrap-abuse, lock-across-await, over-abstraction, string-for-str, type-erasure …

Full source: https://microsoft.github.io/rust-guidelines/ · https://rust-lang.github.io/api-guidelines/ · https://nnethercote.github.io/perf-book/
