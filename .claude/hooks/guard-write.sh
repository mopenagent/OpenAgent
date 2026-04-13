#!/usr/bin/env bash
# guard-write.sh — PreToolUse hook for Write and Edit tools.
#
# Blocks:
#   - Writes to .env files (secrets)
#   - Writes inside data/ (runtime storage)
#   - static mut in Rust non-test source
#   - unbounded_channel in Rust non-test source
#
# Warns (exit 1 — shown to Claude but does not block):
#   - unwrap()/expect() in Rust non-test source

set -euo pipefail

INPUT=$(cat)

# Extract file path from tool input (Write or Edit)
FILE=$(echo "$INPUT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
inp = d.get('tool_input', {})
print(inp.get('file_path', ''))
" 2>/dev/null || true)

[ -z "$FILE" ] && exit 0

# ── Secrets guard ─────────────────────────────────────────────────────────────
if [[ "$FILE" == *.env ]] || [[ "$FILE" == */.env ]]; then
  echo "BLOCKED: Never write to .env files — credentials must stay out of source."
  echo "Use .env.example as a template. Set real values in the shell environment."
  exit 2
fi

# ── Runtime data guard ────────────────────────────────────────────────────────
if [[ "$FILE" == data/* ]] || [[ "$FILE" == ./data/* ]]; then
  echo "BLOCKED: data/ is runtime storage (SQLite, LanceDB, artifacts)."
  echo "Do not write source files there. Check your file path."
  exit 2
fi

# ── Rust anti-pattern checks ──────────────────────────────────────────────────
# Only applies to .rs files that are not tests
if [[ "$FILE" == *.rs ]]; then
  # Skip test files
  if [[ "$FILE" == *"/tests/"* ]] || [[ "$FILE" == *"_test.rs" ]] || [[ "$FILE" == */test.rs ]]; then
    exit 0
  fi

  # Extract the content being written (Write: content field, Edit: new_string field)
  CONTENT=$(echo "$INPUT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
inp = d.get('tool_input', {})
print(inp.get('content', '') or inp.get('new_string', ''))
" 2>/dev/null || true)

  [ -z "$CONTENT" ] && exit 0

  # Hard block: static mut
  if echo "$CONTENT" | grep -qE '\bstatic\s+mut\b'; then
    echo "BLOCKED: 'static mut' is forbidden in this codebase."
    echo "Use Arc<Mutex<T>>, Arc<RwLock<T>>, or Atomic* types for shared state."
    exit 2
  fi

  # Hard block: unbounded_channel
  if echo "$CONTENT" | grep -qE '\bunbounded_channel\b'; then
    echo "BLOCKED: 'unbounded_channel' is forbidden — silent memory leak risk."
    echo "Use mpsc::channel(N) or broadcast::channel(N) with an explicit capacity."
    exit 2
  fi

  # Warning: unwrap()/expect() — exit 1 shows the warning but does not block
  if echo "$CONTENT" | grep -qE '\.(unwrap|expect)\s*\('; then
    echo "WARNING: unwrap()/expect() detected in non-test Rust code."
    echo "Prefer Result<_, E> with thiserror-derived error types."
    echo "Only proceed if this is truly an unreachable path and you've added a comment."
    exit 1
  fi
fi

exit 0
