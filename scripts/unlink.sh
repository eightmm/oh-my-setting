#!/usr/bin/env bash
set -euo pipefail

# Remove the symlinks link.sh created and restore the backups they replaced.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
usage: unlink.sh

Remove oh-my-setting symlinks and restore the newest matching
*.backup.TIMESTAMP file when one exists.

Set OH_MY_SETTING_DRY_RUN=1 to preview changes.
EOF
}

latest_backup() {
  local target="$1"
  local dir
  local base

  dir="$(dirname "$target")"
  base="$(basename "$target")"

  find "$dir" -maxdepth 1 -name "$base.backup.*" -print 2>/dev/null |
    LC_ALL=C sort |
    tail -n 1
}

unlink_and_restore() {
  local target="$1"
  local source="$2"
  local backup

  if [ ! -L "$target" ] || [ "$(readlink "$target")" != "$source" ]; then
    echo "skip: $target"
    return 0
  fi

  backup="$(latest_backup "$target")"

  if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$backup" ]; then
      echo "would remove $target and restore $backup"
    else
      echo "would remove $target"
    fi
    return 0
  fi

  rm -f "$target"
  echo "removed $target"

  if [ -n "$backup" ]; then
    mv "$backup" "$target"
    echo "restored $target from $backup"
  fi
}

unlink_skills() {
  local target_root="$1"
  local skill
  local name

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    unlink_and_restore "$target_root/$name" "$skill"
  done
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
  "")
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

unlink_and_restore "$HOME/.codex/AGENTS.md" "$ROOT/AGENTS.md"
unlink_and_restore "$HOME/.claude/CLAUDE.md" "$ROOT/AGENTS.md"
unlink_and_restore "$HOME/.gemini/AGENTS.md" "$ROOT/AGENTS.md"
unlink_skills "$HOME/.codex/skills"
unlink_skills "$HOME/.claude/skills"
unlink_skills "$HOME/.gemini/antigravity/skills"
# Legacy cleanup: pi support was removed from defaults; still unlink old installs.
unlink_and_restore "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md" "$ROOT/AGENTS.md"
unlink_skills "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"
unlink_and_restore "$HOME/.oh-my-setting-prompts" "$ROOT/prompts"
unlink_and_restore "$HOME/.oh-my-setting-workflows" "$ROOT/workflows"
unlink_and_restore "$HOME/.local/bin/oms" "$ROOT/scripts/oms"
