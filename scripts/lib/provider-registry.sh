# shellcheck shell=bash
# Provider metadata shared by model diagnostics and runtime compatibility checks.
#
# A provider is an agent transport, not necessarily a model vendor. Cursor and
# OpenCode can carry several model families; Grok Build can carry custom models;
# Claude Code can be configured against Z.AI. Keep those two identities apart
# so a council never turns two transports for one model family into false
# independence.

oms_provider_core_names() {
  printf '%s\n' codex claude antigravity
}

oms_provider_supported_names() {
  printf '%s\n' codex claude antigravity cursor grok gemini qwen opencode
}

oms_provider_is_builtin() {
  case "$1" in
    codex|claude|antigravity|cursor|grok|gemini|qwen|opencode) return 0 ;;
  esac
  return 1
}

oms_provider_custom_id_valid() {
  local value="$1"
  [ -n "$value" ] && [ "${#value}" -le 64 ] || return 1
  case "$value" in
    [a-z0-9]* ) ;;
    *) return 1 ;;
  esac
  case "$value" in *[!a-z0-9._-]*|*..*) return 1 ;; esac
}

oms_provider_adapter_dir() {
  printf '%s\n' "${OMS_PROVIDER_ADAPTER_DIR:-${XDG_CONFIG_HOME:-${HOME:-/nonexistent}/.config}/oh-my-setting/provider-adapters}"
}

# A custom provider is an explicitly named adapter executable, never an
# arbitrary command guessed from PATH. The marker makes discovery safe and
# keeps the shell protocol independently versionable from any vendor CLI.
oms_provider_custom_binary() {
  local provider="$1"
  local direct

  oms_provider_custom_id_valid "$provider" || return 1
  oms_provider_is_builtin "$provider" && return 1
  direct="$(oms_provider_adapter_dir)/oms-agent-adapter-$provider"
  if [ -f "$direct" ] && [ -x "$direct" ]; then
    printf '%s\n' "$direct"
    return 0
  fi
  command -v "oms-agent-adapter-$provider" 2>/dev/null
}

oms_provider_is_custom() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  oms_provider_is_builtin "$provider" && return 1
  oms_provider_custom_binary "$provider" >/dev/null 2>&1
}

# Built-ins either expose a provider-native read boundary or are covered by
# an explicit transport contract below. Antigravity has no file-write-blocking
# read flag, and custom adapters are user-defined executables, so both run in a
# disposable detached worktree for read calls.
oms_provider_requires_read_isolation() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  [ "$provider" = antigravity ] || oms_provider_is_custom "$provider"
}

oms_provider_custom_names() {
  local old_ifs="$IFS"
  local dir file name
  local seen=","
  local adapter_dir

  adapter_dir="$(oms_provider_adapter_dir)"
  for file in "$adapter_dir"/oms-agent-adapter-*; do
    [ -f "$file" ] && [ -x "$file" ] || continue
    name="${file##*/oms-agent-adapter-}"
    oms_provider_custom_id_valid "$name" || continue
    oms_provider_is_builtin "$name" && continue
    case "$seen" in *",$name,"*) continue ;; esac
    printf '%s\n' "$name"
    seen="$seen$name,"
  done

  IFS=:
  for dir in ${PATH:-}; do
    [ -n "$dir" ] || dir=.
    [ -d "$dir" ] || continue
    for file in "$dir"/oms-agent-adapter-*; do
      [ -f "$file" ] && [ -x "$file" ] || continue
      name="${file##*/oms-agent-adapter-}"
      oms_provider_custom_id_valid "$name" || continue
      oms_provider_is_builtin "$name" && continue
      case "$seen" in *",$name,"*) continue ;; esac
      printf '%s\n' "$name"
      seen="$seen$name,"
    done
  done
  IFS="$old_ifs"
}

