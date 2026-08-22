# shellcheck shell=bash
# Shared harness memory helpers. Sourced, not executed.

AGENT_MEMORY_COMMON_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_MEMORY_DB_HELPER="$AGENT_MEMORY_COMMON_LIB_DIR/agent-memory-db.py"

# shellcheck source=file-lock.sh
. "$AGENT_MEMORY_COMMON_LIB_DIR/file-lock.sh"
# shellcheck source=oms-common.sh
. "$AGENT_MEMORY_COMMON_LIB_DIR/oms-common.sh"

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

# The caller identity peer selection must exclude, or "" when unknown. One
# definition for advise and consult: OMS_AGENT wins when exported, detection
# covers interactive sessions that never export it, and the generic "agent"
# answer means unknown rather than a real family to exclude.
oms_peer_caller() {
  local caller

  caller="$(oms_detect_agent 2>/dev/null || printf '')"
  [ "$caller" != agent ] || caller=""
  printf '%s\n' "$caller"
}

# A provider process is delegated work, not a new harness owner. Let it report
# that another opinion or worker is needed, but do not let it reset attempt
# budgets, spend through a fresh peer call, or widen itself into write work.
oms_require_peer_owner() {
  [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && return 0
  printf '%s\n' \
    'error: a harness child cannot start peer calls or delegation; recursive delegation and provider spend belong to the parent - report the need in your answer instead' >&2
  return 2
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

# The failure ledger sits beside the memory directory, not inside it: it is
# written by fail-ledger.sh as JSONL, and indexed here only so one recall
# covers notes, pins, and what has already gone wrong.
agent_memory_failures_file() {
  local file="$1"
  local dir
  dir="$(agent_memory_dir "$file")"
  case "$dir" in
    */.oms/memory) printf '%s/failures.jsonl\n' "${dir%/memory}" ;;
    *) printf '%s/failures.jsonl\n' "$dir" ;;
  esac
}

agent_memory_db_file() {
  local file="$1"
  local dir
  local name

  dir="$(agent_memory_dir "$file")"
  name="$(basename "$file")"
  case "$name" in
    shared.md) printf '%s/memory.sqlite3\n' "$dir" ;;
    *.*) printf '%s/%s.sqlite3\n' "$dir" "${name%.*}" ;;
    *) printf '%s/%s.sqlite3\n' "$dir" "$name" ;;
  esac
}

agent_memory_db_command() {
  local memory_file="$1"
  local command_name="$2"
  local db_file
  local repo=""
  local repo_args=()
  shift 2

  command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 required for the memory database" >&2
    return 2
  }
  [ -f "$AGENT_MEMORY_DB_HELPER" ] || {
    echo "error: memory database helper missing: $AGENT_MEMORY_DB_HELPER" >&2
    return 2
  }
  db_file="$(agent_memory_db_file "$memory_file")"
  repo="$(agent_memory_repo_for_file "$memory_file" 2>/dev/null || true)"
  if [ -n "$repo" ]; then
    repo_args=(--repo "$repo")
  fi
  agent_memory_ensure_oms_ignore_for_path "$db_file"
  python3 "$AGENT_MEMORY_DB_HELPER" "$command_name" \
    --db "$db_file" \
    --shared "$memory_file" \
    --pins "$(agent_memory_pins_file "$memory_file")" \
    --failures "$(agent_memory_failures_file "$memory_file")" \
    "${repo_args[@]}" \
    "$@"
}

agent_memory_db_health() {
  local memory_file="$1"
  local db_file
  shift

  command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 required for the memory database" >&2
    return 2
  }
  [ -f "$AGENT_MEMORY_DB_HELPER" ] || {
    echo "error: memory database helper missing: $AGENT_MEMORY_DB_HELPER" >&2
    return 2
  }
  db_file="$(agent_memory_db_file "$memory_file")"
  # Unlike search/recall/rebuild, health is a pure query. In particular it must
  # not create .oms/.gitignore or synchronize a stale derived database.
  python3 "$AGENT_MEMORY_DB_HELPER" health \
    --db "$db_file" \
    --shared "$memory_file" \
    --pins "$(agent_memory_pins_file "$memory_file")" \
    --failures "$(agent_memory_failures_file "$memory_file")" \
    "$@"
}

