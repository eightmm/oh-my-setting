#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-provider-registry.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

mkdir -p "$TMP/bin" "$TMP/home" "$TMP/cap" "$TMP/locks" "$TMP/logs"

cat > "$TMP/bin/provider-fake" <<'FAKE'
#!/usr/bin/env bash
set -u
name="$(basename "$0")"
case "${1:-}" in
  --version|-v|version) printf '%s 1.0.0\n' "$name"; exit 0 ;;
  --help|-h) printf 'usage: %s [headless options]\n' "$name"; exit 0 ;;
  models|--list-models) printf 'provider/model-a\nprovider/model-b\n'; exit 0 ;;
  --no-auto-update)
    case "${2:-}" in
      version|--version) printf '%s 1.0.0\n' "$name"; exit 0 ;;
      --help) printf 'usage: %s [headless options]\n' "$name"; exit 0 ;;
    esac
    ;;
esac
printf '%s\n' "$@" > "$OMS_TEST_LOG_DIR/$name.argv"
cat > "$OMS_TEST_LOG_DIR/$name.stdin"
if [ "${OMS_TEST_WRITE:-0}" = 1 ]; then
  printf 'written by %s\n' "$name" > "$PWD/$name-write.txt"
fi
printf '%s returned a complete independent answer with enough detail.\n' "$name"
FAKE
chmod +x "$TMP/bin/provider-fake"
for binary in cursor-agent grok gemini qwen opencode; do
  ln -s provider-fake "$TMP/bin/$binary"
done

cat > "$TMP/bin/oms-agent-adapter-localfoo" <<'ADAPTER'
#!/usr/bin/env bash
set -u
case "${1:-}" in
  --version) echo 'localfoo adapter 1.0'; exit 0 ;;
  --help) echo 'usage: oms-agent-adapter-localfoo run'; exit 0 ;;
esac
printf '%s\n' "$@" > "$OMS_TEST_LOG_DIR/localfoo.argv"
prompt=""
access=""
workdir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt-file) shift; prompt="$1" ;;
    --access) shift; access="$1" ;;
    --workdir) shift; workdir="$1" ;;
  esac
  shift
done
[ -z "$prompt" ] || cat "$prompt" > "$OMS_TEST_LOG_DIR/localfoo.prompt"
[ "${OMS_TEST_ADAPTER_TOUCH:-0}" != 1 ] || printf 'isolated read touch\n' > "$workdir/adapter-touch.txt"
if [ "${OMS_TEST_WRITE:-0}" = 1 ] && [ "$access" = write ]; then
  printf 'written by custom adapter\n' > "$workdir/localfoo-write.txt"
fi
echo 'local adapter returned a complete independent answer.'
ADAPTER
chmod +x "$TMP/bin/oms-agent-adapter-localfoo"

export PATH="$TMP/bin:/usr/bin:/bin"
export HOME="$TMP/home"
export OMS_CAPABILITY_DIR="$TMP/cap"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1
export OMS_TEST_LOG_DIR="$TMP/logs"

# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"

expected_supported='codex
claude
antigravity
cursor
grok
gemini
qwen
opencode'
[ "$(oms_provider_supported_names)" = "$expected_supported" ] ||
  fail "supported provider catalog is incomplete: $(oms_provider_supported_names 2>&1 || true)"

expected_installed='cursor
grok
gemini
qwen
opencode
localfoo'
[ "$(oms_provider_installed_names)" = "$expected_installed" ] ||
  fail "installed provider discovery mismatch: $(oms_provider_installed_names 2>&1 || true)"

for pair in \
  'agy antigravity' \
  'cursor-agent cursor' \
  'grok-build grok' \
  'gemini-cli gemini' \
  'qwen-code qwen' \
  'opencode2 opencode' \
  'localfoo localfoo'; do
  set -- $pair
  [ "$(oms_provider_normalize "$1")" = "$2" ] || fail "provider alias $1"
done
if oms_provider_normalize glm >/dev/null 2>&1; then
  fail "GLM is a model family, not an agent transport"
fi

[ "$(oms_provider_model_family grok grok-4.6)" = xai ] || fail 'Grok family'
[ "$(oms_provider_model_family grok provider-default)" = xai ] || fail 'Grok default family'
[ "$(oms_provider_model_family opencode xai/grok-4.6)" = xai ] || fail 'OpenCode Grok family'
[ "$(oms_provider_model_family opencode zai/glm-4.7)" = zhipu ] || fail 'OpenCode GLM family'
[ "$(oms_provider_model_family cursor GLM-5.1)" = zhipu ] || fail 'Cursor GLM family'
[ "$(oms_provider_model_family opencode provider-default)" = unknown ] ||
  fail 'multi-model provider defaults must not invent independence'

