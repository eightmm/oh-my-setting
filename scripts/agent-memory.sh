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

usage() {
  cat <<'EOF'
Usage: agent-memory.sh [options] [path|show|init|append]

Maintain harness-owned memory shared by Codex, Claude Code, and Antigravity.
Project memory defaults to REPO/.oms/memory/shared.md. Global memory defaults to
~/.oh-my-setting/local/agent-memory.md.

Options:
  --repo PATH       Project repo/directory for project memory. Default: PWD.
  --global          Use global harness memory instead of project memory.
  --file PATH       Use an explicit memory file.
  --agent NAME      Agent label for appended notes. Default: agent.
  --text TEXT       Note text for append.
  --stdin           Read append note from stdin.
  -h, --help        Show help.

Commands:
  path              Print the resolved memory file path.
  show              Print memory if it exists. Default.
  init              Create the memory file if missing.
  append            Append a note after sensitive-content screening.
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
    path|show|init|append)
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
if [ -z "$MEMORY_FILE" ]; then
  if [ "$SCOPE" = "global" ]; then
    MEMORY_FILE="$(agent_memory_global_file)"
  else
    MEMORY_FILE="$(agent_memory_project_file "$REPO")"
  fi
fi

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
  append)
    note_file="$(mktemp)"
    cleanup() { rm -f "$note_file"; }
    trap cleanup EXIT
    if [ "$USE_STDIN" -eq 1 ]; then
      cat > "$note_file"
    else
      [ -n "$TEXT" ] || { echo "error: append requires --text or --stdin" >&2; exit 2; }
      printf '%s\n' "$TEXT" > "$note_file"
    fi
    agent_memory_append_file "$MEMORY_FILE" "$SCOPE" "$AGENT" "$note_file"
    echo "memory: appended $MEMORY_FILE"
    ;;
  *)
    echo "error: unknown command: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
