#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
ML_FULL_DOCS="${OH_MY_SETTING_ML_FULL_DOCS:-0}"
PRIVATE="${OH_MY_SETTING_PRIVATE_AGENT_FILES:-1}"
ADD_SLURM=0

# --private/--no-private may appear anywhere, so strip them before the
# positional style/dir/files parse below.
KEPT_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --no-private) PRIVATE=0 ;;
    --private) PRIVATE=1 ;;
    *) KEPT_ARGS+=("$arg") ;;
  esac
done
set -- ${KEPT_ARGS+"${KEPT_ARGS[@]}"}

STYLE="${1:-general}"
BASE_STYLE="$STYLE"

usage() {
  cat <<'EOF'
Usage: apply-project-template.sh [auto|general|ml|slurm] [project_dir]
                                 [--full-docs] [--no-private] [files...]

Apply oh-my-setting project rule blocks and scaffold PROJECT.md.
ML projects create five core docs by default; --full-docs creates all templates.
The agent-facing files are hidden from git locally (see project-private.sh);
--no-private, or OH_MY_SETTING_PRIVATE_AGENT_FILES=0, keeps them visible.
EOF
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

case "$BASE_STYLE" in
  auto|general|ml|slurm) ;;
  *)
    usage >&2
    exit 2
    ;;
esac

[ "$#" -eq 0 ] || shift
PROJECT_DIR="$PWD"
if [ "$#" -gt 0 ] && [ "$1" != "--full-docs" ]; then
  PROJECT_DIR="$1"
  shift
fi
if [ "${1:-}" = "--full-docs" ]; then
  ML_FULL_DOCS=1
  shift
fi
case "$ML_FULL_DOCS" in
  0|1) ;;
  *) echo "error: OH_MY_SETTING_ML_FULL_DOCS must be 0 or 1" >&2; exit 2 ;;
esac
case "$PRIVATE" in
  0|1) ;;
  *) echo "error: OH_MY_SETTING_PRIVATE_AGENT_FILES must be 0 or 1" >&2; exit 2 ;;
esac

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

  # One implementation, not an rg fast path with a find fallback: the two
  # branches disagreed on depth, hidden files, and gitignore, so the same
  # repository detected a different style depending on whether rg was on
  # PATH (codex vendors one). CI runs find-only; find is the contract.
  find "$PROJECT_DIR" -maxdepth 3 \( \
    -name ".git" -o -name ".venv" -o -name "node_modules" -o \
    -name "__pycache__" -o -name "tmp" -o -name "backups" \
  \) -prune -o -type f \( -name '*.sh' -o -name '*.sbatch' \) \
    ! -path "$PROJECT_DIR/scripts/apply-project-template.sh" \
    -exec grep -q "#SBATCH" {} \; -print -quit 2>/dev/null | grep -q .
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
    usage >&2
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

if [ "$#" -gt 0 ]; then
  FILES=("$@")
else
  FILES=("AGENTS.md" "CLAUDE.md")
  # Antigravity reads GEMINI.md as a directory rule file and prefers it over
  # AGENTS.md when both exist — verified live: in a project whose AGENTS.md
  # carried the ml loader and whose leftover GEMINI.md still carried general,
  # agy reported the general template. An unmanaged GEMINI.md therefore makes
  # one CLI follow rules the other two retired, which is the divergence this
  # project exists to prevent. It is adopted only when the project already has
  # one: agy reads AGENTS.md fine, so a new project needs no third loader.
  if [ -f "$PROJECT_DIR/GEMINI.md" ]; then
    FILES+=("GEMINI.md")
  fi
fi

loader_content() {
  local style="$1"
  local template="$2"
  local display_template="$template"
  # The tilde must be escaped: bash tilde-expands an unquoted ~ in the
  # replacement, silently writing the absolute home path into the generated
  # loader — a machine detail, and a pointer that resolves nowhere else.
  display_template="${display_template/#$HOME/\~}"

  printf '# oh-my-setting Loader\n\n'
  printf 'Read `PROJECT.md`, then follow `%s` for shared `%s` rules.\n' "$display_template" "$style"
  printf 'Project rules override global defaults. Resolve only open choices that affect this work.\n'
  printf 'Clear bounded changes may proceed from local evidence.\n'
}

