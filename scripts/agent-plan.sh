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
ACCEPT=""
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
  init   --goal TEXT [--accept CMD]  Create/replace the plan with a goal and an
                                     optional goal-level acceptance command —
                                     the executable definition of done.
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
  accept                             Run the stored acceptance command from the
                                     repo root (outside the plan lock), append
                                     one row to .oms/plan/progress.jsonl, and
                                     exit 0 on pass / 3 on fail.
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

A claim whose last heartbeat (claimed_at, refreshed by touch) is older than
OMS_PLAN_CLAIM_TTL seconds (default 3600; a numeric per-task --ttl wins) is a
dead worker's, and every read says so: list/status/brief tag it EXPIRED, show
adds claim_expired, and ready/next offer the task again — next --claim fences
the old worker by minting a new lease. Reads never write; reclaim (and
plan-run's pre-flight call to it) is what frees the stored row.
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
    --accept) [ "$#" -ge 2 ] || fail "--accept requires command"; ACCEPT="$2"; shift 2 ;;
    --state) [ "$#" -ge 2 ] || fail "--state requires value"; STATE_FILTER="$2"; shift 2 ;;
    --lease-id) [ "$#" -ge 2 ] || fail "--lease-id requires value"; LEASE_ID="$2"; shift 2 ;;
    --claim) CLAIM=1; shift ;;
    --include-running) INCLUDE_RUNNING=1; shift ;;
    --include-review) INCLUDE_REVIEW=1; shift ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    init|add|claim|start|touch|review|land|finish|block|release|reclaim|reopen|show|list|ready|status|next|brief|accept)
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

# One clock for both halves of claim expiry: the read paths present a claim
# past this TTL as expired, and reclaim's default frees exactly those rows.
# The deployed baseline is not measured yet, so it sits behind an override.
CLAIM_TTL="${OMS_PLAN_CLAIM_TTL:-3600}"
case "$CLAIM_TTL" in *[!0-9]*|"") CLAIM_TTL=3600 ;; esac

# The plan file is durable state; a credential in the goal or acceptance
# command would persist verbatim (same contract as fail-ledger's cmd field).
if [ -n "$GOAL$ACCEPT" ]; then
  scan="$(mktemp)" || fail "mktemp failed"
  printf '%s\n%s\n' "$GOAL" "$ACCEPT" > "$scan"
  if agent_memory_file_has_secret_content "$scan"; then
    rm -f "$scan"
    fail "goal/acceptance text looks sensitive; pass credentials via environment, not command text"
  fi
  rm -f "$scan"
fi

# All mutations and queries run in one python process: load -> act -> (write|print).
# The whole load/decide/save section runs under a file lock so concurrent
# `next --claim` from different agents cannot both win the same task (the write
# itself is atomic, but the read-decide-write critical section is not).
export OMS_PLAN_FILE="$PLAN_FILE" OMS_ACTION="$ACTION" OMS_TS="$ts" \
  OMS_ID="$ID" OMS_TITLE="$TITLE" OMS_GOAL="$GOAL" OMS_PROVIDER="$PROVIDER" \
  OMS_TTL="$TTL" OMS_REASON="$REASON" OMS_ARTIFACT="$ARTIFACT" OMS_PATCH="$PATCH" \
  OMS_DEPENDS="$DEPENDS" OMS_ALLOWED="$ALLOWED" OMS_FORBIDDEN="$FORBIDDEN" \
  OMS_VERIFY="$VERIFY" OMS_ACCEPT="$ACCEPT" OMS_ROLE="$ROLE" OMS_STATE_FILTER="$STATE_FILTER" OMS_CLAIM="$CLAIM" \
  OMS_INCLUDE_RUNNING="$INCLUDE_RUNNING" OMS_INCLUDE_REVIEW="$INCLUDE_REVIEW" \
  OMS_LEASE_ID="$LEASE_ID" OMS_AS_JSON="$AS_JSON" OMS_CLAIM_TTL="$CLAIM_TTL"

