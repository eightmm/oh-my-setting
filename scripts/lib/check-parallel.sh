#!/usr/bin/env bash

# Parallel runner for pure-Rust gate partitions: lint, focused shell suites,
# and Rust workspace tests run concurrently up to OMS_CHECK_JOBS slots.
oms_parallel_terminate_running() {
  local pid running
  for pid in "$@"; do
    [ -n "$pid" ] || continue
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

oms_check_parallel_run() {
  set -euo pipefail
  local check="$1" logs="$2" skip_lint="${3:-0}"
  local root="${check%/*/*}"
  local lane rc=0 next=0 total=0 running=0 budget="${OMS_CHECK_JOBS:-6}"
  local pids=() names=() planned=()
  local focused_total=4

  if [ "$skip_lint" != 1 ]; then planned+=(lint); fi
  for ((lane=1; lane<=focused_total; lane++)); do
    planned+=("focused-$lane")
  done
  planned+=(rust-tests)
  mkdir -p "$logs"

  cleanup_parallel() {
    local pid
    oms_parallel_terminate_running "${pids[@]}"
    for pid in "${pids[@]}"; do
      [ -z "$pid" ] || wait "$pid" 2>/dev/null || true
    done
  }
  trap cleanup_parallel EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  set -m

  case "$budget" in ''|0|*[!0-9]*) echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2 ;; esac
  [ "$budget" -gt 0 ] || { echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2; }
  if [ -n "${OMS_CHECK_JOBS:-}" ]; then
    export OMS_LINT_JOBS_MAX=1
  fi

  reap_parallel_slot() {
    local k j alive found=0 omitted separator
    while :; do
      alive=" $(jobs -pr 2>/dev/null | tr '\n' ' ')"
      for ((k=next; k<total; k++)); do
        [ -n "${pids[$k]}" ] || continue
        case "$alive" in *" ${pids[$k]} "*) continue ;; esac
        if ! wait "${pids[$k]}"; then
          pids[$k]=""
          oms_parallel_terminate_running "${pids[@]}"
          cat "$logs/${names[$k]}.log"
          omitted=""; separator=""
          for ((j=0; j<${#planned[@]}; j++)); do
            if [ "$j" -ge "$total" ] || [ -n "${pids[$j]:-}" ]; then
              omitted="$omitted$separator${planned[$j]}"
              separator=", "
            fi
          done
          echo "check: parallel gate stopped after ${names[$k]} failed${omitted:+; not run: $omitted}"
          exit 1
        fi
        pids[$k]=""
        running=$((running - 1))
        found=1
      done
      [ "$found" = 1 ] && break
      sleep 0.1
    done
    while [ "$next" -lt "$total" ] && [ -z "${pids[$next]}" ]; do
      cat "$logs/${names[$next]}.log"
      next=$((next + 1))
    done
  }

  if [ "$skip_lint" != 1 ]; then
    bash "$check" --lint-only > "$logs/lint.log" 2>&1 &
    pids+=("$!"); names+=(lint)
    total=$((total + 1)); running=$((running + 1))
  fi
  for ((lane=1; lane<=focused_total; lane++)); do
    if [ "$running" -ge "$budget" ]; then reap_parallel_slot; fi
    bash "$check" --focused-only --focused-lane "$lane/$focused_total" > "$logs/focused-$lane.log" 2>&1 &
    pids+=("$!"); names+=("focused-$lane")
    total=$((total + 1)); running=$((running + 1))
  done
  if [ "$running" -ge "$budget" ]; then reap_parallel_slot; fi
  (
    cd "$root"
    cargo +1.99.1 test --manifest-path rust/Cargo.toml --workspace --all-targets --locked --offline &&
      cargo +1.99.1 test --manifest-path rust/Cargo.toml --workspace --doc --locked --offline
  ) > "$logs/rust-tests.log" 2>&1 &
  pids+=("$!"); names+=(rust-tests)
  total=$((total + 1)); running=$((running + 1))

  while [ "$running" -gt 0 ]; do
    reap_parallel_slot
  done
  exit "$rc"
}
