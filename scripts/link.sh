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
    rm -f "$target"
    return 0
  fi

  if [ -e "$target" ]; then
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
  local current

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  if legacy_source_matches "$current" "custom-skills"; then
    rm -f "$target"
    echo "removed legacy $target"
  fi
}

legacy_source_matches() {
  local current="$1"
  local rel="$2"
  local old_root="$HOME/.oh-my-setting"

  [ "$current" = "$ROOT/$rel" ] || [ "$current" = "$old_root/$rel" ]
}

remove_legacy_link() {
  local target="$1"
  local rel="$2"
  local current

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  if legacy_source_matches "$current" "$rel"; then
    rm -f "$target"
    echo "removed legacy $target"
  fi
}

remove_legacy_skill_links() {
  local target_root="$1"
  local skill
  local name

  [ -d "$target_root" ] || return 0
  find "$target_root" -maxdepth 1 -type l -name "*.backup.*" -exec rm -f {} +

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    remove_legacy_link "$target_root/$name" "custom-skills/$name"
  done
}

cleanup_legacy_links() {
  remove_legacy_link "$HOME/.gemini/GEMINI.md" "AGENTS.md"
  remove_legacy_link "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md" "AGENTS.md"
  remove_legacy_skill_links "$HOME/.agents/skills"
  remove_legacy_skill_links "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"

  remove_old_skill_group "$HOME/.codex/skills/oh-my-setting"
  remove_old_skill_group "$HOME/.claude/skills/oh-my-setting"
}

link_skills() {
  local target_root="$1"
  local skill
  local name

  mkdir -p "$target_root"
  find "$target_root" -maxdepth 1 -type l -name "*.backup.*" -exec rm -f {} +

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    link_dir "$skill" "$target_root/$name"
  done
}

cleanup_legacy_links

link_file "$ROOT/AGENTS.md" "$HOME/.codex/AGENTS.md"
link_file "$ROOT/AGENTS.md" "$HOME/.claude/CLAUDE.md"
# Antigravity global customizations root: rules at ~/.gemini/AGENTS.md,
# skills under ~/.gemini/antigravity/skills.
link_file "$ROOT/AGENTS.md" "$HOME/.gemini/AGENTS.md"
link_skills "$HOME/.codex/skills"
link_skills "$HOME/.claude/skills"
link_skills "$HOME/.gemini/antigravity/skills"
link_dir "$ROOT/prompts" "$HOME/.oh-my-setting-prompts"
link_dir "$ROOT/workflows" "$HOME/.oh-my-setting-workflows"
