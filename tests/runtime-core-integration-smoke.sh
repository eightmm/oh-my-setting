#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-runtime-integration.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
REPO="$TMP/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" config user.email test@example.com
git -C "$REPO" config user.name 'OMS Runtime Test'

cat > "$REPO/PROJECT.md" <<'PROJECT'
# Runtime integration fixture

## Goal

Project typed task and evidence state through the existing OMS query surface.

## Acceptance Criteria

- [id:integration-projection] Runtime envelope is visible from repo-state.
- [id:integration-evidence] Missing criterion evidence reaches the inbox.
PROJECT
mkdir -p "$REPO/.oms/task"
cat > "$REPO/.oms/task/current.md" <<'TASK'
# Active Agent Task

- task_id: integration-task
- status: active

## Goal

Integrate the runtime projection.

## Done Criteria

- [id:integration-task-gate] The integration gate passes.

## Verify

bash tests/runtime-core-integration-smoke.sh

## Next Step

Bind current evidence.
TASK
printf '*\n' > "$REPO/.oms/.gitignore"
git -C "$REPO" add PROJECT.md
git -C "$REPO" commit -qm fixture

"$ROOT/scripts/runtime-core.sh" --repo "$REPO" envelope show > "$TMP/envelope.json"
python3 - "$TMP/envelope.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 2
assert row["evidence"]["counts"].get("missing", 0) >= 1
assert row["next_actions"]
PY

"$ROOT/scripts/repo-state.sh" --repo "$REPO" --json > "$TMP/state.json"
python3 - "$TMP/state.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
runtime = row["runtime"]
assert runtime["healthy"] is True
assert runtime["schema"] == 2
assert runtime["evidence"]["counts"].get("missing", 0) >= 1
assert runtime["next_actions"]
PY

"$ROOT/scripts/inbox.sh" --repo "$REPO" --json > "$TMP/inbox.json"
python3 - "$TMP/inbox.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
codes = {item["code"] for item in row["items"]}
assert "runtime-evidence-missing" in codes
assert row["recommended_actions"]
PY

"$ROOT/scripts/state-verify.sh" --repo "$REPO" --json > "$TMP/verify.json"
python3 - "$TMP/verify.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["delegated"]["runtime_core_exit"] == 0
assert not any(item["family"] == "runtime-core" and item["level"] == "fail" for item in row["findings"])
PY

echo 'runtime-core-integration-smoke: ok'
