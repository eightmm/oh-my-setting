#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STYLE="${1:-general}"
PROJECT_DIR="${2:-$PWD}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
BASE_STYLE="$STYLE"
ADD_SLURM=0

has_slurm_runtime() {
  command -v sbatch >/dev/null 2>&1 ||
    command -v srun >/dev/null 2>&1 ||
    command -v sinfo >/dev/null 2>&1 ||
    [ -n "${SLURM_CLUSTER_NAME:-}" ] ||
    [ -n "${SLURM_JOB_ID:-}" ]
}

has_slurm_project() {
  [ -d "$PROJECT_DIR" ] || return 1

  if find "$PROJECT_DIR" -maxdepth 3 \( \
      -name ".git" -o -name ".venv" -o -name "node_modules" -o \
      -name "__pycache__" -o -name "tmp" -o -name "backups" \
    \) -prune -o \( -name "*.sbatch" -o -name "slurm*.sh" -o -name "submit*.sh" \) -print -quit | grep -q .; then
    return 0
  fi

  if command -v rg >/dev/null 2>&1; then
    # Search from inside the project so exclude globs match project-relative
    # paths, not absolute path segments (e.g. a project located under /tmp).
    (cd "$PROJECT_DIR" && rg -q "#SBATCH" . \
      -g '*.sh' -g '*.sbatch' \
      -g '!scripts/apply-project-template.sh' \
      -g '!**/.git/**' -g '!**/.venv/**' -g '!**/node_modules/**' \
      -g '!**/__pycache__/**' -g '!**/tmp/**' -g '!**/backups/**' 2>/dev/null)
  else
    find "$PROJECT_DIR" -maxdepth 3 \( \
      -name ".git" -o -name ".venv" -o -name "node_modules" -o \
      -name "__pycache__" -o -name "tmp" -o -name "backups" \
    \) -prune -o -type f \( -name '*.sh' -o -name '*.sbatch' \) \
      ! -path "$PROJECT_DIR/scripts/apply-project-template.sh" \
      -exec grep -q "#SBATCH" {} \; -print -quit 2>/dev/null | grep -q .
  fi
}

if [ "$BASE_STYLE" = "auto" ]; then
  BASE_STYLE="$("$ROOT/scripts/detect-project-style.sh" "$PROJECT_DIR")"
  echo "detected project style: $BASE_STYLE"
fi

if [ "$BASE_STYLE" = "slurm" ]; then
  BASE_STYLE=""
  ADD_SLURM=1
elif has_slurm_project; then
  ADD_SLURM=1
  echo "detected Slurm project files; adding slurm project rules"
elif [ "$BASE_STYLE" = "ml" ] && has_slurm_runtime; then
  ADD_SLURM=1
  echo "detected Slurm runtime; adding slurm project rules"
fi

case "$BASE_STYLE" in
  "") TEMPLATE="" ;;
  general) TEMPLATE="$ROOT/templates/project-general-AGENTS.md" ;;
  ml) TEMPLATE="$ROOT/templates/project-ml-AGENTS.md" ;;
  *)
    echo "usage: $0 [auto|general|ml|slurm] [project_dir] [files...]" >&2
    exit 2
    ;;
esac

SLURM_TEMPLATE="$ROOT/templates/project-slurm-AGENTS.md"

if [ -n "$TEMPLATE" ] && [ ! -f "$TEMPLATE" ]; then
  echo "error: missing template $TEMPLATE" >&2
  exit 1
fi

if [ "$ADD_SLURM" = "1" ] && [ ! -f "$SLURM_TEMPLATE" ]; then
  echo "error: missing template $SLURM_TEMPLATE" >&2
  exit 1
fi

shift || true
shift || true

if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  FILES=("AGENTS.md" "CLAUDE.md")
fi

loader_content() {
  local style="$1"
  local template="$2"
  local display_template="$template"
  display_template="${display_template/#$HOME/~}"

  printf '# oh-my-setting Loader\n\n'
  printf 'Read `PROJECT.md` first. Project work starts only after it is filled and confirmed.\n'
  printf 'Then follow `%s` for shared `%s` rules.\n' "$display_template" "$style"
  printf 'Project rules override global defaults.\n\n'
  printf '## Local Agent Rules\n\n'
  printf -- '- New/broad work or draft `PROJECT.md`: interview -> update `PROJECT.md` -> confirm -> code.\n'
  printf -- '- New/major docs: interview -> concrete outline -> confirm -> write.\n'
  if [ "$style" = "ml" ]; then
    printf -- '- New ML project: create only the standard skeleton before the interview/spec gate.\n'
    printf -- '- Verify with `scripts/check.sh fast` before claiming work done; `scripts/check.sh gpu` for GPU smoke.\n'
    printf -- '- Before proposing experiments, read `docs/EXPERIMENTS.jsonl` (run ledger) if present.\n'
  elif [ "$style" = "slurm" ]; then
    printf -- '- Slurm is an overlay; ask before partition/account/time/GPU/CPU/memory changes.\n'
  fi
  printf -- '- Keep edits task-scoped; do not rewrite unrelated files.\n'
  printf -- '- End with: changed, verified, not verified, next command.\n'
}

