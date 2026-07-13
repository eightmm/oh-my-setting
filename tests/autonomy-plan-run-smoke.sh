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

# A changed resolved route is a changed failure hypothesis even when the CLI
# request remains --model-class auto.
"$PLAN" --repo "$repo" add --id mapped --title mapped --allowed mapped.txt --verify true >/dev/null
rc=0
OMS_PLAN_RUN_DELEGATE="$TMP/failing-delegate" HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "mapped fixture failure should exit 1"
OMS_MODEL_CODEX_BALANCED=changed-balanced HOME="$home" PATH="$bin:/usr/bin:/bin" \
  "$RUN" --repo "$repo" --to codex --id mapped --dry-run >"$TMP/mapped.out" 2>"$TMP/mapped.err" ||
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

echo "autonomy-plan-run-smoke: ok"
