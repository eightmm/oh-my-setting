#!/usr/bin/env bash
set -euo pipefail

# Provenance for what a report says: which recorded run a stated number came
# from, and which script and runs produced a figure. Front doors are
# `oms run claims …` and `oms run figure …`.
#
# Both are records plus a read-only comparison. `claims check` compares each
# claimed number with the metric its capsule recorded; `figure verify`
# compares file digests. Neither runs a training command, a figure script or a
# model, and neither result is a reproduction or verification evidence.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

STATE_ROOT="$(oms_repo_root "$PWD")"
RUNS_DIR="${OMS_RUNS_DIR:-$STATE_ROOT/.oms/runs}"
SPINE="${OMS_RUN_INDEX:-$STATE_ROOT/.oms/runs/spine.jsonl}"
CLAIMS="$STATE_ROOT/.oms/claims.jsonl"
FIGURES="$STATE_ROOT/.oms/figures.jsonl"

usage() {
  cat <<'USAGE'
Usage: oms run claims add --metric KEY --value N --run ID [--tolerance T] [--where TEXT] [--id NAME]
       oms run claims check [ID] [--no-verify] [--strict] [--json]
       oms run claims list
       oms run figure record --output FILE --script FILE [--run ID]... [--command TEXT] [--id NAME]
       oms run figure verify [ID] [--json]

claims add     Record that a number stated in a report (--where, e.g.
               paper/results.tex:42) is metric KEY of run ID. ID is a capsule
               id or a run id whose capsule recorded the metric; an unknown
               run or a metric the capsule did not record is refused.
               Re-recording an --id replaces the earlier claim.
claims check   For each claim: match, mismatch (outside --tolerance, an
               absolute difference, default 0) or missing (capsule or metric
               gone). Notes single-seed runs, failed or dirty runs, and a
               capsule that `oms run capsule verify` no longer matches
               (stale). Exit 1 on a mismatch or missing claim; --strict also
               on a stale one; --no-verify skips the capsule comparison.
figure record  Record the output file's and the script's sha256, the input
               runs and, optionally, the command that made the figure. The
               command is stored as text and never run.
figure verify  current, or stale when the output or script changed or is gone
               or an input capsule is missing; prints the recorded command
               for a stale figure. Exit 1 when any figure is stale.

Records live in .oms/claims.jsonl and .oms/figures.jsonl (append-only, local).
Nothing here runs a command or a model, and a match is not a reproduction.
USAGE
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

run_lib() {
  OMS_CLAIMS_ROOT="$STATE_ROOT" OMS_CLAIMS_RUNS="$RUNS_DIR" OMS_CLAIMS_SPINE="$SPINE" \
    OMS_CLAIMS_CLAIMS="$CLAIMS" OMS_CLAIMS_FIGURES="$FIGURES" \
    OMS_CLAIMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_CLAIMS_AGENT="$(oms_detect_agent)" \
    OMS_CLAIMS_CAPSULE_TOOL="$ROOT/scripts/run-capsule.sh" \
    python3 "$ROOT_LIB/run_claims.py" "$@"
}

# One validated row from the library, scanned, then appended whole.
append_row() {
  local file="$1"
  shift
  local row_tmp
  row_tmp="$(mktemp)" || fail "mktemp failed"
  if ! run_lib "$@" > "$row_tmp"; then
    rm -f "$row_tmp"
    exit 2
  fi
  if agent_memory_file_has_sensitive_content "$row_tmp"; then
    rm -f "$row_tmp"
    fail "the record looks like it carries a credential or private path; nothing was written"
  fi
  mkdir -p "$(dirname "$file")"
  agent_memory_ensure_oms_ignore_for_path "$(dirname "$file")" 2>/dev/null || true
  cat "$row_tmp" >> "$file"
  rm -f "$row_tmp"
}

group="${1:-}"
action="${2:-}"
case "$group" in
  -h|--help) usage; exit 0 ;;
  claims|figure) ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $group" ;;
esac
shift
case "$action" in
  -h|--help) usage; exit 0 ;;
  "") usage >&2; exit 2 ;;
esac
shift

case "$group/$action" in
  claims/add) append_row "$CLAIMS" claims add "$@" ;;
  figure/record) append_row "$FIGURES" figure record "$@" ;;
  claims/check|claims/list|figure/verify) run_lib "$group" "$action" "$@" ;;
  *) fail "unknown $group action: $action" ;;
esac
