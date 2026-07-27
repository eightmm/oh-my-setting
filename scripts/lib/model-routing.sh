# shellcheck shell=bash
# Provider-neutral model policy for local CLI workers. Callers set the request
# through OMS_MODEL_* variables, then invoke oms_model_prepare for one provider.
#
# What a provider can do is read from the capability snapshot, never asserted
# here: the tiers below are preferences, and whether a CLI takes a thinking
# control at all is whatever the last probe saw. Routing only reads that
# snapshot — it never probes — because deciding a route must not put a provider
# call in front of the call the caller actually asked for.

OMS_MODEL_ROUTING_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/provider-registry.sh
. "$OMS_MODEL_ROUTING_LIB_DIR/provider-registry.sh"
# shellcheck source=scripts/lib/model-capability.sh
. "$OMS_MODEL_ROUTING_LIB_DIR/model-capability.sh"

oms_model_validate_class() {
  case "$1" in auto|fast|balanced|deep) return 0 ;; esac
  echo "error: model class must be auto, fast, balanced, or deep" >&2
  return 2
}

oms_model_validate_name() {
  local value="$1"
  [ "${#value}" -le 160 ] || { echo "error: model name exceeds 160 characters" >&2; return 2; }
  if LC_ALL=C printf '%s' "$value" | grep -q '[[:cntrl:]]'; then
    echo "error: model name contains control characters" >&2
    return 2
  fi
}

# Syntax only. Which of these a given provider accepts is a capability
# question, answered by oms_reasoning_provider_validate: claude takes xhigh and
# max, and capping the vocabulary here put both permanently out of reach.
oms_reasoning_validate() {
  case "$1" in auto|low|medium|high|xhigh|max) return 0 ;; esac
  echo "error: reasoning effort must be auto, low, medium, high, xhigh, or max" >&2
  return 2
}

# Does this provider take this effort, as of the last capability probe?
oms_reasoning_provider_validate() {
  local provider="$1" effort="$2"

  [ "$effort" != auto ] || return 0
  oms_capability_peek "$provider" || return 2
  case "$OMS_CAP_EFFORT_MECHANISM" in
    none|absent)
      echo "error: $provider takes no reasoning-effort control; use --model or --model-class" >&2
      return 2
      ;;
  esac
  case " $OMS_CAP_EFFORT_VALUES " in
    *" $effort "*) return 0 ;;
  esac
  echo "error: $provider does not accept reasoning effort '$effort' (it accepts: $OMS_CAP_EFFORT_VALUES)" >&2
  return 2
}

oms_reasoning_for_class() {
  case "$1" in
    fast) printf 'low\n' ;;
    balanced) printf 'medium\n' ;;
    deep) printf 'high\n' ;;
  esac
}

oms_reasoning_from_model() {
  case "$1" in
    *"(Low)") printf 'low\n' ;;
    *"(Medium)") printf 'medium\n' ;;
    *"(High)") printf 'high\n' ;;
  esac
}

# Candidates for a tier, best first. More than one only where a live catalog
# exists to choose between them: a preference that cannot be checked is not a
# preference, it is a guess with extra steps.
oms_model_preferences() {
  local provider="$1"
  local class="$2"
  case "$provider:$class" in
    antigravity:fast)
      printf '%s\n' 'Gemini 3.6 Flash (Low)' 'Gemini 3.5 Flash (Low)' 'Gemini 3.1 Pro (Low)' ;;
    antigravity:balanced)
      printf '%s\n' 'Gemini 3.6 Flash (Medium)' 'Gemini 3.5 Flash (Medium)' ;;
    antigravity:deep)
      printf '%s\n' 'Gemini 3.6 Flash (High)' 'Gemini 3.5 Flash (High)' 'Gemini 3.1 Pro (High)' ;;
    *) oms_model_default "$provider" "$class" ;;
  esac
}

# The first candidate the provider actually has. With no catalog to consult the
# head of the list stands, which is what every provider without a listing
# command gets.
oms_model_available_preference() {
  local provider="$1"
  local class="$2"
  local candidate
  local head=""
  local rc

  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    [ -n "$head" ] || head="$candidate"
    oms_capability_model_available "$provider" "$candidate"
    rc=$?
    [ "$rc" -ne 0 ] || { printf '%s\n' "$candidate"; return 0; }
    # 2 is "no catalog known": nothing to filter with, so stop second-guessing.
    [ "$rc" -ne 2 ] || { printf '%s\n' "$head"; return 0; }
  done <<EOF
$(oms_model_preferences "$provider" "$class")
EOF
  # A catalog exists and holds none of them. Naming a model the CLI has retired
  # fails the call; letting the provider pick its own default still runs.
  [ -z "$head" ] || echo "warning: no configured $provider $class model is in its live catalog; using the provider default" >&2
  printf '\n'
}

