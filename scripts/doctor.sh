#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0
REQUIRE_TOOLS="${OH_MY_SETTING_REQUIRE_TOOLS:-1}"

check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: command $1"
  else
    echo "missing: command $1"
    FAILED=1
  fi
}

check_optional_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "ok: optional command $1"
  else
    echo "optional missing: command $1"
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

if [ "$REQUIRE_TOOLS" != "0" ]; then
  check_cmd node
  check_cmd npm
  check_cmd uv
  check_cmd claude
  check_cmd codex
  check_cmd gemini
  check_cmd pi
fi

check_optional_cmd sbatch
check_optional_cmd srun
check_optional_cmd squeue
check_optional_cmd sinfo
check_optional_cmd scancel

check_path "$ROOT/AGENTS.md"
check_path "$ROOT/skills.manifest.json"
check_path "$HOME/.codex/AGENTS.md"
check_path "$HOME/.codex/skills/oh-my-setting"
check_path "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md"
check_path "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills/oh-my-setting"

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"
