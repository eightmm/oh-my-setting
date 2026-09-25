#!/usr/bin/env bash
set -euo pipefail

ROOT="${OMS_TEST_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
if [ "${OMS_SMOKE_RUNNER_ACTIVE:-0}" != 1 ]; then
  OMS_SMOKE_SUITE="$ROOT/tests/autonomy-plan-run-smoke.sh" exec "$ROOT/tests/run-smoke-shard.sh" "$@"
fi
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-plan-run-tests.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

repo="$TMP/repo"
bin="$TMP/bin"
home="$TMP/home"
mkdir -p "$repo" "$bin" "$home"
# Keep provider discovery hermetic even when the invoking shell exports a real
# NVM_DIR; peer-delegate intentionally loads that directory before execution.
export NVM_DIR="$home/.nvm"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name Test
printf 'base\n' > "$repo/README.md"
mkdir -p "$repo/scripts"
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  t1) grep -Fxq one delegated.txt ;;
  t2) grep -Fxq two delegated2.txt ;;
  executor) grep -Fxq executor executor.txt ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$repo/scripts/check.sh"
git -C "$repo" add README.md scripts/check.sh
git -C "$repo" commit -qm base

cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --version)
    printf 'codex 1.0\n'
    exit 0
    ;;
  exec)
    if [ "${2:-}" = "--help" ]; then
      printf 'usage: codex exec\n'
      exit 0
    fi
    ;;
esac
prompt="$(cat)"
[ -z "${ARGV_DUMP:-}" ] || printf '%s\n' "$@" > "$ARGV_DUMP"
[ -z "${CALL_LOG:-}" ] || printf 'call\n' >> "$CALL_LOG"
[ -z "${PROMPT_DUMP:-}" ] || printf '%s\n' "$prompt" > "$PROMPT_DUMP"
[ -z "${STEAL_TASK:-}" ] || [ "${OMS_TASK_ID:-}" != "$STEAL_TASK" ] || {
  "$STEAL_PLAN" --repo "$STEAL_REPO" release --id "$STEAL_TASK" \
    --lease-id "$STEAL_LEASE" >/dev/null
  "$STEAL_PLAN" --repo "$STEAL_REPO" claim --id "$STEAL_TASK" \
    --provider codex >/dev/null
  echo worker-stole-fresh-lease >&2
  exit 9
}
[ -z "${FAIL_TASK:-}" ] || [ "${OMS_TASK_ID:-}" != "$FAIL_TASK" ] || {
  echo worker-failed >&2
  exit 9
}
[ -z "${SLOW_TASK:-}" ] || [ "${OMS_TASK_ID:-}" != "$SLOW_TASK" ] || {
  : > "$SLOW_STARTED"
  trap 'exit 143' TERM
  while :; do sleep 1; done
}
case "${OMS_TASK_ID:-}:$prompt" in
  t2:*) printf 'two\n' > delegated2.txt ;;
  executor:*|executor-recovery:*) printf 'executor\n' > executor.txt ;;
  resume-fail:*|fresh-fail:*) printf 'repair\n' > repair.txt ;;
  repair-signal:*) printf 'signal-repair\n' > signal-repair.txt ;;
  g1:*) printf 'goal-one\n' > goal1.txt ;;
  g2:*) printf 'goal-two\n' > goal2.txt ;;
  *) printf 'one\n' > delegated.txt ;;
esac
echo worker-ok
EOF
chmod +x "$bin/codex"

PLAN="$ROOT/scripts/agent-plan.sh"
RUN="$ROOT/scripts/plan-run.sh"

test_plan_lifecycle() {
# A parent transport is not a worker identity. Preserve reviewed routes in
# atomic claims, dry-run, and direct peer hydration without calling real CLIs.
assignment_repo="$TMP/assignment-repo"
git clone -q "$repo" "$assignment_repo"
"$PLAN" --repo "$assignment_repo" init --goal allocation >/dev/null
"$PLAN" --repo "$assignment_repo" add --id t1 --title assigned \
  --allowed delegated.txt --verify 'bash scripts/check.sh t1' \
  --assignment '{"provider":"codex","workload":"routine"}' >/dev/null
selected="$($PLAN --repo "$assignment_repo" next --claim --provider claude --json)"
printf '%s' "$selected" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["provider"] == "codex" and d["assignment"]["workload"] == "routine" and d["lease_id"]
' || fail "atomic claim ignored reviewed task assignment"
# Legacy plans acquire lineage at the direct delegate entrance; route and
# verification must come from the final snapshot reread after that transition.
python3 - "$assignment_repo/.oms/plan/tasks.json" <<'PY'
import json,sys
path=sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data=json.load(handle)
data.pop("plan_id", None)
with open(path,"w",encoding="utf-8") as handle:
    json.dump(data,handle)
PY
ARGV_DUMP="$TMP/assigned-argv" HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/peer-delegate.sh" \
  --repo "$assignment_repo" --to claude --model wrong-carrier-model --reasoning-effort ultra --plan-task t1 \
  >"$TMP/assigned-peer.out" 2>&1 || fail "direct peer assignment hydration failed"
if grep -Eq 'wrong-carrier-model|ultra' "$TMP/assigned-argv"; then
  fail "default model/effort leaked into the other provider's invocation"
fi
"$PLAN" --repo "$assignment_repo" show --id t1 | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["provider"] == "codex" and d["state"] == "review"
' || fail "assigned worker did not retain its claim provider"
[ ! -e "$assignment_repo/delegated.txt" ] || fail "assigned worker changed the parent tree"
"$PLAN" --repo "$assignment_repo" add --id reverse --title reverse \
  --allowed delegated.txt --verify 'bash scripts/check.sh t1' \
  --assignment '{"provider":"claude","model":"assigned-claude","workload":"routine"}' >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$assignment_repo" \
  --to codex --model wrong-carrier-model --id reverse --dry-run >"$TMP/assigned-plan.out"
grep -Fq 'provider=claude' "$TMP/assigned-plan.out" || fail "plan-run lost assigned provider"
grep -Fq 'model=assigned-claude' "$TMP/assigned-plan.out" || fail "plan-run lost assigned model"
if grep -Fq wrong-carrier-model "$TMP/assigned-plan.out"; then fail "run default leaked into assigned route"; fi
"$PLAN" --repo "$assignment_repo" show --id reverse | grep -Fq '"state": "ready"' ||
  fail "assigned dry-run claimed a task"


"$PLAN" --repo "$repo" init --goal bounded >/dev/null
initial_plan_id="$(python3 - "$repo/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["plan_id"])
PY
)"
"$PLAN" --repo "$repo" add --id t1 --title review \
  --allowed delegated.txt --verify 'bash scripts/check.sh t1' >/dev/null
"$PLAN" --repo "$repo" add --id t2 --title land --depends t1 \
  --allowed delegated2.txt --verify 'bash scripts/check.sh t2' >/dev/null

# Machine-readable next selection must carry the claim lease atomically.
selected="$($PLAN --repo "$repo" next --claim --provider codex --json)"
printf '%s' "$selected" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["id"]=="t1" and d["state"]=="claimed" and d["lease_id"]' ||
  fail "next --json did not return the atomic claim"
"$PLAN" --repo "$repo" release --id t1 >/dev/null

