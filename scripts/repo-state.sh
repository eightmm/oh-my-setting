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
Usage: repo-state.sh [--repo PATH] [--json]

Print a read-only dashboard of the shared .oms state for a repo: the active
task packet, plan tasks by state (stale claims flagged), the experiment board
(active + stale), the current/open runs, the latest artifact-index rows, and
whether a change-guard is active.

Options:
  --repo PATH   Repo to inspect (default: current directory; anchored to the
                git worktree root).
  --json        Emit a single JSON object instead of the text view.
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

OMS_RS_REPO="$REPO" \
OMS_RS_JSON="$AS_JSON" \
OMS_RS_PLAN_TTL="${OMS_PLAN_CLAIM_TTL:-3600}" \
OMS_RS_REVIEW_TTL="${OMS_PLAN_REVIEW_TTL:-86400}" \
OMS_RS_BOARD_TTL="${OMS_EXPERIMENT_CLAIM_TTL:-86400}" \
OMS_RS_RUN_TTL="${OMS_RUN_CURRENT_TTL:-86400}" \
OMS_RS_GUARD_TTL="${OMS_GUARD_TTL:-86400}" \
python3 <<'PY'
import calendar, json, os, time

repo = os.environ["OMS_RS_REPO"]
as_json = os.environ["OMS_RS_JSON"] == "1"
plan_ttl = int(os.environ["OMS_RS_PLAN_TTL"])
review_ttl = int(os.environ["OMS_RS_REVIEW_TTL"])
board_ttl = int(os.environ["OMS_RS_BOARD_TTL"])
run_ttl = int(os.environ["OMS_RS_RUN_TTL"])
guard_ttl = int(os.environ["OMS_RS_GUARD_TTL"])
now = time.time()


