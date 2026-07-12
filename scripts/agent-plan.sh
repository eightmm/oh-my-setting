#!/usr/bin/env bash
set -euo pipefail

# A small, shared task graph for multi-agent work. Where agent-task.sh holds the
# single active handoff packet, agent-plan.sh holds a DAG of subtasks that can be
# split across Codex / Claude Code / Antigravity: each task has dependencies, a
# path scope, a verify command, and a state. "ready" computes which tasks are
# actionable now (state=ready and every dependency done). State lives in
# .oms/plan/tasks.json (git-ignored, agent-shared); writes are atomic.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

# OMS_STATE_REPO: set by peer-delegate.sh for worktree workers so they
# read the primary repo's shared state instead of the throwaway checkout's.
REPO="${OMS_STATE_REPO:-$PWD}"
PLAN_FILE=""
ACTION=""
ID=""
TITLE=""
GOAL=""
PROVIDER=""
TTL=""
REASON=""
ARTIFACT=""
PATCH=""
DEPENDS=""
ALLOWED=""
FORBIDDEN=""
VERIFY=""
ROLE=""
STATE_FILTER=""
CLAIM=0
INCLUDE_RUNNING=0
INCLUDE_REVIEW=0
LEASE_ID="${OMS_PLAN_LEASE_ID:-}"
AS_JSON=0

usage() {
  cat <<'EOF'
Usage: agent-plan.sh [--repo PATH] [--file PATH] <command> [options]

Commands:
  init   --goal TEXT                 Create/replace the plan with a goal.
  add    --id ID --title TEXT        Add a task (state: ready).
         [--depends a,b] [--allowed "p1,p2"] [--forbidden "p3"]
         [--verify CMD] [--role NAME]
  claim  --id ID --provider NAME [--ttl TEXT]   Claim a ready task for a worker.
  start  --id ID [--lease-id TOKEN]  Mark a claimed task running.
  touch  --id ID [--lease-id TOKEN]  Heartbeat a claimed/running task: refresh
                                     claimed_at so a live worker is not reclaimed.
  review --id ID [--lease-id TOKEN] [--artifact PATH] [--patch PATH]
                                     Move a claimed/running task to review.
  land   --id ID [--lease-id TOKEN]  Fence admitted review work while applying it.
  finish --id ID [--lease-id TOKEN] [--artifact PATH] [--patch PATH]
                                     Mark a landed task done.
  block  --id ID --reason TEXT       Mark a task blocked.
  release --id ID                    Requeue a claimed/running/review task to ready (worker died).
  reclaim [--ttl SECONDS] [--include-running] [--include-review]
                                     Requeue claimed tasks whose TTL since
                                     claimed_at expired (dead-worker recovery).
                                     A numeric per-task ttl wins over --ttl
                                     (default 3600). running needs the opt-in
                                     flag. review holds a finished artifact
                                     awaiting a reviewer, so it is only
                                     reclaimed with --include-review, ages from
                                     its updated timestamp, and defaults to a
                                     longer TTL (86400) unless --ttl is given;
                                     its artifact/patch fields are kept.
  reopen --id ID                     Return a blocked task to ready.
  show   --id ID                     Print one task as JSON.
  list   [--state STATE]             List tasks (optionally by state).
  ready                              Print ids actionable now (deps done).
  status                             Human-readable summary.
  brief  --id ID                     Print a paste-able work brief for a task.
  next   [--provider NAME] [--claim] [--ttl TEXT]
                                     Print the brief for the next actionable
                                     task; with --claim --provider, atomically
                                     claim it first (pull-work primitive).
         [--json]                    Emit the selected task as JSON for safe
                                     composition by another harness command.

State: ready -> claimed -> running -> review -> landing -> done. Any -> blocked (block);
blocked -> ready (reopen); claimed/running/review -> ready (release).
Tasks are stored in REPO/.oms/plan/tasks.json (override with --file).
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Parse: first non-option token is the command.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --file) [ "$#" -ge 2 ] || fail "--file requires path"; PLAN_FILE="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires value"; ID="$2"; shift 2 ;;
    --title) [ "$#" -ge 2 ] || fail "--title requires text"; TITLE="$2"; shift 2 ;;
    --goal) [ "$#" -ge 2 ] || fail "--goal requires text"; GOAL="$2"; shift 2 ;;
    --provider) [ "$#" -ge 2 ] || fail "--provider requires name"; PROVIDER="$2"; shift 2 ;;
    --ttl) [ "$#" -ge 2 ] || fail "--ttl requires text"; TTL="$2"; shift 2 ;;
    --reason) [ "$#" -ge 2 ] || fail "--reason requires text"; REASON="$2"; shift 2 ;;
    --artifact) [ "$#" -ge 2 ] || fail "--artifact requires path"; ARTIFACT="$2"; shift 2 ;;
    --patch) [ "$#" -ge 2 ] || fail "--patch requires path"; PATCH="$2"; shift 2 ;;
    --depends) [ "$#" -ge 2 ] || fail "--depends requires list"; DEPENDS="$2"; shift 2 ;;
    --allowed) [ "$#" -ge 2 ] || fail "--allowed requires list"; ALLOWED="$2"; shift 2 ;;
    --role) [ "$#" -ge 2 ] || fail "--role requires a name"; ROLE="$2"; shift 2 ;;
    --forbidden) [ "$#" -ge 2 ] || fail "--forbidden requires list"; FORBIDDEN="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires command"; VERIFY="$2"; shift 2 ;;
    --state) [ "$#" -ge 2 ] || fail "--state requires value"; STATE_FILTER="$2"; shift 2 ;;
    --lease-id) [ "$#" -ge 2 ] || fail "--lease-id requires value"; LEASE_ID="$2"; shift 2 ;;
    --claim) CLAIM=1; shift ;;
    --include-running) INCLUDE_RUNNING=1; shift ;;
    --include-review) INCLUDE_REVIEW=1; shift ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    init|add|claim|start|touch|review|land|finish|block|release|reclaim|reopen|show|list|ready|status|next|brief)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"; ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