for provider in cursor grok gemini qwen opencode; do
  oms_provider_supports_access "$provider" read || fail "$provider should support read calls"
  oms_provider_supports_access "$provider" write || fail "$provider should support isolated writes"
done
oms_provider_supports_access localfoo read || fail 'custom adapters should be read-capable'
if oms_provider_supports_access localfoo write; then
  fail 'custom adapters need explicit write opt-in'
fi
OMS_PROVIDER_WRITE_ADAPTERS=localfoo oms_provider_supports_access localfoo write ||
  fail 'custom adapter write opt-in was ignored'

# A PATH-discovered adapter must never enter an automatic pool: default-scope
# peer candidates exclude it, only the explicit all scope fans out to it, and
# an unknown scope is a typed refusal rather than a silent widening.
case ",$(oms_provider_peer_candidates claude | tr '\n' ',')" in
  *,localfoo,*) fail 'custom adapter leaked into the automatic peer pool' ;;
esac
case ",$(oms_provider_peer_candidates claude '' all | tr '\n' ',')" in
  *,localfoo,*) ;;
  *) fail 'explicit all scope should include the custom adapter' ;;
esac
if oms_provider_peer_candidates claude '' everything >/dev/null 2>&1; then
  fail 'an unknown candidate scope must be refused'
fi

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.name test
git -C "$repo" config user.email test@example.com
printf 'base\n' > "$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" commit -qm base

call_provider() {
  local provider="$1"
  shift
  env -u NVM_DIR HOME="$HOME" PATH="$PATH" OMS_TEST_LOG_DIR="$OMS_TEST_LOG_DIR" \
    OMS_CAPABILITY_DIR="$OMS_CAPABILITY_DIR" OMS_LOCK_DIR="$OMS_LOCK_DIR" \
    OMS_LOCK_FORCE_MKDIR=1 bash "$ROOT/scripts/agent-call.sh" --repo "$repo" \
      --to "$provider" --prompt 'inspect this repository without changing it' "$@" \
      >/dev/null
}

call_provider cursor
grep -Fxq -- '--mode' "$TMP/logs/cursor-agent.argv" || fail 'Cursor read mode missing'
grep -Fxq -- 'ask' "$TMP/logs/cursor-agent.argv" || fail 'Cursor ask mode missing'
grep -Fxq -- '--sandbox' "$TMP/logs/cursor-agent.argv" || fail 'Cursor sandbox missing'
grep -Fxq -- 'enabled' "$TMP/logs/cursor-agent.argv" || fail 'Cursor sandbox mode missing'

call_provider grok
grep -Fxq -- '--permission-mode' "$TMP/logs/grok.argv" || fail 'Grok permission mode missing'
grep -Fxq -- 'dontAsk' "$TMP/logs/grok.argv" || fail 'Grok read policy missing'
grep -Fxq -- 'strict' "$TMP/logs/grok.argv" || fail 'Grok strict sandbox missing'
grep -Fxq -- '--no-auto-update' "$TMP/logs/grok.argv" || fail 'Grok update suppression missing'

call_provider gemini
grep -Fxq -- 'plan' "$TMP/logs/gemini.argv" || fail 'Gemini plan mode missing'
grep -Fxq -- '--sandbox' "$TMP/logs/gemini.argv" || fail 'Gemini sandbox missing'

call_provider qwen
grep -Fxq -- 'plan' "$TMP/logs/qwen.argv" || fail 'Qwen plan mode missing'
grep -Fxq -- '--safe-mode' "$TMP/logs/qwen.argv" || fail 'Qwen clean read context missing'

call_provider opencode --model zai/glm-4.7
grep -Fxq -- 'run' "$TMP/logs/opencode.argv" || fail 'OpenCode run mode missing'
grep -Fxq -- 'plan' "$TMP/logs/opencode.argv" || fail 'OpenCode plan agent missing'
grep -Fxq -- 'zai/glm-4.7' "$TMP/logs/opencode.argv" || fail 'GLM model did not reach OpenCode'

OMS_TEST_ADAPTER_TOUCH=1 call_provider localfoo
grep -Fxq -- '--access' "$TMP/logs/localfoo.argv" || fail 'adapter access contract missing'
grep -Fxq -- 'read' "$TMP/logs/localfoo.argv" || fail 'adapter read contract missing'
grep -Fq 'inspect this repository' "$TMP/logs/localfoo.prompt" || fail 'adapter prompt contract missing'
[ ! -e "$repo/adapter-touch.txt" ] || fail 'custom read adapter touched the source checkout'

