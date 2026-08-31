# shellcheck shell=bash
# Provider-neutral model selection. Capability facts come from the cached
# snapshot; callers either name a model or leave the provider to choose.

OMS_MODEL_ROUTING_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/provider-registry.sh
. "$OMS_MODEL_ROUTING_LIB_DIR/provider-registry.sh"
# shellcheck source=scripts/lib/model-capability.sh
. "$OMS_MODEL_ROUTING_LIB_DIR/model-capability.sh"

oms_model_validate_name() {
  local value="$1"
  [ "${#value}" -le 160 ] || { echo "error: model name exceeds 160 characters" >&2; return 2; }
  if LC_ALL=C printf '%s' "$value" | grep -q '[[:cntrl:]]'; then
    echo "error: model name contains control characters" >&2
    return 2
  fi
  if [ "$value" = provider-default ]; then
    echo "error: provider-default is reserved; omit --model to use the provider default" >&2
    return 2
  fi
}

# Syntax only. Provider and model support are checked against the capability
# snapshot below.
oms_reasoning_validate() {
  case "$1" in auto|low|medium|high|xhigh|max|ultra) return 0 ;; esac
  echo "error: reasoning effort must be auto, low, medium, high, xhigh, max, or ultra" >&2
  return 2
}

oms_reasoning_provider_validate() {
  local provider="$1" effort="$2" model="${3:-}"
  local scale=""

  [ "$effort" != auto ] || return 0
  oms_capability_peek "$provider" || return 2
  case "$OMS_CAP_EFFORT_MECHANISM" in
    none|absent)
      echo "error: $provider takes no reasoning-effort control; use --model" >&2
      return 2
      ;;
  esac
  [ -z "$model" ] || scale="$(oms_capability_model_efforts "$provider" "$model" 2>/dev/null || true)"
  [ -n "$scale" ] || scale="$OMS_CAP_EFFORT_VALUES"
  case " $scale " in *" $effort "*) return 0 ;; esac
  if [ -n "$model" ] && [ "$scale" != "$OMS_CAP_EFFORT_VALUES" ]; then
    echo "error: $provider model $model does not accept reasoning effort '$effort' (it accepts: $scale)" >&2
  else
    echo "error: $provider does not accept reasoning effort '$effort' (it accepts: $scale)" >&2
  fi
  return 2
}

# A catalog is an advisory source for retry candidates. Its absence must not
# invent a provider-specific model name. Only the routable set is a candidate:
# recovery never lands on a previous generation or a re-hosted foreign family.
oms_model_alternative() {
  local provider="$1" current="$2" candidate current_key candidate_key
  current_key="$(oms_model_catalog_key "$current")"
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    candidate_key="$(oms_model_catalog_key "$candidate")"
    [ "$candidate_key" = "$current_key" ] && continue
    printf '%s\n' "$candidate"
    return 0
  done <<EOF
$(oms_capability_routable_models "$provider" 2>/dev/null || true)
EOF
  [ "$current" = provider-default ] || printf '%s\n' provider-default
}

# Emit each routable catalog model once, excluding aliases of CURRENT. With no
# catalog, the only safe recovery is to let the provider select its default.
oms_model_distinct_chain() {
  local provider="$1" current="$2" candidate current_key candidate_key seen_key duplicate
  local -a seen=()
  current_key="$(oms_model_catalog_key "$current")"
  seen+=("$current_key")
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    candidate_key="$(oms_model_catalog_key "$candidate")"
    duplicate=0
    for seen_key in "${seen[@]}"; do
      [ "$candidate_key" = "$seen_key" ] && { duplicate=1; break; }
    done
    [ "$duplicate" -eq 0 ] || continue
    seen+=("$candidate_key")
    printf '%s\n' "$candidate"
  done <<EOF
$(oms_capability_routable_models "$provider" 2>/dev/null || true)
EOF
  if ! oms_capability_models "$provider" >/dev/null 2>&1 && [ "$current" != provider-default ]; then
    printf '%s\n' provider-default
  fi
}

