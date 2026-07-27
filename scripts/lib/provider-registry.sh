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

# How a provider takes its thinking control, and the scale it is declared to
# accept. These are the starting point, not the answer: model-capability.sh
# probes the installed CLI and the probe wins where it can see. `config` cannot
# be probed — codex takes effort through `-c model_reasoning_effort=`, which no
# help text advertises — so for that mechanism the declaration stands.
oms_provider_effort_mechanism() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf 'config\n' ;;
    claude|antigravity) printf 'flag\n' ;;
  esac
}

oms_provider_effort_flag() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf -- '-c model_reasoning_effort\n' ;;
    claude|antigravity) printf -- '--effort\n' ;;
  esac
}

oms_provider_effort_values() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex|claude|antigravity) printf 'low medium high\n' ;;
  esac
}

# Where the flag list lives: codex documents the run flags under its subcommand.
oms_provider_help_args() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf 'exec --help\n' ;;
    claude|antigravity) printf -- '--help\n' ;;
  esac
}

# How a provider's catalog can be read locally, without spending a model call.
# `lines`: a subcommand that prints one model per line.
# `codex-app-server`: the JSON-RPC `model/list` method its app-server exposes,
#   which also reports each model's own reasoning-effort scale.
# `none`: no local source. Claude Code has no such command — its `--help`
#   documents the aliases (fable, opus, sonnet) and nothing enumerates them.
oms_provider_model_listing_kind() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    antigravity) printf 'lines\n' ;;
    codex) printf 'codex-app-server\n' ;;
    claude) printf 'none\n' ;;
  esac
}

oms_provider_supports_model_listing() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  [ "$(oms_provider_model_listing_kind "$provider")" != none ]
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
