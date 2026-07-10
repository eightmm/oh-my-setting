#!/usr/bin/env bash
set -euo pipefail

# Install the repo-local oh-my-setting Codex plugin so Codex gets the same
# prompt skill hints and Stop-hook turn guard as Claude Code.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
MARKETPLACE_FILE="${OMS_CODEX_MARKETPLACE_FILE:-$ROOT/.agents/plugins/marketplace.json}"
MARKETPLACE_ROOT="${OMS_CODEX_MARKETPLACE_ROOT:-$ROOT}"
PLUGIN_NAME="oh-my-setting"
REMOVE=0
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

if [ "${OMS_INSTALL_LOCK_HELD:-0}" != "1" ]; then
  oms_with_file_lock "$(oms_install_receipt_path)" \
    env OMS_INSTALL_LOCK_HELD=1 bash "$ROOT/scripts/install-codex-plugin.sh" "$@"
  exit $?
fi

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

oms_install_require_owner "$ROOT" "modify the Codex plugin" || exit 1

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
case "$MARKETPLACE_NAME" in
  *[!A-Za-z0-9._-]*|"") fail "unsafe marketplace name: $MARKETPLACE_NAME" ;;
esac

PLUGIN_VERSION="$(oms_install_plugin_version "$ROOT")"
PLUGIN_HASH="$(oms_install_plugin_hash "$ROOT")"
case "$PLUGIN_VERSION" in
  *[!A-Za-z0-9._+-]*|"") fail "unsafe plugin version: $PLUGIN_VERSION" ;;
esac
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
PLUGIN_CACHE="$CODEX_HOME_DIR/plugins/cache/$MARKETPLACE_NAME/$PLUGIN_NAME/$PLUGIN_VERSION"

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

plugin_is_installed() {
  codex plugin list --json 2>/dev/null |
    python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(1)
target = sys.argv[1]
sys.exit(0 if any(p.get("pluginId") == target and p.get("installed") for p in data.get("installed", [])) else 1)
' "$PLUGIN_NAME@$MARKETPLACE_NAME" 2>/dev/null
}

write_source_marker() {
  [ "$DRY_RUN" = "1" ] && return 0
  local plugin_root
  plugin_root="${1:-}"
  if [ -z "$plugin_root" ] && plugin_is_installed && [ -d "$PLUGIN_CACHE" ]; then
    plugin_root="$PLUGIN_CACHE"
  fi
  [ -n "$plugin_root" ] || plugin_root="$(installed_plugin_root)"
  [ -n "$plugin_root" ] || return 0
  [ -d "$plugin_root" ] || return 0
  # Never add cache metadata to the checked-in source tree.
  [ "$(cd "$plugin_root" 2>/dev/null && pwd -P)" != "$ROOT/plugins/oh-my-setting" ] || return 0
  oms_install_atomic_text "$ROOT" "$plugin_root/.oh-my-setting-source-root"
  oms_install_atomic_text "$PLUGIN_HASH" "$plugin_root/.oh-my-setting-source-sha256"
}

refresh_stale_cache() {
  local actual_hash

  plugin_is_installed || return 0
  [ -d "$PLUGIN_CACHE" ] || return 0
  actual_hash="$(oms_install_tree_hash "$PLUGIN_CACHE")"
  [ "$actual_hash" != "$PLUGIN_HASH" ] || return 0

  echo "codex-plugin: refreshing stale cache for $PLUGIN_NAME@$PLUGIN_VERSION"
  run_cmd codex plugin remove "$PLUGIN_NAME@$MARKETPLACE_NAME" || true
  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove cache: $PLUGIN_CACHE"
  else
    # Every path component above is fixed or restricted to a safe basename.
    rm -rf "$PLUGIN_CACHE"
  fi
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

refresh_stale_cache
install_plugin
if [ "$DRY_RUN" = "1" ]; then
  echo "codex-plugin: would install $PLUGIN_NAME@$MARKETPLACE_NAME"
else
  echo "codex-plugin: installed $PLUGIN_NAME@$MARKETPLACE_NAME"
fi
