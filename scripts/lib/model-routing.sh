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
    antigravity:fast) printf '%s\n' 'Gemini 3.5 Flash (Low)' ;;
    antigravity:balanced) printf '%s\n' 'Gemini 3.5 Flash (Medium)' ;;
    antigravity:deep) printf '%s\n' 'Gemini 3.1 Pro (High)' ;;
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

  if [ "$requested" != auto ]; then
    resolved="$requested"
  else
    resolved="$(oms_model_operation_class "$operation")"
    [ -n "$resolved" ] || resolved="$(oms_model_role_class "$role")"
    [ -n "$resolved" ] || resolved=balanced
  fi

  OMS_MODEL_RESOLVED_CLASS="$resolved"
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
