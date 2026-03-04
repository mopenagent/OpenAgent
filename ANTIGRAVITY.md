# Antigravity Context: OpenAgent Architecture

This file serves as a durable record of the architectural consensus reached regarding OpenAgent, specifically focusing on the deployment realities for Raspberry Pi and the hybrid Python/Go split.

## 1. The Workflow Engine Paradigm
OpenAgent is distinct from typical agentic loops because the LLM is **not** the center of the universe.
- Python acts as a deterministic **Workflow Orchestrator**.
- The LLM is just one non-deterministic node in the graph.
- A deterministic chain (e.g. `WhatsApp IN -> STT -> LLM -> TTS -> WhatsApp OUT`) is preferred over giving the LLM raw tools and asking it to figure out what to do. This saves massive amounts of tokens, latency, and context window logic on the 14B model.

## 2. Zero-Copy Artifact Routing
IPC (Inter-Process Communication) JSON serialization is a massive tax on low-power hardware. 
- **Rule:** Never send binary data, large text blobs, or heavy arrays over the MCP-lite Unix Sockets.
- **Solution:** Go Services write data directly to the shared filesystem (`data/artifacts/`). They emit a tiny JSON payload containing only the pointer/path (`{"path": "/data/artifacts/audio_123.ogg"}`).
- Python receives this path and passes it as an argument to the next service in the workflow. 

## 3. No East-West Mesh
Go services **never** talk to each other directly. 
- All routing goes strictly through the Python Control Plane.
- This prevents microservice spaghetti and ensures Python remains the absolute Source of Truth for the state of an agent's workflow.

## 4. Hardware Realism (The Pi-First Mindset)
The platform is designed to run entirely on a single 8GB Raspberry Pi (with the LLM/Vector DB potentially running on a dedicated local API/GPU).
- **Vector DB (LanceDB):** Uses a direct Python client wrapper to leverage LanceDB's fast, native Rust core. We do *not* isolate this into a Go service initially, as that would introduce the JSON IPC serialization tax for massive vector arrays. We only shift it if profiling shows it aggressively blocking the `asyncio` event loop.
- The philosophy is: Build for a single node, monitor, profile, and optimize. Only distribute if absolutely necessary.
