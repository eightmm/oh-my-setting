#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-models.XXXXXX")"; trap 'rm -rf "$TMP"' EXIT HUP INT TERM
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
PY
echo 'models-smoke: ok'