oms_model_default() {
  local provider="$1"
  local class="$2"
  case "$provider:$class" in
    codex:fast) printf '%s\n' 'gpt-5.6-luna' ;;
    codex:balanced) printf '%s\n' 'gpt-5.6-terra' ;;
    codex:deep) printf '%s\n' 'gpt-5.6-sol' ;;
    claude:fast) printf '%s\n' 'claude-haiku-4-5-20251001' ;;
    claude:balanced) printf '%s\n' 'claude-sonnet-5' ;;
    claude:deep) printf '%s\n' 'claude-fable-5' ;;
    # Antigravity has no effort flag, so the tier IS the model variant. 3.6
    # Flash wins every published coding and agentic benchmark against 3.1 Pro
    # (SWE-Bench Pro, DeepSWE, Terminal-Bench, MLE-Bench) at roughly twice the
    # speed; Pro keeps only a narrow lead on pure-reasoning sets (GPQA Diamond,
    # HLE). This harness routes agentic work, so deep follows the coding
    # evidence. Override per tier with OMS_MODEL_ANTIGRAVITY_DEEP etc.
    antigravity:fast) printf '%s\n' 'Gemini 3.6 Flash (Low)' ;;
    antigravity:balanced) printf '%s\n' 'Gemini 3.6 Flash (Medium)' ;;
    antigravity:deep) printf '%s\n' 'Gemini 3.6 Flash (High)' ;;
  esac
}

oms_model_mapping() {
  local provider="$1"
  local class="$2"
  local provider_key
  local class_key
  local key
  local value

  provider_key="$(printf '%s' "$provider" | tr '[:lower:]-' '[:upper:]_')"
  class_key="$(printf '%s' "$class" | tr '[:lower:]' '[:upper:]')"
  key="OMS_MODEL_${provider_key}_${class_key}"
  value="${!key-}"
  if [ -n "$value" ]; then
    # An operator naming a model outright is a decision, not a suggestion: it
    # is used as given, catalog or no catalog.
    printf '%s\n' "$value"
  else
    oms_model_available_preference "$provider" "$class"
  fi
}

oms_model_role_class() {
  case "$1" in
    repo-auditor) printf 'fast\n' ;;
    implementation-worker|test-designer|patch-reviewer) printf 'balanced\n' ;;
    decision-advisor) printf 'deep\n' ;;
  esac
}

# A custom role knows what tier it needs better than a name table does, so a
# role file may declare it: `oms-model-class: deep` anywhere in the file.
# Callers that already resolve the role file export the value; routing never
# reads the filesystem itself.
oms_model_class_from_role_file() {
  local file="$1"
  local value

  [ -n "$file" ] && [ -f "$file" ] || return 1
  value="$(sed -n 's/^[^A-Za-z0-9]*oms-model-class:[[:space:]]*\([a-z]*\).*/\1/p' "$file" |
    sed -n '1p')"
  case "$value" in
    fast|balanced|deep) printf '%s\n' "$value" ;;
    *) return 1 ;;
  esac
}

oms_model_operation_class() {
  case "$1" in
    call|read|run|execute|test|verify|check|triage) printf 'fast\n' ;;
    ask|review|delegate|implement|implementation|write|repair) printf 'balanced\n' ;;
    plan|planning|advise|decision|review-gate|review-synthesis|release) printf 'deep\n' ;;
  esac
}

oms_model_next_class() {
  case "$1" in
    deep) printf 'balanced\n' ;;
    balanced) printf 'fast\n' ;;
  esac
}

