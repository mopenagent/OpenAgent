# OpenAgent — build targets
#
# Usage:
#   make                    # cross-compile all services (Go + Rust)
#   make local              # build all services for the current host platform only
#   make <service>          # cross-compile one service by name
#   make test-go            # run Go tests for all Go services + sdk-go
#   make test-rust          # run Rust tests for sdk-rust + all Rust services
#   make test-py            # run Python tests (openagent/ + app/)
#   make clean              # remove all compiled binaries from bin/
#
# All binaries land in <project-root>/bin/ — one clean directory, gitignored.
# service.json binary paths are relative to the project root (e.g. bin/stt-darwin-arm64).
#
# Prerequisites:
#   Go services    : go 1.21+
#   Rust services  : rustup (1.75+)
#   Cross-compile  : `cargo install cross --locked` + Docker (for linux targets)
#                    Darwin (arm64 + amd64) builds natively on Mac without Docker.
#
# Model files (download once into data/models/):
#   TTS (Kokoro-82M):
#     curl -L -o data/models/kokoro-v1.0.onnx  <kokoros-release-url>
#     curl -L -o data/models/voices-v1.0.bin    <kokoros-release-url>
#   STT (Whisper small):
#     curl -L -o data/models/whisper-ggml-small.bin \
#       https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin
#     (HF serves file as ggml-small.bin; -o renames it locally)

# ---------------------------------------------------------------------------
# Service lists
# ---------------------------------------------------------------------------

GO_SERVICES   := whatsapp
RUST_SERVICES := sandbox browser memory tts stt validator cortex channels guard research
# openagent is the Rust control plane binary — built separately (not a service)
OPENAGENT_DIR := openagent

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)

ifeq ($(UNAME_S),Darwin)
  HOST_OS := darwin
  # Pin SDKROOT so C dependencies (lz4_sys, zstd-sys, etc.) always link
  # against the currently installed Xcode SDK — prevents clang_rt version mismatches.
  export SDKROOT := $(shell xcrun --sdk macosx --show-sdk-path 2>/dev/null)
else
  HOST_OS := linux
endif

ifeq ($(UNAME_M),arm64)
  HOST_ARCH := arm64
else ifeq ($(UNAME_M),aarch64)
  HOST_ARCH := arm64
else
  HOST_ARCH := amd64
endif

HOST_SUFFIX := $(HOST_OS)-$(HOST_ARCH)

# ---------------------------------------------------------------------------
# TTS darwin exclusion
#
# espeak-rs-sys (bundled by kokoros) builds espeak-ng from source via cmake.
# The espeak-ng phoneme compiler has a bug that truncates filenames when the
# build directory sits on a case-insensitive filesystem (macOS HFS+/APFS).
# There is no env-var bypass — the build script always runs cmake from source.
#
# Workaround: skip the native darwin binary for tts and rely on `cross`
# (Docker, Linux targets) for deployable binaries.  Rust + kokoros runtime
# itself works fine on macOS once the binary is cross-compiled.
# ---------------------------------------------------------------------------
TTS_SKIP_DARWIN := true

BIN := bin

.PHONY: all local clean test-go test-rust test-py download-models help openagent openagent-local \
        $(GO_SERVICES) $(RUST_SERVICES)

# Default: cross-compile everything (services + openagent control plane)
all: $(GO_SERVICES) $(RUST_SERVICES) openagent

# ---------------------------------------------------------------------------
# Go services — cross-compile to all targets
# ---------------------------------------------------------------------------

define build_go_service
$(1):
	@echo "==> services/$(1) (Go)"
	@mkdir -p $(BIN)
	cd services/$(1) && GOOS=linux  GOARCH=arm64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-linux-arm64  .
	cd services/$(1) && GOOS=linux  GOARCH=amd64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-linux-amd64  .
	cd services/$(1) && GOOS=darwin GOARCH=arm64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-darwin-arm64 .
	cd services/$(1) && GOOS=darwin GOARCH=amd64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-darwin-amd64 .
	@echo "   ✓ $(BIN)/$(1)-{linux,darwin}-{arm64,amd64}"
endef

$(foreach svc,$(GO_SERVICES),$(eval $(call build_go_service,$(svc))))

# ---------------------------------------------------------------------------
# Rust services — native Darwin build + cross via `cross` for Linux
# ---------------------------------------------------------------------------

define build_rust_service
$(1):
	@echo "==> services/$(1) (Rust)"
	@mkdir -p $(BIN)
ifeq ($(HOST_OS),darwin)
ifeq ($(1),tts)
	@echo "   Skipping darwin native build for tts (espeak-rs-sys cmake fails on HFS+/APFS)."
	@echo "   Deploy via linux cross targets below, or build on Linux/Docker."
