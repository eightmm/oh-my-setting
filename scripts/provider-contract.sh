#!/usr/bin/env bash
set -euo pipefail

# Verify Codex, Claude Code, and Antigravity discover the same harness
# surfaces. Native loaders drift silently — one CLI following rules the other
# two retired is the failure class this gate exists to catch, and no unit test
# of a single script can prove the cross-provider integration. Three contracts
# are checked on this machine: project loader parity on a throwaway fixture
# (managed rule blocks stay identical across AGENTS.md, CLAUDE.md, and an
# adopted GEMINI.md, with stale base styles retired on a style switch),
# harness-state MCP registration (every registered provider points at one and
# the same oms-mcp-server.py), and fail-open no-ops (hook scripts exit 0 and
# seed no .oms state in an unadopted directory). Read-only outside its own
# temp fixture; a missing provider CLI is a note, never a failure.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
Usage: provider-contract.sh [--check]

Verify the cross-CLI harness contract: loader parity on a temp fixture,
harness-state MCP registration parity, and fail-open hook no-ops.

Options:
  --check      Run the checks (the default action).
  -h, --help   Show this help.

Exit status: 0 all checks pass, 1 at least one check failed, 2 misuse.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

CHECKS=0
FAILS=0
NOTES=0
ok() { CHECKS=$((CHECKS + 1)); printf 'ok: %s\n' "$*"; }
bad() { CHECKS=$((CHECKS + 1)); FAILS=$((FAILS + 1)); printf 'fail: %s\n' "$*" >&2; }
note() { NOTES=$((NOTES + 1)); printf 'note: %s\n' "$*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-provider-contract.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# 1. Loader parity on a throwaway fixture.
# ---------------------------------------------------------------------------

marker_set() {
  # The per-style managed block markers a loader file carries.
  grep -o '<!-- oh-my-setting:[a-z]*:begin -->' "$1" 2>/dev/null | LC_ALL=C sort -u
}

loader_parity() {
  local fixture="$TMP/fixture"
  local log="$TMP/apply.log"
  local agents claude gemini

  mkdir -p "$fixture"
  if ! "$ROOT/scripts/apply-project-template.sh" general "$fixture" \
    --no-private >"$log" 2>&1; then
    bad "loader fixture: apply-project-template general failed (see below)"
    cat "$log" >&2
    return 0
  fi
  agents="$(marker_set "$fixture/AGENTS.md")"
  claude="$(marker_set "$fixture/CLAUDE.md")"
  if [ -z "$agents" ]; then
    bad "loader parity (general): AGENTS.md carries no managed block"
  elif [ "$agents" != "$claude" ]; then
    bad "loader parity (general): AGENTS.md and CLAUDE.md block sets differ"
  elif [ -e "$fixture/GEMINI.md" ]; then
    bad "loader parity (general): GEMINI.md was created for a new project"
  else
    ok "loader parity (general): one block set across loaders, no GEMINI.md invented"
  fi

  # A leftover GEMINI.md must be adopted on the next apply, and a base-style
  # switch must retire the old style everywhere — the drift both fixes in the
  # 0.4.0 cycle exist to prevent.
  printf '%s\nstale\n%s\n' \
    '<!-- oh-my-setting:general:begin -->' \
    '<!-- oh-my-setting:general:end -->' > "$fixture/GEMINI.md"
  if ! "$ROOT/scripts/apply-project-template.sh" ml "$fixture" \
    --no-private >"$log" 2>&1; then
    bad "loader fixture: apply-project-template ml failed (see below)"
    cat "$log" >&2
    return 0
  fi
  agents="$(marker_set "$fixture/AGENTS.md")"
  claude="$(marker_set "$fixture/CLAUDE.md")"
  gemini="$(marker_set "$fixture/GEMINI.md")"
  if [ "$agents" != "$claude" ] || [ "$agents" != "$gemini" ]; then
    bad "loader parity (ml): block sets diverge across AGENTS.md/CLAUDE.md/GEMINI.md"
  elif printf '%s\n' "$agents" | grep -q ':general:'; then
    bad "loader parity (ml): stale general block survived the style switch"
  elif ! printf '%s\n' "$agents" | grep -q ':ml:'; then
    bad "loader parity (ml): ml block missing after apply"
  else
    ok "loader parity (ml): GEMINI.md adopted and stale style retired in sync"
  fi
}

loader_parity

# ---------------------------------------------------------------------------
# 2. Fail-open no-ops: hook scripts must exit 0 and seed no .oms state in an
#    unadopted directory.
# ---------------------------------------------------------------------------

fail_open() {
  local plain="$TMP/unadopted"
  local hook rc bad_hooks=""

  mkdir -p "$plain"
  for hook in skill-router turn-guard fail-ledger-hook precompact-handoff; do
    [ -f "$ROOT/scripts/$hook.sh" ] || { bad_hooks="$bad_hooks $hook(missing)"; continue; }
    rc=0
    (cd "$plain" && printf '' |
      OMS_STATE_REPO="$plain" OMS_HARNESS_CHILD=0 \
        bash "$ROOT/scripts/$hook.sh" >/dev/null 2>&1) || rc=$?
    [ "$rc" -eq 0 ] || bad_hooks="$bad_hooks $hook(exit $rc)"
  done
  if [ -n "$bad_hooks" ]; then
    bad "fail-open: hook scripts not silent in an unadopted directory:$bad_hooks"
  elif [ -d "$plain/.oms" ]; then
    bad "fail-open: a hook seeded .oms in an unadopted directory"
  else
    ok "fail-open: hook scripts exit 0 without seeding .oms in an unadopted directory"
  fi
}

fail_open

# ---------------------------------------------------------------------------
# 3. Harness-state MCP registration parity. The registered path may belong to
#    a different checkout than this one (dev checkout vs install); the
#    contract is that every provider reads the SAME server.
# ---------------------------------------------------------------------------

SERVER_BASENAME="oms-mcp-server.py"
FOUND_PATHS="$TMP/mcp-paths"
: > "$FOUND_PATHS"

registered_path_claude() {
  python3 - "$1" <<'PY'
import json, sys
try:
    cfg = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
entry = (cfg.get("mcpServers") or {}).get("oh-my-setting") or {}
args = entry.get("args") or []
if args:
    print(args[-1])
PY
}

registered_path_codex() {
  python3 - "$1" <<'PY'
import re, sys
try:
    text = open(sys.argv[1]).read()
except Exception:
    sys.exit(0)
m = re.search(r"^\[mcp_servers\.(?:\"oh-my-setting\"|oh-my-setting)\]\s*$(.*?)(?=^\[|\Z)",
              text, re.M | re.S)
if not m:
    sys.exit(0)
args = re.search(r"^\s*args\s*=\s*\[(.*?)\]", m.group(1), re.M | re.S)
if args:
    parts = re.findall(r'"([^"]*)"', args.group(1))
    if parts:
        print(parts[-1])
PY
}

check_provider() {
  local provider="$1" cli="$2" cfg="$3" extractor="$4"
  local path=""

  if [ -f "$cfg" ]; then
    path="$("$extractor" "$cfg")"
  fi
  if [ -n "$path" ]; then
    case "$path" in
      */"$SERVER_BASENAME") ;;
      *)
        bad "mcp ($provider): registration points at $path, not $SERVER_BASENAME"
        return 0
        ;;
    esac
    if [ ! -f "$path" ]; then
      bad "mcp ($provider): registered server $path is missing on disk"
      return 0
    fi
    printf '%s\n' "$path" >> "$FOUND_PATHS"
    ok "mcp ($provider): registered at $path"
  elif command -v "$cli" >/dev/null 2>&1; then
    bad "mcp ($provider): CLI installed but no oh-my-setting registration — run scripts/install-mcp.sh"
  else
    note "mcp ($provider): CLI not installed; skipped"
  fi
}

check_provider claude claude "$HOME/.claude.json" registered_path_claude
check_provider codex codex "${CODEX_HOME:-$HOME/.codex}/config.toml" registered_path_codex
check_provider antigravity agy \
  "$HOME/.gemini/config/plugins/oh-my-setting/mcp_config.json" registered_path_claude

distinct="$(LC_ALL=C sort -u "$FOUND_PATHS" | grep -c . || true)"
if [ "$distinct" -gt 1 ]; then
  bad "mcp parity: providers read different servers: $(LC_ALL=C sort -u "$FOUND_PATHS" | tr '\n' ' ')"
elif [ "$distinct" -eq 1 ]; then
  ok "mcp parity: every registered provider reads the same server"
else
  note "mcp parity: no provider registrations on this machine"
fi

# ---------------------------------------------------------------------------

if [ "$FAILS" -gt 0 ]; then
  printf 'provider-contract: FAILED (%d of %d checks, %d notes)\n' \
    "$FAILS" "$CHECKS" "$NOTES" >&2
  exit 1
fi
printf 'provider-contract: ok (%d checks, %d notes)\n' "$CHECKS" "$NOTES"
