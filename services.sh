#!/usr/bin/env bash
# services.sh — dev-mode service supervisor (macOS + Linux, bash 3+)
#
# On PRODUCTION (Linux/Pi) use systemd instead:
#   sudo bash scripts/install-systemd.sh
#   sudo systemctl enable --now openagent.target
#
# Usage:
#   ./services.sh                   # start all services
#   ./services.sh start browser     # start one service
#   ./services.sh stop              # stop all services
#   ./services.sh stop sandbox      # stop one service
#   ./services.sh restart           # restart all
#   ./services.sh restart cortex    # restart one
#   ./services.sh status            # show status of all services

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/bin"
RUN_DIR="$ROOT/data/run"
LOG_DIR="$ROOT/logs"

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------
UNAME_S="$(uname -s)"
UNAME_M="$(uname -m)"
if   [ "$UNAME_S" = "Darwin" ];                                 then HOST_OS="darwin"
else                                                                 HOST_OS="linux"; fi
if   [ "$UNAME_M" = "arm64" ] || [ "$UNAME_M" = "aarch64" ];   then HOST_ARCH="arm64"
else                                                                 HOST_ARCH="amd64"; fi
HOST_SUFFIX="${HOST_OS}-${HOST_ARCH}"

# ---------------------------------------------------------------------------
# Port map — must match services/<name>/service.json "address" field
# (plain strings — no associative arrays, compatible with bash 3.x on macOS)
# ---------------------------------------------------------------------------
ALL_SERVICES="browser channels cortex guard memory research sandbox stt tts validator whatsapp"

svc_port() {
  case "$1" in
    browser)   echo 9001 ;;
    channels)  echo 9002 ;;
    cortex)    echo 9003 ;;
    guard)     echo 9004 ;;
    memory)    echo 9005 ;;
    research)  echo 9006 ;;
    sandbox)   echo 9007 ;;
    stt)       echo 9008 ;;
    tts)       echo 9009 ;;
    validator) echo 9010 ;;
    whatsapp)  echo 9011 ;;
    *)         echo "" ;;
  esac
}

# Services whose absence is expected (binary may not be built yet)
OPTIONAL_SERVICES="tts stt channels whatsapp"

is_optional() { echo "$OPTIONAL_SERVICES" | grep -qw "$1"; }

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

pid_file() { echo "$RUN_DIR/$1.pid"; }
log_file() { echo "$LOG_DIR/$1.log"; }
bin_path() { echo "$BIN/$1-${HOST_SUFFIX}"; }

is_running() {
  local pidfile
  pidfile="$(pid_file "$1")"
  [ -f "$pidfile" ] || return 1
  local pid
  pid="$(cat "$pidfile" 2>/dev/null)" || return 1
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

load_env() {
  [ -f "$ROOT/.env" ] || return 0
  set -a
  # shellcheck source=/dev/null
  source "$ROOT/.env"
  set +a
}

# Build env var list for a service (printed as KEY=VALUE lines for env(1))
svc_env_vars() {
  local name="$1"
  local port="$2"
  echo "OPENAGENT_TCP_ADDRESS=0.0.0.0:$port"
  echo "OPENAGENT_LOGS_DIR=$LOG_DIR"
  case "$name" in
    cortex)
      [ -n "${OPENAGENT_LLM_BASE_URL:-}" ] && echo "OPENAGENT_LLM_BASE_URL=$OPENAGENT_LLM_BASE_URL"
      [ -n "${OPENAGENT_API_KEY:-}"       ] && echo "OPENAGENT_API_KEY=$OPENAGENT_API_KEY"
      [ -n "${OPENAGENT_MODEL:-}"         ] && echo "OPENAGENT_MODEL=$OPENAGENT_MODEL"
      ;;
    browser)
      [ -n "${SEARXNG_URL:-}"             ] && echo "SEARXNG_URL=$SEARXNG_URL"
      ;;
    sandbox)
      [ -n "${MSB_SERVER_URL:-}"          ] && echo "MSB_SERVER_URL=$MSB_SERVER_URL"
      [ -n "${MSB_API_KEY:-}"             ] && echo "MSB_API_KEY=$MSB_API_KEY"
      [ -n "${MSB_MEMORY_MB:-}"           ] && echo "MSB_MEMORY_MB=$MSB_MEMORY_MB"
      ;;
    channels)
      [ -n "${DISCORD_TOKEN:-}"           ] && echo "DISCORD_TOKEN=$DISCORD_TOKEN"
      [ -n "${SLACK_BOT_TOKEN:-}"         ] && echo "SLACK_BOT_TOKEN=$SLACK_BOT_TOKEN"
      [ -n "${SLACK_APP_TOKEN:-}"         ] && echo "SLACK_APP_TOKEN=$SLACK_APP_TOKEN"
      ;;
    whatsapp)
      [ -n "${WHATSAPP_PHONE:-}"          ] && echo "WHATSAPP_PHONE=$WHATSAPP_PHONE"
      ;;
  esac
}