PLAN_FILE="${PLAN_FILE:-$REPO/.oms/plan/tasks.json}"
if [ -n "$PROVIDER" ]; then
  PROVIDER="$(oms_normalize_provider "$PROVIDER")" ||
    fail "unknown provider: use codex, claude, or antigravity (agy)"
fi

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# All mutations and queries run in one python process: load -> act -> (write|print).
# The whole load/decide/save section runs under a file lock so concurrent
# `next --claim` from different agents cannot both win the same task (the write
# itself is atomic, but the read-decide-write critical section is not).
export OMS_PLAN_FILE="$PLAN_FILE" OMS_ACTION="$ACTION" OMS_TS="$ts" \
  OMS_ID="$ID" OMS_TITLE="$TITLE" OMS_GOAL="$GOAL" OMS_PROVIDER="$PROVIDER" \
  OMS_TTL="$TTL" OMS_REASON="$REASON" OMS_ARTIFACT="$ARTIFACT" OMS_PATCH="$PATCH" \
  OMS_DEPENDS="$DEPENDS" OMS_ALLOWED="$ALLOWED" OMS_FORBIDDEN="$FORBIDDEN" \
  OMS_VERIFY="$VERIFY" OMS_ROLE="$ROLE" OMS_STATE_FILTER="$STATE_FILTER" OMS_CLAIM="$CLAIM" \
  OMS_INCLUDE_RUNNING="$INCLUDE_RUNNING" OMS_INCLUDE_REVIEW="$INCLUDE_REVIEW" \
  OMS_LEASE_ID="$LEASE_ID" OMS_AS_JSON="$AS_JSON"

