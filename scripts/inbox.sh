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
  "$ROOT/scripts/state.sh" --repo "$REPO" --json > "$tmp/state.json"
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
python3 "$ROOT/scripts/lib/inbox_projection.py" "$tmp/state.json"
