#!/usr/bin/env bash
set -euo pipefail

# Print the latest CI conclusion for a branch (default: current) so a red run
# can't go unnoticed after a push — the "nobody looked" failure mode. Exits
# nonzero when the most recent completed run failed. gh is overridable
# (OMS_GH_BIN) for testing. `record` also appends the conclusion to
# .oms/ci.jsonl so a later session / oms state sees "CI failed on <sha>"
# instead of the result vanishing after the print.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

GH="${OMS_GH_BIN:-gh}"
RECORD=0
branch=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    record) RECORD=1; shift ;;
    --record) RECORD=1; shift ;;
    -h|--help) echo "Usage: ci-status.sh [record] [branch]"; exit 0 ;;
    *) branch="$1"; shift ;;
  esac
done
if [ -z "$branch" ]; then
  branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
fi

command -v "$GH" >/dev/null 2>&1 || { echo "ci-status: gh CLI not installed" >&2; exit 2; }

json="$("$GH" run list --branch "$branch" --limit 1 \
  --json status,conclusion,workflowName,headSha,url 2>/dev/null || true)"

LEDGER=""
if [ "$RECORD" = 1 ]; then
  state_root="$(oms_repo_root "$PWD")"
  LEDGER="${OMS_CI_LEDGER:-$state_root/.oms/ci.jsonl}"
  mkdir -p "$(dirname "$LEDGER")"
  agent_memory_ensure_oms_ignore_for_path "$LEDGER" 2>/dev/null || true
fi

OMS_BRANCH="$branch" OMS_JSON="$json" OMS_CI_LEDGER_OUT="$LEDGER" \
  OMS_CI_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" python3 - <<'PY'
import json, os, sys
branch = os.environ["OMS_BRANCH"]
raw = os.environ.get("OMS_JSON", "").strip()
try:
    runs = json.loads(raw) if raw else []
except Exception:
    runs = []
if not runs:
    print("ci-status: no runs for %s" % branch)
    sys.exit(0)
r = runs[0]
status = r.get("status")
concl = r.get("conclusion") or status
print("ci-status [%s]: %s %s  %s" % (
    branch, status, concl, r.get("url", "")))

ledger = os.environ.get("OMS_CI_LEDGER_OUT", "")
if ledger:
    sha = r.get("headSha", "")
    row = {"schema": 1, "ts": os.environ["OMS_CI_TS"], "branch": branch,
           "sha": sha, "status": status, "conclusion": concl, "url": r.get("url", "")}
    # Dedupe: skip if the last row is the same sha with the same conclusion.
    prev = None
    if os.path.isfile(ledger):
        for line in open(ledger, encoding="utf-8", errors="replace"):
            line = line.strip()
            if line:
                try:
                    prev = json.loads(line)
                except Exception:
                    pass
    if not (prev and prev.get("sha") == sha and prev.get("conclusion") == concl):
        with open(ledger, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

# Nonzero only on a completed failure; in-progress/queued are not failures.
sys.exit(1 if concl in ("failure", "timed_out", "cancelled", "startup_failure") else 0)
PY
