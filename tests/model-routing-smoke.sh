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
printf '.ignored-state\n.verify-cache\nagy-first-attempt\n' > "$repo/.gitignore"
git -C "$repo" add README.md
git -C "$repo" add .gitignore
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
if [ "${FAIL_MODE:-}" = ignored-dirty-capacity ] && [ "$count" -eq 1 ]; then
  printf 'dirty\n' > .ignored-state
  echo 'Selected model is at capacity. Please try a different model.' >&2
  exit 1
fi
if [ "${FAIL_MODE:-}" = agy-read-dirty ] && [ "$count" -eq 1 ]; then
  printf 'dirty\n' > agy-first-attempt
  echo 'Selected model is at capacity. Please try a different model.' >&2
  exit 1
fi
if [ "${FAIL_MODE:-}" = agy-read-dirty ] && [ -e agy-first-attempt ]; then
  echo 'fallback reused dirty Antigravity read worktree' >&2
  exit 9
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

default_agy_balanced="$(
  unset OMS_MODEL_ANTIGRAVITY_BALANCED
  # shellcheck source=scripts/lib/model-routing.sh
  . "$ROOT/scripts/lib/model-routing.sh"
  oms_model_mapping antigravity balanced
)"
[ "$default_agy_balanced" = 'Gemini 3.5 Flash (Medium)' ] || fail "Antigravity balanced default should encode medium effort"

route_class() (
  export OMS_MODEL_ROLE="$1"
  export OMS_MODEL_OPERATION="$2"
  export OMS_MODEL_CLASS_REQUEST="${3:-auto}"
  export OMS_MODEL_EXPLICIT="" OMS_MODEL_FALLBACK_EXPLICIT=""
  export OMS_REASONING_EFFORT_REQUEST=auto OMS_REASONING_FALLBACK_EXPLICIT=""
  # shellcheck source=scripts/lib/model-routing.sh
  . "$ROOT/scripts/lib/model-routing.sh"
  oms_model_prepare codex
  printf '%s\n' "$OMS_MODEL_RESOLVED_CLASS"
)

# Auto routing follows the current work phase. Roles are fallback hints only,
# so a role cannot downgrade planning or inflate routine execution.
[ "$(route_class repo-auditor decision)" = deep ] ||
  fail "decision phase should override a fast role"
[ "$(route_class decision-advisor delegate)" = balanced ] ||
  fail "implementation phase should override a deep role"
[ "$(route_class decision-advisor verify)" = fast ] ||
  fail "verification phase should override a deep role"
[ "$(route_class '' ask)" = balanced ] ||
  fail "ordinary peer asks should use balanced routing"
[ "$(route_class repo-auditor unknown)" = fast ] ||
  fail "unknown phases should fall back to the role"
[ "$(route_class repo-auditor decision fast)" = fast ] ||
  fail "an explicit model class should override automatic phase routing"

