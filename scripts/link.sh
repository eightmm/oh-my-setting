#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date +%Y%m%d%H%M%S)"
# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"

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

link_target() {
  local source="$1"
  local target="$2"

  mkdir -p "$(dirname "$target")"
  backup_if_needed "$target" "$source"
  ln -sfn "$source" "$target"
  echo "linked $target -> $source"
}

link_skills() {
  local target_root="$1"
  local skill
  local name

  mkdir -p "$target_root"
  oms_ops_clean_backup_skill_links "$target_root" 0

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    link_target "$skill" "$target_root/$name"
  done
}

oms_ops_cleanup_legacy_links 0

link_target "$ROOT/AGENTS.md" "$HOME/.codex/AGENTS.md"
link_target "$ROOT/AGENTS.md" "$HOME/.claude/CLAUDE.md"
# Antigravity global customizations root: rules at ~/.gemini/AGENTS.md,
# skills under ~/.gemini/antigravity/skills.
link_target "$ROOT/AGENTS.md" "$HOME/.gemini/AGENTS.md"
link_skills "$HOME/.codex/skills"
link_skills "$HOME/.claude/skills"
link_skills "$HOME/.gemini/antigravity/skills"
link_target "$ROOT/prompts" "$HOME/.oh-my-setting-prompts"
link_target "$ROOT/workflows" "$HOME/.oh-my-setting-workflows"