project_content() {
  local style="$1"
  local add_slurm="$2"
  if [ -z "$style" ]; then
    style="slurm"
  fi

  printf '# PROJECT.md\n\n'
  printf 'Project-specific spec. Work starts only after this file is filled and confirmed.\n\n'
  printf '## Status\n\n'
  printf -- '- State: draft\n'
  printf -- '- Last confirmed by:\n'
  printf -- '- Last updated:\n\n'
  printf '## Interview\n\n'
  printf -- '- Stage 1 intent: goal, users, non-goals\n'
  printf -- '- Stage 2 scope: interface, data, paths, constraints\n'
  printf -- '- Stage 3 execution: commands, verification, risks\n'
  printf -- '- Open decisions:\n\n'
  printf '## Project\n\n'
  printf -- '- Name:\n'
  printf -- '- Type: %s\n' "$style"
  printf -- '- Goal:\n'
  printf -- '- Users/workflow:\n'
  printf -- '- Scope:\n'
  printf -- '- Non-goals:\n\n'
  printf '## Current Task\n\n'
  printf -- '- Request:\n'
  printf -- '- Assumptions:\n'
  printf -- '- Open questions:\n\n'
  printf '## Commands\n\n'
  if [ "$style" = "ml" ]; then
    printf -- '- Setup: uv sync\n'
  else
    printf -- '- Setup:\n'
  fi
  printf -- '- Test:\n'
  printf -- '- Run:\n'
  printf -- '- Lint/typecheck:\n\n'
  if [ "$style" = "ml" ]; then
    printf '## Environment\n\n'
    printf -- '- Package manager: uv\n'
    printf -- '- Python env: .venv\n'
    printf -- '- Run prefix: uv run\n'
    printf -- '- Machine snapshot: ~/.oh-my-setting/local/machine.md\n'
    printf -- '- Project-specific compute:\n\n'
  fi
  printf '## Paths\n\n'
  printf -- '- Data:\n'
  printf -- '- Config:\n'
  printf -- '- Outputs/logs:\n'
  printf -- '- Checkpoints:\n\n'
  printf '## Docs\n\n'
  printf -- '- Docs needed:\n'
  printf -- '- Docs interview status: draft\n'
  printf -- '- Architecture doc:\n'
  printf -- '- Data doc:\n'
  printf -- '- Experiments doc:\n'
  printf -- '- Operations doc:\n'
  printf -- '- Decisions dir:\n'
  printf -- '- Update triggers:\n\n'
  if [ "$style" = "ml" ]; then
    printf '## ML Startup\n\n'
    printf -- '- Standard skeleton created: no\n'
    printf -- '- Package/module name:\n'
    printf -- '- Task type:\n'
    printf -- '- Data source/schema:\n'
    printf -- '- Label/target definition:\n'
    printf -- '- Split policy:\n'
    printf -- '- Leakage risks:\n'
    printf -- '- Baseline:\n'
    printf -- '- Primary metric:\n'
    printf -- '- Seed policy:\n\n'
    printf '## ML Structure\n\n'
    printf -- '- Configs: configs/\n'
    printf -- '- Source: src/<package>/\n'
    printf -- '- CLI scripts: scripts/\n'
    printf -- '- Tests: tests/\n'
    printf -- '- Docs: docs/\n'
    printf -- '- Notebooks: notebooks/\n'
    printf -- '- Raw data: data/raw/ (gitignored unless explicitly intended)\n'
    printf -- '- Processed data: data/processed/ (gitignored unless explicitly intended)\n'
    printf -- '- Outputs: outputs/ (gitignored)\n'
    printf -- '- Checkpoints: checkpoints/ (gitignored)\n\n'
  fi
  printf '## Verification\n\n'
  printf -- '- Success criteria:\n'
  printf -- '- Required checks:\n\n'
  if [ "$style" = "ml" ]; then
    printf '## Experiment Pre-Registration\n\n'
    printf 'Fill BEFORE each long/expensive run; see the research-method skill.\n\n'
    printf -- '- Hypothesis (falsifiable):\n'
    printf -- '- Metric + split:\n'
    printf -- '- Success threshold (delta vs baseline):\n'
    printf -- '- Baseline:\n'
    printf -- '- Predicted outcome:\n'
    printf -- '- Abandon condition:\n\n'
  fi
  if [ "$add_slurm" = "1" ]; then
    printf '## Slurm\n\n'
    printf -- '- Partition/account:\n'
    printf -- '- CPU/GPU/memory/time:\n'
    printf -- '- Logs/checkpoints:\n\n'
  fi
  printf '## Notes\n\n'
  printf -- '- Do not touch:\n'
  printf -- '- Risks:\n'
}

