# shellcheck shell=bash
# Shared active-task helpers. Sourced, not executed.

# shellcheck source=agent-memory-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/agent-memory-common.sh"

agent_task_project_file() {
  local repo="$1"
  repo="$(oms_repo_root "$repo")" || return 1
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

agent_task_new_id() {
  printf 'task-%s-%s-%s\n' "$(date -u +%Y%m%dT%H%M%SZ)" "$$" "${RANDOM:-0}"
}

agent_task_metadata_value() {
  local file="$1"
  local key="$2"

  [ -s "$file" ] || return 1
  awk -v key="$key" '
    /^## / { exit found ? 0 : 1 }
    {
      pattern = "^- " key ":[[:space:]]*"
      if ($0 ~ pattern) {
        sub(pattern, "")
        print
        found = 1
        exit 0
      }
    }
    END { if (!found) exit 1 }
  ' "$file"
}

agent_task_is_stale() {
  local file="$1"
  local ttl="${2:-${OMS_AGENT_TASK_TTL:-604800}}"
  local value

  case "$ttl" in *[!0-9]*|"") ttl=604800 ;; esac
  value="$(agent_task_metadata_value "$file" last_activity 2>/dev/null || agent_task_metadata_value "$file" updated 2>/dev/null || true)"
  [ -n "$value" ] || return 1
  command -v python3 >/dev/null 2>&1 || return 1
  python3 - "$value" "$ttl" <<'PY'
import datetime, sys
try:
    then = datetime.datetime.strptime(sys.argv[1], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)
    ttl = int(sys.argv[2])
except Exception:
    raise SystemExit(1)
age = (datetime.datetime.now(datetime.timezone.utc) - then).total_seconds()
raise SystemExit(0 if age >= ttl else 1)
PY
}

