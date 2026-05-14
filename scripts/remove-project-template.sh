#!/usr/bin/env bash
set -euo pipefail

STYLE="${1:-all}"
PROJECT_DIR="${2:-$PWD}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"

case "$STYLE" in
  all|general|ml|slurm) ;;
  *)
    echo "usage: $0 [all|general|ml|slurm] [project_dir] [files...]" >&2
    exit 2
    ;;
esac

shift || true
shift || true

if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  FILES=()
  [ -e "$PROJECT_DIR/AGENTS.md" ] && FILES+=("AGENTS.md")
  [ -e "$PROJECT_DIR/CLAUDE.md" ] && FILES+=("CLAUDE.md")
  [ -e "$PROJECT_DIR/GEMINI.md" ] && FILES+=("GEMINI.md")
fi

remove_one_style() {
  local file="$1"
  local style="$2"
  local begin="<!-- oh-my-setting:${style}:begin -->"
  local end="<!-- oh-my-setting:${style}:end -->"
  local tmp
  tmp="$(mktemp)"

  awk -v begin="$begin" -v end="$end" '
    $0 == begin { skip = 1; changed = 1; next }
    $0 == end { skip = 0; next }
    !skip { print }
    END { if (!changed) exit 3 }
  ' "$file" > "$tmp" || {
    local code="$?"
    rm -f "$tmp"
    [ "$code" -eq 3 ] && return 0
    return "$code"
  }

  mv "$tmp" "$file"
  echo "removed $style block from $file"
}

remove_one() {
  local rel="$1"
  local target="$PROJECT_DIR/$rel"

  [ -f "$target" ] || return 0

  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove $STYLE block(s) from $target"
    return 0
  fi

  if [ "$STYLE" = "all" ]; then
    remove_one_style "$target" "general"
    remove_one_style "$target" "ml"
    remove_one_style "$target" "slurm"
  else
    remove_one_style "$target" "$STYLE"
  fi
}

for f in "${FILES[@]}"; do
  remove_one "$f"
done
