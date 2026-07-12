#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKIP_TOOLS="${OH_MY_SETTING_UPDATE_SKIP_TOOLS:-1}"
SKIP_DOCTOR="${OH_MY_SETTING_UPDATE_SKIP_DOCTOR:-0}"
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-0}"
CODEX_PLUGIN="${OH_MY_SETTING_CODEX_PLUGIN:-auto}"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

usage() {
  cat <<'EOF'
Usage: update.sh [--tools] [--no-tools] [--no-doctor] [-h|--help]

Update the local oh-my-setting checkout, refresh symlinks, and re-run doctor.

Options:
  --tools       Refresh Node/uv/provider tools after updating.
  --no-tools    Skip tool refresh (default; retained for compatibility).
  --no-doctor   Skip post-update doctor run.

Environment:
  OH_MY_SETTING_UPDATE_SKIP_TOOLS=0   Same as --tools.
  OH_MY_SETTING_UPDATE_SKIP_TOOLS=1   Same as --no-tools (default).
  OH_MY_SETTING_UPDATE_SKIP_DOCTOR=1  Same as --no-doctor.
  OH_MY_SETTING_CODEX_PLUGIN=0|1|auto Skip, require, or refresh an installed Codex plugin.
  OH_MY_SETTING_AUTO_UPDATE=1         Refresh the auto-update trigger.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tools)
      SKIP_TOOLS=0
      shift
      ;;
    --no-tools)
      SKIP_TOOLS=1
      shift
      ;;
    --no-doctor)
      SKIP_DOCTOR=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

oms_install_require_owner "$ROOT" "update the install" || exit 1

case "$CODEX_PLUGIN" in
  auto)
    if command -v codex >/dev/null 2>&1 &&
       codex plugin list --json 2>/dev/null |
         python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; sys.exit(0 if any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null; then
      CODEX_PLUGIN=1
    else
      CODEX_PLUGIN=0
    fi
    ;;
  0|1) ;;
  *) echo "error: OH_MY_SETTING_CODEX_PLUGIN must be 0, 1, or auto" >&2; exit 2 ;;
esac
export OH_MY_SETTING_CODEX_PLUGIN="$CODEX_PLUGIN"

if [ ! -d "$ROOT/.git" ]; then
  echo "error: $ROOT is not a git checkout" >&2
  exit 1
fi

current="$(git -C "$ROOT" rev-parse --short HEAD)"
echo "current: $current"

git -C "$ROOT" pull --ff-only

new="$(git -C "$ROOT" rev-parse --short HEAD)"
if [ "$current" = "$new" ]; then
  echo "already up to date: $new"
else
  echo "updated: $current -> $new"
fi

if [ "$SKIP_TOOLS" != "1" ] && [ -x "$ROOT/scripts/install-tools.sh" ]; then
  "$ROOT/scripts/install-tools.sh"
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"
else
  export OH_MY_SETTING_REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-0}"
fi

"$ROOT/scripts/link.sh"

# Refresh the Claude hook registration so a moved checkout path heals.
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ] && [ -x "$ROOT/scripts/install-claude-hooks.sh" ]; then
  "$ROOT/scripts/install-claude-hooks.sh" ||
    echo "warning: claude hook refresh failed (update continues)" >&2
fi

if [ "$CODEX_PLUGIN" = "1" ] && [ -x "$ROOT/scripts/install-codex-plugin.sh" ]; then
  "$ROOT/scripts/install-codex-plugin.sh"
fi

if [ "$SKIP_DOCTOR" != "1" ]; then
  "$ROOT/scripts/doctor.sh"
fi

if [ "$AUTO_UPDATE" = "1" ] && [ -x "$ROOT/scripts/install-autoupdate.sh" ]; then
  "$ROOT/scripts/install-autoupdate.sh"
elif [ "$AUTO_UPDATE" != "0" ]; then
  echo "error: OH_MY_SETTING_AUTO_UPDATE must be 0 or 1" >&2
  exit 2
fi

echo "update: ok"
