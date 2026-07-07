#!/usr/bin/env bash
set -euo pipefail

# Install the repo-local oh-my-setting Codex plugin so Codex gets the same
# prompt skill hints and Stop-hook turn guard as Claude Code.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MARKETPLACE_FILE="${OMS_CODEX_MARKETPLACE_FILE:-$ROOT/.agents/plugins/marketplace.json}"
MARKETPLACE_ROOT="${OMS_CODEX_MARKETPLACE_ROOT:-$ROOT}"
PLUGIN_NAME="oh-my-setting"
REMOVE=0
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: install-codex-plugin.sh [--remove]

Register oh-my-setting's local Codex plugin marketplace and install the
oh-my-setting plugin. The plugin adds UserPromptSubmit skill hints and a Stop
turn guard. --remove removes only this plugin and marketplace entry.

Environment:
  OH_MY_SETTING_DRY_RUN=1        Preview commands without changing Codex config.
  OMS_CODEX_MARKETPLACE_FILE=PATH Override marketplace.json path.
  OMS_CODEX_MARKETPLACE_ROOT=PATH Override marketplace root path.
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

command -v codex >/dev/null 2>&1 || fail "codex command is required"
[ -f "$MARKETPLACE_FILE" ] || fail "missing marketplace: $MARKETPLACE_FILE"
[ -d "$MARKETPLACE_ROOT" ] || fail "missing marketplace root: $MARKETPLACE_ROOT"

MARKETPLACE_NAME="$(python3 - "$MARKETPLACE_FILE" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh)["name"])
PY
)" || fail "failed to read marketplace name"
[ -n "$MARKETPLACE_NAME" ] || fail "marketplace name is empty"

run_cmd() {
  if [ "$DRY_RUN" = "1" ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

marketplace_root() {
  codex plugin marketplace list 2>/dev/null |
    awk -v name="$MARKETPLACE_NAME" '$1 == name { print $2; exit }'
}

installed_plugin_root() {
  codex plugin list --json 2>/dev/null |
    python3 -c '
import json, sys
target = sys.argv[1]
raw = sys.stdin.read()
if not raw.strip():
    sys.exit(0)
try:
    data = json.loads(raw)
except Exception:
    sys.exit(0)
for plugin in data.get("installed", []):
    if plugin.get("pluginId") == target:
        path = plugin.get("source", {}).get("path", "")
        if path:
            print(path)
        break
' "$PLUGIN_NAME@$MARKETPLACE_NAME" 2>/dev/null || true
}

write_source_marker() {
  [ "$DRY_RUN" = "1" ] && return 0
  local plugin_root
  plugin_root="${1:-}"
  if [ -z "$plugin_root" ]; then
    plugin_root="$(installed_plugin_root)"
  fi
  [ -n "$plugin_root" ] || return 0
  [ -d "$plugin_root" ] || return 0
  printf '%s\n' "$ROOT" > "$plugin_root/.oh-my-setting-source-root" || true
}

install_plugin() {
  if [ "$DRY_RUN" = "1" ]; then
    run_cmd codex plugin add "$PLUGIN_NAME@$MARKETPLACE_NAME"
    return 0
  fi

  local out
  local plugin_root
  out="$(codex plugin add "$PLUGIN_NAME@$MARKETPLACE_NAME" 2>&1)" || {
    printf '%s\n' "$out" >&2
    return 1
  }
  printf '%s\n' "$out"
  plugin_root="$(printf '%s\n' "$out" | awk -F': ' '/^Installed plugin root:/ { print $2; exit }')"
  write_source_marker "$plugin_root"
}

if [ "$REMOVE" = "1" ]; then
  run_cmd codex plugin remove "$PLUGIN_NAME@$MARKETPLACE_NAME" || true
  run_cmd codex plugin marketplace remove "$MARKETPLACE_NAME" || true
  echo "codex-plugin: removed $PLUGIN_NAME@$MARKETPLACE_NAME"
  exit 0
fi

current_root="$(marketplace_root || true)"
expected_a="$MARKETPLACE_FILE"
expected_b="$ROOT"
expected_c="$MARKETPLACE_ROOT"

if [ -n "$current_root" ] &&
   [ "$current_root" != "$expected_a" ] &&
   [ "$current_root" != "$expected_b" ] &&
   [ "$current_root" != "$expected_c" ]; then
  echo "codex-plugin: refreshing moved marketplace $MARKETPLACE_NAME ($current_root -> $MARKETPLACE_ROOT)"
  run_cmd codex plugin marketplace remove "$MARKETPLACE_NAME" || true
  current_root=""
fi

if [ -z "$current_root" ]; then
  run_cmd codex plugin marketplace add "$MARKETPLACE_ROOT"
else
  echo "codex-plugin: marketplace already registered ($MARKETPLACE_NAME)"
fi

install_plugin
if [ "$DRY_RUN" = "1" ]; then
  echo "codex-plugin: would install $PLUGIN_NAME@$MARKETPLACE_NAME"
else
  echo "codex-plugin: installed $PLUGIN_NAME@$MARKETPLACE_NAME"
fi
