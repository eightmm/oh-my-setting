#!/usr/bin/env bash
set -euo pipefail
# These fixtures set role inputs per case below. Do not inherit a caller's
# operation or opt-out switch into generic provider-default assertions.
unset OMS_MODEL_OPERATION OMS_MODEL_WORKLOAD OMS_MODEL_ROLE OMS_ROLE_ROUTING OMS_RESUME_SESSION OMS_CODEX_ACCOUNT_ROUTE || true
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_OPERATION='' OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_FALLBACK" "$OMS_REASONING_RESOLVED" "$OMS_REASONING_EXPLICIT"')"
[ "$out" = 'provider-default|||0' ] || fail "provider default route: $out"
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=provider-default \
  OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' \
  >/dev/null 2>&1; then
  fail 'provider-default is a reserved sentinel, not an explicit model name'
fi
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT=model-a OMS_REASONING_EFFORT_REQUEST=high bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' >/dev/null 2>&1; then fail 'per-model effort must validate'; fi
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s" "$OMS_MODEL_DISTINCT_CHAIN"')"
printf '%s\n' "$out" | grep >/dev/null -Fx 'Model B' || fail 'provider-default catalog chain missing distinct model'
# The tier layer is gone entirely: a legacy class request is plain unknown
# environment — routing neither warns about it nor lets it change the route.
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_CLASS_REQUEST=deep OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex; printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_RESOLVED_CLASS"' 2>"$TMP/warn.err")"
[ "$out" = 'provider-default|provider-default' ] || fail "legacy class request must not change the route: $out"
if grep -q 'model tiers' "$TMP/warn.err"; then fail 'removed tier layer must not warn'; fi

# BEGIN catalog-auto-refresh
catalog="$TMP/catalog-refresh"
mkdir -p "$catalog/bin" "$catalog/cache" "$catalog/state"
cat > "$catalog/bin/agy" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  --version) echo 'agy 1' ;;
  --help) echo '--model --effort (low, high)' ;;
  models) printf 'probe\n' >> "$CATALOG_CALLS"; [ "${CATALOG_FAIL:-0}" != 1 ] || exit 1; echo gemini-4-argon ;;
  *) exit 0 ;;
esac
STUB
printf '#!/usr/bin/env bash\nexit 1\n' > "$catalog/bin/codex"
cp "$catalog/bin/codex" "$catalog/bin/opencode"
chmod +x "$catalog/bin/agy" "$catalog/bin/codex" "$catalog/bin/opencode"
printf 'old-model\n' > "$catalog/cache/antigravity.models"
python3 - "$catalog/cache/antigravity.models" <<'PYAGE'
import os, sys, time
old = time.time() - 8 * 86400
os.utime(sys.argv[1], (old, old))
PYAGE
catalog_tick() {
  PATH="$catalog/bin:$PATH" XDG_CONFIG_HOME="$catalog/config" OMS_PROVIDER_ADAPTER_DIR="$catalog/adapters" XDG_STATE_HOME="$catalog/state" OMS_LOCK_DIR="$catalog/locks" OMS_CAPABILITY_DIR="$catalog/cache" \
    OMS_TICK_REGISTRY="$catalog/no-registry" CATALOG_CALLS="$catalog/calls" \
    OMS_CAPABILITY_SETTLE_WAIT=0 bash "$ROOT/scripts/tick.sh" run >/dev/null
}
catalog_tick
[ "$(cat "$catalog/cache/antigravity.models")" = gemini-4-argon ] || fail 'tick did not refresh stale catalog'
[ "$(wc -l < "$catalog/calls" | tr -d ' ')" = 1 ] || fail 'expected one catalog listing'
catalog_tick
[ "$(wc -l < "$catalog/calls" | tr -d ' ')" = 1 ] || fail 'tick repeated daily catalog listing'
PATH="$catalog/bin:$PATH" OMS_CAPABILITY_DIR="$catalog/cache" bash "$ROOT/scripts/models.sh" --json --providers antigravity | \
  python3 -c 'import json,sys; a=json.load(sys.stdin)["providers"][0]["catalog_age_seconds"]; assert isinstance(a,int) and 0 <= a < 60' || fail 'catalog age missing'
# A failed listing retains the prior catalog and the daily claim.
python3 - "$catalog/cache/antigravity.models" "$catalog/state/oh-my-setting/catalog-refresh.stamp" <<'PYAGE'
import os, sys, time
old = time.time() - 8 * 86400
for path in sys.argv[1:]: os.utime(path, (old, old))
PYAGE
export CATALOG_FAIL=1
catalog_tick
[ "$(cat "$catalog/cache/antigravity.models")" = gemini-4-argon ] || fail 'failed listing replaced catalog'
count="$(wc -l < "$catalog/calls" | tr -d ' ')"
[ "$count" = 3 ] || fail 'failed listing did not use the bounded retry'
catalog_tick
[ "$(wc -l < "$catalog/calls" | tr -d ' ')" = "$count" ] || fail 'failed listing retried on the same day'
unset CATALOG_FAIL
# Capture first: grep -q exiting early would SIGPIPE models.sh under pipefail.
text="$(PATH="$catalog/bin:$PATH" OMS_CAPABILITY_DIR="$catalog/cache" bash "$ROOT/scripts/models.sh" --providers antigravity)"
grep -q '^catalog age: [0-9][0-9]*s$' <<< "$text" || fail 'text catalog age missing'
PATH="$catalog/bin:$PATH" OMS_CAPABILITY_DIR="$catalog/empty" bash "$ROOT/scripts/models.sh" --json --providers antigravity | \
  python3 -c 'import json,sys; assert json.load(sys.stdin)["providers"][0]["catalog_age_seconds"] is None' || fail 'missing catalog age must be unknown'
# END catalog-auto-refresh

