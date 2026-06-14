#!/usr/bin/env bash
set -euo pipefail

# A coordination layer ABOVE single tasks: a shared study board so three agents
# do not duplicate experiments. The run ledger records what executed; this
# records what is *intended* — a hypothesis, its owner, its lifecycle
# (claimed -> running -> done/aborted), and its result. `claim` refuses an id
# already active (the duplicate-run guard), with stale-claim recovery so a dead
# owner does not wedge the board. Append-only events are the source of truth;
# the current view is derived by replay (last status per id wins).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT_LIB/file-lock.sh"

BOARD="${OMS_EXPERIMENT_BOARD:-.oms/experiments.jsonl}"
CLAIM_TTL="${OMS_EXPERIMENT_CLAIM_TTL:-86400}"
SCHEMA=1
AGENT_LABEL="${OMS_AGENT:-agent}"

ID=""
HYPOTHESIS=""
BASELINE=""
RESULT=""
NEXT=""
JOB=""
REASON=""
NOTE=""
FORCE=0
SHOW_ALL=0
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: experiment-board.sh claim  --hypothesis TEXT [--id ID] [--baseline TEXT] [--owner NAME] [--force]
       experiment-board.sh start  --id ID [--job JOBID]
       experiment-board.sh finish --id ID [--result TEXT] [--next TEXT]
       experiment-board.sh abort  --id ID [--reason TEXT]
       experiment-board.sh list   [--all]
       experiment-board.sh show   --id ID

Shared experiment study board so agents do not duplicate runs. Events are
append-only in .oms/experiments.jsonl; the live view is derived by replay.

claim   Register an intended experiment (status=claimed). Refuses an id that is
        already claimed/running unless the claim is stale (older than
        OMS_EXPERIMENT_CLAIM_TTL, default 1d) or --force is given.
start   Mark an experiment running, optionally attaching a Slurm/queue job id.
finish  Mark done with an optional result summary and next-step decision.
abort   Mark aborted with an optional reason.
list    Show active experiments (claimed/running); --all includes finished.
show    Print the full event history for one experiment.

Without --owner, the owner is $OMS_AGENT (default "agent").
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

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

slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cs '[:alnum:]' '-' \
    | sed 's/^-//;s/-$//;s/--*/-/g' | cut -c1-40
}

board_append() {
  local file="$1"
  local row_file="$2"
  cat "$row_file" >> "$file"
}

# Emit the derived current record for one id as JSON (or empty if unknown).
current_record() {
  local id="$1"
  [ -f "$BOARD" ] || return 0
  python3 - "$BOARD" "$id" <<'PY'
import json, sys
board, target = sys.argv[1], sys.argv[2]
cur = None
for line in open(board, encoding="utf-8", errors="replace"):
    try:
        e = json.loads(line)
    except Exception:
        continue
    if e.get("id") != target:
        continue
    if cur is None:
        cur = {"id": target}
    for k, v in e.items():
        if v == "" or v is None:
            continue
        if k == "owner" and "owner" in cur:
            continue  # owner is the claimer; later events do not reassign it
        cur[k] = v
    cur["status"] = e.get("status", cur.get("status"))
if cur:
    print(json.dumps(cur, ensure_ascii=False))
PY
}

append_event() {
  # append_event STATUS  (uses the global field vars)
  local status="$1"
  local ts row_tmp
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  mkdir -p "$(dirname "$BOARD")"
  agent_memory_ensure_oms_ignore_for_path "$(dirname "$BOARD")" 2>/dev/null || true
  row_tmp="$(mktemp)" || fail "mktemp failed"
  OMS_HYP="$HYPOTHESIS" OMS_BASE="$BASELINE" OMS_RESULT="$RESULT" OMS_NEXT="$NEXT" \
  python3 - "$SCHEMA" "$ID" "$status" "$ts" "$AGENT_LABEL" "$JOB" "$REASON" \
    > "$row_tmp" <<'PY'
import json, os, sys
a = sys.argv[1:]
e = {"schema": int(a[0]), "id": a[1], "status": a[2], "ts": a[3], "owner": a[4]}
if a[5]: e["job"] = a[5]
if a[6]: e["reason"] = a[6]
for key, env in (("hypothesis","OMS_HYP"),("baseline","OMS_BASE"),
                 ("result","OMS_RESULT"),("next","OMS_NEXT")):
    v = os.environ.get(env, "")
    if v:
        e[key] = v
print(json.dumps(e, ensure_ascii=False))
PY
  SCAN_FILE="$row_tmp"
  if agent_memory_file_has_sensitive_content "$row_tmp"; then
    echo "experiment-board: warning: event looks sensitive; recorded locally under .oms" >&2
  fi
  SCAN_FILE=""
  oms_with_file_lock "$BOARD" board_append "$BOARD" "$row_tmp"
  rm -f "$row_tmp"
  # Thin-spine join: link this lifecycle event to the active run id when set.
  if [ -n "${OMS_RUN_ID:-}" ]; then
    "$ROOT/scripts/oms-run.sh" link --tool experiment-board --event "$status" \
      --detail "$ID" >/dev/null 2>&1 || true
  fi
}

