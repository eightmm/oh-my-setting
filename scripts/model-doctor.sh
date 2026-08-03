#!/usr/bin/env bash
set -euo pipefail

# Diagnose provider binaries and cached capabilities without selecting models.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/model-capability.sh
. "$ROOT/scripts/lib/model-capability.sh"
# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"

JSON=0 LIVE=0 REQUIRE_ALL=0 STRICT=0 PROVIDERS="codex,claude,antigravity"
usage() { cat <<'EOF'
Usage: model-doctor.sh [--json] [--live-models] [--require-all] [--strict-diversity] [--providers CSV]

Check installed provider CLIs, their default invocation surface, and cached
model/effort capabilities. --live-models refreshes catalog-capable providers.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON=1 ;;
    --live-models) LIVE=1 ;;
    --require-all) REQUIRE_ALL=1 ;;
    --strict-diversity) STRICT=1 ;;
    --providers) shift; [ "$#" -gt 0 ] || { echo 'error: --providers requires CSV' >&2; exit 2; }; PROVIDERS="$1" ;;
    --timeout) shift; [ "$#" -gt 0 ] || { echo 'error: --timeout requires seconds' >&2; exit 2; } ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
PROVIDERS="$(oms_provider_normalize_list "$PROVIDERS")" || exit $?
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
rows="$tmp/rows.jsonl"; : > "$rows"
IFS=',' read -r -a values <<< "$PROVIDERS"
for provider in "${values[@]}"; do
  binary="$(oms_provider_binary "$provider")"; installed=false; version=""; default_reachable=false
  if command -v "$binary" >/dev/null 2>&1; then
    installed=true
    version="$($binary --version 2>&1 | head -n 1 || true)"
    # A help invocation confirms the CLI can start without forcing a model.
    case "$provider" in codex) "$binary" exec --help >/dev/null 2>&1 ;; *) "$binary" --help >/dev/null 2>&1 ;; esac && default_reachable=true
    if [ "$LIVE" -eq 1 ]; then oms_capability_refresh "$provider" || true; else OMS_CAPABILITY_SKIP_MODELS=1 oms_capability_refresh "$provider" || true; fi
  fi
  file="$(oms_capability_file "$provider")"
  family="$(oms_provider_model_family "$provider" provider-default)"
  python3 - "$provider" "$binary" "$installed" "$version" "$default_reachable" "$family" \
    "$(oms_capability_read_field "$file" effort_mechanism 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" effort_values 2>/dev/null || true)" >> "$rows" <<'PY'
import json, sys
p,b,i,v,d,f,m,e=sys.argv[1:]
print(json.dumps({"provider":p,"binary":b,"installed":i=="true","version":v or None,
 "provider_default_reachable":d=="true","family":f,"effort_mechanism":m or None,
 "effort_values":e.split() if e else []}))
PY
done
result="$tmp/result.json"
python3 - "$rows" "$LIVE" "$REQUIRE_ALL" "$STRICT" > "$result" <<'PY'
import json, sys
providers=[json.loads(x) for x in open(sys.argv[1]) if x.strip()]
live, require, strict=(x=="1" for x in sys.argv[2:])
errors=[]; warnings=[]
for p in providers:
    if not p["installed"]: (errors if require else warnings).append("%s: provider binary '%s' is not installed" % (p["provider"],p["binary"]))
    elif not p["provider_default_reachable"]: errors.append("%s: provider default invocation is not reachable" % p["provider"])
families={p["family"] for p in providers if p["installed"] and p["provider_default_reachable"] and p["family"] != "unknown"}
if strict and len(families)<2: errors.append("model-family diversity needs at least two usable families")
print(json.dumps({"schema":2,"ok":not errors,"live_models":live,"require_all":require,"strict_diversity":strict,"providers":providers,"warnings":warnings,"errors":errors},sort_keys=True))
PY
if [ "$JSON" -eq 1 ]; then cat "$result"; else
  python3 - "$result" <<'PY'
import json,sys
x=json.load(open(sys.argv[1])); print("# oh-my-setting model doctor\n")
for p in x["providers"]: print("%s: %s; default=%s; effort=%s"%(p["provider"],"installed" if p["installed"] else "missing",p["provider_default_reachable"],p["effort_mechanism"] or "unknown"))
for w in x["warnings"]: print("warning: "+w)
for e in x["errors"]: print("error: "+e)
print("model-doctor: ok" if x["ok"] else "model-doctor: FAILED")
PY
fi
python3 - "$result" <<'PY'
import json,sys
raise SystemExit(0 if json.load(open(sys.argv[1]))["ok"] else 1)
PY