else
	cd services/$(1) && cargo build --release --target aarch64-apple-darwin
	cp services/$(1)/target/aarch64-apple-darwin/release/$(1) \
	   $(BIN)/$(1)-darwin-arm64
	@if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then \
	  cd services/$(1) && cargo build --release --target x86_64-apple-darwin && \
	  cp services/$(1)/target/x86_64-apple-darwin/release/$(1) \
	     $(BIN)/$(1)-darwin-amd64; \
	else \
	  echo "   Skipping darwin/amd64 (run: rustup target add x86_64-apple-darwin)"; \
	fi
endif
else
	cd services/$(1) && cargo build --release
	cp services/$(1)/target/release/$(1) $(BIN)/$(1)-$(HOST_SUFFIX)
endif
	@if command -v cross >/dev/null 2>&1; then \
	  cd services/$(1) && cross build --release --target aarch64-unknown-linux-musl && \
	  cp services/$(1)/target/aarch64-unknown-linux-musl/release/$(1) \
	     $(BIN)/$(1)-linux-arm64; \
	  cd services/$(1) && cross build --release --target x86_64-unknown-linux-musl && \
	  cp services/$(1)/target/x86_64-unknown-linux-musl/release/$(1) \
	     $(BIN)/$(1)-linux-amd64; \
	else \
	  echo "   Skipping linux targets (install: cargo install cross --locked)"; \
	fi
	@echo "   ✓ $(1) binaries in $(BIN)/"
endef

$(foreach svc,$(RUST_SERVICES),$(eval $(call build_rust_service,$(svc))))

# ---------------------------------------------------------------------------
# Local build — current host platform only (fastest dev loop)
# ---------------------------------------------------------------------------

local: openagent-local
	@echo "==> Building for $(HOST_OS)/$(HOST_ARCH)"
	@mkdir -p $(BIN)
	@echo "--- Go services ---"
	@for svc in $(GO_SERVICES); do \
	  echo "  → $$svc"; \
	  (cd services/$$svc && \
	    GOOS=$(HOST_OS) GOARCH=$(HOST_ARCH) go build -ldflags="-s -w" \
	    -o ../../$(BIN)/$$svc-$(HOST_SUFFIX) .) || exit 1; \
	done
	@echo "--- Rust services ---"
ifeq ($(HOST_OS),darwin)
	@for svc in $(filter-out tts,$(RUST_SERVICES)); do \
	  echo "  → $$svc"; \
	  (cd services/$$svc && \
	    cargo build --release --target aarch64-apple-darwin && \
	    cp target/aarch64-apple-darwin/release/$$svc \
	       ../../$(BIN)/$$svc-$(HOST_SUFFIX)) || exit 1; \
	done
	@echo "  → tts (skipped on macOS — espeak-rs-sys cmake fails on HFS+/APFS; use cross for linux targets)"
else
	@for svc in $(RUST_SERVICES); do \
	  echo "  → $$svc"; \
	  (cd services/$$svc && \
	    cargo build --release && \
	    cp target/release/$$svc \
	       ../../$(BIN)/$$svc-$(HOST_SUFFIX)) || exit 1; \
	done
endif
	@echo "Done — binaries in $(BIN)/*-$(HOST_SUFFIX)"
	@if [ ! -d "data/models/models--Xenova--bge-small-en-v1.5" ]; then \
	  echo ""; \
	  echo "  NOTE: embedding model not cached."; \
	  echo "  Run 'make download-models' before starting the memory service,"; \
	  echo "  or it will download on first start (~70 MB, may take a while)."; \
	  echo ""; \
	fi

# ---------------------------------------------------------------------------
# Model cache check — prints status and download command if model is missing
# ---------------------------------------------------------------------------

MEMORY_BIN := $(BIN)/memory-$(HOST_SUFFIX)
EMBED_MODEL_MARKER := data/models/models--Xenova--bge-small-en-v1.5

.PHONY: download-models
download-models:
	@echo "==> Embedding model cache check (BAAI/bge-small-en-v1.5)"
	@if [ -d "$(EMBED_MODEL_MARKER)" ]; then \
	  echo "   ✓ Model cached at $(EMBED_MODEL_MARKER)"; \
	else \
	  echo "   ✗ Model not found. To download (~70 MB), run:"; \
	  echo ""; \
	  echo "     OPENAGENT_DOWNLOAD_ONLY=1 OPENAGENT_EMBED_CACHE_PATH=data/models $(MEMORY_BIN)"; \
	  echo ""; \
	  echo "   (build the memory binary first if needed: make memory)"; \
	fi

# ---------------------------------------------------------------------------
# OpenAgent Rust control plane binary
# ---------------------------------------------------------------------------

# openagent-local: build for the current host only (fastest dev loop)
openagent-local:
	@echo "==> openagent (Rust control plane, $(HOST_OS)/$(HOST_ARCH))"
	@mkdir -p $(BIN)