# Only the provider's own family at its newest generation is routable on its
# own: a previous generation, a re-hosted foreign-family model, and an
# unversioned name beside versioned siblings never enter the default chain.
# The generation is the first delimited digit run in the name, so both the
# catalog notation and the accepted one (`Gemini 3.7 Flash (High)`) parse.
gen="$TMP/cap-gen"
mkdir -p "$gen"
cp "$TMP/cap/codex.env" "$TMP/cap/antigravity.env" "$gen/"
printf 'gpt-5.6-sol\ngpt-5.6-terra\ngpt-5.6-luna\ngpt-5.5\ngpt-5.4-mini\ngpt-5.3-codex-spark\n' > "$gen/codex.models"
printf 'Fetching available models...\ngemini-3.7-flash-high\ngemini-3.7-flash-low\ngemini-3.1-pro-high\nclaude-opus-4-6-thinking\ngpt-oss-120b-medium\n' > "$gen/antigravity.models"
# Model catalogs belong to the parent router, not non-delegating workers.
PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" bash -c '
  . "$1/scripts/lib/peer-common.sh"
  oms_capability_binary_key codex >/dev/null
  before="$OMS_CAPABILITY_KEY_CACHE"
  ma_write_harness_context "$2" 0 0 0 > "$2/harness-prompt"
  [ "$OMS_CAPABILITY_KEY_CACHE" = "$before" ] || exit 7
' _ "$ROOT" "$TMP" || fail 'prompt construction reset the binary identity cache'
if grep -Fq 'gpt-5.6-sol' "$TMP/harness-prompt"; then
  fail 'worker context included an unused model catalog'
fi
grep -Fq 'file/line evidence' "$TMP/harness-prompt" || fail 'worker lost evidence guidance'
routable() {  # routable PROVIDER -> space-joined routable catalog
  PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_capability_routable_models "$1"' _ "$1" | tr '\n' ' '
}
prepare_gen() {  # prepare_gen PROVIDER EXPLICIT ERRFILE REPORT-EXPRESSION
  PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" OMS_MODEL_EXPLICIT="$2" OMS_REASONING_EFFORT_REQUEST=auto \
    bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare "$1" 2>"$2"; eval "$3"' _ "$1" "$3" "$4"
}
out="$(routable codex)"
[ "$out" = 'gpt-5.6-sol gpt-5.6-terra gpt-5.6-luna ' ] ||
  fail "codex routable set must be the newest generation only: $out"
out="$(routable antigravity)"
[ "$out" = 'gemini-3.7-flash-high gemini-3.7-flash-low ' ] ||
  fail "antigravity routable set must drop the older pro line and re-hosted foreign families: $out"
out="$(prepare_gen codex '' "$TMP/gen.err" 'printf "%s|%s" "$OMS_MODEL_ALTERNATE" "$(printf "%s" "$OMS_MODEL_DISTINCT_CHAIN" | tr "\n" " ")"')"
[ "$out" = 'gpt-5.6-sol|gpt-5.6-sol gpt-5.6-terra gpt-5.6-luna' ] ||
  fail "provider-default recovery must stay inside the newest generation: $out"
# A named previous-generation model still runs — it was named — but says so.
out="$(prepare_gen codex gpt-5.5 "$TMP/gen.err" 'printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 'gpt-5.5|1' ] || fail "an explicit previous-generation model runs and is flagged: $out"
grep -q 'previous generation' "$TMP/gen.err" ||
  fail "an explicit previous-generation model must be named on stderr: $(cat "$TMP/gen.err")"
out="$(prepare_gen antigravity 'Gemini 3.7 Flash (High)' "$TMP/gen2.err" 'printf "%s" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 0 ] || fail "the accepted notation of a current model must not be flagged: $out"
[ ! -s "$TMP/gen2.err" ] || fail "a current model must not warn: $(cat "$TMP/gen2.err")"
out="$(prepare_gen antigravity 'Gemini 3.1 Pro (High)' "$TMP/gen3.err" 'printf "%s" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 1 ] || fail "the accepted notation of an older line must be flagged: $out"
out="$(prepare_gen codex gpt-7-unlisted "$TMP/gen4.err" 'printf "%s" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 0 ] && [ ! -s "$TMP/gen4.err" ] || fail "a model the catalog does not know cannot be judged: $out $(cat "$TMP/gen4.err")"
# Generations compare as numbers: 5.10 is newer than 5.6.
# A safeguard retry walks the recovery chain and then Claude's Opus 5 floor;
# the floor is never a first choice, and a named model gets no retry at all.
safeguard_chain() {  # safeguard_chain PROVIDER OPERATION EXPLICIT
  PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" OMS_MODEL_EXPLICIT="$3" OMS_REASONING_EFFORT_REQUEST=auto \
    OMS_MODEL_OPERATION="$2" bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare "$1" 2>/dev/null; printf "%s" "$OMS_MODEL_SAFEGUARD_CHAIN" | tr "\n" ","' _ "$1"
}
[ "$(safeguard_chain claude delegate '')" = 'claude-fable-5-1,claude-opus-5' ] ||
  fail "a claude worker's safeguard retry must end on Opus 5: $(safeguard_chain claude delegate '')"
[ "$(safeguard_chain claude consult '')" = 'claude-opus-5' ] ||
  fail "a provider-default claude call must still reach the Opus 5 floor: $(safeguard_chain claude consult '')"
[ -z "$(safeguard_chain claude delegate claude-opus-5-5)" ] || fail "a named model must keep no safeguard chain"
case "$(safeguard_chain codex delegate '')" in *opus*) fail "the floor belongs to claude only" ;; esac
printf 'gpt-5.6-nova\ngpt-5.10-nova\n' > "$gen/codex.models"
out="$(routable codex)"
[ "$out" = 'gpt-5.10-nova ' ] || fail "a two-digit minor must compare numerically: $out"
# A catalog whose names carry no generation is kept whole — nothing to compare —
# while an unversioned name beside versioned siblings cannot prove it is current.
printf 'model-a\nModel B\n' > "$gen/codex.models"
out="$(routable codex)"
[ "$out" = 'model-a Model B ' ] || fail "an unversioned catalog stays routable: $out"
printf 'model-a\ngpt-5.6-sol\n' > "$gen/codex.models"
out="$(routable codex)"
[ "$out" = 'gpt-5.6-sol ' ] || fail "an unversioned name beside versioned siblings drops: $out"

