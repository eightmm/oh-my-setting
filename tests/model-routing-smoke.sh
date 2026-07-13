#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-routing.XXXXXX")"
trap '[ "${KEEP_TMP:-0}" = 1 ] || rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

repo="$TMP/repo"
bin="$TMP/bin"
home="$TMP/home"
capture="$TMP/capture"
mkdir -p "$repo" "$bin" "$home" "$capture"
export HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin"
export CAPTURE_DIR="$capture"

git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name Test
printf 'base\n' > "$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" commit -qm base

cat > "$bin/provider-fake" <<'EOF'
#!/usr/bin/env bash
set -u
provider="$(basename "$0")"
count_file="$CAPTURE_DIR/$provider.count"
count=0
[ ! -f "$count_file" ] || count="$(cat "$count_file")"
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
argv_file="$CAPTURE_DIR/$provider.$count.argv"
: > "$argv_file"
model=""
want_model=0
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$argv_file"
  if [ "$want_model" = 1 ]; then model="$arg"; want_model=0; continue; fi
  case "$arg" in -m|--model) want_model=1 ;; esac
done
cat >/dev/null
printf '%s\t%s\t%s\t%s\n' "$provider" "$count" "$model" "$PWD" >> "$CAPTURE_DIR/calls"
if [ "${FAIL_MODE:-}" = repair-stage-dirty ] && [ "$count" -eq 2 ]; then
  printf 'mutated\n' > routed.txt
  git add routed.txt
  echo 'Selected model is at capacity. Please try a different model.' >&2
  exit 1
fi
if [ "${FAIL_MODE:-}" = repair-capacity ] && [ "$count" -eq 2 ]; then
  echo 'Selected model is at capacity. Please try a different model.' >&2
  exit 1
fi
case "${FAIL_MODE:-}:$model" in
  capacity-all:*)
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  capacity:"${PRIMARY_MODEL:-__none__}")
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  dirty-capacity:"${PRIMARY_MODEL:-__none__}")
    printf 'partial\n' > routed.txt
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  generic:"${PRIMARY_MODEL:-__none__}")
    echo 'authentication failed' >&2
    exit 1
    ;;
esac
[ -z "${WRITE_FILE:-}" ] || printf '%s\n' "$model" > "$WRITE_FILE"
echo worker-ok
EOF
chmod +x "$bin/provider-fake"
ln -s provider-fake "$bin/codex"
ln -s provider-fake "$bin/claude"
ln -s provider-fake "$bin/agy"

export OMS_MODEL_CODEX_FAST=codex-fast-x
export OMS_MODEL_CODEX_BALANCED=codex-balanced-x
export OMS_MODEL_CODEX_DEEP=codex-deep-x
export OMS_MODEL_CLAUDE_FAST=claude-fast-x
export OMS_MODEL_CLAUDE_BALANCED=claude-balanced-x
export OMS_MODEL_CLAUDE_DEEP=claude-deep-x
export OMS_MODEL_ANTIGRAVITY_FAST=agy-fast-x
export OMS_MODEL_ANTIGRAVITY_BALANCED=agy-balanced-x
export OMS_MODEL_ANTIGRAVITY_DEEP=agy-deep-x

