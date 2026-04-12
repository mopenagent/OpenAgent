#!/usr/bin/env bash
# entrypoint.debug.sh — Docker entrypoint for the openagent debug container
#
# Sequence:
#   1. Detect platform (linux-arm64 or linux-amd64)
#   2. Validate the openagent control-plane binary is present
#   3. Create runtime directories (data/run, data/artifacts, logs)
#   4. Clear stale logs from any previous run
#   5. Start MCP-lite service daemons via services.sh
#   6. Pre-create service log files and tail them to Docker stdout
#      (so `docker compose logs -f debug` shows all service output in real time;
#       the raw files are also visible at ./logs/ on the host for Claude to read)
#   7. exec the openagent binary — replaces this shell as PID 1 so Docker's
#      SIGTERM reaches it directly and triggers tokio's graceful shutdown

set -euo pipefail

ROOT="/app"

# ---------------------------------------------------------------------------
# Detect linux architecture
# ---------------------------------------------------------------------------
UNAME_M="$(uname -m)"
if [ "$UNAME_M" = "aarch64" ] || [ "$UNAME_M" = "arm64" ]; then
    HOST_ARCH="arm64"
else
    HOST_ARCH="amd64"
fi
HOST_SUFFIX="linux-${HOST_ARCH}"

OPENAGENT_BIN="${ROOT}/bin/openagent-${HOST_SUFFIX}"

# ---------------------------------------------------------------------------
# Pre-flight: openagent binary must exist (built into the image by Dockerfile)
# ---------------------------------------------------------------------------
if [ ! -x "$OPENAGENT_BIN" ]; then
    echo ""
    echo "ERROR: openagent binary not found at $OPENAGENT_BIN"
    echo ""
    echo "The image needs to be rebuilt — the binary was not compiled:"
    echo "  docker compose build debug"
    echo ""
    echo "Files in /app/bin:"
    ls /app/bin 2>/dev/null || echo "  (empty or missing)"
    echo ""
    exit 1
fi

# ---------------------------------------------------------------------------
# Runtime directories
# ---------------------------------------------------------------------------
mkdir -p \
    "${ROOT}/data/run" \
    "${ROOT}/data/artifacts" \
    "${ROOT}/data/models" \
    "${ROOT}/logs"