# Role routing: Codex role families retain Astra, Sol, Luna cost order across
# their independent newest versions; other providers keep generation seeds.
role_prepare() {  # role_prepare PROVIDER OPERATION ROLE ERRFILE
  PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto \
    OMS_MODEL_OPERATION="$2" OMS_MODEL_ROLE="$3" \
    bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare "$1" 2>"$2"; printf "%s|%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_RESOLVED_CLASS" "$OMS_MODEL_CLASS_REASON" "$OMS_MODEL_ALTERNATE"' _ "$1" "$4"
}
printf 'gpt-5.6-sol\ngpt-5.6-terra\ngpt-5.6-luna\ngpt-5.5\n' > "$gen/codex.models"
out="$(role_prepare codex delegate implementation-worker "$TMP/role.err")"
[ "$out" = 'gpt-5.6-sol|role-default|role:worker|gpt-5.6-terra' ] ||
  fail "a standard worker uses Sol and tries the next cheaper family, Terra, first: $out"
out="$(OMS_MODEL_WORKLOAD=routine role_prepare codex delegate implementation-worker "$TMP/routine.err")"
case "$out" in 'gpt-5.6-luna|role-default|role:routine-worker|'*) ;; *) fail "routine work must select the lowest routable seeded rank: $out" ;; esac
out="$(OMS_MODEL_WORKLOAD=routine role_prepare claude delegate implementation-worker "$TMP/routine-claude.err")"
case "$out" in 'claude-opus-5-5|role-default|role:routine-worker|'*) ;; *) fail "routine selection must work across providers: $out" ;; esac
out="$(OMS_MODEL_WORKLOAD=routine OMS_ROLE_ROUTING=0 role_prepare codex delegate implementation-worker "$TMP/routine-off.err")"
case "$out" in 'provider-default|provider-default|'*) ;; *) fail "routing opt-out must also disable routine: $out" ;; esac
out="$(OMS_MODEL_WORKLOAD=routine role_prepare codex consult '' "$TMP/role2.err")"
[ "$out" = 'provider-default|provider-default|provider-default|gpt-5.6-sol' ] ||
  fail "a judge keeps the provider default: $out"
out="$(OMS_ROLE_ROUTING=0 role_prepare codex delegate implementation-worker "$TMP/role3.err")"
[ "$out" = 'provider-default|provider-default|provider-default|gpt-5.6-sol' ] ||
  fail "role routing must be switchable off: $out"
# The fixture catalog lists no medium line, so the seed's second entry is not
# routable and the next routable rank is taken instead of an unlisted name.
# The flash seed names no generation, so a newer generation (3.8) needs no code change.
printf 'gemini-3.8-flash-high\ngemini-3.8-flash-medium\ngemini-3.7-flash-low\ngemini-3.1-pro-high\n' > "$gen/antigravity.models"
out="$(role_prepare antigravity delegate implementation-worker "$TMP/role5.err")"
case "$out" in 'gemini-3.8-flash-medium|role-default|role:worker|'*) ;; *) fail "the antigravity worker rank follows the live generation without a pin: $out" ;; esac
printf 'Gemini 3.7 Flash (High)\nGemini 3.7 Flash (Low)\n' > "$gen/antigravity.models"
for workload in standard routine; do
  out="$(OMS_MODEL_WORKLOAD="$workload" role_prepare antigravity delegate implementation-worker "$TMP/spaced.err")"
  case "$out" in 'Gemini 3.7 Flash (Low)|role-default|'*) ;; *) fail "worker ranks must preserve spaced catalog names: $out" ;; esac
