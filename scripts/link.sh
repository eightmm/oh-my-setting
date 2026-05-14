#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date +%Y%m%d%H%M%S)"

backup_if_needed() {
  local target="$1"
  local source="$2"

  if [ -L "$target" ]; then
    local current
    current="$(readlink "$target")"
    if [ "$current" = "$source" ]; then
      return 0
    fi
  fi

  if [ -e "$target" ] || [ -L "$target" ]; then
    mv "$target" "$target.backup.$STAMP"
  fi
}

link_file() {
  local source="$1"
  local target="$2"

  mkdir -p "$(dirname "$target")"
  backup_if_needed "$target" "$source"
  ln -sfn "$source" "$target"
  echo "linked $target -> $source"
}

link_dir() {
  local source="$1"
  local target="$2"

  mkdir -p "$(dirname "$target")"
  backup_if_needed "$target" "$source"
  ln -sfn "$source" "$target"
  echo "linked $target -> $source"
}

link_file "$ROOT/AGENTS.md" "$HOME/.codex/AGENTS.md"
link_file "$ROOT/AGENTS.md" "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md"
link_dir "$ROOT/custom-skills" "$HOME/.codex/skills/oh-my-setting"
link_dir "$ROOT/custom-skills" "$HOME/.agents/skills/oh-my-setting"
link_dir "$ROOT/custom-skills" "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills/oh-my-setting"
link_dir "$ROOT/prompts" "$HOME/.oh-my-setting-prompts"
link_dir "$ROOT/workflows" "$HOME/.oh-my-setting-workflows"
