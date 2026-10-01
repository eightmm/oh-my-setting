#!/usr/bin/env bash

# Run existing CI partitions; this is orchestration, not another gate. CI
# spreads four focused lanes over four runners. One host runs every lane at
# once, so it splits the focused stages eight ways and scripts-smoke six: with
# four of each, the longest lane, not the longest stage, set the gate's wall
# time.
oms_check_parallel() (
  set -euo pipefail
  local check="$1" logs="$2" skip_lint="${3:-0}"
  local lane i rc=0
  local pids=() names=()
  mkdir -p "$logs"
  cleanup_parallel() {
    local pid
    for pid in "${pids[@]}"; do
      [ -z "$pid" ] || kill -TERM -- "-$pid" 2>/dev/null || true
    done
    for pid in "${pids[@]}"; do
      [ -z "$pid" ] || wait "$pid" 2>/dev/null || true
    done
  }
  trap cleanup_parallel EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  # Plain background Bash jobs inherit ignored INT/QUIT. Job control gives
  # each lane a signal-capable process group, also used for cancellation.
  set -m
  if [ "$skip_lint" != 1 ]; then
    bash "$check" --lint-only > "$logs/lint.log" 2>&1 &
    pids+=("$!"); names+=(lint)
  fi
  for lane in 1 2 3 4 5 6 7 8; do
    bash "$check" --focused-only --focused-lane "$lane/8" > "$logs/focused-$lane.log" 2>&1 &
    pids+=("$!"); names+=("focused-$lane")
  done
  OMS_SMOKE_JOBS="${OMS_SMOKE_JOBS:-6}" bash "$check" --scripts-smoke-only > "$logs/smoke.log" 2>&1 &
  pids+=("$!"); names+=(smoke)
  for ((i=0; i<${#pids[@]}; i++)); do
    wait "${pids[$i]}" || rc=1
    pids[$i]=""
    cat "$logs/${names[$i]}.log"
  done
  exit "$rc"
)
