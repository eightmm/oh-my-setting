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
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

# Anchored to the git worktree root so the board does not fork per subdirectory.
STATE_ROOT="$(oms_repo_root "$PWD")"
BOARD="${OMS_EXPERIMENT_BOARD:-$STATE_ROOT/.oms/experiments.jsonl}"
CLAIM_TTL="${OMS_EXPERIMENT_CLAIM_TTL:-86400}"
SCHEMA=1
AGENT_LABEL="$(oms_detect_agent)"

ID=""
HYPOTHESIS=""
BASELINE=""
RESULT=""
NEXT=""
JOB=""
REASON=""
FORCE=0
TOUCH_APPEND=0
SHOW_ALL=0
STALE_ONLY=0
OWNER_FILTER=""
SCAN_FILE=""
cleanup_done=0

usage() {
  cat <<'EOF'
Usage: experiment-board.sh claim  --hypothesis TEXT [--id ID] [--baseline TEXT] [--owner NAME] [--force]
       experiment-board.sh start  --id ID [--job JOBID]
       experiment-board.sh touch  --id ID
       experiment-board.sh finish --id ID [--result TEXT] [--next TEXT]
       experiment-board.sh abort  --id ID [--reason TEXT]
       experiment-board.sh list   [--all] [--stale] [--owner NAME]
       experiment-board.sh show   --id ID

Shared experiment study board so agents do not duplicate runs. Events are
append-only in .oms/experiments.jsonl; the live view is derived by replay.

claim   Register an intended experiment (status=claimed). Refuses an id that is
        already claimed/running unless the claim is stale (older than
        OMS_EXPERIMENT_CLAIM_TTL, default 1d) or --force is given.
start   Mark an experiment running, optionally attaching a Slurm/queue job id.
touch   Heartbeat a claimed/running experiment: re-stamp its timestamp so a
        live owner's claim is not treated as stale/reclaimable mid-run.
finish  Mark done with an optional result summary and next-step decision.
abort   Mark aborted with an optional reason.
list    Show active experiments (claimed/running); --all includes finished.
        Claims older than the TTL are tagged STALE (reclaimable); --stale
        shows only those, --owner NAME filters by claim owner.
show    Print the full event history for one experiment.

Without --owner, the owner is the detected calling agent: $OMS_AGENT when
set, else the CLI's own env markers, else "agent".
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

claim_append() {
  local file="$1"
  local row_file="$2"
  local id="$3"
  local cur status owner ts stale age

  cur="$(current_record "$id")"
  if [ -n "$cur" ]; then
    status="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("status", ""))')"
    owner="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("owner", ""))')"
    ts="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("ts", ""))')"
    if [ "$status" = "claimed" ] || [ "$status" = "running" ]; then
      stale=0
      if [ "$FORCE" != 1 ]; then
        # Stale recovery: a claim (not yet running) older than TTL is reclaimable.
        if [ "$status" = "claimed" ]; then
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
          echo "experiment-board: '$id' already $status (owner: $owner). Use --force or pick a new --id." >&2
          exit 4
        fi
        echo "experiment-board: reclaiming stale claim '$id' (was $owner, $status)" >&2
      fi
    fi
  fi
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
        # Only a (re)claim reassigns the owner; a stale reclaim by a new agent
        # must take ownership. touch/start/finish/abort keep the current owner.
        if k == "owner" and "owner" in cur and e.get("status") != "claimed":
            continue
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
  local lock_rc=0
  if [ "$status" = "claimed" ] && [ "$TOUCH_APPEND" != 1 ]; then
    oms_with_file_lock "$BOARD" claim_append "$BOARD" "$row_tmp" "$ID" || lock_rc=$?
  else
    # touch re-stamps an already-owned claim; the duplicate-claim guard would
    # (correctly) reject a fresh claim of the same id, so bypass it here.
    oms_with_file_lock "$BOARD" board_append "$BOARD" "$row_tmp" || lock_rc=$?
  fi
  rm -f "$row_tmp"
  [ "$lock_rc" = 0 ] || exit "$lock_rc"
  # Thin-spine join: link this lifecycle event to the active run id when set.
  if oms_effective_run_id "$STATE_ROOT" >/dev/null 2>&1; then
    "$ROOT/scripts/oms-run.sh" link --tool experiment-board --event "$status" \
      --detail "$ID" >/dev/null 2>&1 || true
  fi
}

cmd_claim() {
  parse_args "$@"
  [ -n "$HYPOTHESIS" ] || fail "claim requires --hypothesis"
  [ -n "$ID" ] || ID="$(slugify "$HYPOTHESIS")"
  [ -n "$ID" ] || fail "could not derive an id; pass --id"

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

cmd_touch() {
  parse_args "$@"
  [ -n "$ID" ] || fail "touch requires --id"
  local cur status
  cur="$(current_record "$ID")"
  [ -n "$cur" ] || fail "no such experiment: $ID"
  status="$(printf '%s' "$cur" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("status",""))')"
  case "$status" in
    claimed|running) ;;
    *) fail "touch: $ID is $status; only a claimed/running experiment can be touched" ;;
  esac
  # Re-stamp the current status with a fresh ts so the stale/reclaim clock
  # restarts for a live owner. Replay keeps the original owner/hypothesis.
  TOUCH_APPEND=1 append_event "$status"
  echo "touched: $ID ($status)" >&2
}