agent_task_set_metadata_unlocked() {
  local file="$1"
  local key="$2"
  local value="$3"
  local tmp

  tmp="$(agent_memory_mktemp)" || return 1
  awk -v key="$key" -v value="$value" '
    BEGIN { found = 0; inserted = 0; before_sections = 1 }
    before_sections == 1 {
      pattern = "^- " key ":[[:space:]]*"
      if ($0 ~ pattern) {
        print "- " key ": " value
        found = 1
        next
      }
      if (($0 ~ /^## / || $0 ~ /^Short-lived handoff packet/) && found == 0) {
        print "- " key ": " value
        print ""
        inserted = 1
      }
      if ($0 ~ /^## /) before_sections = 0
    }
    { print }
    END {
      if (found == 0 && inserted == 0) print "- " key ": " value
    }
  ' "$file" > "$tmp" || {
    rm -f "$tmp"
    return 1
  }
  mv "$tmp" "$file"
}

agent_task_set_metadata() {
  local file="$1"
  local key="$2"
  local value="$3"

  oms_with_file_lock "$file" agent_task_set_metadata_unlocked "$file" "$key" "$value"
}

agent_task_ensure_metadata_unlocked() {
  local file="$1"
  local now
  local value

  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  value="$(agent_task_metadata_value "$file" created 2>/dev/null || true)"
  [ -n "$value" ] || agent_task_set_metadata_unlocked "$file" created "$now"
  value="$(agent_task_metadata_value "$file" updated 2>/dev/null || true)"
  [ -n "$value" ] || agent_task_set_metadata_unlocked "$file" updated "$now"
  value="$(agent_task_metadata_value "$file" task_id 2>/dev/null || true)"
  [ -n "$value" ] || agent_task_set_metadata_unlocked "$file" task_id "$(agent_task_new_id)"
  value="$(agent_task_metadata_value "$file" status 2>/dev/null || true)"
  [ -n "$value" ] || agent_task_set_metadata_unlocked "$file" status active
  if ! grep -Eq '^- source_session:' "$file" 2>/dev/null; then
    agent_task_set_metadata_unlocked "$file" source_session "${OMS_AGENT_TASK_SOURCE_SESSION:-}"
  fi
  value="$(agent_task_metadata_value "$file" last_activity 2>/dev/null || true)"
  if [ -z "$value" ]; then
    value="$(agent_task_metadata_value "$file" updated 2>/dev/null || true)"
    agent_task_set_metadata_unlocked "$file" last_activity "${value:-$now}"
  fi
  if ! grep -Eq '^- closed_at:' "$file" 2>/dev/null; then
    agent_task_set_metadata_unlocked "$file" closed_at ""
  fi
}

agent_task_init_file_unlocked() {
  local file="$1"
  local now

  if [ -f "$file" ]; then
    agent_task_ensure_metadata_unlocked "$file"
    return 0
  fi
  agent_memory_ensure_oms_ignore_for_path "$file"
  mkdir -p "$(dirname "$file")"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  {
    printf '# Active Agent Task\n\n'
    printf -- '- created: %s\n' "$now"
    printf -- '- updated: %s\n' "$now"
    printf -- '- task_id: %s\n' "$(agent_task_new_id)"
    printf -- '- status: active\n'
    printf -- '- source_session: %s\n' "${OMS_AGENT_TASK_SOURCE_SESSION:-}"
    printf -- '- last_activity: %s\n' "$now"
    printf -- '- closed_at:\n'
    printf -- '- owner: oh-my-setting agent harness\n\n'
    printf 'Short-lived handoff packet for Codex, Claude Code, and Antigravity.\n'
    printf 'Do not store secrets, credentials, private machine paths, cluster details, raw logs, datasets, or checkpoints here.\n\n'
    printf '## Goal\n\n'
    printf '## Constraints\n\n'
    printf '## Done Criteria\n\n'
    printf '## Verify\n\n'
    printf '## Loop State\n\n'
    printf '## Last Failure\n\n'
    printf '## Verification\n\n'
    printf '## Decisions\n\n'
    printf '## Current State\n\n'
    printf '## Next Step\n'
  } > "$file"
}

agent_task_init_file() {
  local file="$1"

  oms_with_file_lock "$file" agent_task_init_file_unlocked "$file"
}

agent_task_touch_updated_unlocked() {
  local file="$1"
  local now

  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  agent_task_ensure_metadata_unlocked "$file"
  agent_task_set_metadata_unlocked "$file" updated "$now"
  agent_task_set_metadata_unlocked "$file" last_activity "$now"
}

agent_task_touch_updated() {
  local file="$1"

  oms_with_file_lock "$file" agent_task_touch_updated_unlocked "$file"
}

agent_task_set_status_unlocked() {
  local file="$1"
  local status="$2"
  local now

  agent_task_init_file_unlocked "$file"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  agent_task_set_metadata_unlocked "$file" status "$status"
  if [ "$status" = closed ]; then
    agent_task_set_metadata_unlocked "$file" closed_at "$now"
  fi
  agent_task_set_metadata_unlocked "$file" updated "$now"
  agent_task_set_metadata_unlocked "$file" last_activity "$now"
}

agent_task_set_status() {
  local file="$1"
  local status="$2"

  oms_with_file_lock "$file" agent_task_set_status_unlocked "$file" "$status"
}

agent_task_archive_unlocked() {
  local file="$1"
  local archive_dir
  local archive_file
  local task_id
  local stamp
  local suffix=0

  [ -e "$file" ] || return 0
  agent_task_ensure_metadata_unlocked "$file"
  agent_task_set_status_unlocked "$file" closed
  task_id="$(agent_task_metadata_value "$file" task_id 2>/dev/null || agent_task_new_id)"
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  archive_dir="$(dirname "$file")/archive"
  archive_file="$archive_dir/${task_id}-${stamp}.md"
  mkdir -p "$archive_dir"
  while [ -e "$archive_file" ]; do
    suffix=$((suffix + 1))
    archive_file="$archive_dir/${task_id}-${stamp}-${suffix}.md"
  done
  mv "$file" "$archive_file"
  printf '%s\n' "$archive_file"
}

agent_task_archive() {
  local file="$1"

  oms_with_file_lock "$file" agent_task_archive_unlocked "$file"
}

agent_task_rotate_unlocked() {
  local file="$1"
  local archive_file=""

  if [ -e "$file" ]; then
    archive_file="$(agent_task_archive_unlocked "$file")" || return 1
  fi
  agent_task_init_file_unlocked "$file"
  [ -z "$archive_file" ] || printf '%s\n' "$archive_file"
}

agent_task_rotate() {
  local file="$1"

  oms_with_file_lock "$file" agent_task_rotate_unlocked "$file"
}

agent_task_prune_current_state_unlocked() {
  local file="$1"
  local max="${OMS_AGENT_TASK_MAX_CURRENT_BULLETS:-100}"
  local tmp

  case "$max" in *[!0-9]*|"") max=100 ;; esac
  [ "$max" -gt 0 ] || return 0
  tmp="$(agent_memory_mktemp)" || return 1
  awk -v max="$max" '
    function flush(    i, count, skip) {
      count = 0
      for (i = 1; i <= n; i++) if (buf[i] ~ /^- /) count++
      skip = count - max
      for (i = 1; i <= n; i++) {
        if (buf[i] ~ /^- / && skip > 0) { skip--; continue }
        print buf[i]
      }
      delete buf
      n = 0
    }
    $0 == "## Current State" { print; current = 1; n = 0; next }
    current == 1 && /^## / { flush(); current = 0; print; next }
    current == 1 { buf[++n] = $0; next }
    { print }
    END { if (current == 1) flush() }
  ' "$file" > "$tmp" || {
    rm -f "$tmp"
    return 1
  }
  mv "$tmp" "$file"
}

