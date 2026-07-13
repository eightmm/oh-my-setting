# shellcheck shell=bash
# Shared harness memory helpers. Sourced, not executed.

# shellcheck source=file-lock.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/file-lock.sh"

# Normalize a repo argument to its git worktree root so shared state does not
# silently fork when a command runs from a subdirectory (repo/src/.oms vs
# repo/.oms). Non-git directories resolve to themselves.
oms_repo_root() {
  local repo="$1"
  local root
  root="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null || true)"
  if [ -n "$root" ]; then
    printf '%s\n' "$root"
  else
    (cd "$repo" && pwd)
  fi
}

# Best-effort identity of the agent CLI running this process, for attribution
# of notes, claims, and spine rows. Order: explicit OMS_AGENT > markers the
# CLIs export to their subprocesses > generic "agent". Harness-spawned workers
# are reliable: the spawning side exports OMS_AGENT=<provider> for them.
oms_detect_agent() {
  if [ -n "${OMS_AGENT:-}" ]; then
    printf '%s\n' "$OMS_AGENT"
  elif [ -n "${CLAUDECODE:-}" ] || [ -n "${CLAUDE_CODE_ENTRYPOINT:-}" ]; then
    printf 'claude\n'
  elif [ -n "${CODEX_SANDBOX:-}" ]; then
    printf 'codex\n'
  else
    printf 'agent\n'
  fi
}

# Canonical provider namespace shared by the plan board, the router, and the
# delegate. Accepts the aliases users type; prints the canonical name or fails,
# so board/artifact records never fork into "agy" vs "antigravity".
oms_normalize_provider() {
  case "${1:-}" in
    codex|claude|antigravity) printf '%s\n' "$1" ;;
    agy) printf 'antigravity\n' ;;
    *) return 1 ;;
  esac
}

agent_memory_project_file() {
  local repo="$1"
  repo="$(oms_repo_root "$repo")" || return 1
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

agent_memory_ensure_oms_ignore() {
  local repo="$1"
  local oms_dir
  local ignore

  [ -n "$repo" ] || return 0
  repo="$(cd "$repo" && pwd)" || return 0
  oms_dir="$repo/.oms"
  ignore="$oms_dir/.gitignore"
  mkdir -p "$oms_dir"
  [ -e "$ignore" ] && return 0
  printf '*\n' > "$ignore"
}

agent_memory_ensure_oms_ignore_for_path() {
  local path="$1"
  local repo=""

  case "$path" in
    .oms|.oms/*) repo="$PWD" ;;
    */.oms/*) repo="${path%/.oms/*}" ;;
    */.oms) repo="${path%/.oms}" ;;
    *) return 0 ;;
  esac
  [ -n "$repo" ] || return 0
  agent_memory_ensure_oms_ignore "$repo"
}

# Bracket classes like [o] keep these literal patterns from matching their own
# source line, so harness diffs stay reviewable. Do not "simplify" them away.
agent_memory_sensitive_re() {
  printf '%s\n' '((^|[^A-Za-z0-9])[A-Za-z0-9_]*(t[o]ken|s[e]cret|passw(or)?[d]|credentia[l]s?|(ap[i]|s[e]cret|privat[e])[-_ ]?(ke[y]|t[o]ken)|aws_s[e]cret_access_[k]ey)["'\'']?[[:space:]]*[:=]|auth[o]rization:[[:space:]]+[^[:space:]]+|bear[e]r[[:space:]]+[A-Za-z0-9._-]{10,}|[a-z][a-z0-9+.-]*://[^[:space:]/:@]+:[^*[:space:]@/][^[:space:]@/]*@[^[:space:]/]+|(^|[^A-Za-z0-9_-])ey[J][A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}($|[^A-Za-z0-9_-])|gh[pousr]_[A-Za-z0-9_]{20,}|githu[b]_pa[t]_[A-Za-z0-9_]{20,}|npm_[A-Za-z0-9]{36,}|h[f]_[A-Za-z0-9]{34,}|glpa[t]-[A-Za-z0-9_-]{20,}|[sr]k_(liv[e]|tes[t])_[A-Za-z0-9]{16,}|AI[z]a[0-9A-Za-z_-]{35}|hook[s]\.slac[k]\.com/service[s]/|disc[o]rd(app)?\.com/api/webhook[s]/|machin[e][[:space:]]+[^[:space:]]+[[:space:]]+logi[n][[:space:]]+[^[:space:]]+[[:space:]]+passwor[d][[:space:]]+[^[:space:]]+|(^|[^A-Za-z0-9_])s[k]-[A-Za-z0-9_-]{10,}|xox[bap]-[A-Za-z0-9-]{10,}|AK[I]A[0-9A-Z]{16}|-----BE[G]IN|/hom[e]/[^[:space:]]+|/User[s]/[^[:space:]]+|/scratc[h]/[^[:space:]]+|/lustr[e]/[^[:space:]]+|/gpf[s]/[^[:space:]]+|/beegf[s]/[^[:space:]]+|\.ss[h]/|\.aw[s]/|clust[e]r[[:space:]]*[:=]|partiti[o]n[[:space:]]*[:=]|nodelis[t][[:space:]]*[:=]|sbatc[h][[:space:]]+--partition)'
}