apply_one() {
  local rel="$1"
  local style="$2"
  local template="$3"
  local target="$PROJECT_DIR/$rel"
  local dir
  local begin="<!-- oh-my-setting:${style}:begin -->"
  local end="<!-- oh-my-setting:${style}:end -->"
  dir="$(dirname "$target")"

  [ "$DRY_RUN" = "1" ] && return 0

  mkdir -p "$dir"

  local tmp
  tmp="$(mktemp)"

  if [ -f "$target" ]; then
    awk -v begin="$begin" -v end="$end" '
      $0 == begin {
        if (skip) {
          printf "error: nested managed block begin: %s\n", begin > "/dev/stderr"
          exit 2
        }
        skip = 1
        next
      }
      $0 == end {
        if (!skip) {
          printf "error: unmatched managed block end: %s\n", end > "/dev/stderr"
          exit 2
        }
        skip = 0
        next
      }
      !skip { print }
      END {
        if (skip) {
          printf "error: missing managed block end: %s\n", end > "/dev/stderr"
          exit 2
        }
      }
    ' "$target" > "$tmp" || {
      rm -f "$tmp"
      return 1
    }
  fi

  if [ -s "$tmp" ]; then
    printf '\n' >> "$tmp"
  fi

  {
    printf '%s\n\n' "$begin"
    loader_content "$style" "$template"
    printf '\n%s\n' "$end"
  } >> "$tmp"

  mv "$tmp" "$target"
}

for f in "${FILES[@]}"; do
  applied=()
  if [ -n "$BASE_STYLE" ]; then
    apply_one "$f" "$BASE_STYLE" "$TEMPLATE"
    applied+=("$BASE_STYLE")
  fi
  if [ "$ADD_SLURM" = "1" ]; then
    apply_one "$f" "slurm" "$SLURM_TEMPLATE"
    applied+=("slurm")
  fi
  if [ "${#applied[@]}" -gt 0 ]; then
    styles="${applied[0]}"
    for style in "${applied[@]:1}"; do
      styles="$styles, $style"
    done
    if [ "$DRY_RUN" = "1" ]; then
      printf 'would update %s/%s: %s\n' "$PROJECT_DIR" "$f" "$styles"
    else
      printf 'updated %s/%s: %s\n' "$PROJECT_DIR" "$f" "$styles"
    fi
  fi
done

project_target="$PROJECT_DIR/PROJECT.md"
if [ "$DRY_RUN" = "1" ]; then
  echo "would ensure $project_target"
elif [ ! -e "$project_target" ]; then
  project_content "$BASE_STYLE" "$ADD_SLURM" > "$project_target"
  echo "created $project_target"
fi

if [ "$BASE_STYLE" = "ml" ]; then
  ML_DOCS_SRC="$ROOT/templates/ml-docs"
  if [ -d "$ML_DOCS_SRC" ]; then
    DOCS_DIR="$PROJECT_DIR/docs"
    if [ "$DRY_RUN" = "1" ]; then
      echo "would scaffold ML docs at $DOCS_DIR"
    else
      mkdir -p "$DOCS_DIR"
      for src in "$ML_DOCS_SRC"/*.md; do
        [ -e "$src" ] || continue
        base="$(basename "$src")"
        dst="$DOCS_DIR/$base"
        if [ -e "$dst" ]; then
          echo "skip existing $dst"
        else
          cp "$src" "$dst"
          echo "created $dst"
        fi
      done
    fi
  fi

  CHECK_SRC="$ROOT/templates/check.sh"
  CHECK_DST="$PROJECT_DIR/scripts/check.sh"
  if [ -f "$CHECK_SRC" ]; then
    if [ "$DRY_RUN" = "1" ]; then
      echo "would scaffold $CHECK_DST"
    elif [ -e "$CHECK_DST" ]; then
      echo "skip existing $CHECK_DST"
    else
      mkdir -p "$PROJECT_DIR/scripts"
      cp "$CHECK_SRC" "$CHECK_DST"
      chmod +x "$CHECK_DST"
      echo "created $CHECK_DST"
    fi
  fi

  ML_SMOKE_SRC="$ROOT/templates/ml_smoke.py"
  ML_SMOKE_DST="$PROJECT_DIR/scripts/ml_smoke.py"
  if [ -f "$ML_SMOKE_SRC" ]; then
    if [ "$DRY_RUN" = "1" ]; then
      echo "would scaffold $ML_SMOKE_DST"
    elif [ -e "$ML_SMOKE_DST" ]; then
      echo "skip existing $ML_SMOKE_DST"
    else
      mkdir -p "$PROJECT_DIR/scripts"
      cp "$ML_SMOKE_SRC" "$ML_SMOKE_DST"
      echo "created $ML_SMOKE_DST"
    fi
  fi

  GITIGNORE="$PROJECT_DIR/.gitignore"
  ML_IGNORE_ENTRIES=("data/" "outputs/" "checkpoints/" "wandb/" "runs/" ".venv/" ".oms/")
  if [ "$DRY_RUN" = "1" ]; then
    echo "would ensure ML entries in $GITIGNORE"
  else
    added=0
    for entry in "${ML_IGNORE_ENTRIES[@]}"; do
      if [ ! -f "$GITIGNORE" ] || ! grep -qxF "$entry" "$GITIGNORE"; then
        printf '%s\n' "$entry" >> "$GITIGNORE"
        added=$((added + 1))
      fi
    done
    if [ "$added" -gt 0 ]; then
      echo "updated $GITIGNORE: added $added ML entries"
    fi
  fi
fi
