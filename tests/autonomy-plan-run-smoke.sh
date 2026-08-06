#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
prompt="$(cat)"
[ -z "${CALL_LOG:-}" ] || printf 'call\n' >> "$CALL_LOG"
case "${OMS_TASK_ID:-}:$prompt" in
  t2:*) printf 'two\n' > delegated2.txt ;;
  executor:*) printf 'executor\n' > executor.txt ;;
  g1:*) printf 'goal-one\n' > goal1.txt ;;
  g2:*) printf 'goal-two\n' > goal2.txt ;;
  *) printf 'one\n' > delegated.txt ;;
esac
echo worker-ok
EOF
chmod +x "$bin/codex"

PLAN="$ROOT/scripts/agent-plan.sh"
RUN="$ROOT/scripts/plan-run.sh"
"$PLAN" --repo "$repo" init --goal bounded >/dev/null
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

# Land the reviewed first task through the public boundary to unlock t2, then
# prove plan-run --land itself uses patch-land and finishes the second task.
"$ROOT/scripts/patch-land.sh" --repo "$repo" --plan-task t1 --verify 'bash scripts/check.sh t1' >/dev/null
grep -Fxq one "$repo/delegated.txt" || fail "reviewed patch did not land"
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --next --land >"$TMP/land.out"
grep -Fq 'state=done' "$TMP/land.out" || fail "land result missing"
"$PLAN" --repo "$repo" show --id t2 | grep -Fq '"state": "done"' || fail "t2 not done"
grep -Fxq two "$repo/delegated2.txt" || fail "plan-run --land did not apply patch"
grep -Fq '"kind": "patch-land"' "$repo/.oms/artifacts/index.jsonl" || fail "landing lineage missing"

# A plan-bound executor freezes an existing claim lease. plan-run must accept
# that exact claimed task instead of requiring a new, incompatible lease.
"$PLAN" --repo "$repo" add --id executor --title executor \
  --allowed executor.txt --verify 'bash scripts/check.sh executor' >/dev/null
"$PLAN" --repo "$repo" claim --id executor --provider codex >/dev/null
printf '# Specialization\n\nImplement only the claimed executor task.\n' > "$repo/executor-soul.md"
"$ROOT/scripts/agent-executor.sh" create --repo "$repo" --id plan-executor \
  --provider codex --plan-task executor --soul-file "$repo/executor-soul.md" >/dev/null
"$ROOT/scripts/agent-executor.sh" freeze --repo "$repo" --id plan-executor >/dev/null
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex \
  --id executor --executor plan-executor >"$TMP/executor.out"
grep -Fq 'state=review' "$TMP/executor.out" || fail "plan executor result missing"
"$PLAN" --repo "$repo" show --id executor | grep -Fq '"state": "review"' ||
  fail "plan executor did not preserve review"

# A preflight refusal must not release a claim owned by a frozen executor; its
# lease remains the authority for a corrected retry.
"$PLAN" --repo "$repo" add --id executor-preflight --title executor-preflight >/dev/null
"$PLAN" --repo "$repo" claim --id executor-preflight --provider codex >/dev/null
preflight_lease="$($PLAN --repo "$repo" show --id executor-preflight | python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])')"
"$ROOT/scripts/agent-executor.sh" create --repo "$repo" --id preflight-executor \
  --provider codex --plan-task executor-preflight --soul-file "$repo/executor-soul.md" >/dev/null
"$ROOT/scripts/agent-executor.sh" freeze --repo "$repo" --id preflight-executor >/dev/null
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex \
  --id executor-preflight --executor preflight-executor >/dev/null 2>"$TMP/preflight.err" || rc=$?
[ "$rc" = 2 ] || fail "unsafe executor preflight should fail"
"$PLAN" --repo "$repo" show --id executor-preflight | python3 -c \
  'import json,sys;d=json.load(sys.stdin); assert d["state"]=="claimed" and d["lease_id"]==sys.argv[1]' "$preflight_lease" ||
  fail "preflight refusal released the executor-owned claim"
"$ROOT/scripts/agent-executor.sh" validate --repo "$repo" --id preflight-executor >/dev/null ||
  fail "preflight refusal invalidated the frozen executor"

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

# An unchanged failed task contract is not silently repeated.
"$PLAN" --repo "$repo" add --id known --title known --allowed known.txt --verify true >/dev/null
call_log="$TMP/calls"
printf '#!/usr/bin/env bash\nexit 1\n' > "$TMP/failing-delegate"
chmod +x "$TMP/failing-delegate"
rc=0
OMS_PLAN_RUN_DELEGATE="$TMP/failing-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id known >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "fixture plan-run failure should exit 1"
rc=0
HOME="$home" CALL_LOG="$call_log" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex --id known >/dev/null 2>"$TMP/known.err" || rc=$?
[ "$rc" = 2 ] || fail "known unchanged failure should be refused with exit 2, got $rc"
[ ! -e "$call_log" ] || fail "known unchanged failure called provider"
grep -Fq 'known unchanged plan-run failure' "$TMP/known.err" || fail "known failure guidance missing"
"$PLAN" --repo "$repo" show --id known | grep -Fq '"state": "ready"' || fail "known-failure refusal stranded claim"

# A changed resolved route is a changed failure hypothesis; with tiers gone
# the route changes by naming a different model outright.
"$PLAN" --repo "$repo" add --id mapped --title mapped --allowed mapped.txt --verify true >/dev/null
rc=0
OMS_PLAN_RUN_DELEGATE="$TMP/failing-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "mapped fixture failure should exit 1"
HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped --model changed-model --dry-run \
  >"$TMP/mapped.out" 2>"$TMP/mapped.err" ||
  fail "changed resolved route should not match the old known failure"

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
for _ in 1 2 3 4 5; do [ -e "$TMP/child-started" ] && break; sleep 1; done
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
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$RUN" --repo "$repo" --to codex \
  --id t9 --land --auto-repair >"$TMP/t9.out" 2>&1 || rc=$?
[ "$rc" -ne 0 ] || fail "auto-repair on an unlandable tree should exit nonzero"
grep -Fq "one auto-repair round" "$TMP/t9.out" ||
  fail "auto-repair round not attempted: $(tail -5 "$TMP/t9.out")"
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
git -C "$repo" log --format=%s -2 | grep -Fq 'feat: goal two lands' ||
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

# Acceptance failing with no actionable task parks with a recorded reason —
# v1 never invents new tasks on its own.
"$PLAN" --repo "$repo" init --goal "unreachable" --accept 'grep -Fxq nope absent.txt' >/dev/null
rc=0
HOME="$home" PATH="$bin:/usr/bin:/bin" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repo" --to codex >"$TMP/gd-park.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "exhausted plan should park with exit 3, got $rc"
grep -Fq 'reason=tasks-exhausted' "$TMP/gd-park.out" || fail "park reason missing"
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

echo "autonomy-plan-run-smoke: ok"