agent_memory_file_has_sensitive_content() {
  local file="$1"
  [ -s "$file" ] || return 1
  grep -Eiq "$(agent_memory_sensitive_re)" "$file"
}

# Byte-budget truncation that never leaves a split multibyte character
# (notes are often Korean); falls back to a plain byte cut without iconv.
# iconv exits nonzero when it drops a split trailing character — that is the
# expected truncation case, so the status must be swallowed for pipefail.
agent_memory_truncate_bytes() {
  local max="$1"
  if command -v iconv >/dev/null 2>&1; then
    head -c "$max" | { iconv -f UTF-8 -t UTF-8 -c 2>/dev/null || true; }
  else
    head -c "$max"
  fi
}

# Temp files land in the caller's trapped dir when it exports OMS_LIB_TMPDIR,
# so a crash mid-edit cannot leak them; otherwise plain mktemp.
agent_memory_mktemp() {
  if [ -n "${OMS_LIB_TMPDIR:-}" ] && [ -d "${OMS_LIB_TMPDIR:-}" ]; then
    mktemp "$OMS_LIB_TMPDIR/oms.XXXXXX"
  else
    mktemp
  fi
}

oms_check_sh_has_ml_smoke() {
  local check_sh_path="$1"

  [ -f "$check_sh_path" ] || return 1
  grep -Eq '(^|[[:space:]("|'\''])ml-smoke("|'\'')?\)' "$check_sh_path"
}

# Pure read intent wins over write nouns ("review the fix" is a read), but a
# read request explicitly coordinated with an action ("review and fix") is a
# write. Anything ambiguous stays read — the conservative default.
oms_classify_prompt_mode() {
  local text="$1"
  local lower
  local read_re='(^|[^a-z])(review|assess|evaluate|analy[sz]e|explain|compare|inspect|audit|summari[sz]e|investigate|describe|why|what|how)([^a-z]|$)|검토|평가|분석|리뷰|설명|조사|비교'
  local write_re='(^|[^a-z])(add|implement|fix|change|modify|update|refactor|remove|delete|create|generate|write|apply|migrate|rename|scaffold|build|install)([^a-z]|$)|구현|수정|추가|변경|삭제|제거|고쳐|만들|작성|적용|리팩터|정리'
  local mixed_write_re='(review|assess|evaluate|analy[sz]e|inspect|audit|investigate)([^a-z]|.)*(and|then)([^a-z]|.)*(add|implement|fix|change|modify|update|refactor|remove|delete|create|write|apply|build|install)|검토.*(하고|해서|후|및).*([[:space:]]|)(구현|수정|추가|변경|삭제|제거|고쳐|작성|적용|정리)'
  lower="$(printf '%s' "$text" | tr '[:upper:]' '[:lower:]')"

  if printf '%s' "$lower" | grep -Eq "$mixed_write_re"; then
    printf 'write\n'
  elif printf '%s' "$lower" | grep -Eq "$read_re"; then
    printf 'read\n'
  elif printf '%s' "$lower" | grep -Eq "$write_re"; then
    printf 'write\n'
  else
    printf 'read\n'
  fi
}