agent_memory_sync_db() {
  local memory_file="$1"

  agent_memory_db_command "$memory_file" sync
}

agent_memory_sync_db_best_effort() {
  local memory_file="$1"

  if ! agent_memory_sync_db "$memory_file"; then
    echo "warning: memory note is safe in the Markdown source, but its database index is stale" >&2
  fi
  return 0
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
#
# Two tiers. Secret tier: credential material that must never be written
# anywhere, no matter the destination. Machine tier: machine identity (home
# paths, HPC filesystems, cluster fields) that must stay out of git-tracked
# files and outbound prompts, but is harmless in repo-local git-ignored state
# once normalized — a handoff digest that cannot mention a file path is empty.
agent_memory_secret_re() {
  printf '%s\n' '((^|[^A-Za-z0-9])[A-Za-z0-9_]*(t[o]ken|s[e]cret|passw(or)?[d]|credentia[l]s?|(ap[i]|s[e]cret|privat[e])[-_ ]?(ke[y]|t[o]ken)|aws_s[e]cret_access_[k]ey)["'\'']?[[:space:]]*[:=]|auth[o]rization:[[:space:]]+[^[:space:]]+|bear[e]r[[:space:]]+[A-Za-z0-9._-]{10,}|[a-z][a-z0-9+.-]*://[^[:space:]/:@]+:[^*[:space:]@/][^[:space:]@/]*@[^[:space:]/]+|(^|[^A-Za-z0-9_-])ey[J][A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}($|[^A-Za-z0-9_-])|gh[pousr]_[A-Za-z0-9_]{20,}|githu[b]_pa[t]_[A-Za-z0-9_]{20,}|npm_[A-Za-z0-9]{36,}|h[f]_[A-Za-z0-9]{34,}|glpa[t]-[A-Za-z0-9_-]{20,}|[sr]k_(liv[e]|tes[t])_[A-Za-z0-9]{16,}|AI[z]a[0-9A-Za-z_-]{35}|hook[s]\.slac[k]\.com/service[s]/|disc[o]rd(app)?\.com/api/webhook[s]/|machin[e][[:space:]]+[^[:space:]]+[[:space:]]+logi[n][[:space:]]+[^[:space:]]+[[:space:]]+passwor[d][[:space:]]+[^[:space:]]+|(^|[^A-Za-z0-9_])s[k]-[A-Za-z0-9_-]{10,}|xox[bap]-[A-Za-z0-9-]{10,}|AK[I]A[0-9A-Z]{16}|-----BE[G]IN)'
}

agent_memory_machine_re() {
  printf '%s\n' '(/hom[e]/[^[:space:]]+|/User[s]/[^[:space:]]+|/scratc[h]/[^[:space:]]+|/lustr[e]/[^[:space:]]+|/gpf[s]/[^[:space:]]+|/beegf[s]/[^[:space:]]+|\.ss[h]/|\.aw[s]/|clust[e]r[[:space:]]*[:=]|partiti[o]n[[:space:]]*[:=]|nodelis[t][[:space:]]*[:=]|sbatc[h][[:space:]]+--partition)'
}

agent_memory_sensitive_re() {
  printf '%s|%s\n' "$(agent_memory_secret_re)" "$(agent_memory_machine_re)"
}

agent_memory_file_has_sensitive_content() {
  local file="$1"
  [ -s "$file" ] || return 1
  grep -Eiq "$(agent_memory_sensitive_re)" "$file"
}