plan_run() {
python3 <<'PY'
import datetime, json, os, re, secrets, sys, tempfile

SCHEMA = 3
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
        return {"schema": SCHEMA, "goal": "", "accept": "", "tasks": {}}
    with open(path, encoding="utf-8") as fh:
        d = json.load(fh)
    d.setdefault("tasks", {})
    d.setdefault("accept", "")   # schema 2 plans predate the acceptance contract
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

CLAIM_TTL = int(os.environ.get("OMS_CLAIM_TTL") or 3600)

def parse_ts(s):
    try:
        return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
    except Exception:
        return None

now_dt = parse_ts(ts)

def claim_anchor(t):
    """When this task's TTL clock last restarted. touch refreshes claimed_at,
    so the clock runs from the last heartbeat, not from the claim. review ages
    from when it entered review instead: it waits on a reviewer, not a worker."""
    if t.get("state") == "review":
        return parse_ts(t.get("updated", ""))
    return parse_ts(t.get("claimed_at", "")) or parse_ts(t.get("updated", ""))

def claim_ttl_for(t, default_ttl=CLAIM_TTL):
    v = t.get("ttl", "")
    if isinstance(v, str) and v.isdigit():
        return int(v)
    return default_ttl

def claim_age(t):
    anchor = claim_anchor(t)
    if anchor is None or now_dt is None:
        return None
    return int((now_dt - anchor).total_seconds())

def claim_expired(t):
    """Read-time view of a claim: past its TTL it belongs to a worker nobody
    has heard from, so it is not a live hold on the task. Reads present that
    and change nothing (the same way agent-thread treats an expired CURRENT
    pointer); reclaim is what rewrites the row."""
    if t.get("state") != "claimed":
        return False
    age = claim_age(t)
    if age is None:
        return False
    return age >= claim_ttl_for(t)

def actionable(d, t):
    """Claimable right now: ready, or held by an expired claim."""
    return deps_done(d, t) and (t["state"] == "ready" or claim_expired(t))

def expiry_note(t):
    return "claim EXPIRED (age %ss >= ttl %ss, was @%s)" % (
        claim_age(t), claim_ttl_for(t), t.get("provider", "") or "?")

def brief_text(t):
    state = t["state"]
    if claim_expired(t):
        state = "%s [%s; claimable]" % (state, expiry_note(t))
    lines = ["# Task %s: %s" % (t["id"], t["title"]), "state: %s" % state]
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
    d = {"schema": SCHEMA, "goal": env("OMS_GOAL"), "accept": env("OMS_ACCEPT"), "tasks": {}}
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
        # Computed on a copy: the stored task keeps exactly the fields its
        # writers put there, and a later save() cannot persist a read's view.
        view = dict(t)
        view["claim_expired"] = claim_expired(t)
        if t.get("state") in ("claimed", "running", "review"):
            age = claim_age(t)
            if age is not None:
                view["claim_age_s"] = age
        print(json.dumps(view, ensure_ascii=False, indent=2)); sys.exit(0)
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
    # finished work is not lost. The claimed/running default is the same
    # OMS_PLAN_CLAIM_TTL the read paths present expiry with, so this frees
    # exactly the claims that already read as expired.
    raw_ttl = env("OMS_TTL")
    if raw_ttl and not raw_ttl.isdigit():
        die("reclaim --ttl must be an integer number of seconds")
    default_ttl = int(raw_ttl) if raw_ttl else CLAIM_TTL
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
        anchor = claim_anchor(t)
        if anchor is None or now_dt is None:
            continue
        if t["state"] == "review":
            ttl_s = review_ttl
        else:
            ttl_s = claim_ttl_for(t, default_ttl)
        age = int((now_dt - anchor).total_seconds())
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
    candidates = [t for t in ordered if actionable(d, t)]
    if not candidates:
        sys.stderr.write("plan: no actionable task\n")
        sys.exit(3)
    t = candidates[0]
    if claim_expired(t):
        sys.stderr.write("plan: %s %s; %s\n" % (
            t["id"], expiry_note(t),
            "re-claiming under a new lease" if env("OMS_CLAIM") == "1"
            else "offered as claimable"))
    if env("OMS_CLAIM") == "1":
        prov = env("OMS_PROVIDER")
        if not prov:
            die("--claim requires --provider")
        # A fresh lease is the fence: whatever the previous holder does next
        # fails the lease check instead of racing this worker.
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="")
        t["updated"] = ts
        save(d)
    if env("OMS_AS_JSON") == "1":
        view = dict(t)
        view["claim_expired"] = claim_expired(t)
        print(json.dumps(view, ensure_ascii=False, indent=2))
    else:
        print(brief_text(t))
    sys.exit(0)

if act == "ready":
    # Ids only: this output is consumed. The expiry note goes to stderr.
    for t in ordered:
        if not actionable(d, t):
            continue
        if claim_expired(t):
            sys.stderr.write("plan: %s %s; counted as ready\n" % (t["id"], expiry_note(t)))
        print(t["id"])
    sys.exit(0)

if act == "list":
    sf = env("OMS_STATE_FILTER")
    if sf and sf not in STATES: die("unknown --state: %s" % sf)
    for t in ordered:
        if sf and t["state"] != sf: continue
        dep = (" depends=%s" % ",".join(t["depends"])) if t["depends"] else ""
        prov = (" @%s" % t["provider"]) if t.get("provider") else ""
        # The state column keeps saying what is stored; the tag says how that
        # stored state reads now.
        tag = (" EXPIRED(%s)" % expiry_note(t)) if claim_expired(t) else ""
        print("%-10s %-9s %s%s%s%s" % (t["id"], t["state"], t["title"], prov, dep, tag))
    sys.exit(0)

