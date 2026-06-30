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

REPO="$PWD"
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
STATE_FILTER=""

usage() {
  cat <<'EOF'
Usage: agent-plan.sh [--repo PATH] [--file PATH] <command> [options]

Commands:
  init   --goal TEXT                 Create/replace the plan with a goal.
  add    --id ID --title TEXT        Add a task (state: ready).
         [--depends a,b] [--allowed "p1,p2"] [--forbidden "p3"]
         [--verify CMD]
  claim  --id ID --provider NAME [--ttl TEXT]   Mark a task claimed by a worker.
  start  --id ID                     Mark a claimed task running.
  finish --id ID [--artifact PATH] [--patch PATH]   Mark a task done.
  block  --id ID --reason TEXT       Mark a task blocked.
  reopen --id ID                     Return a task to ready.
  show   --id ID                     Print one task as JSON.
  list   [--state STATE]             List tasks (optionally by state).
  ready                              Print ids actionable now (deps done).
  status                             Human-readable summary.

State: ready -> claimed -> running -> {review|done}; block -> blocked; reopen -> ready.
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
    --forbidden) [ "$#" -ge 2 ] || fail "--forbidden requires list"; FORBIDDEN="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires command"; VERIFY="$2"; shift 2 ;;
    --state) [ "$#" -ge 2 ] || fail "--state requires value"; STATE_FILTER="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    init|add|claim|start|finish|block|reopen|show|list|ready|status)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"; ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
PLAN_FILE="${PLAN_FILE:-$REPO/.oms/plan/tasks.json}"

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# All mutations and queries run in one python process: load -> act -> (write|print).
OMS_PLAN_FILE="$PLAN_FILE" OMS_ACTION="$ACTION" OMS_TS="$ts" \
OMS_ID="$ID" OMS_TITLE="$TITLE" OMS_GOAL="$GOAL" OMS_PROVIDER="$PROVIDER" \
OMS_TTL="$TTL" OMS_REASON="$REASON" OMS_ARTIFACT="$ARTIFACT" OMS_PATCH="$PATCH" \
OMS_DEPENDS="$DEPENDS" OMS_ALLOWED="$ALLOWED" OMS_FORBIDDEN="$FORBIDDEN" \
OMS_VERIFY="$VERIFY" OMS_STATE_FILTER="$STATE_FILTER" \
python3 <<'PY'
import json, os, re, sys, tempfile

SCHEMA = 1
path = os.environ["OMS_PLAN_FILE"]
act = os.environ["OMS_ACTION"]
ts = os.environ["OMS_TS"]
def env(k): return os.environ.get(k, "")

STATES = {"ready", "claimed", "running", "review", "blocked", "done"}
ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")

def die(msg):
    sys.stderr.write("error: %s\n" % msg); sys.exit(2)

def load():
    if not os.path.exists(path):
        return {"schema": SCHEMA, "goal": "", "tasks": {}}
    with open(path, encoding="utf-8") as fh:
        d = json.load(fh)
    d.setdefault("tasks", {})
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
        "provider": "", "ttl": "", "artifact": "", "patch": "", "reason": "",
        "created": ts, "updated": ts,
    }
    save(d); print("plan: added %s (%s)" % (i, title)); sys.exit(0)

def get_task(i):
    t = tasks.get(i)
    if not t: die("no such task: %s" % i)
    return t

if act in ("claim", "start", "finish", "block", "reopen", "show"):
    i = require_id(); t = get_task(i)
    if act == "claim":
        prov = env("OMS_PROVIDER")
        if not prov: die("--provider is required for claim")
        if t["state"] not in ("ready", "blocked"):
            die("task %s is %s; only ready/blocked can be claimed" % (i, t["state"]))
        if not deps_done(d, t):
            pending = [x for x in t["depends"] if tasks.get(x, {}).get("state") != "done"]
            die("task %s has unfinished dependencies: %s" % (i, ", ".join(pending)))
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"), reason="")
    elif act == "start":
        if t["state"] != "claimed": die("task %s is %s; claim it first" % (i, t["state"]))
        t["state"] = "running"
    elif act == "finish":
        t.update(state="done", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""))
    elif act == "block":
        r = env("OMS_REASON")
        if not r: die("--reason is required for block")
        t.update(state="blocked", reason=r)
    elif act == "reopen":
        t.update(state="ready", provider="", ttl="", reason="")
    elif act == "show":
        print(json.dumps(t, ensure_ascii=False, indent=2)); sys.exit(0)
    t["updated"] = ts
    save(d); print("plan: %s -> %s" % (i, t["state"])); sys.exit(0)

# Read-only queries.
ordered = sorted(tasks.values(), key=lambda t: t.get("created", ""))

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
    order = ["ready", "claimed", "running", "review", "blocked", "done"]
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

# Keep the plan dir out of git like the rest of .oms state.
agent_memory_ensure_oms_ignore_for_path "$PLAN_FILE" 2>/dev/null || true
