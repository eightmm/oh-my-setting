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

# A plan instance has an immutable additive lineage. New plans mint it, every
# ordinary mutation preserves it, and read-only commands leave legacy bytes
# untouched. The parent-only ensure-lineage command upgrades a legacy plan once
# before a plan-scoped evidence producer takes its snapshot.
LINEAGE_REPO="$TMP/lineage-repo"
mkdir -p "$LINEAGE_REPO"
git -C "$LINEAGE_REPO" init -q
git -C "$LINEAGE_REPO" config user.email test@example.com
git -C "$LINEAGE_REPO" config user.name 'OMS Runtime Test'
printf '# Lineage fixture\n' > "$LINEAGE_REPO/PROJECT.md"
git -C "$LINEAGE_REPO" add PROJECT.md
git -C "$LINEAGE_REPO" commit -qm fixture
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" init \
  --goal 'lineage fixture' --accept true >/dev/null
first_plan_id="$(python3 - "$LINEAGE_REPO/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8")).get("plan_id", ""))
PY
)"
printf '%s\n' "$first_plan_id" | grep -Eq '^plan_[0-9a-f]{32}$' || {
  echo "agent-plan init did not mint a plan lineage: $first_plan_id" >&2
  exit 1
}
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" init \
  --goal 'lineage fixture' --accept true >/dev/null
replacement_plan_id="$(python3 - "$LINEAGE_REPO/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["plan_id"])
PY
)"
[ "$replacement_plan_id" != "$first_plan_id" ] || {
  echo 'replacing a plan reused the previous lineage' >&2
  exit 1
}
first_plan_id="$replacement_plan_id"
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" add --id t1 \
  --title 'lineage task' --allowed PROJECT.md --verify true >/dev/null
python3 - "$LINEAGE_REPO/.oms/plan/tasks.json" "$first_plan_id" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 3
assert row["plan_id"] == sys.argv[2]
PY

python3 - "$LINEAGE_REPO/.oms/plan/tasks.json" <<'PY'
import json, sys
path = sys.argv[1]
row = json.load(open(path, encoding="utf-8"))
row.pop("plan_id", None)
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle, ensure_ascii=False, indent=2)
PY
legacy_before="$(git hash-object "$LINEAGE_REPO/.oms/plan/tasks.json")"
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" show --id t1 >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" status >/dev/null
legacy_after="$(git hash-object "$LINEAGE_REPO/.oms/plan/tasks.json")"
[ "$legacy_before" = "$legacy_after" ] || {
  echo 'read-only plan commands rewrote a legacy plan' >&2
  exit 1
}
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" ensure-lineage >/dev/null
ensured_once="$(git hash-object "$LINEAGE_REPO/.oms/plan/tasks.json")"
"$ROOT/scripts/agent-plan.sh" --repo "$LINEAGE_REPO" ensure-lineage >/dev/null
ensured_twice="$(git hash-object "$LINEAGE_REPO/.oms/plan/tasks.json")"
[ "$ensured_once" = "$ensured_twice" ] || {
  echo 'ensure-lineage was not idempotent' >&2
  exit 1
}
python3 - "$LINEAGE_REPO/.oms/plan/tasks.json" <<'PY'
import json, re, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert re.fullmatch(r"plan_[0-9a-f]{32}", row.get("plan_id", "")), row
PY

# Initial reviewed-proposal apply also mints a lineage without changing the
# schema-3 plan contract.
PROPOSAL_REPO="$TMP/proposal-repo"
mkdir -p "$PROPOSAL_REPO"
git -C "$PROPOSAL_REPO" init -q
git -C "$PROPOSAL_REPO" config user.email test@example.com
git -C "$PROPOSAL_REPO" config user.name 'OMS Runtime Test'
printf '%s\n' '## Status' '' '- State: confirmed' '' '## Project' '' \
  '- Goal: proposal lineage' > "$PROPOSAL_REPO/PROJECT.md"
git -C "$PROPOSAL_REPO" add PROJECT.md
git -C "$PROPOSAL_REPO" commit -qm fixture
proposal_head="$(git -C "$PROPOSAL_REPO" rev-parse HEAD)"
proposal_spec="$(python3 - "$PROPOSAL_REPO/PROJECT.md" <<'PY' | tr -d '\r'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
python3 - "$TMP/proposal.json" "$proposal_head" "$proposal_spec" <<'PY'
import json, sys
path, head, spec = sys.argv[1:]
row = {
    "schema": 1, "kind": "agent-plan-proposal", "spec_sha256": spec,
    "plan_sha256": "absent", "base_sha": head, "id_prefix": "",
    "allowed_envelope": ["PROJECT.md"], "acceptance_files": ["PROJECT.md"],
    "tasks": [{"id": "t1", "title": "proposal task", "allowed": ["PROJECT.md"],
               "verify": "true", "depends": []}],
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle, sort_keys=True, separators=(",", ":"))
PY
proposal_sha="$(python3 - "$TMP/proposal.json" <<'PY' | tr -d '\r'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
"$ROOT/scripts/agent-plan.sh" --repo "$PROPOSAL_REPO" apply-proposal \
  --proposal "$TMP/proposal.json" --expected-proposal-sha256 "$proposal_sha" \
  --expected-plan-sha256 absent --goal 'proposal lineage' --accept true \
  --allowed-envelope PROJECT.md --accept-files PROJECT.md >/dev/null