if act == "status":
    if d.get("goal"): print("goal: %s" % d["goal"])
    if d.get("accept"):
        print("accept: %s" % d["accept"])
        progress_path = os.path.join(os.path.dirname(path), "progress.jsonl")
        last = None
        try:
            with open(progress_path, encoding="utf-8") as fh:
                for line in fh:
                    try:
                        last = json.loads(line)
                    except Exception:
                        continue
        except OSError:
            pass
        if last:
            print("acceptance: %s at %s (base %s)" % (
                last.get("status", "?"), last.get("ts", "?"),
                str(last.get("base_sha", "?"))[:12]))
    by = {}
    for t in tasks.values(): by[t["state"]] = by.get(t["state"], 0) + 1
    order = ["ready", "claimed", "running", "review", "landing", "blocked", "done"]
    print("tasks: %d  [%s]" % (len(tasks),
        " ".join("%s=%d" % (s, by[s]) for s in order if by.get(s))))
    claimable = [t["id"] for t in ordered if actionable(d, t)]
    print("ready now: %s" % (" ".join(claimable) if claimable else "(none)"))
    for t in ordered:
        if claim_expired(t):
            print("expired claim %s: %s" % (t["id"], expiry_note(t)))
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

# `accept` runs OUTSIDE the plan lock: the acceptance command is an arbitrary
# project check (often the full gate) and must not hold the task-graph lock
# for its whole runtime. It only reads the stored command, runs it from the
# repo root, and appends one receipt row — the executable answer to "is the
# goal actually met", which no per-task verify can give.
if [ "$ACTION" = "accept" ]; then
  [ -f "$PLAN_FILE" ] || fail "no plan at $PLAN_FILE; run: agent-plan init --goal ... --accept CMD"
  accept_cmd="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
print(d.get("accept", "") or "")
' "$PLAN_FILE")"
  [ -n "$accept_cmd" ] ||
    fail "plan has no acceptance command; set one with: agent-plan init --goal ... --accept CMD"
  progress="$(dirname "$PLAN_FILE")/progress.jsonl"
  out_tmp="$(mktemp)" || fail "mktemp failed"
  start_s="$(date +%s)"
  set +e
  ( cd "$REPO" && bash -c "$accept_cmd" ) > "$out_tmp" 2>&1
  accept_exit=$?
  set -e
  duration=$(( $(date +%s) - start_s ))
  base_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unborn)"
  out_digest="$(oms_sha256_stream < "$out_tmp" 2>/dev/null || echo unhashed)"
  verdict=pass
  [ "$accept_exit" -eq 0 ] || verdict=fail
  accept_digest="$(printf '%s' "$accept_cmd" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  # run_id/cycle are set by goal-drive so one run's rows correlate; manual
  # invocations leave them empty. The row is the goal-run protocol record:
  # enough to answer which command, on which tree, in which cycle, and why.
  OMS_PA_TS="$ts" OMS_PA_SHA="$base_sha" OMS_PA_VERDICT="$verdict" \
    OMS_PA_EXIT="$accept_exit" OMS_PA_DIGEST="$out_digest" OMS_PA_DUR="$duration" \
    OMS_PA_ACCEPT="$accept_digest" OMS_PA_RUN="${OMS_GOAL_RUN_ID:-}" \
    OMS_PA_CYCLE="${OMS_GOAL_CYCLE:-}" \
    python3 -c '
import json, os
row = {
    "schema": 1, "kind": "acceptance",
    "ts": os.environ["OMS_PA_TS"], "base_sha": os.environ["OMS_PA_SHA"],
    "status": os.environ["OMS_PA_VERDICT"], "exit": int(os.environ["OMS_PA_EXIT"]),
    "accept_sha256": os.environ["OMS_PA_ACCEPT"][:16],
    "output_sha256": os.environ["OMS_PA_DIGEST"][:16],
    "duration_s": int(os.environ["OMS_PA_DUR"]),
}
if os.environ.get("OMS_PA_RUN"): row["run_id"] = os.environ["OMS_PA_RUN"]
if os.environ.get("OMS_PA_CYCLE"): row["cycle"] = int(os.environ["OMS_PA_CYCLE"])
print(json.dumps(row, ensure_ascii=False))
' >> "$progress"
  echo "plan-accept: $verdict (exit $accept_exit, ${duration}s) base=$base_sha"
  if [ "$verdict" = "fail" ]; then
    echo "--- acceptance output (last 20 lines) ---" >&2
    tail -n 20 "$out_tmp" >&2
    rm -f "$out_tmp"
    exit 3
  fi
  rm -f "$out_tmp"
  exit 0
fi

# Serialize the read-decide-write section against other agents.
oms_with_file_lock "$PLAN_FILE" plan_run
