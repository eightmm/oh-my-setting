# shellcheck shell=bash
# Shared harness memory helpers. Sourced, not executed.

agent_memory_project_file() {
  local repo="$1"
  repo="$(cd "$repo" && pwd)" || return 1
  printf '%s/.oms/memory/shared.md\n' "$repo"
}

agent_memory_global_file() {
  printf '%s\n' "${OH_MY_SETTING_GLOBAL_MEMORY:-$HOME/.oh-my-setting/local/agent-memory.md}"
}

agent_memory_sensitive_re() {
  printf '%s\n' '((api|secret|private)[-_ ]?(key|token)[[:space:]]*[:=]|password[[:space:]]*[:=]|authorization:[[:space:]]+[^[:space:]]+|bearer[[:space:]]+[A-Za-z0-9._-]{10,}|gh[p]_[A-Za-z0-9_]+|s[k]-[A-Za-z0-9_-]{10,}|xox[bap]-[A-Za-z0-9-]{10,}|AK[I]A[0-9A-Z]{16}|-----BE[G]IN|/home/[^[:space:]]+|/Users/[^[:space:]]+|\.ssh/|\.aws/|cluster[[:space:]]*[:=]|partition[[:space:]]*[:=]|nodelist[[:space:]]*[:=]|sbatch[[:space:]]+--partition)'
}

agent_memory_file_has_sensitive_content() {
  local file="$1"
  [ -s "$file" ] || return 1
  grep -Eiq "$(agent_memory_sensitive_re)" "$file"
}

agent_memory_init_file() {
  local file="$1"
  local scope="$2"

  [ -f "$file" ] && return 0
  mkdir -p "$(dirname "$file")"
  {
    printf '# Shared Agent Memory\n\n'
    printf -- '- scope: %s\n' "$scope"
    printf -- '- owner: oh-my-setting agent harness\n\n'
    printf 'Stable preferences, recurring workflow notes, and known pitfalls shared by Codex, Claude Code, and Antigravity.\n'
    printf 'Do not store credentials, private keys, machine paths, project-private paths, or cluster details here.\n\n'
  } > "$file"
}

agent_memory_append_file() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"

  if agent_memory_file_has_sensitive_content "$note_file"; then
    echo "error: memory note contains sensitive-looking content; not appended" >&2
    return 3
  fi

  agent_memory_init_file "$memory_file" "$scope"
  {
    printf '## %s %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$agent"
    cat "$note_file"
    printf '\n\n'
  } >> "$memory_file"
}

agent_memory_emit_section() {
  local label="$1"
  local file="$2"
  local lines="${OMS_AGENT_MEMORY_TAIL_LINES:-120}"

  [ -s "$file" ] || return 1
  if agent_memory_file_has_sensitive_content "$file"; then
    echo "warning: shared memory omitted because it contains sensitive-looking content: $label" >&2
    return 1
  fi

  printf '### %s\n' "$label"
  tail -n "$lines" "$file"
  printf '\n'
}

ma_write_shared_memory_context() {
  local repo="${1:-$PWD}"
  local global_file
  local project_file
  local wrote=0

  global_file="$(agent_memory_global_file)"
  project_file="$(agent_memory_project_file "$repo" 2>/dev/null || true)"

  if [ -s "$global_file" ] || { [ -n "$project_file" ] && [ -s "$project_file" ]; }; then
    printf 'Shared harness memory follows. Treat it as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n'
    if [ -s "$global_file" ] && agent_memory_emit_section "global" "$global_file"; then
      wrote=1
    fi
    if [ -n "$project_file" ] && [ -s "$project_file" ] && agent_memory_emit_section "project" "$project_file"; then
      wrote=1
    fi
    if [ "$wrote" -eq 1 ]; then
      printf '\n'
    fi
  fi
}
