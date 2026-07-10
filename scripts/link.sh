#!/usr/bin/env bash
set -euo pipefail

# Symlink rules, skills, prompts/workflows, and the oms dispatcher into all three agent CLIs.

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
  local source
  local enabled_sources

  mkdir -p "$target_root"
  oms_ops_clean_backup_skill_links "$target_root" 0

  enabled_sources="$(python3 - "$ROOT/skills.manifest.json" <<'PY'
import json
import sys
for skill in json.load(open(sys.argv[1], encoding="utf-8")).get("skills", []):
    source = str(skill.get("source", ""))
    if skill.get("enabled") is True and source.startswith("custom-skills/"):
        print(source)
PY
)"
  while IFS= read -r source; do
    [ -n "$source" ] || continue
    skill="$ROOT/$source"
    name="$(basename "$skill")"
    link_target "$skill" "$target_root/$name"
  done <<< "$enabled_sources"

  # Remove only disabled links owned by this checkout. User directories and
  # foreign links are preserved for explicit cleanup/recovery.
  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    source="${skill#"$ROOT"/}"
    printf '%s\n' "$enabled_sources" | grep -Fxq "$source" && continue
    name="$(basename "$skill")"
    if [ -L "$target_root/$name" ] && [ "$(readlink "$target_root/$name")" = "$skill" ]; then
      rm -f "$target_root/$name"
      echo "unlinked disabled skill $target_root/$name"
    fi
  done
}

if ! skill_validation="$("$ROOT/scripts/install-skills.sh" 2>&1)"; then
  printf '%s\n' "$skill_validation" >&2
  exit 1
fi
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
# The one-name dispatcher: `oms <tool>` from any agent CLI, no script paths.
link_target "$ROOT/scripts/oms" "$HOME/.local/bin/oms"
