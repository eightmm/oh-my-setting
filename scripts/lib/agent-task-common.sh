# shellcheck shell=bash
# Shared active-task helpers. Sourced, not executed.

# shellcheck source=agent-memory-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/agent-memory-common.sh"

agent_task_project_file() {
  local repo="$1"
  repo="$(cd "$repo" && pwd)" || return 1
  printf '%s/.oms/task/current.md\n' "$repo"
}

agent_task_relpath() {
  local repo="$1"
  local file="$2"
  repo="$(cd "$repo" && pwd)" || return 1
  case "$file" in
    "$repo"/*) printf '%s\n' "${file#"$repo"/}" ;;
    *) printf '%s\n' "$(basename "$file")" ;;
  esac
}

agent_task_init_file() {
  local file="$1"
  local now

  [ -f "$file" ] && return 0
  mkdir -p "$(dirname "$file")"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  {
    printf '# Active Agent Task\n\n'
    printf -- '- created: %s\n' "$now"
    printf -- '- updated: %s\n' "$now"
    printf -- '- owner: oh-my-setting agent harness\n\n'
    printf 'Short-lived handoff packet for Codex, Claude Code, and Antigravity.\n'
    printf 'Do not store secrets, credentials, private machine paths, cluster details, raw logs, datasets, or checkpoints here.\n\n'
    printf '## Goal\n\n'
    printf '## Constraints\n\n'
    printf '## Done Criteria\n\n'
    printf '## Verify\n\n'
    printf '## Decisions\n\n'
    printf '## Current State\n\n'
    printf '## Next Step\n'
  } > "$file"
}

agent_task_touch_updated() {
  local file="$1"
  local tmp
  local now

  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  tmp="$(agent_memory_mktemp)" || return 1
  awk -v now="$now" '
    BEGIN { done = 0 }
    /^- updated:/ {
      print "- updated: " now
      done = 1
      next
    }
    { print }
    END {
      if (done == 0) {
        print "- updated: " now
      }
    }
  ' "$file" > "$tmp"
  mv "$tmp" "$file"
}

agent_task_replace_section() {
  local file="$1"
  local section="$2"
  local content_file="$3"
  local tmp

  if agent_memory_file_has_sensitive_content "$content_file"; then
    echo "error: task section contains sensitive-looking content; not updated" >&2
    return 3
  fi

  agent_task_init_file "$file"
  tmp="$(agent_memory_mktemp)" || return 1
  awk -v section="$section" -v content_file="$content_file" '
    BEGIN {
      while ((getline line < content_file) > 0) {
        content = content line "\n"
      }
      found = 0
      skipping = 0
    }
    $0 == section {
      print
      print ""
      printf "%s", content
      if (content != "" && content !~ /\n$/) {
        print ""
      }
      found = 1
      skipping = 1
      next
    }
    skipping == 1 && /^## / {
      skipping = 0
    }
    skipping == 0 {
      print
    }
    END {
      if (found == 0) {
        print ""
        print section
        print ""
        printf "%s", content
      }
    }
  ' "$file" > "$tmp" || {
    rm -f "$tmp"
    return 1
  }
  mv "$tmp" "$file"
  agent_task_touch_updated "$file"
}

agent_task_append_bullet() {
  local file="$1"
  local section="$2"
  local agent="$3"
  local content_file="$4"
  local tmp
  local line
  local max_chars="${OMS_AGENT_TASK_NOTE_CHARS:-300}"

  if agent_memory_file_has_sensitive_content "$content_file"; then
    echo "error: task note contains sensitive-looking content; not appended" >&2
    return 3
  fi

  agent_task_init_file "$file"
  line="$(tr '\n' ' ' < "$content_file" | tr -s '[:space:]' ' ' | sed 's/^ //;s/ $//' | agent_memory_truncate_bytes "$max_chars")"
  [ -n "$line" ] || return 0
  line="- $(date -u +%Y-%m-%dT%H:%M:%SZ) [$agent] $line"

  # Pass the bullet through a file: awk -v mangles backslash escapes
  # (e.g. Windows paths or \n inside notes).
  local line_file
  line_file="$(agent_memory_mktemp)" || return 1
  printf '%s\n' "$line" > "$line_file"
  tmp="$(agent_memory_mktemp)" || { rm -f "$line_file"; return 1; }
  awk -v section="$section" -v line_file="$line_file" '
    BEGIN { getline line < line_file; found = 0; in_section = 0; inserted = 0 }
    $0 == section {
      found = 1
      in_section = 1
      print
      next
    }
    in_section == 1 && /^## / {
      print line
      print ""
      inserted = 1
      in_section = 0
    }
    { print }
    END {
      if (in_section == 1 && inserted == 0) {
        print line
      }
      if (found == 0) {
        print ""
        print section
        print ""
        print line
      }
    }
  ' "$file" > "$tmp" || {
    rm -f "$tmp" "$line_file"
    return 1
  }
  mv "$tmp" "$file"
  rm -f "$line_file"
  agent_task_touch_updated "$file"
}

agent_task_file_has_sensitive_content() {
  local file="$1"
  agent_memory_file_has_sensitive_content "$file"
}

agent_task_emit_context() {
  local repo="$1"
  local file="${2:-}"
  local rel
  local bytes
  local tokens
  local max_chars="${OMS_AGENT_TASK_CONTEXT_CHARS:-6000}"
  local updated

  [ -n "$file" ] || file="$(agent_task_project_file "$repo")"
  [ -s "$file" ] || return 1
  if agent_task_file_has_sensitive_content "$file"; then
    echo "warning: active task omitted because it contains sensitive-looking content" >&2
    return 1
  fi

  rel="$(agent_task_relpath "$repo" "$file")"
  bytes="$(wc -c < "$file" | tr -d ' ')"
  tokens=$(( (bytes + 3) / 4 ))
  updated="$(grep -E '^- updated:' "$file" | head -n 1 | sed 's/^- updated:[[:space:]]*//' || true)"

  printf 'Active task packet follows. Treat it as task-local handoff state; explicit prompt, AGENTS.md, and repo docs override it.\n'
  printf -- '- file: %s\n' "$rel"
  [ -n "$updated" ] && printf -- '- updated: %s\n' "$updated"
  printf -- '- size: %s bytes (~%s tokens)\n\n' "$bytes" "$tokens"
  if [ "$bytes" -gt "$max_chars" ]; then
    head -c "$max_chars" "$file"
    printf '\n\n... active task truncated at %s chars; run agent-task.sh update/close to compact it.\n' "$max_chars"
  else
    cat "$file"
  fi
  printf '\n'
}