plan_run() {
python3 <<'PY'
import json, os, re, secrets, sys, tempfile

SCHEMA = 2
path = os.environ["OMS_PLAN_FILE"]
act = os.environ["OMS_ACTION"]
ts = os.environ["OMS_TS"]
def env(k): return os.environ.get(k, "")

STATES = {"ready", "claimed", "running", "review", "landing", "blocked", "done"}
ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")

def die(msg):
    sys.stderr.write("error: %s\n" % msg); sys.exit(2)

def load():
    if not os.path.exists(path):
        return {"schema": SCHEMA, "goal": "", "tasks": {}}
    with open(path, encoding="utf-8") as fh:
        d = json.load(fh)
    d.setdefault("tasks", {})
    d["schema"] = SCHEMA
    for task in d["tasks"].values():
        task.setdefault("lease_epoch", 0)
        task.setdefault("lease_id", "")
    return d

def save(d):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(d, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, path)   # atomic
    except Exception:
        os.unlink(tmp); raise

def split_list(s):
    return [x.strip() for x in re.split(r"[,\s]+", s) if x.strip()]

def require_id():
    i = env("OMS_ID")
    if not i: die("--id is required for %s" % act)
    if not ID_RE.match(i): die("--id must match [A-Za-z0-9._-]+")
    return i

def deps_done(d, t):
    return all(d["tasks"].get(x, {}).get("state") == "done" for x in t.get("depends", []))

def issue_lease(t):
    t["lease_epoch"] = int(t.get("lease_epoch", 0)) + 1
    t["lease_id"] = "lease_" + secrets.token_hex(16)

def require_current_lease(t):
    supplied = env("OMS_LEASE_ID")
    current = t.get("lease_id", "")
    if supplied and supplied != current:
        die("task %s lease mismatch; worker is stale" % t["id"])
    if env("OMS_HARNESS_CHILD") == "1" and current and not supplied:
        die("task %s requires --lease-id for harness child mutation" % t["id"])

def brief_text(t):
    lines = ["# Task %s: %s" % (t["id"], t["title"]), "state: %s" % t["state"]]
    lines.append("depends: %s" % (", ".join(t.get("depends", [])) or "(none)"))
    lines.append("allowed_paths: %s" % (", ".join(t.get("allowed_paths", [])) or "(unrestricted)"))
    if t.get("forbidden_paths"):
        lines.append("forbidden_paths: %s" % ", ".join(t["forbidden_paths"]))
    lines.append("verify: %s" % (t.get("verify") or "(none)"))
    if t.get("role"):
        lines.append("role: %s" % t["role"])
    return "\n".join(lines)

d = load()
tasks = d["tasks"]

if act == "init":
    d = {"schema": SCHEMA, "goal": env("OMS_GOAL"), "tasks": {}}
    save(d); print("plan: initialized (%s)" % path); sys.exit(0)

if act == "add":
    i = require_id(); title = env("OMS_TITLE")
    if not title: die("--title is required for add")
    if i in tasks: die("task already exists: %s" % i)
    depends = split_list(env("OMS_DEPENDS"))
    unknown = [x for x in depends if x not in tasks]
    if unknown: die("unknown dependency id(s): %s" % ", ".join(unknown))
    tasks[i] = {
        "id": i, "title": title, "state": "ready",
        "depends": depends,
        "allowed_paths": split_list(env("OMS_ALLOWED")),
        "forbidden_paths": split_list(env("OMS_FORBIDDEN")),
        "verify": env("OMS_VERIFY"),
        "role": env("OMS_ROLE"),
        "provider": "", "ttl": "", "artifact": "", "patch": "", "reason": "",
        "lease_epoch": 0, "lease_id": "", "review_lease_id": "",
        "created": ts, "updated": ts,
    }
    save(d); print("plan: added %s (%s)" % (i, title)); sys.exit(0)

def get_task(i):
    t = tasks.get(i)
    if not t: die("no such task: %s" % i)
    return t

if act in ("claim", "start", "finish", "review", "land", "block", "release", "reopen", "show", "touch"):
    i = require_id(); t = get_task(i)
    if act == "touch":
        # Heartbeat: a live worker refreshes claimed_at so reclaim's TTL clock
        # restarts and it is not mistaken for a dead worker mid-run.
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can be touched" % (i, t["state"]))
        require_current_lease(t)
        t["claimed_at"] = ts
    elif act == "claim":
        prov = env("OMS_PROVIDER")
        if not prov: die("--provider is required for claim")
        # Only a ready task can be claimed; a blocked task must be reopened first.
        if t["state"] != "ready":
            die("task %s is %s; only a ready task can be claimed (reopen blocked first)" % (i, t["state"]))
        if not deps_done(d, t):
            pending = [x for x in t["depends"] if tasks.get(x, {}).get("state") != "done"]
            die("task %s has unfinished dependencies: %s" % (i, ", ".join(pending)))
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="")
    elif act == "start":
        if t["state"] != "claimed": die("task %s is %s; claim it first" % (i, t["state"]))
        require_current_lease(t)
        t["state"] = "running"
    elif act == "review":
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can go to review" % (i, t["state"]))
        require_current_lease(t)
        t.update(state="review", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""),
                 review_lease_id=t.get("lease_id", ""))
    elif act == "land":
        if t["state"] != "review":
            die("task %s is %s; only reviewed work can enter landing" % (i, t["state"]))
        require_current_lease(t)
        if t.get("review_lease_id", "") != t.get("lease_id", ""):
            die("task %s review patch lease mismatch; patch is stale" % i)
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        t["state"] = "landing"
    elif act == "finish":
        # Done is a landing receipt, not a worker self-report. patch-land owns
        # the review -> landing fence after mechanical admission succeeds.
        if t["state"] != "landing":
            die("task %s is %s; finish only after reviewed work enters landing" % (i, t["state"]))
        require_current_lease(t)
        t.update(state="done", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""))
    elif act == "block":
        r = env("OMS_REASON")
        if not r: die("--reason is required for block")
        if t["state"] in ("claimed", "running", "review", "landing"):
            require_current_lease(t)
        t.update(state="blocked", reason=r)
    elif act == "release":
        # Requeue a claimed/running task (e.g. the worker died) back to ready.
        if t["state"] not in ("claimed", "running", "review", "landing"):
            die("task %s is %s; only a claimed/running/review/landing task can be released" % (i, t["state"]))
        require_current_lease(t)
        t.update(state="ready", provider="", ttl="", claimed_at="", reason="", lease_id="")
    elif act == "reopen":
        if t["state"] != "blocked":
            die("task %s is %s; only a blocked task can be reopened" % (i, t["state"]))
        t.update(state="ready", provider="", ttl="", claimed_at="", reason="", lease_id="")
    elif act == "show":
        print(json.dumps(t, ensure_ascii=False, indent=2)); sys.exit(0)
    t["updated"] = ts
    save(d); print("plan: %s -> %s" % (i, t["state"])); sys.exit(0)

