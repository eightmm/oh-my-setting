#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${OH_MY_SETTING_REPO_URL:-git@github.com:eightmm/oh-my-setting.git}"
DEST="${OH_MY_SETTING_DIR:-$HOME/.oh-my-setting}"

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is required" >&2
  exit 1
fi

if [ -d "$DEST/.git" ]; then
  git -C "$DEST" pull --ff-only
else
  mkdir -p "$(dirname "$DEST")"
  git clone "$REPO_URL" "$DEST"
fi

"$DEST/scripts/link.sh"
"$DEST/scripts/doctor.sh"
