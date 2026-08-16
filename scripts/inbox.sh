#!/usr/bin/env bash
set -euo pipefail

# Prioritized attention view derived entirely from `oms state`. The default is
# read-only. `--fix-safe` performs only three idempotent mechanical repairs:
# reclaim expired claimed plan leases, refresh a stale CI record, and resolve
# artifact failures whose exact patch bytes later succeeded. Failures, other
# unresolved artifacts, reviews, guards, and threads always remain judgments.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
AS_JSON=0
FIX_SAFE=0

usage() {
  cat <<'EOF'
Usage: inbox.sh [--repo PATH] [--json] [--fix-safe]

Rank the shared harness state by attention needed and print one exact next
command for every item. The default is a pure query.

  --repo PATH  Repository to inspect (default: current repository).
  --json       Emit one schema-1 JSON object.
  --fix-safe   Reclaim expired claimed plan leases, refresh stale CI, and
               resolve exactly-superseded artifact failures, then report the
               remaining inbox. Never deletes work or judges anything else.

Only cross-agent threads idle past OMS_THREAD_STALE_TTL are listed, and
OMS_THREAD_ATTENTION=0 drops that advisory entirely.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    --fix-safe) FIX_SAFE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-inbox.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

read_state() {
  "$ROOT/scripts/repo-state.sh" --repo "$REPO" --json > "$tmp/state.json"
}

read_state
safe_actions=""
if [ "$FIX_SAFE" = 1 ]; then
  stale_claims="$(python3 - "$tmp/state.json" <<'PY'
import json, sys
state = json.load(open(sys.argv[1], encoding="utf-8"))
print(len(state.get("plan", {}).get("stale", [])))
PY
)"
  if [ "$stale_claims" -gt 0 ]; then
    if "$ROOT/scripts/agent-plan.sh" --repo "$REPO" reclaim >/dev/null 2>&1; then
      safe_actions="reclaimed-stale-plan"
    else
      safe_actions="reclaim-stale-plan-failed"
    fi
  fi

  # Refresh only where a query can change the answer: a pushed HEAD with no
  # run recorded for it, or a repo whose push state cannot be determined. An
  # unpushed HEAD has no run to find, so asking gh is pure latency.
  ci_stale="$(python3 - "$tmp/state.json" <<'PY'
import json, sys
ci = json.load(open(sys.argv[1], encoding="utf-8")).get("ci", {})
print(1 if ci.get("state") in ("stale", "pending") else 0)
PY
)"
  if [ "$ci_stale" = 1 ] && command -v "${OMS_GH_BIN:-gh}" >/dev/null 2>&1; then
    # A completed red run makes record exit one by design; it still refreshed
    # the ledger, so safe repair records the attempt and leaves the red item.
    (cd "$REPO" && "$ROOT/scripts/ci-status.sh" record) >/dev/null 2>&1 || true
    safe_actions="${safe_actions:+$safe_actions,}refreshed-ci"
  fi

  # Mechanically resolvable by construction: a failed artifact row whose exact
  # patch bytes the same provider later admitted or landed. Idempotent sweep.
  swept="$("$ROOT/scripts/artifact-index.sh" --repo "$REPO" resolve-superseded 2>/dev/null |
    grep -c '^artifact-index: resolved ' || true)"
  if [ "${swept:-0}" -gt 0 ] 2>/dev/null; then
    safe_actions="${safe_actions:+$safe_actions,}resolved-superseded:$swept"
  fi
  read_state
fi

OMS_INBOX_JSON="$AS_JSON" OMS_INBOX_SAFE="$safe_actions" \
OMS_INBOX_THREADS="${OMS_THREAD_ATTENTION:-1}" \
python3 - "$tmp/state.json" <<'PY'
import json, os, sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
items = []

def add(order, priority, code, summary, command, count=1):
    items.append({
        "order": order,
        "priority": priority,
        "code": code,
        "count": count,
        "summary": summary,
        "command": command,
    })

plan = state.get("plan", {})
stale = plan.get("stale", [])
if stale:
    add(10, "P1", "stale-plan-claim", "%d expired plan claim(s)" % len(stale),
        "oms agent-plan --repo . reclaim", len(stale))

