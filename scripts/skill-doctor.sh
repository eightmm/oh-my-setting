#!/usr/bin/env bash
set -euo pipefail

FAILED=0

extract_skill_name() {
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

check_root() {
  local label="$1"
  local root="$2"
  local file
  local name
  local total=0
  local issues=0
  local backup
  declare -A first_seen=()

  printf '## %s\n\n' "$label"

  if [ ! -d "$root" ]; then
    printf 'skip: %s missing\n\n' "$root"
    return 0
  fi

  while IFS= read -r -d '' backup; do
    printf 'stale backup skill link: %s\n' "$backup"
    FAILED=1
    issues=1
  done < <(find "$root" -maxdepth 1 -type l -name '*.backup.*' -print0 2>/dev/null)

  while IFS= read -r -d '' file; do
    name="$(extract_skill_name "$file")"
    if [ -z "$name" ]; then
      printf 'missing skill name: %s\n' "$file"
      FAILED=1
      issues=1
      continue
    fi

    total=$((total + 1))
    if [ -n "${first_seen[$name]+set}" ]; then
      printf 'duplicate skill name: %s\n' "$name"
      printf '  first: %s\n' "${first_seen[$name]}"
      printf '  also:  %s\n' "$file"
      FAILED=1
      issues=1
    else
      first_seen[$name]="$file"
    fi
  done < <(find -L "$root" -mindepth 2 -maxdepth 2 -name SKILL.md -type f -print0 2>/dev/null)

  if [ "$issues" -eq 0 ]; then
    printf 'ok: %s (%s skills, no duplicates)\n' "$root" "$total"
  fi
  printf '\n'
}

printf '# oh-my-setting skill doctor\n\n'
check_root "Codex skills" "$HOME/.codex/skills"
check_root "Claude skills" "$HOME/.claude/skills"
check_root "Antigravity skills" "$HOME/.gemini/antigravity/skills"

if [ "$FAILED" -ne 0 ]; then
  printf 'skill-doctor: failed\n'
  exit 1
fi

printf 'skill-doctor: ok\n'
