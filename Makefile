# OpenAgent — build targets
#
# Usage:
#   make                    # cross-compile all Go services + Rust sandbox + browser
#   make local              # build all services for your current host platform only
#   make <service>          # cross-compile a single Go service
#   make sandbox            # cross-compile the Rust sandbox service
#   make browser            # build the Rust browser service
#   make test-go            # run Go tests for all Go services + sdk-go
#   make test-rust          # run Rust tests for sdk-rust + sandbox
#   make test-py            # run Python tests (openagent/ + app/)
#   make clean              # remove all compiled binaries
#
# All binaries are placed in <project-root>/bin/ so the services/ tree stays source-only.
#
# Prerequisites:
#   Go services   : go 1.21+
#   Rust services : rustup (1.75+)
#                   Cross-compilation uses `cross` (cargo install cross --locked)
#                   and requires Docker.  Darwin (arm64 and amd64) builds natively on Mac.

GO_SERVICES := discord telegram slack whatsapp

PLATFORMS := linux/arm64 linux/amd64 darwin/arm64 darwin/amd64

# All compiled binaries land here — one clean directory, gitignored.
BIN := bin

# Detect host platform for `make local`
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

.PHONY: all local clean test-go test-rust test-py help sandbox browser $(GO_SERVICES)

# Default: cross-compile everything
all: $(GO_SERVICES) sandbox browser

# ---------------------------------------------------------------------------
# Go services — per-service cross-compile rule
# ---------------------------------------------------------------------------

define build_service
$(1):
	@echo "==> services/$(1)"
	@mkdir -p $(BIN)
	cd services/$(1) && GOOS=linux  GOARCH=arm64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-linux-arm64  .
	cd services/$(1) && GOOS=linux  GOARCH=amd64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-linux-amd64  .
	cd services/$(1) && GOOS=darwin GOARCH=arm64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-darwin-arm64 .
	cd services/$(1) && GOOS=darwin GOARCH=amd64 go build -ldflags="-s -w" -o ../../$(BIN)/$(1)-darwin-amd64 .
	@echo "   ✓ $(BIN)/$(1)-{linux,darwin}-{arm64,amd64}"
endef

$(foreach svc,$(GO_SERVICES),$(eval $(call build_service,$(svc))))

# ---------------------------------------------------------------------------
# Rust sandbox — cross-compile via `cross`
# ---------------------------------------------------------------------------

sandbox:
	@echo "==> services/sandbox (Rust)"
	@mkdir -p $(BIN)
ifeq ($(HOST_OS),darwin)
	cd services/sandbox && cargo build --release --target aarch64-apple-darwin
	cp services/sandbox/target/aarch64-apple-darwin/release/sandbox \
	   $(BIN)/sandbox-darwin-arm64
	@if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then \
	  cd services/sandbox && cargo build --release --target x86_64-apple-darwin && \
	  cp services/sandbox/target/x86_64-apple-darwin/release/sandbox \
	     $(BIN)/sandbox-darwin-amd64; \
	else \
	  echo "   Skipping darwin/amd64 (run: rustup target add x86_64-apple-darwin)"; \
	fi
endif
	@if command -v cross >/dev/null 2>&1; then \
	  cd services/sandbox && cross build --release --target aarch64-unknown-linux-musl && \
	  cp services/sandbox/target/aarch64-unknown-linux-musl/release/sandbox \
	     $(BIN)/sandbox-linux-arm64 && \
	  cd services/sandbox && cross build --release --target x86_64-unknown-linux-musl && \
	  cp services/sandbox/target/x86_64-unknown-linux-musl/release/sandbox \
	     $(BIN)/sandbox-linux-amd64; \
	else \
	  echo "   Skipping linux targets (install: cargo install cross --locked)"; \
	fi
	@echo "   ✓ sandbox binaries in $(BIN)/"

# ---------------------------------------------------------------------------
# Rust browser — headless browser automation service
# ---------------------------------------------------------------------------

browser:
	@echo "==> services/browser (Rust)"
	@mkdir -p $(BIN)
ifeq ($(HOST_OS),darwin)
	cd services/browser && cargo build --release --target aarch64-apple-darwin
	cp services/browser/target/aarch64-apple-darwin/release/browser \
	   $(BIN)/browser-darwin-arm64
	@if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then \
	  cd services/browser && cargo build --release --target x86_64-apple-darwin && \
	  cp services/browser/target/x86_64-apple-darwin/release/browser \
	     $(BIN)/browser-darwin-amd64; \
	else \
	  echo "   Skipping darwin/amd64 (run: rustup target add x86_64-apple-darwin)"; \
	fi
