#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

# OMS_STATE_REPO: set by multi-agent-delegate.sh for worktree workers so they
# read the primary repo's shared state instead of the throwaway checkout's.
REPO="${OMS_STATE_REPO:-$PWD}"
SCOPE="project"
MEMORY_FILE=""
ACTION=""
AGENT="$(oms_detect_agent)"
# Separate from AGENT (which defaults to the detected agent): only set when
# --agent is passed, so `search` filters by author only on explicit request.
AGENT_SEARCH=""
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
  --agent NAME      Agent label for appended notes / search author filter.
  --text TEXT       Note text for append/pin; search pattern for search.
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
  search PATTERN    Print memory + pins entries matching PATTERN (case-
                    insensitive, over header and body); --agent filters by
                    the entry's author. Scales recall past cat-ing show.
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
    path|show|context|init|append|pin|compact|search)
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
  append|pin|search) ;;
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
  search)
    [ -n "$TEXT" ] || { echo "error: search requires --text PATTERN (or a positional pattern)" >&2; exit 2; }
    command -v python3 >/dev/null 2>&1 || { echo "error: python3 required for search" >&2; exit 2; }
    # Scan the source log and pins for '## <ts> <agent>' entry blocks whose
    # header or body matches; --agent filters by the entry author. Matches go
    # to stdout, the hit count to stderr, and exit is nonzero on no match so
    # an empty search is distinguishable from a miss.
    OMS_Q="$TEXT" OMS_AGENT_FILTER="$AGENT_SEARCH" \
      python3 - "$MEMORY_FILE" "$(agent_memory_pins_file "$MEMORY_FILE")" <<'PY'
import os, re, sys
q = os.environ["OMS_Q"].lower()
agent_filter = os.environ.get("OMS_AGENT_FILTER", "")
# shared.md/summary.md entries are '## <ts> <agent>' blocks; pins.md entries
# are one-line '- <ts> [<agent>] <text>' bullets. Handle both.
hdr = re.compile(r"^## (\S+) (.*)$")
pin = re.compile(r"^- (\S+) \[([^\]]+)\] (.*)$")
shown = 0
for src in sys.argv[1:]:
    if not src or not os.path.isfile(src):
        continue
    blocks = []
    cur = None
    for raw in open(src, encoding="utf-8", errors="replace"):
        line = raw.rstrip("\n")
        mp = pin.match(line)
        if mp:
            blocks.append({"agent": mp.group(2).strip(), "lines": [line]})
            cur = None
            continue
        mh = hdr.match(line)
        if mh:
            cur = {"agent": mh.group(2).strip(), "lines": [line]}
            blocks.append(cur)
        elif cur is not None:
            cur["lines"].append(line)
    for b in blocks:
        if agent_filter and b["agent"] != agent_filter:
            continue
        body = "\n".join(b["lines"]).strip()
        if q in body.lower():
            sys.stdout.write(body + "\n\n")
            shown += 1
sys.stderr.write("memory: %d match(es) for \"%s\"\n" % (shown, os.environ["OMS_Q"]))
sys.exit(0 if shown else 1)
PY
    ;;
  *)
    echo "error: unknown command: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
