#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-routing.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
mkdir -p "$TMP/cap" "$TMP/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/codex"
cp "$TMP/bin/codex" "$TMP/bin/agy"
chmod +x "$TMP/bin/codex" "$TMP/bin/agy"
now="$(date +%s)"
printf 'binary_key=%s\neffort_mechanism=config\neffort_values=low medium high xhigh max ultra\nprobed_at=%s\n' "$(PATH="$TMP/bin:$PATH" bash -c '. "'$ROOT'/scripts/lib/model-capability.sh"; oms_capability_binary_key codex')" "$now" > "$TMP/cap/codex.env"
printf 'binary_key=%s\neffort_mechanism=flag\neffort_values=low medium high\nprobed_at=%s\n' "$(PATH="$TMP/bin:$PATH" bash -c '. "'$ROOT'/scripts/lib/model-capability.sh"; oms_capability_binary_key agy')" "$now" > "$TMP/cap/antigravity.env"
printf 'model-a\nModel B\nmodel-a\n' > "$TMP/cap/codex.models"
printf 'model-a\tlow medium\nModel B\tlow medium high xhigh max ultra\n' > "$TMP/cap/codex.efforts"
printf 'agy-model-a\nagy-model-b\n' > "$TMP/cap/antigravity.models"
printf 'agy-model-a\tlow medium high\nagy-model-b\tlow medium high\n' > "$TMP/cap/antigravity.efforts"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=medium bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_MODEL_ALTERNATE" "$OMS_MODEL_DISTINCT_CHAIN" "$OMS_REASONING_RESOLVED" "$OMS_MODEL_RESOLVED_CLASS"')"
[ "$out" = 'model-a||||medium|explicit' ] || fail "explicit route must not invent fallback candidates: $out"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_MODEL_FALLBACK_EXPLICIT='Model B' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_MODEL_ALTERNATE" "$OMS_MODEL_DISTINCT_CHAIN"')"
[ "$out" = 'model-a|Model B||' ] || fail "explicit capacity fallback route: $out"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_REASONING_RESOLVED" "$OMS_REASONING_EXPLICIT"')"
[ "$out" = 'provider-default|||0' ] || fail "provider default route: $out"
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=provider-default \
  OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' \
  >/dev/null 2>&1; then
  fail 'provider-default is a reserved sentinel, not an explicit model name'
fi
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=high bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' >/dev/null 2>&1; then fail 'per-model effort must validate'; fi
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s" "$OMS_MODEL_DISTINCT_CHAIN"')"
printf '%s\n' "$out" | grep -Fxq 'Model B' || fail 'provider-default catalog chain missing distinct model'
# The tier layer is gone entirely: a legacy class request is plain unknown
# environment — routing neither warns about it nor lets it change the route.
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_CLASS_REQUEST=deep OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_RESOLVED_CLASS"' 2>"$TMP/warn.err")"
[ "$out" = 'provider-default|provider-default' ] || fail "legacy class request must not change the route: $out"
if grep -q 'model tiers' "$TMP/warn.err"; then fail 'removed tier layer must not warn'; fi