ifeq ($(HOST_OS),darwin)
	cd $(OPENAGENT_DIR) && cargo build --release --target aarch64-apple-darwin
	cp $(OPENAGENT_DIR)/target/aarch64-apple-darwin/release/openagent \
	   $(BIN)/openagent-$(HOST_SUFFIX)
else
	cd $(OPENAGENT_DIR) && cargo build --release
	cp $(OPENAGENT_DIR)/target/release/openagent $(BIN)/openagent-$(HOST_SUFFIX)
endif
	@echo "   ✓ $(BIN)/openagent-$(HOST_SUFFIX)"

# openagent: cross-compile (darwin native + linux via cross)
openagent:
	@echo "==> openagent (Rust control plane, all targets)"
	@mkdir -p $(BIN)
ifeq ($(HOST_OS),darwin)
	cd $(OPENAGENT_DIR) && cargo build --release --target aarch64-apple-darwin
	cp $(OPENAGENT_DIR)/target/aarch64-apple-darwin/release/openagent \
	   $(BIN)/openagent-darwin-arm64
	@if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then \
	  cd $(OPENAGENT_DIR) && cargo build --release --target x86_64-apple-darwin && \
	  cp $(OPENAGENT_DIR)/target/x86_64-apple-darwin/release/openagent \
	     $(BIN)/openagent-darwin-amd64; \
	else \
	  echo "   Skipping darwin/amd64 (run: rustup target add x86_64-apple-darwin)"; \
	fi
else
	cd $(OPENAGENT_DIR) && cargo build --release
	cp $(OPENAGENT_DIR)/target/release/openagent $(BIN)/openagent-$(HOST_SUFFIX)
endif
	@if command -v cross >/dev/null 2>&1; then \
	  cd $(OPENAGENT_DIR) && cross build --release --target aarch64-unknown-linux-musl && \
	  cp $(OPENAGENT_DIR)/target/aarch64-unknown-linux-musl/release/openagent \
	     $(BIN)/openagent-linux-arm64; \
	  cd $(OPENAGENT_DIR) && cross build --release --target x86_64-unknown-linux-musl && \
	  cp $(OPENAGENT_DIR)/target/x86_64-unknown-linux-musl/release/openagent \
	     $(BIN)/openagent-linux-amd64; \
	else \
	  echo "   Skipping linux targets (install: cargo install cross --locked)"; \
	fi
	@echo "   ✓ openagent binaries in $(BIN)/"

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

test-go:
	@for pkg in sdk-go $(GO_SERVICES); do \
	  echo "→ testing services/$$pkg ..."; \
	  (cd services/$$pkg && go test ./...) || exit 1; \
	done

test-rust:
	@for svc in sdk-rust $(RUST_SERVICES); do \
	  echo "→ testing services/$$svc ..."; \
	  (cd services/$$svc && cargo test) || exit 1; \
	done

test-py:
	python -m pytest openagent/ app/ -q

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

clean:
	rm -f $(BIN)/*
	@echo "  cleaned $(BIN)/"

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------

help:
	@echo ""
	@echo "OpenAgent build targets"
	@echo "  make              Cross-compile all services"
	@echo "  make local        Build for current host: $(HOST_OS)/$(HOST_ARCH)"
	@echo ""
	@echo "  Go  ($(GO_SERVICES)):"
	@echo "    make <name>     → linux/arm64 linux/amd64 darwin/arm64 darwin/amd64"
	@echo ""
	@echo "  Rust ($(RUST_SERVICES)):"
	@echo "    make <name>     → darwin natively; linux via cross"
	@echo "    Darwin/amd64:   rustup target add x86_64-apple-darwin"
	@echo "    Linux:          cargo install cross --locked  (requires Docker)"
	@echo ""
	@echo "  make test-go      Run Go unit tests"
	@echo "  make test-rust    Run Rust unit tests"
	@echo "  make test-py      Run Python tests"
	@echo "  make clean        Remove all binaries from $(BIN)/"
	@echo ""
	@echo "  Control plane:"
	@echo "  make openagent-local   Build openagent for current host (fast)"
	@echo "  make openagent         Cross-compile openagent for all targets"
	@echo ""
	@echo "  make download-models  Check embedding model cache; prints download command if missing"
	@echo ""
	@echo "  Binaries:   $(BIN)/"
	@echo "  Models:     data/models/whisper-ggml-small.bin  kokoro-v1.0.onnx  voices-v1.0.bin"
	@echo "              data/models/models--Xenova--bge-small-en-v1.5  (embedding, via download-models)"
	@echo "  Sandbox:    msb server start --dev"
	@echo "  TTS/macOS:  native darwin build skipped (espeak-rs-sys cmake HFS+ bug)."
	@echo "              Linux targets still built via cross (Docker).  Deploy to Pi directly."
	@echo ""
