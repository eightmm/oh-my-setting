#!/usr/bin/env bash
set -euo pipefail

# The subscription bridge is a separate wire path; fake RPC/HTTP peers keep it offline.
PYTHONDONTWRITEBYTECODE=1 python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/test_codex_harness.py"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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
  --version|-v|version)
    printf '%s version\n' "$name" >> "$OMS_TEST_LOG_DIR/probes"
    if [ "$name" = cn ]; then printf 'Continue CLI 1.0.0\n'; else printf '%s 1.0.0\n' "$name"; fi
    exit 0
    ;;
  --help|-h)
    printf '%s help\n' "$name" >> "$OMS_TEST_LOG_DIR/probes"
    if [ "$name" = cn ]; then printf 'Continue CLI help\n'; else printf 'usage: %s [headless options]\n' "$name"; fi
    if [ "$name" = goose ]; then printf 'Execute commands from an instruction file or stdin\n'; fi
    if [ "$name" = kilo ]; then printf 'upgrade kilo to the latest or a specific version\n'; fi
    exit 0
    ;;
  run)
    if [ "${2:-}" = --help ]; then
      printf '%s run --help\n' "$name" >> "$OMS_TEST_LOG_DIR/$name.run-probe"
      if [ "$name" = goose ]; then printf 'Goose CLI commands: goose run, goose session\n';
      elif [ "$name" = kilo ]; then printf 'usage: kilo run [options]\n--variant <name>\n';
      else printf 'usage: %s run [options]\n' "$name"; fi
      exit 0
    fi
    ;;
  models|--list-models) printf 'provider/model-a\nprovider/model-b\n'; exit 0 ;;
  --no-auto-update)
    case "${2:-}" in
      version|--version) printf '%s 1.0.0\n' "$name"; exit 0 ;;
      --help) printf 'usage: %s [headless options]\n' "$name"; exit 0 ;;
    esac
    ;;
esac
printf '%s\n' "$@" > "$OMS_TEST_LOG_DIR/$name.argv"
printf '%s\0' "$@" > "$OMS_TEST_LOG_DIR/$name.argv0"
prompt_path=""
previous=""
for argument do
  case "$previous:$argument" in
    --message-file:*|-f:*) prompt_path="$argument" ;;
  esac
  previous="$argument"
done
[ -z "$prompt_path" ] || cat "$prompt_path" > "$OMS_TEST_LOG_DIR/$name.prompt"
printf 'DSH_PERMISSION_MODE=%s\n' "${DSH_PERMISSION_MODE:-}" > "$OMS_TEST_LOG_DIR/$name.env"
printf 'DSH_TELEMETRY_DISABLED=%s\n' "${DSH_TELEMETRY_DISABLED:-}" >> "$OMS_TEST_LOG_DIR/$name.env"
printf 'GOOSE_MODE=%s\n' "${GOOSE_MODE:-}" >> "$OMS_TEST_LOG_DIR/$name.env"
printf 'AUGMENT_DISABLE_AUTO_UPDATE=%s\n' "${AUGMENT_DISABLE_AUTO_UPDATE:-}" >> "$OMS_TEST_LOG_DIR/$name.env"
cat > "$OMS_TEST_LOG_DIR/$name.stdin"
if [ "${OMS_TEST_FAIL_LAUNCH:-0}" = 1 ]; then exit 9; fi
if [ "${OMS_TEST_WRITE:-0}" = 1 ]; then
  printf 'written by %s\n' "$name" > "$PWD/$name-write.txt"
fi
printf '%s returned a complete independent answer with enough detail.\n' "$name"
FAKE
chmod +x "$TMP/bin/provider-fake"
for binary in cursor-agent grok gemini qwen opencode dsh vibe copilot droid aider muse kimi kiro-cli cn goose cline kilo openhands auggie; do
  ln -s provider-fake "$TMP/bin/$binary"
done

# A stale PATH wrapper is discovered but must not enter an automatic pool. Both
# documented no-inference probes hang, so this also pins the wall-clock bound.
cat > "$TMP/bin/pi" <<'BROKEN'
#!/usr/bin/env bash
sleep 30
exit 9
BROKEN
chmod +x "$TMP/bin/pi"

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
export OMS_PROVIDER_PROBE_TIMEOUT=1

# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"
# shellcheck source=scripts/lib/model-capability.sh
. "$ROOT/scripts/lib/model-capability.sh"

[ "$(oms_provider_transport codex)" = cli-exec ] || fail "default CLI transport changed"
for retired_transport in app-server unknown; do
  if OMS_CODEX_TRANSPORT="$retired_transport" oms_provider_transport codex >/dev/null 2>&1; then
    fail "unsupported transport silently fell back to CLI: $retired_transport"
  fi
done
[ "$(OMS_CODEX_TRANSPORT=app-server oms_provider_transport claude)" = cli-exec ] ||
  fail "Codex legacy setting changed Claude transport"

expected_supported='codex
claude
antigravity
cursor
grok
gemini
qwen
opencode
deepseek
vibe
pi
copilot
droid
aider
muse
kimi
kiro
continue
goose
cline
kilo
openhands
auggie'
[ "$(oms_provider_supported_names)" = "$expected_supported" ] ||
  fail "supported provider catalog is incomplete: $(oms_provider_supported_names 2>&1 || true)"

for provider in muse kimi kiro; do
  if oms_provider_supports_access "$provider" read; then
    fail "$provider must not support read calls"
  fi
done
for provider in goose cline kilo openhands auggie; do
  if oms_provider_supports_access "$provider" read; then fail "$provider must not support read calls"; fi
done

cat > "$TMP/bin/unrelated-cn" <<'BAD_CN'
#!/usr/bin/env sh
case "${1:-}" in
  --version) echo 'CodeNode 1.0.0' ;;
  --help) echo 'Continue a previous operation' ;;
esac
BAD_CN
chmod +x "$TMP/bin/unrelated-cn"
rm -f "$TMP/bin/cn"
ln -s unrelated-cn "$TMP/bin/cn"
OMS_PROVIDER_PROBE_CACHE='|'
if oms_provider_cli_available continue; then
  fail 'an unrelated cn executable entered the Continue provider pool'
