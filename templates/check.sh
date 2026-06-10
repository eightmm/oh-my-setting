#!/usr/bin/env bash
# Project verification contract. Agents run this before claiming work done.
#   fast      CPU-only, under 60 seconds, safe to run anytime.
#   ml-smoke  ML interface smoke: import/config/data/model/loss one-batch checks.
#   gpu       Short GPU smoke; wrapped in a transient srun on Slurm machines.
# Fill the TODO blocks as the project takes shape. An empty contract fails
# loudly on purpose -- never let "no checks" look like a pass.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

MODE="${1:-fast}"

run_fast() {
  local ran=0
  local dirs=()
  [ -d src ] && dirs+=(src)
  [ -d scripts ] && dirs+=(scripts)

  if [ "${#dirs[@]}" -gt 0 ] && [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1; then
    uv run python -m compileall -q "${dirs[@]}"
    ran=1
  fi
  if [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1 &&
    uv run ruff --version >/dev/null 2>&1; then
    uv run ruff check .
    ran=1
  fi

  # TODO project: add an import smoke and a 1-batch forward/backward on
  # synthetic data (<60s, CPU), e.g.:
  #   uv run python -c "from <package>.models import <Model>; ..."

  if [ "$ran" -eq 0 ]; then
    echo "check fast: no checks ran; configure scripts/check.sh" >&2
    exit 1
  fi
  echo "check fast: ok"
}

run_ml_smoke() {
  local ran=0

  if [ -f scripts/ml_smoke.py ] && [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1; then
    uv run python scripts/ml_smoke.py
    ran=1
  elif [ -f scripts/ml_smoke.py ]; then
    python3 scripts/ml_smoke.py
    ran=1
  fi

  # TODO ML project: implement scripts/ml_smoke.py or replace this function
  # with a CPU-only one-batch check covering config load, dataloader sample,
  # model forward, loss, backward, eval mode, and checkpoint save/load.
  if [ "$ran" -eq 0 ]; then
    echo "check ml-smoke: no ML smoke configured; add scripts/ml_smoke.py or edit scripts/check.sh" >&2
    exit 1
  fi
  echo "check ml-smoke: ok"
}

run_gpu() {
  # TODO project: replace with a real 1-batch GPU train/eval smoke.
  if command -v srun >/dev/null 2>&1; then
    srun --gres=gpu:1 --time=00:10:00 bash scripts/check.sh fast
  else
    bash scripts/check.sh fast
  fi
  echo "check gpu: ok"
}

case "$MODE" in
  fast) run_fast ;;
  ml-smoke) run_ml_smoke ;;
  gpu) run_gpu ;;
  *)
    echo "usage: scripts/check.sh [fast|ml-smoke|gpu]" >&2
    exit 2
    ;;
esac
