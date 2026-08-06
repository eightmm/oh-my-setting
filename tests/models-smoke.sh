#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-models.XXXXXX")"; trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
mkdir -p "$TMP/cap" "$TMP/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/codex"; chmod +x "$TMP/bin/codex"
now="$(date +%s)"
printf 'binary_key=x\neffort_mechanism=config\neffort_values=low high\nprobed_at=%s\n' "$now" > "$TMP/cap/codex.env"
printf 'model-a\nmodel-b\n' > "$TMP/cap/codex.models"
printf 'model-a\tlow high\n' > "$TMP/cap/codex.efforts"
out="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" "$ROOT/scripts/models.sh" --json)"
OMS_MODELS_JSON="$out" python3 - <<'PY'
import json, os
x=json.loads(os.environ['OMS_MODELS_JSON']); assert x['schema']==1
assert x['providers'][0]['models']==['model-a','model-b']
# A snapshot written before the field existed is unknown, never skipped.
assert x['providers'][0]['catalog_probe']=='unknown', x['providers'][0]['catalog_probe']
PY

# What an empty catalog means, and what a full one means regardless of the
# probe. The skipped/full pairing is the one a routine model-doctor run
# manufactures: it rewrites the snapshot with the probe deliberately off while
# the catalog an earlier --refresh wrote sits beside it untouched.
render="$TMP/cap2"
mkdir -p "$render"
printf 'binary_key=x\neffort_mechanism=config\neffort_values=low high\nmodels_probe=skipped\nprobed_at=%s\n' "$now" > "$render/codex.env"
printf 'model-a\nmodel-b\n' > "$render/codex.models"
printf 'binary_key=y\neffort_mechanism=flag\neffort_values=low high\nmodels_probe=failed\nprobed_at=%s\n' "$now" > "$render/antigravity.env"
printf 'binary_key=z\neffort_mechanism=flag\neffort_values=low high\nmodels_probe=skipped\nprobed_at=%s\n' "$now" > "$render/claude.env"
text="$TMP/render.out"
PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$render" "$ROOT/scripts/models.sh" > "$text"
grep -Fq 'models: model-a, model-b' "$text" ||
  fail 'a present catalog must print even when the last run skipped the probe'
grep -Fq 'models: no local catalog; the CLI validates --model at call time' "$text" ||
  fail 'a provider with no listing source must say so instead of unknown'
grep -Fq "models: unknown — last probe failed; retry with 'oms models --refresh'" "$text" ||
  fail 'a failed probe must be distinguishable from one that never ran'
if grep -Fq "models: model-a, model-b — " "$text"; then
  fail 'a listed catalog must not carry a repair hint'
fi

printf 'binary_key=y\neffort_mechanism=flag\neffort_values=low high\nprobed_at=%s\n' "$now" > "$render/antigravity.env"
never="$TMP/never.out"
PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$render" "$ROOT/scripts/models.sh" > "$never"
grep -Fq "models: unknown — probe with 'oms models --refresh' (local catalog listing, no model tokens)" "$never" ||
  fail 'an unprobed catalog-capable provider must name the command that fills it'

# The write side, through the front door. Stubs only, and PATH is narrowed so a
# real provider CLI on this machine cannot be the one answering.
live="$TMP/cap3"
stubs="$TMP/stubs"
mkdir -p "$live" "$stubs"
cat > "$stubs/agy" <<'EOF_STUB'
#!/usr/bin/env bash
case "${1:-}" in
  models) printf 'gemini-a\ngemini-b\n' ;;
  *) printf -- '--effort (low, medium, high)\n' ;;
esac
EOF_STUB
printf '#!/usr/bin/env bash\nprintf -- "--effort\\n"\n' > "$stubs/claude"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stubs/codex"
chmod +x "$stubs/agy" "$stubs/claude" "$stubs/codex"
PATH="$stubs:/usr/bin:/bin" OMS_CAPABILITY_DIR="$live" OMS_CAPABILITY_SETTLE_WAIT=0 \
  OMS_CAPABILITY_RPC_WAIT=0 "$ROOT/scripts/models.sh" --refresh > "$TMP/refresh.out"
grep -Fqx 'models_probe=ok' "$live/antigravity.env" ||
  fail "a probe that returned a catalog must record ok: $(grep -E '^models_probe=' "$live/antigravity.env" || true)"
[ -s "$live/antigravity.models" ] || fail 'successful probe wrote no catalog'
grep -Fqx 'models_probe=skipped' "$live/claude.env" ||
  fail 'a provider with no listing source must record a skipped probe, not a failed one'
# The stub app-server answers nothing, which is what a timeout looks like here.
grep -Fqx 'models_probe=failed' "$live/codex.env" ||
  fail "a probe that returned no catalog must record failed: $(grep -E '^models_probe=' "$live/codex.env" || true)"
PATH="$stubs:/usr/bin:/bin" OMS_CAPABILITY_DIR="$live" python3 -c '
import json, subprocess, sys
out = subprocess.check_output([sys.argv[1], "--json"], text=True)
probes = {p["provider"]: p["catalog_probe"] for p in json.loads(out)["providers"]}
assert probes == {"codex": "failed", "claude": "unsupported", "antigravity": "ok"}, probes
' "$ROOT/scripts/models.sh"

echo 'models-smoke: ok'
