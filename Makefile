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

GO_SERVICES   := telegram slack whatsapp
RUST_SERVICES := sandbox browser memory tts stt discord

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)

ifeq ($(UNAME_S),Darwin)
  HOST_OS := darwin
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

BIN := bin

.PHONY: all local clean test-go test-rust test-py help \
        $(GO_SERVICES) $(RUST_SERVICES)

# Default: cross-compile everything
all: $(GO_SERVICES) $(RUST_SERVICES)

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

local:
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
	@for svc in $(RUST_SERVICES); do \
	  echo "  → $$svc"; \
	  (cd services/$$svc && \
	    cargo build --release --target aarch64-apple-darwin && \
	    cp target/aarch64-apple-darwin/release/$$svc \
	       ../../$(BIN)/$$svc-$(HOST_SUFFIX)) || exit 1; \
	done
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
	@echo "  Binaries:   $(BIN)/"
	@echo "  Models:     data/models/whisper-ggml-small.bin  kokoro-v1.0.onnx  voices-v1.0.bin"
	@echo "  Sandbox:    msb server start --dev"
	@echo ""
