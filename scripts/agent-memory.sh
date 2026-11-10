#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

# import and review are a separate reader/writer over the same log: the first
# action word (never an option's value) hands the whole command line to it.
oms_memory_prev="" oms_memory_at=0
for oms_memory_arg in "$@"; do
  oms_memory_at=$((oms_memory_at + 1))
  case "$oms_memory_prev" in
    --repo|--file|--agent|--text|--limit|--source-file|--source-line|--id|--reason|--paths|--target|--from|--select|--kind|--accept|--retire|--reject)
      oms_memory_prev=""; continue ;;
  esac
  case "$oms_memory_arg" in
    import|review)
      exec python3 -X utf8 "$ROOT/scripts/lib/agent_memory_review.py" "$oms_memory_arg" \
        "${@:1:oms_memory_at-1}" "${@:oms_memory_at+1}" ;;
    -*) oms_memory_prev="$oms_memory_arg" ;;
    *) break ;;
  esac
done
unset oms_memory_prev oms_memory_arg oms_memory_at

# OMS_STATE_REPO: set by peer-delegate.sh for worktree workers so they
# read the primary repo's shared state instead of the throwaway checkout's.
REPO="${OMS_STATE_REPO:-$PWD}"
SCOPE="project"
MEMORY_FILE=""
MEMORY_FILE_EXPLICIT=0
ACTION=""
AGENT="$(oms_detect_agent)"
# Separate from AGENT (which defaults to the detected agent): only set when
# --agent is passed, so `search` filters by author only on explicit request.
AGENT_SEARCH=""
TEXT=""
USE_STDIN=0
FULL=0
LIMIT=""
AS_JSON=0
JSON_ARGS=()
SOURCE_FILE=""
SOURCE_LINE=""
NOTE_PATHS=""
TARGET_PATH=""
INCLUDE_STALE=0
INCLUDE_SUPERSEDED=0
TARGET_ID=""
REASON=""
APPLY=0

