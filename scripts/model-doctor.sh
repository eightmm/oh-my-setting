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
The default includes the historical core and detected optional agents; auto
checks only detected agents, and all also reports absent built-ins. A detected
but unusable executable is reported as broken rather than hidden.
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
tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
provider_names_file="$tmp/providers"
# Doctor is an inventory surface, not a router. Keep discovered-but-broken
# executables in the report; only the routing pool filters them by usability.
oms_provider_selection_discovered_names "$PROVIDERS" > "$provider_names_file" || exit $?
PROVIDER_NAMES="$(cat "$provider_names_file")"
rows="$tmp/rows.jsonl"; : > "$rows"
while IFS= read -r provider; do
  [ -n "$provider" ] || continue
  binary="$(oms_provider_binary "$provider")"; installed=false; usable=false; version=""; default_reachable=false
  oms_provider_cli_discovered "$provider" && installed=true
  oms_provider_cli_available "$provider" && usable=true
  if [ "$usable" = true ]; then
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
  # The configured default is the one route the router never looks at: after a
  # generation rotation a config pin keeps naming last year's model in
  # silence. Codex declares its default in config.toml; checked against the
  # routable set, warning only, because the pin still runs.
  configured="" configured_standing="" routable=""
  if [ "$provider" = codex ]; then
    codex_config="${OMS_CODEX_CONFIG:-${CODEX_HOME:-$HOME/.codex}/config.toml}"
    [ ! -f "$codex_config" ] ||
      configured="$(sed -n 's/^model[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$codex_config" | head -n 1)"
  fi
  if [ -n "$configured" ]; then
    standing=0
    oms_capability_model_routable "$provider" "$configured" || standing=$?
    case "$standing" in
      0) configured_standing=routable ;;
      1) configured_standing=not-routable ;;
    esac
    routable="$(oms_capability_routable_models "$provider" 2>/dev/null | tr '\n' ' ' || true)"
  fi
  python3 - "$provider" "$binary" "$installed" "$usable" "$version" "$default_reachable" "$family" \
    "$(oms_capability_read_field "$file" effort_mechanism 2>/dev/null || true)" \
    "$(oms_capability_read_field "$file" effort_values 2>/dev/null || true)" \
    "$configured" "$configured_standing" "$routable" >> "$rows" <<'PY'
import json, sys
p,b,i,u,v,d,f,m,e,c,cs,r=sys.argv[1:]
print(json.dumps({"provider":p,"binary":b,"installed":i=="true","usable":u=="true","version":v or None,
 "provider_default_reachable":d=="true","family":f,"effort_mechanism":m or None,
 "effort_values":e.split() if e else [],"configured_default":c or None,
 "configured_default_routable":{"routable":True,"not-routable":False}.get(cs),
 "routable":r.split()}))
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
    elif not p["usable"]: errors.append("%s: provider binary is present but its bounded version/help probe failed" % p["provider"])
    elif not p["provider_default_reachable"]: errors.append("%s: provider default invocation is not reachable" % p["provider"])
    if p.get("configured_default_routable") is False:
        warnings.append("%s: configured default model %s is not routable (previous generation or foreign family); routable: %s"
                        % (p["provider"], p["configured_default"], ", ".join(p["routable"]) or "none"))
families={p["family"] for p in providers if p["usable"] and p["provider_default_reachable"] and p["family"] != "unknown"}
if strict and len(families)<2: errors.append("model-family diversity needs at least two usable families")
print(json.dumps({"schema":2,"ok":not errors,"live_models":live,"require_all":require,"strict_diversity":strict,"providers":providers,"warnings":warnings,"errors":errors},sort_keys=True))
PY
if [ "$JSON" -eq 1 ]; then cat "$result"; else
  python3 - "$result" <<'PY'
import json,sys
x=json.load(open(sys.argv[1])); print("# oh-my-setting model doctor\n")
for p in x["providers"]:
    status = "installed" if p["usable"] else ("broken" if p["installed"] else "missing")
    print("%s: %s; default=%s; effort=%s"%(p["provider"],status,p["provider_default_reachable"],p["effort_mechanism"] or "unknown"))
for w in x["warnings"]: print("warning: "+w)
for e in x["errors"]: print("error: "+e)
print("model-doctor: ok" if x["ok"] else "model-doctor: FAILED")
PY
fi
python3 - "$result" <<'PY'
import json,sys
raise SystemExit(0 if json.load(open(sys.argv[1]))["ok"] else 1)
PY