cmd_claim() {
  parse_args "$@"
  [ -n "$HYPOTHESIS" ] || fail "claim requires --hypothesis"
  [ -n "$ID" ] || ID="$(slugify "$HYPOTHESIS")"
  [ -n "$ID" ] || fail "could not derive an id; pass --id"

  local cur status owner ts
  cur="$(current_record "$ID")"
  if [ -n "$cur" ]; then
    status="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("status",""))')"
    owner="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("owner",""))')"
    ts="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("ts",""))')"
    if [ "$status" = "claimed" ] || [ "$status" = "running" ]; then
      local stale=0
      if [ "$FORCE" != 1 ]; then
        # Stale recovery: a claim (not yet running) older than TTL is reclaimable.
        if [ "$status" = "claimed" ]; then
          local age
          age="$(OMS_TS="$ts" python3 -c '
import calendar,os,time
try:
    t=calendar.timegm(time.strptime(os.environ["OMS_TS"],"%Y-%m-%dT%H:%M:%SZ"))
    print(int(time.time()-t))
except Exception:
    print(-1)')"
          [ "$age" -ge "$CLAIM_TTL" ] 2>/dev/null && stale=1
        fi
        if [ "$stale" = 0 ]; then
          echo "experiment-board: '$ID' already $status (owner: $owner). Use --force or pick a new --id." >&2
          exit 4
        fi
        echo "experiment-board: reclaiming stale claim '$ID' (was $owner, $status)" >&2
      fi
    fi
  fi
  append_event claimed
  echo "claimed: $ID (owner $AGENT_LABEL)" >&2
  printf '%s\n' "$ID"
}

require_active() {
  local cur
  cur="$(current_record "$ID")"
  [ -n "$cur" ] || fail "no such experiment: $ID"
}

cmd_start() {
  parse_args "$@"
  [ -n "$ID" ] || fail "start requires --id"
  require_active
  append_event running
  echo "running: $ID${JOB:+ (job $JOB)}" >&2
}

cmd_finish() {
  parse_args "$@"
  [ -n "$ID" ] || fail "finish requires --id"
  require_active
  append_event done
  echo "done: $ID" >&2
}

cmd_abort() {
  parse_args "$@"
  [ -n "$ID" ] || fail "abort requires --id"
  require_active
  append_event aborted
  echo "aborted: $ID" >&2
}

cmd_list() {
  parse_args "$@"
  [ -f "$BOARD" ] || { echo "no experiments"; return 0; }
  OMS_ALL="$SHOW_ALL" python3 - "$BOARD" <<'PY'
import json, os, sys
board = sys.argv[1]
show_all = os.environ.get("OMS_ALL") == "1"
cur = {}
order = []
for line in open(board, encoding="utf-8", errors="replace"):
    try:
        e = json.loads(line)
    except Exception:
        continue
    i = e.get("id")
    if not i:
        continue
    if i not in cur:
        cur[i] = {}
        order.append(i)
    for k, v in e.items():
        if v == "" or v is None:
            continue
        if k == "owner" and "owner" in cur[i]:
            continue  # owner is the claimer; later events do not reassign it
        cur[i][k] = v
    cur[i]["status"] = e.get("status", cur[i].get("status"))
for i in order:
    r = cur[i]
    st = r.get("status", "?")
    if not show_all and st in ("done", "aborted"):
        continue
    hyp = (r.get("hypothesis", "") or "")[:60]
    job = (" job=%s" % r["job"]) if r.get("job") else ""
    print("%-40s %-8s owner=%s%s  %s" % (i, st, r.get("owner", "?"), job, hyp))
PY
}

cmd_show() {
  parse_args "$@"
  [ -n "$ID" ] || fail "show requires --id"
  [ -f "$BOARD" ] || fail "no board at $BOARD"
  OMS_ID="$ID" python3 - "$BOARD" <<'PY'
import json, os, sys
board = sys.argv[1]
target = os.environ["OMS_ID"]
found = False
for line in open(board, encoding="utf-8", errors="replace"):
    try:
        e = json.loads(line)
    except Exception:
        continue
    if e.get("id") != target:
        continue
    found = True
    extra = ""
    for k in ("job", "result", "next", "reason", "hypothesis", "baseline"):
        if e.get(k):
            extra += "  %s=%s" % (k, e[k])
    print("%s  %-8s owner=%s%s" % (e.get("ts"), e.get("status"), e.get("owner"), extra))
if not found:
    sys.stderr.write("no such experiment: %s\n" % target)
    sys.exit(2)
PY
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --id) [ "$#" -ge 2 ] || fail "--id requires a value"; ID="$2"; shift 2 ;;
      --hypothesis) [ "$#" -ge 2 ] || fail "--hypothesis requires text"; HYPOTHESIS="$2"; shift 2 ;;
      --baseline) [ "$#" -ge 2 ] || fail "--baseline requires text"; BASELINE="$2"; shift 2 ;;
      --result) [ "$#" -ge 2 ] || fail "--result requires text"; RESULT="$2"; shift 2 ;;
      --next) [ "$#" -ge 2 ] || fail "--next requires text"; NEXT="$2"; shift 2 ;;
      --job) [ "$#" -ge 2 ] || fail "--job requires an id"; JOB="$2"; shift 2 ;;
      --reason) [ "$#" -ge 2 ] || fail "--reason requires text"; REASON="$2"; shift 2 ;;
      --owner) [ "$#" -ge 2 ] || fail "--owner requires a name"; AGENT_LABEL="$2"; shift 2 ;;
      --force) FORCE=1; shift ;;
      --all) SHOW_ALL=1; shift ;;
      *) fail "unknown argument: $1" ;;
    esac
  done
}

case "${1:-}" in
  claim) shift; cmd_claim "$@" ;;
  start) shift; cmd_start "$@" ;;
  finish) shift; cmd_finish "$@" ;;
  abort) shift; cmd_abort "$@" ;;
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