done
# No catalog at all (claude): the seed stands in for the routable set. Opus
# is the cheapest seeded rank, so recovery can only climb to Fable.
out="$(role_prepare claude delegate implementation-worker "$TMP/role6.err")"
[ "$out" = 'claude-opus-5-5|role-default|role:worker|claude-fable-5-1' ] || fail "a catalog-less provider routes its worker and recovery from the seed: $out"
# Codex role families advance independently. Sol 6.1, Astra 6.10 and Luna
# 6.2 are the newest known IDs in their respective families; old Sol 6 stays
# excluded while explicit pins retain the warning and exact route.
printf 'gpt-6-astra\ngpt-6.2-astra\ngpt-6.10-astra\ngpt-6-sol\ngpt-6.1-sol\ngpt-6-luna\ngpt-6.2-luna\ngpt-6.10-nova\ngpt-6..11-sol\n' > "$gen/codex.models"
out="$(routable codex)"
[ "$out" = 'gpt-6.10-astra gpt-6.1-sol gpt-6.2-luna ' ] || fail "Codex keeps each known family at its newest numeric version: $out"
out="$(role_prepare codex delegate implementation-worker "$TMP/role-g6.err")"
[ "$out" = 'gpt-6.1-sol|role-default|role:worker|gpt-6.2-luna' ] || fail "a standard worker uses newest Sol and cheaper Luna recovery: $out"
out="$(OMS_MODEL_WORKLOAD=routine role_prepare codex delegate implementation-worker "$TMP/routine-g6.err")"
case "$out" in 'gpt-6.2-luna|role-default|role:routine-worker|gpt-6.1-sol') ;; *) fail "routine work uses newest Luna and cheaper Sol recovery: $out" ;; esac
out="$(prepare_gen codex gpt-6-sol "$TMP/old-sol.err" 'printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 'gpt-6-sol|1' ] || fail "an older exact Sol pin remains unchanged and flagged: $out"
grep -q 'previous generation' "$TMP/old-sol.err" || fail 'an older exact Sol pin must retain its warning'
out="$(prepare_gen codex gpt-6.1-sol "$TMP/current-sol.err" 'printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 'gpt-6.1-sol|0' ] && [ ! -s "$TMP/current-sol.err" ] || fail "newest exact Sol pin remains exact: $out"
# Numeric comparison, absent tiers and malformed/unknown IDs are bounded to
# known families; legacy unknown-generation behavior remains covered above.
printf 'gpt-6.2-sol\ngpt-6.10-sol\n' > "$gen/codex.models"
[ "$(routable codex)" = 'gpt-6.10-sol ' ] || fail '6.10 must compare newer than 6.2'
# Terra sits between Sol and Luna. A family whose newest ID is a major
# generation behind the others (any 5.x beside 6.x) is not routed, and a newer
# family version (6.2 Astra) replaces the older one without a seed change.
printf 'gpt-6.1-sol\ngpt-6.2-astra\ngpt-6-astra\ngpt-6.1-terra\ngpt-6-sol\ngpt-6-luna\ngpt-5.6-sol\ngpt-5.6-terra\ngpt-5.6-luna\n' > "$gen/codex.models"
out="$(routable codex)"
[ "$out" = 'gpt-6.1-sol gpt-6.2-astra gpt-6.1-terra gpt-6-luna ' ] || fail "Terra 6.1 routes and no 5.6 model does: $out"
out="$(role_prepare codex delegate implementation-worker "$TMP/role-terra.err")"
[ "$out" = 'gpt-6.1-sol|role-default|role:worker|gpt-6.1-terra' ] || fail "Terra is the first cheaper recovery after Sol: $out"
out="$(OMS_MODEL_WORKLOAD=routine role_prepare codex delegate implementation-worker "$TMP/routine-terra.err")"
case "$out" in 'gpt-6-luna|role-default|role:routine-worker|gpt-6.1-terra') ;; *) fail "routine recovery climbs to Terra before Sol: $out" ;; esac
out="$(prepare_gen codex gpt-5.6-terra "$TMP/old-terra.err" 'printf "%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_PREVIOUS_GENERATION"')"
[ "$out" = 'gpt-5.6-terra|1' ] || fail "a 5.6 Terra pin runs only as named and is flagged: $out"
printf 'gpt-6.1-sol\ngpt-6-astra\ngpt-5.6-terra\n' > "$gen/codex.models"
[ "$(routable codex)" = 'gpt-6.1-sol gpt-6-astra ' ] || fail "a family left on 5.x must not route beside 6.x: $(routable codex)"
printf 'gpt-6.10-astra\ngpt-6.2-nova\ngpt-6..10-luna\n' > "$gen/codex.models"
[ "$(routable codex)" = 'gpt-6.10-astra ' ] || fail 'absent tiers and malformed IDs must not be fabricated'
out="$(role_prepare codex delegate implementation-worker "$TMP/role-absent.err")"
[ "$out" = 'gpt-6.10-astra|role-default|role:worker|provider-default' ] || fail "missing Sol must use only available known family: $out"
printf 'gpt-5.6-nova\ngpt-5.10-nova\n' > "$gen/codex.models"
out="$(OMS_MODEL_WORKLOAD=routine role_prepare codex delegate implementation-worker "$TMP/routine-new.err")"
case "$out" in 'provider-default|provider-default|'*) ;; *) fail "routine must not invent unseeded generation models: $out" ;; esac
out="$(role_prepare codex delegate implementation-worker "$TMP/role4.err")"
[ "$out" = 'provider-default|provider-default|provider-default|gpt-5.10-nova' ] ||
  fail "an unseeded generation routes as provider default: $out"
grep -q 'no price order' "$TMP/role4.err" ||
  fail "an unseeded generation must say so: $(cat "$TMP/role4.err")"

# Auto effort resolves to "" (provider default). The REAL invocation must then
# omit the effort flag entirely: codex refuses -c model_reasoning_effort=""
# outright ("reasoning_effort must not be empty"), which silently killed every
# no-effort council seat while the dry-run smokes stayed green. Recorder stubs
# exercise the live command build, not the dry-run shortcut.
cat > "$TMP/bin/codex" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in --version) echo 'codex 1.0'; exit 0 ;; esac
printf '%s\n' "$@" > "$CODEX_ARGV_OUT"
[ -z "${OMS_ACCOUNT_SEEN:-}" ] || printf '%s\n' "${CODEX_HOME:-unset}" > "$OMS_ACCOUNT_SEEN"
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
[ -z "${OMS_ACCOUNT_SEEN:-}" ] || printf '%s\n' "${CODEX_HOME:-unset}" > "$OMS_ACCOUNT_SEEN"
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
# agy answers arrive in its --output-format json envelope.
printf '%s\n' '{"conversation_id": "stub", "status": "SUCCESS", "response": "stub Antigravity review answer with enough substance to count", "usage": {}}'
EOF
chmod +x "$TMP/bin/codex" "$TMP/bin/claude" "$TMP/bin/agy"
mkdir -p "$TMP/invoke-repo/.oms" "$TMP/locks" "$TMP/home"
# agy write delegations refuse up front without the standing delegate rules.
# Pin settings and the worktree parent inside the fake HOME so no inherited
# override reaches the real ones.
unset OMS_ANTIGRAVITY_SETTINGS OMS_DELEGATE_WORKTREE_ROOT
export XDG_CACHE_HOME="$TMP/home/.cache"
HOME="$TMP/home" PATH="$TMP/bin:$PATH" bash "$ROOT/scripts/provider-permissions.sh" --apply --profile delegate >/dev/null ||
  fail 'delegate permissions fixture failed'
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