# Default execution leaves a reviewed patch and never mutates the main tree.
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --next >"$TMP/review.out"
grep -Fq 'state=review' "$TMP/review.out" || fail "review result missing"
"$PLAN" --repo "$repo" show --id t1 | grep -Fq '"state": "review"' || fail "t1 not in review"
[ ! -e "$repo/delegated.txt" ] || fail "review-default mutated the main tree"
python3 - "$repo/.oms/artifacts/index.jsonl" "$initial_plan_id" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
row = next(row for row in reversed(rows)
           if row.get("kind") == "delegate" and row.get("task_id") == "t1")
assert row.get("plan_id") == sys.argv[2], row
PY
  fail "plan-bound delegation omitted its immutable plan lineage"

# Continue the reviewed first task through plan-run itself. A review
# continuation must try its stored patch before asking the provider to do the
# same work again; this is the handoff goal-drive uses after durable intent.
continue_calls="$TMP/review-continuation-calls"
if CALL_LOG="$continue_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to claude --id t1 --land >/dev/null 2>&1; then
  fail "review continuation accepted a different repair provider"
fi
[ ! -e "$continue_calls" ] || fail "provider mismatch reached a worker call"
CALL_LOG="$continue_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id t1 --land >"$TMP/continue-review.out"
[ ! -e "$continue_calls" ] || fail "review continuation called the provider before landing"
grep -Fq 'continuing stored review' "$TMP/continue-review.out" ||
  fail "review continuation was not reported"
grep -Fxq one "$repo/delegated.txt" || fail "reviewed patch did not land"
# Landing deliberately leaves reviewable working-tree bytes. Commit the first
# task before asking a second task to land: patch-land now treats untracked
# files as dirty and must never stack a new admission onto them.
git -C "$repo" add delegated.txt
git -C "$repo" commit -qm 'test: commit first reviewed task'
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --next --land >"$TMP/land.out"
grep -Fq 'state=done' "$TMP/land.out" || fail "land result missing"
"$PLAN" --repo "$repo" show --id t2 | grep -Fq '"state": "done"' || fail "t2 not done"
grep -Fxq two "$repo/delegated2.txt" || fail "plan-run --land did not apply patch"
grep -Fq '"kind": "patch-land"' "$repo/.oms/artifacts/index.jsonl" || fail "landing lineage missing"
python3 - "$repo/.oms/artifacts/index.jsonl" "$initial_plan_id" <<'PY' ||
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
for task_id in ("t1", "t2"):
    row = next(row for row in reversed(rows)
               if row.get("kind") == "patch-land" and row.get("task_id") == task_id)
    assert row.get("plan_id") == sys.argv[2], row
row = next(row for row in reversed(rows)
           if row.get("kind") == "delegate" and row.get("task_id") == "t2")
assert row.get("plan_id") == sys.argv[2], row
PY
  fail "plan-bound delegate/landing rows did not share the plan lineage"
git -C "$repo" add delegated2.txt
git -C "$repo" commit -qm 'test: commit second reviewed task'


# A resumed one-shot repair remains terminal when its provider fails. The
# delegated worker must block the exact repair lease before returning failure;
# exposing an intermediate ready row lets another runner claim and call the
# provider again. The plan retains the same one-shot repair contract.
resume_repo="$TMP/resume-failure-repo"
mkdir -p "$resume_repo"
git -C "$resume_repo" init -q
git -C "$resume_repo" config user.email test@example.com
git -C "$resume_repo" config user.name Test
printf 'base\n' > "$resume_repo/README.md"
git -C "$resume_repo" add README.md
git -C "$resume_repo" commit -qm base

# The first (non-resumed) plan-level landing repair has the same one-shot
# authority as a recovered repair. A failed provider must block inside
# peer-delegate before returning; it may never expose ready for the parent to
# patch up afterward.
"$PLAN" --repo "$resume_repo" init --goal resume-failure --accept false >/dev/null
"$PLAN" --repo "$resume_repo" add --id fresh-fail --title fresh-fail \
  --allowed repair.txt --verify 'grep -Fxq repair repair.txt' >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$resume_repo" --to codex \
  --id fresh-fail >/dev/null
fresh_lease="$($PLAN --repo "$resume_repo" show --id fresh-fail |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])')"
printf 'dirt\n' >> "$resume_repo/README.md"
fresh_fail_calls="$TMP/fresh-fail-calls"
rc=0
FAIL_TASK=fresh-fail CALL_LOG="$fresh_fail_calls" HOME="$home" \
  PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$resume_repo" --to codex \
  --id fresh-fail --land --auto-repair >"$TMP/fresh-fail.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "failed first landing repair unexpectedly succeeded"
[ "$(wc -l < "$fresh_fail_calls" | tr -d ' ')" = 1 ] ||
  fail "first one-shot repair made more than one provider call"
grep -Fq 'plan: fresh-fail -> blocked' "$TMP/fresh-fail.out" ||
  fail "first failed repair did not block inside delegated execution"
if grep -Fq 'plan: fresh-fail -> ready' "$TMP/fresh-fail.out"; then
  fail "first failed repair exposed an intermediate ready state"
fi
"$PLAN" --repo "$resume_repo" show --id fresh-fail | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["state"] == "blocked" and d["repair_count"] == 1, d
assert d["lease_id"] == sys.argv[1] and d["review_lease_id"] == sys.argv[1], d
' "$fresh_lease" || fail "first failed repair lost its exact blocked receipt"
git -C "$resume_repo" checkout -q README.md

# TERM after the repair provider starts follows the same terminal contract.
# plan-run kills its child first; peer-delegate must park the exact lease in
# its own signal cleanup before the parent can release anything.
"$PLAN" --repo "$resume_repo" add --id repair-signal --title repair-signal \
  --allowed signal-repair.txt \
  --verify 'grep -Fxq signal-repair signal-repair.txt' >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$resume_repo" --to codex \
  --id repair-signal >/dev/null
signal_repair_lease="$($PLAN --repo "$resume_repo" show --id repair-signal |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])')"
printf 'dirt\n' >> "$resume_repo/README.md"
slow_started="$TMP/repair-signal-started"
SLOW_TASK=repair-signal SLOW_STARTED="$slow_started" HOME="$home" \
  PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$resume_repo" --to codex \
  --id repair-signal --land --auto-repair >"$TMP/repair-signal.out" 2>&1 &
repair_signal_pid="$!"
# Admission rejection and repair setup precede this handshake; five seconds
# is not a reliable startup bound, especially beside the full lint/test gate.
for _ in $(seq 1 300); do
  [ -e "$slow_started" ] && break
  kill -0 "$repair_signal_pid" 2>/dev/null || break
  sleep 0.1
done
if [ ! -e "$slow_started" ]; then
  kill -TERM "$repair_signal_pid" 2>/dev/null || true
  wait "$repair_signal_pid" 2>/dev/null || true
  cat "$TMP/repair-signal.out" >&2
  fail "repair signal fixture never reached its provider"
fi
kill -TERM "$repair_signal_pid"
rc=0
wait "$repair_signal_pid" || rc=$?
[ "$rc" = 143 ] || fail "repair signal exit should be 143, got $rc"
"$PLAN" --repo "$resume_repo" show --id repair-signal | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["state"] == "blocked" and d["repair_count"] == 1, d
assert d["lease_id"] == sys.argv[1] and d["review_lease_id"] == sys.argv[1], d
' "$signal_repair_lease" || fail "signaled repair exposed or lost its exact lease"
git -C "$resume_repo" checkout -q README.md

