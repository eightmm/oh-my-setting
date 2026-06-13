#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
SCOPE="project"
MEMORY_FILE=""
ACTION=""
AGENT="agent"
TEXT=""
USE_STDIN=0
FULL=0

usage() {
  cat <<'EOF'
Usage: agent-memory.sh [options] [path|show|context|init|append|pin|compact]

Maintain harness-owned memory shared by Codex, Claude Code, and Antigravity.
Project memory defaults to REPO/.oms/memory/shared.md. Global memory defaults to
~/.oh-my-setting/local/agent-memory.md.

Files:
  shared.md   Human-readable source log; not sent to providers by default.
  pins.md     Short high-signal notes always eligible for provider context.
  summary.md  Compact recent notes generated from shared.md.

Options:
  --repo PATH       Project repo/directory for project memory. Default: PWD.
  --global          Use global harness memory instead of project memory.
  --file PATH       Use an explicit shared memory source file.
  --agent NAME      Agent label for appended notes. Default: agent.
  --text TEXT       Note text for append/pin.
  --stdin           Read append/pin note from stdin.
  --full            context: emit full source tail instead of compact view.
  -h, --help        Show help.

Commands:
  path              Print the resolved memory file path.
  show              Print source memory if it exists. Default.
  context           Print provider context view (compact by default).
  init              Create the source memory file if missing.
  append            Append a source note and refresh summary.md.
  pin               Append a short note to pins.md.
  compact           Regenerate summary.md from source memory.
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
      shift 2
      ;;
    --agent)
      [ "$#" -ge 2 ] || { echo "error: --agent requires name" >&2; exit 2; }
      AGENT="$2"
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
    path|show|context|init|append|pin|compact)
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
  append|pin) ;;
  *)
    [ -z "$TEXT" ] || { echo "error: unknown argument: $TEXT" >&2; usage >&2; exit 2; }
    ;;
esac
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
  init)
    agent_memory_init_file "$MEMORY_FILE" "$SCOPE"
    echo "memory: initialized $MEMORY_FILE"
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
    agent_memory_append_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file"
    echo "memory: appended $MEMORY_FILE"
    echo "memory: refreshed $(agent_memory_summary_file "$MEMORY_FILE")"
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
    echo "memory: refreshed $(agent_memory_summary_file "$MEMORY_FILE")"
    ;;
  *)
    echo "error: unknown command: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
