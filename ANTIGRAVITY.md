# Antigravity Context: OpenAgent Architecture

This file serves as a durable record of the architectural consensus reached regarding OpenAgent, specifically focusing on the deployment realities for Raspberry Pi and the hybrid Python/Rust/Go split (Rust-first; only WhatsApp remains in Go).

## 1. Communication Protocol (User Preference)
Whenever the user sends an input where their intention needs clarification or the context needs expansion, **do not assume the correct path.** 
- Ask clarifying questions **one by one** (1-by-1).
- Provide possible **options/paths** for the user to choose from.
- Record and apply this explicitly in every conversation regarding architecture and implementation.

## 2. The Workflow Engine Paradigm
OpenAgent is distinct from typical agentic loops because the LLM is **not** the center of the universe.
- Python acts as a deterministic **Workflow Orchestrator**.
- The LLM is just one non-deterministic node in the graph.
- A deterministic chain (e.g. `WhatsApp IN -> STT -> LLM -> TTS -> WhatsApp OUT`) is preferred over giving the LLM raw tools and asking it to figure out what to do. This saves massive amounts of tokens, latency, and context window logic on the 14B model.

## 3. Zero-Copy Artifact Routing
IPC (Inter-Process Communication) JSON serialization is a massive tax on low-power hardware. 
- **Rule:** Never send binary data, large text blobs, or heavy arrays over the MCP-lite Unix Sockets.
- **Solution:** Rust/Go services write data directly to the shared filesystem (`data/artifacts/`). They emit a tiny JSON payload containing only the pointer/path (`{"path": "/data/artifacts/audio_123.ogg"}`).
- Python receives this path and passes it as an argument to the next service in the workflow. 

## 4. No East-West Mesh
Rust and Go services **never** talk to each other directly. 
- All routing goes strictly through the Python Control Plane.
- This prevents microservice spaghetti and ensures Python remains the absolute Source of Truth for the state of an agent's workflow.

## 5. Hardware Realism (The Pi-First Mindset)
The platform is designed to run entirely on a single 8GB Raspberry Pi (with the LLM/Vector DB potentially running on a dedicated local API/GPU).
- **Vector DB (LanceDB):** Uses a direct Python client wrapper to leverage LanceDB's fast, native Rust core. We do *not* isolate this into a Rust service initially, as that would introduce the JSON IPC serialization tax for massive vector arrays. We only shift it if profiling shows it aggressively blocking the `asyncio` event loop.
- The philosophy is: Build for a single node, monitor, profile, and optimize. Only distribute if absolutely necessary.

## 6. Granular Edge Observability
- **Modular Data Plane:** All Rust service logic (the Hands) is decoupled into strictly separated `handlers`, `tools`, `state`, and `metrics` modules.
- **Native Telemetry:** Telemetry is explicitly wired up through the overarching `sdk-rust/otel.rs` implementation natively emitting granular tracing spans for all execution paths.
- **Trace Ingestion:** We embrace Jaeger UI to ingest these traces, giving the Orchestrator high visibility into sub-component latencies and success metrics without the overhead of heavy polling solutions.