"$PLAN" --repo "$resume_repo" add --id resume-fail --title resume-fail \
  --allowed repair.txt --verify 'grep -Fxq repair repair.txt' >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$resume_repo" --to codex \
  --id resume-fail >/dev/null
resume_lease="$($PLAN --repo "$resume_repo" show --id resume-fail | python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])')"
printf 'dirt\n' >> "$resume_repo/README.md"
rc=0
OMS_PLAN_RUN_TEST_STOP_AFTER_PLAN_REPAIR=1 HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$resume_repo" --to codex --id resume-fail \
  --land --auto-repair \
  >"$TMP/resume-fail-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "resume failure fixture should stop after durable repair, got $rc"
git -C "$resume_repo" checkout -q README.md
resume_fail_calls="$TMP/resume-fail-calls"
rc=0
FAIL_TASK=resume-fail CALL_LOG="$resume_fail_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$resume_repo" --to codex --id resume-fail \
  --land --auto-repair \
  >"$TMP/resume-fail.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "failed resumed repair unexpectedly succeeded"
[ -s "$resume_fail_calls" ] || fail "failed resumed repair never called its provider"
[ "$(wc -l < "$resume_fail_calls" | tr -d ' ')" = 1 ] ||
  fail "resumed one-shot repair made a second provider call"
grep -Fq 'plan: resume-fail -> blocked' "$TMP/resume-fail.out" ||
  fail "delegated resumed repair did not block its exact lease before returning"
if grep -Fq 'plan: resume-fail -> ready' "$TMP/resume-fail.out"; then
  fail "delegated resumed repair exposed an intermediate ready state"
fi
"$PLAN" --repo "$resume_repo" show --id resume-fail | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["state"] == "blocked" and d["repair_count"] == 1, d
assert d["lease_id"] == sys.argv[1] and d["review_lease_id"] == sys.argv[1], d
' "$resume_lease" || fail "failed resumed repair was exposed as ready work"
resume_calls_before="$(wc -l < "$resume_fail_calls" | tr -d ' ')"
rc=0
FAIL_TASK=resume-fail CALL_LOG="$resume_fail_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$resume_repo" --to codex --id resume-fail \
  --land --auto-repair --retry-known \
  >"$TMP/resume-fail-repeat.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "blocked resumed repair unexpectedly ran again"
[ "$(wc -l < "$resume_fail_calls" | tr -d ' ')" = "$resume_calls_before" ] ||
  fail "a later plan-run called the failed one-shot repair provider again"
rc=0
FAIL_TASK=resume-fail CALL_LOG="$resume_fail_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$ROOT/scripts/goal-drive.sh" --repo "$resume_repo" --to codex --max-cycles 1 \
  >"$TMP/resume-fail-goal.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "goal-drive should park behind the blocked one-shot repair, got $rc"
[ "$(wc -l < "$resume_fail_calls" | tr -d ' ')" = "$resume_calls_before" ] ||
  fail "goal-drive called the failed one-shot repair provider again"

# Blocking the released row is fenced by the original repair lease. If a
# sibling wins a fresh claim first, the stale repair runner must fail closed
# instead of overwriting that new owner.
printf 'prior\n' > "$TMP/resume-race-artifact"
: > "$TMP/resume-race.patch"
"$PLAN" --repo "$resume_repo" add --id resume-race --title resume-race \
  --allowed race.txt --verify true >/dev/null
"$PLAN" --repo "$resume_repo" claim --id resume-race --provider codex >/dev/null
resume_race_lease="$($PLAN --repo "$resume_repo" show --id resume-race |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])')"
"$PLAN" --repo "$resume_repo" start --id resume-race --lease-id "$resume_race_lease" >/dev/null
"$PLAN" --repo "$resume_repo" review --id resume-race --lease-id "$resume_race_lease" \
  --artifact "$TMP/resume-race-artifact" --patch "$TMP/resume-race.patch" >/dev/null
"$PLAN" --repo "$resume_repo" repair --id resume-race --lease-id "$resume_race_lease" >/dev/null
resume_race_calls="$TMP/resume-race-calls"
rc=0
STEAL_TASK=resume-race STEAL_PLAN="$PLAN" STEAL_REPO="$resume_repo" \
  STEAL_LEASE="$resume_race_lease" CALL_LOG="$resume_race_calls" \
  HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$resume_repo" --to codex --id resume-race --land --auto-repair \
  >"$TMP/resume-race.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "stale repair terminalization unexpectedly succeeded"
[ "$(wc -l < "$resume_race_calls" | tr -d ' ')" = 1 ] ||
  fail "fresh-lease race made a second provider call"
"$PLAN" --repo "$resume_repo" show --id resume-race | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["state"] == "claimed" and d["lease_id"] and d["lease_id"] != sys.argv[1], d
' "$resume_race_lease" || fail "stale repair runner overwrote a fresh sibling lease"

# Empty scope and missing verification fail closed and release the claim.
"$PLAN" --repo "$repo" add --id unsafe --title unsafe >/dev/null
if HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --id unsafe >"$TMP/unsafe.out" 2>"$TMP/unsafe.err"; then
  fail "unsafe task should be refused"
fi
grep -Fq 'non-empty allowed_paths' "$TMP/unsafe.err" || fail "unsafe refusal reason missing"
"$PLAN" --repo "$repo" show --id unsafe | grep -Fq '"state": "ready"' || fail "refused claim was stranded"

# Dry-run selects but never claims or calls the provider.
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --id unsafe --dry-run >"$TMP/dry.out" 2>"$TMP/dry.err" || true
"$PLAN" --repo "$repo" show --id unsafe | grep -Fq '"state": "ready"' || fail "dry-run changed plan state"

# A reviewed-patch repair is a one-shot transition under the exact review
# lease. It retains the prior artifact/patch until a replacement review exists,
# rejects a stale lease, and cannot be used for an unbounded third pass.
printf 'prior artifact\n' > "$TMP/prior-artifact"
printf 'prior patch\n' > "$TMP/prior-patch"
"$PLAN" --repo "$repo" add --id repair-fence --title repair-fence \
  --allowed repair.txt --verify true >/dev/null
"$PLAN" --repo "$repo" claim --id repair-fence --provider codex >/dev/null
repair_lease="$($PLAN --repo "$repo" show --id repair-fence |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["lease_id"])')"
"$PLAN" --repo "$repo" start --id repair-fence --lease-id "$repair_lease" >/dev/null
"$PLAN" --repo "$repo" review --id repair-fence --lease-id "$repair_lease" \
  --artifact "$TMP/prior-artifact" --patch "$TMP/prior-patch" >/dev/null
if "$PLAN" --repo "$repo" repair --id repair-fence --lease-id lease_stale >/dev/null 2>&1; then
  fail "stale lease entered reviewed-patch repair"
fi
"$PLAN" --repo "$repo" repair --id repair-fence --lease-id "$repair_lease" >/dev/null
"$PLAN" --repo "$repo" show --id repair-fence |
  python3 -c 'import json,sys
