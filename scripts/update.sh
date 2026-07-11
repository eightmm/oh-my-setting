#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKIP_TOOLS="${OH_MY_SETTING_UPDATE_SKIP_TOOLS:-0}"
SKIP_DOCTOR="${OH_MY_SETTING_UPDATE_SKIP_DOCTOR:-0}"
AUTO_UPDATE="${OH_MY_SETTING_AUTO_UPDATE:-1}"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

usage() {
  cat <<'EOF'
Usage: update.sh [--no-tools] [--no-doctor] [-h|--help]

Update the local oh-my-setting checkout, refresh symlinks, and re-run doctor.

Options:
  --no-tools    Skip CLI tool reinstall step.
  --no-doctor   Skip post-update doctor run.

Environment:
  OH_MY_SETTING_UPDATE_SKIP_TOOLS=1   Same as --no-tools.
  OH_MY_SETTING_UPDATE_SKIP_DOCTOR=1  Same as --no-doctor.
  OH_MY_SETTING_CODEX_PLUGIN=0        Skip Codex plugin hook refresh.
  OH_MY_SETTING_AUTO_UPDATE=0         Skip auto-update trigger refresh.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
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
fi

"$ROOT/scripts/link.sh"

# Refresh the Claude hook registration so a moved checkout path heals.
if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ] && [ -x "$ROOT/scripts/install-claude-hooks.sh" ]; then
  "$ROOT/scripts/install-claude-hooks.sh" ||
    echo "warning: claude hook refresh failed (update continues)" >&2
fi

if [ "${OH_MY_SETTING_CODEX_PLUGIN:-1}" = "1" ] && [ -x "$ROOT/scripts/install-codex-plugin.sh" ]; then
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
