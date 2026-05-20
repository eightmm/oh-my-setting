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

remove_old_skill_group() {
  local target="$1"

  if [ -L "$target" ] && [ "$(readlink "$target")" = "$ROOT/custom-skills" ]; then
    rm -f "$target"
  fi
}

link_skills() {
  local target_root="$1"
  local skill
  local name

  mkdir -p "$target_root"

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    link_dir "$skill" "$target_root/$name"
  done
}

link_file "$ROOT/AGENTS.md" "$HOME/.codex/AGENTS.md"
link_file "$ROOT/AGENTS.md" "$HOME/.claude/CLAUDE.md"
link_file "$ROOT/AGENTS.md" "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md"
remove_old_skill_group "$HOME/.codex/skills/oh-my-setting"
remove_old_skill_group "$HOME/.claude/skills/oh-my-setting"
remove_old_skill_group "$HOME/.agents/skills/oh-my-setting"
remove_old_skill_group "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills/oh-my-setting"
link_skills "$HOME/.codex/skills"
link_skills "$HOME/.claude/skills"
link_skills "$HOME/.agents/skills"
link_skills "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"
link_dir "$ROOT/prompts" "$HOME/.oh-my-setting-prompts"
link_dir "$ROOT/workflows" "$HOME/.oh-my-setting-workflows"
