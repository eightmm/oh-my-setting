#!/usr/bin/env bash
set -euo pipefail

# One read-only view over the repo-local .oms state the three agents share, so
# any agent resuming a repo can answer "what is active, claimed, stale, pending
# review, open, or just changed?" in a single command instead of cat-ing five
# files. Pure query layer: it reads task/plan/board/spine/artifacts/change-guard
# and prints or emits JSON. It never mutates state and never orchestrates — the
# design keeps writers independent and coherence here, in the query.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

REPO="$PWD"
AS_JSON=0
REFRESH_CI=0

usage() {
  cat <<'EOF'
Usage: state.sh [--repo PATH] [--json]

Print a read-only dashboard of the shared .oms state for a repo: the active
task packet, plan tasks by state (stale claims flagged), the experiment board
(active + stale), task-scoped executors, the current/open runs, the latest
artifact-index rows, and whether a change-guard is active.

Options:
  --repo PATH   Repo to inspect (default: current directory; anchored to the
                git worktree root).
  --json        Emit a single JSON object (schema 1) instead of the text view.
  --refresh-ci  Best-effort `ci-status record` first (needs gh) so the CI
                section reflects the latest run instead of the last recording.
  -h, --help    Show this help.

Read-only: this never writes, claims, or launches anything (--refresh-ci is
the one opt-in exception: it appends the latest CI conclusion to .oms/ci.jsonl
before reading).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    --refresh-ci) REFRESH_CI=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"

# ci-status has a `record` mode but nothing calls it automatically; this is
# the read-side wiring so "oms state --refresh-ci" is one command, not two.
if [ "$REFRESH_CI" = 1 ]; then
  (cd "$REPO" && "$ROOT/scripts/ci-status.sh" record >/dev/null 2>&1) || true
fi

# Privacy state comes from project-private itself (one source of truth for what
# counts as tracked/hidden/exposed) rather than a second copy of the rules here.
PRIVATE_JSON="$("$ROOT/scripts/project-private.sh" --repo "$REPO" status --json 2>/dev/null || true)"

# The shared auto-update verdict rides the state so inbox stays a pure
# derivation of state while still surfacing a dying updater — the state
# an agent resuming here most needs to distrust.
AUTOUPDATE_ATTENTION="$("$ROOT/scripts/auto-update.sh" attention 2>/dev/null || true)"

RS_TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-repo-state.XXXXXX")"
trap 'rm -rf "$RS_TMP"' EXIT HUP INT TERM
LIFECYCLE_HEALTHY=1
"$ROOT/scripts/agent-events.sh" --repo "$REPO" list --json \
  > "$RS_TMP/lifecycle.json" 2>/dev/null || {
    LIFECYCLE_HEALTHY=0
    printf '[]\n' > "$RS_TMP/lifecycle.json"
  }
APPROVAL_HEALTHY=1
"$ROOT/scripts/approval-inbox.sh" --repo "$REPO" list --json \
  > "$RS_TMP/approvals.json" 2>/dev/null || {
    APPROVAL_HEALTHY=0
    printf '[]\n' > "$RS_TMP/approvals.json"
  }
FAILURE_HEALTHY=1
FAILURE_PHYSICAL=0
if [ -e "$REPO/.oms/failures.jsonl" ] || [ -L "$REPO/.oms/failures.jsonl" ]; then
  FAILURE_PHYSICAL=1
fi
if [ "$FAILURE_PHYSICAL" = 1 ] && [ ! -f "$REPO/.oms/failures.jsonl" ]; then
  FAILURE_HEALTHY=0
else
  "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" list --unresolved --json \
    > "$RS_TMP/failures.json" 2>/dev/null || FAILURE_HEALTHY=0
fi
if [ "$FAILURE_HEALTHY" = 1 ] && ! python3 -c \
  'import json,sys; row=json.load(open(sys.argv[1], encoding="utf-8")); failures=row.get("failures") if isinstance(row, dict) else None; invalid=row.get("invalid_rows") if isinstance(row, dict) else None; assert row.get("schema") == 1 and isinstance(invalid, int) and not isinstance(invalid, bool) and invalid >= 0 and isinstance(failures, list) and all(isinstance(item, dict) and isinstance(item.get("attention"), str) and isinstance(item.get("actionable"), bool) and isinstance(item.get("retiring"), bool) for item in failures)' \
  "$RS_TMP/failures.json" 2>/dev/null; then
  FAILURE_HEALTHY=0
fi
if [ "$FAILURE_HEALTHY" = 0 ]; then
  printf '{"schema":1,"failures":[]}\n' > "$RS_TMP/failures.json"
fi
TASK_HEALTHY=1
TASK_PHYSICAL=0
if [ -e "$REPO/.oms/task/current.md" ] || [ -L "$REPO/.oms/task/current.md" ]; then
  TASK_PHYSICAL=1
fi
if [ "$TASK_PHYSICAL" = 1 ] && { [ -L "$REPO/.oms/task/current.md" ] ||
  [ ! -f "$REPO/.oms/task/current.md" ]; }; then
  TASK_HEALTHY=0
else
  "$ROOT/scripts/agent-task.sh" --repo "$REPO" status --json \
    > "$RS_TMP/task.json" 2>/dev/null || TASK_HEALTHY=0
fi
if [ "$TASK_HEALTHY" = 1 ] && ! python3 -c \
  'import json,sys; row=json.load(open(sys.argv[1], encoding="utf-8")); present=row.get("present") if isinstance(row, dict) else None; assert row.get("schema") == 1 and isinstance(present, bool) and present == (sys.argv[2] == "1") and (not present or (isinstance(row.get("status"), str) and isinstance(row.get("verification"), str) and isinstance(row.get("stale"), bool)))' \
  "$RS_TMP/task.json" "$TASK_PHYSICAL" 2>/dev/null; then
  TASK_HEALTHY=0
fi
if [ "$TASK_HEALTHY" = 0 ]; then
  printf '{"schema":1,"present":false}\n' > "$RS_TMP/task.json"
fi
PLAN_HEALTHY=1
PLAN_PHYSICAL=0
if [ -e "$REPO/.oms/plan/tasks.json" ] || [ -L "$REPO/.oms/plan/tasks.json" ]; then
  PLAN_PHYSICAL=1
