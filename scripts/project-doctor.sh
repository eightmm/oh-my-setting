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
cleanup() {
  rm -rf "$REF_DIR"
}
trap cleanup EXIT

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
  state="$(grep -E '^- State:' "$PROJECT_DIR/PROJECT.md" | head -n 1 | sed 's/^- State:[[:space:]]*//' || true)"
  if [ -z "$state" ]; then
    warn "PROJECT.md has no '- State:' field"
  elif [ "$state" = "draft" ]; then
    warn "PROJECT.md state is draft; confirm spec before broad work"
  else
    ok "PROJECT.md state: $state"
  fi
else
  fail "PROJECT.md missing; run: apply-project-template.sh auto $PROJECT_DIR"
fi

if has_block "$agents_file" "ml" || has_block "$claude_file" "ml"; then
  missing_docs=0
  for src in "$ROOT"/templates/ml-docs/*.md; do
    [ -e "$src" ] || continue
    if [ ! -f "$PROJECT_DIR/docs/$(basename "$src")" ]; then
      missing_docs=$((missing_docs + 1))
    fi
  done
  if [ "$missing_docs" -eq 0 ]; then
    ok "ml docs scaffold complete under docs/"
  else
    warn "$missing_docs ml doc template(s) missing under docs/; re-run: apply-project-template.sh ml $PROJECT_DIR"
  fi

  for entry in data/ outputs/ checkpoints/; do
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
  # hold mid-project, not only at scaffold time.
  stray_py="$(find "$PROJECT_DIR" -maxdepth 1 -name '*.py' -type f ! -name 'setup.py' -printf '%f ' 2>/dev/null || true)"
  if [ -n "$stray_py" ]; then
    warn "top-level python files (move into src/ or scripts/): $stray_py"
  else
    ok "no stray top-level python files"
  fi

  stray_md="$(find "$PROJECT_DIR" -maxdepth 2 \( \
      -name '.git' -o -name '.venv' -o -name 'node_modules' -o -name 'docs' \
    \) -prune -o -name '*.md' -type f \
      ! -name 'README*.md' ! -name 'AGENTS.md' ! -name 'CLAUDE.md' ! -name 'PROJECT.md' \
      -printf '%P ' 2>/dev/null || true)"
  if [ -n "$stray_md" ]; then
    warn "markdown outside docs/ (move there): $stray_md"
  else
    ok "markdown files live under docs/"
  fi

  stray_nb="$(find "$PROJECT_DIR" -maxdepth 3 \( \
      -name '.git' -o -name '.venv' -o -name 'node_modules' -o -name 'notebooks' \
    \) -prune -o -name '*.ipynb' -type f -printf '%P ' 2>/dev/null || true)"
  if [ -n "$stray_nb" ]; then
    warn "notebooks outside notebooks/ (move there): $stray_nb"
  else
    ok "notebooks live under notebooks/"
  fi

  if [ -f "$PROJECT_DIR/pyproject.toml" ] && [ ! -d "$PROJECT_DIR/src" ]; then
    warn "pyproject.toml without src/ layout; ml rules expect src/<package>/"
  fi

  if git -C "$PROJECT_DIR" rev-parse --git-dir >/dev/null 2>&1; then
    tracked_ignored="$(git -C "$PROJECT_DIR" ls-files -- data outputs checkpoints wandb runs 2>/dev/null | head -3 || true)"
    if [ -n "$tracked_ignored" ]; then
      warn "tracked files inside gitignored dirs (committed before ignore?): $(printf '%s' "$tracked_ignored" | tr '\n' ' ')"
    else
      ok "no tracked files inside data/outputs/checkpoints/wandb/runs"
    fi

    big_tracked="$(git -C "$PROJECT_DIR" ls-files -z 2>/dev/null |
      (cd "$PROJECT_DIR" && xargs -0 du -b 2>/dev/null || true) |
      awk '$1 > 10*1024*1024 { print $2 }' | head -3 || true)"
    if [ -n "$big_tracked" ]; then
      warn "tracked files over 10MB (data/checkpoints belong outside git): $(printf '%s' "$big_tracked" | tr '\n' ' ')"
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
