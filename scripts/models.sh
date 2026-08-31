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
PROVIDERS=default
usage() {
  cat <<'EOF'
Usage: models.sh [--json] [--refresh] [--providers default|auto|all|CSV]

Show cached provider model catalogs and reasoning-effort capabilities. --refresh
updates the cache first; without it this command never invokes a provider CLI.
The models line is the routable set — the provider's own family at its newest
generation; previous generations and re-hosted foreign families are listed
apart and are never chosen unless named with --model.
The default is the historical core plus detected optional agents; auto means
only detected agents, and all also shows absent built-in transports. With
--refresh, a detected executable that fails the bounded probe remains visible
as broken and is excluded from routing.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON=1 ;;
    --refresh) REFRESH=1 ;;
    --providers)
      shift
      [ "$#" -gt 0 ] || { echo 'error: --providers requires a selector' >&2; exit 2; }
      PROVIDERS="$1"
      ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || [ "$REFRESH" -eq 0 ] || {
  echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
  exit 2
}
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-models.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
provider_names_file="$tmp/providers"
# Inventory always starts from physical discovery. A refresh adds a bounded
# usability verdict; it must not make a broken PATH entry disappear from the
# very diagnostic that should explain why routing excluded it.
oms_provider_selection_discovered_names "$PROVIDERS" > "$provider_names_file" || exit $?
PROVIDER_NAMES="$(cat "$provider_names_file")"
rows="$tmp/rows.jsonl"
: > "$rows"
while IFS= read -r provider; do
  [ -n "$provider" ] || continue
  binary="$(oms_provider_binary "$provider")"
  present=false
  usable=unknown
  model_override=false
  oms_provider_cli_discovered "$provider" && present=true
  if [ "$REFRESH" -eq 1 ]; then
    usable=false
    oms_provider_cli_available "$provider" && usable=true
  fi
  oms_provider_supports_model_override "$provider" && model_override=true
  if [ "$REFRESH" -eq 1 ] && [ "$usable" = true ]; then
    oms_capability_refresh "$provider" || true
  fi
  file="$(oms_capability_file "$provider")"
  models_file="$(oms_capability_cache_dir)/$provider.models"
  efforts_file="$(oms_capability_cache_dir)/$provider.efforts"
  routable_file="$tmp/$provider.routable"
  oms_capability_routable_models "$provider" > "$routable_file" 2>/dev/null || : > "$routable_file"
  OMS_MODELS_FILE="$models_file" OMS_EFFORTS_FILE="$efforts_file" OMS_ROUTABLE_FILE="$routable_file" python3 - \
    "$provider" "$binary" "$present" "$usable" "$model_override" \
    "$(oms_capability_read_field "$file" effort_mechanism 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" effort_values 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" probed_at 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" models_probe 2>/dev/null || true)" \
    "$(oms_provider_model_listing_kind "$provider")" >> "$rows" <<'PY'
import json, os, sys, time
provider, binary, present, usable, model_override, mechanism, values, probed, probe, listing = sys.argv[1:]
def lines(path, limit=20):
    try:
        with open(path, encoding="utf-8") as f:
            rows = [x.rstrip("\n") for x in f if x.rstrip("\n")]
            return rows if limit is None else rows[:limit]
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
# "unsupported" is a property of the CLI, so it is derived here rather than
# stored: a snapshot taken before the provider grew a listing command would
# otherwise keep asserting it has none.
catalog_probe = "unsupported" if listing == "none" else (probe or "unknown")
usable_value = None if usable == "unknown" else usable == "true"
# The catalog is what exists; the routable set is what a route may pick on its
# own — the provider's own family at its newest generation. The rest stays
# visible, named for what it is, and is never chosen unless named.
models = lines(os.environ["OMS_MODELS_FILE"])
routable = [m for m in lines(os.environ["OMS_ROUTABLE_FILE"], limit=None) if m in models]
print(json.dumps({"provider": provider, "binary": binary, "present": present == "true",
 "usable": usable_value, "exact_model_override": model_override == "true",
 "models": models, "routable": routable, "not_routed": [m for m in models if m not in routable],
 "effort_mechanism": mechanism or None,
 "effort_values": values.split() if values else [], "model_effort_scales": scales(os.environ["OMS_EFFORTS_FILE"]),
 "catalog_probe": catalog_probe, "snapshot_age_seconds": age}, ensure_ascii=False))
PY
done <<EOF
$PROVIDER_NAMES
EOF
if [ "$JSON" -eq 1 ]; then
  python3 - "$rows" <<'PY'
import json, sys
print(json.dumps({"schema": 1, "providers": [json.loads(x) for x in open(sys.argv[1]) if x.strip()]}, ensure_ascii=False, sort_keys=True))
PY
else
  python3 - "$rows" <<'PY'
import json, sys
HINT = "'oms models --refresh' (local catalog listing, no model tokens)"
for raw in open(sys.argv[1]):
    row=json.loads(raw)
    status = ("usable" if row["usable"] else "broken") if row["usable"] is not None else ("present" if row["present"] else "absent")
    print("## %s (%s)" % (row["provider"], status))
    # Catalog presence decides, then catalog_probe only explains an empty one:
    # a run that skips the probe rewrites the snapshot beside a catalog an
    # earlier --refresh wrote, and a repair named there points at nothing.
    if row["models"]:
        print("models: " + (", ".join(row["routable"]) or "none routable"))
        if row["not_routed"]:
            print("not routed (previous generation or foreign family): " + ", ".join(row["not_routed"]))
    elif row["catalog_probe"] == "unsupported":
        print("models: no local catalog; the CLI validates --model at call time")
    else:
        lead = "last probe failed; retry with " if row["catalog_probe"] == "failed" else "probe with "
        print("models: unknown — " + lead + HINT)
    print("effort: %s %s" % (row["effort_mechanism"] or "unknown", " ".join(row["effort_values"])))
    print("exact model override: %s" % row["exact_model_override"])
    print("snapshot age: %s" % (str(row["snapshot_age_seconds"]) + "s" if row["snapshot_age_seconds"] is not None else "unknown"))
PY
fi