# BEGIN shared-account-runner
account_root="$TMP/accounts"
mkdir -p "$account_root/config/oh-my-setting" "$account_root/selected" "$TMP/home/.codex"
chmod 700 "$account_root/selected"
python3 - "$account_root" <<'PYACCOUNT'
import json, sys
from pathlib import Path
root = Path(sys.argv[1])
(root / "config/oh-my-setting/provider-accounts.json").write_text(json.dumps({
    "schema": 1, "providers": {"codex": {"accounts": [
        {"id": "selected", "home": str(root / "selected")}]}}}))
PYACCOUNT
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  invoke --to codex --model model-a --prompt "shared account route" || fail 'non-panel account call failed'
[ "$(cat "$account_root/seen")" = "$account_root/selected" ] || fail 'shared runner did not apply selected home'
python3 - "$TMP/invoke-repo/.oms/artifacts" <<'PYACCOUNT' || fail 'attempt omitted applied account'
import json, sys
from pathlib import Path
records = list(Path(sys.argv[1]).rglob("*.attempts.json"))
assert records
row = json.loads(max(records, key=lambda path: path.stat().st_mtime).read_text())["attempts"][-1]
assert row["account"] == "selected" and row["account_applied"] is True, row
PYACCOUNT
mv "$account_root/selected" "$account_root/offline"
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  invoke --to codex --prompt "explicit provider account fallback" || fail 'explicit provider target did not fall back'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'explicit provider target changed fallback home'
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  HOME="$TMP/home" PATH="$TMP/bin:$PATH" CODEX_ARGV_OUT="$TMP/codex.argv" \
  OMS_TEST_CALL_LOG="$TMP/provider.calls" OMS_CAPABILITY_DIR="$TMP/cap" \
  bash "$ROOT/scripts/peer-ask.sh" --repo "$TMP/invoke-repo" --providers codex \
    --require-complete --prompt 'explicit council account fallback' >"$TMP/ask-account.out" 2>&1 ||
  fail 'explicit council account did not fall back'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'explicit council changed fallback home'
for peer_command in consult advise; do
  rm -f "$TMP/codex.argv" "$TMP/claude.argv" "$TMP/agy.argv"
  caller=antigravity
  expected_argv="$TMP/claude.argv"
  if [ "$peer_command" = advise ]; then
    caller=claude
    expected_argv="$TMP/agy.argv"
  fi
  OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
    HOME="$TMP/home" PATH="$TMP/bin:$PATH" CODEX_ARGV_OUT="$TMP/codex.argv" CLAUDE_ARGV_OUT="$TMP/claude.argv" AGY_ARGV_OUT="$TMP/agy.argv" \
    OMS_TEST_CALL_LOG="$TMP/provider.calls" OMS_CAPABILITY_DIR="$TMP/cap" OMS_AGENT="$caller" \
    OMS_CONSULT_PROVIDER=codex bash "$ROOT/scripts/$peer_command.sh" --repo "$TMP/invoke-repo" \
      --prompt 'automatic account failover' >"$TMP/auto-account.out" 2>&1 ||
    fail "$peer_command did not fail over"
  [ ! -f "$TMP/codex.argv" ] && [ -f "$expected_argv" ] || fail "$peer_command did not skip exhausted Codex"
done
mv "$account_root/offline" "$account_root/selected"
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  OMS_RESUME_SESSION=account-main-session invoke --to codex --model model-a --prompt "main home resume" || fail 'main home resume failed'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'direct resume selected another account'
grep -Fx resume "$TMP/codex.argv" >/dev/null || fail 'resume command omitted'
grep -Fx account-main-session "$TMP/codex.argv" >/dev/null || fail 'resume id omitted'
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$account_root/explicit" \
  invoke --to codex --model model-a --prompt "explicit account route" || fail 'explicit home call failed'
[ "$(cat "$account_root/seen")" = "$account_root/explicit" ] || fail 'explicit non-main home replaced'
# A pre-applied panel account must stay pinned even at the main home.
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  OMS_CODEX_ACCOUNT_ROUTE='{"account":"pinned","account_applied":true}' \
  invoke --to codex --model model-a --prompt "panel account route" || fail 'pinned panel call failed'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'panel decision applied twice'
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  invoke --to claude --prompt "other provider account route" || fail 'non-Codex call changed'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'non-Codex home changed'
for malformed_registry in '' '{' '0' '[]'; do
  printf '%s' "$malformed_registry" > "$account_root/config/oh-my-setting/provider-accounts.json"
  OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
    invoke --to codex --model model-a --prompt "invalid account registry" || fail 'malformed registry blocked the caller home'
  [ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'malformed registry changed home'
  python3 - "$TMP/invoke-repo/.oms/artifacts/call" <<'PYACCOUNT' || fail 'registry error did not warn exactly once'
import sys
from pathlib import Path
records = list(Path(sys.argv[1]).glob("*.attempt-1.out"))
body = max(records, key=lambda path: path.stat().st_mtime).read_text()
assert body.count("warning: Codex account selection skipped:") == 1, body
assert "provider accounts" in body and "caller home" in body, body
PYACCOUNT
done
# An early prompt-copy failure must not attribute a previous account to this attempt.
ACCOUNT_FAILURE_ROOT="$account_root" bash -c '
  . "$1/scripts/lib/peer-common.sh"
  printf -- "-leading prompt\n" > "$ACCOUNT_FAILURE_ROOT/prompt"
  agent_memory_mktemp() { return 1; }
  OMS_PROVIDER_ACCOUNT=previous
  if ma_provider_attempt codex read "$ACCOUNT_FAILURE_ROOT/prompt" "$ACCOUNT_FAILURE_ROOT/failed"; then exit 1; fi
  [ -z "$OMS_PROVIDER_ACCOUNT" ] || exit 1
  ma_record_attempt "$ACCOUNT_FAILURE_ROOT/early.md" codex model-a medium 2 failure "$ACCOUNT_FAILURE_ROOT/failed"
' _ "$ROOT" || fail 'early attempt retained a stale account'
python3 - "$account_root/early.attempts.json" <<'PYACCOUNT' || fail 'early failure recorded the previous account'
import json, sys
row = json.load(open(sys.argv[1]))["attempts"][-1]
assert "account" not in row and "account_applied" not in row, row
PYACCOUNT
rm "$account_root/config/oh-my-setting/provider-accounts.json"
: > "$TMP/provider.calls"
OMS_ACCOUNT_SEEN="$account_root/seen" XDG_CONFIG_HOME="$account_root/config" CODEX_HOME="$TMP/home/.codex" \
  invoke --to codex --model model-a --prompt "unregistered account route" || fail 'no-accounts call failed'
[ "$(cat "$account_root/seen")" = "$TMP/home/.codex" ] || fail 'no-accounts home changed'
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] || fail 'account lookup made another provider call'
python3 - "$TMP/invoke-repo/.oms/artifacts" <<'PYACCOUNT' || fail 'unregistered account changed attempt schema'
import json, sys
from pathlib import Path
records = list(Path(sys.argv[1]).rglob("*.attempts.json"))
row = json.loads(max(records, key=lambda path: path.stat().st_mtime).read_text())["attempts"][-1]
assert "account" not in row and "account_applied" not in row, row
PYACCOUNT
# END shared-account-runner