d=json.load(sys.stdin)
assert d["state"] == "claimed" and d["lease_id"] == sys.argv[1]
assert d["artifact"] == sys.argv[2] and d["patch"] == sys.argv[3]
assert d["repair_count"] == 1' \
    "$repair_lease" "$TMP/prior-artifact" "$TMP/prior-patch" ||
  fail "repair transition widened the lease or discarded prior evidence"
"$PLAN" --repo "$repo" start --id repair-fence --lease-id "$repair_lease" >/dev/null
"$PLAN" --repo "$repo" review --id repair-fence --lease-id "$repair_lease" >/dev/null
if "$PLAN" --repo "$repo" repair --id repair-fence --lease-id "$repair_lease" >/dev/null 2>&1; then
  fail "review repair exceeded its one-shot contract"
fi
"$PLAN" --repo "$repo" block --id repair-fence --lease-id "$repair_lease" \
  --reason "repair transition tested" >/dev/null

# An unchanged failed task contract is not silently repeated.
"$PLAN" --repo "$repo" add --id known --title known --allowed known.txt --verify true >/dev/null
call_log="$TMP/calls"
printf '#!/usr/bin/env bash\nexit 1\n' > "$TMP/failing-delegate"
chmod +x "$TMP/failing-delegate"
mkdir -p "$TMP/routing-cap"
printf 'gpt-5.6-sol\ngpt-5.6-terra\ngpt-5.6-luna\n' > "$TMP/routing-cap/codex.models"
rc=0
OMS_CAPABILITY_DIR="$TMP/routing-cap" OMS_MODEL_WORKLOAD=standard OMS_ROLE_ROUTING=1 \
  OMS_PLAN_RUN_DELEGATE="$TMP/failing-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id known >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "fixture plan-run failure should exit 1"
rc=0
OMS_CAPABILITY_DIR="$TMP/routing-cap" OMS_MODEL_WORKLOAD=standard OMS_ROLE_ROUTING=1 \
  HOME="$home" CALL_LOG="$call_log" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --id known >/dev/null 2>"$TMP/known.err" || rc=$?
[ "$rc" = 2 ] || fail "known unchanged failure should be refused with exit 2, got $rc"
[ ! -e "$call_log" ] || fail "known unchanged failure called provider"
grep -Fq 'known unchanged plan-run failure' "$TMP/known.err" || fail "known failure guidance missing"
"$PLAN" --repo "$repo" show --id known | grep -Fq '"state": "ready"' || fail "known-failure refusal stranded claim"

# Ambient workload cannot change a plan's failure identity: peer-delegate
# defaults to standard unless its caller explicitly supplies --workload.
rc=0
OMS_CAPABILITY_DIR="$TMP/routing-cap" OMS_MODEL_WORKLOAD=routine OMS_ROLE_ROUTING=1 \
  HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id known --dry-run \
  >"$TMP/ambient-workload.out" 2>"$TMP/ambient-workload.err" || rc=$?
[ "$rc" = 2 ] || fail "ambient routine workload bypassed the standard-route known failure: exit $rc"
grep -Fq 'known unchanged plan-run failure' "$TMP/ambient-workload.err" ||
  fail "ambient workload changed the standard-route failure fingerprint"
"$PLAN" --repo "$repo" show --id known | grep -Fq '"state": "ready"' ||
  fail "ambient workload dry-run changed plan state"

# A changed resolved route is a changed failure hypothesis; with tiers gone
# the route changes by naming a different model outright.
"$PLAN" --repo "$repo" add --id mapped --title mapped --allowed mapped.txt --verify true >/dev/null
rc=0
OMS_CAPABILITY_DIR="$TMP/routing-cap" OMS_MODEL_WORKLOAD=standard OMS_ROLE_ROUTING=1 \
  OMS_PLAN_RUN_DELEGATE="$TMP/failing-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "mapped fixture failure should exit 1"
OMS_CAPABILITY_DIR="$TMP/routing-cap" OMS_MODEL_WORKLOAD=routine OMS_ROLE_ROUTING=1 \
  HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped --model changed-model --dry-run \
  >"$TMP/mapped.out" 2>"$TMP/mapped.err" ||
  fail "changed resolved route should not match the old known failure"
grep -Fq 'plan-run: assignment model=changed-model ' "$TMP/mapped.out" ||
  fail "assignment should name the resolved explicit model"

# TERM waits for child cleanup before releasing the exact lease.
"$PLAN" --repo "$repo" add --id signal --title signal --allowed signal.txt --verify true >/dev/null
cat > "$TMP/slow-delegate" <<'EOF'
#!/usr/bin/env bash
trap 'sleep 1; : > "$CHILD_CLEANED"; exit 143' TERM
: > "$CHILD_STARTED"
while :; do sleep 1; done
EOF
chmod +x "$TMP/slow-delegate"
CHILD_STARTED="$TMP/child-started" CHILD_CLEANED="$TMP/child-cleaned" \
  OMS_PLAN_RUN_DELEGATE="$TMP/slow-delegate" OMS_PLAN_RUN_KILL_AFTER=3 \
  HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --id signal \
  >"$TMP/signal.out" 2>"$TMP/signal.err" &
runner_pid="$!"
for _ in $(seq 1 50); do [ -e "$TMP/child-started" ] && break; sleep 0.1; done
[ -e "$TMP/child-started" ] || fail "signal fixture child did not start"
kill -TERM "$runner_pid"
rc=0
wait "$runner_pid" || rc=$?
[ "$rc" = 143 ] || fail "signal exit should be 143, got $rc"
[ -e "$TMP/child-cleaned" ] || fail "claim released before child cleanup completed"
"$PLAN" --repo "$repo" show --id signal | grep -Fq '"state": "ready"' || fail "signal cleanup stranded claim"

# --- --auto-repair: one embedded repair round at landing, then park ----------
# Delegation succeeds (worker writes, verify passes in the worktree); landing
# fails on the dirty main tree both times, so exactly one repair round runs
# and the task parks with a recommended next action instead of looping.
"$PLAN" --repo "$repo" add --id t9 --title autorepair \
  --allowed delegated.txt --verify 'bash scripts/check.sh t1' >/dev/null
printf 'dirt\n' >> "$repo/README.md"
auto_repair_calls="$TMP/auto-repair-calls"
rc=0
CALL_LOG="$auto_repair_calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex \
  --id t9 --land --auto-repair >"$TMP/t9.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "auto-repair on an unlandable tree should exit nonzero"
grep -Fq "one auto-repair round" "$TMP/t9.out" ||
  fail "auto-repair round not attempted: $(tail -5 "$TMP/t9.out")"
[ -f "$auto_repair_calls" ] || fail "auto-repair never called the provider"
[ "$(wc -l < "$auto_repair_calls" | tr -d ' ')" = 2 ] ||
  fail "auto-repair did not make exactly two provider calls: $(cat "$TMP/t9.out")"
grep -Fq "parked task=t9" "$TMP/t9.out" ||
  fail "task should park after the bounded repair: $(tail -5 "$TMP/t9.out")"
bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list |
  grep -Fq "next: get an outside read" ||
  fail "park should record a recommended next action"
git -C "$repo" checkout -q README.md