# Where the scrubber matched, and in which half of the composed prompt, without
# echoing the match itself. A refusal that names nothing costs a whole round: a
# caller cannot tell its own sentence from the memory, task packet or git
# context the harness attached, the tier decides whether the fix is "remove it"
# or "write it relative", and the regex is deliberately private. Line numbers
# and a tier name are a pointer, never a disclosure.
agent_memory_sensitive_report() {  # FILE
  local file="$1"
  local begin end tier re line number region
  local prompt_lines context_lines prompt_count context_count

  [ -s "$file" ] || return 0
  begin="$(grep -n -m1 -F -- '--- begin harness context' "$file" 2>/dev/null | cut -d: -f1)"
  end="$(grep -n -m1 -F -- '--- end harness context' "$file" 2>/dev/null | cut -d: -f1)"
  case "$begin" in *[!0-9]*|"") begin=0 ;; esac
  case "$end" in *[!0-9]*|"") end=0 ;; esac
  for tier in secret machine; do
    if [ "$tier" = secret ]; then
      re="$(agent_memory_secret_re)"
    else
      re="$(agent_memory_machine_re)"
    fi
    prompt_lines=""; context_lines=""; prompt_count=0; context_count=0
    while IFS= read -r line; do
      number="${line%%:*}"
      case "$number" in *[!0-9]*|"") continue ;; esac
      region=prompt
      if [ "$begin" -gt 0 ] && [ "$number" -gt "$begin" ] &&
        { [ "$end" -eq 0 ] || [ "$number" -lt "$end" ]; }; then
        region=context
      fi
      if [ "$region" = context ]; then
        context_count=$((context_count + 1))
        [ "$context_count" -gt 3 ] || context_lines="${context_lines:+$context_lines, }$number"
      else
        prompt_count=$((prompt_count + 1))
        [ "$prompt_count" -gt 3 ] || prompt_lines="${prompt_lines:+$prompt_lines, }$number"
      fi
    done <<EOF
$(grep -Ein "$re" "$file" 2>/dev/null | cut -d: -f1)
EOF
    agent_memory_sensitive_report_line "$tier" "your prompt" "$prompt_lines" "$prompt_count"
    agent_memory_sensitive_report_line "$tier" "attached harness context" "$context_lines" "$context_count"
  done
}

agent_memory_sensitive_report_line() {  # TIER WHERE LINES COUNT
  local tier="$1" where="$2" lines="$3" count="$4"
  local more=""

  [ "$count" -gt 0 ] || return 0
  [ "$count" -le 3 ] || more=" (+$((count - 3)) more)"
  if [ "$tier" = secret ]; then
    printf 'secret-tier match in %s at line %s%s: credential material is never sent\n' \
      "$where" "$lines" "$more"
  else
    printf 'machine-tier match in %s at line %s%s: home/cluster paths and node fields; write them repository-relative\n' \
      "$where" "$lines" "$more"
  fi
}

agent_memory_file_has_secret_content() {
  local file="$1"
  [ -s "$file" ] || return 1
  grep -Eiq "$(agent_memory_secret_re)" "$file"
}