invoke --to codex --prompt "argv probe" || fail 'auto-effort codex call failed'
[ -s "$TMP/codex.argv" ] || fail 'codex recorder stub was not invoked'
if grep -q 'model_reasoning_effort=""' "$TMP/codex.argv"; then
  fail 'auto effort must never pass an empty codex effort'
fi
grep -q 'model_reasoning_effort="medium"' "$TMP/codex.argv" ||
  fail 'a codex judging seat with no chosen effort must run at medium'
# Only an unchosen effort on a Codex read seat moves; write workers, other
# providers and any chosen effort keep their route.
effort_after() {
  OMS_REASONING_RESOLVED="$3" bash -c '. "$1/scripts/lib/peer-common.sh"; ma_default_read_effort "$2" "$3"; printf "%s" "$OMS_REASONING_RESOLVED"' \
    _ "$ROOT" "$1" "$2"
}
[ "$(effort_after codex read "")" = medium ] || fail 'codex read seat default is not medium'
[ -z "$(effort_after codex write "")" ] || fail 'a write worker must keep the provider default effort'
[ -z "$(effort_after claude read "")" ] || fail 'only codex seats were measured; claude keeps its default'
[ "$(effort_after codex read high)" = high ] || fail 'a chosen effort must win over the seat default'
fallback_effort="$(OMS_MODEL_PRIMARY=provider-default OMS_MODEL_FALLBACK=gpt-6-luna OMS_REASONING_RESOLVED='' OMS_REASONING_FALLBACK='' \
  bash -c '. "$1/scripts/lib/peer-common.sh"; ma_default_read_effort codex read; printf "%s|%s" "$OMS_REASONING_RESOLVED" "$OMS_REASONING_FALLBACK"' _ "$ROOT")"
[ "$fallback_effort" = 'medium|medium' ] || fail "a capacity fallback must keep the seat's medium effort: $fallback_effort"
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
tail -n 1 "$TMP/provider.calls" | grep -F 'Model B' > /dev/null ||
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
  sed -n '2p' "$TMP/provider.calls" | grep -F -- '--model model-a' > /dev/null ||
    fail "provider-default $mode recovery did not select the first catalog model"
done

: > "$TMP/provider.calls"
if OMS_MODEL_SAFEGUARD_RETRIES=0 OMS_TEST_PROVIDER_MODE=safeguard \
  invoke --to codex --prompt "safeguard retry cap zero"; then
  fail 'OMS_MODEL_SAFEGUARD_RETRIES=0 should not recover on safeguard'
fi
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 1 ] ||
  fail 'OMS_MODEL_SAFEGUARD_RETRIES=0 still retried on safeguard'

(
  read -r OMS_ATTEMPT_MAX_TOKENS <<< 5000
  export OMS_ATTEMPT_MAX_TOKENS
  OMS_PARENT_EVENT_ID=evt_parent_probe OMS_ATTEMPT_MAX_WALL_SECONDS=120 \
    OMS_ATTEMPT_MAX_COST_MICROUSD=900 \
    invoke --to codex --prompt "index parent and attempt budget probe"
) || fail 'parent event and attempt budget probe failed'
python3 - "$TMP/invoke-repo" <<'PY' || fail 'parent_event_id or attempt budget was not recorded'
import json, sys
from pathlib import Path
repo = Path(sys.argv[1])
row = [json.loads(line) for line in (repo / ".oms/artifacts/index.jsonl").read_text(encoding="utf-8").splitlines() if line.strip()][-1]
assert row.get("parent_event_id") == "evt_parent_probe", row
events = [json.loads(line) for line in (repo / ".oms/lifecycle/events.jsonl").read_text(encoding="utf-8").splitlines()
          if line.strip() and json.loads(line).get("attempt_id") == row["attempt_id"] and json.loads(line).get("event_type") == "attempt.created"]
assert events and events[-1].get("budget") == {"max_wall_seconds": 120, "max_tok" + "ens": 5000, "max_cost_microusd": 900}, events
PY

: > "$TMP/provider.calls"
OMS_TEST_PROVIDER_MODE=unknown invoke --to codex --reasoning-effort ultra \
  --prompt "default recovery with a model-specific effort" ||
  fail 'provider-default recovery should find a model that accepts the requested effort'
[ "$(wc -l < "$TMP/provider.calls" | tr -d ' ')" = 2 ] ||
  fail 'effort-compatible recovery should make one retry'
sed -n '2p' "$TMP/provider.calls" | grep -F -- '--model Model B' > /dev/null ||
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

