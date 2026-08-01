#!/usr/bin/env bash
set -euo pipefail

# Register the read-only oms MCP server with the MCP-capable provider CLIs
# (Claude Code and Codex; Antigravity receives it through the agy plugin).
# The server exposes one repository's harness state — task packet,
# fail-ledger, handoff digests, Work Journal — so every MCP client reads the
# same state without per-CLI hook code. Idempotent: an existing registration
# pointing at this checkout is left alone, anything else is replaced.
# A missing CLI is a note, not a failure. --remove unregisters.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SERVER="$ROOT/scripts/oms-mcp-server.py"
NAME="oh-my-setting"
REMOVE=0

usage() {
  cat <<'EOF'
Usage: install-mcp.sh [--remove]

Register (or with --remove, unregister) the oh-my-setting read-only MCP
server for Claude Code and Codex. OH_MY_SETTING_MCP=0 skips registration.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --remove) REMOVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

if [ "${OH_MY_SETTING_MCP:-1}" != "1" ] && [ "$REMOVE" != "1" ]; then
  echo "mcp: skipped (OH_MY_SETTING_MCP=0)"
  exit 0
fi
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
[ -f "$SERVER" ] || fail "oms-mcp-server.py not found under $ROOT"

register_claude() {
  command -v claude >/dev/null 2>&1 || {
    echo "mcp: note: claude CLI absent; skipped"
    return 0
  }
  if [ "$REMOVE" = "1" ]; then
    claude mcp remove -s user "$NAME" >/dev/null 2>&1 || true
    echo "mcp: claude registration removed (if present)"
    return 0
  fi
  local current
  if current="$(claude mcp get "$NAME" 2>/dev/null)" &&
     printf '%s' "$current" | grep -Fq "$SERVER"; then
    echo "mcp: claude already registered"
    return 0
  fi
  claude mcp remove -s user "$NAME" >/dev/null 2>&1 || true
  if claude mcp add -s user "$NAME" -- python3 "$SERVER" >/dev/null 2>&1; then
    echo "mcp: claude registered ($NAME)"
  else
    echo "warn: could not register the MCP server with claude" >&2
  fi
}

register_codex() {
  command -v codex >/dev/null 2>&1 || {
    echo "mcp: note: codex CLI absent; skipped"
    return 0
  }
  if [ "$REMOVE" = "1" ]; then
    codex mcp remove "$NAME" >/dev/null 2>&1 || true
    echo "mcp: codex registration removed (if present)"
    return 0
  fi
  local current
  if current="$(codex mcp get "$NAME" 2>/dev/null)" &&
     printf '%s' "$current" | grep -Fq "$SERVER"; then
    echo "mcp: codex already registered"
    return 0
  fi
  codex mcp remove "$NAME" >/dev/null 2>&1 || true
  if codex mcp add "$NAME" -- python3 "$SERVER" >/dev/null 2>&1; then
    echo "mcp: codex registered ($NAME)"
  else
    echo "warn: could not register the MCP server with codex" >&2
  fi
}

register_claude
register_codex