project_content() {
  local style="$1"
  local add_slurm="$2"
  if [ -z "$style" ]; then
    style="slurm"
  fi

  printf '# PROJECT.md\n\n'
  printf 'Project-specific decisions and verification contract.\n\n'
  printf '## Status\n\n'
  printf -- '- State: draft\n'
  printf -- '- Open decisions:\n\n'
  printf '## Project\n\n'
  printf -- '- Type: %s\n' "$style"
  printf -- '- Goal:\n'
  printf -- '- Scope:\n'
  printf -- '- Non-goals:\n\n'
  printf '## Commands\n\n'
  if [ "$style" = "ml" ]; then
    printf -- '- Setup: uv sync\n'
  else
    printf -- '- Setup:\n'
  fi
  printf -- '- Test:\n'
  printf -- '- Run:\n'
  printf -- '- Lint/typecheck:\n\n'
  printf '## Paths\n\n'
  printf -- '- Data:\n'
  printf -- '- Config:\n'
  printf -- '- Outputs/logs:\n'
  printf -- '- Checkpoints:\n\n'
  if [ "$style" = "ml" ]; then
    printf '## ML Scientific Contract\n\n'
    printf -- '- Prediction unit:\n'
    printf -- '- Inference-time information boundary:\n'
    printf -- '- Entity IDs/standardization:\n'
    printf -- '- Source snapshot/provenance:\n'
    printf -- '- Label/target definition:\n'
    printf -- '- Label units/direction/censoring/replicates:\n'
    printf -- '- Split policy:\n'
    printf -- '- Split/group keys:\n'
    printf -- '- Leakage risks:\n'
    printf -- '- Train-only fitted transforms:\n'
    printf -- '- Data manifest:\n'
    printf -- '- Baseline / primary metric:\n'
    printf -- '- Calibration/applicability-domain plan:\n\n'
  fi
  printf '## Verification\n\n'
  printf -- '- Success criteria:\n'
  printf -- '- Required checks:\n\n'
  if [ "$style" = "ml" ]; then
    printf '## Experiment Pre-Registration\n\n'
    printf -- '- Hypothesis / predicted outcome:\n'
    printf -- '- Metric + split:\n'
    printf -- '- Success / abandon threshold:\n\n'
  fi
  if [ "$add_slurm" = "1" ]; then
    printf '## Slurm\n\n'
    printf -- '- Partition/account:\n'
    printf -- '- CPU/GPU/memory/time:\n'
    printf -- '- Logs/checkpoints:\n\n'
  fi
  printf '## Notes\n\n'
  printf -- '- Gotchas (non-obvious decisions an agent would get wrong):\n'
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

# Switching a project's base style must retire the one it had. Managed blocks
# are per-style, so applying `ml` over `general` used to leave both in place
# and the agent read two loaders with different rules.
#
# Base styles (general, ml) are mutually exclusive and safe to replace. The
# slurm block is an additive overlay whose presence follows machine detection
# (has_slurm_runtime), so re-applying `ml` from a workstation must not strip
# the cluster rules a cluster project deliberately carries — removing that is
# `remove-project-template.sh slurm`, an explicit act.
drop_stale_styles() {
  local rel="$1"
  local target="$PROJECT_DIR/$rel"
  local style

  [ -f "$target" ] || return 0
  [ -n "$BASE_STYLE" ] || return 0
  for style in general ml; do
    [ "$style" = "$BASE_STYLE" ] && continue
    grep -Fq "<!-- oh-my-setting:${style}:begin -->" "$target" || continue
    if [ "$DRY_RUN" = "1" ]; then
      printf 'would remove stale %s block from %s/%s\n' "$style" "$PROJECT_DIR" "$rel"
      continue
    fi
    "$ROOT/scripts/remove-project-template.sh" "$style" "$PROJECT_DIR" "$rel" \
      >/dev/null 2>&1 ||
      printf 'warn: could not remove stale %s block from %s\n' "$style" "$rel" >&2
  done
}

for f in "${FILES[@]}"; do
  applied=()
  drop_stale_styles "$f"
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
        if [ "$ML_FULL_DOCS" != "1" ]; then
          case "$base" in
            DATA.md|MODEL.md|EVALUATION.md|EXPERIMENTS.md|REPRODUCIBILITY.md) ;;
            *) continue ;;
          esac
        fi
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

  # ML project skills: the run-discipline and dataset-safety workflows ship
  # as project skills so every CLI's native project discovery loads them —
  # only in ML repos, never in the global catalog. Installed through
  # skill-forge, which is also the validation and scrubbing gate.
  if [ "$DRY_RUN" = "1" ]; then
    echo "would install ML project skills (oms-ml-experiment, oms-dataset-safety)"
  else
    for tskill in oms-ml-experiment oms-dataset-safety; do
      tskill_src="$ROOT/templates/project-skills/$tskill/SKILL.md"
      [ -f "$tskill_src" ] || continue
      [ ! -f "$PROJECT_DIR/.oms/skills/$tskill/SKILL.md" ] || continue
      # A project that applied the template before the oms- rename holds
      # the same lesson under the legacy name; installing the prefixed
      # copy beside it would load the skill twice in every session.
      [ ! -f "$PROJECT_DIR/.oms/skills/${tskill#oms-}/SKILL.md" ] || continue
      "$ROOT/scripts/skill-forge.sh" --repo "$PROJECT_DIR" add \
        --name "$tskill" --file "$tskill_src" >/dev/null ||
        echo "warn: could not install project skill $tskill"
    done
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

# Keep the agent-facing files out of the project's git history (public repos
# stay clean). Best effort: the rule blocks above are the contract, hiding is an
# add-on, and project-doctor re-reports an exposed file later.
if [ "$PRIVATE" = "1" ]; then
  PRIVATE_ARGS=(--repo "$PROJECT_DIR" apply)
  [ "$DRY_RUN" = "1" ] && PRIVATE_ARGS+=(--dry-run)
  for f in "${FILES[@]}"; do
    case "$f" in
      AGENTS.md|CLAUDE.md|GEMINI.md|PROJECT.md) ;;  # already default entries
      *[[:space:]]*|/*|*..*)
        echo "note: cannot auto-hide $f; add it by hand to .git/info/exclude" ;;
      *) PRIVATE_ARGS+=(--path "$f") ;;
    esac
  done
  # A dry run never creates the project dir, so there is nothing to inspect.
  if [ -d "$PROJECT_DIR" ]; then
    "$ROOT/scripts/project-private.sh" "${PRIVATE_ARGS[@]}" ||
      echo "warn: could not hide agent files from git; run: oms project-private status"
  fi
fi
