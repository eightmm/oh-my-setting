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
REASON=""
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: research-runner.sh [options] -- <command...>

Gate an ML/research experiment with a compact pre-registration, then launch it
through run-ledger.sh so the command, commit, exit code, and metrics are kept in
one ledger row. This is intentionally a thin wrapper, not a tuner or scheduler.

Required options (or --contract, which derives all seven):
  --question TEXT     Specific question this run answers.
  --hypothesis TEXT   Falsifiable claim under test.
  --prediction TEXT   Expected direction/magnitude before seeing results.
  --baseline TEXT     Ledger row, config, or result being compared against.
  --metric TEXT       Primary metric and split, e.g. "val_auc/scaffold".
  --success TEXT      Pre-registered success threshold or abandon condition.
  --change TEXT       Single intended independent variable/change.

Options:
  --contract PATH     ExperimentContract v2 JSON: the pre-registration fields
                      are derived from the validated contract and the contract
                      digest is recorded with the run, so the typed contract
                      and this front door are one system, not two.
  --metrics PATH      Metrics JSON emitted by the command; passed to run-ledger.
  --file PATH         Ledger path. Default: docs/EXPERIMENTS.jsonl.
  --no-gate           Skip run-ledger's scripts/check.sh pre-flight gate.
                      Unsafe override: requires --reason, recorded in the row.
  --reason TEXT       Justification for an unsafe --no-gate skip.
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
    --contract)
      [ "$#" -ge 2 ] || fail "--contract requires a path"
      CONTRACT_PATH="$2"
      shift 2
      ;;
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
    --reason)
      [ "$#" -ge 2 ] || fail "--reason requires text"
      REASON="$2"
      shift 2
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

if [ -n "${CONTRACT_PATH:-}" ]; then
  # The typed contract is the pre-registration: validate it through the same
  # code the runtime uses, then derive the seven registration fields. A
  # contract that does not validate refuses the launch.
  contract_fields="$(OMS_RR_CONTRACT="$CONTRACT_PATH" OMS_RR_LIB="$ROOT/scripts/lib" python3 - <<'PY_CONTRACT'
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, os.environ["OMS_RR_LIB"])
from oms_runtime.common import CoreError, read_json
from oms_runtime.experiment_contract import validate

try:
    raw = read_json(Path(os.environ["OMS_RR_CONTRACT"]))
    contract = validate(raw)
except CoreError as exc:
    sys.stderr.write("invalid experiment contract: %s\n" % exc)
    raise SystemExit(2)
success = contract.get("success", {})
success_text = "min_improvement=%s" % success.get("min_improvement", 0.0)
regressions = sorted(success.get("no_regression", {}))
if regressions:
    success_text += "; no_regression=" + ",".join(regressions)
fields = [
    contract["question"],
    contract["hypothesis"],
    contract["prediction"],
    json.dumps(contract["baseline"], sort_keys=True)[:300],
    "%s (%s)" % (contract["primary_metric"], contract["metrics"]["primary"]["direction"]),
    success_text,
    contract["treatment"]["independent_change"],
    contract["contract_digest"],
]
for value in fields:
    print(" ".join(str(value).split()))
PY_CONTRACT
)" || fail "contract validation failed: $CONTRACT_PATH"
  QUESTION="${QUESTION:-$(printf '%s\n' "$contract_fields" | sed -n 1p)}"
  HYPOTHESIS="${HYPOTHESIS:-$(printf '%s\n' "$contract_fields" | sed -n 2p)}"
  PREDICTION="${PREDICTION:-$(printf '%s\n' "$contract_fields" | sed -n 3p)}"
  BASELINE="${BASELINE:-$(printf '%s\n' "$contract_fields" | sed -n 4p)}"
  METRIC="${METRIC:-$(printf '%s\n' "$contract_fields" | sed -n 5p)}"
  SUCCESS="${SUCCESS:-$(printf '%s\n' "$contract_fields" | sed -n 6p)}"
  CHANGE="${CHANGE:-$(printf '%s\n' "$contract_fields" | sed -n 7p)}"
  CONTRACT_DIGEST="$(printf '%s\n' "$contract_fields" | sed -n 8p)"
fi
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
research_json_file="$(mktemp)" || { rm -f "$note_file"; fail "mktemp failed"; }
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$note_file" "$research_json_file"
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

OMS_RR_QUESTION="$QUESTION" OMS_RR_HYPOTHESIS="$HYPOTHESIS" \
  OMS_RR_CONTRACT_DIGEST="${CONTRACT_DIGEST:-}" \
  OMS_RR_PREDICTION="$PREDICTION" OMS_RR_BASELINE="$BASELINE" \
  OMS_RR_METRIC="$METRIC" OMS_RR_SUCCESS="$SUCCESS" OMS_RR_CHANGE="$CHANGE" \
  python3 - "$research_json_file" <<'PY'
import json
import os
import sys

fields = {
    "question": os.environ["OMS_RR_QUESTION"],
    "hypothesis": os.environ["OMS_RR_HYPOTHESIS"],
    "prediction": os.environ["OMS_RR_PREDICTION"],
    "baseline": os.environ["OMS_RR_BASELINE"],
    "metric": os.environ["OMS_RR_METRIC"],
    "success": os.environ["OMS_RR_SUCCESS"],
    "change": os.environ["OMS_RR_CHANGE"],
}
if os.environ.get("OMS_RR_CONTRACT_DIGEST"):
    fields["contract_digest"] = os.environ["OMS_RR_CONTRACT_DIGEST"]
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as handle:
    json.dump(fields, handle, ensure_ascii=False)
PY

cmd=("$ROOT/scripts/run-ledger.sh" --note "$note")
[ -n "$LEDGER" ] && cmd+=(--file "$LEDGER")
[ -n "$METRICS_FILE" ] && cmd+=(--metrics "$METRICS_FILE")
if [ "$GATE" -eq 0 ]; then
  need_text "--reason (required with --no-gate)" "$REASON"
  cmd+=(--no-gate --reason "$REASON")
fi
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
OMS_RESEARCH_METADATA_FILE="$research_json_file" \
  OMS_WORK_JOURNAL_EVENT_TYPE=experiment "${cmd[@]}"