fi
if [ "$PLAN_PHYSICAL" = 1 ] && { [ -L "$REPO/.oms/plan/tasks.json" ] ||
  [ ! -f "$REPO/.oms/plan/tasks.json" ]; }; then
  PLAN_HEALTHY=0
else
  "$ROOT/scripts/agent-plan.sh" --repo "$REPO" status --json \
    > "$RS_TMP/plan.json" 2>/dev/null || PLAN_HEALTHY=0
fi
if [ "$PLAN_HEALTHY" = 1 ] && ! python3 -c \
  'import json,sys; row=json.load(open(sys.argv[1], encoding="utf-8")); present=row.get("present") if isinstance(row, dict) else None; count=row.get("task_count") if isinstance(row, dict) else None; by_state=row.get("by_state") if isinstance(row, dict) else None; contract=row.get("contract") if isinstance(row, dict) else None; assert row.get("schema") == 1 and isinstance(present, bool) and present == (sys.argv[2] == "1") and isinstance(count, int) and not isinstance(count, bool) and count >= 0 and all(isinstance(row.get(key), bool) for key in ("nonempty", "all_done", "has_unfinished")) and row.get("nonempty") == (count > 0) and row.get("has_unfinished") == (count > 0 and not row.get("all_done")) and isinstance(by_state, dict) and all(isinstance(value, int) and not isinstance(value, bool) and value >= 0 for value in by_state.values()) and sum(by_state.values()) == count and all(isinstance(row.get(key), list) for key in ("actionable", "stale", "stale_review")) and isinstance(contract, dict) and all(isinstance(contract.get(key), bool) for key in ("bound", "satisfied", "project_present", "project_healthy")) and all(isinstance(contract.get(key), str) for key in ("project_state", "blocker", "expected_spec_sha256", "current_spec_sha256"))' \
  "$RS_TMP/plan.json" "$PLAN_PHYSICAL" 2>/dev/null; then
  PLAN_HEALTHY=0
fi
if [ "$PLAN_HEALTHY" = 0 ]; then
  printf '{"schema":1,"present":false}\n' > "$RS_TMP/plan.json"
fi
RUNTIME_HEALTHY=1
"$ROOT/scripts/runtime.sh" --repo "$REPO" envelope show \
  > "$RS_TMP/runtime.json" 2>/dev/null || {
    RUNTIME_HEALTHY=0
    printf '{}\n' > "$RS_TMP/runtime.json"
  }

OMS_RS_AUTOUPDATE="$AUTOUPDATE_ATTENTION" \
OMS_RS_REPO="$REPO" \
OMS_RS_JSON="$AS_JSON" \
OMS_RS_PRIVATE="$PRIVATE_JSON" \
OMS_RS_LIFECYCLE_FILE="$RS_TMP/lifecycle.json" \
OMS_RS_LIFECYCLE_HEALTHY="$LIFECYCLE_HEALTHY" \
OMS_RS_APPROVAL_FILE="$RS_TMP/approvals.json" \
OMS_RS_APPROVAL_HEALTHY="$APPROVAL_HEALTHY" \
OMS_RS_FAILURE_FILE="$RS_TMP/failures.json" \
OMS_RS_FAILURE_HEALTHY="$FAILURE_HEALTHY" \
OMS_RS_FAILURE_PHYSICAL="$FAILURE_PHYSICAL" \
OMS_RS_TASK_FILE="$RS_TMP/task.json" \
OMS_RS_TASK_HEALTHY="$TASK_HEALTHY" \
OMS_RS_TASK_PHYSICAL="$TASK_PHYSICAL" \
OMS_RS_PLAN_FILE="$RS_TMP/plan.json" \
OMS_RS_PLAN_HEALTHY="$PLAN_HEALTHY" \
OMS_RS_PLAN_PHYSICAL="$PLAN_PHYSICAL" \
OMS_RS_RUNTIME_FILE="$RS_TMP/runtime.json" \
OMS_RS_RUNTIME_HEALTHY="$RUNTIME_HEALTHY" \
OMS_RS_BOARD_TTL="${OMS_EXPERIMENT_CLAIM_TTL:-86400}" \
OMS_RS_RUN_TTL="${OMS_RUN_CURRENT_TTL:-86400}" \
OMS_RS_GUARD_TTL="${OMS_GUARD_TTL:-86400}" \
OMS_RS_THREAD_TTL="${OMS_THREAD_CURRENT_TTL:-86400}" \
OMS_RS_THREAD_STALE_TTL="${OMS_THREAD_STALE_TTL:-259200}" \
OMS_RS_HEAD="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)" \
OMS_RS_BRANCH="$(git -C "$REPO" rev-parse --abbrev-ref HEAD 2>/dev/null || true)" \
OMS_RS_UPSTREAM="$(git -C "$REPO" rev-parse --abbrev-ref '@{u}' 2>/dev/null || true)" \
OMS_RS_AHEAD="$(git -C "$REPO" rev-list --count '@{u}..HEAD' 2>/dev/null || true)" \
OMS_RS_HAS_WORKFLOWS="$([ -d "$REPO/.github/workflows" ] && echo 1 || echo 0)" \
python3 - "$ROOT/scripts/lib/process_liveness.py" <<'PY'
import calendar, glob, json, os, runpy, sys, time

process_liveness = runpy.run_path(sys.argv[1])
process_pid_alive = process_liveness["pid_alive"]
persisted_native_pid_is_proven = process_liveness[
    "persisted_native_pid_is_proven"
]

repo = os.environ["OMS_RS_REPO"]
as_json = os.environ["OMS_RS_JSON"] == "1"
board_ttl = int(os.environ["OMS_RS_BOARD_TTL"])
run_ttl = int(os.environ["OMS_RS_RUN_TTL"])
guard_ttl = int(os.environ["OMS_RS_GUARD_TTL"])
thread_ttl = int(os.environ["OMS_RS_THREAD_TTL"])
thread_stale_ttl = int(os.environ["OMS_RS_THREAD_STALE_TTL"])
now = time.time()


