#!/usr/bin/env bash
set -euo pipefail

# Package and import the Antigravity plugin: `agy plugin install` copies the
# plugin directory verbatim into ~/.gemini/config/plugins/<name>, so the
# installer bakes this checkout's absolute paths into a generated copy
# instead of shipping placeholders no expansion would fill.
#
# The plugin carries mcpServers only. Antigravity 1.1.9 accepts a hooks.json
# component but fires no hook event in headless (-p) runs — verified by
# probe — and this harness ships only surfaces it can verify. The MCP server
# is also how agy sessions reach journal/handoff/fail-ledger state that the
# prompt-submit hooks give Claude and Codex.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
MODE="${OH_MY_SETTING_AGY_PLUGIN:-auto}"
REMOVE=0
AGY_BIN="${OMS_AGY_BIN:-agy}"

usage() {
  cat <<'EOF'
Usage: install-agy-plugin.sh [--remove]

Import the oh-my-setting plugin (harness-state MCP server) into Antigravity.
OH_MY_SETTING_AGY_PLUGIN=0 skips, 1 requires agy, auto (default) installs
when agy is on PATH. --remove uninstalls the plugin.
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

case "$MODE" in
  0)
    [ "$REMOVE" = "1" ] || { echo "agy-plugin: skipped (OH_MY_SETTING_AGY_PLUGIN=0)"; exit 0; }
    ;;
  1|auto) ;;
  *) fail "OH_MY_SETTING_AGY_PLUGIN must be 0, 1, or auto" ;;
esac

if ! command -v "$AGY_BIN" >/dev/null 2>&1; then
  [ "$MODE" = "1" ] && fail "agy is not installed"
  echo "agy-plugin: note: agy CLI absent; skipped"
  exit 0
fi

if [ "$REMOVE" = "1" ]; then
  "$AGY_BIN" plugin uninstall oh-my-setting >/dev/null 2>&1 || true
  echo "agy-plugin: uninstalled (if present)"
  exit 0
fi

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
[ -f "$ROOT/scripts/oms-mcp-server.py" ] || fail "oms-mcp-server.py not found under $ROOT"
[ -f "$ROOT/plugins/antigravity/plugin.json" ] || fail "plugin template missing"

BUILD="$(mktemp -d "${TMPDIR:-/tmp}/oms-agy-plugin.XXXXXX")"
trap 'rm -rf "$BUILD"' EXIT

OMS_AP_ROOT="$ROOT" OMS_AP_BUILD="$BUILD" python3 - <<'PY'
import json, os

root = os.environ["OMS_AP_ROOT"]
build = os.environ["OMS_AP_BUILD"]
with open(os.path.join(root, "plugins/antigravity/plugin.json"), encoding="utf-8") as fh:
    plugin = json.load(fh)
try:
    with open(os.path.join(root, "VERSION"), encoding="utf-8") as fh:
        plugin["version"] = fh.read().strip() or plugin["version"]
except OSError:
    pass
with open(os.path.join(build, "plugin.json"), "w", encoding="utf-8") as fh:
    json.dump(plugin, fh, ensure_ascii=False, indent=2)
    fh.write("\n")
mcp = {
    "mcpServers": {
        "oh-my-setting": {
            "command": "python3",
            "args": [os.path.join(root, "scripts", "oms-mcp-server.py")],
        }
    }
}
with open(os.path.join(build, "mcp_config.json"), "w", encoding="utf-8") as fh:
    json.dump(mcp, fh, ensure_ascii=False, indent=2)
    fh.write("\n")
PY

if out="$("$AGY_BIN" plugin install "$BUILD" 2>&1)"; then
  echo "agy-plugin: installed oh-my-setting (harness-state MCP server)"
else
  echo "warn: agy plugin install failed:" >&2
  printf '%s\n' "$out" >&2
  [ "$MODE" = "1" ] && exit 1
  exit 0
fi