oms_provider_normalize() {
  case "$1" in
    agy) printf 'antigravity\n' ;;
    cursor-agent) printf 'cursor\n' ;;
    grok-build) printf 'grok\n' ;;
    gemini-cli) printf 'gemini\n' ;;
    qwen-code) printf 'qwen\n' ;;
    opencode2) printf 'opencode\n' ;;
    codex|claude|antigravity|cursor|grok|gemini|qwen|opencode)
      printf '%s\n' "$1"
      ;;
    *)
      if oms_provider_custom_binary "$1" >/dev/null 2>&1; then
        printf '%s\n' "$1"
      else
        echo "error: unsupported provider: $1" >&2
        return 2
      fi
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
    cursor) printf 'cursor-agent\n' ;;
    grok) printf 'grok\n' ;;
    gemini) printf 'gemini\n' ;;
    qwen) printf 'qwen\n' ;;
    opencode)
      if command -v opencode >/dev/null 2>&1; then
        printf 'opencode\n'
      elif command -v opencode2 >/dev/null 2>&1; then
        printf 'opencode2\n'
      else
        printf 'opencode\n'
      fi
      ;;
    *) oms_provider_custom_binary "$provider" ;;
  esac
}

oms_provider_cli_available() {
  local binary
  binary="$(oms_provider_binary "$1" 2>/dev/null)" || return 1
  command -v "$binary" >/dev/null 2>&1
}

oms_provider_installed_names() {
  local provider
  while IFS= read -r provider; do
    [ -n "$provider" ] || continue
    oms_provider_cli_available "$provider" && printf '%s\n' "$provider"
  done <<EOF
$(oms_provider_supported_names)
EOF
  oms_provider_custom_names
}

# Preserve the historical core diagnostics even when one of those CLIs is
# missing, then add optional transports only when they are actually installed.
# This avoids turning every absent optional agent into a permanent doctor
# warning while still making newly installed agents visible without config.
oms_provider_default_names() {
  local provider
  oms_provider_core_names
  while IFS= read -r provider; do
    case "$provider" in codex|claude|antigravity) continue ;; esac
    printf '%s\n' "$provider"
  done <<EOF
$(oms_provider_installed_names)
EOF
}

oms_provider_supports_access() {
  local provider access candidate
  local -a oms_write_adapters
  provider="$(oms_provider_normalize "$1")" || return $?
  access="$2"
  case "$access" in
    read) return 0 ;;
    write) ;;
    *) return 2 ;;
  esac
  if oms_provider_is_builtin "$provider"; then
    return 0
  fi
  IFS=',' read -r -a oms_write_adapters <<< "${OMS_PROVIDER_WRITE_ADAPTERS:-}"
  # Bash 3.2 + set -u abort on expanding an empty array; the guard keeps the
  # DENY branch a typed refusal instead of a shell death on macOS.
  for candidate in ${oms_write_adapters[@]+"${oms_write_adapters[@]}"}; do
    candidate="$(printf '%s' "$candidate" | tr -d '[:space:]')"
    [ "$candidate" = "$provider" ] && return 0
  done
  return 1
}

# Provider invocation transport. The installed CLI remains the default for
# every provider. Codex's app-server is an explicit, read-only alternate wire;
# selecting it never silently changes Claude or Antigravity routing.
oms_provider_transport() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  if [ "$provider" != codex ]; then
    printf 'cli-exec\n'
    return 0
  fi
  case "${OMS_CODEX_TRANSPORT:-cli-exec}" in
    cli-exec|'') printf 'cli-exec\n' ;;
    app-server) printf 'app-server\n' ;;
    *)
      echo "error: unsupported Codex transport: ${OMS_CODEX_TRANSPORT}" >&2
      return 2
      ;;
  esac
}

# Print installed peer candidates in one stable order. A preferred provider
# leads when it is an installed provider other than the caller; the caller is
# emitted only as the last-resort single-family fallback. The default scope is
# builtin: a PATH-discovered custom adapter never joins an automatic pool
# (auto-pick, failover) — selecting a different transport changes cost,
# policy, and data exposure, so a custom seat runs only when explicitly named
# or under an explicit all-scope fan-out request.
oms_provider_peer_candidates() {  # CALLER [PREFERRED] [builtin|all]
  local caller="${1:-}"
  local preferred="${2:-}"
  local scope="${3:-builtin}"
  local candidate
  local normalized
  local seen=","
  local found=0
  local custom_names=""

  case "$scope" in
    builtin) ;;
    all) custom_names="$(oms_provider_custom_names)" ;;
    *)
      echo "error: peer candidate scope must be builtin or all: $scope" >&2
      return 2
      ;;
  esac

  if [ -n "$caller" ]; then
    normalized="$(oms_provider_normalize "$caller" 2>/dev/null)" || normalized=""
    caller="$normalized"
  fi
  if [ -n "$preferred" ]; then
    preferred="$(oms_provider_normalize "$preferred")" || return $?
    if [ "$preferred" != "$caller" ] && oms_provider_cli_available "$preferred"; then
      printf '%s\n' "$preferred"
      seen="$seen$preferred,"
      found=1
    fi
  fi
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    [ "$candidate" != "$caller" ] || continue
    case "$seen" in *",$candidate,"*) continue ;; esac
    if oms_provider_cli_available "$candidate"; then
      printf '%s\n' "$candidate"
      seen="$seen$candidate,"
      found=1
    fi
  done <<EOF
