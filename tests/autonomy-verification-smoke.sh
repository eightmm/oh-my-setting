#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

task="$ROOT/scripts/agent-task.sh"
plan="$ROOT/scripts/agent-plan.sh"

repo="$TMP/task"
mkdir -p "$repo"
"$task" --repo "$repo" init --goal verify --verify 'printf ran > verify-ran; exit 7' >/dev/null
rc=0
"$task" --repo "$repo" verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 7 ] || fail "failed Verify command exit was not propagated: $rc"
[ -f "$repo/verify-ran" ] || fail "stored Verify command was not executed in repo"
"$task" --repo "$repo" status | grep -Fq 'status: active' ||
  fail "failed verification marked task verified"
grep -Fq 'exit: 7' "$repo/.oms/task/current.md" || fail "failed exit evidence missing"
grep -Fq 'duration_seconds:' "$repo/.oms/task/current.md" || fail "timing evidence missing"

"$task" --repo "$repo" update --verify 'printf passed > verify-passed' >/dev/null
"$task" --repo "$repo" verify >/dev/null
[ -f "$repo/verify-passed" ] || fail "passing Verify command did not run"
"$task" --repo "$repo" status | grep -Fq 'status: verified' ||
  fail "passing verification did not mark task verified"
grep -Fq 'exit: 0' "$repo/.oms/task/current.md" || fail "success exit evidence missing"
"$task" --repo "$repo" update --verify 'exit 6' >/dev/null
rc=0
"$task" --repo "$repo" verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 6 ] || fail "re-verification failure exit was not propagated: $rc"
"$task" --repo "$repo" status | grep -Fq 'status: active' ||
  fail "stale verified status survived a later failed verification"

skip_repo="$TMP/skip"
mkdir -p "$skip_repo"
"$task" --repo "$skip_repo" init --goal skip --verify 'exit 9' >/dev/null
"$task" --repo "$skip_repo" verify --skip-verify-run 'external hardware unavailable' >/dev/null
"$task" --repo "$skip_repo" status | grep -Fq 'status: active' ||
  fail "explicit verification skip falsely marked task verified"
grep -Fq 'result: SKIPPED' "$skip_repo/.oms/task/current.md" || fail "skip evidence missing"
grep -Fq 'reason: external hardware unavailable' "$skip_repo/.oms/task/current.md" ||
  fail "skip reason missing"

plan_repo="$TMP/plan"
mkdir -p "$plan_repo"
git -C "$plan_repo" init -q
artifact="$plan_repo/result.md"
patch="$plan_repo/result.patch"
printf 'review\n' > "$artifact"
printf 'patch\n' > "$patch"
"$plan" --repo "$plan_repo" init --goal contract >/dev/null
"$plan" --repo "$plan_repo" add --id t1 --title task --verify 'true' >/dev/null
"$plan" --repo "$plan_repo" claim --id t1 --provider codex >/dev/null
if "$plan" --repo "$plan_repo" finish --id t1 >/dev/null 2>&1; then
  fail "claimed task reached done without review/landing"
fi
"$plan" --repo "$plan_repo" start --id t1 >/dev/null
if "$plan" --repo "$plan_repo" finish --id t1 >/dev/null 2>&1; then
  fail "running task reached done without review/landing"
fi
"$plan" --repo "$plan_repo" review --id t1 --artifact "$artifact" --patch "$patch" >/dev/null
if "$plan" --repo "$plan_repo" finish --id t1 >/dev/null 2>&1; then
  fail "review task reached done without landing"
fi
review_lease="$(python3 - "$plan_repo/.oms/plan/tasks.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    task = json.load(handle)["tasks"]["t1"]
print(task["review_lease_id"])
PY
)"
patch_sha="$(python3 - "$patch" <<'PY'
import hashlib, sys
with open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
PY
)"
"$plan" --repo "$plan_repo" land --id t1 --lease-id "$review_lease" \
  --expected-review-patch "$patch" \
  --expected-review-patch-sha256 "$patch_sha" \
  --expected-review-verify true \
  --expected-review-executor-id "" \
  --expected-review-executor-soul-sha256 "" \
  --expected-review-lease-id "$review_lease" >/dev/null
if "$plan" --repo "$plan_repo" finish --id t1 --lease-id "$review_lease" \
  >/dev/null 2>&1; then
  fail "landed task finished without an exact landing receipt"
fi
landing_receipt_sha="$("$plan" --repo "$plan_repo" show --id t1 | python3 -c '
import hashlib,json,sys
d=json.load(sys.stdin)
for name in ("state", "updated", "claim_expired", "claim_age_s"):
    d.pop(name, None)
print(hashlib.sha256(json.dumps(
 d,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest())
')"
"$plan" --repo "$plan_repo" finish --id t1 --lease-id "$review_lease" \
  --expected-landing-receipt-sha256 "$landing_receipt_sha" >/dev/null
"$plan" --repo "$plan_repo" show --id t1 | grep -Fq '"state": "done"' ||
  fail "reviewed landing could not finish"

# --- verify certifies only the task generation it started on ---------------
# The command runs long and unlocked; if the packet rotates underneath it,
# recording the outcome on the successor forges a pass the successor never
# earned.
race_repo="$TMP/verify-race"
race_barrier="$TMP/verify-race-barrier"
mkdir -p "$race_repo" "$race_barrier"
"$task" --repo "$race_repo" init --goal race \
  --verify ": > '$race_barrier/started'; while [ ! -e '$race_barrier/release' ]; do sleep 0.1; done; exit 0" >/dev/null
first_id="$(awk '$1=="-" && $2=="task_id:" {print $3; exit}' "$race_repo/.oms/task/current.md")"
[ -n "$first_id" ] || fail "race fixture has no task id"
"$task" --repo "$race_repo" verify > "$TMP/verify-race.out" 2>&1 &
verify_pid=$!
for _ in $(seq 1 100); do [ -e "$race_barrier/started" ] && break; sleep 0.1; done
[ -e "$race_barrier/started" ] || {
  kill "$verify_pid" 2>/dev/null || true
  fail "verify never reached its barrier"
}
"$task" --repo "$race_repo" rotate --goal successor >/dev/null
second_id="$(awk '$1=="-" && $2=="task_id:" {print $3; exit}' "$race_repo/.oms/task/current.md")"
[ -n "$second_id" ] && [ "$second_id" != "$first_id" ] || fail "rotate did not mint a new task"
: > "$race_barrier/release"
race_rc=0
wait "$verify_pid" || race_rc=$?
[ "$race_rc" -ne 0 ] || fail "verify certified a task it did not start on"
grep -Fq 'rotated' "$TMP/verify-race.out" ||
  fail "verify did not name the rotation: $(cat "$TMP/verify-race.out")"
"$task" --repo "$race_repo" status | grep -Fq 'status: active' ||
  fail "the successor inherited a verified status it never earned"
if grep -Fq 'duration_seconds' "$race_repo/.oms/task/current.md"; then
  fail "the successor inherited the predecessor's verification evidence"
fi

echo "autonomy verification smoke: ok"