endif
	@if command -v cross >/dev/null 2>&1; then \
	  cd services/browser && cross build --release --target aarch64-unknown-linux-musl && \
	  cp services/browser/target/aarch64-unknown-linux-musl/release/browser \
	     $(BIN)/browser-linux-arm64 && \
	  cd services/browser && cross build --release --target x86_64-unknown-linux-musl && \
	  cp services/browser/target/x86_64-unknown-linux-musl/release/browser \
	     $(BIN)/browser-linux-amd64; \
	else \
	  echo "   Skipping linux targets (install: cargo install cross --locked)"; \
	fi
	@echo "   ✓ browser binaries in $(BIN)/"

# ---------------------------------------------------------------------------
# Build only for the current host (faster dev loop)
# ---------------------------------------------------------------------------

local:
	@echo "==> Building Go services for $(HOST_OS)/$(HOST_ARCH)"
	@mkdir -p $(BIN)
	@for svc in $(GO_SERVICES); do \
	  echo "  → $$svc"; \
	  cd services/$$svc && \
	    GOOS=$(HOST_OS) GOARCH=$(HOST_ARCH) go build -ldflags="-s -w" -o ../../$(BIN)/$$svc-$(HOST_SUFFIX) . && \
	    cd ../..; \
	done
	@echo "==> Building sandbox (Rust) for $(HOST_OS)/$(HOST_ARCH)"
ifeq ($(HOST_OS),darwin)
	cd services/sandbox && cargo build --release --target aarch64-apple-darwin
	cp services/sandbox/target/aarch64-apple-darwin/release/sandbox \
	   $(BIN)/sandbox-$(HOST_SUFFIX)
else
	cd services/sandbox && cargo build --release
	cp services/sandbox/target/release/sandbox \
	   $(BIN)/sandbox-$(HOST_SUFFIX)
endif
	@echo "==> Building browser (Rust) for $(HOST_OS)/$(HOST_ARCH)"
ifeq ($(HOST_OS),darwin)
	cd services/browser && cargo build --release --target aarch64-apple-darwin
	cp services/browser/target/aarch64-apple-darwin/release/browser \
	   $(BIN)/browser-$(HOST_SUFFIX)
else
	cd services/browser && cargo build --release
	cp services/browser/target/release/browser \
	   $(BIN)/browser-$(HOST_SUFFIX)
endif
	@echo "Done — binaries in $(BIN)/<name>-$(HOST_SUFFIX)"

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

test-go:
	@for pkg in sdk-go $(GO_SERVICES); do \
	  echo "→ testing services/$$pkg ..."; \
	  cd services/$$pkg && go test ./... && cd ../..; \
	done

test-rust:
	@echo "→ testing services/sdk-rust ..."
	cd services/sdk-rust && cargo test
	@echo "→ testing services/sandbox ..."
	cd services/sandbox && cargo test
	@echo "→ testing services/browser ..."
	cd services/browser && cargo test

test-py:
	python -m pytest openagent/ app/ -q

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

clean:
	rm -f $(BIN)/*
	@echo "  cleaned $(BIN)/"

help:
	@echo ""
	@echo "OpenAgent build targets"
	@echo "  make              Cross-compile all services ($(PLATFORMS))"
	@echo "  make local        Build for current host only ($(HOST_OS)/$(HOST_ARCH))"
	@echo "  make <service>    Cross-compile one Go service: $(GO_SERVICES)"
	@echo "  make sandbox      Cross-compile Rust sandbox"
	@echo "  make browser      Cross-compile Rust browser"
	@echo "                   darwin/amd64: rustup target add x86_64-apple-darwin"
	@echo "                   linux: cargo install cross --locked"
	@echo "  make test-go      Run Go tests"
	@echo "  make test-rust    Run Rust tests"
	@echo "  make test-py      Run Python tests"
	@echo "  make clean        Remove all binaries from $(BIN)/"
	@echo ""
	@echo "  All binaries land in: $(BIN)/"
	@echo "  Rust cross-compile: cargo install cross --locked"
	@echo "  MSB required at runtime: msb server start --dev"
	@echo ""