# Claude's envelope line is authoritative: a clean end_turn answer that quotes
# the decline patterns is an answer, while reason=refusal stays a decline and a
# cleanly closed Codex turn is still read for text refusals.
decline() { bash -c '. "$1/scripts/lib/model-routing.sh"; oms_model_is_policy_decline_output "$2"' _ "$ROOT" "$1"; }
printf '%s\n' 'stop-reason: provider=claude reason=end_turn subtype=success is_error=0' \
  'The pattern `stop-reason: .*reason=refusal` also matches "Unable to respond to this request".' > "$TMP/claude-answer.txt"
if decline "$TMP/claude-answer.txt"; then fail 'a clean Claude end_turn answer quoting refusal patterns was read as a decline'; fi
printf '%s\n' 'stop-reason: provider=claude reason=refusal subtype=success is_error=0' > "$TMP/claude-refusal.txt"
decline "$TMP/claude-refusal.txt" || fail 'a Claude refusal envelope must stay a decline'
printf '%s\n' 'stop-reason: provider=codex reason=turn_completed subtype=success is_error=0' \
  'Unable to respond to this request.' > "$TMP/codex-refusal.txt"
decline "$TMP/codex-refusal.txt" || fail 'a Codex text refusal in a closed turn must stay a decline'
# A refusal never quotes itself: an answer that quotes the policy sentence in
# curly quotes, straight quotes or backticks is an answer, while a bare
# sentence and a JSON refusal field stay declines.
printf '%s\n' 'stop-reason: provider=codex reason=turn_completed subtype=success is_error=0' \
  '2. 실제 답변이 “Unable to respond to this request”이면 거절을 놓칩니다.' \
  'The matcher looks for `unable to respond to this request` and "Unable to respond to this request".' > "$TMP/codex-quoted.txt"
if decline "$TMP/codex-quoted.txt"; then fail 'an answer quoting the refusal sentence was read as a decline'; fi
printf '%s\n' '{"type":"result","stop_reason": "refusal"}' > "$TMP/json-refusal.txt"
decline "$TMP/json-refusal.txt" || fail 'a JSON refusal field must stay a decline despite its quotes'
# Only a cleanly completed answer earns the quote exemption: a failed turn's
# JSON error and untyped output keep quoted policy sentences as declines.
printf '%s\n' 'stop-reason: provider=codex reason=turn_failed subtype=error is_error=1' \
  '{"message": "Your request may violate our usage policy."}' > "$TMP/codex-failed.txt"
decline "$TMP/codex-failed.txt" || fail 'a failed turn quoting a policy sentence in its JSON error must stay a decline'
printf '%s\n' '"Unable to respond to this request."' > "$TMP/untyped-quoted.txt"
decline "$TMP/untyped-quoted.txt" || fail 'untyped output keeps quoted refusal sentences as declines'

env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:$PATH" AGY_ARGV_OUT="$TMP/agy.argv" \
  OMS_CAPABILITY_DIR="$TMP/cap" OMS_LOCK_DIR="$TMP/locks" OMS_LOCK_FORCE_MKDIR=1 \
  bash "$ROOT/scripts/peer-review.sh" --repo "$TMP/invoke-repo" \
    --providers antigravity --no-diff --reasoning-effort high \
    --prompt "effort routing probe" >/dev/null 2>&1 ||
  fail 'peer-review should accept Antigravity reasoning effort supported by the capability registry'
grep -Fxq -- '--effort' "$TMP/agy.argv" || fail 'peer-review dropped Antigravity --effort'
grep -Fxq -- 'high' "$TMP/agy.argv" || fail 'peer-review dropped Antigravity effort value'

# Exercise the real review parser: an unterminated final line used to count
# one provider as zero and two providers as one for explicit model options.
for model_option in --model --fallback-model; do
  bash "$ROOT/scripts/peer-review.sh" --repo "$TMP/invoke-repo" \
    --providers codex "$model_option" gpt-6-astra --no-diff --dry-run \
    --prompt 'single reviewer model' > "$TMP/review-model.out" 2>&1 ||
    fail "single reviewer rejected $model_option: $(cat "$TMP/review-model.out")"
  if bash "$ROOT/scripts/peer-review.sh" --repo "$TMP/invoke-repo" \
      --providers codex,claude "$model_option" gpt-6-astra --no-diff --dry-run \
      --prompt 'ambiguous reviewer model' > "$TMP/review-model.out" 2>&1; then
    fail "multiple reviewers accepted $model_option"
  fi
  grep -Fq 'requires exactly one provider' "$TMP/review-model.out" ||
    fail 'multiple reviewer model failed for an unrelated reason'
done

# Per-seat model notation must survive effort validation, including spaces;
# use each model's scale rather than the provider-wide maximum.
for target in 'codex:model=model-a' 'codex:model=Model B'; do
  PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" \
    bash "$ROOT/scripts/peer-ask.sh" --repo "$TMP/invoke-repo" \
      --providers "$target" --reasoning-effort medium --dry-run \
      --prompt 'pinned effort' > "$TMP/ask-effort.out" 2>&1 ||
    fail "pinned council effort rejected: $(cat "$TMP/ask-effort.out")"
done
if PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" \
    bash "$ROOT/scripts/peer-ask.sh" --repo "$TMP/invoke-repo" \
      --providers codex:model=model-a --reasoning-effort high --dry-run \
      --prompt 'unsupported pinned effort' > "$TMP/ask-effort.out" 2>&1; then
  fail 'council ignored the pinned model effort scale'
fi
grep -Fq "does not accept reasoning effort 'high'" "$TMP/ask-effort.out" ||
  fail 'pinned effort refused for unrelated reason'

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
sed -n '3p' "$TMP/provider.calls" | grep -F -- '--model Model B' > /dev/null ||
  fail 'repair did not stay on the successfully selected fallback model'

