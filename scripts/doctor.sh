#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0

check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: command $1"
  else
    echo "missing: command $1"
    FAILED=1
  fi
}

check_path() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    echo "ok: $1"
  else
    echo "missing: $1"
    FAILED=1
  fi
}

check_cmd git
check_cmd curl

check_path "$ROOT/AGENTS.md"
check_path "$ROOT/skills.manifest.json"
check_path "$HOME/.codex/AGENTS.md"
check_path "$HOME/.codex/skills/oh-my-setting"

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"

