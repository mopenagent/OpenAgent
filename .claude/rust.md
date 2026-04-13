# Rust Standards — OpenAgent

## Service Crate Setup

Each service is a standalone crate (`services/<name>/Cargo.toml`). Shared deps:
- `sdk-rust = { path = "../sdk-rust" }` — MCP-lite server boilerplate
- `tokio = { features = ["rt-multi-thread","macros","sync","net"] }` — never `"full"`
- `mimalloc` as `#[global_allocator]`

Release profile (all service crates):
```toml
[profile.release]
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

Cross-compile targets: `aarch64-apple-darwin` (native Mac), `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl` (via `cross`).

## Patterns — Always Apply

### 1. Typestate — encode lifecycle in the type system

Invalid state transitions must be compiler errors, not runtime panics.

```rust
struct Ready;
struct Charging;
struct Actuator<S> { _state: std::marker::PhantomData<S> }

impl Actuator<Ready> {
    pub fn move_to(&self, x: f32, y: f32, z: f32) -> Result<(), ActuatorError> { ... }
}
// Actuator<Charging>::move_to does not exist — compiler rejects it.
```

Use wherever a resource has a lifecycle (connections, hardware sessions, agent states).

### 2. Trait-Based Tooling — extensibility without hardcoding

```rust
pub trait RobotActuator {
    fn move_to(&self, x: f32, y: f32, z: f32) -> Result<(), RobotError>;
}
```

Apply to all service integrations, hardware drivers, and pluggable components. Enables test mocks without changing call sites.

### 3. Actor Model — concurrency via bounded channels

Each independent concern is an actor communicating via `tokio::sync::mpsc`. No shared mutable state between actors.

```rust
let (tx, rx) = mpsc::channel(256); // always set capacity
```

Never use `unbounded_channel` — silent memory leak when producer outpaces consumer.

## Anti-Patterns — Never Do These

| Anti-pattern | Why banned | What to do instead |
|---|---|---|
| `unwrap()` / `expect()` in non-test code | Hard crash, no recovery or error propagation | `Result<_, E>` with `thiserror`-derived error types |
| `static mut` | Undefined behaviour under concurrent access | `Arc<Mutex<T>>`, `Arc<RwLock<T>>`, or `Atomic*` |
| Blocking inside `async fn` | Starves tokio executor — all tasks on the thread freeze | `tokio::task::spawn_blocking` for CPU-heavy or blocking I/O |
| `unbounded_channel` | Fast producer + slow consumer = unbounded memory growth | `mpsc::channel(N)` or `broadcast::channel(N)` |
| `clone()` on large buffers in hot paths | Data copy on every iteration | Pass `Arc<T>` or `&T`; clone only at ownership boundaries |

## Tower / Axum Conventions (openagent binary only)

Tower/Axum lives in `openagent/` — **never** add `axum` or `tower` to any service crate.

**Layer order (outermost → innermost):**
```
ConcurrencyLimitLayer (max 50)
→ HandleErrorLayer
→ TimeoutLayer
→ TraceLayer
→ CorsLayer
→ GuardLayer
→ SttLayer
→ TtsLayer
→ Router
```

- Use `tower::ServiceBuilder` to compose timeout + error handling
- Use `axum::middleware::from_fn_with_state` for stateful layers
- Axum is external-facing only — speaks JSON on :8080 to platform connectors and web UI
- Axum never replaces MCP-lite between `openagent` and services

## sdk-rust Exports

Key exports from `services/sdk-rust/src/`:
- `McpLiteServer` + `serve_auto(default_addr)` — reads `OPENAGENT_TCP_ADDRESS` env var
- `setup_otel(service_name, logs_dir)` → `OTELGuard` (hold for process lifetime)
- `MetricsWriter` — daily JSONL metrics, `Arc` inside, Clone-cheap
- `attach_context(params, baggage_kvs)` — OTEL context propagation from MCP-lite params
- `ts_ms()` / `elapsed_ms(Instant)` — timestamp helpers

OTEL guard pattern — hold guard for process lifetime:
```rust
let _otel_guard = setup_otel("my-service", &logs_dir)
    .inspect_err(|e| eprintln!("otel init failed: {e}"))
    .ok();
// NOT: if let Err(e) = setup_otel(...) — drops guard immediately
```
