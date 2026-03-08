# TTS Service — Kokoros (Kokoro TTS in Rust)

MCP-lite Rust service for text-to-speech using [Kokoros](https://github.com/lucasjinreal/Kokoros) — Kokoro-82M in Rust. Local, fast, no API key. Communicates over `tts.sock` with the Python control plane.

## Features

- **Local inference** — No external API calls, runs fully offline
- **Kokoro-82M** — 82M-parameter ONNX model, high-quality neural TTS
- **Voice blending** — Mix any two voices: `af_sarah.4+af_nicole.6`
- **Dual output** — WAV file (`tts.synthesize`) or base64 PCM (`tts.synthesize_bytes`)
- **MCP-lite** — Same protocol as sandbox, memory, and browser services

## Memory footprint (warmed)

Kokoro-82M loaded in ONNX Runtime on CPU:

| Component | Size |
|-----------|------|
| ONNX model weights (`kokoro-v1.0.onnx`) | ~310 MB on disk |
| Voice embeddings (`voices-v1.0.bin`) | ~5 MB |
| ONNX Runtime session + buffers | ~80–120 MB overhead |
| **Total RSS (warm, idle)** | **~400–450 MB** |

During inference, temporary activation buffers add ~50–100 MB (freed after each call).

On Raspberry Pi 4 (4 GB): tight but workable — plan for ~450 MB committed RSS. On Pi 5 (8 GB): comfortable. Consider not warming on startup and instead initialising on first call if memory is constrained.

## Prerequisites

- **Rust** 1.70+
- **Opus** (required by Kokoros audio layer):
  ```bash
  brew install pkg-config opus    # macOS
  sudo apt install pkg-config libopus-dev   # Linux / Pi
  ```

> **macOS build note:** Requires opus installed via brew (above). Without it the build falls back to compiling opus from source via cmake, which fails on cmake 4.x due to an old `cmake_minimum_required` in the vendored source. A `.cargo/config.toml` pins `MACOSX_DEPLOYMENT_TARGET=14.0` to avoid SDK version mismatches.

## Model Download

Models are stored in `data/models/` (shared across all services that use local models).

```bash
mkdir -p data/models data/artifacts/tts

# Kokoro ONNX model (~310 MB)
curl -L "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX-timestamped/resolve/main/onnx/model.onnx" \
  -o data/models/kokoro-v1.0.onnx

# Voice embeddings (~5 MB)
curl -L "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin" \
  -o data/models/voices-v1.0.bin
```

Or as a one-liner script (run from project root):

```bash
mkdir -p data/models data/artifacts/tts && \
  curl -L "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX-timestamped/resolve/main/onnx/model.onnx" -o data/models/kokoro-v1.0.onnx && \
  curl -L "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin" -o data/models/voices-v1.0.bin
```

## Build

```bash
# From project root
make tts        # cross-compile all targets
make local      # build for current host only (faster dev loop)

# Or directly
cd services/tts
cargo build --release
```

Binaries are placed in `bin/` (gitignored): `bin/tts-darwin-arm64`, `bin/tts-linux-arm64`, etc.

## Run

The service is managed by OpenAgent's **ServiceManager** — it starts automatically on agent launch.

Standalone (for testing):

```bash
OPENAGENT_SOCKET_PATH=data/sockets/tts.sock \
  ./bin/tts-darwin-arm64
```

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENAGENT_SOCKET_PATH` | `data/sockets/tts.sock` | Unix socket path |
| `OPENAGENT_TTS_MODEL` | `data/models/kokoro-v1.0.onnx` | Kokoro ONNX model path |
| `OPENAGENT_TTS_VOICES` | `data/models/voices-v1.0.bin` | Voice embeddings path |
| `OPENAGENT_ARTIFACTS_DIR` | `data/artifacts/tts` | WAV output directory |
| `OPENAGENT_LOGS_DIR` | `logs` | OTLP trace file directory |

## Tools

### `tts.synthesize`

Synthesize speech and write WAV to `data/artifacts/tts/<uuid>.wav`.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | yes | Text to speak |
| `voice` | string | no | Voice style (default: `af_sarah.4+af_nicole.6`) |
| `speed` | number | no | Speech rate multiplier (default: `1.0`) |
| `language` | string | no | Language code (default: `en-us`) |

Returns:
```json
{"path": "data/artifacts/tts/<uuid>.wav", "sample_rate": 24000, "format": "wav"}
```

### `tts.synthesize_bytes`

Synthesize to base64-encoded f32 little-endian PCM. For in-memory or streaming playback without writing a file.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `text` | string | yes | Text to speak |
| `voice` | string | no | Voice style |
| `speed` | number | no | Speech rate multiplier |
| `language` | string | no | Language code |

Returns:
```json
{"audio_base64": "...", "sample_rate": 24000, "format": "f32_le", "channels": 1}
```

## Voice styles

Kokoro supports per-inference style blending:

| Voice | Description |
|-------|-------------|
| `af_sarah` | American female |
| `af_nicole` | American female |
| `af_sky` | American female (ASMR-style) |
| `af_sarah.4+af_nicole.6` | 40% sarah, 60% nicole blend (default) |

## Python integration

```yaml
# config/openagent.yaml
tts:
  provider: kokoro
  voice: af_sarah.4+af_nicole.6
  speed: 1.0
  language: en-us
```

Or via environment:
```bash
export OPENAGENT_TTS_PROVIDER=kokoro
```

The `KokoroProvider` in `extensions/tts/` connects to `tts.sock` and calls `tts.synthesize_bytes` for full text, or sentence-by-sentence for chunked playback.

## References

- [Kokoros](https://github.com/lucasjinreal/Kokoros) — Kokoro TTS in Rust
- [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) — Hugging Face model card
- [kokoro-onnx](https://github.com/thewh1teagle/kokoro-onnx) — ONNX voices source