# Rewrite machine-identifying path prefixes to ~ so repo-local records can keep
# their content without recording the account layout. Textual only, applied to
# stored copies — never to a command that still has to execute. Secret-tier
# content is not rewritten: a secret must block, not be laundered into
# something that scans clean.
agent_memory_normalize_machine_paths() {
  local home_re
  home_re="$(printf '%s' "${HOME:-}" | sed -e 's/[][\.*^$/]/\\&/g')"
  if [ -n "$home_re" ]; then
    sed -E -e "s/$home_re/~/g" \
      -e 's|/hom[e]/[A-Za-z0-9._-]+|~|g' -e 's|/User[s]/[A-Za-z0-9._-]+|~|g'
  else
    sed -E -e 's|/hom[e]/[A-Za-z0-9._-]+|~|g' -e 's|/User[s]/[A-Za-z0-9._-]+|~|g'
  fi
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

# Scratch for an atomic replace of TARGET: the temp file must live in the
# target's own directory, or `mv` degrades to copy+unlink whenever TMPDIR is a
# different filesystem (tmpfs /tmp against an ext4 repo is the common case) —
# and a copy-in-place gives every concurrent reader a window where the shared
# state file is empty or truncated. Same-directory rename is atomic on any
# POSIX filesystem.
agent_memory_mktemp_beside() {
  local target="$1"

  mktemp "$(dirname "$target")/.oms-replace.XXXXXX"
}

agent_memory_repo_for_file() {
  local file="$1"
  local repo=""

  case "$file" in
    */.oms/memory/*) repo="${file%/.oms/memory/*}" ;;
    *) return 1 ;;
  esac
  if [ -z "$repo" ] ||
    ! git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    return 1
  fi
  oms_repo_root "$repo"
}

agent_memory_task_metadata_value() {
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

agent_memory_new_event_id() {
  printf 'mem-%s-%s-%s\n' \
    "$(date -u +%Y%m%dT%H%M%SZ)" "$$" "${RANDOM:-0}"
}

# Capture provenance before taking the append lock. Only bounded identifiers
# and hashes are persisted by default: no diff, command, or branch name enters
# memory or provider context. A user-requested source citation is the one
# exception: its repository-relative tracked path and exact line hash remain in
# the source log, but cited notes are excluded from compact provider context.
agent_memory_write_metadata() {
  local memory_file="$1"
  local kind="$2"
  local output="$3"
  local citation_file="${4:-}"
  local repo=""
  local task_file=""
  local task_id="${OMS_TASK_ID:-}"
  local session_hash="${OMS_AGENT_TASK_SOURCE_SESSION:-}"
  local git_sha=""
  local git_dirty=""
  local git_state=""
  local untracked=""

  repo="$(agent_memory_repo_for_file "$memory_file" 2>/dev/null || true)"
  if [ -n "$repo" ]; then
    task_file="$repo/.oms/task/current.md"
    [ -n "$task_id" ] ||
      task_id="$(agent_memory_task_metadata_value "$task_file" task_id 2>/dev/null || true)"
    [ -n "$session_hash" ] ||
      session_hash="$(agent_memory_task_metadata_value "$task_file" source_session 2>/dev/null || true)"
    git_sha="$(git -C "$repo" rev-parse HEAD 2>/dev/null || true)"
    untracked="$(git -C "$repo" ls-files --others --exclude-standard 2>/dev/null |
      sed -n '1p')"
    if ! git -C "$repo" diff --quiet HEAD -- 2>/dev/null ||
      ! git -C "$repo" diff --cached --quiet -- 2>/dev/null ||
      [ -n "$untracked" ]; then
      git_dirty=1
    else
      git_dirty=0
    fi
    git_state="$(oms_git_state_fingerprint "$repo" 2>/dev/null || true)"
  fi

  {
    printf '<!-- oms-memory\n'
    printf 'schema: 1\n'
    printf 'event_id: %s\n' "$(agent_memory_new_event_id)"
    printf 'kind: %s\n' "$kind"
    printf 'task_id: %s\n' "$task_id"
    printf 'session_hash: %s\n' "$session_hash"
    printf 'git_sha: %s\n' "$git_sha"
    printf 'git_dirty: %s\n' "$git_dirty"
    printf 'git_state: %s\n' "$git_state"
    if [ -n "$citation_file" ] && [ -s "$citation_file" ]; then
      cat "$citation_file"
    fi
    printf -- '-->\n'
  } > "$output"
}

# Resolve one citation against the committed tree. Working-tree content is not
# accepted as evidence: the stable blob oid and exact line bytes must be
# reproducible on another checkout of the same commit.
agent_memory_write_source_citation() {
  local repo="$1"
  local source_file="$2"
  local source_line="$3"
  local output="$4"
  local source_path=""
  local blob_oid=""
  local line_hash=""

  [ -n "$source_file" ] && [ -n "$source_line" ] || return 0
  repo="$(oms_repo_root "$repo")" || return 1
  case "$source_file" in
    *$'\n'*|*$'\r'*)
      echo "error: citation paths cannot contain newlines" >&2
      return 2
      ;;
  esac
  # Disable Git's default C-style quoting so spaces and non-ASCII repository
  # paths remain the exact tree path consumed by rev-parse/show below.
  source_path="$(git -C "$repo" -c core.quotePath=false \
    ls-files --full-name --error-unmatch -- "$source_file" 2>/dev/null || true)"
  source_path="$(printf '%s' "$source_path" | tr -d '\r')"
  [ -n "$source_path" ] && [ "$(printf '%s\n' "$source_path" | wc -l | tr -d ' ')" -eq 1 ] || {
    echo "error: citation source must be one tracked file in the current project: $source_file" >&2
    return 2
  }
  blob_oid="$(git -C "$repo" rev-parse "HEAD:$source_path" 2>/dev/null || true)"
  blob_oid="$(printf '%s' "$blob_oid" | tr -d '\r')"
  [ -n "$blob_oid" ] && [ "$(git -C "$repo" cat-file -t "$blob_oid" 2>/dev/null || true)" = blob ] || {
    echo "error: citation source is not a committed file at HEAD: $source_path" >&2
    return 2
  }
  line_hash="$(git -C "$repo" show "HEAD:$source_path" | python3 -c '
import hashlib, sys
line = int(sys.argv[1])
rows = sys.stdin.buffer.read().splitlines()
if line < 1 or line > len(rows):
    raise SystemExit(3)
sys.stdout.write(hashlib.sha256(rows[line - 1]).hexdigest())
' "$source_line")" || {
    echo "error: citation line $source_line does not exist in HEAD:$source_path" >&2
    return 2
  }
  line_hash="$(printf '%s' "$line_hash" | tr -d '\r')"
  {
    printf 'source_path: %s\n' "$source_path"
    printf 'source_line: %s\n' "$source_line"
    printf 'source_line_sha256: %s\n' "$line_hash"
    printf 'source_blob_oid: %s\n' "$blob_oid"
  } > "$output"
}

oms_check_sh_has_ml_smoke() {
  local check_sh_path="$1"

  [ -f "$check_sh_path" ] || return 1
  grep -Eq '(^|[[:space:]("|'\''])ml-smoke("|'\'')?\)' "$check_sh_path"
}

# The template check.sh dispatches on a `fast)` case arm; a project that wrote
# its own check.sh (this repo included) may not. Auto-verify defaults must probe
# before invoking `check.sh fast`, or the gate runs an invalid command and
# blocks on a usage error instead of the diff.
oms_check_sh_has_fast_mode() {
  local check_sh_path="$1"

  [ -f "$check_sh_path" ] || return 1
  grep -Eq '(^|[[:space:]("|'\''])fast("|'\'')?\)' "$check_sh_path"
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
  local staged

  # Prompt readers take no lock, so the swap must be one same-directory
  # rename: truncate-then-append exposed a header-only summary to any reader
  # racing the refresh. The lock above still serializes writers.
  staged="$(agent_memory_mktemp_beside "$summary_file")" || return 1
  agent_memory_write_summary_header "$staged" "$scope"
  if ! cat "$body_file" >> "$staged"; then
    rm -f "$staged"
    return 1
  fi
  mv "$staged" "$summary_file"
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
  tmp="$(agent_memory_mktemp_beside "$summary_file")" || return 1

  awk -v max_chars="$chars" '
    $0 == "<!-- oms-memory" {
      in_metadata=1
      pending_cited=0
      next
    }
    in_metadata {
      if ($0 == "-->") {
        in_metadata=0
      } else if ($0 ~ /^source_path:[[:space:]]*[^[:space:]]/) {
        pending_cited=1
      }
      next
    }
    /^## / {
      current=$0
      sub(/^## /, "", current)
      captured=0
      cited=pending_cited
      pending_cited=0
      next
    }
    current != "" && captured == 0 && NF {
      if (cited) {
        captured=1
        next
      }
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

# shared.md and pins.md are append-only, and that is a contract other parts of
# the harness rely on: the worker-authority guard verifies that existing bytes
# never change, so a worker cannot quietly rewrite what another agent recorded.
# Never rewrite or trim these in place — a correction is another appended note,
# and compaction belongs in the derived summary.md.
agent_memory_append_file_unlocked() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local occurred_at="$5"
  local metadata_file="$6"

  agent_memory_init_file_unlocked "$memory_file" "$scope"
  {
    cat "$metadata_file"
    printf '## %s %s\n\n' "$occurred_at" "$agent"
    cat "$note_file"
    printf '\n\n'
  } >> "$memory_file"
}

agent_memory_append_file() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local kind="${5:-note}"
  local source_file="${6:-}"
  local source_line="${7:-}"
  local occurred_at
  local metadata_file
  local citation_file=""
  local repo=""

  if agent_memory_file_has_sensitive_content "$note_file"; then
    echo "error: memory note contains sensitive-looking content; not appended" >&2
    return 3
  fi
  if grep -Fq '<!-- oms-memory' "$note_file"; then
    echo "error: memory note contains the reserved memory metadata marker; not appended" >&2
    return 3
  fi

  occurred_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  metadata_file="$(agent_memory_mktemp)" || return 1
  if [ -n "$source_file" ] || [ -n "$source_line" ]; then
    [ -n "$source_file" ] && [ -n "$source_line" ] || {
      echo "error: citation source file and line must be provided together" >&2
      rm -f "$metadata_file"
      return 2
    }
    repo="$(agent_memory_repo_for_file "$memory_file" 2>/dev/null || true)"
    [ -n "$repo" ] || {
      echo "error: source citations require project memory under REPO/.oms/memory" >&2
      rm -f "$metadata_file"
      return 2
    }
    citation_file="$(agent_memory_mktemp)" || {
      rm -f "$metadata_file"
      return 1
    }
    if ! agent_memory_write_source_citation "$repo" "$source_file" "$source_line" "$citation_file"; then
      rm -f "$metadata_file" "$citation_file"
      return 2
    fi
  fi
  if ! agent_memory_write_metadata "$memory_file" "$kind" "$metadata_file" "$citation_file"; then
    rm -f "$metadata_file"
    [ -z "$citation_file" ] || rm -f "$citation_file"
    return 1
  fi
  [ -z "$citation_file" ] || rm -f "$citation_file"
  if ! oms_with_file_lock "$memory_file" agent_memory_append_file_unlocked \
    "$memory_file" "$scope" "$agent" "$note_file" "$occurred_at" "$metadata_file"; then
    rm -f "$metadata_file"
    return 1
  fi
  rm -f "$metadata_file"
  # The note is already written; a stale summary must not turn the append
  # into a failure (refresh prints its own warning).
  agent_memory_refresh_summary "$memory_file" "$scope" || true
  agent_memory_sync_db_best_effort "$memory_file"
}

agent_memory_pin_file_unlocked() {
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local pins_file="$5"
  local occurred_at="$6"
  local metadata_file="$7"
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
  {
    cat "$metadata_file"
    printf -- '- %s [%s] %s\n' "$occurred_at" "$agent" "$line"
  } >> "$pins_file"
}

agent_memory_pin_file() {
  local memory_file="$1"
  local scope="$2"
  local agent="$3"
  local note_file="$4"
  local pins_file
  local occurred_at
  local metadata_file

  if agent_memory_file_has_sensitive_content "$note_file"; then
    echo "error: memory pin contains sensitive-looking content; not appended" >&2
    return 3
  fi
  if grep -Fq '<!-- oms-memory' "$note_file"; then
    echo "error: memory pin contains the reserved memory metadata marker; not appended" >&2
    return 3
  fi

  pins_file="$(agent_memory_pins_file "$memory_file")"
  agent_memory_ensure_oms_ignore_for_path "$pins_file"
  mkdir -p "$(dirname "$pins_file")"
  occurred_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  metadata_file="$(agent_memory_mktemp)" || return 1
  if ! agent_memory_write_metadata "$memory_file" pin "$metadata_file"; then
    rm -f "$metadata_file"
    return 1
  fi
  if ! oms_with_file_lock "$pins_file" agent_memory_pin_file_unlocked \
    "$memory_file" "$scope" "$agent" "$note_file" "$pins_file" "$occurred_at" "$metadata_file"; then
    rm -f "$metadata_file"
    return 1
  fi
  rm -f "$metadata_file"
  agent_memory_sync_db_best_effort "$memory_file"
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
  # Line-bounded is not size-bounded: one pasted wall of text is one line.
  # Capture, then byte-cap from the file so the consumer's early exit has no
  # writer to kill.
  local buf_file total
  buf_file="$(agent_memory_mktemp)" || return 1
  tail -n "$lines" "$file" > "$buf_file"
  total="$(LC_ALL=C wc -c < "$buf_file" | tr -d ' ')"
  if [ "$total" -gt "${OMS_AGENT_MEMORY_FULL_BYTES:-16384}" ]; then
    agent_memory_truncate_bytes "${OMS_AGENT_MEMORY_FULL_BYTES:-16384}" < "$buf_file"
    printf '\n[shared memory truncated: %s of %s bytes shown; OMS_AGENT_MEMORY_FULL_BYTES]\n' \
      "${OMS_AGENT_MEMORY_FULL_BYTES:-16384}" "$total"
  else
    cat "$buf_file"
  fi
  rm -f "$buf_file"
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


# Query-ranked recall rides along when the caller names its operation: the
# recency tails answer "what happened lately", this answers "what do we know
# about THIS". Ranking (FTS5 bm25 with a token-count fallback) lives in the
# db helper; here we only bound and scrub the result.
agent_memory_emit_recall_section() {
  local memory_file="$1"
  local query="$2"
  local limit="${OMS_AGENT_MEMORY_RECALL_LIMIT:-5}"
  local out

  [ -s "$memory_file" ] || return 1
  query="$(printf '%s' "$query" | head -c 300 | tr '\n' ' ')"
  [ -n "$query" ] || return 1
  out="$(agent_memory_mktemp)" || return 1
  if ! agent_memory_db_command "$memory_file" recall \
      --query "$query" --limit "$limit" > "$out" 2>/dev/null; then
    rm -f "$out"; return 1
  fi
  [ -s "$out" ] || { rm -f "$out"; return 1; }
  if agent_memory_file_has_sensitive_content "$out"; then
    echo "warning: ranked recall omitted because it contains sensitive-looking content" >&2
    rm -f "$out"; return 1
  fi
  printf '### relevant recall\n'
  agent_memory_truncate_bytes 4000 < "$out"
  printf '\n'
  rm -f "$out"
  return 0
}

ma_write_shared_memory_context() {
  local repo="${1:-$PWD}"
  local query="${2:-}"
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
    if [ -n "$query" ]; then
      # Project memory first — it holds this repo's lessons; fall back to the
      # global store only when the project has none.
      if [ -n "$project_file" ] && [ -s "$project_file" ]; then
        agent_memory_emit_recall_section "$project_file" "$query" || true
      elif [ -s "$global_file" ]; then
        agent_memory_emit_recall_section "$global_file" "$query" || true
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
