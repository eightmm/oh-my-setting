#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN=1
REMOVED=0
WOULD_REMOVE=0

usage() {
  cat <<'EOF'
Usage: cleanup.sh [--dry-run|--apply] [-h|--help]

Clean safe oh-my-setting install leftovers. Default is --dry-run.
Removes only known oh-my-setting legacy symlinks and backup skill symlinks;
never removes regular files, third-party plugins, caches, or directories.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --apply)
      DRY_RUN=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

legacy_source_matches() {
  local current="$1"
  local rel="$2"
  local old_root="$HOME/.oh-my-setting"

  [ "$current" = "$ROOT/$rel" ] || [ "$current" = "$old_root/$rel" ]
}

remove_symlink() {
  local target="$1"
  local reason="$2"

  [ -L "$target" ] || return 0
  if [ "$DRY_RUN" -eq 1 ]; then
    printf 'would remove: %s (%s)\n' "$target" "$reason"
    WOULD_REMOVE=$((WOULD_REMOVE + 1))
  else
    rm -f "$target"
    printf 'removed: %s (%s)\n' "$target" "$reason"
    REMOVED=$((REMOVED + 1))
  fi
}

remove_legacy_link() {
  local target="$1"
  local rel="$2"
  local current

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  if legacy_source_matches "$current" "$rel"; then
    remove_symlink "$target" "legacy oh-my-setting link"
  fi
}

clean_backup_skill_links() {
  local target_root="$1"
  local target

  [ -d "$target_root" ] || return 0
  while IFS= read -r -d '' target; do
    remove_symlink "$target" "backup skill symlink"
  done < <(find "$target_root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)
}

clean_legacy_skill_links() {
  local target_root="$1"
  local skill
  local name

  [ -d "$target_root" ] || return 0
  clean_backup_skill_links "$target_root"

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    remove_legacy_link "$target_root/$name" "custom-skills/$name"
  done
}

printf '# oh-my-setting cleanup\n\n'
if [ "$DRY_RUN" -eq 1 ]; then
  printf 'mode: dry-run\n\n'
else
  printf 'mode: apply\n\n'
fi

clean_backup_skill_links "$HOME/.codex/skills"
clean_backup_skill_links "$HOME/.claude/skills"
clean_backup_skill_links "$HOME/.gemini/antigravity/skills"
remove_legacy_link "$HOME/.codex/skills/oh-my-setting" "custom-skills"
remove_legacy_link "$HOME/.claude/skills/oh-my-setting" "custom-skills"
remove_legacy_link "$HOME/.gemini/GEMINI.md" "AGENTS.md"
remove_legacy_link "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/AGENTS.md" "AGENTS.md"
clean_legacy_skill_links "$HOME/.agents/skills"
clean_legacy_skill_links "${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}/skills"

if [ "$DRY_RUN" -eq 1 ]; then
  printf '\ncleanup: %s removable item(s) found\n' "$WOULD_REMOVE"
else
  printf '\ncleanup: removed %s item(s)\n' "$REMOVED"
  "$ROOT/scripts/skill-doctor.sh"
fi