# ---------------------------------------------------------------------------
# start / stop / status for a single service
# ---------------------------------------------------------------------------

start_one() {
  local name="$1"
  local port
  port="$(svc_port "$name")"

  if [ -z "$port" ]; then
    echo "  [SKIP]    $name — not in port map"
    return
  fi

  local bin
  bin="$(bin_path "$name")"

  if [ ! -x "$bin" ]; then
    if is_optional "$name"; then
      echo "  [SKIP]    $name — binary not built (optional)"
    else
      echo "  [MISSING] $name :$port — binary not found: $bin  (run: make local)"
    fi
    return
  fi

  if is_running "$name"; then
    echo "  [RUNNING] $name :$port  (pid $(cat "$(pid_file "$name")"))"
    return
  fi

  mkdir -p "$RUN_DIR" "$LOG_DIR"

  # Build env and exec in a subshell so we don't pollute the parent environment
  (
    while IFS= read -r kv; do
      export "$kv"
    done < <(svc_env_vars "$name" "$port")
    exec "$bin" >> "$(log_file "$name")" 2>&1
  ) &
  local pid=$!
  echo "$pid" > "$(pid_file "$name")"
  echo "  [STARTED] $name :$port  (pid $pid)  log → logs/$name.log"
}

stop_one() {
  local name="$1"
  local pidfile
  pidfile="$(pid_file "$name")"

  if ! is_running "$name"; then
    echo "  [STOPPED] $name — not running"
    rm -f "$pidfile"
    return
  fi

  local pid
  pid="$(cat "$pidfile")"
  kill -TERM "$pid" 2>/dev/null || true

  local i
  for i in 1 2 3 4 5; do
    sleep 1
    kill -0 "$pid" 2>/dev/null || break
  done
  kill -KILL "$pid" 2>/dev/null || true
  rm -f "$pidfile"
  echo "  [STOPPED] $name  (pid $pid)"
}

status_one() {
  local name="$1"
  local port
  port="$(svc_port "$name")"
  if is_running "$name"; then
    local pid
    pid="$(cat "$(pid_file "$name")")"
    printf "  %-12s  :%-5s  RUNNING   pid=%s\n" "$name" "$port" "$pid"
  else
    printf "  %-12s  :%-5s  stopped\n" "$name" "$port"
  fi
}

# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

cmd_start() {
  local target="${1:-all}"
  load_env
  echo "Starting services  [$HOST_SUFFIX]"
  if [ "$target" = "all" ]; then
    for name in $ALL_SERVICES; do start_one "$name"; done
  else
    start_one "$target"
  fi
}

cmd_stop() {
  local target="${1:-all}"
  echo "Stopping services"
  if [ "$target" = "all" ]; then
    for name in $ALL_SERVICES; do stop_one "$name"; done
  else
    stop_one "$target"
  fi
}

cmd_restart() {
  local target="${1:-all}"
  cmd_stop "$target"
  sleep 1
  cmd_start "$target"
}

cmd_status() {
  echo "Service status  [$HOST_SUFFIX]"
  echo ""
  for name in $ALL_SERVICES; do status_one "$name"; done
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

COMMAND="${1:-start}"
TARGET="${2:-all}"

case "$COMMAND" in
  start)   cmd_start   "$TARGET" ;;
  stop)    cmd_stop    "$TARGET" ;;
  restart) cmd_restart "$TARGET" ;;
  status)  cmd_status ;;
  *)
    echo "Usage: $0 {start|stop|restart|status} [service|all]"
    exit 1
    ;;
esac