fi
cat > "$TMP/bin/help-named-cn" <<'HELP_NAMED_CN'
#!/usr/bin/env sh
case "${1:-}" in
  --version) echo 'cn 1.0.0' ;;
  --help) echo 'Continue CLI usage' ;;
esac
HELP_NAMED_CN
chmod +x "$TMP/bin/help-named-cn"
rm -f "$TMP/bin/cn"
ln -s help-named-cn "$TMP/bin/cn"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available continue || fail 'Continue identity probe ignored its help output'
rm -f "$TMP/bin/cn"
ln -s provider-fake "$TMP/bin/cn"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available continue || fail 'Continue identity probe rejected the Continue CLI'

cat > "$TMP/bin/probe-codex" <<'PROBE_CODEX'
#!/usr/bin/env sh
case "${1:-}" in
  --version) echo 'codex version unavailable'; exit 1 ;;
  --help) echo 'Codex top-level help'; exit 0 ;;
  exec) echo 'exec --help' > "$OMS_TEST_LOG_DIR/codex.exec-probe"; exit 0 ;;
esac
exit 1
PROBE_CODEX
chmod +x "$TMP/bin/probe-codex"
ln -sf probe-codex "$TMP/bin/codex"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available codex || fail 'top-level Codex help did not establish usability'
[ ! -e "$TMP/logs/codex.exec-probe" ] ||
  fail 'Codex usability probe invoked operational exec help'
cat > "$TMP/bin/probe-codex-rejected" <<'REJECTED_CODEX'
#!/usr/bin/env sh
case "${1:-}" in
  --version|--help) exit 1 ;;
  exec) echo 'exec --help' > "$OMS_TEST_LOG_DIR/codex.exec-probe"; exit 0 ;;
  app-server) echo invoked > "$OMS_TEST_LOG_DIR/codex.models-probe"; exit 1 ;;
esac
exit 1
REJECTED_CODEX
chmod +x "$TMP/bin/probe-codex-rejected"
ln -sf probe-codex-rejected "$TMP/bin/codex"
rm -f "$TMP/logs/codex.exec-probe"
OMS_PROVIDER_PROBE_CACHE='|'
OMS_CAPABILITY_SKIP_MODELS=1 oms_capability_refresh codex >/dev/null ||
  fail 'Codex capability refresh failed'
[ ! -e "$TMP/logs/codex.exec-probe" ] ||
  fail 'Codex capability probe invoked operational help before identity passed'
unset OMS_CAPABILITY_SKIP_MODELS
OMS_CAPABILITY_RPC_WAIT=0 oms_capability_refresh codex >/dev/null ||
  fail 'Codex capability refresh with model discovery failed'
[ ! -e "$TMP/logs/codex.models-probe" ] ||
  fail 'Codex capability probe invoked app-server before identity passed'
[ "$(oms_capability_read_field "$(oms_capability_file codex)" models_probe)" = skipped ] ||
  fail 'Codex rejected identity did not skip model discovery'
rm -f "$TMP/bin/codex"

cat > "$TMP/bin/probe-opencode-rejected" <<'REJECTED_OPENCODE'
#!/usr/bin/env sh
case "${1:-}" in
  --version|--help) exit 1 ;;
  models) echo invoked > "$OMS_TEST_LOG_DIR/opencode.models-probe"; exit 1 ;;
esac
exit 1
REJECTED_OPENCODE
chmod +x "$TMP/bin/probe-opencode-rejected"
ln -sf probe-opencode-rejected "$TMP/bin/opencode"
OMS_PROVIDER_PROBE_CACHE='|'
printf 'supplied help\n' > "$TMP/supplied-help"
oms_capability_refresh opencode "$TMP/supplied-help" >/dev/null ||
  fail 'OpenCode capability refresh failed'
[ ! -e "$TMP/logs/opencode.models-probe" ] ||
  fail 'OpenCode capability probe invoked models before identity passed'
[ "$(oms_capability_read_field "$(oms_capability_file opencode)" models_probe)" = skipped ] ||
  fail 'OpenCode rejected identity did not skip model discovery'
ln -sf provider-fake "$TMP/bin/opencode"
OMS_PROVIDER_PROBE_CACHE='|'

cat > "$TMP/bin/probe-grok" <<'PROBE_GROK'
#!/usr/bin/env sh
case "$*" in
  '--no-auto-update version') exit 1 ;;
  '--no-auto-update --help') echo 'Grok protected top-level help'; exit 0 ;;
  --help) echo 'bare help invoked' > "$OMS_TEST_LOG_DIR/grok.bare-help-probe"; exit 0 ;;
esac
exit 1
PROBE_GROK
chmod +x "$TMP/bin/probe-grok"
ln -sf probe-grok "$TMP/bin/grok"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available grok || fail 'Grok protected top-level help did not establish usability'
[ ! -e "$TMP/logs/grok.bare-help-probe" ] ||
  fail 'Grok usability probe invoked help without update suppression'
ln -sf provider-fake "$TMP/bin/grok"

cat > "$TMP/bin/unrelated-goose" <<'BAD_GOOSE'
#!/usr/bin/env sh
case "${1:-}" in
  --version) echo 'Goose version 1.0.0' ;;
  --help) echo 'usage: goose [options]' ;;
  run) echo 'goose run [options]' > "$OMS_TEST_LOG_DIR/unrelated-goose.run-probe" ;;
esac
BAD_GOOSE
chmod +x "$TMP/bin/unrelated-goose"
rm -f "$TMP/bin/goose"
ln -s unrelated-goose "$TMP/bin/goose"
OMS_PROVIDER_PROBE_CACHE='|'
if oms_provider_cli_available goose; then
  fail 'an unrelated goose executable entered the provider pool'
fi
if [ -e "$TMP/logs/unrelated-goose.run-probe" ]; then
  fail 'Goose identity probe invoked the operational run subcommand'
fi
OMS_PROVIDER_PROBE_CACHE='|'
oms_capability_refresh goose >/dev/null || fail 'Goose capability refresh failed'
if [ -e "$TMP/logs/unrelated-goose.run-probe" ]; then
  fail 'Goose capability probe invoked run before identity passed'
