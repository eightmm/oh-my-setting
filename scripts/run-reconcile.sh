#!/usr/bin/env bash
set -euo pipefail

# Long Slurm jobs outlive the episodic agent session that launched them. The
# run ledger records the launch (with slurm_job_id) but never the final state,
# so "is it done? did it OOM?" drifts and agents relaunch duplicates. This
# reconciles recorded job ids against sacct/squeue and writes the terminal
# outcome back to .oms/runs/reconcile.jsonl (and optionally shared memory), so
# the next agent — any of the three — sees ground truth without re-asking.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

# Anchored to the git worktree root so reconcile state does not fork per
# subdirectory. Env overrides stay verbatim.
STATE_ROOT="$(oms_repo_root "$PWD")"
LEDGER="${OMS_RECONCILE_LEDGER:-$STATE_ROOT/docs/EXPERIMENTS.jsonl}"
RECONCILE_FILE="${OMS_RECONCILE_FILE:-$STATE_ROOT/.oms/runs/reconcile.jsonl}"
DIGEST_DIR="${OMS_RECONCILE_DIGEST_DIR:-$STATE_ROOT/.oms/runs/reconcile}"
SACCT="${OMS_SACCT_CMD:-sacct}"
SQUEUE="${OMS_SQUEUE_CMD:-squeue}"
WRITE_MEMORY=0
AGENT_LABEL="$(oms_detect_agent)"

usage() {
  cat <<'EOF'
Usage: run-reconcile.sh scan   [--ledger FILE] [--job ID]
       run-reconcile.sh apply  [--ledger FILE] [--job ID] [--memory] [--agent NAME]
       run-reconcile.sh list   [N]

Reconcile launched Slurm jobs against their terminal state and record the
outcome so async runs are not lost between agent sessions.

scan    Print each tracked job id and its current Slurm state.
apply   For each FINISHED job not yet reconciled: record state + exit + a
        job-digest summary to .oms/runs/reconcile.jsonl. --memory also appends
        a compact note to shared agent memory.
list    Show the last N reconcile records (default 10).

Job ids come from non-empty slurm_job_id fields in the ledger
(default docs/EXPERIMENTS.jsonl), or a single --job ID.

Environment:
  OMS_SACCT_CMD / OMS_SQUEUE_CMD   Override the Slurm query binaries.
  OMS_RECONCILE_FILE               Output JSONL (default .oms/runs/reconcile.jsonl).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

reconcile_append_row() {
  local file="$1"
  local row_file="$2"
  cat "$row_file" >> "$file"
}

# Distinct non-empty slurm_job_id values from the ledger, launch order.
ledger_job_ids() {
  [ -f "$LEDGER" ] || return 0
  python3 - "$LEDGER" <<'PY'
import json, sys
seen = set()
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    jid = str(r.get("slurm_job_id") or "").strip()
    if jid and jid not in seen:
        seen.add(jid)
        print(jid)
PY
}

# Print: "<state>\t<exit>\t<elapsed>\t<source>" for one job id. Best-effort:
# sacct first (terminal states survive there), then squeue (still queued/
# running), else unknown.
job_state() {
  local jid="$1"
  local line=""
  if command -v "$SACCT" >/dev/null 2>&1; then
    line="$("$SACCT" -j "$jid" --format=State,ExitCode,Elapsed -n -P 2>/dev/null \
      | awk -F'|' 'NF>=1 && $1!="" {print; exit}')"
    if [ -n "$line" ]; then
      printf '%s\t%s\t%s\tsacct\n' \
        "$(printf '%s' "$line" | cut -d'|' -f1 | tr -d ' ')" \
        "$(printf '%s' "$line" | cut -d'|' -f2)" \
        "$(printf '%s' "$line" | cut -d'|' -f3)"
      return 0
    fi
  fi
  if command -v "$SQUEUE" >/dev/null 2>&1; then
    local st
    st="$("$SQUEUE" -j "$jid" -h -o '%T' 2>/dev/null | head -n1)"
    if [ -n "$st" ]; then
      printf '%s\t\t\tsqueue\n' "$st"
      return 0
    fi
  fi
  printf 'UNKNOWN\t\t\tnone\n'
}

is_terminal() {
  case "$1" in
    COMPLETED|FAILED|CANCELLED|CANCELLED+|TIMEOUT|OUT_OF_MEMORY|NODE_FAIL|BOOT_FAIL|DEADLINE|PREEMPTED)
      return 0 ;;
    *) return 1 ;;
  esac
}

already_reconciled() {
  local jid="$1"
  [ -f "$RECONCILE_FILE" ] || return 1
  python3 - "$RECONCILE_FILE" "$jid" <<'PY'
import json, sys
target = sys.argv[2]
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    if str(r.get("job_id")) == target:
        sys.exit(0)
sys.exit(1)
PY
}