reset_capture() {
  rm -f "$capture"/*
  unset FAIL_MODE PRIMARY_MODEL WRITE_FILE
}

model_arg() {
  awk 'p{print;exit} $0=="-m" || $0=="--model"{p=1}' "$1"
}

CALL="$ROOT/scripts/agent-call.sh"
DELEGATE="$ROOT/scripts/peer-delegate.sh"
RUN="$ROOT/scripts/agent-run.sh"
EXECUTOR="$ROOT/scripts/agent-executor.sh"

# Explicit class maps to the provider model and records bounded provenance.
reset_capture
"$CALL" --repo "$repo" --to codex --model-class fast --prompt 'inspect routing' >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-fast-x ] || fail "Codex fast mapping missing"
grep -Fq '"model_class": "fast"' "$repo/.oms/artifacts/index.jsonl" || fail "model class not indexed"
grep -Fq '"selected_model": "codex-fast-x"' "$repo/.oms/artifacts/index.jsonl" || fail "selected model not indexed"

# Every provider receives its own mapped model flag.
reset_capture
"$CALL" --repo "$repo" --to claude --model-class balanced --prompt 'inspect routing' >/dev/null
[ "$(model_arg "$capture/claude.1.argv")" = claude-balanced-x ] || fail "Claude balanced mapping missing"
reset_capture
"$CALL" --repo "$repo" --to antigravity --model-class fast --prompt 'inspect routing' >/dev/null
[ "$(model_arg "$capture/agy.1.argv")" = agy-fast-x ] || fail "Antigravity fast mapping missing"

# Invalid environment mappings fail before constructing a provider argv.
reset_capture
export OMS_MODEL_CODEX_FAST=$'invalid\tmodel'
rc=0
"$CALL" --repo "$repo" --to codex --model-class fast --prompt 'bad mapping' >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid mapped model should fail validation"
[ ! -f "$capture/codex.count" ] || fail "invalid mapping must not call provider"
export OMS_MODEL_CODEX_FAST=codex-fast-x

# The unified entrypoint forwards the routing contract to its selected path.
reset_capture
"$RUN" --repo "$repo" --to codex --mode read --model-class fast \
  --prompt 'inspect entrypoint routing' >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-fast-x ] || fail "agent-run did not forward model class"

# Capacity alone retries once on the next lower class and records the fallback.
reset_capture
export FAIL_MODE=capacity PRIMARY_MODEL=codex-deep-x
"$CALL" --repo "$repo" --to codex --model-class deep --prompt 'capacity fallback' >/dev/null
[ "$(cat "$capture/codex.count")" = 2 ] || fail "capacity fallback must make two attempts"
[ "$(model_arg "$capture/codex.1.argv")" = codex-deep-x ] || fail "deep primary missing"
[ "$(model_arg "$capture/codex.2.argv")" = codex-balanced-x ] || fail "deep fallback should use balanced"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"fallback_used": true' || fail "fallback use not indexed"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"fallback_reason": "capacity"' || fail "fallback reason not indexed"

# Generic failures and exact-model overrides never trigger an implicit fallback.
reset_capture
export FAIL_MODE=generic PRIMARY_MODEL=codex-deep-x
rc=0
"$CALL" --repo "$repo" --to codex --model-class deep --prompt 'generic failure' >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "generic provider failure should propagate"
[ "$(cat "$capture/codex.count")" = 1 ] || fail "generic failure must not fallback"
reset_capture
export FAIL_MODE=capacity PRIMARY_MODEL=exact-model-x
rc=0
"$CALL" --repo "$repo" --to codex --model exact-model-x --prompt 'exact model' >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "exact-model capacity should propagate without explicit fallback"
[ "$(cat "$capture/codex.count")" = 1 ] || fail "exact model must disable implicit fallback"

# An explicit fallback remains bounded to one retry.
reset_capture
export FAIL_MODE=capacity PRIMARY_MODEL=exact-model-x
"$CALL" --repo "$repo" --to codex --model exact-model-x --fallback-model backup-model-x \
  --prompt 'exact fallback' >/dev/null
[ "$(cat "$capture/codex.count")" = 2 ] || fail "explicit fallback should retry once"
[ "$(model_arg "$capture/codex.2.argv")" = backup-model-x ] || fail "explicit fallback model missing"

# A hydrated implementation role selects balanced for an isolated write worker.
reset_capture
export WRITE_FILE=routed.txt
"$DELEGATE" --repo "$repo" --to codex --role implementation-worker \
  --prompt 'write routed model' --no-verify >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-balanced-x ] || fail "implementation role should route balanced"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"model_class": "balanced"' || fail "delegate class not indexed"

# A write worker that changed its worktree must not be retried after capacity.
reset_capture
export FAIL_MODE=dirty-capacity PRIMARY_MODEL=codex-deep-x
rc=0
"$DELEGATE" --repo "$repo" --to codex --model-class deep \
  --prompt 'dirty capacity' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "dirty capacity should fail"
[ "$(cat "$capture/codex.count")" = 1 ] || fail "dirty worktree must block fallback"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"fallback_reason": "capacity-dirty-worktree"' ||
  fail "dirty fallback block not indexed"

# Capacity exhaustion is terminal for the provider pass even when repair was
# requested: primary plus one fallback is the complete attempt budget.
reset_capture
export FAIL_MODE=capacity-all
rc=0
"$DELEGATE" --repo "$repo" --to codex --model-class deep --repair 3 \
  --prompt 'bounded capacity exhaustion' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "capacity exhaustion should fail"
[ "$(cat "$capture/codex.count")" = 2 ] || fail "repair must not repeat an exhausted capacity route"

# A repair that mutates an already-staged path keeps the same porcelain status,
# but content fingerprinting must still block fallback.
reset_capture
export FAIL_MODE=repair-stage-dirty WRITE_FILE=routed.txt
rc=0
"$DELEGATE" --repo "$repo" --to codex --model-class deep --repair 1 \
  --prompt 'detect staged content mutation' --verify false >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "dirty staged repair should fail"
[ "$(cat "$capture/codex.count")" = 2 ] || fail "dirty staged repair must block fallback"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"fallback_reason": "capacity-dirty-worktree"' ||
  fail "staged content mutation was not recorded"

# If fallback first occurs during a repair, final lineage must record it.
reset_capture
export FAIL_MODE=repair-capacity WRITE_FILE=routed.txt
"$DELEGATE" --repo "$repo" --to codex --model-class deep --repair 1 \
  --prompt 'record repair fallback' --verify 'grep -q codex-balanced-x routed.txt' >/dev/null
[ "$(cat "$capture/codex.count")" = 3 ] || fail "repair fallback should use three total attempts"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"fallback_used": true' ||
  fail "repair fallback use not indexed"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"selected_model": "codex-balanced-x"' ||
  fail "repair fallback selection not indexed"

# Executor creation freezes the resolved model contract. Later mapping changes
# cannot silently change the worker, and conflicting overrides are rejected.
reset_capture
printf '# Specialization\n\nUse the bounded implementation strategy.\n' > "$repo/executor-soul.md"
"$EXECUTOR" create --repo "$repo" --id routed-executor --provider codex \
  --strategy implementation-worker --soul-file "$repo/executor-soul.md" >/dev/null
"$EXECUTOR" freeze --repo "$repo" --id routed-executor >/dev/null
"$EXECUTOR" brief --repo "$repo" --id routed-executor > "$repo/executor-brief.md"
grep -Fq 'model_class: balanced' "$repo/executor-brief.md" || fail "executor class not frozen"
grep -Fq 'model: codex-balanced-x' "$repo/executor-brief.md" || fail "executor model not frozen"
export OMS_MODEL_CODEX_BALANCED=codex-balanced-changed
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor \
  --prompt 'use frozen route' --no-verify >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-balanced-x ] || fail "executor route changed after creation"
rc=0
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor --model wrong-model \
  --prompt 'reject route override' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "executor should reject a conflicting model override"
rc=0
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor --no-model-fallback \
  --prompt 'reject frozen fallback override' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "executor should reject disabling its frozen fallback"

# Provider-specific fallback names cannot leak into a different synthesis CLI.
rc=0
"$ROOT/scripts/peer-review.sh" --repo "$repo" --providers codex \
  --fallback-model codex-backup --synthesize claude --prompt 'route synthesis' \
  --no-diff --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "different synthesis provider should reject exact fallback"

"$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null
echo "model-routing-smoke: ok"