def epoch(ts):
    try:
        return calendar.timegm(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return None


def read_jsonl(path):
    rows = []
    if not os.path.isfile(path):
        return rows
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except Exception:
                continue
    return rows


def oms(*parts):
    return os.path.join(repo, ".oms", *parts)


state = {}

# --- Active task packet: Goal + Next Step -----------------------------------
task = {"present": False}
tf = oms("task", "current.md")
if os.path.isfile(tf):
    task["present"] = True
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

# --- Plan DAG: counts by state, stale claims --------------------------------
plan = {"present": False, "by_state": {}, "stale": [], "stale_review": [], "actionable": []}
pf = oms("plan", "tasks.json")
if os.path.isfile(pf):
    try:
        pdata = json.load(open(pf, encoding="utf-8"))
    except Exception:
        pdata = {}
    tasks = pdata.get("tasks", {})
    if tasks:
        plan["present"] = True
        plan["goal"] = (pdata.get("goal") or "")[:200]
        done_ids = {i for i, t in tasks.items() if t.get("state") == "done"}
        for i, t in tasks.items():
            st = t.get("state", "?")
            plan["by_state"][st] = plan["by_state"].get(st, 0) + 1
            # Stale claim: claimed past the TTL since claimed_at (per-task ttl wins).
            if st == "claimed":
                e = epoch(t.get("claimed_at", ""))
                ttl = plan_ttl
                raw_ttl = t.get("ttl", "")
                if str(raw_ttl).isdigit():
                    ttl = int(raw_ttl)
                if e is not None and now - e >= ttl:
                    plan["stale"].append({"id": i, "provider": t.get("provider", "")})
            # Stale review: reclaim never touches review, so an abandoned
            # reviewer strands the task silently unless it is flagged here.
            if st == "review":
                e = epoch(t.get("updated", ""))
                if e is not None and now - e >= review_ttl:
                    plan["stale_review"].append({"id": i, "provider": t.get("provider", "")})
            # Actionable: ready with all deps done.
            if st == "ready" and all(d in done_ids for d in t.get("depends", [])):
                plan["actionable"].append(i)
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
arts = read_jsonl(oms("artifacts", "index.jsonl"))
state["artifacts"] = {
    "total": len(arts),
    "latest": [
        {k: r.get(k) for k in ("ts", "kind", "provider", "exit") if r.get(k) not in (None, "")}
        for r in arts[-5:]
    ],
}

# --- Unresolved failures (fail-ledger) --------------------------------------
fail_rows = read_jsonl(oms("failures.jsonl"))
fagg, forder = {}, []
for r in fail_rows:
    fp = r.get("fingerprint")
    if not fp:
        continue
    if fp not in fagg:
        fagg[fp] = {"count": 0, "resolved": False, "last": None}
        forder.append(fp)
    if r.get("event") == "resolved":
        fagg[fp]["resolved"] = True
        fagg[fp]["count"] = 0
    elif r.get("event") == "fail":
        fagg[fp]["count"] += 1
        fagg[fp]["resolved"] = False
        fagg[fp]["last"] = r
open_fails = []
for fp in forder:
    d = fagg[fp]
    if d["resolved"] or d["count"] == 0:
        continue
    last = d["last"] or {}
    open_fails.append({"fingerprint": fp, "count": d["count"],
                       "summary": (last.get("summary") or last.get("cmd", ""))[:80]})
state["failures"] = {"open": open_fails[-5:], "open_total": len(open_fails)}

# --- Latest CI conclusion for HEAD's branch ---------------------------------
ci = {"present": False}
ci_rows = read_jsonl(oms("ci.jsonl"))
if ci_rows:
    ci["present"] = True
    last = ci_rows[-1]
    ci.update({k: last.get(k) for k in ("branch", "sha", "status", "conclusion", "url")})
state["ci"] = ci

# --- In-flight delegations (liveness files) ---------------------------------
def pid_alive(pid):
    try:
        os.kill(int(pid), 0)
        return True
    except (OSError, ValueError, TypeError):
        return False

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
        alive = pid_alive(d.get("pid"))
        delegations.append({"id": d.get("id"), "provider": d.get("provider"),
                            "role": d.get("role", ""), "started_at": d.get("started_at"),
                            "live": alive})
state["delegations"] = delegations

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

if as_json:
    print(json.dumps(state, ensure_ascii=False, indent=2))
else:
    def line(s):
        print(s)

    line("# repo-state: %s" % repo)
    t = state["task"]
    if t["present"]:
        line("\n## Active task")
        if t.get("goal"):
            line("  goal: %s" % t["goal"])
        if t.get("next"):
            line("  next: %s" % t["next"])
    else:
        line("\n## Active task: none")

    p = state["plan"]
    if p["present"]:
        line("\n## Plan")
        if p.get("goal"):
            line("  goal: %s" % p["goal"])
        line("  by state: %s" % ", ".join("%s=%d" % (k, v) for k, v in sorted(p["by_state"].items())))
        if p["actionable"]:
            line("  actionable now: %s" % ", ".join(p["actionable"]))
        if p["stale"]:
            line("  STALE claims: %s" % ", ".join("%s(%s)" % (s["id"], s["provider"]) for s in p["stale"]))
        if p["stale_review"]:
            line("  STALE review (reviewer gone? reclaim --include-review): %s"
                 % ", ".join(s["id"] for s in p["stale_review"]))
    else:
        line("\n## Plan: none")

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

    dl = state["delegations"]
    if dl:
        line("\n## In-flight delegations")
        for e in dl:
            tag = "live" if e["live"] else "ORPHAN (dead pid)"
            line("  %s %s%s  %s  [%s]" % (
                e.get("provider", "?"), ("role=%s " % e["role"]) if e.get("role") else "",
                e.get("id", "?"), e.get("started_at", "?"), tag))

    ci = state["ci"]
    if ci["present"]:
        line("\n## CI (%s)" % (ci.get("branch") or "?"))
        line("  %s %s  %s" % (ci.get("status") or "?", ci.get("conclusion") or "?",
                              (ci.get("sha") or "")[:12]))

    fl = state["failures"]
    if fl["open_total"] > 0:
        line("\n## Unresolved failures (%d)" % fl["open_total"])
        for e in fl["open"]:
            line("  %s  x%d  %s" % (e["fingerprint"], e["count"], e["summary"]))

    a = state["artifacts"]
    line("\n## Artifacts (%d total)" % a["total"])
    for row in a["latest"]:
        line("  %s %s/%s exit=%s" % (row.get("ts", "?"), row.get("kind", "?"),
                                     row.get("provider", "") or "-", row.get("exit", "?")))

    if state["change_guard"]["active"]:
        tag = " (STALE — abandoned? end it: change-guard.sh end)" if state["change_guard"]["stale"] else ""
        line("\n## Change-guard: ACTIVE%s" % tag)
PY
