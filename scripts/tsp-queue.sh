#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

RUN_LEDGER="$ROOT/scripts/run-ledger.sh"
STATE_DIR="${OMS_TSP_STATE_DIR:-${XDG_RUNTIME_DIR:-$HOME/.cache}/oh-my-setting/tsp-queue}"
FALLBACK_DIR="${OMS_TSP_FALLBACK_DIR:-${XDG_RUNTIME_DIR:-$HOME/.cache}/oh-my-setting/tsp-fallback}"
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: tsp-queue.sh <subcommand> [options]

Single-machine job queue wrapper for tsp (task-spooler).

Subcommands:
  enqueue [--label L] [--slots N] [--ledger-note NOTE] -- CMD...
      Set tsp slots (default 1), enqueue CMD, print the tsp job id.
  list
      Show the tsp queue table.
  cancel <id>
      Stop a running job and remove it (tsp -k then tsp -r).
  cancel --all
      Stop and remove every listed job (tsp -k/-r per id), then clear finished rows with tsp -C.
  wait [<id>]
      Wait for a job (or last enqueued job) and record a run-ledger row.
  logs <id>
      Print stdout/stderr captured by tsp.
  -h, --help
      Show this help.

If tsp is missing, enqueue degrades to a nohup background run with no real
queue or slot limiting. Fallback state lives under
${XDG_RUNTIME_DIR:-$HOME/.cache}/oh-my-setting/tsp-fallback/.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  [ -z "$SCAN_FILE" ] || rm -f "$SCAN_FILE"
}

cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}

trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

tsp_available() {
  [ "${OMS_TSP_FORCE_FALLBACK:-0}" != "1" ] && command -v tsp >/dev/null 2>&1
}

warn_fallback() {
  echo "warning: tsp not found; install with apt-get install task-spooler or build from https://github.com/justanhduc/task-spooler" >&2
  echo "warning: degraded fallback active (no real queue / no slot limiting)" >&2
}

quote_words() {
  local word
  for word in "$@"; do
    printf ' %q' "$word"
  done
}

write_meta() {
  local file="$1"
  local meta_note="$2"
  shift 2

  {
    printf 'meta_note=%q\n' "$meta_note"
    printf 'meta_cmd=('
    quote_words "$@"
    printf ' )\n'
  } > "$file"
}

write_fallback_meta() {
  local file="$1"
  local f_pid="$2"
  local f_log="$3"
  local f_note="$4"
  local f_status="$5"
  local f_child="$6"
  shift 6

  {
    printf 'f_pid=%q\n' "$f_pid"
    printf 'f_log=%q\n' "$f_log"
    printf 'f_note=%q\n' "$f_note"
    printf 'f_status=%q\n' "$f_status"
    printf 'f_child=%q\n' "$f_child"
    printf 'f_cmd=('
    quote_words "$@"
    printf ' )\n'
  } > "$file"
}

load_meta() {
  local file="$1"
  [ -f "$file" ] || fail "no metadata for job: ${file##*/}"
  # shellcheck disable=SC1090
  . "$file"
}

scan_outbound() {
  local note="$1"
  shift

  SCAN_FILE="$(mktemp)" || fail "mktemp failed"
  {
    printf '%s\n' "$note"
    printf '%s\n' "$@"
  } > "$SCAN_FILE"
  if agent_memory_file_has_sensitive_content "$SCAN_FILE"; then
    echo "error: command or ledger note looks sensitive; pass credentials via environment or files, not command arguments" >&2
    exit 3
  fi
}

record_ledger() {
  local exit_code="$1"
  local note="$2"
  shift 2

  OMS_RUN_LEDGER_GATE=0 OMS_RUN_LEDGER_STATUS_OVERRIDE="$exit_code" \
    "$RUN_LEDGER" --note "$note" -- "$@" >/dev/null
}

parse_status() {
  local text="$1"
  local parsed=""

  parsed="$(printf '%s\n' "$text" | sed -n 's/.*[Ee]xit[^0-9]*\([0-9][0-9]*\).*/\1/p' | sed -n '1p')"
  if [ -z "$parsed" ]; then
    parsed="$(printf '%s\n' "$text" | sed -n 's/^[[:space:]]*\([0-9][0-9]*\)[[:space:]]*$/\1/p' | sed -n '1p')"
  fi
  if [ -z "$parsed" ] && printf '%s\n' "$text" | grep -Eiq 'finished|done|success'; then
    parsed=0
  fi
  if [ -z "$parsed" ] && printf '%s\n' "$text" | grep -Eiq 'fail|error'; then
    parsed=1
  fi
  printf '%s\n' "${parsed:-0}"
}

