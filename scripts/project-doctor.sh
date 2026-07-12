#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_DIR="${1:-$PWD}"
FAILED=0
WARNED=0

usage() {
  cat <<'EOF'
Usage: project-doctor.sh [project_dir]

Verify that all agent-facing files in a project give every agent the same view:

- AGENTS.md and CLAUDE.md contain the same oh-my-setting managed blocks
- managed blocks match the current oh-my-setting templates (not stale)
- PROJECT.md exists and has a State field
- ml projects: docs/ scaffold and .gitignore entries present
- ml projects: structure drift warnings (stray root *.py, markdown outside
  docs/, notebooks outside notebooks/, missing src/ layout, tracked files in
  gitignored dirs, tracked files over 10MB)

Exit 0 when consistent (warnings allowed), 1 on drift/missing blocks.
EOF
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

[ "$#" -le 1 ] || {
  echo "error: too many arguments" >&2
  usage >&2
  exit 2
}

[ -d "$PROJECT_DIR" ] || {
  echo "error: not a directory: $PROJECT_DIR" >&2
  exit 2
}
PROJECT_DIR="$(cd "$PROJECT_DIR" && pwd)"

fail() {
  echo "drift: $*"
  FAILED=1
}

warn() {
  echo "warn: $*"
  WARNED=1
}

ok() {
  echo "ok: $*"
}

has_block() {
  local file="$1"
  local style="$2"
  [ -f "$file" ] && grep -Fq "<!-- oh-my-setting:${style}:begin -->" "$file"
}

extract_block() {
  local file="$1"
  local style="$2"
  awk -v begin="<!-- oh-my-setting:${style}:begin -->" \
      -v end="<!-- oh-my-setting:${style}:end -->" '
    $0 == begin { f = 1; next }
    $0 == end { f = 0; next }
    f
  ' "$file"
}

# Created eagerly in the main shell: reference_block runs inside process
# substitution (a subshell), so assignments made there would be lost.
REF_DIR="$(mktemp -d)" || {
  echo "error: mktemp failed" >&2
  exit 2
}
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -rf "$REF_DIR"
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

# Generate reference blocks from current templates in a throwaway project.
reference_block() {
  local style="$1"
  if [ ! -f "$REF_DIR/$style/AGENTS.md" ]; then
    mkdir -p "$REF_DIR/$style"
    "$ROOT/scripts/apply-project-template.sh" "$style" "$REF_DIR/$style" >/dev/null
  fi
  extract_block "$REF_DIR/$style/AGENTS.md" "$style"
}

agents_file="$PROJECT_DIR/AGENTS.md"
claude_file="$PROJECT_DIR/CLAUDE.md"
styles_found=0

for style in general ml slurm; do
  in_agents=0
  in_claude=0
  has_block "$agents_file" "$style" && in_agents=1
  has_block "$claude_file" "$style" && in_claude=1

  [ "$in_agents" -eq 0 ] && [ "$in_claude" -eq 0 ] && continue
  styles_found=$((styles_found + 1))

  if [ "$in_agents" -eq 1 ] && [ "$in_claude" -eq 0 ]; then
    fail "$style block in AGENTS.md but missing in CLAUDE.md"
    continue
  fi
  if [ "$in_agents" -eq 0 ] && [ "$in_claude" -eq 1 ]; then
    fail "$style block in CLAUDE.md but missing in AGENTS.md"
    continue
  fi

  if diff -q \
    <(extract_block "$agents_file" "$style") \
    <(extract_block "$claude_file" "$style") >/dev/null; then
    ok "$style block identical in AGENTS.md and CLAUDE.md"
  else
    fail "$style block differs between AGENTS.md and CLAUDE.md"
  fi

  if diff -q \
    <(extract_block "$agents_file" "$style") \
    <(reference_block "$style") >/dev/null; then
    ok "$style block matches current template"
  else
    fail "$style block is stale; re-run: apply-project-template.sh $style $PROJECT_DIR"
  fi
done

if [ "$styles_found" -eq 0 ]; then
  fail "no oh-my-setting managed blocks found; run: apply-project-template.sh auto $PROJECT_DIR"
fi

if [ -f "$PROJECT_DIR/PROJECT.md" ]; then
  pm_field() {
    awk -v prefix="- $1:" '
      index($0, prefix) == 1 {
        value = substr($0, length(prefix) + 1)
        sub(/^[[:space:]]*/, "", value)
        print value
        exit
      }
    ' "$PROJECT_DIR/PROJECT.md"
  }
  state="$(grep -E '^- State:' "$PROJECT_DIR/PROJECT.md" | head -n 1 | sed 's/^- State:[[:space:]]*//' || true)"
  if [ -z "$state" ]; then
    warn "PROJECT.md has no '- State:' field"
  elif [ "$state" = "draft" ]; then
    warn "PROJECT.md state is draft; confirm spec before broad work"
  else
    ok "PROJECT.md state: $state"

    # Past draft: the project contract (commands + verification) is supposed to
    # be filled. Empty fields mean agents have no per-project success criteria
    # to verify against -- enforce it where the contract lives (PROJECT.md), not
    # in the template-synced managed block.
    if [ -z "$(pm_field Setup)$(pm_field Test)$(pm_field Run)" ]; then
      warn "PROJECT.md is past draft but ## Commands (Setup/Test/Run) are empty"
    fi
    if [ -z "$(pm_field 'Success criteria')" ]; then
      warn "PROJECT.md is past draft but ## Verification 'Success criteria' is empty"
    fi
  fi
else
  fail "PROJECT.md missing; run: apply-project-template.sh auto $PROJECT_DIR"
fi

if has_block "$agents_file" "ml" || has_block "$claude_file" "ml"; then
  if [ -n "${state:-}" ] && [ "$state" != "draft" ]; then
    missing_ml=""
    for field in \
      'Prediction unit' \
      'Inference-time information boundary' \
      'Entity IDs/standardization' \
      'Source snapshot/provenance' \
      'Label/target definition' \
      'Label units/direction/censoring/replicates' \
      'Split policy' \
      'Split/group keys' \
      'Leakage risks' \
      'Train-only fitted transforms' \
      'Data manifest' \
      'Calibration/applicability-domain plan'; do
      [ -n "$(pm_field "$field")" ] || missing_ml="${missing_ml}${missing_ml:+, }$field"
    done
    if [ -n "$missing_ml" ]; then
      warn "ML scientific contract fields are empty: $missing_ml (use n/a only when justified)"
    else
      ok "ML scientific contract fields are filled"
    fi
  fi

  missing_docs=0
  for base in DATA.md MODEL.md EVALUATION.md EXPERIMENTS.md REPRODUCIBILITY.md; do
    if [ ! -f "$PROJECT_DIR/docs/$base" ]; then
      missing_docs=$((missing_docs + 1))
    fi
  done
  if [ "$missing_docs" -eq 0 ]; then
    ok "core ml docs scaffold complete under docs/"
  else
    warn "$missing_docs core ml doc(s) missing under docs/; re-run: apply-project-template.sh ml $PROJECT_DIR"
  fi

  # Keep in sync with ML_IGNORE_ENTRIES in apply-project-template.sh.
  for entry in data/ outputs/ checkpoints/ wandb/ runs/ .venv/ .oms/; do
    if [ -f "$PROJECT_DIR/.gitignore" ] && grep -qxF "$entry" "$PROJECT_DIR/.gitignore"; then
      ok ".gitignore covers $entry"
    else
      warn ".gitignore missing entry: $entry"
    fi
  done

  if [ -x "$PROJECT_DIR/scripts/check.sh" ]; then
    ok "verification contract present: scripts/check.sh"
  else
    warn "scripts/check.sh missing or not executable; re-run: apply-project-template.sh ml $PROJECT_DIR"
  fi

  # Structure drift (warn-level): the layout the ml rules promise must still
  # hold mid-project, not only at scaffold time. POSIX find/-print only —
  # GNU-only primaries would silently skip the checks on BSD/macOS.
  rel_names() {
    while IFS= read -r p; do
      printf '%s ' "${p#"$PROJECT_DIR"/}"
    done
  }

  stray_py="$(find "$PROJECT_DIR" -maxdepth 1 -type f -name '*.py' \
      ! -name 'setup.py' ! -name 'conftest.py' ! -name 'noxfile.py' \
      ! -name 'tasks.py' ! -name 'dodo.py' \
      -print 2>/dev/null | rel_names)" || stray_py=""
  if [ -n "$stray_py" ]; then
    warn "top-level python files (move into src/ or scripts/): $stray_py"
  else
    ok "no stray top-level python files"
  fi

  stray_md="$(find "$PROJECT_DIR" -maxdepth 2 \( \
      -name '.git' -o -name '.venv' -o -name 'node_modules' -o -name 'docs' \
    \) -prune -o -type f -name '*.md' \
      ! -name 'README*.md' ! -name 'AGENTS.md' ! -name 'CLAUDE.md' ! -name 'PROJECT.md' \
      ! -name 'CONTRIBUTING.md' ! -name 'CHANGELOG.md' ! -name 'LICENSE.md' \
      ! -name 'SECURITY.md' ! -name 'CODE_OF_CONDUCT.md' \
      -print 2>/dev/null | rel_names)" || stray_md=""
  if [ -n "$stray_md" ]; then
    warn "markdown outside docs/ (move there): $stray_md"
  else
    ok "markdown files live under docs/"
  fi

  stray_nb="$(find "$PROJECT_DIR" -maxdepth 3 \( \
      -name '.git' -o -name '.venv' -o -name 'node_modules' -o -name 'notebooks' \
    \) -prune -o -type f -name '*.ipynb' \
      -print 2>/dev/null | rel_names)" || stray_nb=""
  if [ -n "$stray_nb" ]; then
    warn "notebooks outside notebooks/ (move there): $stray_nb"
  else
    ok "notebooks live under notebooks/"
  fi

  if [ -f "$PROJECT_DIR/pyproject.toml" ] && [ ! -d "$PROJECT_DIR/src" ]; then
    warn "pyproject.toml without src/ layout; ml rules expect src/<package>/"
  fi

  # Experiments are running (ledger has rows) but nothing is pre-registered:
  # exactly when the research-method discipline starts to matter.
  if [ -s "$PROJECT_DIR/docs/EXPERIMENTS.jsonl" ] && [ -f "$PROJECT_DIR/PROJECT.md" ] &&
    ! grep -q '^## Experiment Pre-Registration' "$PROJECT_DIR/PROJECT.md"; then
    warn "experiments in the ledger but PROJECT.md has no '## Experiment Pre-Registration' section (see research-method skill)"
  fi

  if git -C "$PROJECT_DIR" rev-parse --git-dir >/dev/null 2>&1; then
    # Placeholder files (.gitkeep/.gitignore) are legitimate inside these dirs.
    # "|| true" INSIDE the substitution: head's early exit SIGPIPEs the
    # upstream under pipefail, and an outer fallback would wipe the output.
    tracked_ignored="$(git -C "$PROJECT_DIR" ls-files -- data outputs checkpoints wandb runs 2>/dev/null |
      grep -Ev '(^|/)\.git(keep|ignore)$' | head -3 || true)"
    if [ -n "$tracked_ignored" ]; then
      warn "tracked files inside gitignored dirs (committed before ignore?): $(printf '%s' "$tracked_ignored" | tr '\n' ' ')"
    else
      ok "no tracked files inside data/outputs/checkpoints/wandb/runs"
    fi

    # Big-file scan: plain bash loop — no xargs (empty input must scan
    # nothing), no du (GNU-only, and directory totals would flag submodules),
    # filenames with spaces/newlines preserved via -z. Bounded for monorepos.
    big_tracked=""
    big_count=0
    scanned=0
    max_scan="${OMS_DOCTOR_MAX_SCAN:-2000}"
    while IFS= read -r -d '' p; do
      scanned=$((scanned + 1))
      if [ "$scanned" -gt "$max_scan" ]; then
        warn "big-file scan stopped at $max_scan tracked files (OMS_DOCTOR_MAX_SCAN)"
        break
      fi
      f="$PROJECT_DIR/$p"
      if [ -L "$f" ] || [ ! -f "$f" ]; then
        continue  # symlinks would be measured by their target; skip
      fi
      sz="$(wc -c < "$f" 2>/dev/null | tr -d ' ')" || continue
      if [ "${sz:-0}" -gt 10485760 ]; then
        big_tracked="$big_tracked$p "
        big_count=$((big_count + 1))
        if [ "$big_count" -ge 3 ]; then
          break
        fi
      fi
    done < <(git -C "$PROJECT_DIR" ls-files -z 2>/dev/null || true)
    if [ -n "$big_tracked" ]; then
      warn "tracked files over 10MB (data/checkpoints belong outside git): $big_tracked"
    fi
  fi
fi

if [ "$FAILED" -ne 0 ]; then
  echo "project-doctor: failed"
  exit 1
fi
if [ "$WARNED" -ne 0 ]; then
  echo "project-doctor: ok (with warnings)"
else
  echo "project-doctor: ok"
fi