# Read-only queries.
ordered = sorted(tasks.values(), key=lambda t: t.get("created", ""))

if act == "reclaim":
    # Dead-worker recovery: claim/next store provider+ttl+claimed_at, and this
    # is the consumer. Only ages out claimed (and, opted in, running) tasks by
    # default. review holds a finished artifact awaiting a reviewer, so TTL
    # expiry there means "waiting on reviewer", not "dead worker" — reclaiming
    # it is a separate opt-in with its own clock (updated = when it entered
    # review) and a longer default TTL, and keeps artifact/patch so the
    # finished work is not lost.
    import datetime
    def parse_ts(s):
        try:
            return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
        except Exception:
            return None
    now = parse_ts(ts)
    raw_ttl = env("OMS_TTL")
    if raw_ttl and not raw_ttl.isdigit():
        die("reclaim --ttl must be an integer number of seconds")
    default_ttl = int(raw_ttl) if raw_ttl else 3600
    review_ttl = int(raw_ttl) if raw_ttl else 86400
    states = {"claimed"}
    if env("OMS_INCLUDE_RUNNING") == "1":
        states.add("running")
    if env("OMS_INCLUDE_REVIEW") == "1":
        states.add("review")
    reclaimed = 0
    for t in ordered:
        if t["state"] not in states:
            continue
        if t["state"] == "review":
            anchor = parse_ts(t.get("updated", ""))
        else:
            anchor = parse_ts(t.get("claimed_at", "")) or parse_ts(t.get("updated", ""))
        if anchor is None:
            continue
        t_ttl = t.get("ttl", "")
        if t["state"] == "review":
            ttl_s = review_ttl
        elif isinstance(t_ttl, str) and t_ttl.isdigit():
            ttl_s = int(t_ttl)
        else:
            ttl_s = default_ttl
        age = int((now - anchor).total_seconds())
        if age < ttl_s:
            continue
        prov = t.get("provider", "") or "?"
        was = t["state"]
        t.update(state="ready", provider="", ttl="", claimed_at="", reason="", lease_id="")
        t["updated"] = ts
        reclaimed += 1
        print("plan: reclaimed %s from %s (age %ss > ttl %ss, was @%s)" % (t["id"], was, age, ttl_s, prov))
    if reclaimed:
        save(d)
    print("plan: reclaimed %d task(s)" % reclaimed)
    sys.exit(0)