require_id() {
  local id="$1"
  case "$id" in
    *[!0-9]*|"") fail "job id must be numeric" ;;
  esac
}

last_id() {
  [ -f "$STATE_DIR/last" ] || fail "no last tsp job recorded"
  sed -n '1p' "$STATE_DIR/last"
}

fallback_last_id() {
  [ -f "$FALLBACK_DIR/last" ] || fail "no last fallback job recorded"
  sed -n '1p' "$FALLBACK_DIR/last"
}

cmd_enqueue() {
  local label=""
  local slots=1
  local note=""
  local job_id

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --label)
        [ "$#" -ge 2 ] || fail "--label requires text"
        label="$2"
        shift 2
        ;;
      --slots)
        [ "$#" -ge 2 ] || fail "--slots requires count"
        case "$2" in *[!0-9]*|"") fail "--slots requires a positive integer" ;; esac
        [ "$2" -gt 0 ] || fail "--slots requires a positive integer"
        slots="$2"
        shift 2
        ;;
      --ledger-note)
        [ "$#" -ge 2 ] || fail "--ledger-note requires text"
        note="$2"
        shift 2
        ;;
      --)
        shift
        break
        ;;
      *)
        fail "unknown enqueue argument before --: $1"
        ;;
    esac
  done
  [ "$#" -gt 0 ] || fail "enqueue requires CMD after --"
  scan_outbound "$note" "$@"

  if ! tsp_available; then
    fallback_enqueue "$note" "$@"
    return
  fi

  mkdir -p "$STATE_DIR"
  tsp -S "$slots" >/dev/null
  if [ -n "$label" ]; then
    job_id="$(tsp -L "$label" -- "$@")"
  else
    job_id="$(tsp -- "$@")"
  fi
  require_id "$job_id"
  write_meta "$STATE_DIR/$job_id" "$note" "$@"
  printf '%s\n' "$job_id" > "$STATE_DIR/last"
  printf '%s\n' "$job_id"
}

cmd_list() {
  if ! tsp_available; then
    warn_fallback
    fallback_list
    return
  fi
  tsp
}

cmd_cancel() {
  [ "$#" -eq 1 ] || fail "cancel requires <id> or --all"
  if ! tsp_available; then
    warn_fallback
    fallback_cancel "$1"
    return
  fi
  if [ "$1" = "--all" ]; then
    tsp | awk 'NR > 1 && $1 ~ /^[0-9]+$/ {print $1}' |
      while IFS= read -r job_id; do
        [ -n "$job_id" ] || continue
        # -k stops a running job; -r removes a still-queued one. A running
        # job ignores -r, so try both per id.
        tsp -k "$job_id" >/dev/null 2>&1 || true
        tsp -r "$job_id" >/dev/null 2>&1 || true
      done
    tsp -C
    return
  fi
  require_id "$1"
  # -r alone leaves a running job running; kill it first, then drop from queue.
  tsp -k "$1" >/dev/null 2>&1 || true
  tsp -r "$1"
}

cmd_wait() {
  local job_id="${1:-}"
  local status_text
  local exit_code
  local meta_note=""
  local -a meta_cmd=()

  [ "$#" -le 1 ] || fail "wait takes at most one job id"
  if ! tsp_available; then
    warn_fallback
    fallback_wait "$job_id"
    return
  fi
  if [ -z "$job_id" ]; then
    job_id="$(last_id)"
  fi
  require_id "$job_id"
  tsp -w "$job_id"
  status_text="$(tsp -s "$job_id" 2>/dev/null || true)"
  exit_code="$(parse_status "$status_text")"
  load_meta "$STATE_DIR/$job_id"
  record_ledger "$exit_code" "$meta_note" "${meta_cmd[@]}"
}

cmd_logs() {
  [ "$#" -eq 1 ] || fail "logs requires <id>"
  if ! tsp_available; then
    warn_fallback
    fallback_logs "$1"
    return
  fi
  require_id "$1"
  tsp -c "$1"
}