# The standalone model options are unambiguous only with one plain, explicit
# provider target. The target notation already carries an exact model and must
# not be silently overwritten by a second --model.
consult_help="$(bash "$ROOT/scripts/consult.sh" --help)" || fail 'consult --help failed'
printf '%s' "$consult_help" | grep >/dev/null -F -- '--model MODEL' || fail 'consult help omits --model'
printf '%s' "$consult_help" | grep >/dev/null -F -- '--fallback-model M' || fail 'consult help omits --fallback-model'
printf '%s' "$consult_help" | grep >/dev/null -F -- '--reasoning-effort E' || fail 'consult help omits --reasoning-effort'
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

# The collaboration profile must remain pinned even with a legacy-only catalog.
printf 'gpt-6.2-astra\ngpt-6.10-astra\ngpt-6.2-sol\ngpt-6.10-sol\ngpt-6.10-nova\ngpt-6..10-luna\n' > "$gen/codex.models"
for operation in delegate plan; do
  out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" \
    OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT='' \
    OMS_MODEL_OPERATION="$operation" bash -c \
    '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex || exit; printf "%s" "$OMS_MODEL_PRIMARY"')"
  case "$operation" in delegate) expected=gpt-6.10-sol ;; *) expected=gpt-6.10-astra ;; esac
  [ "$out" = "$expected" ] || fail "collaboration must select the newest model in its fixed family: $operation => $out"
done
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" \
  OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT=gpt-6.2-sol \
  OMS_MODEL_OPERATION=delegate bash -c \
  '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex || exit; printf "%s" "$OMS_MODEL_PRIMARY"')"
[ "$out" = gpt-6.2-sol ] || fail "collaboration rewrote an explicit older GPT-6 pin: $out"
# An absent requested family keeps its established exact seed; it never borrows
# a different GPT-6 tier or fabricates a catalog ID.
printf 'gpt-6.10-astra\ngpt-6.2-nova\ngpt-6..10-sol\n' > "$gen/codex.models"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" \
  OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT='' \
  OMS_MODEL_OPERATION=delegate bash -c \
  '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex || exit; printf "%s" "$OMS_MODEL_PRIMARY"')"
[ "$out" = gpt-6.1-sol ] || fail "an absent collaboration family must keep its seed: $out"
printf 'gpt-5.6-nova\ngpt-5.10-nova\n' > "$gen/codex.models"
for operation in delegate plan; do
  out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$gen" \
    OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT='' \
    OMS_MODEL_OPERATION="$operation" OMS_MODEL_FALLBACK_EXPLICIT='' \
    bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex || exit; printf "%s|%s|%s" "$OMS_MODEL_PRIMARY" "$OMS_MODEL_DISTINCT_CHAIN" "$OMS_MODEL_SAFEGUARD_CHAIN"')"
  case "$operation" in delegate) expected=gpt-6.1-sol ;; *) expected=gpt-6-astra ;; esac
  [ "$out" = "$expected||" ] || fail "collaboration recovered to a legacy model: $out"
done
for bad in gpt-5.5 gpt-5.6-sol provider-default; do
  if OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT="$bad" \
    bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' >/dev/null 2>&1; then
    fail "collaboration accepted $bad"
  fi
  if OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT=gpt-6-sol OMS_MODEL_FALLBACK_EXPLICIT="$bad" \
    bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' >/dev/null 2>&1; then
    fail "collaboration accepted fallback $bad"
  fi
done
for current in gpt-6.10-astra gpt-6.2-sol gpt-6.1-terra gpt-6.10-luna gpt-7.1-sol gpt-10-terra; do
  OMS_AUTOPILOT_COLLABORATION=auto bash -c \
    '. "$1/scripts/lib/model-routing.sh"; oms_collaboration_route_validate codex "$2"' _ "$ROOT" "$current" ||
    fail "collaboration rejected a recognized GPT-6-or-newer family model: $current"
  OMS_AUTOPILOT_COLLABORATION=auto bash -c \
    '. "$1/scripts/lib/model-routing.sh"; oms_collaboration_route_validate codex gpt-6.1-sol "$2"' _ "$ROOT" "$current" ||
    fail "collaboration rejected a recognized GPT-6-or-newer fallback: $current"
  out="$(OMS_AUTOPILOT_COLLABORATION=auto OMS_MODEL_EXPLICIT="$current" bash -c \
    '. "$1/scripts/lib/model-routing.sh"; oms_model_prepare codex && printf "%s" "$OMS_MODEL_PRIMARY"' _ "$ROOT")"
  [ "$out" = "$current" ] || fail "collaboration rewrote an explicit model pin: $out"
done
for bad in gpt-6.10-nova gpt-6..10-luna gpt-7.1.1-sol gpt-6.1-unknown gpt-5.6-terra gpt-5-sol gpt-sol gpt-x-luna; do
  if OMS_AUTOPILOT_COLLABORATION=auto bash -c \
    '. "$1/scripts/lib/model-routing.sh"; oms_collaboration_route_validate codex "$2"' _ "$ROOT" "$bad" >/dev/null 2>&1; then
    fail "collaboration accepted an unknown or malformed Codex family: $bad"
  fi
  if OMS_AUTOPILOT_COLLABORATION=auto bash -c \
    '. "$1/scripts/lib/model-routing.sh"; oms_collaboration_route_validate codex gpt-6.1-sol "$2"' _ "$ROOT" "$bad" >/dev/null 2>&1; then
    fail "collaboration accepted an unknown or malformed fallback family: $bad"
  fi
done
if OMS_AUTOPILOT_COLLABORATION=auto bash -c \
  '. "$1/scripts/lib/model-routing.sh"; oms_collaboration_route_validate codex ""' _ "$ROOT" >/dev/null 2>&1; then
  fail 'collaboration accepted an empty Codex model'
fi
if OMS_AUTOPILOT_COLLABORATION=auto bash -c \
  '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare antigravity' >/dev/null 2>&1; then
  fail 'collaboration widened provider authority'
fi

echo 'model-routing-smoke: ok'