agent_task_replace_section_unlocked() {
  local file="$1"
  local section="$2"
  local content_file="$3"
  local tmp

  agent_task_init_file_unlocked "$file"
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
  agent_task_touch_updated_unlocked "$file"
}

agent_task_replace_section() {
  local file="$1"
  local section="$2"
  local content_file="$3"

  if agent_memory_file_has_sensitive_content "$content_file"; then
    echo "error: task section contains sensitive-looking content; not updated" >&2
    return 3
  fi

  oms_with_file_lock "$file" agent_task_replace_section_unlocked "$file" "$section" "$content_file"
}

agent_task_section_value() {
  local file="$1"
  local section="$2"
  local key="$3"

  [ -s "$file" ] || return 1
  awk -v section="$section" -v key="$key" '
    $0 == section { in_section = 1; next }
    in_section == 1 && /^## / { in_section = 0 }
    in_section == 1 {
      pattern = "^- " key ":[[:space:]]*"
      if ($0 ~ pattern) {
        sub(pattern, "")
        print
        found = 1
        exit
      }
    }
    END { exit found ? 0 : 1 }
  ' "$file"
}

agent_task_upsert_loop_state_unlocked() {
  local file="$1"
  local attempts="$2"
  local max_attempts="$3"
  local diff_budget="$4"
  local verify_level="$5"
  local content_file

  agent_task_init_file_unlocked "$file"
  [ -n "$attempts" ] || attempts="$(agent_task_section_value "$file" "## Loop State" attempts 2>/dev/null || true)"
  [ -n "$max_attempts" ] || max_attempts="$(agent_task_section_value "$file" "## Loop State" max_attempts 2>/dev/null || true)"
  [ -n "$diff_budget" ] || diff_budget="$(agent_task_section_value "$file" "## Loop State" diff_budget_lines 2>/dev/null || true)"
  [ -n "$verify_level" ] || verify_level="$(agent_task_section_value "$file" "## Loop State" verification_level 2>/dev/null || true)"

  content_file="$(agent_memory_mktemp)" || return 1
  {
    [ -n "$attempts" ] && printf -- '- attempts: %s\n' "$attempts"
    [ -n "$max_attempts" ] && printf -- '- max_attempts: %s\n' "$max_attempts"
    [ -n "$diff_budget" ] && printf -- '- diff_budget_lines: %s\n' "$diff_budget"
    [ -n "$verify_level" ] && printf -- '- verification_level: %s\n' "$verify_level"
  } > "$content_file"
  agent_task_replace_section_unlocked "$file" "## Loop State" "$content_file"
  rm -f "$content_file"
}

agent_task_upsert_loop_state() {
  local file="$1"
  local attempts="$2"
  local max_attempts="$3"
  local diff_budget="$4"
  local verify_level="$5"

  oms_with_file_lock "$file" agent_task_upsert_loop_state_unlocked "$file" "$attempts" "$max_attempts" "$diff_budget" "$verify_level"
}

agent_task_loop_warnings() {
  local repo="$1"
  local file="${2:-}"
  local attempts
  local max_attempts
  local diff_budget
  local repeated_failures
  local repeat_threshold="${OMS_AGENT_TASK_STUCK_REPEATS:-3}"
  local changed_lines

  [ -n "$file" ] || file="$(agent_task_project_file "$repo")"
  [ -s "$file" ] || return 0
  agent_task_file_has_sensitive_content "$file" && return 0

  attempts="$(agent_task_section_value "$file" "## Loop State" attempts 2>/dev/null || true)"
  max_attempts="$(agent_task_section_value "$file" "## Loop State" max_attempts 2>/dev/null || true)"
  if [ -n "$attempts" ] && [ -n "$max_attempts" ] &&
     printf '%s\n' "$attempts" | grep -Eq '^[0-9]+$' &&
     printf '%s\n' "$max_attempts" | grep -Eq '^[0-9]+$' &&
     [ "$attempts" -ge "$max_attempts" ] && [ "$max_attempts" -gt 0 ]; then
    printf 'warning: loop attempts exhausted: %s/%s\n' "$attempts" "$max_attempts"
  fi

  repeated_failures="$(
    awk '
      /^## Last Failure$/ { in_section = 1; next }
      in_section == 1 && /^## / { in_section = 0 }
      in_section == 1 && /^- / {
        line = $0
        sub(/^- [^[]*\[[^]]*\] /, "", line)
        if (line != "") print line
      }
    ' "$file" | tail -n "$repeat_threshold"
  )"
  if [ "$(printf '%s\n' "$repeated_failures" | sed '/^$/d' | wc -l | tr -d ' ')" -ge "$repeat_threshold" ] &&
     [ "$(printf '%s\n' "$repeated_failures" | sed '/^$/d' | sort -u | wc -l | tr -d ' ')" -eq 1 ]; then
    printf 'warning: repeated last failure detected (%sx): %s\n' \
      "$repeat_threshold" "$(printf '%s\n' "$repeated_failures" | sed '/^$/d' | head -n 1)"
  fi

  diff_budget="$(agent_task_section_value "$file" "## Loop State" diff_budget_lines 2>/dev/null || true)"
  if [ -n "$diff_budget" ] && printf '%s\n' "$diff_budget" | grep -Eq '^[0-9]+$' &&
     [ "$diff_budget" -gt 0 ] && git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    changed_lines="$(
      { git -C "$repo" diff --numstat HEAD -- 2>/dev/null || git -C "$repo" diff --numstat -- 2>/dev/null || true; } |
        awk '{ add += $1; del += $2 } END { print add + del + 0 }'
    )"
    if [ "$changed_lines" -gt "$diff_budget" ]; then
      printf 'warning: loop diff budget exceeded: %s/%s changed lines\n' "$changed_lines" "$diff_budget"
    fi
  fi
}

