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

"$ROOT/scripts/runtime.sh" --repo "$REPO" envelope show > "$TMP/envelope.json"
python3 - "$TMP/envelope.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 2
assert row["evidence"]["counts"].get("missing", 0) >= 1
assert row["next_actions"]
PY

"$ROOT/scripts/state.sh" --repo "$REPO" --json > "$TMP/state.json"
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

# The producer chain end to end: the shared appender carries covers and a
# context-manifest digest, and the projection links the row with no manual
# binding. Malformed coverage fails the append instead of publishing a
# half-labeled row.
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/true.sh"
(
  cd "$REPO"
  # shellcheck source=/dev/null
  . "$ROOT/scripts/lib/peer-common.sh"
  OMS_INDEX_COVERS_JSON='{"covers":["integration-projection"],"status":"verified","scope_digest":"0123456789abcdef"}' \
  OMS_INDEX_CONTEXT_MANIFEST_DIGEST="fedcba9876543210" \
    ma_append_artifact_index "$REPO" review-verify local 0 "" "" "" 0
  if OMS_INDEX_COVERS_JSON='{"covers":"not-a-list"}' \
    ma_append_artifact_index "$REPO" review-verify local 0 "" "" "" 0 2>/dev/null; then
    echo "runtime-core-integration-smoke: malformed covers payload must fail the append" >&2
    exit 1
  fi
)
python3 - "$REPO/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
tagged = [row for row in rows if row.get("covers") == ["integration-projection"]]
assert len(tagged) == 1, rows
assert tagged[0]["status"] == "verified"
assert tagged[0]["scope_digest"] == "0123456789abcdef"
assert tagged[0]["context_manifest_digest"] == "fedcba9876543210"
PY
"$ROOT/scripts/runtime.sh" --repo "$REPO" envelope show > "$TMP/envelope2.json"
python3 - "$TMP/envelope2.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
statuses = {item["id"]: item["status"] for item in row["criteria"]}
assert statuses["integration-projection"] == "verified", statuses
PY

echo 'runtime-core-integration-smoke: covers chain ok'

# Front door: patch-admit --covers. An unknown id is a usage error before any
# admission side effect; a valid id rides the admission receipt exit-judged
# (verified on ADMIT, failed on REJECT) and the projection links it with no
# manual binding.
capture_patch() {  # capture_patch MESSAGE OUT
  git -C "$REPO" add -A ':!.oms'
  git -C "$REPO" -c user.email=test@example.com -c user.name='OMS Runtime Test' \
    commit -qm "$1"
  git -C "$REPO" diff HEAD~1 HEAD > "$2"
  git -C "$REPO" reset -q --hard HEAD~1
}
printf '\nadmitted line\n' >> "$REPO/PROJECT.md"
capture_patch admit-fixture "$TMP/admit.patch"

rc=0
"$ROOT/scripts/patch-admit.sh" --repo "$REPO" --patch "$TMP/admit.patch" \
  --verify true --covers no-such-criterion >/dev/null 2>"$TMP/admit-err" || rc=$?
[ "$rc" = 2 ] || {
  echo "unknown covers id must exit 2, got $rc" >&2
  exit 1
}
grep -q "unknown criterion id" "$TMP/admit-err" || {
  echo "unknown covers id must be named in the error" >&2
  exit 1
}
[ ! -d "$REPO/.oms/artifacts/admit" ] || {
  echo "a rejected covers id must not leave admission artifacts" >&2
  exit 1
}

"$ROOT/scripts/patch-admit.sh" --repo "$REPO" --patch "$TMP/admit.patch" \
  --verify true --covers integration-evidence > "$TMP/admit-verdict" 2>/dev/null
grep -qx ADMIT "$TMP/admit-verdict" || {
  echo "covers admission should ADMIT: $(cat "$TMP/admit-verdict")" >&2
  exit 1
}

rc=0
"$ROOT/scripts/patch-admit.sh" --repo "$REPO" --patch "$TMP/admit.patch" \
  --verify false --covers integration-task-gate >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || {
  echo "a failing verify must REJECT" >&2
  exit 1
}

python3 - "$REPO/.oms/artifacts/index.jsonl" <<'PY'
import json, re, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
admits = [row for row in rows if row.get("kind") == "patch-admit"]
admitted = [row for row in admits if row.get("covers") == ["integration-evidence"]]
assert len(admitted) == 1, admits
assert admitted[0]["status"] == "verified", admitted[0]
assert re.match(r"^[0-9a-f]{16}$", admitted[0].get("scope_digest", "")), admitted[0]
rejected = [row for row in admits if row.get("covers") == ["integration-task-gate"]]
assert len(rejected) == 1, admits
assert rejected[0]["status"] == "failed", rejected[0]
PY
"$ROOT/scripts/runtime.sh" --repo "$REPO" envelope show > "$TMP/envelope3.json"
python3 - "$TMP/envelope3.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
statuses = {item["id"]: item["status"] for item in row["criteria"]}
assert statuses["integration-evidence"] == "verified", statuses
assert statuses["integration-task-gate"] == "failed", statuses
PY

echo 'runtime-core-integration-smoke: patch-admit covers ok'

# A task-linked admission receipt fails closed when its index write fails:
# the projection trusts the latest row per criterion, so a dropped REJECT
# would let a stale ADMIT keep reading as current verification. Without
# task lineage the row binds nothing and stays best-effort.
rc=0
OMS_TASK_ID=t1 OMS_ARTIFACT_INDEX=/proc/version/cannot/index.jsonl \
  "$ROOT/scripts/patch-admit.sh" --repo "$REPO" --patch "$TMP/admit.patch" \
  --verify true >/dev/null 2>"$TMP/admit-closed-err" || rc=$?
[ "$rc" != 0 ] || {
  echo "a task-linked admission with an unwritable index must fail closed" >&2
  exit 1
}
grep -q "failing closed" "$TMP/admit-closed-err" || {
  echo "the fail-closed refusal must be named: $(cat "$TMP/admit-closed-err")" >&2
  exit 1
}
OMS_ARTIFACT_INDEX=/proc/version/cannot/index.jsonl \
  "$ROOT/scripts/patch-admit.sh" --repo "$REPO" --patch "$TMP/admit.patch" \
  --verify true > "$TMP/admit-besteffort" 2>/dev/null || {
  echo "an unlinked admission keeps best-effort indexing (must not fail)" >&2
  exit 1
}
grep -qx ADMIT "$TMP/admit-besteffort" || {
  echo "best-effort admission should still ADMIT" >&2
  exit 1
}

echo 'runtime-core-integration-smoke: admission fail-closed ok'