fi
ln -sf provider-fake "$TMP/bin/goose"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available goose || fail 'Goose identity probe rejected its CLI'
if [ -e "$TMP/logs/goose.run-probe" ]; then
  fail 'Goose identity probe invoked the operational run subcommand'
fi

cat > "$TMP/bin/unrelated-kilo" <<'BAD_KILO'
#!/usr/bin/env sh
case "${1:-}" in
  --version) echo 'kilo version 1.0.0' ;;
  --help) echo 'usage: kilo [options]' ;;
  run) echo 'kilo run [options]' > "$OMS_TEST_LOG_DIR/unrelated-kilo.run-probe" ;;
esac
BAD_KILO
chmod +x "$TMP/bin/unrelated-kilo"
rm -f "$TMP/bin/kilo"
ln -s unrelated-kilo "$TMP/bin/kilo"
OMS_PROVIDER_PROBE_CACHE='|'
if oms_provider_cli_available kilo; then
  fail 'an unrelated kilo executable entered the provider pool'
fi
if [ -e "$TMP/logs/unrelated-kilo.run-probe" ]; then
  fail 'Kilo identity probe invoked the operational run subcommand'
fi
OMS_PROVIDER_PROBE_CACHE='|'
oms_capability_refresh kilo >/dev/null || fail 'Kilo capability refresh failed'
if [ -e "$TMP/logs/unrelated-kilo.run-probe" ]; then
  fail 'Kilo capability probe invoked run before identity passed'
fi
ln -sf provider-fake "$TMP/bin/kilo"
OMS_PROVIDER_PROBE_CACHE='|'
oms_provider_cli_available kilo || fail 'Kilo identity probe rejected its CLI'
if [ -e "$TMP/logs/kilo.run-probe" ]; then
  fail 'Kilo identity probe invoked the operational run subcommand'
fi
OMS_PROVIDER_PROBE_CACHE='|'
oms_capability_refresh kilo >/dev/null || fail 'Kilo capability refresh failed'
[ -e "$TMP/logs/kilo.run-probe" ] ||
  fail 'Kilo effort probe did not run after its identity passed'

oms_provider_cli_discovered pi || fail 'the stale Pi wrapper should remain visible to diagnostics'
if oms_provider_cli_available pi; then
  fail 'a hanging PATH wrapper must not enter the usable provider pool'
fi

for pair in 'muse muse' 'kimi kimi' 'kiro kiro-cli' 'continue cn' 'goose goose' 'cline cline' 'kilo kilo' 'openhands openhands' 'auggie auggie'; do
  set -- $pair
  oms_provider_cli_discovered "$1" || fail "$2 should be discovered from PATH"
  rm -f "$TMP/bin/$2"
  if oms_provider_cli_discovered "$1"; then fail "$2 should be absent when removed from PATH"; fi
  ln -s provider-fake "$TMP/bin/$2"
done

broken_json="$(bash "$ROOT/scripts/models.sh" --providers all --json)"
OMS_MODELS_JSON="$broken_json" python3 - <<'PY' || fail 'models hid the discovered-but-broken provider'
import json, os
providers = {row["provider"]: row for row in json.loads(os.environ["OMS_MODELS_JSON"])["providers"]}
assert providers["pi"]["present"] is True, providers["pi"]
assert providers["pi"]["usable"] is None, providers["pi"]
PY

set +e
broken_refresh_json="$(bash "$ROOT/scripts/models.sh" --providers auto --refresh --json)"
broken_refresh_rc=$?
broken_doctor_json="$(bash "$ROOT/scripts/model-doctor.sh" --providers auto --json)"
broken_doctor_rc=$?
set -e
[ "$broken_refresh_rc" -eq 0 ] || fail 'models refresh should report, not hide, a broken provider'
[ "$broken_doctor_rc" -eq 1 ] || fail 'model-doctor should fail on a discovered broken provider'
OMS_MODELS_JSON="$broken_refresh_json" OMS_DOCTOR_JSON="$broken_doctor_json" python3 - <<'PY' || fail 'refreshed diagnostics hid the discovered-but-broken provider'
import json, os
models = {row["provider"]: row for row in json.loads(os.environ["OMS_MODELS_JSON"])["providers"]}
doctor = {row["provider"]: row for row in json.loads(os.environ["OMS_DOCTOR_JSON"])["providers"]}
assert models["pi"]["present"] is True and models["pi"]["usable"] is False, models["pi"]
assert doctor["pi"]["installed"] is True and doctor["pi"]["usable"] is False, doctor["pi"]
PY

rm -f "$TMP/bin/pi"
ln -s provider-fake "$TMP/bin/pi"
OMS_PROVIDER_PROBE_CACHE='|'


for pair in \
  'agy antigravity' \
  'cursor-agent cursor' \
  'grok-build grok' \
  'gemini-cli gemini' \
  'qwen-code qwen' \
  'opencode2 opencode' \
  'dsh deepseek' \
  'deepseek-harness deepseek' \
  'mistral-vibe vibe' \
  'github-copilot copilot' \
  'factory-droid droid' \
  'kiro-cli kiro' \
  'cn continue' \
  'goose goose' \
  'cline cline' \
  'kilo kilo' \
  'openhands openhands' \
  'auggie auggie' \
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
[ "$(oms_provider_model_family muse provider-default)" = meta ] || fail 'Muse family'
[ "$(oms_provider_model_family kimi provider-default)" = moonshot ] || fail 'Kimi family'
[ "$(oms_provider_model_family kiro openai/gpt-5.6-sol)" = openai ] || fail 'Kiro model family inference'
[ -z "$(oms_provider_primary_family kiro)" ] || fail 'Kiro is a multi-family carrier'
[ "$(oms_provider_model_family continue anthropic/claude-sonnet-5)" = anthropic ] || fail 'Continue model family inference'
[ "$(oms_provider_model_family kiro minimax-m2.5)" = minimax ] || fail 'Kiro open-weight model family inference'
[ "$(oms_provider_primary_family muse)" = meta ] || fail 'Muse primary family'
[ "$(oms_provider_primary_family kimi)" = moonshot ] || fail 'Kimi primary family'
[ "$(oms_provider_effort_values pi)" = 'low medium high xhigh max' ] ||
  fail 'Pi effort scale must include the documented max level'

