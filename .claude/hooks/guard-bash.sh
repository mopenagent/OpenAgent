#!/usr/bin/env bash
# guard-bash.sh — PreToolUse hook for Bash tool.
#
# Blocks:
#   - rm -rf on source directories
#   - git push --force to main/master
#   - direct writes to data/ or logs/ via shell redirection
#
# Warns:
#   - git commit --amend (prefer new commits)
#   - cargo build inside a service dir without --release (builds debug binary)

set -euo pipefail

INPUT=$(cat)

COMMAND=$(echo "$INPUT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print(d.get('tool_input', {}).get('command', ''))
" 2>/dev/null || true)

[ -z "$COMMAND" ] && exit 0

# ── Destructive rm guard ───────────────────────────────────────────────────────
if echo "$COMMAND" | grep -qE 'rm\s+-rf?\s+\S*(openagent|services|app|skills|config)'; then
  echo "BLOCKED: Refusing to rm -rf a source directory."
  echo "Delete files individually or use git clean -fd after checking what would be removed."
  exit 2
fi

# ── Force push to main/master ─────────────────────────────────────────────────
if echo "$COMMAND" | grep -qE 'git\s+push.*--force.*\b(main|master)\b|git\s+push.*\b(main|master)\b.*--force'; then
  echo "BLOCKED: Force-pushing to main/master is not allowed."
  echo "Create a new commit instead, or discuss the reset with the team first."
  exit 2
fi

# ── Shell redirection into data/ or logs/ ─────────────────────────────────────
if echo "$COMMAND" | grep -qE '>\s*(data|logs)/'; then
  echo "BLOCKED: Do not write to data/ or logs/ via shell redirection."
  echo "data/ is runtime storage; logs/ is OTEL output. Write to a temp file instead."
  exit 2
fi

# ── Warning: git commit --amend ───────────────────────────────────────────────
if echo "$COMMAND" | grep -qE 'git\s+commit.*--amend'; then
  echo "WARNING: --amend rewrites published history if the commit is already pushed."
  echo "Prefer creating a new commit unless you are certain this is a local-only fix."
  exit 1
fi

exit 0
