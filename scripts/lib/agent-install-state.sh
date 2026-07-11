#!/usr/bin/env bash
# shellcheck shell=bash

# Shared checks and cleanup for installed agent links/skills.
if [ -z "${ROOT:-}" ]; then
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi

# Exported state read by thin wrapper scripts.
# shellcheck disable=SC2034
OMS_OPS_REMOVED=0
# shellcheck disable=SC2034
OMS_OPS_WOULD_REMOVE=0
# shellcheck disable=SC2034
OMS_OPS_FAILED=0

oms_ops_reset_cleanup_counters() {
  OMS_OPS_REMOVED=0
  OMS_OPS_WOULD_REMOVE=0
}

oms_ops_reset_check_state() {
  OMS_OPS_FAILED=0
}

oms_ops_legacy_source_matches() {
  local current="$1"
  local rel="$2"
  local old_root="$HOME/.oh-my-setting"

  [ "$current" = "$ROOT/$rel" ] || [ "$current" = "$old_root/$rel" ]
}

oms_ops_remove_symlink() {
  local target="$1"
  local reason="$2"
  local dry_run="${3:-0}"

  [ -L "$target" ] || return 0
  if [ "$dry_run" = "1" ]; then
    printf 'would remove: %s (%s)\n' "$target" "$reason"
    OMS_OPS_WOULD_REMOVE=$((OMS_OPS_WOULD_REMOVE + 1))
  else
    rm -f "$target"
    printf 'removed: %s (%s)\n' "$target" "$reason"
    OMS_OPS_REMOVED=$((OMS_OPS_REMOVED + 1))
  fi
}

oms_ops_remove_legacy_link() {
  local target="$1"
  local rel="$2"
  local dry_run="${3:-0}"
  local current

  [ -L "$target" ] || return 0
  current="$(readlink "$target")"
  if oms_ops_legacy_source_matches "$current" "$rel"; then
    oms_ops_remove_symlink "$target" "legacy oh-my-setting link" "$dry_run"
  fi
}

oms_ops_clean_backup_skill_links() {
  local target_root="$1"
  local dry_run="${2:-0}"
  local target

  [ -d "$target_root" ] || return 0
  while IFS= read -r -d '' target; do
    case "$target" in
      *.backup.[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9])
        continue
        ;;
    esac
    oms_ops_remove_symlink "$target" "backup skill symlink" "$dry_run"
  done < <(find "$target_root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)
}

oms_ops_clean_legacy_skill_links() {
  local target_root="$1"
  local dry_run="${2:-0}"
  local skill
  local name

  [ -d "$target_root" ] || return 0
  oms_ops_clean_backup_skill_links "$target_root" "$dry_run"

  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    [ -f "$skill/SKILL.md" ] || continue
    name="$(basename "$skill")"
    oms_ops_remove_legacy_link "$target_root/$name" "custom-skills/$name" "$dry_run"
  done
}

oms_ops_cleanup_legacy_links() {
  local dry_run="${1:-0}"
  local pi_agent_dir="${PI_CODING_AGENT_DIR:-$HOME/.pi/agent}"

  oms_ops_reset_cleanup_counters
  oms_ops_clean_backup_skill_links "$HOME/.codex/skills" "$dry_run"
  oms_ops_clean_backup_skill_links "$HOME/.claude/skills" "$dry_run"
  oms_ops_clean_backup_skill_links "$HOME/.gemini/antigravity/skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.codex/skills/oh-my-setting" "custom-skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.claude/skills/oh-my-setting" "custom-skills" "$dry_run"
  oms_ops_remove_legacy_link "$HOME/.gemini/GEMINI.md" "AGENTS.md" "$dry_run"
  oms_ops_remove_legacy_link "$pi_agent_dir/AGENTS.md" "AGENTS.md" "$dry_run"
  oms_ops_clean_legacy_skill_links "$HOME/.agents/skills" "$dry_run"
  oms_ops_clean_legacy_skill_links "$pi_agent_dir/skills" "$dry_run"
}

oms_ops_extract_skill_name() {
  local file="$1"
  awk -F: '
    /^name:[[:space:]]*/ {
      sub(/^[[:space:]]+/, "", $2)
      sub(/[[:space:]]+$/, "", $2)
      print $2
      exit
    }
  ' "$file"
}

oms_ops_check_skill_root() {
  local label="$1"
  local root="$2"
  local file
  local name
  local total=0
  local issues=0
  local backup
  local prev
  # Newline-delimited "name<TAB>path" records: bash 3.2 (macOS) has no
  # associative arrays, and `declare -A` aborts the whole check under set -e.
  local first_seen=""

  printf '## %s\n\n' "$label"

  if [ ! -d "$root" ]; then
    printf 'skip: %s missing\n\n' "$root"
    return 0
  fi

  while IFS= read -r -d '' backup; do
    printf 'stale backup skill link: %s\n' "$backup"
    OMS_OPS_FAILED=1
    issues=1
  done < <(find "$root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)

  while IFS= read -r -d '' file; do
    name="$(oms_ops_extract_skill_name "$file")"
    if [ -z "$name" ]; then
      printf 'missing skill name: %s\n' "$file"
      OMS_OPS_FAILED=1
      issues=1
      continue
    fi

    total=$((total + 1))
    prev="$(printf '%s' "$first_seen" | awk -F'\t' -v n="$name" '$1 == n { print $2; exit }')"
    if [ -n "$prev" ]; then
      printf 'duplicate skill name: %s\n' "$name"
      printf '  first: %s\n' "$prev"
      printf '  also:  %s\n' "$file"
      OMS_OPS_FAILED=1
      issues=1
    else
      first_seen="${first_seen}${name}$(printf '\t')${file}
"
    fi
  done < <(find -L "$root" -mindepth 2 -maxdepth 2 -name SKILL.md -type f -print0 2>/dev/null)

  if [ "$issues" -eq 0 ]; then
    printf 'ok: %s (%s skills, no duplicates)\n' "$root" "$total"
  fi
  printf '\n'
}