for provider in cursor grok gemini qwen opencode deepseek vibe pi copilot droid aider continue; do
  oms_provider_supports_access "$provider" read || fail "$provider should support read calls"
  oms_provider_supports_access "$provider" write || fail "$provider should support isolated writes"
done
for provider in muse kimi kiro goose cline kilo openhands auggie; do
  if oms_provider_supports_access "$provider" read; then fail "$provider must not support read calls"; fi
  oms_provider_supports_access "$provider" write || fail "$provider should support writes"
  if oms_provider_requires_read_isolation "$provider"; then
    fail "$provider should be refused reads instead of isolated"
  fi
done
oms_provider_requires_read_isolation aider ||
  fail 'Aider read calls must isolate provider-created repo-map state'
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
for provider in muse kimi kiro goose cline kilo openhands auggie; do
  case ",$(oms_provider_peer_candidates claude "$provider" | tr '\n' ',')" in
    *,"$provider",*) fail "$provider was selected as a preferred read seat" ;;
  esac
  case ",$(oms_provider_peer_candidates "$provider" '' all | tr '\n' ',')" in
    *,"$provider",*) fail "$provider was selected as a read-seat fallback" ;;
  esac
done
(
  oms_provider_cli_available() { return 1; }
  [ -z "$(oms_provider_peer_candidates kimi)" ] ||
    fail 'a write-only caller was used as the last-resort read seat'
)
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

# Prompt construction may read cached catalogs but must not execute provider
# CLIs. Routing performs its own bounded usability decision at call time.
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT/scripts/lib/peer-common.sh"
rm -f "$TMP/logs/probes"
ma_write_harness_context "$repo" 0 0 0 >/dev/null
[ ! -e "$TMP/logs/probes" ] || fail 'cached harness context invoked provider probes'

for access in read write; do
  (
    oms_provider_cli_discovered() { fail 'retired transport reached a provider probe'; }
    rc=0
    OMS_CODEX_TRANSPORT=app-server ma_provider_attempt codex "$access" \
      "$repo/README.md" "$TMP/retired-output" "$repo" provider-default "" \
      retired "$repo" "retired-$access" 2>/dev/null || rc=$?
    [ "$rc" = 2 ] || fail "retired transport did not fail closed for $access"
  )
done

read_refusal_prompt="$TMP/read-refusal-prompt"
printf 'Review without changing anything\n' > "$read_refusal_prompt"
for pair in 'muse muse' 'kimi kimi' 'kiro kiro-cli' 'goose goose' 'cline cline' 'kilo kilo' 'openhands openhands' 'auggie auggie'; do
  set -- $pair
  rm -f "$TMP/logs/probes" "$TMP/logs/$2.argv"
  rc=0
  ma_provider_attempt "$1" read "$read_refusal_prompt" "$TMP/logs/$1.read-refused" \
    "$TMP" provider-default '' provider-registry '' "refused-read-$1" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || fail "$1 read call was not refused before launch"
  grep -Fq "provider '$1' does not support read access" "$TMP/logs/$1.read-refused" ||
    fail "$1 read refusal did not use the typed access error"
  [ ! -e "$TMP/logs/probes" ] || fail "$1 read refusal ran a CLI probe"
  [ ! -e "$TMP/logs/$2.argv" ] || fail "$1 read refusal launched the CLI"
done

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

call_provider deepseek
grep -Fxq -- '--profile' "$TMP/logs/dsh.argv" || fail 'DeepSeek profile flag missing'
grep -Fxq -- 'headless' "$TMP/logs/dsh.argv" || fail 'DeepSeek headless profile missing'
grep -Fxq -- 'DSH_PERMISSION_MODE=read-only' "$TMP/logs/dsh.env" ||
  fail 'DeepSeek read sandbox missing'
grep -Fxq -- 'DSH_TELEMETRY_DISABLED=1' "$TMP/logs/dsh.env" ||
  fail 'DeepSeek invocation telemetry was not disabled'

call_provider vibe
grep -Fxq -- '--agent' "$TMP/logs/vibe.argv" || fail 'Vibe agent mode missing'
grep -Fxq -- 'plan' "$TMP/logs/vibe.argv" || fail 'Vibe read-only plan agent missing'
grep -Fxq -- '--trust' "$TMP/logs/vibe.argv" || fail 'Vibe temporary worktree trust missing'
grep -Fxq -- 'read_file' "$TMP/logs/vibe.argv" || fail 'Vibe read tool missing'
if grep -Fxq -- 'bash' "$TMP/logs/vibe.argv"; then fail 'Vibe read call exposed bash'; fi

call_provider pi
grep -Fxq -- '--no-session' "$TMP/logs/pi.argv" || fail 'Pi ephemeral mode missing'
grep -Fxq -- '--no-approve' "$TMP/logs/pi.argv" || fail 'Pi project resource fence missing'
grep -Fxq -- 'read,grep,find,ls' "$TMP/logs/pi.argv" || fail 'Pi read tool belt missing'

call_provider copilot
grep -Fxq -- '--no-ask-user' "$TMP/logs/copilot.argv" || fail 'Copilot noninteractive mode missing'
grep -Fxq -- '--disable-builtin-mcps' "$TMP/logs/copilot.argv" || fail 'Copilot MCP isolation missing'
grep -Fxq -- 'view,grep,glob' "$TMP/logs/copilot.argv" || fail 'Copilot read tool belt missing'

call_provider droid
grep -Fxq -- 'exec' "$TMP/logs/droid.argv" || fail 'Droid exec mode missing'
grep -Fxq -- '--cwd' "$TMP/logs/droid.argv" || fail 'Droid cwd fence missing'
if grep -Fxq -- '--auto' "$TMP/logs/droid.argv"; then fail 'Droid read call widened autonomy'; fi

