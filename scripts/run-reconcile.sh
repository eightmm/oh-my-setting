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
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT_LIB/work-journal.sh"

# Anchored to the git worktree root so reconcile state does not fork per
# subdirectory. Env overrides stay verbatim.
STATE_ROOT="$(oms_repo_root "$PWD")"
LEDGER="$STATE_ROOT/docs/EXPERIMENTS.jsonl"
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
        job-digest summary to .oms/runs/reconcile.jsonl, with the cluster,
        submit/start/end times, AllocTRES, node list, requeue count and each
        array task's ending when sacct reports them. --memory also appends
        a compact note to shared agent memory. A job that ended PREEMPTED or
        NODE_FAIL but is still in the queue was requeued: it is reported as
        REQUEUED and not recorded until it really ends. This command only
        queries Slurm; it never submits, requeues or cancels.
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

TERMINAL_STATES="COMPLETED FAILED CANCELLED TIMEOUT OUT_OF_MEMORY NODE_FAIL BOOT_FAIL DEADLINE PREEMPTED"
# A requeue-capable ending: Slurm may put the same job id back in the queue.
REQUEUE_STATES="PREEMPTED NODE_FAIL"
MAX_TASK_ROWS=200

# One JSON object for a job id, read-only. sacct first (terminal states survive
# there), then squeue (still queued/running), else UNKNOWN.
#
# sacct is asked for the allocation rows (-X) of every record of the id (-D:
# a requeued job keeps its id and gains a record), in the parsable2 fields
# JobID,State,ExitCode,Elapsed,Cluster,Submit,Start,End,AllocTRES,NodeList. An
# array or hetjob returns one element per task: each element answers with its
# newest record, any non-terminal element keeps the job pending, else the first
# failure wins. sacct prints "CANCELLED by <uid>", so only the first word is
# the state. A row with fewer fields is read as State|ExitCode|Elapsed.
#
# PREEMPTED and NODE_FAIL end a record, not always the job: when squeue still
# lists the id, it was requeued, so the state becomes REQUEUED and nothing is
# recorded as finished yet.
job_query() {
  OMS_Q_TERMINAL="$TERMINAL_STATES" OMS_Q_REQUEUE="$REQUEUE_STATES" OMS_Q_MAX="$MAX_TASK_ROWS" \
    python3 - "$1" "$SACCT" "$SQUEUE" <<'PY'
import json, os, shutil, subprocess, sys

jid, sacct, squeue = sys.argv[1:4]
terminal = set(os.environ["OMS_Q_TERMINAL"].split())
requeue = set(os.environ["OMS_Q_REQUEUE"].split())
limit = int(os.environ["OMS_Q_MAX"])
FIELDS = "JobID,State,ExitCode,Elapsed,Cluster,Submit,Start,End,AllocTRES,NodeList"


def lines(argv):
    if not shutil.which(argv[0]):
        return []
    try:
        done = subprocess.run(argv, capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL)
    except OSError:
        return []
    return [line for line in done.stdout.splitlines() if line.strip()]


def known(value):
    return value if value and value not in ("Unknown", "None", "None assigned", "N/A") else ""


elements, order = {}, []
for index, line in enumerate(lines([sacct, "-X", "-D", "-j", jid, "--format=" + FIELDS, "-n", "-P"])):
    cells = line.split("|")
    if len(cells) >= 10:
        ident, cells = cells[0] or jid, cells[1:]
    else:
        ident, cells = "%s#%d" % (jid, index), cells + [""] * 9  # legacy row: its own element
    state = (cells[0].split() or [""])[0]
    if not state:
        continue
    if ident not in elements:
        elements[ident] = []
        order.append(ident)
    elements[ident].append({
        "id": ident, "state": state, "exit_code": cells[1], "elapsed": cells[2], "cluster": known(cells[3]),
        "submit": known(cells[4]), "start": known(cells[5]), "end": known(cells[6]),
        "alloc_tres": known(cells[7]), "nodelist": known(cells[8])})

out = {"job_id": jid, "state": "UNKNOWN", "exit_code": "", "elapsed": "", "source": "none"}
if order:
    newest = [elements[ident][-1] for ident in order]
    pending = next((row for row in newest if row["state"] not in terminal), None)
    failed = next((row for row in newest if row["state"] != "COMPLETED"), None)
    pick = pending or failed or newest[0]
    out.update(state=pick["state"], exit_code=pick["exit_code"], elapsed=pick["elapsed"], source="sacct")
    lead = elements[order[0]]
    for key in ("cluster", "submit", "start", "alloc_tres", "nodelist"):
        value = lead[0][key] if key == "submit" else lead[-1][key]
        if value:
            out[key] = value
    if pending is None:
        ends = sorted(row["end"] for row in newest if row["end"])
        if ends:
            out["end"] = ends[-1]
    attempts = max(len(rows) for rows in elements.values())
    if attempts > 1:
        out["attempts"], out["requeues"] = attempts, attempts - 1
    if len(order) > 1:
        counts = {}
        for row in newest:
            counts[row["state"]] = counts.get(row["state"], 0) + 1
        out["task_states"] = counts
        out["tasks"] = [{k: row[k] for k in ("id", "state", "exit_code", "elapsed")} for row in newest[:limit]]
        if len(newest) > limit:
            out["tasks_omitted"] = len(newest) - limit
    if pending is None and any(row["state"] in requeue for row in newest):
        queued = lines([squeue, "-j", jid, "-h", "-o", "%T"])
        if queued:
            # The accounting record of the next attempt may not exist yet: count it.
            out["ended_as"], out["state"], out["queue_state"] = out["state"], "REQUEUED", queued[0].strip()
            out["attempts"] = attempts + 1
            out["requeues"] = attempts
            out.pop("end", None)
else:
    queued = lines([squeue, "-j", jid, "-h", "-o", "%T"])
    if queued:
        out.update(state=queued[0].strip(), source="squeue")
print(json.dumps(out, ensure_ascii=False))
PY
}

