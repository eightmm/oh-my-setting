# shellcheck shell=bash
# What a provider CLI can actually do right now, cached.
#
# The routing table used to state capabilities as facts: "antigravity has no
# effort flag", "effort is one of low/medium/high". Both went stale while the
# CLIs moved — agy grew --effort, claude grew xhigh and max — and a stale
# capability is not a harmless comment: it made one provider refuse a control it
# supports and put two of another's tiers permanently out of reach. Ask the
# binary instead, and cache the answer so asking costs nothing per call.
#
# Detection widens what the registry declares; it never silently removes it.
# Codex takes effort through `-c model_reasoning_effort=`, which no --help text
# advertises, so a failed probe there must not be read as "no support".

oms_capability_cache_dir() {
  printf '%s\n' "${OMS_CAPABILITY_DIR:-$HOME/.cache/oh-my-setting/capabilities}"
}

# Identity of the binary we probed, so a different one — a test stub, a fresh
# install, an nvm switch — is a cache miss rather than a wrong answer.
oms_capability_binary_key() {
  local binary="$1"
  local path
  path="$(command -v "$binary" 2>/dev/null)" || { printf 'absent\n'; return 0; }
  path="$(oms_capability_resolve_path "$path")"
  printf '%s|%s\n' "$path" "$(oms_capability_stat "$path")"
}