def epoch(ts):
    try:
        return calendar.timegm(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return None


def read_jsonl(path):
    return read_jsonl_checked(path)[0]


def read_jsonl_checked(path):
    rows = []
    invalid = 0
    if not os.path.isfile(path):
        return rows, invalid
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except Exception:
                invalid += 1
                continue
            if isinstance(row, dict):
                rows.append(row)
            else:
                invalid += 1
    return rows, invalid


def oms(*parts):
    return os.path.join(repo, ".oms", *parts)


# Machine consumers key on this the way artifact-index/model-doctor do;
# bump it only with a deliberate output-contract change.
state = {"schema": 1}

# --- Durable agent attempts and private approvals ---------------------------
def load_json_file(path, fallback):
    try:
        value = json.load(open(path, encoding="utf-8"))
        return value if isinstance(value, type(fallback)) else fallback
    except Exception:
        return fallback


attempts = load_json_file(os.environ["OMS_RS_LIFECYCLE_FILE"], [])
attempt_by_state = {}
for attempt in attempts:
    key = str(attempt.get("state") or "unknown")
    attempt_by_state[key] = attempt_by_state.get(key, 0) + 1
attention_states = {"waiting_input", "waiting_approval", "blocked"}
agent_operations = {
    "healthy": os.environ["OMS_RS_LIFECYCLE_HEALTHY"] == "1",
    "total": len(attempts),
    "active": sum(not bool(item.get("terminal")) for item in attempts),
    "by_state": attempt_by_state,
    "needs_attention": [
        {key: item.get(key) for key in (
            "attempt_id", "state", "provider", "tool", "task_id", "reason_code", "updated_at"
        ) if item.get(key) not in (None, "")}
        for item in attempts if item.get("state") in attention_states
    ][-10:],
    "latest": [
        {key: item.get(key) for key in (
            "attempt_id", "state", "provider", "tool", "task_id", "reason_code", "updated_at"
        ) if item.get(key) not in (None, "")}
        for item in attempts[-5:]
    ],
}
state["agent_operations"] = agent_operations

approval_rows = load_json_file(os.environ["OMS_RS_APPROVAL_FILE"], [])
approval_by_state = {}
approval_by_durable_state = {}
approval_by_effective_state = {}
pending_approvals = []
effective_expired = 0
for item in approval_rows:
    durable_state = str(item.get("state") or "unknown")
    effective_state = str(item.get("effective_state") or durable_state)
    if effective_state == "expired" and durable_state != "expired":
        effective_expired += 1
    # Compatibility: this field historically projected only requested rows
    # through read-time expiry. Keep that shape while naming both explicit
    # projections additively for new consumers.
    legacy_state = "expired" if durable_state == "requested" and effective_state == "expired" else durable_state
    approval_by_state[legacy_state] = approval_by_state.get(legacy_state, 0) + 1
    approval_by_durable_state[durable_state] = approval_by_durable_state.get(durable_state, 0) + 1
    approval_by_effective_state[effective_state] = approval_by_effective_state.get(effective_state, 0) + 1
    if effective_state in {"requested", "approved", "consuming"}:
        pending_approvals.append({
            key: item.get(key) for key in (
                "approval_id", "version", "state", "effective_state", "action", "object_id", "summary",
                "attempt_id", "task_id", "expires_at", "updated_at"
            ) if item.get(key) not in (None, "")
        })
state["approvals"] = {
    "healthy": os.environ["OMS_RS_APPROVAL_HEALTHY"] == "1",
    "total": len(approval_rows),
    "pending": len(pending_approvals),
    "effective_expired": effective_expired,
    "by_state": approval_by_state,
    "by_durable_state": approval_by_durable_state,
    "by_effective_state": approval_by_effective_state,
    "latest_pending": pending_approvals[-5:],
}

# Typed runtime projection: read-only evidence over the same shared state. An
# unhealthy or schema-mismatched projection is reported, never guessed at.
runtime_raw = load_json_file(os.environ["OMS_RS_RUNTIME_FILE"], {})
runtime_healthy = (
    os.environ["OMS_RS_RUNTIME_HEALTHY"] == "1" and runtime_raw.get("schema") == 2
)
state["runtime"] = {
    "healthy": runtime_healthy,
    "schema": runtime_raw.get("schema") if runtime_healthy else None,
    "state_digest": runtime_raw.get("state_digest") if runtime_healthy else None,
    "objective": runtime_raw.get("objective", {}) if runtime_healthy else {},
    "scope": runtime_raw.get("scope", {}) if runtime_healthy else {},
    "criteria": runtime_raw.get("criteria", []) if runtime_healthy else [],
    "evidence": runtime_raw.get("evidence", {}) if runtime_healthy else {},
    "next_actions": runtime_raw.get("next_actions", []) if runtime_healthy else [],
    "continuity": runtime_raw.get("continuity", {}) if runtime_healthy else {},
    "warnings": runtime_raw.get("warnings", []) if runtime_healthy else [],
}

# --- Active task packet + canonical lifecycle status ------------------------
task_status = load_json_file(os.environ["OMS_RS_TASK_FILE"], {})
task_healthy = os.environ["OMS_RS_TASK_HEALTHY"] == "1"
if task_healthy:
    task = {
        key: task_status.get(key) for key in (
            "present", "task_id", "status", "source_session", "last_activity",
            "closed_at", "stale", "verification"
        ) if key in task_status
    }
    task.setdefault("present", False)
    task["healthy"] = True
else:
    task = {
        "present": os.environ["OMS_RS_TASK_PHYSICAL"] == "1",
        "healthy": False,
        "error": "status-unavailable",
    }
tf = oms("task", "current.md")
if task_healthy and task["present"] and os.path.isfile(tf):
    section = None
    buf = {"## Goal": [], "## Next Step": []}
    for raw in open(tf, encoding="utf-8", errors="replace"):
        line = raw.rstrip("\n")
        if line.startswith("## "):
            section = line if line in buf else None
            continue
        if section and line.strip():
            buf[section].append(line.strip())
    task["goal"] = " ".join(buf["## Goal"])[:200]
    task["next"] = " ".join(buf["## Next Step"])[:200]
state["task"] = task

# --- Plan DAG: canonical read-time snapshot ---------------------------------
plan_status = load_json_file(os.environ["OMS_RS_PLAN_FILE"], {})
plan_healthy = os.environ["OMS_RS_PLAN_HEALTHY"] == "1"
plan = {
    "present": (bool(plan_status.get("present")) if plan_healthy else
                os.environ["OMS_RS_PLAN_PHYSICAL"] == "1"),
    "healthy": plan_healthy,
    "task_count": int(plan_status.get("task_count") or 0),
    "nonempty": bool(plan_status.get("nonempty")),
    "all_done": bool(plan_status.get("all_done")),
    "has_unfinished": bool(plan_status.get("has_unfinished")),
    "by_state": plan_status.get("by_state", {}),
    "stale": plan_status.get("stale", []),
    "stale_review": plan_status.get("stale_review", []),
    "actionable": plan_status.get("actionable", []),
    "contract": plan_status.get("contract", {}),
}
if not plan_healthy:
    plan["error"] = "status-unavailable"
pf = oms("plan", "tasks.json")
if plan_healthy and plan["present"] and os.path.isfile(pf):
    try:
        pdata = json.load(open(pf, encoding="utf-8"))
    except Exception:
        pdata = {}
    plan["goal"] = (pdata.get("goal") or "")[:200]
retirements = [row for row in read_jsonl(oms("plan", "retirements.jsonl"))
               if row.get("schema") == 1 and row.get("kind") == "plan-retirement"]
if retirements:
    latest = retirements[-1]
    plan["latest_retirement"] = {
        key: latest.get(key) for key in (
            "ts", "retirement_id", "plan_sha256", "plan_id", "disposition",
            "reason", "head", "git_ref", "archive", "acceptance_verified",
            "task_landings_claimed"
        ) if latest.get(key) not in (None, "")
    }
state["plan"] = plan

# --- Experiment board: active + stale (replay last-wins) --------------------
board = {"present": False, "active": [], "stale": []}
bf = oms("experiments.jsonl")
brows = read_jsonl(bf)
if brows:
    board["present"] = True
    cur, order = {}, []
    for e in brows:
        i = e.get("id")
        if not i:
            continue
        if i not in cur:
            cur[i] = {}
            order.append(i)
        for k, v in e.items():
            if v in ("", None):
                continue
            if k == "owner" and "owner" in cur[i] and e.get("status") != "claimed":
                continue  # only a (re)claim reassigns owner
            cur[i][k] = v
        cur[i]["status"] = e.get("status", cur[i].get("status"))
    for i in order:
        r = cur[i]
        st = r.get("status", "?")
        if st in ("done", "aborted"):
            continue
        entry = {"id": i, "status": st, "owner": r.get("owner", "?")}
        board["active"].append(entry)
        if st == "claimed":
            e = epoch(r.get("ts", ""))
            if e is not None and now - e >= board_ttl:
                board["stale"].append(entry)
state["board"] = board

# --- Runs: current pointer + open runs from the spine -----------------------
runs = {"current": None, "open": []}
cur_ptr = oms("runs", "CURRENT")
if os.path.isfile(cur_ptr):
    parts = open(cur_ptr, encoding="utf-8", errors="replace").read().split()
    if parts:
        rid = parts[0]
        minted = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else None
        fresh = minted is not None and now - minted <= run_ttl
        runs["current"] = {"run_id": rid, "fresh": fresh}
spine = read_jsonl(oms("runs", "spine.jsonl"))
if spine:
    seen, order, closed = {}, [], set()
    for r in spine:
        rid = r.get("run_id")
        if not rid:
            continue
        if rid not in seen:
            seen[rid] = r.get("ts")
            order.append(rid)
        if r.get("tool") == "oms-run" and r.get("event") == "close":
            closed.add(rid)
    runs["open"] = [rid for rid in order if rid not in closed][-10:]
state["runs"] = runs

# --- Latest artifact-index rows ---------------------------------------------
arts, artifact_invalid_rows = read_jsonl_checked(oms("artifacts", "index.jsonl"))
required_artifact_fields = {
    "schema", "event_id", "operation_id", "artifact_id", "ts", "kind", "provider", "exit"
}
artifact_id_counts = {}
for r in arts:
    event_id = r.get("event_id")
    if r.get("schema") == 1 and isinstance(event_id, str):
        artifact_id_counts[event_id] = artifact_id_counts.get(event_id, 0) + 1


def artifact_row_contract_valid(row):
    # Legacy rows remain visible until `artifact-index migrate`; schema-1 rows
    # are never allowed to false-green when their envelope is incomplete.
    if row.get("schema") != 1:
        return row.get("kind") != "artifact-resolution"
    if required_artifact_fields - row.keys():
        return False
    for key in ("event_id", "operation_id", "artifact_id", "ts", "kind"):
        if not isinstance(row.get(key), str) or not row.get(key):
            return False
    if not isinstance(row.get("provider"), str):
        return False
    exit_value = row.get("exit")
    if isinstance(exit_value, bool) or not isinstance(exit_value, int) or exit_value < 0:
        return False
    return artifact_id_counts.get(row.get("event_id")) == 1


contract_arts = []
for row in arts:
    if artifact_row_contract_valid(row):
        contract_arts.append(row)
    else:
        artifact_invalid_rows += 1
arts = contract_arts
resolved_at = {}
artifact_by_id = {r.get("event_id"): (i, r) for i, r in enumerate(arts)
                  if r.get("schema") == 1}
valid_resolution_indexes = set()
for i, r in enumerate(arts):
    if r.get("kind") != "artifact-resolution":
        continue
    target_id = r.get("resolves_event_id")
    target_entry = artifact_by_id.get(target_id)
    if (not target_entry or r.get("schema") != 1 or
            artifact_id_counts.get(r.get("event_id")) != 1 or
            r.get("parent_event_id") != target_id or
            r.get("resolution") != "resolved"):
        continue
    target_i, target = target_entry
    resolver_exit = r.get("exit")
    target_exit = target.get("exit")
    if (isinstance(resolver_exit, bool) or not isinstance(resolver_exit, int) or
            isinstance(target_exit, bool) or not isinstance(target_exit, int)):
        continue
    resolver_ok = resolver_exit == 0
    target_failed = target_exit > 0
    if (resolver_ok and target_failed and target_i < i and target.get("schema") == 1 and
            target.get("kind") != "artifact-resolution" and
            r.get("operation_id") == target.get("operation_id") and
            r.get("artifact_id") == target.get("artifact_id")):
        resolved_at[target_id] = r.get("ts")
        valid_resolution_indexes.add(i)
for i, row in enumerate(arts):
    if row.get("kind") == "artifact-resolution" and i not in valid_resolution_indexes:
        artifact_invalid_rows += 1
resolution_rows = [r for i, r in enumerate(arts) if i in valid_resolution_indexes]
outcomes = [r for r in arts if r.get("kind") != "artifact-resolution"]
arts = [r for i, r in enumerate(arts)
        if r.get("kind") != "artifact-resolution" or i in valid_resolution_indexes]


def artifact_status(row):
    exit_value = row.get("exit")
    if isinstance(exit_value, bool) or not isinstance(exit_value, int) or exit_value < 0:
        return "unresolved"
    failed = exit_value != 0
    if not failed:
        return "success"
    return "resolved" if row.get("event_id") in resolved_at else "unresolved"


artifact_counts = {"success": 0, "unresolved": 0, "resolved": 0}
for row in outcomes:
    artifact_counts[artifact_status(row)] += 1
latest_artifacts = []
for r in outcomes[-5:]:
    item = {k: r.get(k) for k in ("ts", "kind", "provider", "exit", "event_id")
            if r.get(k) not in (None, "")}
    item["status"] = artifact_status(r)
    if r.get("event_id") in resolved_at:
        item["resolved_at"] = resolved_at[r.get("event_id")]
    latest_artifacts.append(item)
state["artifacts"] = {
    "total": len(arts),
    "invalid_rows": artifact_invalid_rows,
    "healthy": artifact_invalid_rows == 0,
    "outcomes_total": len(outcomes),
    "resolution_events": len(resolution_rows),
    "counts": artifact_counts,
    "latest": latest_artifacts,
}

# --- Unresolved failures (canonical fail-ledger projection) -----------------
failure_doc = load_json_file(os.environ["OMS_RS_FAILURE_FILE"], {})
failure_rows = failure_doc.get("failures", []) if isinstance(failure_doc, dict) else []
# Corrupt rows are quarantined by the ledger, not fatal: the valid rows still
# project here while healthy goes false, which keeps the inbox corruption
# item alive without bricking the envelope.
failure_invalid_rows = failure_doc.get("invalid_rows", 0) if isinstance(failure_doc, dict) else 0
if not isinstance(failure_invalid_rows, int) or isinstance(failure_invalid_rows, bool):
    failure_invalid_rows = 0
failure_healthy = (os.environ["OMS_RS_FAILURE_HEALTHY"] == "1"
                   and failure_invalid_rows == 0)
open_fails = []
for row in failure_rows:
    if not isinstance(row, dict):
        continue
    open_fails.append({
        "fingerprint": row.get("fingerprint") or "",
        "count": int(row.get("count") or 0),
        "kind": row.get("kind") or "",
        "attention": row.get("attention") or "none",
        "actionable": row.get("actionable") is True,
        "retiring": bool(row.get("retiring")),
        "summary": (row.get("summary") or row.get("cmd") or "")[:80],
    })
actionable_total = sum(1 for f in open_fails if f["actionable"])
state["failures"] = {
    "present": os.environ["OMS_RS_FAILURE_PHYSICAL"] == "1",
    "healthy": failure_healthy,
    "invalid_rows": failure_invalid_rows,
    "open": open_fails[-5:], "open_total": len(open_fails),
    "actionable_total": actionable_total,
    "retiring_total": len(open_fails) - actionable_total,
}
if not failure_healthy:
    state["failures"]["error"] = (
        "invalid-rows" if failure_invalid_rows else "projection-unavailable")

# --- Install attention (auto-update) ----------------------------------------
au = os.environ.get("OMS_RS_AUTOUPDATE") or ""
verdict, detail = "", ""
if au.startswith("attention: "):
    body = au[len("attention: "):]
    parts = body.split(" — ", 1)
    verdict = parts[0].strip()
    detail = parts[1].strip() if len(parts) > 1 else ""
state["install"] = {"auto_update": verdict or "unknown",
                    "auto_update_detail": detail[:160]}

# --- CI for HEAD, keyed by (branch, sha) ------------------------------------
# A recorded conclusion is evidence about the commit it ran on. Three unlike
# situations used to collapse into one "stale" nag: a HEAD CI has not been
# shown yet, a HEAD that was never pushed, and a repo whose push state cannot
# be determined at all. Only the unpushed one has an action, and it is a push.
ci = {"present": False, "current_sha": None, "fresh": None, "state": "none",
      "ahead": None, "unpushed": None, "upstream": None, "current_branch": None}
ahead_raw = (os.environ.get("OMS_RS_AHEAD") or "").strip()
ahead = int(ahead_raw) if ahead_raw.isdigit() else None
head_sha = os.environ.get("OMS_RS_HEAD") or None
ci["current_sha"] = head_sha
ci["current_branch"] = os.environ.get("OMS_RS_BRANCH") or None
ci["upstream"] = os.environ.get("OMS_RS_UPSTREAM") or None
ci["ahead"] = ahead
if ahead is not None:
    ci["unpushed"] = ahead > 0
ci_rows = read_jsonl(oms("ci.jsonl"))
if ci_rows:
    ci["present"] = True
    last = ci_rows[-1]
    ci.update({k: last.get(k) for k in ("branch", "sha", "status", "conclusion", "url")})
    recorded = ci.get("sha") or ""
    if head_sha and recorded:
        short = min(len(recorded), len(head_sha))
        ci["fresh"] = recorded[:short] == head_sha[:short]
if ci["fresh"]:
    ci["state"] = "current"
elif ci["unpushed"]:
    # Silent in a repo with neither CI history nor workflows: nothing is
    # waiting on that push, so naming it would be noise, not attention.
    if ci["present"] or os.environ.get("OMS_RS_HAS_WORKFLOWS") == "1":
        ci["state"] = "unpushed"
elif ci["present"] and ahead == 0:
    ci["state"] = "pending"
elif ci["present"]:
    # No upstream to compare against: the recorded run may or may not describe
    # this commit, and refreshing is the only answer available.
    ci["state"] = "stale"
state["ci"] = ci

# --- In-flight delegations (liveness files) ---------------------------------
def pid_alive(pid, native_pid=None, native_pid_source=None):
    if isinstance(pid, str) and pid.isdigit():
        pid = int(pid)
    if isinstance(native_pid, str) and native_pid.isdigit():
        native_pid = int(native_pid)
    if not persisted_native_pid_is_proven(native_pid, native_pid_source):
        # Read-only state must preserve an owner whose persisted Windows
        # identity predates provenance; it cannot label that work orphaned.
        return True
    return process_pid_alive(pid, native_pid=native_pid)

delegations = []
deleg_dir = oms("delegations")
if os.path.isdir(deleg_dir):
    import glob as _glob
    for f in sorted(_glob.glob(os.path.join(deleg_dir, "*.json"))):
        try:
            d = json.load(open(f, encoding="utf-8"))
        except Exception:
            continue
        # Same-host liveness: a leftover file whose pid is gone is a crashed
        # orphan (only meaningful when the record was written on this host).
        alive = pid_alive(
            d.get("pid"), d.get("native_pid"), d.get("native_pid_source")
        )
        delegations.append({"id": d.get("id"), "provider": d.get("provider"),
                            "role": d.get("role", ""), "executor_id": d.get("executor_id", ""),
                            "soul_sha256": d.get("soul_sha256", ""), "started_at": d.get("started_at"),
                            "live": alive})
state["delegations"] = delegations

# --- Task-scoped executors -------------------------------------------------
executors = []
executor_dir = oms("executors")
if os.path.isdir(executor_dir):
    import glob as _executor_glob
    for f in sorted(_executor_glob.glob(os.path.join(executor_dir, "*", "meta.json"))):
        try:
            d = json.load(open(f, encoding="utf-8"))
        except Exception:
            continue
        executors.append({"id": d.get("executor_id", os.path.basename(os.path.dirname(f))),
                          "state": d.get("state", "unknown"),
                          "provider": d.get("provider", ""),
                          "strategy": d.get("strategy", ""),
                          "task_id": d.get("task_id", ""),
                          "soul_sha256": d.get("soul_sha256", "")})
state["executors"] = executors

# --- Change-guard active? ---------------------------------------------------
guard = {"active": False, "stale": False}
gf = oms("guards", "change-guard.tsv")
if os.path.isfile(gf):
    guard["active"] = True
    gpid = gstarted = ""
    for raw in open(gf, encoding="utf-8", errors="replace"):
        parts = raw.rstrip("\n").split("\t")
        if len(parts) >= 2 and parts[0] == "pid" and not gpid:
            gpid = parts[1]
        if len(parts) >= 2 and parts[0] == "started" and not gstarted:
            gstarted = parts[1]
    # Stale: dead opt-in owner pid, else started + TTL (the begin process is
    # short-lived, so age is the default liveness signal).
    if gpid:
        guard["stale"] = not pid_alive(gpid)
    elif gstarted.isdigit():
        guard["stale"] = now - int(gstarted) > guard_ttl
state["change_guard"] = guard

# --- Interrupted landings ---------------------------------------------------
# A land is three writes (apply, lineage, plan finish). An intent with no
# terminal row means one of them never happened, which otherwise looks exactly
# like "nothing was landed".
landings = {"outstanding": []}
for row in read_jsonl(oms("landings.jsonl")):
    lid = row.get("landing_id")
    if not lid:
        continue
    if row.get("event") == "intent":
        landings.setdefault("_open", {})[lid] = row
    elif row.get("event") in ("complete", "abandoned"):
        landings.setdefault("_open", {}).pop(lid, None)
for lid, row in (landings.pop("_open", {}) or {}).items():
    landings["outstanding"].append({
        "landing_id": lid, "ts": row.get("ts"), "task": row.get("task") or None,
        "patch": row.get("patch"),
    })
state["landings"] = landings

# --- Project harness: the rules every agent reads before any .oms state ------
# Presence and exposure only. Template freshness needs reference blocks
# regenerated from the install, which is project-doctor's job, not a query's.
harness = {"rules": "missing", "styles": [], "spec": "missing", "private": "n/a",
           "exposed": []}
seen_styles = []
rule_files = []
agent_files = []
for name in ("AGENTS.md", "CLAUDE.md"):
    path = os.path.join(repo, name)
    if not os.path.isfile(path):
        continue
    agent_files.append(name)
    styles = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line.startswith("<!-- oh-my-setting:") and line.endswith(":begin -->"):
                styles.append(line[len("<!-- oh-my-setting:"):-len(":begin -->")])
    if styles:
        rule_files.append(name)
        for s in styles:
            if s not in seen_styles:
                seen_styles.append(s)
if rule_files:
    harness["rules"] = "present" if len(rule_files) == 2 else "partial"
    harness["styles"] = seen_styles
    harness["rule_files"] = rule_files
elif agent_files:
    # Hand-written agent rules (this repo's own AGENTS.md is one): report them,
    # do not nag to overwrite them with a template.
    harness["rules"] = "unmanaged"
    harness["rule_files"] = agent_files
spec_path = os.path.join(repo, "PROJECT.md")
if os.path.isfile(spec_path):
    harness["spec"] = "unset"
    with open(spec_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if line.startswith("- State:"):
                harness["spec"] = line[len("- State:"):].strip() or "unset"
                break
try:
    private = json.loads(os.environ.get("OMS_RS_PRIVATE") or "{}")
except Exception:
    private = {}
if private.get("git"):
    harness["exposed"] = [e["path"] for e in private.get("entries", [])
                          if e.get("state") == "exposed"]
    harness["private"] = "exposed" if harness["exposed"] else "hidden"
state["harness"] = harness

# --- Cross-agent threads: what the agents are actually discussing -----------
threads = {"current": None, "open": 0, "stale_open": 0, "recent": []}
tdir = oms("threads")
if os.path.isdir(tdir):
    cur = os.path.join(tdir, "CURRENT")
    if os.path.isfile(cur):
        parts = open(cur, encoding="utf-8", errors="replace").read().split()
        if parts:
            minted = parts[1] if len(parts) > 1 and parts[1].isdigit() else None
            fresh = minted is None or (now - int(minted)) <= thread_ttl
            if fresh and os.path.isfile(os.path.join(tdir, parts[0] + ".jsonl")):
                threads["current"] = parts[0]
    rows_by_thread = []
    for path in sorted(glob.glob(os.path.join(tdir, "*.jsonl"))):
        rows = read_jsonl(path)
        if not rows:
            continue
        tid = os.path.basename(path)[: -len(".jsonl")]
        closed = any(r.get("role") == "closed" for r in rows)
        providers = []
        for r in rows:
            p = r.get("provider")
            if p and p not in providers:
                providers.append(p)
        # Date a thread by its newest turn whose timestamp parses: a malformed
        # ts cannot age it, and an undatable thread is never called abandoned.
        last_epoch = None
        for r in reversed(rows):
            last_epoch = epoch(r.get("ts"))
            if last_epoch is not None:
                break
        rows_by_thread.append({
            "id": tid, "turns": len(rows), "closed": closed,
            "providers": providers, "last_ts": rows[-1].get("ts"),
            "age_seconds": None if last_epoch is None else int(now - last_epoch),
        })
    rows_by_thread.sort(key=lambda t: t["last_ts"] or "", reverse=True)
    open_threads = [t for t in rows_by_thread if not t["closed"]]
    threads["open"] = len(open_threads)
    # Advisory count only: the current thread is live by definition, and an
    # open thread is the only record that an exchange happened, so nothing
    # here (or in gc) closes one — see `oms thread list --stale`.
    threads["stale_open"] = sum(
        1 for t in open_threads
        if t["id"] != threads["current"] and t["age_seconds"] is not None
        and t["age_seconds"] > thread_stale_ttl)
    threads["recent"] = open_threads[:3]
state["threads"] = threads

if as_json:
    print(json.dumps(state, ensure_ascii=False, indent=2))
else:
    def line(s):
        print(s)

    line("# state: %s" % repo)

    h = state["harness"]
    if h["rules"] == "missing" and h["spec"] == "missing" and h["private"] != "exposed":
        line("\n## Project harness: none (run: oms apply-project-template auto .)")
    else:
        line("\n## Project harness")
        if h["rules"] == "present":
            line("  rules: %s" % (", ".join(h["styles"]) or "none"))
        elif h["rules"] == "unmanaged":
            line("  rules: %s, hand-written (no oh-my-setting block)" %
                 ", ".join(h.get("rule_files", [])))
        elif h["rules"] == "partial":
            line("  rules: %s in %s only (run: oms project-doctor .)" % (
                ", ".join(h["styles"]) or "none", ", ".join(h.get("rule_files", []))))
        else:
            line("  rules: missing (run: oms apply-project-template auto .)")
        line("  spec: PROJECT.md %s" % h["spec"])
        if h["private"] == "exposed":
            line("  git: %s visible to git (run: oms project-private apply)" %
                 ", ".join(h["exposed"]))

    th = state["threads"]
    if th["current"] or th["open"]:
        line("\n## Cross-agent threads (%d open, %d stale)" % (
            th["open"], th["stale_open"]))
        for entry in th["recent"]:
            line("  %s%-26s turns=%-3d %s" % (
                "* " if entry["id"] == th["current"] else "  ",
                entry["id"], entry["turns"],
                ",".join(entry["providers"]) or "-"))
        if th["stale_open"]:
            line("  stale: oms thread list --stale  (advisory; nothing auto-closes)")
        if th["current"]:
            line("  resume: oms consult \"...\"  (joins %s)" % th["current"])

    t = state["task"]
    if t["present"]:
        line("\n## Active task")
        if not t.get("healthy", True):
            line("  status unavailable or invalid (run: oms agent-task status --json)")
        if t.get("goal"):
            line("  goal: %s" % t["goal"])
        if t.get("next"):
            line("  next: %s" % t["next"])
    else:
        line("\n## Active task: none")

    runtime = state.get("runtime", {})
    line("\n## Runtime contract")
    if not runtime.get("healthy"):
        line("  unavailable or invalid (run: oms runtime doctor --strict)")
    else:
        evidence = runtime.get("evidence", {})
        counts = evidence.get("counts", {})
        line("  evidence coverage: %.1f%%  complete=%s" % (
            100.0 * float(evidence.get("coverage", 0.0) or 0.0),
            "yes" if evidence.get("complete") else "no"))
        if counts:
            line("  criteria: %s" % ", ".join(
                "%s=%s" % (key, value) for key, value in sorted(counts.items())))
        next_actions = runtime.get("next_actions", [])
        if next_actions:
            line("  next: %s" % next_actions[0].get("command", next_actions[0].get("id", "-")))
        latest_import = runtime.get("continuity", {}).get("latest_import", {})
        if latest_import.get("present"):
            line("  imported capsule: %s status=%s (advisory only; no authority transferred)" % (
                latest_import.get("capsule_id", "unknown"),
                latest_import.get("status", "unknown")))

    p = state["plan"]
    if p["present"]:
        line("\n## Plan")
        if not p.get("healthy", True):
            line("  status unavailable or invalid (run: oms agent-plan status --json)")
        if p.get("goal"):
            line("  goal: %s" % p["goal"])
        line("  by state: %s" % ", ".join("%s=%d" % (k, v) for k, v in sorted(p["by_state"].items())))
        contract = p.get("contract", {})
        if contract.get("bound") and not contract.get("satisfied"):
            line("  PROJECT contract: BLOCKED (%s; inspect: oms agent-plan --repo . status --json)" %
                 (contract.get("blocker") or "unknown"))
        if p["actionable"]:
            line("  actionable now: %s" % ", ".join(p["actionable"]))
        if p["stale"]:
            line("  STALE claims: %s" % ", ".join("%s(%s)" % (s["id"], s["provider"]) for s in p["stale"]))
        if p["stale_review"]:
            line("  STALE review (reviewer gone? reclaim --include-review): %s"
                 % ", ".join(s["id"] for s in p["stale_review"]))
    else:
        line("\n## Plan: none")
        if p.get("latest_retirement"):
            retired = p["latest_retirement"]
            line("  latest retired: %s %s" % (
                retired.get("disposition", "?"),
                str(retired.get("plan_sha256", ""))[:12] or "?"))

    b = state["board"]
    if b["present"]:
        line("\n## Experiment board")
        if b["active"]:
            for e in b["active"]:
                tag = " STALE" if e in b["stale"] else ""
                line("  %-8s %s owner=%s%s" % (e["status"], e["id"], e["owner"], tag))
        else:
            line("  no active experiments")

    r = state["runs"]
    line("\n## Runs")
    if r["current"]:
        line("  current: %s%s" % (r["current"]["run_id"], "" if r["current"]["fresh"] else " (stale pointer)"))
    else:
        line("  current: none")
    if r["open"]:
        line("  open: %s" % ", ".join(r["open"]))

    ops = state["agent_operations"]
    if ops["total"] or not ops["healthy"]:
        line("\n## Agent operations (%d total, %d active)" % (ops["total"], ops["active"]))
        if not ops["healthy"]:
            line("  CORRUPT lifecycle stream (run: oms agent-events validate)")
        if ops["by_state"]:
            line("  by state: %s" % ", ".join(
                "%s=%d" % (key, value) for key, value in sorted(ops["by_state"].items())))
        for entry in ops["needs_attention"]:
            line("  ATTENTION %s %s %s/%s" % (
                entry.get("attempt_id", "?"), entry.get("state", "?"),
                entry.get("tool", "?"), entry.get("provider", "-") or "-"))

    approvals = state["approvals"]
    if approvals["pending"] or not approvals["healthy"]:
        line("\n## Approvals (%d pending)" % approvals["pending"])
        if not approvals["healthy"]:
            line("  CORRUPT private approval stream (run: oms approval-inbox validate)")
        for entry in approvals["latest_pending"]:
            line("  %s v%s %s %s" % (
                entry.get("approval_id", "?"), entry.get("version", "?"),
                entry.get("state", "?"), entry.get("summary", "")))

    dl = state["delegations"]
    if dl:
        line("\n## In-flight delegations")
        for e in dl:
            tag = "live" if e["live"] else "ORPHAN (dead pid)"
            line("  %s %s%s%s  %s  [%s]" % (
                e.get("provider", "?"), ("role=%s " % e["role"]) if e.get("role") else "",
                ("executor=%s soul=%s " % (e["executor_id"], (e.get("soul_sha256") or "-")[:12])) if e.get("executor_id") else "",
                e.get("id", "?"), e.get("started_at", "?"), tag))

    executors = state["executors"]
    if executors:
        line("\n## Executors")
        for e in executors:
            line("  %-8s %s provider=%s strategy=%s task=%s soul=%s" % (
                e.get("state", "?"), e.get("id", "?"), e.get("provider", "?") or "-",
                e.get("strategy", "?") or "-", e.get("task_id", "") or "-",
                (e.get("soul_sha256", "") or "-")[:12]))

    ci = state["ci"]
    if ci["state"] != "none":
        line("\n## CI (%s)" % (ci.get("branch") or ci.get("current_branch") or "?"))
        recorded = "%s %s  %s" % (ci.get("status") or "?", ci.get("conclusion") or "?",
                                  (ci.get("sha") or "")[:12])
        if ci["state"] == "current":
            line("  %s" % recorded)
        elif ci["state"] == "unpushed":
            line("  unpushed: %d commit(s) ahead of %s — push to get CI" % (
                ci.get("ahead") or 0, ci.get("upstream") or "the upstream"))
            if ci["present"]:
                line("  history: %s" % recorded)
        elif ci["state"] == "pending":
            line("  unknown/pending: no run recorded for HEAD %s" % (
                (ci.get("current_sha") or "?")[:12]))
            if ci["present"]:
                line("  history: %s" % recorded)
            line("  refresh: oms state --refresh-ci")
        else:
            line("  %s  STALE (HEAD is %s)" % (
                recorded, (ci.get("current_sha") or "?")[:12]))
            line("  refresh: oms state --refresh-ci")

    ld = state["landings"]
    if ld["outstanding"]:
        line("\n## Interrupted landings (%d)" % len(ld["outstanding"]))
        for entry in ld["outstanding"]:
            line("  %s  %s%s" % (entry["landing_id"], entry["ts"] or "?",
                                 ("  task=" + entry["task"]) if entry["task"] else ""))
        line("  recover: oms patch-land --recover")

    fl = state["failures"]
    if not fl.get("healthy", True):
        line("\n## Unresolved failures: unavailable or invalid (run: oms fail-ledger list)")
    elif fl["open_total"] > 0:
        line("\n## Unresolved failures (%d)" % fl["open_total"])
        for e in fl["open"]:
            line("  %s  x%d  %s" % (e["fingerprint"], e["count"], e["summary"]))

    a = state["artifacts"]
    line("\n## Artifacts (%d total)" % a["total"])
    if not a["healthy"]:
        line("  CORRUPT invalid_rows=%d" % a["invalid_rows"])
    line("  retained outcomes: %d  success=%d unresolved=%d resolved=%d" % (
        a["outcomes_total"], a["counts"]["success"], a["counts"]["unresolved"],
        a["counts"]["resolved"]))
    for row in a["latest"]:
        line("  %s %s/%s exit=%s status=%s event=%s" % (
            row.get("ts", "?"), row.get("kind", "?"), row.get("provider", "") or "-",
            row.get("exit", "?"), row.get("status", "?"), row.get("event_id", "?")))

    if state["change_guard"]["active"]:
        tag = " (STALE — abandoned? end it: change-guard.sh end)" if state["change_guard"]["stale"] else ""
        line("\n## Change-guard: ACTIVE%s" % tag)
PY
