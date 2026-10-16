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
  oms_check_parallel_run "$@"
)

# Called inside the wrapper or as a supervised background job. A second
# subshell would hide the cleanup owner behind an intermediate PID.
oms_check_parallel_run() {
  set -euo pipefail
  local check="$1" logs="$2" skip_lint="${3:-0}" queued="${4:-}"
  local lane i rc=0 next=0 total=0 budget="${OMS_CHECK_JOBS:-10}"
  local pids=() names=()
  mkdir -p "$logs"
  cleanup_parallel() {
    local pid
    oms_parallel_terminate_running ${pids[@]+"${pids[@]}"}
    for pid in ${pids[@]+"${pids[@]}"}; do
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
  case "$budget" in ''|0|*[!0-9]*) echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2 ;; esac
  [ "$budget" -gt 0 ] || { echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2; }
  if [ -n "${OMS_CHECK_JOBS:-}" ]; then
    # Each occupied outer slot owns one serial lane, including its linter,
    # smoke runner and operator fixtures. Nested fans cannot multiply the cap.
    export OMS_LINT_JOBS_MAX=1 OMS_SMOKE_JOBS=1 OMS_OPERATOR_JOBS=1
  fi
  reap_parallel_slot() {
    wait "${pids[$next]}" || rc=1
    pids[$next]=""
    cat "$logs/${names[$next]}.log"
    next=$((next + 1))
  }
  if [ "$skip_lint" != 1 ] && [ -z "$queued" ]; then
    bash "$check" --lint-only > "$logs/lint.log" 2>&1 &
    pids+=("$!"); names+=(lint)
    total=$((total + 1))
  fi
  for lane in 1 2 3 4 5 6 7 8; do
    if [ $((total - next)) -ge "$budget" ]; then reap_parallel_slot; fi
    if [ -n "$queued" ]; then
      oms_check_queued_lane "$queued" "$logs" "$lane" > "$logs/focused-$lane.log" 2>&1 &
    else
      bash "$check" --focused-only --focused-lane "$lane/8" > "$logs/focused-$lane.log" 2>&1 &
    fi
    pids+=("$!"); names+=("focused-$lane")
    total=$((total + 1))
  done
  if [ $((total - next)) -ge "$budget" ]; then reap_parallel_slot; fi
  if [ -n "$queued" ]; then
    oms_check_queued_lane "$queued" "$logs" smoke > "$logs/smoke.log" 2>&1 &
  else
    OMS_SMOKE_JOBS="${OMS_SMOKE_JOBS:-6}" bash "$check" --scripts-smoke-only > "$logs/smoke.log" 2>&1 &
  fi
  pids+=("$!"); names+=(smoke)
  total=$((total + 1))
  for ((i=next; i<total; i++)); do
    reap_parallel_slot
  done
  exit "$rc"
}

# Reuse stage() in a fork so the validated selection and provider-less PATH
# stay owned by the parent. No graph replanning or public bypass flags.
oms_check_queued_lane() {
  local queued="$1" logs="$2" lane="$3" index count arg position=0
  local args=()
  CHECK_RUNTIME="$logs/runtime-$lane"
  CHECK_STAGE_PID=""
  mkdir -p "$CHECK_RUNTIME"
  cleanup_queued_lane() {
    if [ -n "$CHECK_STAGE_PID" ]; then
      kill -TERM "$CHECK_STAGE_PID" 2>/dev/null || true
      wait "$CHECK_STAGE_PID" 2>/dev/null || true
    fi
  }
  trap cleanup_queued_lane EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  export XDG_CACHE_HOME="${OMS_CHECK_CACHE_HOME:-$CHECK_RUNTIME/cache}"
  export XDG_CONFIG_HOME="${OMS_CHECK_CONFIG_HOME:-$CHECK_RUNTIME/config}"
  export OMS_LOCK_DIR="${OMS_CHECK_LOCK_DIR:-$CHECK_RUNTIME/locks}"
  mkdir -p "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$OMS_LOCK_DIR"
  count="$(cat "$queued/count")"
  for ((index=1; index<=count; index++)); do
    args=()
    while IFS= read -r -d '' arg; do args+=("$arg"); done < "$queued/$index"
    if [ "${args[0]}" = affected-scripts-smoke ]; then
      [ "$lane" = smoke ] || continue
    else
      position=$((position + 1))
      [ "$lane" != smoke ] || continue
      [ $(( (position - 1) % 8 + 1 )) = "$lane" ] || continue
    fi
    stage "${args[@]}"
  done
}

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
