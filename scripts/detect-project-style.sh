#!/usr/bin/env bash
set -euo pipefail

DIR="${1:-$PWD}"

exists_any() {
  local pattern
  for pattern in "$@"; do
    if find "$DIR" -maxdepth 3 -path "$DIR/.git" -prune -o -name "$pattern" -print -quit | grep -q .; then
      return 0
    fi
  done
  return 1
}

has_code_text() {
  if command -v rg >/dev/null 2>&1; then
    rg -q "$1" "$DIR" \
      -g '*.py' -g '*.sh' -g '*.yaml' -g '*.yml' -g '*.toml' \
      -g '!scripts/detect-project-style.sh' \
      -g '!scripts/doctor.sh' \
      -g '!/.git' -g '!/.venv' -g '!node_modules' -g '!__pycache__' 2>/dev/null
  else
    find "$DIR" -maxdepth 3 -type f \( -name '*.py' -o -name '*.sh' -o -name '*.yaml' -o -name '*.yml' -o -name '*.toml' \) \
      ! -path '*/scripts/detect-project-style.sh' \
      ! -path '*/scripts/doctor.sh' \
      -exec grep -q "$1" {} \; -print -quit 2>/dev/null | grep -q .
  fi
}

style="general"

if exists_any "train.py" "infer.py" "inference.py" "dataset.py" "dataloader.py" "model.py" "configs" "config.yaml" "config.yml"; then
  style="ml"
elif has_code_text "torch|tensorflow|jax|sklearn|DataLoader|LightningModule|nn\\.Module"; then
  style="ml"
fi

echo "$style"
