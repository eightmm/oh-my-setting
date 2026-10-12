#!/usr/bin/env bash

# Run existing CI partitions; this is orchestration, not another gate. CI
# spreads four focused lanes over four runners. One host runs every lane at
# once, so it splits the focused stages eight ways and scripts-smoke six: with
# four of each, the longest lane, not the longest stage, set the gate's wall
# time.
oms_parallel_terminate_running() {
  local pid running
  for pid in "$@"; do
    [ -n "$pid" ] || continue
    # Reaped jobs retain array slots while an earlier wait is blocked. Check
    # this shell's job table per signal; Bash cannot make check+kill atomic.
    for running in $(jobs -pr); do
      [ "$pid" = "$running" ] || continue
      kill -TERM -- "-$pid" 2>/dev/null || true
      break
    done
  done
}

oms_check_parallel() (
  set -euo pipefail
  local check="$1" logs="$2" skip_lint="${3:-0}"
  local lane i rc=0
  local pids=() names=()
  mkdir -p "$logs"
  cleanup_parallel() {
    local pid
    oms_parallel_terminate_running ${pids[@]+"${pids[@]}"}
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

# The operator suite keeps dependent phases in one section and assigns the
# independent fixtures to the remaining shards. Use the same process-group
# ownership as scripts-smoke so cancellation reaches shard descendants.
oms_operator_parallel() (
  set -euo pipefail
  local suite="$1" jobs="$2" logs index status=0 started=$SECONDS
  local pids=()
  logs="$(mktemp -d "${TMPDIR:-/tmp}/oms-operator-shards.XXXXXX")"
  cleanup_operator() {
    local pid
    trap - EXIT HUP INT TERM
    oms_parallel_terminate_running ${pids[@]+"${pids[@]}"}
    for pid in ${pids[@]+"${pids[@]}"}; do
      [ -z "$pid" ] || wait "$pid" 2>/dev/null || true
    done
    rm -rf "$logs"
  }
  trap cleanup_operator EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  set -m
  for ((index=1; index<=jobs; index++)); do
    bash "$suite" --shard "$index/$jobs" > "$logs/$index.log" 2>&1 &
    pids+=("$!")
  done
  set +m
  for ((index=1; index<=jobs; index++)); do
    wait "${pids[$((index - 1))]}" || status=1
    pids[$((index - 1))]=""
    cat "$logs/$index.log"
  done
  echo "operator-tools-shards: wall $((SECONDS - started))s ($jobs shards, exit $status)"
  exit "$status"
)