fallback_enqueue() {
  local note="$1"
  local run_id
  local log_file
  local status_file
  local child_file
  local monitor_pid
  local tag
  shift

  warn_fallback
  mkdir -p "$FALLBACK_DIR"
  tag="$$.${RANDOM:-0}"
  log_file="$FALLBACK_DIR/job.$tag.log"
  status_file="$FALLBACK_DIR/$tag.status"
  child_file="$FALLBACK_DIR/$tag.child"
  (
    set +e
    nohup "$@" > "$log_file" 2>&1 &
    printf '%s\n' "$!" > "$child_file"
    wait "$!"
    run_status=$?
    printf '%s\n' "$run_status" > "$status_file"
    record_ledger "$run_status" "$note" "$@"
    exit "$run_status"
  ) &
  monitor_pid=$!
  run_id="$monitor_pid"
  write_fallback_meta "$FALLBACK_DIR/$run_id.meta" "$run_id" "$log_file" "$note" "$status_file" "$child_file" "$@"
  printf '%s\n' "$run_id" > "$FALLBACK_DIR/last"
  printf '%s\n' "$run_id"
}

fallback_list() {
  local file
  local state
  local f_pid
  local f_log
  local f_status

  mkdir -p "$FALLBACK_DIR"
  printf 'ID\tSTATE\tLOG\n'
  for file in "$FALLBACK_DIR"/*.meta; do
    [ -f "$file" ] || continue
    f_pid=""
    f_log=""
    f_status=""
    # shellcheck disable=SC1090
    . "$file"
    state="done"
    if kill -0 "$f_pid" 2>/dev/null; then
      state="running"
    elif [ -n "$f_status" ] && [ -f "$f_status" ]; then
      state="exit $(sed -n '1p' "$f_status")"
    fi
    printf '%s\t%s\t%s\n' "$f_pid" "$state" "$f_log"
  done
}

fallback_cancel() {
  local job_id="$1"
  local file
  local child=""
  local f_pid
  local f_child

  mkdir -p "$FALLBACK_DIR"
  if [ "$job_id" = "--all" ]; then
    for file in "$FALLBACK_DIR"/*.meta; do
      [ -f "$file" ] || continue
      f_pid=""
      # shellcheck disable=SC1090
      . "$file"
      fallback_cancel "$f_pid" || true
    done
    return
  fi
  require_id "$job_id"
  if [ -f "$FALLBACK_DIR/$job_id.meta" ]; then
    f_child=""
    # shellcheck disable=SC1090
    . "$FALLBACK_DIR/$job_id.meta"
    [ -n "$f_child" ] && [ -f "$f_child" ] && child="$(sed -n '1p' "$f_child")"
  fi
  [ -z "$child" ] || kill "$child" 2>/dev/null || true
  kill "$job_id" 2>/dev/null || true
}

fallback_wait() {
  local job_id="$1"
  local status_file
  local exit_code
  local f_status

  if [ -z "$job_id" ]; then
    job_id="$(fallback_last_id)"
  fi
  require_id "$job_id"
  [ -f "$FALLBACK_DIR/$job_id.meta" ] || fail "no fallback job: $job_id"
  f_status=""
  # shellcheck disable=SC1090
  . "$FALLBACK_DIR/$job_id.meta"
  status_file="$f_status"
  while kill -0 "$job_id" 2>/dev/null; do
    sleep 1
  done
  [ -f "$status_file" ] || fail "fallback status not recorded for job: $job_id"
  exit_code="$(sed -n '1p' "$status_file")"
  printf '%s\n' "$exit_code"
}

fallback_logs() {
  local job_id="$1"
  local f_log
  require_id "$job_id"
  [ -f "$FALLBACK_DIR/$job_id.meta" ] || fail "no fallback job: $job_id"
  f_log=""
  # shellcheck disable=SC1090
  . "$FALLBACK_DIR/$job_id.meta"
  [ -f "$f_log" ] || fail "log file not found: $f_log"
  cat "$f_log"
}

case "${1:-}" in
  enqueue)
    shift
    cmd_enqueue "$@"
    ;;
  list)
    shift
    [ "$#" -eq 0 ] || fail "list takes no arguments"
    cmd_list
    ;;
  cancel)
    shift
    cmd_cancel "$@"
    ;;
  wait)
    shift
    cmd_wait "$@"
    ;;
  logs)
    shift
    cmd_logs "$@"
    ;;
  -h|--help)
    usage
    ;;
  "")
    usage >&2
    exit 2
    ;;
  *)
    fail "unknown subcommand: $1"
    ;;
esac