proposal_plan_before="$(git hash-object "$PROPOSAL_REPO/.oms/plan/tasks.json")"
"$ROOT/scripts/agent-plan.sh" --repo "$PROPOSAL_REPO" apply-proposal \
  --proposal "$TMP/proposal.json" --expected-proposal-sha256 "$proposal_sha" \
  --expected-plan-sha256 absent --goal 'proposal lineage' --accept true \
  --allowed-envelope PROJECT.md --accept-files PROJECT.md >/dev/null
proposal_plan_after="$(git hash-object "$PROPOSAL_REPO/.oms/plan/tasks.json")"
[ "$proposal_plan_before" = "$proposal_plan_after" ] || {
  echo 'reviewed proposal replay changed plan lineage or bytes' >&2
  exit 1
}
python3 - "$PROPOSAL_REPO/.oms/plan/tasks.json" <<'PY'
import json, re, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 3
assert re.fullmatch(r"plan_[0-9a-f]{32}", row.get("plan_id", "")), row
PY

echo 'runtime-core-integration-smoke: plan lineage lifecycle ok'

# Freeze the plan id with the task contract before a long verifier. Replacing
# the plan while verification runs must leave the receipt on the old lineage;
# the same task id in the new plan stays missing until a new admission runs.
SWAP_REPO="$TMP/swap-repo"
mkdir -p "$SWAP_REPO"
git -C "$SWAP_REPO" init -q
git -C "$SWAP_REPO" config user.email test@example.com
git -C "$SWAP_REPO" config user.name 'OMS Runtime Test'
printf '%s\n' '# Swap fixture' '' '## Acceptance Criteria' '' \
  '- [id:swap-project] Project evidence remains stable.' > "$SWAP_REPO/PROJECT.md"
git -C "$SWAP_REPO" add PROJECT.md
git -C "$SWAP_REPO" commit -qm fixture
"$ROOT/scripts/agent-plan.sh" --repo "$SWAP_REPO" init \
  --goal 'swap lineage' --accept true >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$SWAP_REPO" add --id t1 \
  --title 'swap task' --allowed PROJECT.md --verify true >/dev/null
swap_first="$(python3 - "$SWAP_REPO/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["plan_id"])
PY
)"
python3 - "$SWAP_REPO/.oms/plan/tasks.json" "$TMP/swap-plan-b.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["plan_id"] = "plan_" + "b" * 32
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump(row, handle, ensure_ascii=False, indent=2)
PY
printf '\nswap patch\n' >> "$SWAP_REPO/PROJECT.md"
capture_patch_swap() {
  git -C "$SWAP_REPO" add PROJECT.md
  git -C "$SWAP_REPO" commit -qm swap-patch
  git -C "$SWAP_REPO" diff HEAD~1 HEAD > "$TMP/swap.patch"
  git -C "$SWAP_REPO" reset -q --hard HEAD~1
}
capture_patch_swap
"$ROOT/scripts/patch-admit.sh" --repo "$SWAP_REPO" --patch "$TMP/swap.patch" \
  --plan-task t1 --covers plan-task-t1 \
  --verify "cp '$TMP/swap-plan-b.json' '$SWAP_REPO/.oms/plan/tasks.json'" \
  >/dev/null
python3 - "$SWAP_REPO/.oms/artifacts/index.jsonl" "$swap_first" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
row = [item for item in rows if item.get("kind") == "patch-admit"][-1]
assert row.get("plan_id") == sys.argv[2], row
PY
"$ROOT/scripts/runtime.sh" --repo "$SWAP_REPO" envelope show > "$TMP/swap-before.json"
python3 - "$TMP/swap-before.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
statuses = {item["id"]: item["status"] for item in row["criteria"]}
assert statuses["plan-task-t1"] == "missing", statuses
PY
"$ROOT/scripts/patch-admit.sh" --repo "$SWAP_REPO" --patch "$TMP/swap.patch" \
  --plan-task t1 --covers plan-task-t1 --verify true >/dev/null
"$ROOT/scripts/runtime.sh" --repo "$SWAP_REPO" envelope show > "$TMP/swap-after.json"
python3 - "$TMP/swap-after.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
statuses = {item["id"]: item["status"] for item in row["criteria"]}
assert statuses["plan-task-t1"] == "verified", statuses
PY

echo 'runtime-core-integration-smoke: verifier plan swap lineage ok'