# Which seeded worker route an operation takes. A write delegate uses the
# second price rank; judging calls keep their account-specific provider
# default. The worker rank is therefore a stable role preset, not a claim that
# every account's provider default is the first seed entry.
# Exit 1: no role-shaped route here.
oms_model_role_rank() {
  case "${OMS_MODEL_OPERATION:-}" in
    delegate) printf 'worker\n' ;;
    *) return 1 ;;
  esac
}

# Emit the seed in price order, intersected with the exact current routable
# catalog when one exists. Emit the catalog spelling, not the seed spelling,
# so a normalized alias cannot turn into a model the provider did not list.
oms_model_role_ranked_candidates() {
  local provider="$1" routable generation order candidate key model model_name
  local first
  [ "${OMS_ROLE_ROUTING:-1}" != 0 ] || return 1
  routable="$(oms_capability_routable_models "$provider" 2>/dev/null || true)"
  generation=""
  if [ -n "$routable" ]; then
    first="$(printf '%s\n' "$routable" | sed -n '1p')"
    generation="$(oms_model_generation "${first%%	*}" 2>/dev/null || true)"
  fi
  order="$(oms_provider_price_order "$provider" "$generation" 2>/dev/null)" || {
    echo "note: no price order seeded for $provider generation ${generation:-unknown}; the provider default runs (seed oms_provider_price_order)" >&2
    return 1
  }
  # shellcheck disable=SC2086
  for candidate in $order; do
    if [ -n "$routable" ]; then
      key="$(oms_model_catalog_key "$candidate")"
      while IFS= read -r model; do
        [ -n "$model" ] || continue
        model_name="${model%%	*}"
        if [ "$(oms_model_catalog_key "$model_name")" = "$key" ]; then
          printf '%s\n' "$model_name"
          break
        fi
      done <<EOF
$routable
EOF
    else
      printf '%s\n' "$candidate"
    fi
  done
}

# Select this operation's role preset from the ranked candidates.
oms_model_role_default() {
  local provider="$1" role ranked
  [ "${OMS_ROLE_ROUTING:-1}" != 0 ] || return 1
  role="$(oms_model_role_rank)" || return 1
  ranked="$(oms_model_role_ranked_candidates "$provider")" || return 1
  [ -n "$ranked" ] || return 1
  # shellcheck disable=SC2086
  set -- $ranked
  case "$role" in
    worker) printf '%s\n' "${2:-$1}" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

# A worker that cannot use its seeded rank tries cheaper candidates first and
# only then higher ranks. This preserves availability without making the first
# recovery an accidental cost escalation.
oms_model_role_recovery_chain() {
  local provider="$1" current="$2" ranked candidate key current_key
  local lower="" higher="" after=0
  ranked="$(oms_model_role_ranked_candidates "$provider")" || return 1
  current_key="$(oms_model_catalog_key "$current")"
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    key="$(oms_model_catalog_key "$candidate")"
    if [ "$key" = "$current_key" ]; then
      after=1
    elif [ "$after" = 1 ]; then
      lower="${lower}${lower:+$'\n'}$candidate"
    else
      higher="${higher}${higher:+$'\n'}$candidate"
    fi
  done <<EOF
$ranked
EOF
  [ -z "$lower" ] || printf '%s\n' "$lower"
  [ -z "$higher" ] || printf '%s\n' "$higher"
}