# --- goal-drive: acceptance-first bounded loop over an approved plan ---------
# Two dependent tasks whose landings the driver must commit itself — the g1
# commit is exactly what unblocks the g2 landing on a clean tree — then the
# acceptance command passes and the run reports done. Verify goes through
# check.sh arms (committed first) so the admission gate's verifier-integrity
# rule does not see the created file inside its own verify command.
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  t1) grep -Fxq one delegated.txt ;;
  t2) grep -Fxq two delegated2.txt ;;
  executor) grep -Fxq executor executor.txt ;;
  g1) grep -Fxq goal-one goal1.txt ;;
  g2) grep -Fxq goal-two goal2.txt ;;
  *) exit 2 ;;
esac
EOF
git -C "$repo" add scripts/check.sh
git -C "$repo" commit -qm 'test: goal arms join the check contract'
"$PLAN" --repo "$repo" init --goal "both goal files exist" \
  --accept 'bash scripts/check.sh g1 && bash scripts/check.sh g2' >/dev/null
"$PLAN" --repo "$repo" add --id g1 --title "feat: goal one lands" \
  --allowed goal1.txt --verify 'bash scripts/check.sh g1' >/dev/null
"$PLAN" --repo "$repo" add --id g2 --title "feat: goal two lands" --depends g1 \
  --allowed goal2.txt --verify 'bash scripts/check.sh g2' >/dev/null
head0="$(git -C "$repo" rev-parse HEAD)"
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex --max-cycles 3 >"$TMP/gd.out" 2>&1 ||
  fail "goal-drive should reach acceptance: $(tail -8 "$TMP/gd.out")"
grep -Fq 'goal-drive: done' "$TMP/gd.out" || fail "done line missing"
grep -Fq 'acceptance passed' "$TMP/gd.out" || fail "acceptance pass missing"
[ "$(git -C "$repo" rev-list --count "$head0"..HEAD)" = 2 ] ||
  fail "driver should have committed exactly the two landed tasks"
# Drain git's output: grep -q can turn a match into SIGPIPE under pipefail.
git -C "$repo" log --format=%s -2 | grep -F 'feat: goal two lands' >/dev/null ||
  fail "task title should be the commit subject"
[ -z "$(git -C "$repo" status --porcelain --untracked-files=no)" ] ||
  fail "driver left tracked changes uncommitted"
"$PLAN" --repo "$repo" show --id g2 | grep -Fq '"state": "done"' || fail "g2 not done"
grep -Fq '"kind": "acceptance"' "$repo/.oms/plan/progress.jsonl" ||
  fail "acceptance rows missing from progress.jsonl"
grep -Fq '"reason": "acceptance-pass"' "$repo/.oms/plan/progress.jsonl" ||
  fail "terminal done row missing from progress.jsonl"

# A dirty tree refuses up front: driving over someone's live edits is the
# split-brain incident this driver exists to prevent.
printf 'dirt\n' >> "$repo/README.md"
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex >"$TMP/gd-dirty.out" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "dirty tree should refuse with exit 2, got $rc"
grep -Fq 'tree is dirty' "$TMP/gd-dirty.out" || fail "dirty refusal reason missing"
git -C "$repo" checkout -q README.md

# Acceptance failing with a nonempty but exhausted plan parks with a recorded
# reason — v1 never invents new tasks on its own. An actually empty plan is a
# malformed work contract and is covered by the separate non-vacuity refusal.
"$PLAN" --repo "$repo" init --goal "unreachable" --accept 'grep -Fxq nope absent.txt' >/dev/null
"$PLAN" --repo "$repo" add --id spent --title "test: exhausted work" \
  --allowed README.md --verify true >/dev/null
python3 - "$repo/.oms/plan/tasks.json" <<'PY'
import json, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    data = json.load(handle)
data["tasks"]["spent"]["state"] = "done"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, ensure_ascii=False, indent=2)
PY
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex >"$TMP/gd-park.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "exhausted plan should park with exit 3, got $rc"
grep -Fq 'reason=tasks-exhausted' "$TMP/gd-park.out" || fail "park reason missing"
grep -Fq 'goal-drive: parent-agent next:' "$TMP/gd-park.out" ||
  fail "park recovery should be addressed to the parent agent"
if grep -Fq 'goal-drive: next:' "$TMP/gd-park.out"; then
  fail "park recovery should not look like an end-user instruction"
fi
grep -Fq '"reason": "tasks-exhausted"' "$repo/.oms/plan/progress.jsonl" ||
  fail "terminal park row missing"
bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list | grep -Fq 'parked: tasks-exhausted' ||
  fail "park should leave a fail-ledger trail"

# The park row is fingerprinted on the contract (reason + acceptance hash), not
# on the run: a RUN_ID in the cmd survives the ledger's digit mask and made
# every park a singleton, so the advise-at-repeat threshold could never be
# reached. Lineage stays readable in the summary.
park_row() {
  bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved --json |
    python3 -c 'import json,sys
rows = [r for r in json.load(sys.stdin)["failures"]
         if (r.get("cmd") or "").startswith("goal-drive park ")]
print(json.dumps(rows[-1] if rows else {}))'
}
park_cmd="$(park_row | python3 -c 'import json,sys; print(json.load(sys.stdin).get("cmd",""))')"
case "$park_cmd" in
  "goal-drive park reason=tasks-exhausted accept="*) ;;
  *) fail "park row is not contract-shaped: $park_cmd" ;;
esac
case "$park_cmd" in
  *gd-*) fail "run id must not be part of the park fingerprint: $park_cmd" ;;
esac
park_row | grep -Fq 'parked: tasks-exhausted (gd-' ||
  fail "park summary should carry the run lineage"

# The same goal parking twice is the pattern the advise threshold exists for,
# and the hint used to go to /dev/null with the rest of record's stderr.
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex >"$TMP/gd-park2.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "second exhausted run should park with exit 3, got $rc"
grep -Fq 'oms advise' "$TMP/gd-park2.out" ||
  fail "a repeated park must surface the advise hint: $(cat "$TMP/gd-park2.out")"
[ "$(park_row | python3 -c 'import json,sys; print(json.load(sys.stdin).get("count",0))')" = 2 ] ||
  fail "two parks of the same goal should share one fingerprint"

# Acceptance passing is what closes those rows: without it every park stayed
# OPEN in the resume hook, inbox, and advise prompts until a human swept it.
printf 'nope\n' > "$repo/absent.txt"
git -C "$repo" add absent.txt
git -C "$repo" commit -qm 'test: the acceptance target now exists'
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex >"$TMP/gd-resolve.out" 2>&1 ||
  fail "acceptance should now pass: $(tail -5 "$TMP/gd-resolve.out")"
grep -Fq 'goal-drive: done' "$TMP/gd-resolve.out" || fail "done line missing after fix"
if bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved |
  grep -Fq 'parked: tasks-exhausted'; then
  fail "acceptance pass should resolve this goal's park rows"
fi
bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list |
  grep -Fq 'fixed: acceptance passed in gd-' ||
  fail "park resolution should record how it was fixed"

# An acceptance command that already passes on the base tree cannot tell the
# goal state from the start state. Reporting done there is a run that did
# nothing — the shape that sent a whole autopilot campaign into semantic
# review with an empty diff. Its own repository: this must not depend on the
# plan history above.
vacuous="$TMP/vacuous"
mkdir -p "$vacuous"
git -C "$vacuous" init -q
git -C "$vacuous" config user.email test@example.com
git -C "$vacuous" config user.name Test
printf 'base\n' > "$vacuous/README.md"
git -C "$vacuous" add README.md
git -C "$vacuous" commit -qm base
"$PLAN" --repo "$vacuous" init --goal "already satisfied" --accept 'true' >/dev/null
"$PLAN" --repo "$vacuous" add --id v1 --title "feat: work that never runs" \
  --allowed goal.txt --verify 'test -f goal.txt' >/dev/null
