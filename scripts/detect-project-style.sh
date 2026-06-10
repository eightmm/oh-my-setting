#!/usr/bin/env bash
set -euo pipefail

DIR="${1:-$PWD}"

exists_any() {
  local pattern
  for pattern in "$@"; do
    if find "$DIR" -maxdepth 3 \( \
        -name ".git" -o -name ".venv" -o -name "node_modules" -o \
        -name "__pycache__" -o -name "tmp" -o -name "backups" \
      \) -prune -o -name "$pattern" -print -quit | grep -q .; then
      return 0
    fi
  done
  return 1
}

has_code_text() {
  if command -v rg >/dev/null 2>&1; then
    # Search from inside DIR so exclude globs match project-relative paths,
    # not absolute path segments (e.g. a project located under /tmp).
    (cd "$DIR" && rg -q "$1" . \
      -g '*.py' -g '*.sh' -g '*.yaml' -g '*.yml' -g '*.toml' \
      -g '!scripts/detect-project-style.sh' \
      -g '!scripts/doctor.sh' \
      -g '!**/.git/**' -g '!**/.venv/**' -g '!**/node_modules/**' \
      -g '!**/__pycache__/**' -g '!**/tmp/**' -g '!**/backups/**' 2>/dev/null)
  else
    find "$DIR" -maxdepth 3 \( \
      -name ".git" -o -name ".venv" -o -name "node_modules" -o \
      -name "__pycache__" -o -name "tmp" -o -name "backups" \
    \) -prune -o -type f \( -name '*.py' -o -name '*.sh' -o -name '*.yaml' -o -name '*.yml' -o -name '*.toml' \) \
      ! -path '*/scripts/detect-project-style.sh' \
      ! -path '*/scripts/doctor.sh' \
      -exec grep -Eq "$1" {} \; -print -quit 2>/dev/null | grep -q .
  fi
}

style="general"

# configs/ and config.yaml alone are not ML signals; require ML filenames or ML code text.
if exists_any "train.py" "infer.py" "inference.py" "dataset.py" "dataloader.py" "model.py"; then
  style="ml"
elif has_code_text "torch|tensorflow|jax|sklearn|DataLoader|LightningModule|nn\\.Module"; then
  style="ml"
fi

echo "$style"