# ---------------------------------------------------------------------------
# Clear stale logs from previous container run.
# Matches what run.sh does on the host so the log viewer always shows a fresh
# session. Files are recreated immediately when services start writing.
# ---------------------------------------------------------------------------
rm -f "${ROOT}/logs"/*.jsonl \
      "${ROOT}/logs"/*.log \
      "${ROOT}/logs"/*.log.*
echo "  Logs cleared"

# ---------------------------------------------------------------------------
# Export runtime environment
# ---------------------------------------------------------------------------
export OPENAGENT_ROOT="${ROOT}"
export OPENAGENT_LOGS_DIR="${ROOT}/logs"
# OTEL endpoint is injected by docker-compose (defaults to jaeger service)
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://jaeger:4318}"

# Microsandbox env — MSB_API_KEY must be non-empty even in --dev mode (Rust client validates).
# In --dev mode the server accepts any bearer token, so "dev" is a safe placeholder.
export MSB_API_KEY="${MSB_API_KEY:-dev}"
export MSB_SERVER_URL="${MSB_SERVER_URL:-http://127.0.0.1:5555}"
export MSB_MEMORY_MB="${MSB_MEMORY_MB:-512}"

# ---------------------------------------------------------------------------
# Start microsandbox server (required by the sandbox MCP-lite service)
# ---------------------------------------------------------------------------
echo "==> Starting microsandbox server (dev mode)"
msb server start --dev >> "${ROOT}/logs/msb.log" 2>&1 &

# Poll the JSON-RPC endpoint until the server is ready (up to 30 s).
# Any valid JSON-RPC response (including error bodies) means the server is up.
echo -n "     Waiting for microsandbox"
for _i in $(seq 1 30); do
    if curl -sf -X POST "${MSB_SERVER_URL}/api/v1/rpc" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer ${MSB_API_KEY}" \
            -d '{"jsonrpc":"2.0","method":"sandbox.start","params":{},"id":"hc"}' \
            2>/dev/null | grep -q '"jsonrpc"'; then
        echo " ready"
        break
    fi
    sleep 1
    echo -n "."
done

# Pre-warm the Python sandbox image so the first agent tool call starts instantly
# rather than cold-pulling the OCI image. Start then immediately stop a dummy sandbox.
echo "==> Pre-warming Python sandbox image (microsandbox/python)"
if curl -sf -X POST "${MSB_SERVER_URL}/api/v1/rpc" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${MSB_API_KEY}" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"sandbox.start\",\"params\":{\"sandbox\":\"prewarm-python\",\"namespace\":\"default\",\"config\":{\"image\":\"microsandbox/python\",\"memory\":${MSB_MEMORY_MB}}},\"id\":\"pw1\"}" \
        > /dev/null 2>&1; then
    curl -s -X POST "${MSB_SERVER_URL}/api/v1/rpc" \
         -H "Content-Type: application/json" \
         -H "Authorization: Bearer ${MSB_API_KEY}" \
         -d '{"jsonrpc":"2.0","method":"sandbox.stop","params":{"sandbox":"prewarm-python","namespace":"default"},"id":"pw2"}' \
         > /dev/null 2>&1 || true
    echo "     Python image cached"
else
    echo "     Warning: pre-warm failed — image will be pulled on first use"
fi
echo ""

# ---------------------------------------------------------------------------
# Start MCP-lite service daemons
#
# services.sh daemonizes each service, writes PID files to data/run/, and
# redirects each service's stdout+stderr to logs/<name>.log.
# Missing binaries are printed as [SKIP] or [MISSING] and do not abort.
# ---------------------------------------------------------------------------
echo "==> Starting MCP-lite services  [${HOST_SUFFIX}]"
"${ROOT}/services.sh" start
echo ""

# ---------------------------------------------------------------------------
# Stream service log files to Docker stdout
#
# services.sh redirects each daemon's output to logs/<name>.log.
# Pre-creating the files lets tail -F start following them immediately,
# even before the service writes its first line. tail -F (capital F) re-opens
# files by name if they are deleted and recreated.
#
# This means:
#   docker compose logs -f debug   → shows all service output + openagent stdout
#   tail -f logs/tts.log           → watch a specific service on the host
#   Claude reads ./logs/*.log      → can inspect any log file directly
# ---------------------------------------------------------------------------
for svc in browser memory msb sandbox stt tts validator whatsapp; do
    touch "${ROOT}/logs/${svc}.log" 2>/dev/null || true
done

tail -F \
    "${ROOT}/logs/browser.log" \
    "${ROOT}/logs/memory.log" \
    "${ROOT}/logs/msb.log" \
    "${ROOT}/logs/sandbox.log" \
    "${ROOT}/logs/stt.log" \
    "${ROOT}/logs/tts.log" \
    "${ROOT}/logs/validator.log" \
    "${ROOT}/logs/whatsapp.log" \
    2>/dev/null &

# Give services a moment to bind their ports before openagent connects
sleep 1

# ---------------------------------------------------------------------------
# Start openagent control plane (exec → becomes PID 1)
#
# We exec the binary directly rather than delegating to run.sh to avoid
# run.sh clearing logs a second time (which would interrupt the tail -F above).
# The Rust binary handles SIGTERM via tokio's shutdown signal.
# ---------------------------------------------------------------------------
echo "==> Starting openagent control plane  [${HOST_SUFFIX}]"
echo "  binary → ${OPENAGENT_BIN}"
echo "  root   → ${OPENAGENT_ROOT}"
echo "  logs   → ${OPENAGENT_LOGS_DIR}"
echo "  OTEL   → ${OTEL_EXPORTER_OTLP_ENDPOINT}"
echo ""

exec "${OPENAGENT_BIN}"
