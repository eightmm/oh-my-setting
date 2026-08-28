#!/usr/bin/env bash
set -euo pipefail

# Diagnose provider binaries and cached capabilities without selecting models.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/model-capability.sh
. "$ROOT/scripts/lib/model-capability.sh"
# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"

JSON=0 LIVE=0 REQUIRE_ALL=0 STRICT=0 PROVIDERS=default
usage() { cat <<'EOF'
Usage: model-doctor.sh [--json] [--live-models] [--require-all] [--strict-diversity] [--providers default|auto|all|CSV]

Check installed provider CLIs, their default invocation surface, and cached
model/effort capabilities. --live-models refreshes catalog-capable providers.
The default includes the historical core and installed optional agents; auto
checks only installed agents, and all also reports absent built-ins.
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
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || {
  echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
  exit 2
}
PROVIDER_NAMES="$(oms_provider_selection_names "$PROVIDERS")" || exit $?
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
rows="$tmp/rows.jsonl"; : > "$rows"
while IFS= read -r provider; do
  [ -n "$provider" ] || continue
  binary="$(oms_provider_binary "$provider")"; installed=false; version=""; default_reachable=false
  if command -v "$binary" >/dev/null 2>&1; then
    installed=true
    version_out="$tmp/$provider.version"
    help_out="$tmp/$provider.help"
    version_cmd=("$binary")
    for version_arg in $(oms_provider_version_args "$provider"); do
      version_cmd+=("$version_arg")
    done
    oms_capability_run_bounded 10 "$version_out" "${version_cmd[@]}" </dev/null || true
    version="$(sed -n '1p' "$version_out" 2>/dev/null || true)"
    help_cmd=("$binary")
    for help_arg in $(oms_provider_help_args "$provider"); do
      help_cmd+=("$help_arg")
    done
    # A bounded help invocation confirms the CLI can start without forcing a
    # model, login, or inference request. Grok also suppresses update checks.
    if oms_capability_run_bounded 10 "$help_out" "${help_cmd[@]}" </dev/null; then
      default_reachable=true
    fi
    if [ "$LIVE" -eq 1 ]; then
      oms_capability_refresh "$provider" "$help_out" || true
    else
      OMS_CAPABILITY_SKIP_MODELS=1 oms_capability_refresh "$provider" "$help_out" || true
    fi
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
done <<EOF
$PROVIDER_NAMES
EOF
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
