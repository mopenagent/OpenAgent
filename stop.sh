#!/usr/bin/env bash
# stop.sh — kill all OpenAgent processes and clean up stale sockets.
# Run this when Ctrl-C didn't work or after a crash leaves orphans behind.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
SOCKETS_DIR="$ROOT/data/sockets"

echo "Stopping OpenAgent processes..."

# Kill by binary name pattern — covers both managed children and manual runs.
PATTERNS=(
    "openagent-darwin-arm64"
    "openagent-linux-arm64"
    "openagent-linux-amd64"
    "cortex-darwin-arm64"
    "cortex-linux-arm64"
    "research-darwin-arm64"
    "research-linux-arm64"
    "guard-darwin-arm64"
    "guard-linux-arm64"
    "browser-darwin-arm64"
    "browser-linux-arm64"
    "channels-darwin-arm64"
    "channels-linux-arm64"
    "whatsapp-darwin-arm64"
    "whatsapp-linux-arm64"
    "sandbox-darwin-arm64"
    "sandbox-linux-arm64"
    "stt-darwin-arm64"
    "stt-linux-arm64"
    "tts-darwin-arm64"
    "tts-linux-arm64"
    "memory-darwin-arm64"
    "memory-linux-arm64"
    "validator-darwin-arm64"
    "validator-linux-arm64"
)

killed=0
for pat in "${PATTERNS[@]}"; do
    pids=$(pgrep -f "$pat" 2>/dev/null || true)
    if [ -n "$pids" ]; then
        echo "  killing $pat (pids: $pids)"
        kill -TERM $pids 2>/dev/null || true
        killed=$((killed + $(echo "$pids" | wc -w)))
    fi
done

# Give processes a moment to exit cleanly.
[ "$killed" -gt 0 ] && sleep 1

# Force-kill anything still alive.
for pat in "${PATTERNS[@]}"; do
    pids=$(pgrep -f "$pat" 2>/dev/null || true)
    if [ -n "$pids" ]; then
        echo "  force-killing $pat (pids: $pids)"
        kill -KILL $pids 2>/dev/null || true
    fi
done

# Remove stale socket files so the next start doesn't find leftover locks.
if [ -d "$SOCKETS_DIR" ]; then
    echo "Cleaning sockets in $SOCKETS_DIR..."
    find "$SOCKETS_DIR" -name "*.sock" -delete 2>/dev/null || true
fi

echo "Done."