# Exercise the write transport constructor directly. The higher-level
# peer-delegate lifecycle is provider-neutral and has its own regression suite;
# this focused test pins the provider-specific flags without spending a model
# call or duplicating the full delegation fixture eight times.
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

prompt_file="$TMP/write-prompt.txt"
printf 'make one bounded test change\n' > "$prompt_file"
run_write_attempt() {
  local provider="$1"
  local output="$TMP/logs/$provider.write-output"
  rm -f "$repo"/*-write.txt
  OMS_TEST_WRITE=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    ma_provider_attempt "$provider" write "$prompt_file" "$output" "$repo" \
      provider-default '' provider-registry '' "write-$provider"
}

for provider in cursor grok gemini qwen opencode; do
  run_write_attempt "$provider" || fail "$provider write transport failed"
done
grep -Fxq -- '--force' "$TMP/logs/cursor-agent.argv" || fail 'Cursor write authority missing'
grep -Fxq -- '--always-approve' "$TMP/logs/grok.argv" || fail 'Grok write authority missing'
grep -Fxq -- 'workspace' "$TMP/logs/grok.argv" || fail 'Grok workspace sandbox missing'
grep -Fxq -- 'yolo' "$TMP/logs/gemini.argv" || fail 'Gemini write authority missing'
grep -Fxq -- 'yolo' "$TMP/logs/qwen.argv" || fail 'Qwen write authority missing'
grep -Fxq -- 'build' "$TMP/logs/opencode.argv" || fail 'OpenCode build agent missing'
grep -Fxq -- '--auto' "$TMP/logs/opencode.argv" || fail 'OpenCode write approval missing'

if OMS_TEST_WRITE=1 ma_provider_attempt localfoo write "$prompt_file" \
  "$TMP/logs/localfoo.refused" "$repo" provider-default '' provider-registry '' custom-refused; then
  fail 'custom write adapter ran without explicit opt-in'
fi
[ ! -e "$repo/localfoo-write.txt" ] || fail 'refused custom adapter changed the repo'
OMS_PROVIDER_WRITE_ADAPTERS=localfoo OMS_TEST_WRITE=1 \
  ma_provider_attempt localfoo write "$prompt_file" "$TMP/logs/localfoo.write-output" \
    "$repo" provider-default '' provider-registry '' custom-write ||
  fail 'custom write adapter opt-in did not reach the transport'
[ -s "$repo/localfoo-write.txt" ] || fail 'custom write adapter did not run after opt-in'

models_json="$(bash "$ROOT/scripts/models.sh" --json)"
OMS_MODELS_JSON="$models_json" python3 - <<'PY' || fail 'models surface omitted detected providers'
import json, os
x = json.loads(os.environ["OMS_MODELS_JSON"])
providers = {row["provider"]: row for row in x["providers"]}
for name in ("codex", "claude", "antigravity", "cursor", "grok", "gemini", "qwen", "opencode", "localfoo"):
    assert name in providers, (name, providers)
for name in ("cursor", "grok", "gemini", "qwen", "opencode", "localfoo"):
    assert providers[name]["present"] is True, (name, providers[name])
PY

doctor_json="$(bash "$ROOT/scripts/model-doctor.sh" --providers auto --json)"
OMS_DOCTOR_JSON="$doctor_json" python3 - <<'PY' || fail 'model-doctor auto detection mismatch'
import json, os
x = json.loads(os.environ["OMS_DOCTOR_JSON"])
names = [row["provider"] for row in x["providers"]]
assert names == ["cursor", "grok", "gemini", "qwen", "opencode", "localfoo"], names
assert all(row["installed"] and row["provider_default_reachable"] for row in x["providers"]), x
PY

OMS_MCP_ROOT="$ROOT" python3 - <<'PY' || fail 'MCP provider routing diverged from the shell registry'
import os, runpy

server = runpy.run_path(os.path.join(os.environ["OMS_MCP_ROOT"], "scripts/oms-mcp-server.py"))
targets, error = server["peer_targets"](
    "grok-build,opencode:model=zai/glm-4.7,localfoo"
)
assert not error, error
assert targets == ["grok", "opencode:model=zai/glm-4.7", "localfoo"], targets
targets, error = server["peer_targets"]("glm")
assert not targets and "registered provider" in error, (targets, error)
PY

echo 'provider-registry-smoke: ok'
