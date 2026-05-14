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
  FILES=("AGENTS.md" "CLAUDE.md" "GEMINI.md")
fi

BEGIN="<!-- oh-my-setting:${STYLE}:begin -->"
END="<!-- oh-my-setting:${STYLE}:end -->"

loader_content() {
  local display_template="$TEMPLATE"
  display_template="${display_template/#$HOME/~}"

  printf '# oh-my-setting Loader\n\n'
  printf 'Read `PROJECT.md` first for project-specific details.\n'
  printf 'Then follow `%s` for shared `%s` rules.\n' "$display_template" "$STYLE"
  printf 'Project rules override global defaults.\n'
}

project_content() {
  printf '# PROJECT.md\n\n'
  printf 'Project-specific spec. Fill this file per repository.\n\n'
  printf '## Project\n\n'
  printf -- '- Name:\n'
  printf -- '- Type: %s\n' "$STYLE"
  printf -- '- Goal:\n'
  printf -- '- Non-goals:\n\n'
  printf '## Commands\n\n'
  printf -- '- Setup:\n'
  printf -- '- Test:\n'
  printf -- '- Run:\n'
  printf -- '- Lint/typecheck:\n\n'
  printf '## Paths\n\n'
  printf -- '- Data:\n'
  printf -- '- Config:\n'
  printf -- '- Outputs/logs:\n'
  printf -- '- Checkpoints:\n\n'
  printf '## Verification\n\n'
  printf -- '- Success criteria:\n'
  printf -- '- Required checks:\n'
  printf -- '- Baseline/metric:\n\n'
  printf '## Notes\n\n'
  printf -- '- Do not touch:\n'
  printf -- '- Risks:\n'
}

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
    loader_content
    printf '\n%s\n' "$END"
  } >> "$tmp"

  mv "$tmp" "$target"
  echo "updated $target"
}

for f in "${FILES[@]}"; do
  apply_one "$f"
done

project_target="$PROJECT_DIR/PROJECT.md"
if [ "$DRY_RUN" = "1" ]; then
  echo "would ensure $project_target"
elif [ ! -e "$project_target" ]; then
  project_content > "$project_target"
  echo "created $project_target"
fi