# Auto effort resolves to "" (provider default). The REAL invocation must then
# omit the effort flag entirely: codex refuses -c model_reasoning_effort=""
# outright ("reasoning_effort must not be empty"), which silently killed every
# no-effort council seat while the dry-run smokes stayed green. Recorder stubs
# exercise the live command build, not the dry-run shortcut.
cat > "$TMP/bin/codex" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in --version) echo 'codex 1.0'; exit 0 ;; esac
printf '%s\n' "$@" > "$CODEX_ARGV_OUT"
prompt="$(cat)"
count=0
[ ! -f "$OMS_TEST_CALL_LOG" ] || count="$(wc -l < "$OMS_TEST_CALL_LOG" | tr -d ' ')"
count=$((count + 1))
printf '%s\n' "$*" >> "$OMS_TEST_CALL_LOG"
case "${OMS_TEST_PROVIDER_MODE:-}:$count" in
  capacity:1)
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  unknown:1)
    echo "There's an issue with the selected model. It may not exist." >&2
    exit 1
    ;;
  safeguard:1)
    echo "Our safeguards flagged this message; can't respond to this message with that model." >&2
    exit 1
    ;;
  policy-safeguard:1)
    echo "Unable to respond to this request: our safeguards flagged this message; can't respond to this message with that model." >&2
    exit 1
    ;;
  policy-success:1)
    echo "Unable to respond to this request because policy does not allow it."
    exit 0
    ;;
  echoed-policy:1)
    printf '%s\n' "$prompt"
    echo "The quoted refusal text is only input data; this is a complete normal answer."
    exit 0
    ;;
  echoed-policy-refusal:1)
    printf '%s\n' "$prompt"
    echo "Unable to respond to this request."
    exit 1
    ;;
  safeguard-policy:1)
    echo "Our safeguards flagged this message; can't respond to this message with that model." >&2
    exit 1
    ;;
  safeguard-policy:2|capacity-policy:2)
    echo "Unable to respond to this request: our safeguards flagged this message; can't respond to this message with that model." >&2
    exit 1
    ;;
  capacity-policy:1)
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  repair-capacity:1|repair-capacity:3)
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
esac
echo stub answer with enough substance to count
EOF
cat > "$TMP/bin/claude" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in --version) echo 'claude 1.0'; exit 0 ;; esac
printf '%s\n' "$@" > "$CLAUDE_ARGV_OUT"
cat >/dev/null
echo stub answer with enough substance to count
EOF
cat > "$TMP/bin/agy" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in --version) echo 'agy 1.0'; exit 0 ;; esac
printf '%s\n' "$@" > "$AGY_ARGV_OUT"
cat >/dev/null
count=1
if [ -n "${OMS_TEST_AGY_COUNT_FILE:-}" ]; then
  count=0
  [ ! -s "$OMS_TEST_AGY_COUNT_FILE" ] || count="$(cat "$OMS_TEST_AGY_COUNT_FILE")"
  count=$((count + 1))
  printf '%s\n' "$count" > "$OMS_TEST_AGY_COUNT_FILE"
fi
if [ -n "${OMS_TEST_AGY_ARGV_DIR:-}" ]; then
  mkdir -p "$OMS_TEST_AGY_ARGV_DIR"
  printf '%s\n' "$@" > "$OMS_TEST_AGY_ARGV_DIR/$count.argv"
fi
case "${OMS_TEST_AGY_MODE:-}:$count" in
  capacity:1)
    echo 'Selected model is at capacity. Please try a different model.' >&2
    exit 1
    ;;
  repair-effort:1)
    echo "Our safeguards flagged this message; can't respond to this message with that model." >&2
    exit 1
    ;;
  repair-effort:2) printf 'broken\n' > delegated.txt ;;
  repair-effort:3) printf 'fixed\n' > delegated.txt ;;
esac
echo stub Antigravity review answer with enough substance to count
EOF
chmod +x "$TMP/bin/codex" "$TMP/bin/claude" "$TMP/bin/agy"
mkdir -p "$TMP/invoke-repo/.oms" "$TMP/locks" "$TMP/home"
git -C "$TMP/invoke-repo" init -q
git -C "$TMP/invoke-repo" config user.name test
git -C "$TMP/invoke-repo" config user.email test@example.com
printf 'base\n' > "$TMP/invoke-repo/README.md"
git -C "$TMP/invoke-repo" add README.md
git -C "$TMP/invoke-repo" commit -qm base
export OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1

