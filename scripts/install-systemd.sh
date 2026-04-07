#!/usr/bin/env bash
# scripts/install-systemd.sh — install OpenAgent services as systemd units
#
# Generates one unit file per service, installs them into
# /etc/systemd/system/, reloads the daemon, and optionally enables them.
#
# Must be run as root (or with sudo) on Linux.
#
# Usage:
#   sudo bash scripts/install-systemd.sh            # install + enable all
#   sudo bash scripts/install-systemd.sh browser    # install one service
#   sudo bash scripts/install-systemd.sh --dry-run  # print units, don't install
#
# After installation:
#   systemctl start openagent-browser
#   systemctl status openagent-browser
#   journalctl -u openagent-browser -f
#   systemctl enable openagent-browser   # start on boot
#
# To remove:
#   sudo bash scripts/install-systemd.sh --uninstall

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SYSTEMD_DIR="/etc/systemd/system"
UNIT_PREFIX="openagent"

# Running user — default to current user, override with OPENAGENT_USER env var
RUN_USER="${OPENAGENT_USER:-$(logname 2>/dev/null || echo "$USER")}"
RUN_GROUP="${OPENAGENT_GROUP:-$RUN_USER}"

# Detect arch for binary suffix
UNAME_M="$(uname -m)"
if [ "$UNAME_M" = "arm64" ] || [ "$UNAME_M" = "aarch64" ]; then
  ARCH_SUFFIX="linux-arm64"
else
  ARCH_SUFFIX="linux-amd64"
fi

# ---------------------------------------------------------------------------
# Port map — must match services/<name>/service.json "address" field
# ---------------------------------------------------------------------------
declare -A SVC_PORT=(
  [browser]=9001   [channels]=9002  [cortex]=9003
  [guard]=9004     [memory]=9005    [research]=9006
  [sandbox]=9007   [stt]=9008       [tts]=9009
  [validator]=9010 [whatsapp]=9011
)

# Service-specific EnvironmentFile or ExecStartPre notes
declare -A SVC_AFTER=(
  [cortex]="openagent-guard.service openagent-memory.service"
  [channels]="openagent-cortex.service"
)

# ---------------------------------------------------------------------------
# Flags
# ---------------------------------------------------------------------------
DRY_RUN=0
UNINSTALL=0
TARGET_SVC=""

for arg in "$@"; do
  case "$arg" in
    --dry-run)   DRY_RUN=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --*)         echo "Unknown flag: $arg"; exit 1 ;;
    *)           TARGET_SVC="$arg" ;;
  esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

unit_name() { echo "${UNIT_PREFIX}-$1.service"; }

generate_unit() {
  local name="$1"
  local port="${SVC_PORT[$name]}"
  local binary="$ROOT/bin/$name-${ARCH_SUFFIX}"
  local after="network.target"

  if [ -n "${SVC_AFTER[$name]:-}" ]; then
    after="network.target ${SVC_AFTER[$name]}"
  fi

  # Build EnvironmentFile path — load secrets from .env if present
  local env_file=""
  if [ -f "$ROOT/.env" ]; then
    env_file="EnvironmentFile=-$ROOT/.env"
  fi

  cat << EOF
[Unit]
Description=OpenAgent service: $name (TCP :$port)
Documentation=https://github.com/kmaneesh/OpenAgent
After=$after
PartOf=openagent.target

[Service]
Type=simple
User=$RUN_USER
Group=$RUN_GROUP
WorkingDirectory=$ROOT
ExecStart=$binary
Restart=on-failure
RestartSec=2s
RestartMaxDelaySec=30s

# TCP address this service binds on
Environment=OPENAGENT_TCP_ADDRESS=0.0.0.0:$port
Environment=OPENAGENT_LOGS_DIR=$ROOT/logs

# Load secrets (.env) if present — ignored if file missing
${env_file}

# Resource limits (tune per service)
LimitNOFILE=65536
MemoryMax=512M

# Security hardening
NoNewPrivileges=yes
PrivateTmp=yes

# Logging — use journald (systemctl journalctl -u openagent-$name -f)
StandardOutput=journal
StandardError=journal
SyslogIdentifier=${UNIT_PREFIX}-$name

[Install]
WantedBy=multi-user.target openagent.target
EOF
}