operations = state.get("agent_operations", {})
operation_states = operations.get("by_state", {})
if not operations.get("healthy", True):
    add(5, "P1", "agent-lifecycle-corrupt", "agent lifecycle stream is invalid",
        "oms agent-events --repo . validate")
for state_name, code, summary, command, order, priority in (
    ("blocked", "agent-blocked", "blocked agent attempt(s)",
     "oms agent-events --repo . list --state blocked", 11, "P1"),
    ("waiting_input", "agent-waiting-input", "agent attempt(s) waiting for input",
     "oms agent-events --repo . list --state waiting_input", 12, "P1"),
    ("waiting_approval", "agent-waiting-approval", "agent attempt(s) waiting for approval",
     "oms agent-events --repo . list --state waiting_approval", 13, "P1"),
    ("queued", "agent-queued", "queued agent attempt(s) not yet dispatched",
     "oms agent-supervisor --repo . dispatch", 14, "P2"),
    ("review", "agent-review", "agent attempt(s) ready for review",
     "oms agent-events --repo . list --state review", 15, "P2"),
):
    count = int(operation_states.get(state_name, 0) or 0)
    if count:
        add(order, priority, code, "%d %s" % (count, summary), command, count)

approvals = state.get("approvals", {})
if not approvals.get("healthy", True):
    add(6, "P1", "approval-store-corrupt", "private approval stream is invalid",
        "oms approval-inbox --repo . validate")
pending_approvals = int(approvals.get("pending", 0) or 0)
if pending_approvals:
    add(16, "P1", "pending-approval", "%d approval action(s) need a decision or completion" % pending_approvals,
        "oms approval-inbox --repo . list --pending", pending_approvals)

runtime = state.get("runtime", {})
if not runtime.get("healthy", False):
    add(7, "P1", "runtime-core-unavailable", "typed task/evidence projection is unavailable",
        "oms runtime doctor --strict")
else:
    evidence_counts = runtime.get("evidence", {}).get("counts", {})
    failed = int(evidence_counts.get("failed", 0) or 0)
    stale_evidence = int(evidence_counts.get("stale", 0) or 0)
    missing_evidence = int(evidence_counts.get("missing", 0) or 0)
    if failed:
        add(17, "P1", "runtime-evidence-failed", "%d acceptance criterion/criteria have failing evidence" % failed,
            "oms runtime evidence show", failed)
    if stale_evidence:
        add(75, "P2", "runtime-evidence-stale", "%d acceptance criterion/criteria have stale evidence" % stale_evidence,
            "oms runtime evidence show", stale_evidence)
    if missing_evidence:
        add(85, "P3", "runtime-evidence-missing", "%d acceptance criterion/criteria lack current evidence" % missing_evidence,
            "oms runtime evidence show", missing_evidence)

ci = state.get("ci", {})
ci_state = ci.get("state")
if ci_state == "current" and ci.get("conclusion") in (
    "failure", "timed_out", "cancelled", "startup_failure"
):
    add(20, "P1", "ci-failed", "latest CI conclusion is %s" % ci.get("conclusion"),
        "oms ci-status")
elif ci_state == "stale":
    # Only where push state is unknowable does "does it describe HEAD?" remain
    # the open question; elsewhere the item below names what to actually do.
    add(20, "P1", "ci-stale", "recorded CI does not match the current HEAD",
        "oms state --repo . --refresh-ci")
elif ci_state == "unpushed":
    # Committed work no CI has ever seen. Not urgent like a red run, but it is
    # the one CI state with an action, and silence here is how commits sit for
    # a day behind a dashboard that only says "stale".
    ahead = int(ci.get("ahead") or 0)
    add(25, "P2", "unpushed-head",
        "%d commit(s) not pushed — CI has never seen this HEAD" % ahead,
        "git push", ahead)

artifacts = state.get("artifacts", {})
unresolved = int(artifacts.get("counts", {}).get("unresolved", 0) or 0)
if unresolved:
    add(30, "P1", "unresolved-artifacts", "%d unresolved artifact outcome(s)" % unresolved,
        "oms artifact-index --repo . unresolved; oms artifact-index --repo . resolve-recovered --dry-run", unresolved)