# HOME + NVM_DIR isolation: load_user_tool_paths prepends $HOME/.local/bin
# and the nvm bin dir ahead of the stub PATH, which would silently invoke
# the dev machine's real CLIs instead of the recorders.
invoke() {  # invoke ARGS...
  env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
    CODEX_ARGV_OUT="$TMP/codex.argv" CLAUDE_ARGV_OUT="$TMP/claude.argv" \
    OMS_TEST_CALL_LOG="$TMP/provider.calls" \
    OMS_TEST_PROVIDER_MODE="${OMS_TEST_PROVIDER_MODE:-}" \
    OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
    bash "$ROOT/scripts/agent-call.sh" --repo "$TMP/invoke-repo" "$@" >/dev/null 2>&1
}
agy_invoke() {  # agy_invoke ARGS...
  env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
    AGY_ARGV_OUT="$TMP/agy.argv" OMS_TEST_AGY_COUNT_FILE="$TMP/agy.count" \
    OMS_TEST_AGY_ARGV_DIR="$TMP/agy-argv" \
    OMS_TEST_AGY_MODE="${OMS_TEST_AGY_MODE:-}" \
    OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
    bash "$ROOT/scripts/agent-call.sh" --repo "$TMP/invoke-repo" "$@" >/dev/null 2>&1
}

invoke --to codex --prompt "argv probe" || fail 'auto-effort codex call failed'
[ -s "$TMP/codex.argv" ] || fail 'codex recorder stub was not invoked'
if grep -q 'model_reasoning_effort' "$TMP/codex.argv"; then
  fail 'auto effort must omit the codex effort flag, not pass it empty'
fi
invoke --to codex --reasoning-effort high --prompt "argv probe" ||
  fail 'high-effort codex call failed'
grep -q 'model_reasoning_effort="high"' "$TMP/codex.argv" ||
  fail 'explicit effort must still reach codex'
for extended_effort in xhigh max ultra; do
  invoke --to codex --reasoning-effort "$extended_effort" --prompt "index $extended_effort route probe" ||
    fail "$extended_effort-effort codex call failed"
  python3 - "$TMP/invoke-repo/.oms/artifacts/index.jsonl" "$extended_effort" <<'PY' || fail 'artifact index dropped the current route class or extended effort'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
row = rows[-1]
assert row.get("model_class") == "provider-default", row
assert row.get("reasoning_effort") == sys.argv[2], row
assert row.get("selected_reasoning_effort") == sys.argv[2], row
PY
done
invoke --to claude --prompt "argv probe" || fail 'auto-effort claude call failed'
[ -s "$TMP/claude.argv" ] || fail 'claude recorder stub was not invoked'
if grep -q -- '--effort' "$TMP/claude.argv"; then
  fail 'auto effort must omit the claude effort flag, not pass it empty'
fi

# Naming a model is a reproducibility contract: unavailable, safeguard, and
# capacity failures must not silently move to a catalog entry or provider
# default. Only an explicit capacity backup permits one bounded retry.
: > "$TMP/provider.calls"
if OMS_TEST_PROVIDER_MODE=capacity invoke --to codex --model model-a --prompt "capacity exact"; then
  fail 'exact-model capacity failure should propagate without an explicit fallback'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'exact-model capacity failure made an implicit retry'

: > "$TMP/provider.calls"
OMS_TEST_PROVIDER_MODE=capacity invoke --to codex --model model-a \
  --fallback-model 'Model B' --prompt "capacity backup" ||
  fail 'explicit capacity fallback should recover'
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
  fail 'explicit capacity fallback should make exactly two attempts'
tail -n 1 "$TMP/provider.calls" | grep -Fq 'Model B' ||
  fail 'the explicit fallback model was not selected'

