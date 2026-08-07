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
        "oms artifact-index --repo . unresolved", unresolved)

open_failures = int(state.get("failures", {}).get("open_total", 0) or 0)
if open_failures:
    add(40, "P1", "open-failures", "%d unresolved failure fingerprint(s)" % open_failures,
        "oms fail-ledger --repo . list", open_failures)

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
report = {
    "schema": 1,
    "actionable": len(items),
    "safe_actions": actions,
    "items": items,
}
if os.environ.get("OMS_INBOX_JSON") == "1":
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
else:
    print("oms inbox: %d item(s)" % len(items))
    for item in items:
        print("%s %s: %s" % (item["priority"], item["code"], item["summary"]))
        print("  next: %s" % item["command"])
    if actions:
        print("safe actions: %s" % ", ".join(actions))
PY
