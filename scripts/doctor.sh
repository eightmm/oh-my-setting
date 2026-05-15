#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

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

check_custom_skills() {
  local target_root="$1"
  local skill
  local name

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    check_path "$target_root/$name/SKILL.md"
  done
}

load_user_tool_paths

check_cmd git
check_cmd curl
check_cmd node
check_cmd npm
check_cmd uv
check_cmd claude
check_cmd codex
check_cmd gemini
check_cmd pi

check_optional_cmd sbatch
check_optional_cmd srun
check_optional_cmd squeue
check_optional_cmd sinfo
check_optional_cmd scancel
check_optional_cmd gh

check_path "$ROOT/AGENTS.md"
check_path "$ROOT/skills.manifest.json"
check_path "$HOME/.codex/AGENTS.md"
check_path "$HOME/.claude/CLAUDE.md"
check_path "$HOME/.gemini/GEMINI.md"
check_custom_skills "$HOME/.codex/skills"
check_custom_skills "$HOME/.claude/skills"
check_custom_skills "$HOME/.agents/skills"
check_path "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md"
check_custom_skills "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"

if [ "$FAILED" -ne 0 ]; then
  echo "doctor: failed"
  exit 1
fi

echo "doctor: ok"
