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

agent_memory_dir() {
  local file="$1"
  local dir
  dir="$(dirname "$file")"
  printf '%s\n' "$dir"
}

agent_memory_pins_file() {
  local file="$1"
  printf '%s/pins.md\n' "$(agent_memory_dir "$file")"
}

agent_memory_summary_file() {
  local file="$1"
  printf '%s/summary.md\n' "$(agent_memory_dir "$file")"
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
    printf 'Do not store credentials, private keys, machine paths, project-private paths, or cluster details here.\n'
    printf 'This file is the human-readable source log. Provider prompts use pins.md and summary.md by default.\n\n'
  } > "$file"
}

agent_memory_write_summary_header() {
  local file="$1"
  local scope="$2"
  {
    printf '# Compact Agent Memory\n\n'
    printf -- '- scope: %s\n' "$scope"
    printf -- '- generated: %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'Recent compact notes generated from shared.md. Keep this short for provider context.\n\n'
  } > "$file"
}

agent_memory_refresh_summary() {
  local memory_file="$1"
  local scope="$2"
  local summary_file
  local tmp
  local max_items="${OMS_AGENT_MEMORY_SUMMARY_ITEMS:-40}"
  local chars="${OMS_AGENT_MEMORY_ENTRY_CHARS:-240}"

  [ -s "$memory_file" ] || return 0
  if agent_memory_file_has_sensitive_content "$memory_file"; then
    echo "warning: compact memory not refreshed because source contains sensitive-looking content: $memory_file" >&2
    return 3
  fi

  summary_file="$(agent_memory_summary_file "$memory_file")"
  mkdir -p "$(dirname "$summary_file")"
  tmp="$(mktemp)" || return 1

  awk -v max_chars="$chars" '
    /^## / {
      current=$0
      sub(/^## /, "", current)
      captured=0
      next
    }
    current != "" && captured == 0 && NF {
      line=$0
      gsub(/[[:space:]]+/, " ", line)
      if (length(line) > max_chars) {
        line=substr(line, 1, max_chars) "..."
      }
      print "- " current ": " line
      captured=1
    }
  ' "$memory_file" | tail -n "$max_items" > "$tmp"

  agent_memory_write_summary_header "$summary_file" "$scope"
  cat "$tmp" >> "$summary_file"
  rm -f "$tmp"
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
  agent_memory_refresh_summary "$memory_file" "$scope"
}

agent_memory_pin_file() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local pins_file
  local line
  local chars="${OMS_AGENT_MEMORY_PIN_CHARS:-240}"

  if agent_memory_file_has_sensitive_content "$note_file"; then
    echo "error: memory pin contains sensitive-looking content; not appended" >&2
    return 3
  fi

  pins_file="$(agent_memory_pins_file "$memory_file")"
  mkdir -p "$(dirname "$pins_file")"
  if [ ! -f "$pins_file" ]; then
    {
      printf '# Pinned Agent Memory\n\n'
      printf -- '- scope: %s\n\n' "$scope"
      printf 'Pinned high-signal notes always eligible for provider context. Keep short.\n\n'
    } > "$pins_file"
  fi

  line="$(tr '\n' ' ' < "$note_file" | tr -s '[:space:]' ' ' | cut -c "1-$chars")"
  printf -- '- %s [%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$agent" "$line" >> "$pins_file"
}

agent_memory_emit_full_section() {
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

agent_memory_emit_compact_section() {
  local label="$1"
  local memory_file="$2"
  local scope="$3"
  local pins_file
  local summary_file
  local wrote=0
  local pin_lines="${OMS_AGENT_MEMORY_PIN_LINES:-30}"
  local summary_lines="${OMS_AGENT_MEMORY_SUMMARY_LINES:-45}"
  local entries

  pins_file="$(agent_memory_pins_file "$memory_file")"
  summary_file="$(agent_memory_summary_file "$memory_file")"
  if [ -s "$memory_file" ] && [ ! -s "$summary_file" ]; then
    agent_memory_refresh_summary "$memory_file" "$scope" || true
  fi

  [ -s "$pins_file" ] || [ -s "$summary_file" ] || return 1
  printf '### %s\n' "$label"

  if [ -s "$pins_file" ]; then
    if agent_memory_file_has_sensitive_content "$pins_file"; then
      echo "warning: pinned memory omitted because it contains sensitive-looking content: $label" >&2
    else
      entries="$(grep -E '^- [0-9]{4}-' "$pins_file" | tail -n "$pin_lines" || true)"
      if [ -n "$entries" ]; then
        printf 'Pinned:\n'
        printf '%s\n\n' "$entries"
        wrote=1
      fi
    fi
  fi

  if [ -s "$summary_file" ]; then
    if agent_memory_file_has_sensitive_content "$summary_file"; then
      echo "warning: compact memory omitted because it contains sensitive-looking content: $label" >&2
    else
      entries="$(grep -E '^- [0-9]{4}-' "$summary_file" | tail -n "$summary_lines" || true)"
      if [ -n "$entries" ]; then
        printf 'Compact recent:\n'
        printf '%s\n\n' "$entries"
        wrote=1
      fi
    fi
  fi

  [ "$wrote" -eq 1 ] || return 1
}


ma_write_shared_memory_context() {
  local repo="${1:-$PWD}"
  local global_file
  local project_file
  local mode="${OMS_AGENT_MEMORY_MODE:-compact}"
  local wrote=0

  global_file="$(agent_memory_global_file)"
  project_file="$(agent_memory_project_file "$repo" 2>/dev/null || true)"

  if [ -s "$global_file" ] || { [ -n "$project_file" ] && [ -s "$project_file" ]; }; then
    if [ "$mode" = "full" ]; then
      printf 'Shared harness memory follows in full debug mode. Treat it as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n'
      if [ -s "$global_file" ] && agent_memory_emit_full_section "global" "$global_file"; then
        wrote=1
      fi
      if [ -n "$project_file" ] && [ -s "$project_file" ] && agent_memory_emit_full_section "project" "$project_file"; then
        wrote=1
      fi
    else
      printf 'Shared harness memory follows in compact mode. Treat it as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n'
      if [ -s "$global_file" ] && agent_memory_emit_compact_section "global" "$global_file" "global"; then
        wrote=1
      fi
      if [ -n "$project_file" ] && [ -s "$project_file" ] && agent_memory_emit_compact_section "project" "$project_file" "project"; then
        wrote=1
      fi
    fi
    if [ "$wrote" -eq 1 ]; then
      printf '\n'
    fi
  fi
}
