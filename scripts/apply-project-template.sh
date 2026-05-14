#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STYLE="${1:-general}"
PROJECT_DIR="${2:-$PWD}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"

if [ "$STYLE" = "auto" ]; then
  STYLE="$("$ROOT/scripts/detect-project-style.sh" "$PROJECT_DIR")"
  echo "detected project style: $STYLE"
fi

case "$STYLE" in
  general) TEMPLATE="$ROOT/templates/project-general-AGENTS.md" ;;
  ml) TEMPLATE="$ROOT/templates/project-ml-AGENTS.md" ;;
  slurm-ml) TEMPLATE="$ROOT/templates/project-slurm-ml-AGENTS.md" ;;
  *)
    echo "usage: $0 [auto|general|ml|slurm-ml] [project_dir] [files...]" >&2
    exit 2
    ;;
esac

if [ ! -f "$TEMPLATE" ]; then
  echo "error: missing template $TEMPLATE" >&2
  exit 1
fi

shift || true
shift || true

if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  FILES=()
  [ -e "$PROJECT_DIR/AGENTS.md" ] && FILES+=("AGENTS.md")
  [ -e "$PROJECT_DIR/CLAUDE.md" ] && FILES+=("CLAUDE.md")
  [ -e "$PROJECT_DIR/GEMINI.md" ] && FILES+=("GEMINI.md")
  [ "${#FILES[@]}" -gt 0 ] || FILES=("AGENTS.md")
fi

BEGIN="<!-- oh-my-setting:${STYLE}:begin -->"
END="<!-- oh-my-setting:${STYLE}:end -->"

apply_one() {
  local rel="$1"
  local target="$PROJECT_DIR/$rel"
  local dir
  dir="$(dirname "$target")"

  mkdir -p "$dir"

  if [ "$DRY_RUN" = "1" ]; then
    echo "would update $target with $STYLE template"
    return 0
  fi

  local tmp
  tmp="$(mktemp)"

  if [ -f "$target" ]; then
    awk -v begin="$BEGIN" -v end="$END" '
      $0 == begin { skip = 1; next }
      $0 == end { skip = 0; next }
      !skip { print }
    ' "$target" > "$tmp"
  fi

  if [ -s "$tmp" ]; then
    printf '\n' >> "$tmp"
  fi

  {
    printf '%s\n\n' "$BEGIN"
    cat "$TEMPLATE"
    printf '\n%s\n' "$END"
  } >> "$tmp"

  mv "$tmp" "$target"
  echo "updated $target"
}

for f in "${FILES[@]}"; do
  apply_one "$f"
done