call_provider aider
grep -Fxq -- 'ask' "$TMP/logs/aider.argv" || fail 'Aider ask mode missing'
grep -Fxq -- '--dry-run' "$TMP/logs/aider.argv" || fail 'Aider read dry-run missing'
grep -Fxq -- '--no-auto-commits' "$TMP/logs/aider.argv" || fail 'Aider commit fence missing'
grep -Fxq -- '--no-analytics' "$TMP/logs/aider.argv" || fail 'Aider telemetry fence missing'
grep -Fxq -- '--env-file' "$TMP/logs/aider.argv" || fail 'Aider explicit environment overlay missing'
grep -Fxq -- '--input-history-file' "$TMP/logs/aider.argv" || fail 'Aider input history isolation missing'
grep -Fxq -- '--chat-history-file' "$TMP/logs/aider.argv" || fail 'Aider chat history isolation missing'
aider_scratch="$(awk 'previous == "--env-file" { print; exit } { previous=$0 }' "$TMP/logs/aider.argv")"
[ -n "$aider_scratch" ] || fail 'Aider scratch path missing'
aider_scratch="$(dirname "$aider_scratch")"
[ ! -e "$aider_scratch" ] || fail 'Aider private invocation state was not cleaned up'

call_provider continue
grep -Fxq -- '--readonly' "$TMP/logs/cn.argv" || fail 'Continue read-only mode missing'
for binary in muse kimi kiro-cli cn; do
  [ ! -s "$TMP/logs/$binary.stdin" ] || fail "$binary received a duplicate prompt on stdin"
done

for binary in dsh vibe pi copilot droid aider; do
  [ ! -s "$TMP/logs/$binary.stdin" ] ||
    fail "$binary received the prompt twice through its documented argv/file input and stdin"
done

if call_provider deepseek --model deepseek-v4 >/dev/null 2>&1; then
  fail 'DeepSeek headless CLI has no documented exact-model flag'
fi
if call_provider vibe --model mistral-large-latest >/dev/null 2>&1; then
  fail 'Vibe programmatic CLI has no documented exact-model flag'
fi

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

for provider in cursor grok gemini qwen opencode deepseek vibe pi copilot droid aider muse kimi kiro continue goose cline kilo openhands auggie; do
  run_write_attempt "$provider" || fail "$provider write transport failed"