agent_memory_init_file_unlocked() {
  local file="$1"
  local scope="$2"

  [ -f "$file" ] && return 0
  agent_memory_ensure_oms_ignore_for_path "$file"
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

agent_memory_init_file() {
  local file="$1"
  local scope="$2"

  oms_with_file_lock "$file" agent_memory_init_file_unlocked "$file" "$scope"
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

agent_memory_refresh_summary_write() {
  local summary_file="$1"
  local scope="$2"
  local body_file="$3"

  agent_memory_write_summary_header "$summary_file" "$scope"
  cat "$body_file" >> "$summary_file"
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
  agent_memory_ensure_oms_ignore_for_path "$summary_file"
  mkdir -p "$(dirname "$summary_file")"
  tmp="$(agent_memory_mktemp)" || return 1

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

  oms_with_file_lock "$summary_file" agent_memory_refresh_summary_write "$summary_file" "$scope" "$tmp"
  rm -f "$tmp"
}

agent_memory_append_file_unlocked() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"

  agent_memory_init_file_unlocked "$memory_file" "$scope"
  {
    printf '## %s %s\n\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$agent"
    cat "$note_file"
    printf '\n\n'
  } >> "$memory_file"
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

  oms_with_file_lock "$memory_file" agent_memory_append_file_unlocked "$memory_file" "$scope" "$agent" "$note_file"
  # The note is already written; a stale summary must not turn the append
  # into a failure (refresh prints its own warning).
  agent_memory_refresh_summary "$memory_file" "$scope" || true
}

agent_memory_pin_file_unlocked() {
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local pins_file="$5"
  local line
  local chars="${OMS_AGENT_MEMORY_PIN_CHARS:-240}"

  if [ ! -f "$pins_file" ]; then
    {
      printf '# Pinned Agent Memory\n\n'
      printf -- '- scope: %s\n\n' "$scope"
      printf 'Pinned high-signal notes always eligible for provider context. Keep short.\n\n'
    } > "$pins_file"
  fi

  line="$(tr '\n' ' ' < "$note_file" | tr -s '[:space:]' ' ' | agent_memory_truncate_bytes "$chars")"
  printf -- '- %s [%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$agent" "$line" >> "$pins_file"
}

agent_memory_pin_file() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local pins_file

  if agent_memory_file_has_sensitive_content "$note_file"; then
    echo "error: memory pin contains sensitive-looking content; not appended" >&2
    return 3
  fi

  pins_file="$(agent_memory_pins_file "$memory_file")"
  agent_memory_ensure_oms_ignore_for_path "$pins_file"
  mkdir -p "$(dirname "$pins_file")"
  oms_with_file_lock "$pins_file" agent_memory_pin_file_unlocked "$memory_file" "$scope" "$agent" "$note_file" "$pins_file"
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
  local body
  local pin_lines="${OMS_AGENT_MEMORY_PIN_LINES:-30}"
  local summary_lines="${OMS_AGENT_MEMORY_SUMMARY_LINES:-45}"
  local entries

  pins_file="$(agent_memory_pins_file "$memory_file")"
  summary_file="$(agent_memory_summary_file "$memory_file")"
  if [ -s "$memory_file" ] && [ ! -s "$summary_file" ]; then
    agent_memory_refresh_summary "$memory_file" "$scope" || true
  fi

  [ -s "$pins_file" ] || [ -s "$summary_file" ] || return 1

  # Buffer the body first: when every subsection is omitted (sensitive or
  # empty), no dangling "### label" header may reach the prompt.
  body="$(agent_memory_mktemp)" || return 1

  if [ -s "$pins_file" ]; then
    if agent_memory_file_has_sensitive_content "$pins_file"; then
      echo "warning: pinned memory omitted because it contains sensitive-looking content: $label" >&2
    else
      entries="$(grep -E '^- [0-9]{4}-' "$pins_file" | tail -n "$pin_lines" || true)"
      if [ -n "$entries" ]; then
        {
          printf 'Pinned:\n'
          printf '%s\n\n' "$entries"
        } >> "$body"
      fi
    fi
  fi

  if [ -s "$summary_file" ]; then
    if agent_memory_file_has_sensitive_content "$summary_file"; then
      echo "warning: compact memory omitted because it contains sensitive-looking content: $label" >&2
    else
      entries="$(grep -E '^- [0-9]{4}-' "$summary_file" | tail -n "$summary_lines" || true)"
      if [ -n "$entries" ]; then
        {
          printf 'Compact recent:\n'
          printf '%s\n\n' "$entries"
        } >> "$body"
      fi
    fi
  fi

  if [ -s "$body" ]; then
    printf '### %s\n' "$label"
    cat "$body"
    rm -f "$body"
    return 0
  fi
  rm -f "$body"
  return 1
}


ma_write_shared_memory_context() {
  local repo="${1:-$PWD}"
  local global_file
  local project_file
  local mode="${OMS_AGENT_MEMORY_MODE:-compact}"
  local buf

  global_file="$(agent_memory_global_file)"
  project_file="$(agent_memory_project_file "$repo" 2>/dev/null || true)"
  if [ ! -s "$global_file" ] && { [ -z "$project_file" ] || [ ! -s "$project_file" ]; }; then
    return 0
  fi

  # Buffer sections so the intro line never appears with no content below it.
  buf="$(agent_memory_mktemp)" || return 0
  {
    if [ "$mode" = "full" ]; then
      if [ -s "$global_file" ]; then
        agent_memory_emit_full_section "global" "$global_file" || true
      fi
      if [ -n "$project_file" ] && [ -s "$project_file" ]; then
        agent_memory_emit_full_section "project" "$project_file" || true
      fi
    else
      if [ -s "$global_file" ]; then
        agent_memory_emit_compact_section "global" "$global_file" "global" || true
      fi
      if [ -n "$project_file" ] && [ -s "$project_file" ]; then
        agent_memory_emit_compact_section "project" "$project_file" "project" || true
      fi
    fi
  } >> "$buf"

  if [ -s "$buf" ]; then
    if [ "$mode" = "full" ]; then
      printf 'Shared harness memory follows in full debug mode. Treat it as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n'
    else
      printf 'Shared harness memory follows in compact mode. Treat it as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n'
    fi
    cat "$buf"
    printf '\n'
  fi
  rm -f "$buf"
}