for mode in unknown safeguard; do
  : > "$TMP/provider.calls"
  if OMS_TEST_PROVIDER_MODE="$mode" invoke --to codex --model model-a \
    --fallback-model 'Model B' --prompt "$mode exact"; then
    fail "exact-model $mode failure should propagate"
  fi
  [ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
    fail "exact-model $mode failure made an implicit catalog retry"
done

for mode in safeguard unknown; do
  : > "$TMP/provider.calls"
  OMS_TEST_PROVIDER_MODE="$mode" invoke --to codex --prompt "default $mode" ||
    fail "provider-default route should retain bounded catalog $mode recovery"
  [ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
    fail "provider-default $mode recovery should make one retry"
  sed -n '2p' "$TMP/provider.calls" | grep -Fq -- '--model model-a' ||
    fail "provider-default $mode recovery did not select the first catalog model"
done

: > "$TMP/provider.calls"
OMS_TEST_PROVIDER_MODE=unknown invoke --to codex --reasoning-effort ultra \
  --prompt "default recovery with a model-specific effort" ||
  fail 'provider-default recovery should find a model that accepts the requested effort'
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
  fail 'effort-compatible recovery should make one retry'
sed -n '2p' "$TMP/provider.calls" | grep -Fq -- '--model Model B' ||
  fail 'catalog recovery selected a model that does not accept the requested effort'

: > "$TMP/provider.calls"
if OMS_TEST_PROVIDER_MODE=policy-safeguard invoke --to codex --prompt "combined policy refusal"; then
  fail 'a policy decline that also mentions safeguards must not be retried'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'combined policy+safeguard output was routed to another model'

: > "$TMP/provider.calls"
policy_status=0
OMS_TEST_PROVIDER_MODE=policy-success invoke --to codex --prompt "successful-exit policy refusal" ||
  policy_status=$?
[ "$policy_status" = 4 ] ||
  fail "policy refusal must use the non-retryable exit status, got $policy_status"
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'successful-exit policy refusal made an extra model attempt'

: > "$TMP/provider.calls"
OMS_TEST_PROVIDER_MODE=echoed-policy invoke --to codex \
  --prompt "Explain this quoted text: Unable to respond to this request." ||
  fail 'a successful answer must not be declined because the CLI echoed policy text from its prompt'
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'prompt-echo policy text caused an unexpected retry'

: > "$TMP/provider.calls"
policy_status=0
OMS_TEST_PROVIDER_MODE=echoed-policy-refusal invoke --to codex \
  --prompt "Unable to respond to this request." || policy_status=$?
[ "$policy_status" = 4 ] ||
  fail "a real refusal repeated after a prompt echo must remain terminal, got $policy_status"
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'a refusal repeated after prompt echo made an unexpected retry'

: > "$TMP/provider.calls"
if OMS_TEST_PROVIDER_MODE=safeguard-policy invoke --to codex --prompt "fallback policy refusal"; then
  fail 'a policy decline reached through safeguard recovery should fail'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
  fail 'policy decline on a safeguard recovery was routed to another model'

: > "$TMP/provider.calls"
if OMS_TEST_PROVIDER_MODE=capacity-policy invoke --to codex --model model-a \
  --fallback-model 'Model B' --prompt "capacity fallback policy refusal"; then
  fail 'a policy decline from the explicit capacity fallback should fail'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
  fail 'capacity fallback policy scenario made an unexpected attempt'
if ! python3 - "$TMP/invoke-repo/.oms/artifacts/index.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert rows[-1].get("fallback_reason") == "policy-declined", rows[-1]
PY
then
  fail 'capacity fallback policy decline was not recorded as policy-declined'
fi

env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" AGY_ARGV_OUT="$TMP/agy.argv" \
  OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-review.sh" --repo "$TMP/invoke-repo" \
    --providers antigravity --no-diff --reasoning-effort high \
    --prompt "effort routing probe" >/dev/null 2>&1 ||
  fail 'peer-review should accept Antigravity reasoning effort supported by the capability registry'
grep -Fxq -- '--effort' "$TMP/agy.argv" || fail 'peer-review dropped Antigravity --effort'
grep -Fxq -- 'high' "$TMP/agy.argv" || fail 'peer-review dropped Antigravity effort value'

# An explicit capacity fallback uses provider-default effort unless a separate
# fallback effort is frozen. Antigravity must omit the flag rather than pass an
# empty value, which its CLI rejects.
printf '0\n' > "$TMP/agy.count"
rm -rf "$TMP/agy-argv"; mkdir -p "$TMP/agy-argv"
OMS_TEST_AGY_MODE=capacity agy_invoke --to antigravity --model agy-model-a \
  --fallback-model agy-model-b --reasoning-effort high --prompt "agy fallback" ||
  fail 'Antigravity explicit capacity fallback should recover'
[ "$(cat "$TMP/agy.count")" = 2 ] ||
  fail 'Antigravity explicit fallback should make exactly two attempts'
if grep -Fxq -- '--effort' "$TMP/agy-argv/2.argv"; then
  fail 'Antigravity fallback passed an empty --effort instead of provider default'
fi
grep -Fxq -- 'agy-model-b' "$TMP/agy-argv/2.argv" ||
  fail 'Antigravity fallback model was not selected'

# Frozen executor effort is part of the route contract and must reach agy.
printf 'Keep the task bounded and report verification.\n' > "$TMP/agy-soul.md"
env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" \
  bash "$ROOT/scripts/agent-executor.sh" create --repo "$TMP/invoke-repo" \
    --id agy-effort --provider antigravity --reasoning-effort high \
    --soul-file "$TMP/agy-soul.md" >/dev/null || fail 'Antigravity executor create failed'
bash "$ROOT/scripts/agent-executor.sh" freeze --repo "$TMP/invoke-repo" \
  --id agy-effort >/dev/null || fail 'Antigravity executor freeze failed'
printf '0\n' > "$TMP/agy.count"
rm -rf "$TMP/agy-argv"; mkdir -p "$TMP/agy-argv"
env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
  AGY_ARGV_OUT="$TMP/agy.argv" OMS_TEST_AGY_COUNT_FILE="$TMP/agy.count" \
  OMS_TEST_AGY_ARGV_DIR="$TMP/agy-argv" OMS_CAPABILITY_DIR="$TMP/cap" \
  OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/invoke-repo" \
    --to antigravity --executor agy-effort --no-verify \
    --artifact-dir "$TMP/invoke-repo/.oms/artifacts/agy-executor" \
    --prompt "executor effort" >/dev/null 2>&1 ||
  fail 'Antigravity frozen executor call failed'
grep -Fxq -- '--effort' "$TMP/agy-argv/1.argv" ||
  fail 'Antigravity frozen executor dropped --effort'
grep -Fxq -- 'high' "$TMP/agy-argv/1.argv" ||
  fail 'Antigravity frozen executor dropped its effort value'

# When catalog recovery succeeds but verification fails, repair continues on
# the selected model and preserves the selected explicit effort.
printf '0\n' > "$TMP/agy.count"
rm -rf "$TMP/agy-argv"; mkdir -p "$TMP/agy-argv"
if ! env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
  AGY_ARGV_OUT="$TMP/agy.argv" OMS_TEST_AGY_COUNT_FILE="$TMP/agy.count" \
  OMS_TEST_AGY_ARGV_DIR="$TMP/agy-argv" OMS_TEST_AGY_MODE=repair-effort \
  OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/invoke-repo" \
    --to antigravity --reasoning-effort high --repair 1 \
    --verify 'grep -q fixed delegated.txt' --prompt "repair effort" \
    >/dev/null 2>&1; then
  fail 'Antigravity repair effort scenario should recover'
fi
[ "$(cat "$TMP/agy.count")" = 3 ] ||
  fail 'Antigravity repair effort scenario should make three attempts'
grep -Fxq -- '--effort' "$TMP/agy-argv/3.argv" ||
  fail 'Antigravity repair dropped the selected effort flag'
grep -Fxq -- 'high' "$TMP/agy-argv/3.argv" ||
  fail 'Antigravity repair dropped the selected effort value'

: > "$TMP/provider.calls"
if env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
  CODEX_ARGV_OUT="$TMP/codex.argv" OMS_TEST_CALL_LOG="$TMP/provider.calls" \
  OMS_TEST_PROVIDER_MODE=safeguard OMS_CAPABILITY_DIR="$TMP/cap" \
  OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/invoke-repo" --to codex \
    --model model-a --repair 2 --no-verify --prompt "terminal safeguard" \
    >"$TMP/delegate-safeguard.out" 2>&1; then
  fail 'explicit safeguard delegation should fail'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail "repair rounds retried an exact-model safeguard failure: $(cat "$TMP/provider.calls"); $(cat "$TMP/delegate-safeguard.out")"

: > "$TMP/provider.calls"
if env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
  CODEX_ARGV_OUT="$TMP/codex.argv" OMS_TEST_CALL_LOG="$TMP/provider.calls" \
  OMS_TEST_PROVIDER_MODE=repair-capacity OMS_CAPABILITY_DIR="$TMP/cap" \
  OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/invoke-repo" --to codex \
    --model model-a --fallback-model 'Model B' --repair 1 --verify false \
    --prompt "fallback repair" >/dev/null 2>&1; then
  fail 'failed verification and repair capacity should fail delegation'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 3 ] ||
  fail 'repair after explicit fallback silently retried provider default'
sed -n '3p' "$TMP/provider.calls" | grep -Fq -- '--model Model B' ||
  fail 'repair did not stay on the successfully selected fallback model'

# The standalone model options are unambiguous only with one plain, explicit
# provider target. The target notation already carries an exact model and must
# not be silently overwritten by a second --model.
consult_help="$(bash "$ROOT/scripts/consult.sh" --help)" || fail 'consult --help failed'
printf '%s' "$consult_help" | grep -Fq -- '--model MODEL' || fail 'consult help omits --model'
printf '%s' "$consult_help" | grep -Fq -- '--fallback-model M' || fail 'consult help omits --fallback-model'
printf '%s' "$consult_help" | grep -Fq -- '--reasoning-effort E' || fail 'consult help omits --reasoning-effort'
consult_invoke() {
  env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
    CODEX_ARGV_OUT="$TMP/codex.argv" CLAUDE_ARGV_OUT="$TMP/claude.argv" \
    AGY_ARGV_OUT="$TMP/agy.argv" OMS_TEST_CALL_LOG="$TMP/provider.calls" \
    OMS_TEST_PROVIDER_MODE="${OMS_TEST_PROVIDER_MODE:-}" \
    OMS_TEST_AGY_COUNT_FILE="$TMP/agy.count" OMS_TEST_AGY_ARGV_DIR="$TMP/agy-argv" \
    OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
    bash "$ROOT/scripts/consult.sh" --repo "$TMP/invoke-repo" "$@" >/dev/null 2>&1
}
if consult_invoke --model model-a --dry-run --prompt "ambiguous model"; then
  fail 'consult --model without one explicit --to should fail'
fi
if consult_invoke --fallback-model 'Model B' --dry-run --prompt "ambiguous fallback"; then
  fail 'consult --fallback-model without one explicit --to should fail'
fi
if consult_invoke --to codex --to claude --model model-a --dry-run --prompt "panel model"; then
  fail 'consult model options with several explicit targets should fail'
fi
if consult_invoke --to codex:model=model-a --model 'Model B' --dry-run \
  --prompt "conflicting model"; then
  fail 'consult target model and standalone model conflict should fail'
fi
consult_invoke --to codex --model model-a --fallback-model 'Model B' \
  --reasoning-effort medium --dry-run --prompt "explicit route" ||
  fail 'consult should accept model options with one plain explicit provider'

: > "$TMP/provider.calls"
printf '0\n' > "$TMP/agy.count"
policy_status=0
OMS_AGENT=claude OMS_TEST_PROVIDER_MODE=policy-success \
  consult_invoke --new-thread --prompt "automatic policy refusal" || policy_status=$?
[ "$policy_status" = 4 ] ||
  fail "automatic consult must preserve the policy exit status, got $policy_status"
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'automatic consult routed a policy decline to the next provider'
[ "$(cat "$TMP/agy.count")" = 0 ] ||
  fail 'automatic consult invoked Antigravity after a policy decline'
echo 'model-routing-smoke: ok'