head_vacuous="$(git -C "$vacuous" rev-parse HEAD)"
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$vacuous" --to codex >"$TMP/gd-vacuous.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "a vacuous acceptance should park with exit 3, got $rc"
grep -Fq 'reason=acceptance-vacuous' "$TMP/gd-vacuous.out" ||
  fail "vacuous park reason missing: $(tail -5 "$TMP/gd-vacuous.out")"
if grep -Fq 'goal-drive: done' "$TMP/gd-vacuous.out"; then
  fail "a run that executed no task must not report done"
fi
grep -Fq '"reason": "acceptance-vacuous"' "$vacuous/.oms/plan/progress.jsonl" ||
  fail "vacuous park row missing from progress.jsonl"
# The park judgment carries its canonical code: a vacuous acceptance is a
# broken contract, and the ledger row says so without classifier guesswork.
tail -1 "$vacuous/.oms/failures.jsonl" | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row.get("failure_code") == "contract_invalid", row
assert row.get("recovery") == "repair_contract", row
' || fail "vacuous park must record contract_invalid/repair_contract"
[ "$(git -C "$vacuous" rev-parse HEAD)" = "$head_vacuous" ] ||
  fail "a parked vacuous run must not commit"
"$PLAN" --repo "$vacuous" show --id v1 | grep -Fq '"state": "ready"' ||
  fail "the pending task must stay ready after the park"

# An empty plan is not evidence that work finished. It is an invalid work
# contract and must refuse before a passing acceptance can manufacture a
# completion receipt; completed work is represented by a nonempty all-done
# plan instead.
"$PLAN" --repo "$vacuous" init --goal "already satisfied" --accept 'true' >/dev/null
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$vacuous" --to codex >"$TMP/gd-empty-plan.out" 2>&1 || rc=$?
[ "$rc" = 2 ] ||
  fail "an empty plan should refuse with exit 2, got $rc: $(tail -5 "$TMP/gd-empty-plan.out")"
grep -Fq 'plan has no tasks' "$TMP/gd-empty-plan.out" ||
  fail "empty-plan refusal did not name the missing work contract"
if grep -Fq 'goal-drive: done' "$TMP/gd-empty-plan.out"; then
  fail "an empty plan must not report completion"
fi

# The discriminator is evidence, not task bookkeeping: an acceptance that
# recorded a failure at an ancestor commit and passes now describes work that
# landed, even with tasks still pending. A "no task is done yet" rule would
# park this, which is why it is not the rule.
"$PLAN" --repo "$vacuous" init --goal "the marker exists" \
  --accept 'test -f marker.txt' >/dev/null
"$PLAN" --repo "$vacuous" add --id e1 --title "feat: unrelated leftover" \
  --allowed leftover.txt --verify 'bash tests/run.sh' >/dev/null
"$PLAN" --repo "$vacuous" accept >/dev/null 2>&1 &&
  fail "the acceptance fixture must fail before the marker exists"
grep -Fq '"status": "fail"' "$vacuous/.oms/plan/progress.jsonl" ||
  fail "the failing acceptance receipt was not recorded"
printf 'here\n' > "$vacuous/marker.txt"
git -C "$vacuous" add marker.txt
git -C "$vacuous" commit -qm 'feat: the marker lands'
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$vacuous" --to codex >"$TMP/gd-evidence.out" 2>&1 ||
  fail "a turned-around acceptance should report done: $(tail -5 "$TMP/gd-evidence.out")"
grep -Fq 'goal-drive: done' "$TMP/gd-evidence.out" ||
  fail "prior failing evidence at an ancestor must redeem a cycle-1 pass"

# Admission re-runs the verify command against a worktree whose verification
# surface was restored from base. A verify naming a file only this task creates
# is therefore unadmittable by construction — a verdict that used to cost a
# full worker run. The precondition is checked before the provider is called.
mkdir -p "$vacuous/tests"
printf '#!/usr/bin/env bash\nexit 0\n' > "$vacuous/tests/run.sh"
chmod +x "$vacuous/tests/run.sh"
git -C "$vacuous" add tests/run.sh
git -C "$vacuous" commit -qm 'test: a suite that exists at base'
"$PLAN" --repo "$vacuous" init --goal "verify precondition" --accept 'false' >/dev/null
"$PLAN" --repo "$vacuous" add --id p1 --title "test: brings its own verifier" \
  --allowed tests/ --verify 'bash tests/new-suite.sh' >/dev/null
: > "$TMP/verify-calls"
rc=0
# A real claim, not a dry run: the refusal has to happen on the path that
# would otherwise spend the provider call, and it has to give the claim back.
CALL_LOG="$TMP/verify-calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$ROOT/scripts/plan-run.sh" --repo "$vacuous" --to codex --next \
  >"$TMP/pr-verify.out" 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "a verify naming a file absent at base must refuse"
grep -Fq 'does not exist at the base commit' "$TMP/pr-verify.out" ||
  fail "refusal must name the precondition: $(tail -3 "$TMP/pr-verify.out")"
[ ! -s "$TMP/verify-calls" ] || fail "the provider was called despite the refusal"
"$PLAN" --repo "$vacuous" show --id p1 | grep -Fq '"state": "ready"' ||
  fail "the refused task must be left claimable, not stuck claimed"

# The detector only speaks about high-confidence verifier inputs inside the
# task's own scope. These three shapes are exactly the false positives a
# slash-token rule would refuse, and every one of them must run.
verify_control() {  # ID TITLE ALLOWED VERIFY
  "$PLAN" --repo "$vacuous" init --goal "verify precondition" --accept 'false' >/dev/null
  "$PLAN" --repo "$vacuous" add --id "$1" --title "$2" --allowed "$3" --verify "$4" >/dev/null
  HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/plan-run.sh" \
    --repo "$vacuous" --to codex --next --dry-run >"$TMP/pr-control.out" 2>&1 ||
    fail "verify control '$4' must not be refused: $(tail -3 "$TMP/pr-control.out")"
  grep -Fq 'dry-run' "$TMP/pr-control.out" ||
    fail "verify control '$4' did not reach the dry-run boundary"
}
verify_control p2 "test: uses the committed suite" tests/ 'bash tests/run.sh'
verify_control p3 "test: package-wide runner" . 'go test ./...'
verify_control p4 "test: writes a report" tests/ 'bash tests/run.sh -o tests/report.xml'