generate_target() {
  cat << 'EOF'
[Unit]
Description=OpenAgent — all services
Documentation=https://github.com/kmaneesh/OpenAgent

[Install]
WantedBy=multi-user.target
EOF
}

install_unit() {
  local name="$1"
  local unit_file="$SYSTEMD_DIR/$(unit_name "$name")"

  echo "  Installing $(unit_name "$name")…"
  generate_unit "$name" > "$unit_file"
  chmod 644 "$unit_file"
}

uninstall_unit() {
  local name="$1"
  local unit_file="$SYSTEMD_DIR/$(unit_name "$name")"

  systemctl stop "$(unit_name "$name")" 2>/dev/null || true
  systemctl disable "$(unit_name "$name")" 2>/dev/null || true
  rm -f "$unit_file"
  echo "  Removed $(unit_name "$name")"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if [ "$DRY_RUN" = "1" ]; then
  echo "=== DRY RUN — units that would be installed ==="
  echo ""
  if [ -n "$TARGET_SVC" ]; then
    echo "### $(unit_name "$TARGET_SVC")"
    generate_unit "$TARGET_SVC"
  else
    echo "### openagent.target"
    generate_target
    echo ""
    for name in $(echo "${!SVC_PORT[@]}" | tr ' ' '\n' | sort); do
      echo "### $(unit_name "$name")"
      generate_unit "$name"
      echo ""
    done
  fi
  exit 0
fi

if [ "$(id -u)" != "0" ]; then
  echo "Error: must run as root. Use: sudo bash scripts/install-systemd.sh"
  exit 1
fi

if [ "$UNINSTALL" = "1" ]; then
  echo "Uninstalling OpenAgent systemd units…"
  for name in $(echo "${!SVC_PORT[@]}" | tr ' ' '\n' | sort); do
    uninstall_unit "$name"
  done
  rm -f "$SYSTEMD_DIR/openagent.target"
  systemctl daemon-reload
  echo ""
  echo "Done. All openagent-*.service units removed."
  exit 0
fi

# Ensure log dir exists and is writable by the service user
mkdir -p "$ROOT/logs" "$ROOT/data/artifacts"
chown -R "$RUN_USER:$RUN_GROUP" "$ROOT/logs" "$ROOT/data" 2>/dev/null || true

echo "Installing OpenAgent systemd units"
echo "  root:   $ROOT"
echo "  user:   $RUN_USER"
echo "  arch:   $ARCH_SUFFIX"
echo "  target: ${TARGET_SVC:-all}"
echo ""

# Install openagent.target
echo "  Installing openagent.target…"
generate_target > "$SYSTEMD_DIR/openagent.target"
chmod 644 "$SYSTEMD_DIR/openagent.target"

# Install service units
if [ -n "$TARGET_SVC" ]; then
  if [ -z "${SVC_PORT[$TARGET_SVC]:-}" ]; then
    echo "Error: unknown service '$TARGET_SVC'"
    echo "Known: ${!SVC_PORT[*]}"
    exit 1
  fi
  install_unit "$TARGET_SVC"
else
  for name in $(echo "${!SVC_PORT[@]}" | tr ' ' '\n' | sort); do
    install_unit "$name"
  done
fi

systemctl daemon-reload
echo ""
echo "Done. Next steps:"
echo ""
echo "  Start all services now:"
echo "    sudo systemctl start openagent.target"
echo ""
echo "  Enable on boot:"
echo "    sudo systemctl enable openagent.target"
echo ""
echo "  Start + enable a single service:"
echo "    sudo systemctl enable --now openagent-browser"
echo ""
echo "  View logs:"
echo "    journalctl -u openagent-browser -f"
echo ""
echo "  Check status:"
echo "    systemctl status 'openagent-*'"