if act == "brief":
    i = require_id()
    print(brief_text(get_task(i)))
    sys.exit(0)

if act == "next":
    candidates = [t for t in ordered if t["state"] == "ready" and deps_done(d, t)]
    if not candidates:
        sys.stderr.write("plan: no actionable task\n")
        sys.exit(3)
    t = candidates[0]
    if env("OMS_CLAIM") == "1":
        prov = env("OMS_PROVIDER")
        if not prov:
            die("--claim requires --provider")
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="")
        t["updated"] = ts
        save(d)
    if env("OMS_AS_JSON") == "1":
        print(json.dumps(t, ensure_ascii=False, indent=2))
    else:
        print(brief_text(t))
    sys.exit(0)

if act == "ready":
    for t in ordered:
        if t["state"] == "ready" and deps_done(d, t):
            print(t["id"])
    sys.exit(0)

if act == "list":
    sf = env("OMS_STATE_FILTER")
    if sf and sf not in STATES: die("unknown --state: %s" % sf)
    for t in ordered:
        if sf and t["state"] != sf: continue
        dep = (" depends=%s" % ",".join(t["depends"])) if t["depends"] else ""
        prov = (" @%s" % t["provider"]) if t.get("provider") else ""
        print("%-10s %-9s %s%s%s" % (t["id"], t["state"], t["title"], prov, dep))
    sys.exit(0)

if act == "status":
    if d.get("goal"): print("goal: %s" % d["goal"])
    by = {}
    for t in tasks.values(): by[t["state"]] = by.get(t["state"], 0) + 1
    order = ["ready", "claimed", "running", "review", "landing", "blocked", "done"]
    print("tasks: %d  [%s]" % (len(tasks),
        " ".join("%s=%d" % (s, by[s]) for s in order if by.get(s))))
    actionable = [t["id"] for t in ordered if t["state"] == "ready" and deps_done(d, t)]
    print("ready now: %s" % (" ".join(actionable) if actionable else "(none)"))
    blocked = [t for t in ordered if t["state"] == "blocked"]
    for t in blocked:
        print("blocked %s: %s" % (t["id"], t.get("reason", "")))
    waiting = [t["id"] for t in ordered
               if t["state"] == "ready" and not deps_done(d, t)]
    if waiting: print("waiting on deps: %s" % " ".join(waiting))
    sys.exit(0)

die("unhandled action: %s" % act)
PY
}

# Keep the plan dir out of git like the rest of .oms state.
mkdir -p "$(dirname "$PLAN_FILE")"
agent_memory_ensure_oms_ignore_for_path "$PLAN_FILE" 2>/dev/null || true

# Serialize the read-decide-write section against other agents.
oms_with_file_lock "$PLAN_FILE" plan_run
