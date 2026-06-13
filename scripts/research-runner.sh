#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

QUESTION=""
HYPOTHESIS=""
PREDICTION=""
BASELINE=""
METRIC=""
SUCCESS=""
CHANGE=""
METRICS_FILE=""
LEDGER=""
GATE=1
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: research-runner.sh [options] -- <command...>

Gate an ML/research experiment with a compact pre-registration, then launch it
through run-ledger.sh so the command, commit, exit code, and metrics are kept in
one ledger row. This is intentionally a thin wrapper, not a tuner or scheduler.

Required options:
  --question TEXT     Specific question this run answers.
  --hypothesis TEXT   Falsifiable claim under test.
  --prediction TEXT   Expected direction/magnitude before seeing results.
  --baseline TEXT     Ledger row, config, or result being compared against.
  --metric TEXT       Primary metric and split, e.g. "val_auc/scaffold".
  --success TEXT      Pre-registered success threshold or abandon condition.
  --change TEXT       Single intended independent variable/change.

Options:
  --metrics PATH      Metrics JSON emitted by the command; passed to run-ledger.
  --file PATH         Ledger path. Default: docs/EXPERIMENTS.jsonl.
  --no-gate           Skip run-ledger's scripts/check.sh pre-flight gate.
  --dry-run           Validate and print the planned ledger note; do not run.
  -h, --help          Show help.

The command line and note are recorded. Do not put secrets in either.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

need_text() {
  local name="$1"
  local value="$2"
  [ -n "$value" ] || fail "$name is required"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --question)
      [ "$#" -ge 2 ] || fail "--question requires text"
      QUESTION="$2"
      shift 2
      ;;
    --hypothesis)
      [ "$#" -ge 2 ] || fail "--hypothesis requires text"
      HYPOTHESIS="$2"
      shift 2
      ;;
    --prediction)
      [ "$#" -ge 2 ] || fail "--prediction requires text"
      PREDICTION="$2"
      shift 2
      ;;
    --baseline)
      [ "$#" -ge 2 ] || fail "--baseline requires text"
      BASELINE="$2"
      shift 2
      ;;
    --metric)
      [ "$#" -ge 2 ] || fail "--metric requires text"
      METRIC="$2"
      shift 2
      ;;
    --success)
      [ "$#" -ge 2 ] || fail "--success requires text"
      SUCCESS="$2"
      shift 2
      ;;
    --change)
      [ "$#" -ge 2 ] || fail "--change requires text"
      CHANGE="$2"
      shift 2
      ;;
    --metrics)
      [ "$#" -ge 2 ] || fail "--metrics requires path"
      METRICS_FILE="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      LEDGER="$2"
      shift 2
      ;;
    --no-gate)
      GATE=0
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    *)
      fail "unknown argument before --: $1"
      ;;
  esac
done

need_text "--question" "$QUESTION"
need_text "--hypothesis" "$HYPOTHESIS"
need_text "--prediction" "$PREDICTION"
need_text "--baseline" "$BASELINE"
need_text "--metric" "$METRIC"
need_text "--success" "$SUCCESS"
need_text "--change" "$CHANGE"
[ "$#" -gt 0 ] || fail "command after -- is required"

note="Q: $QUESTION | H: $HYPOTHESIS | P: $PREDICTION | B: $BASELINE | M: $METRIC | S: $SUCCESS | C: $CHANGE"
max_chars="${OMS_RESEARCH_NOTE_MAX_CHARS:-1600}"
if [ "$(printf '%s' "$note" | wc -c | tr -d ' ')" -gt "$max_chars" ]; then
  fail "pre-registration note exceeds ${max_chars} bytes; shorten the fields"
fi

note_file="$(mktemp)" || fail "mktemp failed"
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$note_file"
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
printf '%s\n' "$note" > "$note_file"
if agent_memory_file_has_sensitive_content "$note_file"; then
  fail "pre-registration contains sensitive-looking content"
fi

cmd=("$ROOT/scripts/run-ledger.sh" --note "$note")
[ -n "$LEDGER" ] && cmd+=(--file "$LEDGER")
[ -n "$METRICS_FILE" ] && cmd+=(--metrics "$METRICS_FILE")
[ "$GATE" -eq 0 ] && cmd+=(--no-gate)
cmd+=(-- "$@")

if [ "$DRY_RUN" -eq 1 ]; then
  printf 'research-runner: dry-run\n'
  printf 'note: %s\n' "$note"
  printf 'command:'
  printf ' %q' "${cmd[@]}"
  printf '\n'
  exit 0
fi

printf 'research-runner: launching registered experiment\n' >&2
printf 'research-runner: %s\n' "$note" >&2
"${cmd[@]}"
