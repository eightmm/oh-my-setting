#!/usr/bin/env bash
set -euo pipefail

# Print cached provider capabilities so callers can choose an explicit model.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/model-capability.sh
. "$ROOT/scripts/lib/model-capability.sh"
# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"

JSON=0
REFRESH=0
usage() {
  cat <<'EOF'
Usage: models.sh [--json] [--refresh]

Show cached provider model catalogs and reasoning-effort capabilities. --refresh
updates the cache first; without it this command never invokes a provider CLI.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON=1 ;;
    --refresh) REFRESH=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-models.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
rows="$tmp/rows.jsonl"
: > "$rows"
for provider in codex claude antigravity; do
  binary="$(oms_provider_binary "$provider")"
  if [ "$REFRESH" -eq 1 ]; then oms_capability_refresh "$provider" || true; fi
  file="$(oms_capability_file "$provider")"
  models_file="$(oms_capability_cache_dir)/$provider.models"
  efforts_file="$(oms_capability_cache_dir)/$provider.efforts"
  OMS_MODELS_FILE="$models_file" OMS_EFFORTS_FILE="$efforts_file" python3 - \
    "$provider" "$binary" "$(command -v "$binary" >/dev/null 2>&1 && echo true || echo false)" \
    "$(oms_capability_read_field "$file" effort_mechanism 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" effort_values 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" probed_at 2>/dev/null || true)" >> "$rows" <<'PY'
import json, os, sys, time
provider, binary, present, mechanism, values, probed = sys.argv[1:]
def lines(path, limit=20):
    try:
        with open(path, encoding="utf-8") as f:
            return [x.rstrip("\n") for x in f if x.rstrip("\n")][:limit]
    except OSError: return []
def scales(path):
    result = {}
    try:
        with open(path, encoding="utf-8") as f:
            for row in f:
                name, sep, scale = row.rstrip("\n").partition("\t")
                if sep: result[name] = scale.split()
    except OSError: pass
    return result
try: age = max(0, int(time.time()) - int(probed))
except ValueError: age = None
print(json.dumps({"provider": provider, "binary": binary, "present": present == "true",
 "models": lines(os.environ["OMS_MODELS_FILE"]), "effort_mechanism": mechanism or None,
 "effort_values": values.split() if values else [], "model_effort_scales": scales(os.environ["OMS_EFFORTS_FILE"]),
 "snapshot_age_seconds": age}, ensure_ascii=False))
PY
done
if [ "$JSON" -eq 1 ]; then
  python3 - "$rows" <<'PY'
import json, sys
print(json.dumps({"schema": 1, "providers": [json.loads(x) for x in open(sys.argv[1]) if x.strip()]}, ensure_ascii=False, sort_keys=True))
PY
else
  python3 - "$rows" <<'PY'
import json, sys
for raw in open(sys.argv[1]):
    row=json.loads(raw); print("## %s (%s)" % (row["provider"], "present" if row["present"] else "absent"))
    print("models: " + (", ".join(row["models"]) or "unknown"))
    print("effort: %s %s" % (row["effort_mechanism"] or "unknown", " ".join(row["effort_values"])))
    print("snapshot age: %s" % (str(row["snapshot_age_seconds"]) + "s" if row["snapshot_age_seconds"] is not None else "unknown"))
PY
fi