done
assert_argv() {
  local binary="$1"
  shift
  printf '%s\n' "$@" > "$TMP/expected.argv"
  cmp -s "$TMP/expected.argv" "$TMP/logs/$binary.argv" ||
    fail "$binary argv mismatch: $(tr '\n' ' ' < "$TMP/logs/$binary.argv")"
}
run_exact() {
  local provider="$1" access="$2" model="$3" effort="$4"
  rm -f "$repo"/*-write.txt
  OMS_TEST_WRITE="$([ "$access" = write ] && printf 1 || printf 0)" \
    MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    ma_provider_attempt "$provider" "$access" "$prompt_file" \
      "$TMP/logs/$provider.exact-output" "$repo" "$model" "$effort" \
      provider-registry '' "exact-$provider-$access" >/dev/null 2>&1 ||
      fail "$provider $access exact argv call failed"
}
run_exact continue read provider-default ''
assert_argv cn -p 'make one bounded test change' --readonly
run_exact continue read continue-model ''
assert_argv cn -p 'make one bounded test change' --model continue-model --readonly
run_exact muse write provider-default ''
assert_argv muse exec --disable-approval 'make one bounded test change'
run_exact muse write meta-model ''
assert_argv muse exec --model meta-model --disable-approval 'make one bounded test change'
run_exact muse write meta-model high
assert_argv muse exec --model meta-model --reasoning-effort high --disable-approval 'make one bounded test change'
run_exact kimi write provider-default ''
assert_argv kimi -p 'make one bounded test change'
run_exact kimi write moonshot-model ''
assert_argv kimi --model moonshot-model -p 'make one bounded test change'
run_exact kiro write provider-default ''
assert_argv kiro-cli chat --v3 --no-interactive --trust-all-tools 'make one bounded test change'
run_exact kiro write model-id ''
assert_argv kiro-cli chat --v3 --no-interactive --model model-id --trust-all-tools 'make one bounded test change'
run_exact kiro write model-id high
assert_argv kiro-cli chat --v3 --no-interactive --model model-id --effort high --trust-all-tools 'make one bounded test change'
run_exact continue write provider-default ''
assert_argv cn -p 'make one bounded test change' --auto
run_exact continue write continue-model ''
assert_argv cn -p 'make one bounded test change' --model continue-model --auto
run_exact goose write provider-default ''
assert_argv goose run --no-session -t 'make one bounded test change'
grep -Fxq 'GOOSE_MODE=auto' "$TMP/logs/goose.env" || fail 'Goose auto mode was not forced for writes'
run_exact goose write openai/model-x ''
assert_argv goose run --no-session --model openai/model-x -t 'make one bounded test change'
run_exact cline write provider-default ''
assert_argv cline --auto-approve true 'make one bounded test change'
run_exact cline write model-x ''
assert_argv cline --model model-x --auto-approve true 'make one bounded test change'
run_exact cline write model-x high
assert_argv cline --model model-x --thinking high --auto-approve true 'make one bounded test change'
run_exact kilo write provider-default ''
assert_argv kilo run --auto 'make one bounded test change'
run_exact kilo write provider-x/model-y ''
assert_argv kilo run --auto --model provider-x/model-y 'make one bounded test change'
run_exact kilo write provider-x/model-y high
assert_argv kilo run --auto --model provider-x/model-y --variant high 'make one bounded test change'
run_exact openhands write provider-default ''
assert_argv openhands --headless -t 'make one bounded test change'
run_exact auggie write provider-default ''
assert_argv auggie --print --permission edit:allow --permission write:allow --permission terminal:allow 'make one bounded test change'
[ "$(tail -n 1 "$TMP/logs/auggie.env")" = 'AUGMENT_DISABLE_AUTO_UPDATE=1' ] || fail 'Auggie auto-update was not suppressed'
run_exact auggie write model-x ''
assert_argv auggie --print --model model-x --permission edit:allow --permission write:allow --permission terminal:allow 'make one bounded test change'
run_leading_dash_prompt_case() {
  local provider="$1" binary="$2" out="$TMP/logs/$1.leading-dash" rc=0
  local dash_prompt="$TMP/prompt-leading-dash"
  local argv_log="$TMP/logs/$binary.argv"
  local stdin_log="$TMP/logs/$binary.stdin"
  local baseline_log="$TMP/logs/$binary.leading-dash-baseline.argv0"
  local temp_dir="$TMP/agent-temps"
  mkdir -p "$temp_dir"
  local before=""
  before="$(find "$temp_dir" -name 'oms.*' -type f -print | LC_ALL=C sort)"
  printf '%s' 'leading dash baseline' > "$dash_prompt"
  MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    OMS_LIB_TMPDIR="$temp_dir" ma_provider_attempt "$provider" write "$dash_prompt" "$out" "$repo" \
      provider-default '' provider-registry '' "dash-baseline-$provider" >/dev/null 2>&1 ||
      fail "$provider baseline launch failed"
  cp "$TMP/logs/$binary.argv0" "$baseline_log"
  printf '%s' '--help' > "$dash_prompt"
  MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    OMS_LIB_TMPDIR="$temp_dir" ma_provider_attempt "$provider" write "$dash_prompt" "$out" "$repo" \
      provider-default '' provider-registry '' "dash-$provider" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 0 ] || fail "$provider refused a leading-dash positional prompt (exit $rc)"
  [ -e "$argv_log" ] || fail "$provider did not launch for a leading-dash prompt"
  python3 - "$provider" "$argv_log" "$baseline_log" "$stdin_log" "$dash_prompt" <<'PY'
import pathlib, sys
provider, argv_path, baseline_path, stdin_path, prompt_path = sys.argv[1:]
argv = pathlib.Path(argv_path.replace(".argv", ".argv0")).read_bytes().split(b"\0")[:-1]
baseline = pathlib.Path(baseline_path).read_bytes().split(b"\0")[:-1]
stdin_file = pathlib.Path(stdin_path)
stdin = stdin_file.read_bytes() if stdin_file.exists() else b""
prompt = pathlib.Path(prompt_path).read_bytes()
normalized = b"\n" + prompt
argv_transports = {"antigravity", "grok", "deepseek", "vibe", "pi", "copilot",
                   "muse", "kimi", "kiro", "continue", "goose", "cline",
                   "kilo", "openhands", "auggie"}
file_transports = {"droid", "aider"}
null_stdin = {"deepseek", "vibe", "pi", "copilot", "droid", "aider", "muse",
              "kimi", "kiro", "continue", "goose", "cline", "kilo", "openhands", "auggie"}
if provider in argv_transports:
    assert argv == [normalized if item == b"leading dash baseline" else item for item in baseline], (provider, argv, baseline)
    if provider == "continue":
        assert argv == [b"-p", normalized, b"--auto"], argv
    if provider in null_stdin:
        assert stdin == b"", (provider, stdin)
elif provider in file_transports:
    assert pathlib.Path(str(argv_path).replace(".argv", ".prompt")).read_bytes() == normalized
    assert stdin == b"", (provider, stdin)
elif provider == "localfoo":
    assert pathlib.Path(prompt_path).read_bytes() == prompt
    assert stdin == normalized, (provider, stdin)
else:
    assert stdin == normalized, (provider, stdin)
    assert provider not in null_stdin, (provider, stdin)
PY
  [ "$(find "$temp_dir" -name 'oms.*' -type f -print | LC_ALL=C sort)" = "$before" ] ||
    fail "$provider left a normalized prompt copy after success"
  if [ "$provider" = continue ]; then
    printf '%s' '-p' > "$dash_prompt"
    rc=0
    MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
      MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
      OMS_LIB_TMPDIR="$temp_dir" ma_provider_attempt "$provider" write "$dash_prompt" "$out" "$repo" \
        provider-default '' provider-registry '' "dash-option-$provider" >/dev/null 2>&1 || rc=$?
    [ "$rc" -eq 0 ] || fail "$provider refused an option-shaped prompt"
    python3 - "$argv_log" <<'PY'
import pathlib, sys
assert pathlib.Path(sys.argv[1].replace(".argv", ".argv0")).read_bytes().split(b"\0")[:-1] == [b"-p", b"\n-p", b"--auto"]
PY
    [ "$(find "$temp_dir" -name 'oms.*' -type f -print | LC_ALL=C sort)" = "$before" ] ||
      fail "$provider left a normalized prompt copy after option prompt"
    local copy_error_out="$TMP/logs/$provider.copy-allocation-error"
    rc=0
    (
      set -u
      agent_memory_mktemp() { return 1; }
      OMS_PROVIDER_ERROR_CLASSIFICATION=stale
      OMS_PROVIDER_ERROR_EXIT=stale
      OMS_PROVIDER_ERROR_PROVIDER=stale
      export OMS_PROVIDER_ERROR_CLASSIFICATION OMS_PROVIDER_ERROR_EXIT OMS_PROVIDER_ERROR_PROVIDER
      ma_provider_attempt "$provider" write "$dash_prompt" "$copy_error_out" "$repo" \
        provider-default '' provider-registry '' "dash-copy-allocation-$provider" || rc=$?
      [ "$rc" -eq 2 ] || fail "$provider copy allocation failure returned $rc, expected 2"
      grep -Fq 'could not prepare a leading-dash prompt copy' "$copy_error_out" ||
        fail "$provider copy allocation failure omitted its diagnostic"
      [ "${OMS_PROVIDER_ERROR_CLASSIFICATION+x}" = x ] &&
        [ "${OMS_PROVIDER_ERROR_EXIT+x}" = x ] &&
        [ "${OMS_PROVIDER_ERROR_PROVIDER+x}" = x ] ||
        fail "$provider copy allocation failure left an error global unset"
      [ -z "$OMS_PROVIDER_ERROR_CLASSIFICATION" ] &&
        [ -z "$OMS_PROVIDER_ERROR_EXIT" ] &&
        [ -z "$OMS_PROVIDER_ERROR_PROVIDER" ] ||
        fail "$provider copy allocation failure left stale error globals"
    ) || fail "$provider copy allocation failure case failed"
    printf '%s' '--help' > "$dash_prompt"
    rc=0
    OMS_TEST_FAIL_LAUNCH=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
      MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
      OMS_LIB_TMPDIR="$temp_dir" ma_provider_attempt "$provider" write "$dash_prompt" "$out" "$repo" \
        provider-default '' provider-registry '' "dash-failure-$provider" >/dev/null 2>&1 || rc=$?
    [ "$rc" -ne 0 ] || fail "$provider failed-launch fixture returned success"
    [ "$(find "$temp_dir" -name 'oms.*' -type f -print | LC_ALL=C sort)" = "$before" ] ||
      fail "$provider left a normalized prompt copy after failed launch"
  fi
}
created_binaries=""
for provider in $(oms_provider_supported_names); do
  binary="$(oms_provider_binary "$provider")"
  [ -n "$binary" ] || fail "$provider has no binary for leading-dash test"
  if [ ! -e "$TMP/bin/$binary" ] && [ ! -L "$TMP/bin/$binary" ]; then
    ln -s provider-fake "$TMP/bin/$binary"
    created_binaries="$created_binaries$binary "
  fi
  run_leading_dash_prompt_case "$provider" "$binary"
done
for binary in $created_binaries; do rm -f "$TMP/bin/$binary"; done
run_prompt_limit_case() {
  local provider="$1" binary="$2" size="$3"
  local out="$TMP/logs/$provider.limit-$size" rc=0
  local limit_prompt="$TMP/prompt-limit"
  if [ "$size" -eq 0 ]; then
    : > "$limit_prompt"
  else
    awk -v bytes="$size" 'BEGIN { for (i = 0; i < bytes; i++) printf "x" }' > "$limit_prompt"
  fi
  rm -f "$TMP/logs/$binary.argv"
  OMS_TEST_WRITE=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    ma_provider_attempt "$provider" write "$limit_prompt" "$out" "$repo" \
      provider-default '' provider-registry '' "size-$provider-$size" >/dev/null 2>&1 || rc=$?
  if [ "$size" -gt 120000 ]; then
    [ "$rc" -eq 2 ] || fail "$provider accepted an oversized argv prompt"
    grep -Fq 'prompt is 120001B' "$out" || fail "$provider oversized-prompt error omitted its size"
    [ ! -e "$TMP/logs/$binary.argv" ] || fail "$provider launched on an oversized prompt"
  else
    [ "$rc" -eq 0 ] || fail "$provider refused a prompt of $size bytes"
    [ -e "$TMP/logs/$binary.argv" ] || fail "$provider did not launch at $size bytes"
  fi
}
for pair in 'muse muse' 'kimi kimi' 'kiro kiro-cli' 'continue cn' 'goose goose' 'cline cline' 'kilo kilo' 'openhands openhands' 'auggie auggie'; do
  set -- $pair
  run_prompt_limit_case "$1" "$2" 0
  run_prompt_limit_case "$1" "$2" 120000
  run_prompt_limit_case "$1" "$2" 120001
done
grep -Fxq -- '--force' "$TMP/logs/cursor-agent.argv" || fail 'Cursor write authority missing'

# Codex judging seats disable web search and the apps connector route;
# researcher seats enable web search while keeping apps disabled; write seats
# set neither read-seat flag.
mkdir -p "$TMP/codexbin"
ln -s ../bin/provider-fake "$TMP/codexbin/codex"
for access in read write; do
  PATH="$TMP/codexbin:$PATH" OMS_TEST_WRITE=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
    MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' \
    OMS_PANEL_ROLE=reviewer \
    ma_provider_attempt codex "$access" "$prompt_file" "$TMP/logs/codex.$access-output" "$repo" \
      provider-default '' provider-registry '' "codex-$access" >/dev/null 2>&1 || true
  cp "$TMP/logs/codex.argv" "$TMP/logs/codex.$access.argv" 2>/dev/null || fail "codex $access seat never reached the CLI"
done
grep -Fxq -- 'web_search="disabled"' "$TMP/logs/codex.read.argv" || fail 'codex read seat leaves web_search on'
grep -Fxq -- 'features.apps=false' "$TMP/logs/codex.read.argv" || fail 'codex read seat leaves apps connectors on'
if grep -Eq -- 'web_search|features\.apps' "$TMP/logs/codex.write.argv"; then fail 'codex write seat must not carry the read-seat web/apps flags'; fi
PATH="$TMP/codexbin:$PATH" OMS_TEST_WRITE=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
  MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' OMS_PANEL_ROLE=researcher \
  ma_provider_attempt codex read "$prompt_file" "$TMP/logs/codex.ambient-output" "$repo" \
    provider-default '' provider-registry '' codex-ambient >/dev/null 2>&1 || true
# A researcher role left in the environment is not a researcher dispatch.
grep -Fxq -- 'web_search="disabled"' "$TMP/logs/codex.argv" ||
  fail 'an ambient researcher role outside a panel dispatch enabled web search'
PATH="$TMP/codexbin:$PATH" OMS_TEST_WRITE=1 MA_KIND=delegate MA_SHOW_REPO=0 MA_QUORUM_FALLBACK=answer \
  MA_DEBATE_ROLE=workers MA_DEBATE_TOPIC=task MA_DEBATE_SECTIONS='' OMS_PANEL_ROLE=researcher OMS_PANEL_DISPATCH=1 \
  ma_provider_attempt codex read "$prompt_file" "$TMP/logs/codex.researcher-output" "$repo" \
    provider-default '' provider-registry '' codex-researcher >/dev/null 2>&1 || true
cp "$TMP/logs/codex.argv" "$TMP/logs/codex.researcher.argv" || fail 'codex researcher seat never reached the CLI'
if grep -Fq -- 'web_search="disabled"' "$TMP/logs/codex.researcher.argv"; then
  fail 'codex researcher seat disables web search'
fi
grep -Fxq -- 'web_search="live"' "$TMP/logs/codex.researcher.argv" ||
  fail 'codex researcher seat must use live web search'
grep -Fxq -- 'features.apps=false' "$TMP/logs/codex.researcher.argv" ||
  fail 'codex researcher seat must keep apps connectors disabled'
grep -Fxq -- '--always-approve' "$TMP/logs/grok.argv" || fail 'Grok write authority missing'
grep -Fxq -- 'workspace' "$TMP/logs/grok.argv" || fail 'Grok workspace sandbox missing'
grep -Fxq -- 'yolo' "$TMP/logs/gemini.argv" || fail 'Gemini write authority missing'
grep -Fxq -- 'yolo' "$TMP/logs/qwen.argv" || fail 'Qwen write authority missing'
grep -Fxq -- 'build' "$TMP/logs/opencode.argv" || fail 'OpenCode build agent missing'
grep -Fxq -- '--auto' "$TMP/logs/opencode.argv" || fail 'OpenCode write approval missing'
grep -Fxq -- 'DSH_PERMISSION_MODE=workspace-write' "$TMP/logs/dsh.env" ||
  fail 'DeepSeek write sandbox missing'
grep -Fxq -- 'accept-edits' "$TMP/logs/vibe.argv" || fail 'Vibe edit-only agent missing'
grep -Fxq -- 'write_file' "$TMP/logs/vibe.argv" || fail 'Vibe write tool missing'
grep -Fxq -- 'edit' "$TMP/logs/vibe.argv" || fail 'Vibe patch tool missing'
if grep -Fxq -- 'search_replace' "$TMP/logs/vibe.argv"; then
  fail 'Vibe received its removed search_replace tool name'
fi
if grep -Fxq -- 'bash' "$TMP/logs/vibe.argv"; then fail 'Vibe write call exposed bash'; fi
grep -Fxq -- 'read,edit,write,grep,find,ls' "$TMP/logs/pi.argv" || fail 'Pi write tool belt missing'
grep -Fxq -- 'view,grep,glob,edit,create,apply_patch' "$TMP/logs/copilot.argv" ||
  fail 'Copilot write tool belt missing'
grep -Fxq -- 'read,write' "$TMP/logs/copilot.argv" || fail 'Copilot write approval missing'
grep -Fxq -- '--auto' "$TMP/logs/droid.argv" || fail 'Droid write autonomy missing'
grep -Fxq -- 'low' "$TMP/logs/droid.argv" || fail 'Droid write autonomy is too broad'
grep -Fxq -- 'code' "$TMP/logs/aider.argv" || fail 'Aider code mode missing'
grep -Fxq -- '--yes-always' "$TMP/logs/aider.argv" || fail 'Aider noninteractive write approval missing'
if grep -Fxq -- '--yes' "$TMP/logs/aider.argv"; then
  fail 'Aider transport used the deprecated abbreviated approval flag'
fi

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

models_json="$(bash "$ROOT/scripts/models.sh" --refresh --json)"
OMS_MODELS_JSON="$models_json" python3 - <<'PY' || fail 'models surface omitted detected providers'
import json, os
x = json.loads(os.environ["OMS_MODELS_JSON"])
providers = {row["provider"]: row for row in x["providers"]}
for name in ("codex", "claude", "antigravity", "cursor", "grok", "gemini", "qwen", "opencode", "deepseek", "vibe", "pi", "copilot", "droid", "aider", "muse", "kimi", "kiro", "continue", "goose", "cline", "kilo", "openhands", "auggie", "localfoo"):
    assert name in providers, (name, providers)
for name in ("cursor", "grok", "gemini", "qwen", "opencode", "deepseek", "vibe", "pi", "copilot", "droid", "aider", "muse", "kimi", "kiro", "continue", "goose", "cline", "kilo", "openhands", "auggie", "localfoo"):
    assert providers[name]["present"] is True, (name, providers[name])
    assert providers[name]["usable"] is True, (name, providers[name])
PY

doctor_json="$(bash "$ROOT/scripts/model-doctor.sh" --providers auto --json)"
OMS_DOCTOR_JSON="$doctor_json" python3 - <<'PY' || fail 'model-doctor auto detection mismatch'
import json, os
x = json.loads(os.environ["OMS_DOCTOR_JSON"])
names = [row["provider"] for row in x["providers"]]
assert names == ["cursor", "grok", "gemini", "qwen", "opencode", "deepseek", "vibe", "pi", "copilot", "droid", "aider", "muse", "kimi", "kiro", "continue", "goose", "cline", "kilo", "openhands", "auggie", "localfoo"], names
assert all(row["installed"] and row["usable"] and row["provider_default_reachable"] for row in x["providers"]), x
PY

OMS_MCP_ROOT="$ROOT" python3 - <<'PY' || fail 'MCP provider routing diverged from the shell registry'
import json, os, runpy, sys

server = runpy.run_path(os.path.join(os.environ["OMS_MCP_ROOT"], "scripts/oms-mcp-server.py"))
assignment = runpy.run_path(os.path.join(os.environ["OMS_MCP_ROOT"], "scripts/lib/task-assignment.py"))
with open(os.path.join(os.environ["OMS_MCP_ROOT"], "config/capability-profiles.json"), encoding="utf-8") as stream:
    config_profiles = json.load(stream)["profiles"]
write_only = {"muse", "kimi", "kiro-cli", "goose", "cline", "kilo", "openhands", "auggie"}
for profile_name in ("core", "council"):
    profile = config_profiles[profile_name]
    names = set(profile["any_of"][0] if profile["any_of"] else profile["min_present"]["commands"])
    assert not names.intersection(write_only), (profile_name, names)
sys.path.insert(0, os.path.join(os.environ["OMS_MCP_ROOT"], "scripts/lib"))
from oms_runtime.profiles import BUILTIN_PROFILE_DEFS
for profile_name in ("core", "council"):
    profile = BUILTIN_PROFILE_DEFS[profile_name]
    names = set(profile["any_of"][0] if profile["any_of"] else profile["min_present"]["commands"])
    assert not names.intersection(write_only), (profile_name, names)
targets, error = server["peer_targets"](
    "dsh,mistral-vibe,github-copilot:model=gpt-5.3-codex,localfoo"
)
assert not error, error
assert targets == ["deepseek", "vibe", "copilot:model=gpt-5.3-codex", "localfoo"], targets
targets, error = server["peer_targets"]("kiro-cli,cn")
assert not error and targets == ["kiro", "continue"], (targets, error)
targets, error = server["peer_targets"]("goose,cline,kilo,openhands,auggie")
assert not error and targets == ["goose", "cline", "kilo", "openhands", "auggie"], (targets, error)
for alias in ("kiro-cli", "cn"):
    try:
        assignment["validate"]({"provider": alias})
    except ValueError:
        pass
    else:
        raise AssertionError("transport alias must not be persisted: " + alias)
targets, error = server["peer_targets"]("glm")
assert not targets and "registered provider" in error, (targets, error)
PY

echo 'provider-registry-smoke: ok'