oms_capability_resolve_path() {
  local src="$1"
  local dir
  # macOS readlink has no -f.
  while [ -L "$src" ]; do
    dir="$(cd "$(dirname "$src")" && pwd)"
    src="$(readlink "$src")"
    case "$src" in /*) ;; *) src="$dir/$src" ;; esac
  done
  printf '%s\n' "$src"
}

oms_capability_stat() {
  local path="$1"
  python3 - "$path" <<'PY' 2>/dev/null || printf 'unknown\n'
import os, sys
try:
    st = os.stat(sys.argv[1])
except OSError:
    print("missing")
else:
    print("%d.%d" % (st.st_size, int(st.st_mtime)))
PY
}

oms_capability_ttl() {
  local ttl="${OMS_CAPABILITY_TTL:-86400}"
  case "$ttl" in *[!0-9]*|"") ttl=86400 ;; esac
  printf '%s\n' "$ttl"
}

# Values a provider accepts for its thinking control, read out of --help.
# Returns nothing when the help text does not name them; the caller keeps the
# declared default rather than assuming the control is gone.
oms_capability_parse_effort_values() {
  local help_file="$1"
  local flag="$2"
  OMS_CAP_HELP="$help_file" OMS_CAP_FLAG="$flag" python3 - <<'PY'
import os, re

flag = os.environ["OMS_CAP_FLAG"]
try:
    with open(os.environ["OMS_CAP_HELP"], encoding="utf-8", errors="replace") as handle:
        lines = handle.read().splitlines()
except OSError:
    raise SystemExit(0)
for index, line in enumerate(lines):
    if flag not in line:
        continue
    # The values often wrap onto the next help line, so look at a small window.
    window = " ".join(lines[index:index + 3])
    match = re.search(r"\(([a-z][a-z0-9]*(?:\s*[,|]\s*[a-z][a-z0-9]*)+)\)", window)
    if match:
        print(" ".join(re.split(r"\s*[,|]\s*", match.group(1))))
        raise SystemExit(0)
PY
}

oms_capability_file() {
  printf '%s/%s.env\n' "$(oms_capability_cache_dir)" "$1"
}

oms_capability_read_field() {
  local file="$1" field="$2"
  [ -f "$file" ] || return 1
  sed -n "s/^$field=//p" "$file" | sed -n '1p'
}

# A CLI that hangs must not hang whatever asked for the snapshot. Where no
# timeout binary exists the probe still runs — refusing to learn a capability
# because a coreutils tool is missing would be worse than the risk.
oms_capability_run_bounded() {
  local seconds="$1" out="$2"
  shift 2
  if command -v timeout >/dev/null 2>&1; then
    timeout "$seconds" "$@" > "$out" 2>&1
  else
    "$@" > "$out" 2>&1
  fi
}

# Read a provider's catalog into MODELS_OUT (one model per line) and, where the
# source reports it, each model's own effort scale into EFFORTS_OUT
# ("<model>\t<space separated efforts>"). Local sources only: no model call is
# spent to find out which models exist.
oms_capability_probe_models() {
  local provider="$1" models_out="$2" efforts_out="$3"
  local binary raw
  binary="$(oms_provider_binary "$provider")"

  case "$(oms_provider_model_listing_kind "$provider")" in
    lines)
      oms_capability_run_bounded 30 "$models_out" "$binary" models
      ;;
    codex-app-server)
      # The app-server speaks JSON-RPC on stdio and answers model/list without
      # starting a conversation, so this costs nothing but a local process.
      raw="$(mktemp "${TMPDIR:-/tmp}/oms-cap-rpc.XXXXXX")" || return 1
      {
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"oh-my-setting","title":"oh-my-setting","version":"1"}}}'
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"model/list","params":{}}'
        sleep "${OMS_CAPABILITY_RPC_WAIT:-10}"
      } | oms_capability_run_bounded 40 "$raw" "$binary" app-server || true
      OMS_CAP_RAW="$raw" OMS_CAP_MODELS="$models_out" OMS_CAP_EFFORTS="$efforts_out" python3 - <<'PY'
import json, os

models, efforts = [], []
with open(os.environ["OMS_CAP_RAW"], encoding="utf-8", errors="replace") as handle:
    for line in handle:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        data = (msg.get("result") or {}).get("data")
        if not isinstance(data, list):
            continue
        for entry in data:
            name = entry.get("id") or entry.get("model")
            if not name or entry.get("hidden"):
                continue
            models.append(name)
            scale = [e.get("reasoningEffort") for e in entry.get("supportedReasoningEfforts") or []]
            scale = [s for s in scale if s]
            if scale:
                efforts.append("%s\t%s" % (name, " ".join(scale)))
if models:
    with open(os.environ["OMS_CAP_MODELS"], "w", encoding="utf-8") as handle:
        handle.write("\n".join(models) + "\n")
    if efforts:
        with open(os.environ["OMS_CAP_EFFORTS"], "w", encoding="utf-8") as handle:
            handle.write("\n".join(efforts) + "\n")
PY
      rm -f "$raw"
      ;;
    *) return 1 ;;
  esac
  [ -s "$models_out" ]
}

# Probe one provider and write the snapshot. Bounded: two short CLI calls at
# most, and a probe that fails leaves the declared defaults in place.
oms_capability_refresh() {
  local provider="$1"
  local binary flag help_file models_file tmp efforts_tmp values detected mechanism dir
  provider="$(oms_provider_normalize "$provider")" || return 2
  binary="$(oms_provider_binary "$provider")"
  mechanism="$(oms_provider_effort_mechanism "$provider")"
  flag="$(oms_provider_effort_flag "$provider")"
  dir="$(oms_capability_cache_dir)"
  mkdir -p "$dir" 2>/dev/null || return 1

  help_file="$(mktemp "${TMPDIR:-/tmp}/oms-cap-help.XXXXXX")" || return 1
  models_file="$dir/$provider.models"
  values=""
  if command -v "$binary" >/dev/null 2>&1; then
    local -a probe=("$binary")
    local arg
    for arg in $(oms_provider_help_args "$provider"); do probe+=("$arg"); done
    oms_capability_run_bounded 10 "$help_file" "${probe[@]}" || true
    values="$(oms_provider_effort_values "$provider")"
    if [ "$mechanism" = flag ]; then
      if grep -Fq -- "$flag" "$help_file"; then
        # The help text is the authority on the scale when it names one.
        detected="$(oms_capability_parse_effort_values "$help_file" "$flag")"
        [ -z "$detected" ] || values="$detected"
      else
        # Declared as a flag and the flag is not there: the control is gone.
        mechanism=none
        values=""
      fi
    fi
    if oms_provider_supports_model_listing "$provider" &&
      [ "${OMS_CAPABILITY_SKIP_MODELS:-0}" != 1 ]; then
      tmp="$(mktemp "${TMPDIR:-/tmp}/oms-cap-models.XXXXXX")" || tmp=""
      efforts_tmp="$(mktemp "${TMPDIR:-/tmp}/oms-cap-efforts.XXXXXX")" || efforts_tmp=""
      if [ -n "$tmp" ] && [ -n "$efforts_tmp" ]; then
        if oms_capability_probe_models "$provider" "$tmp" "$efforts_tmp"; then
          mv "$tmp" "$models_file"
          if [ -s "$efforts_tmp" ]; then
            mv "$efforts_tmp" "$dir/$provider.efforts"
          else
            rm -f "$efforts_tmp"
          fi
        else
          rm -f "$tmp" "$efforts_tmp"
        fi
      fi
    fi
  else
    mechanism=absent
  fi
  rm -f "$help_file"

  tmp="$(mktemp "$dir/.$provider.XXXXXX")" || return 1
  {
    printf 'binary_key=%s\n' "$(oms_capability_binary_key "$binary")"
    printf 'effort_mechanism=%s\n' "$mechanism"
    printf 'effort_values=%s\n' "$values"
    printf 'probed_at=%s\n' "$(date +%s)"
  } > "$tmp"
  mv "$tmp" "$(oms_capability_file "$provider")"
}

# Is the stored snapshot usable — present, fresh, and taken against the binary
# that is on PATH now?
oms_capability_snapshot_current() {
  local provider="$1"
  local file age stored
  file="$(oms_capability_file "$provider")"
  [ -f "$file" ] || return 1
  stored="$(oms_capability_read_field "$file" binary_key)"
  [ "$stored" = "$(oms_capability_binary_key "$(oms_provider_binary "$provider")")" ] || return 1
  age=$(( $(date +%s) - $(oms_capability_read_field "$file" probed_at 2>/dev/null || echo 0) ))
  [ "$age" -ge 0 ] && [ "$age" -lt "$(oms_capability_ttl)" ]
}

# Read what is known WITHOUT running anything. Routing calls this: deciding
# which model to use must not spawn a provider process — it would put a call
# the caller never asked for in front of every route, and make the decision
# cost and the provider's own invocation history depend on cache state.
# An absent snapshot means the registry declaration stands.
oms_capability_peek() {
  local provider="$1"
  local file union
  provider="$(oms_provider_normalize "$provider")" || return 2
  file="$(oms_capability_file "$provider")"
  oms_capability_snapshot_current "$provider" || file=""

  OMS_CAP_PROVIDER="$provider"
  OMS_CAP_EFFORT_MECHANISM=""
  OMS_CAP_EFFORT_VALUES=""
  if [ -n "$file" ]; then
    OMS_CAP_EFFORT_MECHANISM="$(oms_capability_read_field "$file" effort_mechanism)"
    OMS_CAP_EFFORT_VALUES="$(oms_capability_read_field "$file" effort_values)"
  fi
  # Nothing probed yet (fresh machine, or a cache this host cannot write): the
  # registry declaration stands. A provider is never reported as incapable
  # merely because no one has looked.
  if [ -z "$OMS_CAP_EFFORT_MECHANISM" ]; then
    OMS_CAP_EFFORT_MECHANISM="$(oms_provider_effort_mechanism "$provider")"
    OMS_CAP_EFFORT_VALUES="$(oms_provider_effort_values "$provider")"
  fi
  # Where the catalog reports a scale per model, the provider's scale is the
  # union of them: anything narrower would reject an effort one of its models
  # takes, before there is a model to check it against. Codex needs this — its
  # mechanism is invisible to --help, so the declaration alone stops at high
  # while gpt-5.6-sol reaches ultra.
  union="$(oms_capability_effort_union "$provider")"
  [ -z "$union" ] || OMS_CAP_EFFORT_VALUES="$union"
  export OMS_CAP_PROVIDER OMS_CAP_EFFORT_MECHANISM OMS_CAP_EFFORT_VALUES
}

# The live catalog, when the CLI has a listing command and the probe worked.
# Empty output means "unknown", never "empty catalog".
oms_capability_models() {
  local provider="$1"
  provider="$(oms_provider_normalize "$provider")" || return 2
  local file
  file="$(oms_capability_cache_dir)/$provider.models"
  [ -f "$file" ] || return 1
  cat "$file"
}

# A CLI prints its catalog in one notation and accepts another: agy lists
# `gemini-3.6-flash-high` and takes `Gemini 3.6 Flash (High)`. Compare on the
# letters and digits alone, the same rule model-doctor uses.
oms_model_catalog_key() {
  LC_ALL=C printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9'
}

# Is MODEL in the provider's live catalog? Exit 2 means "no catalog known",
# which is not the same as absent and must not be treated as one.
oms_capability_model_available() {
  local provider="$1" model="$2"
  local catalog key line
  catalog="$(oms_capability_models "$provider" 2>/dev/null)" || return 2
  [ -n "$catalog" ] || return 2
  key="$(oms_model_catalog_key "$model")"
  [ -n "$key" ] || return 2
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    case "$(oms_model_catalog_key "$line")" in
      "$key") return 0 ;;
    esac
  done <<EOF
$catalog
EOF
  return 1
}

# Every effort any of a provider's models accepts, in scale order. Empty when
# the catalog says nothing per model.
oms_capability_effort_union() {
  local provider="$1"
  local file candidate out=""
  file="$(oms_capability_cache_dir)/$provider.efforts"
  [ -f "$file" ] || return 0
  for candidate in low medium high xhigh max ultra; do
    if cut -f2 "$file" | grep -qw -- "$candidate"; then
      out="${out:+$out }$candidate"
    fi
  done
  printf '%s\n' "$out"
}

# The effort scale of ONE model, where the catalog reports it. Codex publishes
# a different scale per model — gpt-5.6-sol reaches ultra, gpt-5.5 stops at
# xhigh — so a provider-wide answer is the wrong shape for it. Exit 1 means the
# catalog knows nothing about this model's scale, not that it has none.
oms_capability_model_efforts() {
  local provider="$1" model="$2"
  local file key line
  provider="$(oms_provider_normalize "$provider")" || return 2
  file="$(oms_capability_cache_dir)/$provider.efforts"
  [ -f "$file" ] || return 1
  key="$(oms_model_catalog_key "$model")"
  while IFS="$(printf '\t')" read -r name scale; do
    [ -n "$name" ] || continue
    [ "$(oms_model_catalog_key "$name")" = "$key" ] || continue
    printf '%s\n' "$scale"
    return 0
  done < "$file"
  return 1
}

# Does this provider accept EFFORT right now?
oms_capability_supports_effort() {
  local provider="$1" effort="$2"
  oms_capability_peek "$provider" || return 2
  case "$OMS_CAP_EFFORT_MECHANISM" in none|absent) return 1 ;; esac
  case " $OMS_CAP_EFFORT_VALUES " in *" $effort "*) return 0 ;; esac
  return 1
}

# Highest supported effort at or below WANT, so a tier maps onto the scale the
# provider actually has instead of failing or overshooting it.
oms_capability_clamp_effort() {
  local provider="$1" want="$2"
  local order="low medium high xhigh max"
  local candidate result=""
  oms_capability_peek "$provider" || return 2
  case "$OMS_CAP_EFFORT_MECHANISM" in none|absent) return 1 ;; esac
  for candidate in $order; do
    case " $OMS_CAP_EFFORT_VALUES " in *" $candidate "*) ;; *) continue ;; esac
    result="$candidate"
    [ "$candidate" != "$want" ] || break
  done
  [ -n "$result" ] || return 1
  printf '%s\n' "$result"
}