# A task id can be reused by a replacement plan while a provider or verifier is
# still running. The delegation receipt belongs to the immutable plan snapshot
# taken before that work; it must not borrow the replacement plan's id merely
# because the task object and textual id still match.
delegate_swap_repo="$TMP/delegate-plan-lineage-swap"
mkdir -p "$delegate_swap_repo"
git -C "$delegate_swap_repo" init -q
git -C "$delegate_swap_repo" config user.email test@example.com
git -C "$delegate_swap_repo" config user.name Test
printf 'base\n' > "$delegate_swap_repo/README.md"
git -C "$delegate_swap_repo" add README.md
git -C "$delegate_swap_repo" commit -qm base
"$PLAN" --repo "$delegate_swap_repo" init --goal lineage >/dev/null
delegate_old_plan_id="$(python3 - "$delegate_swap_repo/.oms/plan/tasks.json" <<'PY' | tr -d '\r'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["plan_id"])
PY
)"
delegate_new_plan_id="plan_cccccccccccccccccccccccccccccccc"
delegate_swap_verify="OMS_SWAP_PLAN_FILE='$delegate_swap_repo/.oms/plan/tasks.json' OMS_SWAP_PLAN_ID='$delegate_new_plan_id' python3 -c 'import json,os,pathlib; p=pathlib.Path(os.environ[\"OMS_SWAP_PLAN_FILE\"]); d=json.loads(p.read_text(encoding=\"utf-8\")); d[\"plan_id\"]=os.environ[\"OMS_SWAP_PLAN_ID\"]; p.write_text(json.dumps(d,ensure_ascii=False,indent=2),encoding=\"utf-8\")'"
"$PLAN" --repo "$delegate_swap_repo" add --id same-task --title lineage \
  --allowed delegated.txt --verify "$delegate_swap_verify" >/dev/null
"$PLAN" --repo "$delegate_swap_repo" claim --id same-task --provider codex >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/peer-delegate.sh" \
  --repo "$delegate_swap_repo" --to codex --plan-task same-task \
  --verify "$delegate_swap_verify" >/dev/null
python3 - "$delegate_swap_repo/.oms/plan/tasks.json" \
  "$delegate_swap_repo/.oms/artifacts/index.jsonl" \
  "$delegate_old_plan_id" "$delegate_new_plan_id" <<'PY' ||
import json, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
rows = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
row = next(row for row in reversed(rows)
           if row.get("kind") == "delegate" and row.get("task_id") == "same-task")
assert plan["plan_id"] == sys.argv[4], plan
assert row.get("plan_id") == sys.argv[3], row
assert row["plan_id"] != plan["plan_id"], (row, plan)
PY
  fail "delegation borrowed a replacement plan lineage after its verifier"

}