oms_model_prepare() {
  local provider="$1" explicit="${OMS_MODEL_EXPLICIT:-}"
  local explicit_fallback="${OMS_MODEL_FALLBACK_EXPLICIT:-}"
  local effort_requested="${OMS_REASONING_EFFORT_REQUEST:-auto}"
  local effort_fallback_explicit="${OMS_REASONING_FALLBACK_EXPLICIT:-}"
  local candidate filtered_chain="" standing role_model role_chain

  provider="$(oms_provider_normalize "$provider")" || return $?
  oms_model_validate_name "$explicit" || return $?
  oms_model_validate_name "$explicit_fallback" || return $?
  oms_reasoning_validate "$effort_requested" || return $?
  if [ -n "$effort_fallback_explicit" ]; then
    oms_reasoning_validate "$effort_fallback_explicit" || return $?
    [ "$effort_fallback_explicit" != auto ] || { echo 'error: explicit fallback reasoning effort cannot be auto' >&2; return 2; }
  fi

  OMS_MODEL_PREVIOUS_GENERATION=0
  if [ -n "$explicit" ]; then
    OMS_MODEL_PRIMARY="$explicit"
    OMS_MODEL_RESOLVED_CLASS=explicit
    OMS_MODEL_CLASS_REASON=explicit
    # Named is named: the call runs. It is only said aloud, once, because a
    # pin that outlived its generation otherwise keeps running in silence.
    standing=0
    oms_capability_model_routable "$provider" "$explicit" || standing=$?
    if [ "$standing" -eq 1 ]; then
      OMS_MODEL_PREVIOUS_GENERATION=1
      echo "warning: $provider model $explicit is not in the routable set (previous generation or foreign family); it runs only because it was named — routable: $(oms_capability_routable_models "$provider" 2>/dev/null | tr '\n' ' ')" >&2
    fi
  else
    OMS_MODEL_PRIMARY="provider-default"
    OMS_MODEL_RESOLVED_CLASS="provider-default"
    OMS_MODEL_CLASS_REASON="provider-default"
    if role_model="$(oms_model_role_default "$provider")" && [ -n "$role_model" ]; then
      OMS_MODEL_PRIMARY="$role_model"
      OMS_MODEL_RESOLVED_CLASS="role-default"
      OMS_MODEL_CLASS_REASON="role:$(oms_model_role_rank)"
    fi
  fi
  if [ "$effort_requested" != auto ]; then
    if [ "${OMS_REASONING_CLAMP:-0}" = 1 ]; then
      effort_requested="$(oms_capability_clamp_effort "$provider" "$effort_requested" "${explicit:-}" 2>/dev/null || printf '%s' "$effort_requested")"
    fi
    oms_reasoning_provider_validate "$provider" "$effort_requested" \
      "${explicit:-}" || return $?
    OMS_REASONING_RESOLVED="$effort_requested"
    OMS_REASONING_EXPLICIT=1
  else
    OMS_REASONING_RESOLVED=""
    OMS_REASONING_EXPLICIT=0
  fi
  if [ -n "$effort_fallback_explicit" ]; then
    oms_reasoning_provider_validate "$provider" "$effort_fallback_explicit" \
      "${explicit_fallback:-}" || return $?
  fi

  OMS_MODEL_FALLBACK=""
  if [ -n "$explicit_fallback" ]; then
    OMS_MODEL_FALLBACK="$explicit_fallback"
  fi
  OMS_REASONING_FALLBACK="$effort_fallback_explicit"
  OMS_REASONING_SELECTED="$OMS_REASONING_RESOLVED"
  OMS_MODEL_ALTERNATE=""
  OMS_MODEL_DISTINCT_CHAIN=""
  if [ -z "$explicit" ]; then
    OMS_MODEL_ALTERNATE="$(oms_model_alternative "$provider" "$OMS_MODEL_PRIMARY" || true)"
    OMS_MODEL_DISTINCT_CHAIN="$(oms_model_distinct_chain "$provider" "$OMS_MODEL_PRIMARY" || true)"
    if [ "$OMS_MODEL_RESOLVED_CLASS" = role-default ]; then
      role_chain="$(oms_model_role_recovery_chain "$provider" "$OMS_MODEL_PRIMARY" || true)"
      if [ -n "$role_chain" ]; then
        OMS_MODEL_DISTINCT_CHAIN="$role_chain"
        OMS_MODEL_ALTERNATE="$(printf '%s\n' "$role_chain" | sed -n '1p')"
      fi
    fi
    # Provider-default effort validation uses the catalog-wide union because
    # no model has been selected yet. Once recovery names an exact model, keep
    # only candidates whose own scale accepts that effort.
    if [ -n "$OMS_REASONING_RESOLVED" ]; then
      while IFS= read -r candidate; do
        [ -n "$candidate" ] || continue
        if oms_reasoning_provider_validate "$provider" "$OMS_REASONING_RESOLVED" \
          "$candidate" >/dev/null 2>&1; then
          filtered_chain="${filtered_chain}${filtered_chain:+$'\n'}$candidate"
        fi
      done <<EOF
$OMS_MODEL_DISTINCT_CHAIN
EOF
      OMS_MODEL_DISTINCT_CHAIN="$filtered_chain"
      OMS_MODEL_ALTERNATE="$(printf '%s\n' "$filtered_chain" | sed -n '1p')"
    fi
  fi
  OMS_MODEL_SELECTED="$OMS_MODEL_PRIMARY"
  OMS_MODEL_FALLBACK_USED=0
  OMS_MODEL_FALLBACK_REASON=""
  export OMS_MODEL_RESOLVED_CLASS OMS_MODEL_CLASS_REASON OMS_MODEL_PRIMARY OMS_MODEL_FALLBACK
  export OMS_MODEL_ALTERNATE OMS_MODEL_DISTINCT_CHAIN OMS_MODEL_SELECTED OMS_MODEL_FALLBACK_USED OMS_MODEL_FALLBACK_REASON
  export OMS_MODEL_PREVIOUS_GENERATION
  export OMS_REASONING_EXPLICIT OMS_REASONING_RESOLVED OMS_REASONING_FALLBACK OMS_REASONING_SELECTED
}