claude
codex
antigravity
cursor
grok
gemini
qwen
opencode
$custom_names
EOF
  if [ "$found" -eq 0 ] && [ -n "$caller" ] && \
      oms_provider_cli_available "$caller"; then
    printf '%s\n' "$caller"
  fi
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
    claude|antigravity|grok) printf 'flag\n' ;;
    *) printf 'none\n' ;;
  esac
}

oms_provider_effort_flag() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf -- '-c model_reasoning_effort\n' ;;
    claude|antigravity|grok) printf -- '--effort\n' ;;
    *) printf '\n' ;;
  esac
}

oms_provider_effort_values() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex|claude|antigravity|grok) printf 'low medium high\n' ;;
    *) printf '\n' ;;
  esac
}

# Where the flag list lives: codex documents the run flags under its subcommand.
oms_provider_help_args() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    codex) printf 'exec --help\n' ;;
    grok) printf -- '--no-auto-update --help\n' ;;
    *) printf -- '--help\n' ;;
  esac
}

oms_provider_version_args() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    grok) printf -- '--no-auto-update version\n' ;;
    *) printf -- '--version\n' ;;
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
    antigravity|opencode) printf 'lines\n' ;;
    codex) printf 'codex-app-server\n' ;;
    *) printf 'none\n' ;;
  esac
}

oms_provider_model_list_args() {
  local provider
  provider="$(oms_provider_normalize "$1")" || return $?
  case "$provider" in
    antigravity|opencode) printf 'models\n' ;;
    *) return 1 ;;
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
  local inferred

  provider="$(oms_provider_normalize "$1")" || return $?
  model="$2"
  lower="$(LC_ALL=C printf '%s' "$model" | tr '[:upper:]' '[:lower:]')"

  case "$lower" in
    *glm*|zai/*|zhipu*) inferred=zhipu ;;
    *grok*|xai/*) inferred=xai ;;
    *gemini*|google/*|vertex*) inferred=google ;;
    *claude*|anthropic/*) inferred=anthropic ;;
    *gpt*|openai/*) inferred=openai ;;
    *qwen*|alibaba/*) inferred=alibaba ;;
    *deepseek*) inferred=deepseek ;;
    *mistral*) inferred=mistral ;;
    *) inferred=unknown ;;
  esac

  case "$provider" in
    codex)
      printf 'openai\n'
      ;;
    claude)
      if [ "$inferred" = unknown ]; then printf 'anthropic\n'; else printf '%s\n' "$inferred"; fi
      ;;
    grok) printf 'xai\n' ;;
    gemini) printf 'google\n' ;;
    qwen) printf 'alibaba\n' ;;
    antigravity|cursor|opencode|*)
      printf '%s\n' "$inferred"
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

# Expand a diagnostic/discovery selector to canonical names. Ordinary council
# defaults remain explicit to avoid surprise spend; this selector is for local
# no-model probes and for callers that explicitly ask for installed providers.
oms_provider_selection_names() {
  local selection="${1:-default}"
  local normalized
  case "$selection" in
    default|'') oms_provider_default_names ;;
    auto|installed) oms_provider_installed_names ;;
    all)
      oms_provider_supported_names
      oms_provider_custom_names
      ;;
    *)
      normalized="$(oms_provider_normalize_list "$selection")" || return $?
      printf '%s\n' "$normalized" | tr ',' '\n'
      ;;
  esac
}
