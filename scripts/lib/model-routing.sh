# shellcheck shell=bash
# Provider-neutral model policy for local CLI workers. Callers set the request
# through OMS_MODEL_* variables, then invoke oms_model_prepare for one provider.

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

oms_reasoning_validate() {
  case "$1" in auto|low|medium|high) return 0 ;; esac
  echo "error: reasoning effort must be auto, low, medium, or high" >&2
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
    printf '%s\n' "$value"
  else
    oms_model_default "$provider" "$class"
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
  if [ "$provider" = antigravity ] && [ "$effort_requested" != auto ]; then
    echo "error: antigravity reasoning effort is selected by the model variant; use --model or --model-class" >&2
    return 2
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
  else
    OMS_REASONING_RESOLVED="$effort_requested"
  fi
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
