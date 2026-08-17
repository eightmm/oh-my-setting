#!/usr/bin/env bash
set -euo pipefail

# Register the oms state-and-peer MCP server with the MCP-capable provider CLIs
# (Claude Code and Codex; Antigravity receives it through the agy plugin).
# The server exposes one repository's harness state — task packet,
# fail-ledger, handoff digests, Work Journal — so every MCP client reads the
# same state without per-CLI hook code. Idempotent: an existing registration
# pointing at this checkout is left alone, anything else is replaced.
# A missing CLI is a note while registering. Removal fails closed because it
# cannot prove a provider-local registration is absent without that provider.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SERVER="$ROOT/scripts/oms-mcp-server.py"
NAME="oh-my-setting"
REMOVE=0

usage() {
  cat <<'EOF'
Usage: install-mcp.sh [--remove]

Register (or with --remove, unregister) the oh-my-setting state-and-peer MCP
server for Claude Code and Codex. State tools read local harness data;
oms_peer_start launches an external peer call and writes .oms artifacts.
OH_MY_SETTING_MCP=0 skips registration.
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
if [ "$REMOVE" != "1" ]; then
  command -v python3 >/dev/null 2>&1 || fail "python3 is required"
  [ -f "$SERVER" ] || fail "oms-mcp-server.py not found under $ROOT"
fi

# Provider management CLIs run unattended here — update.sh reaches this from
# auto-update's cron chain — so a call must never wait on a prompt (stdin
# closed) or run unbounded (timeout where coreutils provides one; the agy
# plugin installer set this pattern). A timeout's exit 124 lands in the same
# failure branches as any other nonzero status.
provider_cmd() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "${OMS_PROVIDER_CMD_TIMEOUT:-120}" "$@" </dev/null
  else
    "$@" </dev/null
  fi
}

mcp_registration_state() {  # CLI
  local cli="$1"
  local out

  if out="$(provider_cmd "$cli" mcp get "$NAME" 2>&1)"; then
    return 0
  fi
  case "$out" in
    *"No MCP server named"*"$NAME"*) return 1 ;;
  esac
  [ -z "$out" ] || printf '%s\n' "$out" >&2
  return 2
}

register_claude() {
  command -v claude >/dev/null 2>&1 || {
    if [ "$REMOVE" = "1" ]; then
      echo "error: claude CLI is required to inspect and remove its MCP registration" >&2
      return 1
    fi
    echo "mcp: note: claude CLI absent; skipped"
    return 0
  }
  if [ "$REMOVE" = "1" ]; then
    local state=0
    mcp_registration_state claude || state=$?
    case "$state" in
      0)
        if provider_cmd claude mcp remove -s user "$NAME" >/dev/null 2>&1; then
          echo "mcp: claude registration removed"
          return 0
        fi
        echo "error: could not remove the claude MCP registration" >&2
        return 1
        ;;
      1)
        echo "mcp: claude registration already absent"
        return 0
        ;;
      *)
        echo "error: could not inspect the claude MCP registration" >&2
        return 1
        ;;
    esac
  fi
  local current
  if current="$(provider_cmd claude mcp get "$NAME" 2>/dev/null)" &&
     printf '%s' "$current" | grep -Fq "$SERVER"; then
    echo "mcp: claude already registered"
    return 0
  fi
  provider_cmd claude mcp remove -s user "$NAME" >/dev/null 2>&1 || true
  if provider_cmd claude mcp add -s user "$NAME" -- python3 "$SERVER" >/dev/null 2>&1; then
    echo "mcp: claude registered ($NAME)"
  else
    echo "warn: could not register the MCP server with claude" >&2
  fi
}

register_codex() {
  command -v codex >/dev/null 2>&1 || {
    if [ "$REMOVE" = "1" ]; then
      echo "error: codex CLI is required to inspect and remove its MCP registration" >&2
      return 1
    fi
    echo "mcp: note: codex CLI absent; skipped"
    return 0
  }
  if [ "$REMOVE" = "1" ]; then
    local state=0
    mcp_registration_state codex || state=$?
    case "$state" in
      0)
        if provider_cmd codex mcp remove "$NAME" >/dev/null 2>&1; then
          echo "mcp: codex registration removed"
          return 0
        fi
        echo "error: could not remove the codex MCP registration" >&2
        return 1
        ;;
      1)
        echo "mcp: codex registration already absent"
        return 0
        ;;
      *)
        echo "error: could not inspect the codex MCP registration" >&2
        return 1
        ;;
    esac
  fi
  local current
  if current="$(provider_cmd codex mcp get "$NAME" 2>/dev/null)" &&
     printf '%s' "$current" | grep -Fq "$SERVER"; then
    echo "mcp: codex already registered"
    return 0
  fi
  provider_cmd codex mcp remove "$NAME" >/dev/null 2>&1 || true
  if provider_cmd codex mcp add "$NAME" -- python3 "$SERVER" >/dev/null 2>&1; then
    echo "mcp: codex registered ($NAME)"
  else
    echo "warn: could not register the MCP server with codex" >&2
  fi
}

status=0
register_claude || status=1
register_codex || status=1
exit "$status"