collect_job_ids() {
  if [ -n "$ONE_JOB" ]; then
    printf '%s\n' "$ONE_JOB"
  else
    ledger_job_ids
  fi
}

cmd_scan() {
  local jid state
  local any=0
  while IFS= read -r jid; do
    [ -n "$jid" ] || continue
    any=1
    state="$(job_state "$jid" | cut -f1)"
    printf '%s\t%s\n' "$jid" "$state"
  done <<EOF
$(collect_job_ids)
EOF
  [ "$any" = 1 ] || echo "run-reconcile: no tracked job ids" >&2
}

cmd_apply() {
  local jid info state exit_code elapsed source ts digest_path row_tmp
  local n_done=0 n_pending=0
  mkdir -p "$(dirname "$RECONCILE_FILE")" "$DIGEST_DIR"
  agent_memory_ensure_oms_ignore_for_path "$RECONCILE_FILE" 2>/dev/null || true
  while IFS= read -r jid; do
    [ -n "$jid" ] || continue
    if already_reconciled "$jid"; then
      continue
    fi
    info="$(job_state "$jid")"
    state="$(printf '%s' "$info" | cut -f1)"
    exit_code="$(printf '%s' "$info" | cut -f2)"
    elapsed="$(printf '%s' "$info" | cut -f3)"
    source="$(printf '%s' "$info" | cut -f4)"
    if ! is_terminal "$state"; then
      n_pending=$((n_pending + 1))
      continue
    fi

    # Best-effort compact digest of the finished job (sacct + patterns + tail).
    digest_path="$DIGEST_DIR/$jid.md"
    if "$ROOT/scripts/job-digest.sh" "$jid" > "$digest_path" 2>/dev/null; then
      :
    else
      printf '# job %s digest unavailable\n' "$jid" > "$digest_path"
    fi

    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    row_tmp="$(mktemp)" || fail "mktemp failed"
    python3 - "$jid" "$state" "$exit_code" "$elapsed" "$source" "$ts" "$digest_path" \
      > "$row_tmp" <<'PY'
import json, sys
a = sys.argv[1:]
print(json.dumps({
    "schema": 1, "job_id": a[0], "state": a[1], "exit_code": a[2],
    "elapsed": a[3], "source": a[4], "reconciled_at": a[5], "digest": a[6],
}, ensure_ascii=False))
PY
    oms_with_file_lock "$RECONCILE_FILE" reconcile_append_row "$RECONCILE_FILE" "$row_tmp"
    rm -f "$row_tmp"
    n_done=$((n_done + 1))
    echo "reconciled: job $jid -> $state (exit $exit_code, $elapsed)"

    if [ "$WRITE_MEMORY" = 1 ]; then
      "$ROOT/scripts/agent-memory.sh" --repo . append --agent "$AGENT_LABEL" \
        --text "Slurm job $jid finished: $state (exit $exit_code, $elapsed). Digest: $digest_path" \
        >/dev/null 2>&1 || echo "run-reconcile: memory note skipped for $jid" >&2
    fi
  done <<EOF
$(collect_job_ids)
EOF
  echo "run-reconcile: $n_done reconciled, $n_pending still running/pending" >&2
}

cmd_list() {
  [ "$#" -le 1 ] || fail "list takes at most N"
  local n="${1:-10}"
  case "$n" in *[!0-9]*|"") fail "N must be a positive integer" ;; esac
  [ -f "$RECONCILE_FILE" ] || { echo "no reconcile records"; return 0; }
  tail -n "$n" "$RECONCILE_FILE" | python3 -c '
import json, sys
for line in sys.stdin:
    try: r = json.loads(line)
    except Exception: continue
    print("%s  job=%s  %s  exit=%s  %s" % (
        r.get("reconciled_at"), r.get("job_id"), r.get("state"),
        r.get("exit_code"), r.get("elapsed")))
'
}

ONE_JOB=""

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --ledger) [ "$#" -ge 2 ] || fail "--ledger requires a path"; LEDGER="$2"; shift 2 ;;
      --job) [ "$#" -ge 2 ] || fail "--job requires an id"; ONE_JOB="$2"; shift 2 ;;
      --memory) WRITE_MEMORY=1; shift ;;
      --agent) [ "$#" -ge 2 ] || fail "--agent requires a name"; AGENT_LABEL="$2"; shift 2 ;;
      *) fail "unknown argument: $1" ;;
    esac
  done
}

case "${1:-}" in
  scan) shift; parse_args "$@"; cmd_scan ;;
  apply) shift; parse_args "$@"; cmd_apply ;;
  list) shift; cmd_list "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
