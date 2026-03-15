#!/usr/bin/env bash
# run.sh — start the OpenAgent Rust runtime
#
# The Rust binary (openagent) is the sole control plane process.  It:
#   - spawns and monitors all MCP-lite services (cortex, channels, guard, …)
#   - runs the dispatch loop (channels → cortex → channel.send)
#   - serves the Axum control plane API on TCP :8080
#
# The Python web UI runs separately via Docker:
#   docker compose up -d       # starts jaeger + web UI
#
# Usage:
#   ./run.sh                   # start openagent runtime
#   PORT=9090 ./run.sh         # override Axum port (default 8080)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/bin"

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"
if [ "$UNAME_S" = "Darwin" ]; then HOST_OS="darwin"; else HOST_OS="linux"; fi
if [ "$UNAME_M" = "arm64" ] || [ "$UNAME_M" = "aarch64" ]; then HOST_ARCH="arm64"; else HOST_ARCH="amd64"; fi
HOST_SUFFIX="${HOST_OS}-${HOST_ARCH}"

# ---------------------------------------------------------------------------
# Locate / build the openagent binary
# ---------------------------------------------------------------------------

OPENAGENT_BIN="$BIN/openagent-${HOST_SUFFIX}"

if [ ! -x "$OPENAGENT_BIN" ]; then
  echo "openagent binary missing for ${HOST_SUFFIX} — building it (make openagent-local)"
  make -C "$ROOT" openagent-local
fi

# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------

export OPENAGENT_ROOT="${OPENAGENT_ROOT:-$ROOT}"
export OPENAGENT_LOGS_DIR="${OPENAGENT_LOGS_DIR:-$ROOT/logs}"
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:4318}"

# Load .env if present (secrets — DISCORD_TOKEN, SLACK_*, etc.)
if [ -f "$ROOT/.env" ]; then
  set -a
  # shellcheck source=/dev/null
  source "$ROOT/.env"
  set +a
  echo "  Loaded .env"
fi

mkdir -p "$ROOT/logs" "$ROOT/data/sockets" "$ROOT/data/artifacts" "$ROOT/data/run"

PIDFILE="$ROOT/data/run/openagent.pid"

# ---------------------------------------------------------------------------
# Kill any previous instance recorded in the PID file
# ---------------------------------------------------------------------------
if [ -f "$PIDFILE" ]; then
  OLD_PID=$(cat "$PIDFILE" 2>/dev/null || true)
  if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
    echo "  Stopping previous instance (PID $OLD_PID)…"
    kill -TERM "$OLD_PID" 2>/dev/null || true
    sleep 1
    kill -KILL "$OLD_PID" 2>/dev/null || true
  fi
  rm -f "$PIDFILE"
fi

# ---------------------------------------------------------------------------
# Shutdown handler
# ---------------------------------------------------------------------------

_shutdown() {
  echo ""
  echo "Shutting down…"
  rm -f "$PIDFILE"
  if [ -n "${OPENAGENT_PID:-}" ] && kill -0 "$OPENAGENT_PID" 2>/dev/null; then
    # SIGTERM — openagent handles this and kills its children via stop_all().
    kill -TERM "$OPENAGENT_PID" 2>/dev/null || true
    # Wait up to 5 s for clean exit, then SIGKILL the whole process group.
    for _ in 1 2 3 4 5; do
      sleep 1
      kill -0 "$OPENAGENT_PID" 2>/dev/null || break
    done
    kill -KILL "$OPENAGENT_PID" 2>/dev/null || true
    # Belt-and-suspenders: kill any surviving service children by process group.
    kill -KILL -"$OPENAGENT_PID" 2>/dev/null || true
    wait "$OPENAGENT_PID" 2>/dev/null || true
  fi
  exit 0
}

trap _shutdown INT TERM

# ---------------------------------------------------------------------------
# Start openagent (Rust runtime)
# ---------------------------------------------------------------------------

echo "Starting OpenAgent runtime  [${HOST_SUFFIX}]"
echo "  binary  → $OPENAGENT_BIN"
echo "  root    → $OPENAGENT_ROOT"
echo "  logs    → $OPENAGENT_LOGS_DIR"
echo "  OTEL    → $OTEL_EXPORTER_OTLP_ENDPOINT"
echo ""
echo "  Web UI + Jaeger → docker compose up -d"
echo ""

"$OPENAGENT_BIN" &
OPENAGENT_PID=$!
echo "$OPENAGENT_PID" > "$PIDFILE"
wait "$OPENAGENT_PID"
rm -f "$PIDFILE"
