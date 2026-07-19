# shellcheck shell=bash
# Provider metadata shared by model diagnostics and runtime compatibility checks.

oms_provider_normalize() {
  case "$1" in
    agy) printf 'antigravity\n' ;;
    codex|claude|antigravity) printf '%s\n' "$1" ;;
    *)
      echo "error: unsupported provider: $1" >&2
      return 2
      ;;
  esac
}

oms_provider_binary() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf 'codex\n' ;;
    claude) printf 'claude\n' ;;
    antigravity) printf 'agy\n' ;;
  esac
}

oms_provider_required_flags() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex)
      printf '%s\n' --model --sandbox
      ;;
    claude)
      printf '%s\n' --model --permission-mode --effort
      ;;
    antigravity)
      printf '%s\n' --model --print --sandbox --print-timeout
      ;;
  esac
}

oms_provider_supports_model_listing() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  [ "$provider" = antigravity ]
}

# Family is the underlying model vendor, not the CLI carrying the request.
# This matters because Antigravity can expose Gemini, Claude, and GPT-family
# models through the same provider binary. Unknown names stay unknown instead
# of being guessed into a false independence claim.
oms_provider_model_family() {
  local provider
  local model
  local lower

  provider="$(oms_provider_normalize "$1")" || return $?
  model="$2"
  lower="$(LC_ALL=C printf '%s' "$model" | tr '[:upper:]' '[:lower:]')"

  case "$provider" in
    codex)
      printf 'openai\n'
      ;;
    claude)
      printf 'anthropic\n'
      ;;
    antigravity)
      case "$lower" in
        *gemini*) printf 'google\n' ;;
        *claude*|*anthropic*) printf 'anthropic\n' ;;
        *gpt-oss*|*gpt*|*openai*) printf 'openai\n' ;;
        *) printf 'unknown\n' ;;
      esac
      ;;
  esac
}

oms_provider_normalize_list() {
  local raw="$1"
  local provider
  local normalized
  local seen=","
  local output=""
  local -a values

  IFS=',' read -r -a values <<< "$raw"
  for provider in "${values[@]}"; do
    provider="$(printf '%s' "$provider" | tr -d '[:space:]')"
    [ -n "$provider" ] || continue
    normalized="$(oms_provider_normalize "$provider")" || return $?
    case "$seen" in
      *",$normalized,"*)
        echo "error: duplicate provider: $normalized" >&2
        return 2
        ;;
    esac
    seen="$seen$normalized,"
    if [ -n "$output" ]; then
      output="$output,$normalized"
    else
      output="$normalized"
    fi
  done

  [ -n "$output" ] || {
    echo "error: no providers selected" >&2
    return 2
  }
  printf '%s\n' "$output"
}