# One field of a job_query object, empty when absent.
job_field() {
  python3 -c 'import json, sys
value = json.loads(sys.argv[1]).get(sys.argv[2], "")
print(value if isinstance(value, (str, int)) else json.dumps(value, sort_keys=True))' "$1" "$2"
}

# What scan shows after the state: requeue attempts and per-task state counts.
job_detail() {
  python3 -c 'import json, sys
job, parts = json.loads(sys.argv[1]), []
if job.get("state") == "REQUEUED":
    parts.append("requeued after %s, attempt %s" % (job.get("ended_as"), job.get("attempts")))
elif job.get("requeues"):
    parts.append("attempt %s" % job["attempts"])
if job.get("task_states"):
    parts.append("tasks: " + ", ".join("%s=%s" % item for item in sorted(job["task_states"].items())))
print("; ".join(parts))' "$1"
}

is_terminal() {
  case " $TERMINAL_STATES " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

# A job is recorded once per submission: the id alone for rows that carry no
# submit time (older rows, or sacct gave none), else the id and that time, so
# an id the scheduler reused or requeued after a recorded ending gets its row.
already_reconciled() {
  local jid="$1" submit="${2:-}"
  [ -f "$RECONCILE_FILE" ] || return 1
  python3 - "$RECONCILE_FILE" "$jid" "$submit" <<'PY'
import json, sys
target, submit = sys.argv[2], sys.argv[3]
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    if str(r.get("job_id")) == target and (not submit or not r.get("submit") or r.get("submit") == submit):
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
  local jid state info detail
  local any=0
  while IFS= read -r jid; do
    [ -n "$jid" ] || continue
    any=1
    info="$(job_query "$jid")"
    state="$(job_field "$info" state)"
    detail="$(job_detail "$info")"
    printf '%s\t%s%s\n' "$jid" "$state" "${detail:+$'\t'$detail}"
  done <<EOF
$(collect_job_ids)
EOF
  [ "$any" = 1 ] || echo "run-reconcile: no tracked job ids" >&2
}

cmd_apply() {
  local jid info state exit_code elapsed ts digest_path row_tmp
  local n_done=0 n_pending=0
  mkdir -p "$(dirname "$RECONCILE_FILE")" "$DIGEST_DIR"
  agent_memory_ensure_oms_ignore_for_path "$RECONCILE_FILE" 2>/dev/null || true
  while IFS= read -r jid; do
    [ -n "$jid" ] || continue
    info="$(job_query "$jid")"
    state="$(job_field "$info" state)"
    exit_code="$(job_field "$info" exit_code)"
    elapsed="$(job_field "$info" elapsed)"
    if already_reconciled "$jid" "$(job_field "$info" submit)"; then
      continue
    fi
    if ! is_terminal "$state"; then
      n_pending=$((n_pending + 1))
      [ "$state" != REQUEUED ] || echo "requeued: job $jid ($(job_detail "$info")); not recorded as finished"
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
    python3 - "$info" "$ts" "$digest_path" > "$row_tmp" <<'PY'
import json, sys
job = json.loads(sys.argv[1])
row = {"schema": 1, "job_id": job["job_id"], "state": job["state"], "exit_code": job["exit_code"],
       "elapsed": job["elapsed"], "source": job["source"], "reconciled_at": sys.argv[2], "digest": sys.argv[3]}
# Present only when sacct reported them: where and when the job ran, what it was
# allocated, how often it was requeued, and each array task's own ending.
for key in ("cluster", "submit", "start", "end", "alloc_tres", "nodelist", "attempts", "requeues",
            "task_states", "tasks", "tasks_omitted"):
    if key in job:
        row[key] = job[key]
print(json.dumps(row, ensure_ascii=False))
PY
    oms_with_file_lock "$RECONCILE_FILE" reconcile_append_row "$RECONCILE_FILE" "$row_tmp"
    work_journal_observe "$STATE_ROOT" run-reconcile "$row_tmp" \
      --record-path "$RECONCILE_FILE"
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
    extra = ""
    if r.get("cluster"): extra += "  on %s" % r["cluster"]
    if r.get("requeues"): extra += "  requeues=%s" % r["requeues"]
    if r.get("task_states"):
        extra += "  tasks=" + ",".join("%s:%s" % item for item in sorted(r["task_states"].items()))
    print("%s  job=%s  %s  exit=%s  %s%s" % (
        r.get("reconciled_at"), r.get("job_id"), r.get("state"),
        r.get("exit_code"), r.get("elapsed"), extra))
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
