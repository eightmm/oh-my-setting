#!/usr/bin/env bash
set -euo pipefail

# Classify a repo as ml or general so templates and verify contracts match.

DIR="${1:-$PWD}"

if [ "$#" -gt 1 ]; then
  echo "error: too many arguments" >&2
  exit 2
fi

exists_ml_entry() {
  local pattern
  for pattern in train.py pretrain.py finetune.py infer.py inference.py; do
    if find "$DIR" -maxdepth 3 \( \
        -name ".git" -o -name ".venv" -o -name "node_modules" -o \
        -name "__pycache__" -o -name "tmp" -o -name "backups" \
      \) -prune -o -name "$pattern" -print -quit | grep -q .; then
      return 0
    fi
  done
  return 1
}

has_python_ml_import() {
  local pattern='(^|[[:space:];])(from|import)[[:space:]]+(torch|tensorflow|jax|sklearn|pytorch_lightning|lightning)([.[:space:],]|$)'

  # One implementation: the rg fast path disagreed with this one on depth,
  # hidden files, and gitignore, so style detection depended on whether rg
  # was on PATH (codex vendors one). CI runs find-only; find is the contract.
  find "$DIR" -maxdepth 3 \( \
    -name ".git" -o -name ".venv" -o -name "node_modules" -o \
    -name "__pycache__" -o -name "tmp" -o -name "backups" \
  \) -prune -o -type f -name '*.py' \
    -exec grep -Eq "$pattern" {} \; -print -quit 2>/dev/null | grep -q .
}

has_ml_dependency() {
  local pattern="(^|[\"'[:space:]-])(torch|tensorflow|jax|scikit-learn|pytorch-lightning|lightning)(\\[[^]]+\\])?([\"'[:space:]]*([<>=~!:]|$)|[\"'])"

  # Same single-implementation rule as has_python_ml_import above.
  find "$DIR" -maxdepth 3 \( \
    -name ".git" -o -name ".venv" -o -name "node_modules" -o \
    -name "__pycache__" -o -name "tmp" -o -name "backups" \
  \) -prune -o -type f \( \
    -name 'requirements*.txt' -o -name 'pyproject.toml' -o -name 'setup.cfg' -o \
    -name 'setup.py' -o -name 'environment*.yaml' -o -name 'environment*.yml' -o \
    -name 'Pipfile' -o -name 'poetry.lock' -o -name 'uv.lock' \
  \) -exec grep -Eq "$pattern" {} \; -print -quit 2>/dev/null | grep -q .
}

style="general"

# Shell harnesses often quote ML commands as fixtures. Only executable ML entry
# names, Python imports, and dependency declarations count as strong signals.
if exists_ml_entry; then
  style="ml"
elif has_python_ml_import || has_ml_dependency; then
  style="ml"
fi

echo "$style"