oms_model_prepare() {
  local provider="$1"
  local requested="${OMS_MODEL_CLASS_REQUEST:-auto}"
  local role="${OMS_MODEL_ROLE:-}"
  local operation="${OMS_MODEL_OPERATION:-call}"
  local explicit="${OMS_MODEL_EXPLICIT:-}"
  local explicit_fallback="${OMS_MODEL_FALLBACK_EXPLICIT:-}"
  local effort_requested="${OMS_REASONING_EFFORT_REQUEST:-auto}"
  local effort_fallback_explicit="${OMS_REASONING_FALLBACK_EXPLICIT:-}"
  local resolved
  local next
  local embedded_effort

  [ "$provider" != agy ] || provider=antigravity
  oms_model_validate_class "$requested" || return $?
  oms_model_validate_name "$explicit" || return $?
  oms_model_validate_name "$explicit_fallback" || return $?
  oms_reasoning_validate "$effort_requested" || return $?
  if [ -n "$effort_fallback_explicit" ]; then
    oms_reasoning_validate "$effort_fallback_explicit" || return $?
    [ "$effort_fallback_explicit" != auto ] || {
      echo "error: explicit fallback reasoning effort cannot be auto" >&2
      return 2
    }
  fi
  oms_reasoning_provider_validate "$provider" "$effort_requested" || return $?
  if [ -n "$effort_fallback_explicit" ]; then
    oms_reasoning_provider_validate "$provider" "$effort_fallback_explicit" || return $?
  fi

  # Why a tier was chosen is as operational as which one: a worker silently
  # running on the cheapest model because nobody declared the phase looks
  # identical to one deliberately routed there.
  local reason
  if [ "$requested" != auto ]; then
    resolved="$requested"
    reason=request
  else
    # Phase stays the default signal: it is what the work actually is right
    # now, so a role can neither downgrade a release gate nor inflate a routine
    # check. The exception is a tier a role file states outright — that is an
    # explicit choice by whoever wrote the role, not a guess from its name, and
    # it is the one case where the persona knows better than the phase.
    resolved="${OMS_MODEL_ROLE_CLASS:-}"
    reason=role_file
    if [ -z "$resolved" ]; then
      resolved="$(oms_model_operation_class "$operation")"
      reason=operation
    fi
    if [ -z "$resolved" ]; then
      resolved="$(oms_model_role_class "$role")"
      reason=role
    fi
    if [ -z "$resolved" ]; then
      resolved=balanced
      reason=default
    fi
  fi

  OMS_MODEL_RESOLVED_CLASS="$resolved"
  OMS_MODEL_CLASS_REASON="$reason"
  export OMS_MODEL_CLASS_REASON
  if [ -n "$explicit" ]; then
    OMS_MODEL_PRIMARY="$explicit"
  else
    OMS_MODEL_PRIMARY="$(oms_model_mapping "$provider" "$resolved")"
  fi
  [ -n "$OMS_MODEL_PRIMARY" ] || OMS_MODEL_PRIMARY="provider-default"
  oms_model_validate_name "$OMS_MODEL_PRIMARY" || return $?

  OMS_MODEL_FALLBACK=""
  if [ -n "$explicit_fallback" ]; then
    OMS_MODEL_FALLBACK="$explicit_fallback"
  elif [ -z "$explicit" ] && [ "${OMS_MODEL_NO_FALLBACK:-0}" != 1 ]; then
    next="$(oms_model_next_class "$resolved")"
    [ -z "$next" ] || OMS_MODEL_FALLBACK="$(oms_model_mapping "$provider" "$next")"
  fi
  oms_model_validate_name "$OMS_MODEL_FALLBACK" || return $?

  if [ "$effort_requested" = auto ]; then
    OMS_REASONING_RESOLVED="$(oms_reasoning_for_class "$resolved")"
    OMS_REASONING_EXPLICIT=0
  else
    OMS_REASONING_RESOLVED="$effort_requested"
    # A provider whose control is a flag has to be handed the flag; a caller
    # asking for an effort and getting the default instead is the failure this
    # distinction exists to prevent.
    OMS_REASONING_EXPLICIT=1
  fi
  export OMS_REASONING_EXPLICIT
  OMS_REASONING_FALLBACK=""
  if [ -n "$OMS_MODEL_FALLBACK" ]; then
    OMS_REASONING_FALLBACK="$OMS_REASONING_RESOLVED"
    if [ -n "$effort_fallback_explicit" ]; then
      OMS_REASONING_FALLBACK="$effort_fallback_explicit"
    elif [ "$effort_requested" = auto ] && [ -z "$explicit" ] && [ -z "$explicit_fallback" ]; then
      next="$(oms_model_next_class "$resolved")"
      [ -z "$next" ] || OMS_REASONING_FALLBACK="$(oms_reasoning_for_class "$next")"
    fi
  fi
  if [ "$provider" = antigravity ] && [ "$effort_requested" = auto ]; then
    embedded_effort="$(oms_reasoning_from_model "$OMS_MODEL_PRIMARY")"
    OMS_REASONING_RESOLVED="$embedded_effort"
    embedded_effort="$(oms_reasoning_from_model "$OMS_MODEL_FALLBACK")"
    OMS_REASONING_FALLBACK="$embedded_effort"
  fi
  OMS_REASONING_SELECTED="$OMS_REASONING_RESOLVED"

  OMS_MODEL_SELECTED="$OMS_MODEL_PRIMARY"
  OMS_MODEL_FALLBACK_USED=0
  OMS_MODEL_FALLBACK_REASON=""
  export OMS_MODEL_RESOLVED_CLASS OMS_MODEL_PRIMARY OMS_MODEL_FALLBACK
  export OMS_MODEL_SELECTED OMS_MODEL_FALLBACK_USED OMS_MODEL_FALLBACK_REASON
  export OMS_REASONING_RESOLVED OMS_REASONING_FALLBACK OMS_REASONING_SELECTED
}

oms_model_is_capacity_output() {
  local file="$1"
  grep -Eiq 'selected model is at capacity|model is at capacity|temporarily overloaded|overloaded_error' "$file"
}
