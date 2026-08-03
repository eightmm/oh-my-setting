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
warn="$(PATH="$TMP/bin:$PATH" OMS_CAPABILITY_DIR="$TMP/cap" OMS_MODEL_CLASS_REQUEST=deep OMS_MODEL_EXPLICIT='' OMS_REASONING_EFFORT_REQUEST=auto bash -c '. "'$ROOT'/scripts/lib/model-routing.sh"; oms_model_prepare codex' 2>&1 >/dev/null)"
printf '%s' "$warn" | grep -Fq 'warning: model tiers were removed' || fail 'tier shim did not warn'
echo 'model-routing-smoke: ok'