agent_task_append_bullet_unlocked() {
  local file="$1"
  local section="$2"
  local agent="$3"
  local content_file="$4"
  local tmp
  local line
  local max_chars="${OMS_AGENT_TASK_NOTE_CHARS:-300}"

  agent_task_init_file_unlocked "$file"
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
  if [ "$section" = "## Current State" ]; then
    agent_task_prune_current_state_unlocked "$file"
  fi
  agent_task_touch_updated_unlocked "$file"
}

agent_task_append_bullet() {
  local file="$1"
  local section="$2"
  local agent="$3"
  local content_file="$4"

  if agent_memory_file_has_sensitive_content "$content_file"; then
    echo "error: task note contains sensitive-looking content; not appended" >&2
    return 3
  fi

  oms_with_file_lock "$file" agent_task_append_bullet_unlocked "$file" "$section" "$agent" "$content_file"
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
    agent_task_prune_for_budget "$file" "$max_chars"
    printf '\n\n... active task compacted to ~%s chars (oldest Loop State/Last Failure/Verification bullets dropped first; Goal/Done Criteria/Verify/Decisions/Current State/Next Step preserved). Run agent-task.sh update/close to compact it.\n' "$max_chars"
  else
    cat "$file"
  fi
  printf '\n'
}

# Section-aware truncation. Plain `head -c` cut from the top, which silently
# dropped the most actionable sections (Current State, Next Step, Decisions)
# because they live at the bottom of the file. Instead, keep the priority
# sections in full and spend the remaining budget on the accumulating sections
# (Loop State, Last Failure, Verification) tail-first, so recent entries
# survive and the handoff conclusion is never lost.
agent_task_prune_for_budget() {
  local file="$1"
  local budget="$2"

  awk -v budget="$budget" '
    function is_priority(h) {
      return (h == "## Goal" || h == "## Constraints" || h == "## Done Criteria" \
        || h == "## Verify" || h == "## Decisions" || h == "## Current State" \
        || h == "## Next Step")
    }
    /^## / { section = $0; order[++n] = section; idx[section] = n }
    {
      body[idx[section] "\t" (++count[section])] = $0
      lines[section] = count[section]
      if (section == "") preamble[++pre] = $0
    }
    END {
      # Pass 1: priority sections (and the preamble before the first header)
      # are always emitted in full; tally their byte cost.
      used = 0
      for (i = 1; i <= pre; i++) used += length(preamble[i]) + 1
      for (k = 1; k <= n; k++) {
        s = order[k]
        if (!is_priority(s)) continue
        for (j = 1; j <= lines[s]; j++) used += length(body[k "\t" j]) + 1
      }
      remaining = budget - used
      if (remaining < 0) remaining = 0

      # Emit in original file order. Priority sections print verbatim;
      # accumulating sections keep as many trailing lines as fit.
      for (i = 1; i <= pre; i++) print preamble[i]
      for (k = 1; k <= n; k++) {
        s = order[k]
        print s
        if (is_priority(s)) {
          for (j = 2; j <= lines[s]; j++) print body[k "\t" j]
          continue
        }
        # Find the largest tail [start..end] of body lines fitting remaining.
        size = 0; start = lines[s] + 1
        for (j = lines[s]; j >= 2; j--) {
          cost = length(body[k "\t" j]) + 1
          if (size + cost > remaining) break
          size += cost; start = j
        }
        if (start > 2) print "  (... older entries dropped to fit context budget ...)"
        for (j = start; j <= lines[s]; j++) print body[k "\t" j]
        remaining -= size
        if (remaining < 0) remaining = 0
      }
    }
  ' "$file"
}
