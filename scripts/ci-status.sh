#!/usr/bin/env bash
set -euo pipefail

# Print the latest CI conclusion for a branch (default: current) so a red run
# can't go unnoticed after a push — the "nobody looked" failure mode. Exits
# nonzero when the most recent completed run failed. gh is overridable
# (OMS_GH_BIN) for testing.

GH="${OMS_GH_BIN:-gh}"

branch="${1:-}"
if [ -z "$branch" ]; then
  branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
fi

command -v "$GH" >/dev/null 2>&1 || { echo "ci-status: gh CLI not installed" >&2; exit 2; }

json="$("$GH" run list --branch "$branch" --limit 1 \
  --json status,conclusion,workflowName,headSha,url 2>/dev/null || true)"

OMS_BRANCH="$branch" OMS_JSON="$json" python3 - <<'PY'
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
# Nonzero only on a completed failure; in-progress/queued are not failures.
sys.exit(1 if concl in ("failure", "timed_out", "cancelled", "startup_failure") else 0)
PY
