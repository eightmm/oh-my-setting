#!/usr/bin/env bash
# Project verification contract. Agents run this before claiming work done.
#   fast      CPU-only, under 60 seconds, safe to run anytime.
#             Optional pytest paths/node IDs; no implicit full-suite collection.
#             A leading --jobs N (1-4) runs the independent compile, lint and
#             selected-test stages N at a time; the default is serial. Every
#             explicitly selected stage runs and any failure fails the check.
#             Without --jobs, the serial check stops at the first failure.
#   ml-smoke  ML interface smoke: import/config/data/model/loss one-batch checks.
#   gpu       Short GPU smoke; wrapped in a transient srun on Slurm machines.
# Fill the TODO blocks as the project takes shape. An empty contract fails
# loudly on purpose -- never let "no checks" look like a pass.
# Run project Setup (uv sync) first; CI syncs from its lockfile once. Checks
# reuse that environment rather than resolving/installing dependencies again.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

MODE="${1:-fast}"
[ "$#" -eq 0 ] || shift

FAST_DIRS=()
FAST_TESTS=()
STAGE_PIDS=()
STAGE_GROUPS=()
STAGE_LOGS=""

# exec preserves the stage process-group leader for cancellation.
run_stage() {
  case "$1" in
    compile) exec uv run --no-sync python -m compileall -q "${FAST_DIRS[@]}" ;;
    lint) exec uv run --no-sync ruff check . ;;
    test) exec uv run --no-sync python -m pytest -q "${FAST_TESTS[@]}" ;;
  esac
}

stop_stages() {
  local pid _attempt alive
  if [ "${#STAGE_GROUPS[@]}" -gt 0 ]; then
    for pid in "${STAGE_GROUPS[@]}"; do
      kill -TERM -- "-$pid" 2>/dev/null || true
    done
    # A wrapper can exit while its child ignores TERM. Check the group,
    # bound the grace period, and only then reap the direct stage processes.
    for _attempt in 1 2 3 4 5 6 7 8 9 10; do
      alive=0
      for pid in "${STAGE_GROUPS[@]}"; do
        if kill -0 -- "-$pid" 2>/dev/null; then alive=1; fi
      done
      [ "$alive" -ne 0 ] || break
      sleep 0.1
    done
    for pid in "${STAGE_GROUPS[@]}"; do
      kill -KILL -- "-$pid" 2>/dev/null || true
    done
  fi
  wait || true
  STAGE_PIDS=()
  STAGE_GROUPS=()
  [ -z "$STAGE_LOGS" ] || rm -rf "$STAGE_LOGS"
}

# Batches of at most $1 stages; Bash 3.2 has no wait -n.
run_stages() {
  local jobs="$1" failed="" i stage
  # Each stage has a process group, including subprocesses started by uv.
  set -m
  shift
  STAGE_LOGS="$(mktemp -d "${TMPDIR:-/tmp}/check-fast.XXXXXX")"
  trap stop_stages EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  while [ "$#" -gt 0 ]; do
    local batch=()
    while [ "$#" -gt 0 ] && [ "${#batch[@]}" -lt "$jobs" ]; do
      run_stage "$1" >"$STAGE_LOGS/$1.log" 2>&1 &
      STAGE_PIDS+=("$!")
      STAGE_GROUPS+=("$!")
      batch+=("$1")
      shift
    done
    i=0
    for stage in "${batch[@]}"; do
      if ! wait "${STAGE_PIDS[$i]}"; then
        failed="$failed $stage"
      fi
      unset "STAGE_PIDS[$i]"
      cat "$STAGE_LOGS/$stage.log"
      i=$((i + 1))
    done
    STAGE_PIDS=()
  done
  stop_stages
  trap - EXIT HUP INT TERM
  set +m
  if [ -n "$failed" ]; then
    echo "check fast: failed stages:$failed" >&2
    exit 1
  fi
}

run_fast() {
  local jobs=1 explicit_jobs=0 stage stages=()
  if [ "${1:-}" = "--jobs" ]; then
    explicit_jobs=1
    jobs="${2:-}"
    [ "$#" -lt 2 ] || shift
    shift
  fi
  case "$jobs" in
    [1-4]) ;;
    *) echo "check fast: --jobs takes 1-4, got '$jobs'" >&2; exit 2 ;;
  esac
  [ -d src ] && FAST_DIRS+=(src)
  [ -d scripts ] && FAST_DIRS+=(scripts)

  if [ "${#FAST_DIRS[@]}" -gt 0 ] && [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1; then
    stages+=(compile)
  fi
  if [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1 &&
    uv run --no-sync ruff --version >/dev/null 2>&1; then
    stages+=(lint)
  fi
  if [ "$#" -gt 0 ]; then
    command -v uv >/dev/null 2>&1 || { echo "selected tests require the project uv environment" >&2; exit 1; }
    FAST_TESTS=("$@")
    stages+=(test)
  fi
  if [ "$explicit_jobs" -eq 1 ]; then
    [ "${#stages[@]}" -eq 0 ] || run_stages "$jobs" "${stages[@]}"
  elif [ "${#stages[@]}" -gt 0 ]; then
    for stage in "${stages[@]}"; do
      (run_stage "$stage")
    done
  fi

  # TODO project: add an import smoke and a 1-batch forward/backward on
  # synthetic data (<60s, CPU), e.g.:
  #   uv run --no-sync python -c "from <package>.models import <Model>; ..."

  if [ "${#stages[@]}" -eq 0 ]; then
    echo "check fast: no checks ran; configure scripts/check.sh" >&2
    exit 1
  fi
  echo "check fast: ok"
}

run_ml_smoke() {
  local ran=0

  if [ -f scripts/ml_smoke.py ] && [ -f pyproject.toml ] && command -v uv >/dev/null 2>&1; then
    uv run --no-sync python scripts/ml_smoke.py
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
  fast) run_fast "$@" ;;
  ml-smoke) run_ml_smoke ;;
  gpu) run_gpu ;;
  *)
    echo "usage: scripts/check.sh fast [--jobs N] [PYTEST_PATH_OR_NODE_ID...] | ml-smoke | gpu" >&2
    exit 2
    ;;
esac