oms_model_is_unknown_model_output() {
  local file="$1"
  grep -Eiq "issue with the selected model|model.*(does not exist|may not exist)|unknown model|invalid model|model_not_found|not (available|enabled) for (your|this) (account|organization|workspace)|do not have access to (this |that )?model" "$file"
}

# One model's safeguard fired on a message another model of the same family
# will answer. This is not the model weighing the request and declining it: the
# error says so itself — "our intentionally broad safeguards ... can sometimes
# flag legitimate coding, cybersecurity, and biology tasks" — and names the
# remedy, "change your model". Observed on a protein-ligand binding-affinity
# question about this user's own repository. Retrying once on a different model
# is following that instruction, not working around a decision; a considered
# refusal looks different and is handled by the check below.
oms_model_is_model_safeguard_output() {
  local file="$1"
  grep -Eiq "safeguards flagged this message|can.t respond to this message with " "$file"
}

# The provider declined the request itself, rather than one model's filter
# firing. Reported and never retried elsewhere: machinery that re-sends a
# refused request until some model complies would be a way around the decision
# regardless of what the request happens to be.
oms_model_is_policy_decline_output() {
  local file="$1"
  local prompt="${2:-}"
  local pattern='violate[sd]? (our|the) usage polic|unable to respond to this request|blocked by content filtering|"?stop_reason"?: *"?refusal|stop-reason: .*reason=refusal'
  if [ ! -f "$prompt" ]; then
    grep -Eiq "$pattern" "$file"
    return $?
  fi

  # Some provider CLIs echo the full prompt before the answer. A quoted policy
  # sentence is input data, not a refusal. Subtract prompt lines as a multiset,
  # so an identical line emitted once more by the provider remains a refusal.
  LC_ALL=C awk '
    FNR == NR { prompt[$0]++; next }
    {
      lower = tolower($0)
      if (lower ~ /violate[sd]? (our|the) usage polic|unable to respond to this request|blocked by content filtering|"?stop_reason"?: *"?refusal|stop-reason: .*reason=refusal/) {
        if (prompt[$0] > 0) prompt[$0]--
        else declined = 1
      }
    }
    END { exit(declined ? 0 : 1) }
  ' "$prompt" "$file"
}

oms_model_is_capacity_output() {
  local file="$1"
  grep -Eiq 'selected model is at capacity|model is at capacity|temporarily overloaded|overloaded_error' "$file"
}