reset_capture() {
  rm -f "$capture"/*
  unset FAIL_MODE PRIMARY_MODEL WRITE_FILE
}

model_arg() {
  awk 'p{print;exit} $0=="-m" || $0=="--model"{p=1}' "$1"
}

effort_arg() {
  awk '
    p{print;exit}
    $0=="--effort"{p=1;next}
    $0=="-c"{getline; if ($0 ~ /^model_reasoning_effort=/) {print; exit}}
  ' "$1"
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
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="low"' ] ||
  fail "Codex fast route should use low reasoning"
grep -Fq '"reasoning_effort": "low"' "$repo/.oms/artifacts/index.jsonl" || fail "reasoning effort not indexed"

# Every provider receives its own mapped model flag.
reset_capture
"$CALL" --repo "$repo" --to claude --model-class balanced --prompt 'inspect routing' >/dev/null
[ "$(model_arg "$capture/claude.1.argv")" = claude-balanced-x ] || fail "Claude balanced mapping missing"
[ "$(effort_arg "$capture/claude.1.argv")" = medium ] || fail "Claude balanced route should use medium effort"
reset_capture
"$CALL" --repo "$repo" --to antigravity --model-class fast --prompt 'inspect routing' >/dev/null
[ "$(model_arg "$capture/agy.1.argv")" = agy-fast-x ] || fail "Antigravity fast mapping missing"
if grep -Fxq -- '--effort' "$capture/agy.1.argv"; then fail "Antigravity has no independent effort flag"; fi

# Antigravity provenance is known only when the selected model names an effort
# variant. Custom model names must not inherit a fabricated class effort.
reset_capture
"$CALL" --repo "$repo" --to antigravity --model custom-agy-model \
  --prompt 'unknown variant effort' >/dev/null
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"reasoning_effort"' &&
  fail "custom Antigravity model must not claim a reasoning effort"
reset_capture
"$CALL" --repo "$repo" --to antigravity --model 'Custom Agy (High)' \
  --prompt 'known variant effort' >/dev/null
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"reasoning_effort": "high"' ||
  fail "named Antigravity variant effort not indexed"

# Explicit effort overrides auto for providers that expose a supported control.
reset_capture
"$CALL" --repo "$repo" --to codex --model-class deep --reasoning-effort low \
  --prompt 'explicit reasoning' >/dev/null
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="low"' ] ||
  fail "explicit Codex effort missing"
reset_capture
rc=0
"$CALL" --repo "$repo" --to antigravity --reasoning-effort high \
  --prompt 'unsupported explicit reasoning' >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "Antigravity explicit effort should fail instead of being ignored"
reset_capture
rc=0
"$CALL" --repo "$repo" --to codex --reasoning-effort extreme \
  --prompt 'invalid reasoning' >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid reasoning effort should fail"
[ ! -f "$capture/codex.count" ] || fail "invalid effort must not call provider"

# Export-only validates the provider route and tells the manual caller which
# model contract to preserve.
reset_capture
rc=0
"$CALL" --repo "$repo" --to antigravity --reasoning-effort high \
  --prompt 'invalid export route' --export-only >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "export-only should reject an unsupported route"
reset_capture
"$CALL" --repo "$repo" --to codex --model-class fast \
  --prompt 'export routed model' --export-only >/dev/null
export_artifact="$(find "$repo/.oms/artifacts/call" -type f -name '*export-routed-model*.export.md' | head -n 1)"
grep -Fq -- '- selected-model: codex-fast-x' "$export_artifact" ||
  fail "export artifact missing selected model"
grep -Fq -- '- reasoning-effort: low' "$export_artifact" ||
  fail "export artifact missing reasoning effort"
peer_ask_caller="$TMP/peer-ask-caller"
mkdir -p "$peer_ask_caller"
(
  cd "$peer_ask_caller"
  "$ROOT/scripts/peer-ask.sh" --repo "$repo" --providers codex --model-class fast \
    --prompt 'export peer route' --export-only >/dev/null
)
[ ! -e "$peer_ask_caller/.oms" ] ||
  fail "peer-ask --repo must not leave default artifacts in the caller directory"
find "$repo/.oms/artifacts/ask" -type f -name '*export-peer-route*' | grep -q . ||
  fail "peer-ask --repo should keep default artifacts under the state repo"
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"selected_model"' &&
  fail "local export synthesis must not inherit provider route provenance"

# Invalid environment mappings fail before constructing a provider argv.
reset_capture
export OMS_MODEL_CODEX_FAST=$'invalid\tmodel'
rc=0
"$CALL" --repo "$repo" --to codex --model-class fast --prompt 'bad mapping' >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "invalid mapped model should fail validation"
[ ! -f "$capture/codex.count" ] || fail "invalid mapping must not call provider"
export OMS_MODEL_CODEX_FAST=codex-fast-x

# Dry-run validates and records the same route that a real call would use.
reset_capture
export OMS_MODEL_CODEX_FAST=$'invalid\tmodel'
rc=0
"$CALL" --repo "$repo" --to codex --model-class fast --prompt 'dry route' \
  --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "dry-run should reject an invalid route"
export OMS_MODEL_CODEX_FAST=codex-fast-x
"$CALL" --repo "$repo" --to codex --model-class fast --prompt 'dry route' \
  --dry-run >/dev/null
tail -n 1 "$repo/.oms/artifacts/index.jsonl" | grep -Fq '"selected_model": "codex-fast-x"' ||
  fail "dry-run route not indexed"

# All terminal control bytes are unsafe in argv and provenance output.
reset_capture
rc=0
"$CALL" --repo "$repo" --to codex --model $'bad\033model' \
  --prompt 'control model' >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "control character in model name should fail"

# The unified entrypoint forwards the routing contract to its selected path.
reset_capture
"$RUN" --repo "$repo" --to codex --mode read --model-class fast \
  --prompt 'inspect entrypoint routing' >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-fast-x ] || fail "agent-run did not forward model class"
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="low"' ] || fail "agent-run did not forward auto effort"

# Capacity alone retries once on the next lower class and records the fallback.
reset_capture
export FAIL_MODE=capacity PRIMARY_MODEL=codex-deep-x
"$CALL" --repo "$repo" --to codex --model-class deep --prompt 'capacity fallback' >/dev/null
[ "$(cat "$capture/codex.count")" = 2 ] || fail "capacity fallback must make two attempts"
[ "$(model_arg "$capture/codex.1.argv")" = codex-deep-x ] || fail "deep primary missing"
[ "$(model_arg "$capture/codex.2.argv")" = codex-balanced-x ] || fail "deep fallback should use balanced"
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="high"' ] || fail "deep primary effort missing"
[ "$(effort_arg "$capture/codex.2.argv")" = 'model_reasoning_effort="medium"' ] || fail "fallback effort should downgrade"
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

# Ignored files are still part of a write worker's state and must block a
# capacity retry on the same isolated worktree.
reset_capture
export FAIL_MODE=ignored-dirty-capacity PRIMARY_MODEL=codex-deep-x
rc=0
"$DELEGATE" --repo "$repo" --to codex --model-class deep \
  --prompt 'ignored dirty capacity' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "ignored dirty capacity should fail"
[ "$(cat "$capture/codex.count")" = 1 ] || fail "ignored worktree mutation must block fallback"

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

# Repair starts from the captured patch only; ignored verification cache from
# the previous attempt must not make a later verification pass spuriously.
reset_capture
export WRITE_FILE=routed.txt
rc=0
"$DELEGATE" --repo "$repo" --to codex --model-class balanced --repair 1 \
  --prompt 'clean ignored verify cache' \
  --verify 'if [ -e .verify-cache ]; then true; else touch .verify-cache; false; fi' \
  >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "ignored verify cache must be removed before repair"
[ "$(cat "$capture/codex.count")" = 2 ] || fail "repair should run exactly once"

# Antigravity read fallback also receives a pristine isolation directory.
reset_capture
export FAIL_MODE=agy-read-dirty PRIMARY_MODEL=agy-deep-x
"$CALL" --repo "$repo" --to antigravity --model-class deep \
  --prompt 'pristine read fallback' >/dev/null
[ "$(cat "$capture/agy.count")" = 2 ] || fail "Antigravity read should fallback once"

# Executor creation freezes the resolved model contract. Later mapping changes
# cannot silently change the worker, and conflicting overrides are rejected.
reset_capture
printf '# Specialization\n\nUse the bounded implementation strategy.\n' > "$repo/executor-soul.md"
"$EXECUTOR" create --repo "$repo" --id routed-executor --provider codex \
  --strategy implementation-worker --soul-file "$repo/executor-soul.md" >/dev/null
"$EXECUTOR" freeze --repo "$repo" --id routed-executor >/dev/null
"$EXECUTOR" brief --repo "$repo" --id routed-executor > "$repo/executor-brief.md"
python3 - "$repo/.oms/executors/routed-executor/meta.json" <<'PY'
import json, sys
d=json.load(open(sys.argv[1], encoding="utf-8"))
assert d["model_class"] == "balanced"
assert d["model"] == "codex-balanced-x"
assert d["reasoning_effort"] == "medium"
PY
if grep -Eq '^(model_class|model|fallback_model|reasoning_effort|fallback_reasoning_effort|lease_id|base_sha|soul_sha256):' \
    "$repo/executor-brief.md"; then
  fail "machine-owned executor routing metadata should not enter the worker prompt"
fi
grep -Fq 'executor_id: routed-executor' "$repo/executor-brief.md" || fail "executor identity missing from brief"
export OMS_MODEL_CODEX_BALANCED=codex-balanced-changed
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor \
  --prompt 'use frozen route' --no-verify >/dev/null
[ "$(model_arg "$capture/codex.1.argv")" = codex-balanced-x ] || fail "executor route changed after creation"
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="medium"' ] || fail "executor effort changed after creation"

# Metadata created before reasoning routing had no effort fields. An explicit
# caller override must be honored rather than accepted and silently replaced.
"$EXECUTOR" create --repo "$repo" --id legacy-executor --provider codex \
  --strategy implementation-worker --soul-file "$repo/executor-soul.md" >/dev/null
"$EXECUTOR" freeze --repo "$repo" --id legacy-executor >/dev/null
python3 - "$repo/.oms/executors/legacy-executor/meta.json" <<'PY'
import json, sys
p=sys.argv[1]; d=json.load(open(p)); d.pop("reasoning_effort", None); d.pop("fallback_reasoning_effort", None)
json.dump(d, open(p, "w"), indent=2)
PY
reset_capture
"$DELEGATE" --repo "$repo" --to codex --executor legacy-executor \
  --reasoning-effort high --prompt 'legacy executor effort' --no-verify >/dev/null
[ "$(effort_arg "$capture/codex.1.argv")" = 'model_reasoning_effort="high"' ] ||
  fail "legacy executor explicit effort was ignored"
rc=0
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor --model wrong-model \
  --prompt 'reject route override' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "executor should reject a conflicting model override"
rc=0
"$DELEGATE" --repo "$repo" --to codex --executor routed-executor --reasoning-effort high \
  --prompt 'reject effort override' --no-verify >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "executor should reject a conflicting effort override"
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

rc=0
"$ROOT/scripts/peer-ask.sh" --providers codex,antigravity --reasoning-effort high \
  --prompt 'reject mixed effort' --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "multi-provider explicit effort should reject Antigravity"

# Quorum members must be distinct after provider alias normalization.
for duplicate in codex,codex antigravity,agy; do
  rc=0
  "$ROOT/scripts/peer-ask.sh" --providers "$duplicate" --prompt 'duplicate quorum' \
    --dry-run >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "duplicate provider quorum should fail: $duplicate"
done
rc=0
"$ROOT/scripts/peer-review.sh" --repo "$repo" --providers codex,codex \
  --prompt 'duplicate review quorum' --no-diff --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "duplicate review provider quorum should fail"

"$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null
echo "model-routing-smoke: ok"
