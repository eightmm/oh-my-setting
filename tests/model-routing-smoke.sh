#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-routing.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
mkdir -p "$TMP/cap" "$TMP/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/codex"; chmod +x "$TMP/bin/codex"
now="$(date +%s)"
printf 'binary_key=%s\neffort_mechanism=config\neffort_values=low medium high xhigh\nprobed_at=%s\n' "$(PATH="$TMP/bin:$PATH" bash -c '. "'$ROOT'/scripts/lib/model-capability.sh"; oms_capability_binary_key codex')" "$now" > "$TMP/cap/codex.env"
printf 'model-a\nModel B\nmodel-a\n' > "$TMP/cap/codex.models"
printf 'model-a\tlow medium\nModel B\tlow medium high\n' > "$TMP/cap/codex.efforts"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=medium bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_REASONING_RESOLVED" "$OMS_MODEL_RESOLVED_CLASS"')"
[ "$out" = 'model-a|provider-default|medium|explicit' ] || fail "explicit route: $out"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_REASONING_RESOLVED" "$OMS_REASONING_EXPLICIT"')"
[ "$out" = 'provider-default|||0' ] || fail "provider default route: $out"
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=high bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' >/dev/null 2>&1; then fail 'per-model effort must validate'; fi
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s" "$OMS_MODEL_DISTINCT_CHAIN"')"
printf '%s\n' "$out" | grep -Fxq 'Model B' || fail 'catalog chain missing distinct model'
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
printf '%s\n' "$@" > "$CODEX_ARGV_OUT"
cat >/dev/null
echo stub answer with enough substance to count
EOF
cat > "$TMP/bin/claude" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$CLAUDE_ARGV_OUT"
cat >/dev/null
echo stub answer with enough substance to count
EOF
chmod +x "$TMP/bin/codex" "$TMP/bin/claude"
mkdir -p "$TMP/invoke-repo/.oms" "$TMP/locks" "$TMP/home"
git -C "$TMP/invoke-repo" init -q
export OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1

# HOME + NVM_DIR isolation: load_user_tool_paths prepends $HOME/.local/bin
# and the nvm bin dir ahead of the stub PATH, which would silently invoke
# the dev machine's real CLIs instead of the recorders.
invoke() {  # invoke ARGS...
  env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" \
    CODEX_ARGV_OUT="$TMP/codex.argv" CLAUDE_ARGV_OUT="$TMP/claude.argv" \
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
invoke --to claude --prompt "argv probe" || fail 'auto-effort claude call failed'
[ -s "$TMP/claude.argv" ] || fail 'claude recorder stub was not invoked'
if grep -q -- '--effort' "$TMP/claude.argv"; then
  fail 'auto effort must omit the claude effort flag, not pass it empty'
fi
echo 'model-routing-smoke: ok'