test_context_pack() {
# --- --context-pack: typed Project Graph orientation, never a write scope ----
# The pack says where to look; the brief's allowed_paths stay the only place a
# worker may write. It is validated as a typed input before anything is
# claimed, so a malformed or secret-shaped pack costs no lease and no provider
# call, and nothing about it is stored in plan state.
pack_repo="$TMP/context-pack"
packs="$TMP/packs"
mkdir -p "$pack_repo" "$packs"
git -C "$pack_repo" init -q
git -C "$pack_repo" config user.email test@example.com
git -C "$pack_repo" config user.name Test
printf 'base\n' > "$pack_repo/README.md"
mkdir -p "$pack_repo/scripts" "$pack_repo/tests"
cp "$repo/scripts/check.sh" "$pack_repo/scripts/check.sh"
printf 'ORIENTATION-FILE-BODY\n' > "$pack_repo/tests/run.sh"
git -C "$pack_repo" add README.md scripts/check.sh tests/run.sh
git -C "$pack_repo" commit -qm base
"$PLAN" --repo "$pack_repo" init --goal orientation >/dev/null
"$PLAN" --repo "$pack_repo" add --id cp-ok --title orient \
  --allowed delegated.txt --verify 'bash scripts/check.sh t1' >/dev/null
"$PLAN" --repo "$pack_repo" add --id cp-refuse --title refuse \
  --allowed other.txt --verify 'bash scripts/check.sh t1' >/dev/null

pack_digest='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
cat > "$packs/valid.json" <<EOF
{"task": "carry orientation to the worker",
 "pack_digest": "$pack_digest",
 "project_graph_revision": "rev-abc",
 "files": ["scripts/check.sh", "tests/run.sh"],
 "tests": ["tests/run.sh"],
 "test_cases": [{"id": "symbol:tests/run.sh::test_selected", "language": "shell",
                  "name": "test_selected", "path": "tests/run.sh"}],
 "evidence": [{"path": "scripts/check.sh", "reason": "query:file", "score": 1001}],
 "hubs": [{"id": "file:scripts/check.sh", "degree": 3}]}
EOF
ln -s "$packs/valid.json" "$packs/symlink.json"
cat > "$packs/absolute.json" <<EOF
{"pack_digest": "$pack_digest", "files": ["/etc/passwd"], "tests": []}
EOF
cat > "$packs/traversal.json" <<EOF
{"pack_digest": "$pack_digest", "files": ["scripts/../../outside.py"], "tests": []}
EOF
# A field name, not key material: SECRET_VALUE_RE keys on the shape, and the
# brief's AKIA fixture matches no rule in it at all.
sentinel_head="api_"; sentinel_tail="key: fixture"
cat > "$packs/secret.json" <<EOF
{"pack_digest": "$pack_digest", "task": "${sentinel_head}${sentinel_tail}",
 "files": ["scripts/check.sh"], "tests": []}
EOF
python3 - "$packs/oversized.json" "$pack_digest" <<'PY'
import json, sys
json.dump({"pack_digest": sys.argv[2], "task": "x" * 300000,
           "files": ["scripts/check.sh"], "tests": []},
          open(sys.argv[1], "w", encoding="utf-8"))
PY

# 1. The orientation section reaches the worker prompt, and the task's write
#    scope and plan row are exactly what they were before the pack existed.
: > "$TMP/cp-prompt.txt"
PROMPT_DUMP="$TMP/cp-prompt.txt" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$pack_repo" --to codex --id cp-ok \
  --context-pack "$packs/valid.json" >"$TMP/cp-ok.out" 2>&1 ||
  fail "context-pack run failed: $(tail -5 "$TMP/cp-ok.out")"
grep -Fq '## Project Graph orientation' "$TMP/cp-prompt.txt" ||
  fail "the worker prompt has no orientation section"
grep -Fq 'project_graph_revision: rev-abc' "$TMP/cp-prompt.txt" ||
  fail "the orientation section dropped the graph revision"
grep -Eq '^context_pack_sha256: [0-9a-f]{64}$' "$TMP/cp-prompt.txt" ||
  fail "the orientation section dropped the pack digest"
grep -Fq -- '- scripts/check.sh  (query:file)' "$TMP/cp-prompt.txt" ||
  fail "the orientation section dropped a pack file and its evidence reason"
grep -Fq -- '- tests/run.sh' "$TMP/cp-prompt.txt" ||
  fail "the orientation section dropped the pack tests"
grep -Fq -- '- test_selected  (shell, tests/run.sh)' "$TMP/cp-prompt.txt" ||
  fail "the orientation section dropped the graph-selected test case"
grep -Fq 'the only write scope' "$TMP/cp-prompt.txt" ||
  fail "the orientation section must say it does not widen the write scope"
grep -Fq 'ORIENTATION-FILE-BODY' "$TMP/cp-prompt.txt" &&
  fail "the orientation section inlined file contents"
"$PLAN" --repo "$pack_repo" show --id cp-ok |
  python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["allowed_paths"] == ["delegated.txt"], d' ||
  fail "the pack widened the task allowed_paths"
python3 - "$pack_repo/.oms/plan/tasks.json" "$pack_digest" <<'PY' ||
import json, sys
raw = open(sys.argv[1], encoding="utf-8").read()
plan = json.loads(raw)
task = plan["tasks"]["cp-ok"]
assert task["allowed_paths"] == ["delegated.txt"], task
assert "context_pack" not in raw, "pack provenance leaked into plan state"
assert sys.argv[2] not in raw, "pack digest leaked into plan state"
PY
  fail "the orientation pack was stored in plan state"
cp_artifact="$(awk -F': ' '$1 == "artifact" {v=$2} END {print v}' "$TMP/cp-ok.out")"
[ -n "$cp_artifact" ] && [ -f "$cp_artifact" ] ||
  fail "the context-pack delegation left no artifact"
grep -Eq '^- context_pack_sha256: [0-9a-f]{64}$' "$cp_artifact" ||
  fail "the delegation artifact records no pack provenance"
grep -Fq -- '- context_file_count: 2' "$cp_artifact" ||
  fail "the delegation artifact records no pack file count"

# 2. Every typed violation refuses before the claim, without a provider call,
#    and leaves the task claimable.
refuse_pack() {  # LABEL PACK
  local rc=0
  : > "$TMP/cp-calls"
  CALL_LOG="$TMP/cp-calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
    "$RUN" --repo "$pack_repo" --to codex --id cp-refuse --context-pack "$2" \
    >"$TMP/cp-refuse.out" 2>&1 || rc=$?
  [ "$rc" != 0 ] || fail "$1 pack was accepted"
  # The exact prefix, not the flag name: "unknown argument: --context-pack"
  # would satisfy a looser match on a build where nothing was implemented.
  grep -Fq 'error: context-pack: ' "$TMP/cp-refuse.out" ||
    fail "$1 refusal did not name context-pack: $(tail -3 "$TMP/cp-refuse.out")"
  [ ! -s "$TMP/cp-calls" ] || fail "$1 pack reached the provider"
  "$PLAN" --repo "$pack_repo" show --id cp-refuse | grep -Fq '"state": "ready"' ||
    fail "$1 refusal did not leave the task claimable"
}
refuse_pack symlink "$packs/symlink.json"
refuse_pack absolute "$packs/absolute.json"
refuse_pack traversal "$packs/traversal.json"
refuse_pack secret "$packs/secret.json"
refuse_pack oversized "$packs/oversized.json"
refuse_pack missing "$packs/absent.json"

# 3. The dry run names the pack it would forward.
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$pack_repo" --to codex \
  --id cp-refuse --context-pack "$packs/valid.json" --dry-run \
  >"$TMP/cp-dry.out" 2>&1 || fail "dry run with a valid pack must not refuse"
grep -Eq '^plan-run: context-pack=.*/valid\.json files=2 sha256=[0-9a-f]{64}$' \
  "$TMP/cp-dry.out" ||
  fail "the dry run did not print the context-pack line: $(tail -3 "$TMP/cp-dry.out")"

# 4. peer-delegate is a public front door: it renders and revalidates the pack
#    on its own, without plan-run in front of it; explicit packs take precedence.
: > "$TMP/cp-direct-prompt.txt"
PROMPT_DUMP="$TMP/cp-direct-prompt.txt" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$pack_repo" --to codex --no-verify \
  --prompt 'direct orientation check' --graph-context --context-pack "$packs/valid.json" \
  >"$TMP/cp-direct.out" 2>&1 ||
  fail "direct peer-delegate --context-pack failed: $(tail -5 "$TMP/cp-direct.out")"
grep -Fq '## Project Graph orientation' "$TMP/cp-direct-prompt.txt" ||
  fail "direct peer-delegate rendered no orientation section"
grep -Fq -- '- scripts/check.sh  (query:file)' "$TMP/cp-direct-prompt.txt" ||
  fail "direct peer-delegate dropped the pack files"
if grep -Fq 'Graph source: detached worker snapshot' "$TMP/cp-direct-prompt.txt"; then
  fail "graph preparation replaced an explicit context pack"
fi
cp_direct_rc=0
: > "$TMP/cp-calls"
CALL_LOG="$TMP/cp-calls" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$pack_repo" --to codex --no-verify \
  --prompt 'direct orientation check' --context-pack "$packs/traversal.json" \
  >"$TMP/cp-direct-bad.out" 2>&1 || cp_direct_rc=$?
[ "$cp_direct_rc" != 0 ] || fail "direct peer-delegate accepted a traversing pack"
grep -Fq 'error: context-pack: ' "$TMP/cp-direct-bad.out" ||
  fail "direct refusal did not name context-pack: $(tail -3 "$TMP/cp-direct-bad.out")"
[ ! -s "$TMP/cp-calls" ] || fail "a traversing pack reached the provider"

# 5. With --context-manifest, every pack file becomes a direct target of the
#    bounded bundle: orientation says where to look, the manifest delivers the
#    bytes, and each target is required rather than best-effort.
#    (The fixture check script reads as sensitive to the bundle scrubber and
#    is omitted with debt by design, so this pack names two clean files.)
cat > "$packs/manifest.json" <<EOF
{"task": "orientation plus bounded bytes", "pack_digest": "$pack_digest",
 "files": ["README.md", "tests/run.sh"], "tests": ["tests/run.sh"]}
EOF
: > "$TMP/cp-manifest-prompt.txt"
PROMPT_DUMP="$TMP/cp-manifest-prompt.txt" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$pack_repo" --to codex --no-verify \
  --prompt 'orientation plus bounded bytes' --context-pack "$packs/manifest.json" \
  --context-manifest >"$TMP/cp-manifest.out" 2>&1 ||
  fail "peer-delegate --context-pack --context-manifest failed: $(tail -5 "$TMP/cp-manifest.out")"
cp_manifest="$(ls -t "$pack_repo"/.oms/runtime/context/delegate-*.json 2>/dev/null | head -n 1)"
[ -n "$cp_manifest" ] || fail "no context manifest was compiled beside the pack"
python3 - "$cp_manifest" <<'PY' || fail "the manifest did not carry every pack file as a required direct target"
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
targets = manifest.get("targets", [])
# A default layer may already carry a target under a stronger reason; what
# matters is that every target is required and present in the bundle.
selected = {item["path"] for item in manifest.get("selected", [])}
assert targets == ["README.md", "tests/run.sh"], targets
assert {"README.md", "tests/run.sh"} <= selected, selected
assert manifest.get("sufficient") is True, manifest.get("missing_required")
assert not manifest.get("missing_required"), manifest.get("missing_required")
from pathlib import Path
index = Path(sys.argv[1]).parents[2] / 'artifacts/index.jsonl'
rows = [json.loads(line) for line in index.read_text(encoding='utf-8').splitlines()]
row = next(row for row in reversed(rows) if row.get('kind') == 'delegate')
assert row['context_mode'] == 'pack+bundle', row
assert row['context_orientation_bytes'] > 0 and row['context_prepare_seconds'] >= 0, row
assert row['context_bundle_sha256'] and row['context_selected_bytes'] > 0, row
PY
grep -Fq 'ORIENTATION-FILE-BODY' "$TMP/cp-manifest-prompt.txt" ||
  fail "the compiled bundle did not deliver the pack file's bytes to the worker"

}

# SMOKE_TEST_CALLS_BEGIN
test_plan_lifecycle
test_context_pack
# SMOKE_TEST_CALLS_END
echo "autonomy-plan-run-smoke: ok"