# A failing or stalled auto-updater is machine state, but it is exactly what
# an agent resuming this repo needs to distrust (a weeks-dead timer once hid
# an entire release behind "doctor: ok"). failed/overdue act now; a missing
# trigger or never-run state is setup debt.
install = state.get("install", {})
au = str(install.get("auto_update", "") or "")
au_detail = str(install.get("auto_update_detail", "") or "")
if au in ("failed", "overdue"):
    add(45, "P1", "auto-update-" + au,
        "install auto-update is %s%s" % (au, ": " + au_detail if au_detail else ""),
        "oms auto-update status", 1)
elif au in ("unwired", "no-run"):
    add(20, "P2", "auto-update-" + au,
        "install auto-update is %s%s" % (au, ": " + au_detail if au_detail else ""),
        "oms auto-update status", 1)

failures = state.get("failures", {})
open_failures = int(failures.get("open_total", 0) or 0)
# actionable/retiring: a hook row seen once auto-retires on its TTL and is
# not worth P1 attention; deliberate records and recurring hook failures
# are. An older repo-state without the split falls back to the old count.
actionable = failures.get("actionable_total")
retiring = int(failures.get("retiring_total", 0) or 0)
if actionable is None:
    actionable = open_failures
    retiring = 0
actionable = int(actionable or 0)
if actionable:
    label = "%d actionable failure fingerprint(s)" % actionable
    if retiring:
        label += " (+%d retiring on TTL)" % retiring
    add(40, "P1", "open-failures", label, "oms fail-ledger --repo . list", actionable)
elif retiring:
    add(15, "P3", "retiring-failures",
        "%d one-shot hook failure(s) auto-retire on their TTL" % retiring,
        "oms fail-ledger --repo . list", retiring)

stale_reviews = plan.get("stale_review", [])
if stale_reviews:
    add(50, "P1", "stale-plan-review", "%d abandoned review lease(s)" % len(stale_reviews),
        "oms agent-plan --repo . reclaim --include-review", len(stale_reviews))

guard = state.get("change_guard", {})
if guard.get("active") and guard.get("stale"):
    add(60, "P1", "stale-change-guard", "change guard is active but stale",
        "oms change-guard --repo . status")

landings = state.get("landings", {}).get("outstanding", [])
if landings:
    add(70, "P1", "interrupted-landing", "%d interrupted patch landing(s)" % len(landings),
        "oms state-verify --repo .", len(landings))

# Only abandoned threads are attention: a live conversation is not an inbox
# item, and the advisory never asks for a close it could make itself.
stale_threads = int(state.get("threads", {}).get("stale_open", 0) or 0)
if stale_threads and os.environ.get("OMS_INBOX_THREADS") != "0":
    add(80, "P2", "stale-threads",
        "%d open cross-agent thread(s) idle past the stale TTL" % stale_threads,
        "oms thread list --stale", stale_threads)

items.sort(key=lambda item: (item["order"], item["code"]))
for item in items:
    item.pop("order", None)
actions = [entry for entry in os.environ.get("OMS_INBOX_SAFE", "").split(",") if entry]
recommended = [
    {key: action.get(key) for key in ("id", "priority", "authority", "reason", "command")}
    for action in state.get("runtime", {}).get("next_actions", [])[:3]
]
report = {
    "schema": 1,
    "actionable": len(items),
    "safe_actions": actions,
    "items": items,
    "recommended_actions": recommended,
}
if os.environ.get("OMS_INBOX_JSON") == "1":
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
else:
    print("oms inbox: %d item(s)" % len(items))
    for item in items:
        print("%s %s: %s" % (item["priority"], item["code"], item["summary"]))
        print("  next: %s" % item["command"])
    if recommended:
        print("runtime recommendations:")
        for action in recommended:
            print("  %s: %s" % (action.get("id", "action"), action.get("command", "-")))
    if actions:
        print("safe actions: %s" % ", ".join(actions))
PY