cmd_finish() {
  parse_args "$@"
  [ -n "$ID" ] || fail "finish requires --id"
  require_active
  append_event "done"
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
  OMS_ALL="$SHOW_ALL" OMS_STALE="$STALE_ONLY" OMS_OWNER="$OWNER_FILTER" \
    OMS_TTL="$CLAIM_TTL" python3 - "$BOARD" <<'PY'
import calendar, json, os, sys, time
board = sys.argv[1]
show_all = os.environ.get("OMS_ALL") == "1"
stale_only = os.environ.get("OMS_STALE") == "1"
owner_filter = os.environ.get("OMS_OWNER", "")
try:
    ttl = int(os.environ.get("OMS_TTL", "86400"))
except ValueError:
    ttl = 86400
now = time.time()

def is_stale(r):
    # Same rule as claim's stale recovery: only a claimed (not yet running)
    # entry older than the TTL is reclaimable.
    if r.get("status") != "claimed":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False
    return now - t >= ttl

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
        if k == "owner" and "owner" in cur[i] and e.get("status") != "claimed":
            continue  # only a (re)claim reassigns owner; later events keep it
        cur[i][k] = v
    cur[i]["status"] = e.get("status", cur[i].get("status"))
for i in order:
    r = cur[i]
    st = r.get("status", "?")
    if not show_all and st in ("done", "aborted"):
        continue
    stale = is_stale(r)
    if stale_only and not stale:
        continue
    if owner_filter and r.get("owner", "") != owner_filter:
        continue
    hyp = (r.get("hypothesis", "") or "")[:60]
    job = (" job=%s" % r["job"]) if r.get("job") else ""
    tag = " STALE" if stale else ""
    print("%-40s %-8s owner=%s%s%s  %s" % (i, st, r.get("owner", "?"), job, tag, hyp))
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
      --owner) [ "$#" -ge 2 ] || fail "--owner requires a name"; AGENT_LABEL="$2"; OWNER_FILTER="$2"; shift 2 ;;
      --force) FORCE=1; shift ;;
      --all) SHOW_ALL=1; shift ;;
      --stale) STALE_ONLY=1; shift ;;
      *) fail "unknown argument: $1" ;;
    esac
  done
}

case "${1:-}" in
  claim) shift; cmd_claim "$@" ;;
  start) shift; cmd_start "$@" ;;
  touch) shift; cmd_touch "$@" ;;
  finish) shift; cmd_finish "$@" ;;
  abort) shift; cmd_abort "$@" ;;
  list) shift; cmd_list "$@" ;;
  show) shift; cmd_show "$@" ;;
  -h|--help) usage ;;
  "") usage >&2; exit 2 ;;
  *) fail "unknown subcommand: $1" ;;
esac