# Normalize a comma-separated list of repository-relative globs or paths; a
# value that could name something outside the repository is refused.
oms_memory_path_list() {
  local raw="$1" part out="" count=0
  local -a parts=()
  IFS=, read -r -a parts <<< "$raw"
  for part in "${parts[@]}"; do
    part="${part#"${part%%[![:space:]]*}"}"; part="${part%"${part##*[![:space:]]}"}"
    part="${part#./}"
    [ -n "$part" ] || continue
    case "$part" in
      /*|*..*|*[!A-Za-z0-9._*?/-]*) return 1 ;;
    esac
    [ "${#part}" -le 120 ] || return 1
    count=$((count + 1))
    out="${out:+$out,}$part"
  done
  [ "$count" -ge 1 ] && [ "$count" -le 8 ] || return 1
  printf '%s\n' "$out"
}

usage() {
  cat <<'EOF'
Usage: agent-memory.sh [options] COMMAND [TEXT]

Maintain harness-owned memory shared by Codex, Claude Code, and Antigravity.
Project memory defaults to REPO/.oms/memory/shared.md. Global memory defaults to
~/.oh-my-setting/local/agent-memory.md.

Files:
  shared.md   Human-readable source log; not sent to providers by default.
              Append-only: correct a note by appending another, never by
              editing or trimming it. The worker-authority guard enforces this.
  pins.md     Short high-signal notes always eligible for provider context.
              Append-only, same contract.
  summary.md  Compact recent notes generated from shared.md; regenerated in
              place by `compact`, so it is derived state, not a record.
  memory.sqlite3
              Derived project index for exact search and ranked FTS recall.
              Existing Markdown logs are imported automatically and remain the
              append-only source of truth.

Options:
  --repo PATH       Project repo/directory for project memory. Default: PWD.
  --global          Use global harness memory instead of project memory.
  --file PATH       Use an explicit shared memory source file.
  --agent NAME      Agent label for appended notes / search author filter.
  --text TEXT       Note text for append/pin; query for search/recall.
  --stdin           Read append/pin note from stdin.
  --full            context: emit full source tail instead of compact view.
  --limit N         Limit search/recall results. Search is unlimited by default;
                    recall defaults to 20.
  --json            search/recall: emit JSONL with provenance metadata;
                    health: emit one schema-1 diagnostic object;
                    sources: emit one schema-1 inventory object.
  --source-file PATH
                    append: cite a tracked file from the current project HEAD.
                    Cited notes stay out of compact provider context and are
                    revalidated whenever they are searched or recalled.
  --source-line N   append: 1-based line paired with --source-file.
  --paths GLOB[,GLOB]
                    append: scope the note to repository-relative paths (`**`
                    crosses directories). A scoped note stays out of the
                    compact summary and out of injected recall unless the work's
                    paths match; plain recall and search still find it.
  --target PATH[,PATH]
                    recall: the paths being worked on. Notes scoped to a
                    matching path rank first; notes scoped elsewhere are left out.
  --id ID           supersede/retire/redact: target event_id from search/recall JSON.
  --apply           redact: remove the text. Without it redact only reports.
  --reason TEXT     retire: reason for retirement.
  --include-superseded search/recall: include superseded and retired notes, marked.
  --include-stale   search/recall: include cited notes whose source can no
                    longer be verified. Default recall omits them.
  -h, --help        Show help.

Commands:
  path              Print the resolved memory file path.
  db-path           Print the derived SQLite database path without creating it.
  show              Print source memory if it exists. Default.
  context           Print provider context view (compact by default).
  init              Create the source memory file if missing.
  append            Append a source note and refresh summary.md.
  pin               Append a short note to pins.md.
  supersede         Append a replacement note linked to --id.
  retire            Append a retirement record linked to --id.
  redact            Remove the text of the entry --id from shared.md or pins.md
                    and leave a fixed tombstone under its ID, then drop and
                    rebuild summary.md and the index. The one declared
                    exception to append-only, for text that should never have
                    been recorded. Dry run unless --apply; parent only.
                    Copies elsewhere (provider transcripts, handoffs, backups)
                    are not touched.
  sources           List the instruction and memory files each provider CLI
                    keeps for this repository: names, sizes, counts and times
                    only. No file is opened and nothing is written.
  compact           Regenerate summary.md from source memory.
  search PATTERN    Print memory + pins entries matching PATTERN (case-
                    insensitive substring over header and body) from SQLite;
                    --agent filters by the entry's author.
  recall QUERY      Rank memory + pins with local SQLite FTS; --agent filters
                    by author. No model call or embedding download is used.
  health            Diagnose source/index consistency without creating,
                    synchronizing, repairing, or otherwise changing either.
  rebuild           Recreate the derived database from the Markdown sources.
  import --from serena|PATH [--apply --select N,N|--all] [--json]
                    List candidate lines from Markdown memory written elsewhere
                    (a file, a directory, or .serena/memories); dry run unless
                    --apply, which appends the chosen ones as kind=imported.
  review [--kind task-close|distill|imported|correction] [--json]
         [--accept ID | --retire ID --reason TEXT | --reject ID]
                    List notes that reached shared memory unread (task-close
                    promotions, journal distillation, imports) and captured
                    corrections; accept or turn one down. Project memory only.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { echo "error: --repo requires path" >&2; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --global)
      SCOPE="global"
      shift
      ;;
    --file)
      [ "$#" -ge 2 ] || { echo "error: --file requires path" >&2; exit 2; }
      MEMORY_FILE="$2"
      MEMORY_FILE_EXPLICIT=1
      shift 2
      ;;
    --agent)
      [ "$#" -ge 2 ] || { echo "error: --agent requires name" >&2; exit 2; }
      AGENT="$2"
      AGENT_SEARCH="$2"
      shift 2
      ;;
    --text)
      [ "$#" -ge 2 ] || { echo "error: --text requires text" >&2; exit 2; }
      TEXT="$2"
      shift 2
      ;;
    --stdin)
      USE_STDIN=1
      shift
      ;;
    --full)
      FULL=1
      shift
      ;;
    --limit)
      [ "$#" -ge 2 ] || { echo "error: --limit requires a value" >&2; exit 2; }
      case "$2" in
        *[!0-9]*|"") echo "error: --limit requires a positive integer" >&2; exit 2 ;;
      esac
      [ "$2" -gt 0 ] || { echo "error: --limit requires a positive integer" >&2; exit 2; }
      LIMIT="$2"
      shift 2
      ;;
    --json)
      AS_JSON=1
      shift
      ;;
    --source-file)
      [ "$#" -ge 2 ] || { echo "error: --source-file requires a path" >&2; exit 2; }
      SOURCE_FILE="$2"
      shift 2
      ;;
    --source-line)
      [ "$#" -ge 2 ] || { echo "error: --source-line requires a value" >&2; exit 2; }
      case "$2" in
        *[!0-9]*|"") echo "error: --source-line requires a positive integer" >&2; exit 2 ;;
      esac
      [ "$2" -gt 0 ] || { echo "error: --source-line requires a positive integer" >&2; exit 2; }
      SOURCE_LINE="$2"
      shift 2
      ;;
    --paths|--target)
      [ "$#" -ge 2 ] || { echo "error: $1 requires a value" >&2; exit 2; }
      if [ "$1" = --paths ]; then NOTE_PATHS="$2"; else TARGET_PATH="$2"; fi
      shift 2
      ;;
    --id)
      [ "$#" -ge 2 ] || { echo "error: --id requires a value" >&2; exit 2; }
      TARGET_ID="$2"; shift 2 ;;
    --reason)
      [ "$#" -ge 2 ] || { echo "error: --reason requires text" >&2; exit 2; }
      REASON="$2"; shift 2 ;;
    --apply)
      APPLY=1; shift ;;
    --include-superseded)
      INCLUDE_SUPERSEDED=1; shift ;;
    --include-stale)
      INCLUDE_STALE=1
      shift
      ;;
    path|db-path|show|context|init|append|pin|supersede|retire|redact|sources|compact|search|recall|health|rebuild)
      ACTION="$1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -z "$ACTION" ]; then
        ACTION="$1"
      else
        TEXT="${TEXT:+$TEXT }$1"
      fi
      shift
      ;;
  esac
done

ACTION="${ACTION:-show}"
case "$ACTION" in
  append|pin|supersede|search|recall) ;;
  *)
    [ -z "$TEXT" ] || { echo "error: unknown argument: $TEXT" >&2; usage >&2; exit 2; }
    ;;
esac
if [ -n "$LIMIT" ]; then
  case "$ACTION" in
    search|recall) ;;
    *) echo "error: --limit applies only to search or recall" >&2; exit 2 ;;
  esac
fi
if [ -n "$SOURCE_FILE" ] || [ -n "$SOURCE_LINE" ]; then
  [ "$ACTION" = append ] || {
    echo "error: --source-file/--source-line apply only to append" >&2
    exit 2
  }
  [ -n "$SOURCE_FILE" ] && [ -n "$SOURCE_LINE" ] || {
    echo "error: --source-file and --source-line must be used together" >&2
    exit 2
  }
  [ "$SCOPE" = project ] || {
    echo "error: source citations require project memory" >&2
    exit 2
  }
fi
if [ -n "$NOTE_PATHS" ]; then
  [ "$ACTION" = append ] && [ "$SCOPE" = project ] && [ "$MEMORY_FILE_EXPLICIT" -eq 0 ] || {
    echo "error: --paths applies only to append on project memory" >&2; exit 2; }
  NOTE_PATHS="$(oms_memory_path_list "$NOTE_PATHS")" || {
    echo "error: --paths takes up to 8 repository-relative globs of [A-Za-z0-9._*?/-], no leading / and no .." >&2; exit 2; }
  export OMS_MEMORY_NOTE_PATHS="$NOTE_PATHS"
else
  unset OMS_MEMORY_NOTE_PATHS
fi
if [ -n "$TARGET_PATH" ]; then
  [ "$ACTION" = recall ] || { echo "error: --target applies only to recall" >&2; exit 2; }
  TARGET_PATH="$(oms_memory_path_list "$TARGET_PATH")" || {
    echo "error: --target takes up to 8 repository-relative paths of [A-Za-z0-9._*?/-], no leading / and no .." >&2; exit 2; }
  JSON_ARGS+=(--target-path "$TARGET_PATH")
fi
case "$ACTION" in
  supersede|retire|redact) [ -n "$TARGET_ID" ] || { echo "error: $ACTION requires --id" >&2; exit 2; } ;;
  *) [ -z "$TARGET_ID" ] && [ -z "$REASON" ] || { echo "error: --id/--reason apply only to supersede, retire or redact" >&2; exit 2; } ;;
esac
if [ "$ACTION" = redact ] && [ -n "$REASON" ]; then echo "error: --reason applies only to retire" >&2; exit 2; fi
if [ "$APPLY" -eq 1 ] && [ "$ACTION" != redact ]; then echo "error: --apply applies only to redact" >&2; exit 2; fi
if [ "$ACTION" = supersede ] && [ -n "$REASON" ]; then echo "error: --reason applies only to retire" >&2; exit 2; fi
if [ "$INCLUDE_SUPERSEDED" -eq 1 ]; then
  case "$ACTION" in search|recall) JSON_ARGS+=(--include-superseded) ;; *) echo "error: --include-superseded applies only to search or recall" >&2; exit 2 ;; esac
fi
if [ "$INCLUDE_STALE" -eq 1 ]; then
  case "$ACTION" in
    search|recall) JSON_ARGS+=(--include-stale) ;;
    *) echo "error: --include-stale applies only to search or recall" >&2; exit 2 ;;
  esac
fi
if [ "$AS_JSON" -eq 1 ]; then
  case "$ACTION" in
    search|recall|health|sources) JSON_ARGS+=(--json) ;;
    *) echo "error: --json applies only to search, recall, health, or sources" >&2; exit 2 ;;
  esac
fi
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ] &&
   { [ "$SCOPE" = global ] || [ "$MEMORY_FILE_EXPLICIT" -eq 1 ]; }; then
  case "$ACTION" in
    init|append|pin|compact|search|recall|rebuild)
      echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
      exit 2
      ;;
    context)
      [ "$FULL" -eq 1 ] || {
        echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
        exit 2
      }
      ;;
  esac
fi
if [ -z "$MEMORY_FILE" ]; then
  if [ "$SCOPE" = "global" ]; then
    MEMORY_FILE="$(agent_memory_global_file)"
  else
    MEMORY_FILE="$(agent_memory_project_file "$REPO")"
  fi
fi

# Lazily created: read-only actions (path, show) must not depend on a
# writable TMPDIR. Library temp files land here too once created.
OMS_MEMORY_TMPDIR=""
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  if [ -n "$OMS_MEMORY_TMPDIR" ]; then rm -rf "$OMS_MEMORY_TMPDIR"; fi
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM
ensure_tmpdir() {
  [ -n "$OMS_MEMORY_TMPDIR" ] && return 0
  OMS_MEMORY_TMPDIR="$(mktemp -d)" || exit 1
  export OMS_LIB_TMPDIR="$OMS_MEMORY_TMPDIR"
}

write_note_file() {
  local note_file="$1"
  if [ "$USE_STDIN" -eq 1 ]; then
    cat > "$note_file"
  else
    [ -n "$TEXT" ] || { echo "error: $ACTION requires --text or --stdin" >&2; exit 2; }
    printf '%s\n' "$TEXT" > "$note_file"
  fi
}

case "$ACTION" in
  path)
    printf '%s\n' "$MEMORY_FILE"
    ;;
  db-path)
    printf '%s\n' "$(agent_memory_db_file "$MEMORY_FILE")"
    ;;
  init)
    agent_memory_init_file "$MEMORY_FILE" "$SCOPE"
    agent_memory_sync_db "$MEMORY_FILE"
    echo "memory: initialized $MEMORY_FILE"
    echo "memory: indexed $(agent_memory_db_file "$MEMORY_FILE")"
    ;;
  show)
    if [ -s "$MEMORY_FILE" ]; then
      cat "$MEMORY_FILE"
    else
      echo "memory: empty ($MEMORY_FILE)"
    fi
    ;;
  context)
    ensure_tmpdir
    if [ "$FULL" -eq 1 ]; then
      agent_memory_emit_full_section "$SCOPE" "$MEMORY_FILE" || true
    else
      agent_memory_emit_compact_section "$SCOPE" "$MEMORY_FILE" "$SCOPE" || true
    fi
    ;;
  append)
    ensure_tmpdir
    note_file="$(mktemp "$OMS_MEMORY_TMPDIR/note.XXXXXX")"
    write_note_file "$note_file"
    agent_memory_append_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file" \
      note "$SOURCE_FILE" "$SOURCE_LINE"
    echo "memory: appended $MEMORY_FILE"
    echo "memory: refreshed $(agent_memory_summary_file "$MEMORY_FILE")"
    ;;
  supersede|retire)
    if [ "${OMS_HARNESS_CHILD:-0}" = 1 ] &&
       { [ "$SCOPE" = global ] || [ "$MEMORY_FILE_EXPLICIT" -eq 1 ]; }; then
      echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
      exit 2
    fi
    ensure_tmpdir
    note_file="$(mktemp "$OMS_MEMORY_TMPDIR/note.XXXXXX")"
    if [ "$ACTION" = retire ]; then
      [ -n "$REASON" ] || { echo "error: retire requires --reason" >&2; exit 2; }
      printf '%s\n' "$REASON" > "$note_file"
      agent_memory_append_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file" retirement "" "" "$TARGET_ID" retires
    else
      write_note_file "$note_file"
      [ -s "$note_file" ] || { echo "error: supersede requires nonempty text" >&2; exit 2; }
      agent_memory_append_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file" note "" "" "$TARGET_ID" supersedes
    fi
    echo "memory: $ACTION recorded in $MEMORY_FILE"
    ;;
  redact)
    if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
      echo "error: only the parent session may redact memory; return the request to the parent agent" >&2
      exit 2
    fi
    ensure_tmpdir
    agent_memory_redact "$MEMORY_FILE" "$SCOPE" "$AGENT" "$TARGET_ID" "$APPLY"
    ;;
  sources)
    [ "$SCOPE" = project ] && [ "$MEMORY_FILE_EXPLICIT" -eq 0 ] || {
      echo "error: sources reports one repository; use --repo, not --global or --file" >&2
      exit 2
    }
    python3 "$ROOT/scripts/lib/agent_memory_sources.py" --repo "$REPO" "${JSON_ARGS[@]}"
    ;;
  pin)
    ensure_tmpdir
    note_file="$(mktemp "$OMS_MEMORY_TMPDIR/note.XXXXXX")"
    write_note_file "$note_file"
    agent_memory_pin_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file"
    echo "memory: pinned $(agent_memory_pins_file "$MEMORY_FILE")"
    ;;
  compact)
    ensure_tmpdir
    agent_memory_refresh_summary "$MEMORY_FILE" "$SCOPE"
    agent_memory_sync_db "$MEMORY_FILE"
    echo "memory: refreshed $(agent_memory_summary_file "$MEMORY_FILE")"
    ;;
  search)
    [ -n "$TEXT" ] || { echo "error: search requires --text PATTERN (or a positional pattern)" >&2; exit 2; }
    agent_memory_db_command "$MEMORY_FILE" search \
      --query "$TEXT" \
      --agent "$AGENT_SEARCH" \
      --limit "${LIMIT:-0}" \
      "${JSON_ARGS[@]}"
    ;;
  recall)
    [ -n "$TEXT" ] || { echo "error: recall requires --text QUERY (or a positional query)" >&2; exit 2; }
    agent_memory_db_command "$MEMORY_FILE" recall \
      --query "$TEXT" \
      --agent "$AGENT_SEARCH" \
      --limit "${LIMIT:-20}" \
      "${JSON_ARGS[@]}"
    ;;
  health)
    agent_memory_db_health "$MEMORY_FILE" "${JSON_ARGS[@]}"
    ;;
  rebuild)
    agent_memory_db_command "$MEMORY_FILE" rebuild
    echo "memory: rebuilt $(agent_memory_db_file "$MEMORY_FILE")"
    ;;
  *)
    echo "error: unknown command: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
