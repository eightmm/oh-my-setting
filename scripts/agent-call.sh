#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/multi-agent-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/multi-agent-common.sh"

MA_KIND="call"
MA_SHOW_REPO=1

REPO="$PWD"
TO=""
PROMPT=""
PROMPT_FILE=""
ARTIFACT_DIR=""
INCLUDE_MEMORY=1
DRY_RUN="${OH_MY_SETTING_CALL_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: agent-call.sh --to PROVIDER (--prompt TEXT | --prompt-file PATH) [options]

Call one local agent CLI for a read-only independent pass. For write tasks, use
multi-agent-delegate.sh so edits happen in an isolated worktree.

Options:
  --to PROVIDER        codex, claude, or antigravity. Required.
  --prompt TEXT        Prompt/question to send.
  --prompt-file PATH   Prompt file to send.
  --repo PATH          Repo/directory for context and artifacts. Default: PWD.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/call.
  --no-memory          Do not attach shared harness memory.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write prompt artifact without calling the CLI.
  -h, --help           Show help.

Environment:
  OH_MY_SETTING_CALL_DRY_RUN=1    Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m      Provider wall-clock timeout (GNU timeout).
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --to)
      [ "$#" -ge 2 ] || fail "--to requires provider"
      TO="$2"
      shift 2
      ;;
    --prompt)
      [ "$#" -ge 2 ] || fail "--prompt requires text"
      PROMPT="$2"
      shift 2
      ;;
    --prompt-file)
      [ "$#" -ge 2 ] || fail "--prompt-file requires path"
      PROMPT_FILE="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || fail "--artifact-dir requires path"
      ARTIFACT_DIR="$2"
      shift 2
      ;;
    --no-memory)
      INCLUDE_MEMORY=0
      shift
      ;;
    --print-timeout)
      [ "$#" -ge 2 ] || fail "--print-timeout requires duration"
      OMS_MULTI_AGENT_PRINT_TIMEOUT="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [ -z "$PROMPT" ]; then
        PROMPT="$1"
        shift
      else
        fail "unknown argument: $1"
      fi
      ;;
  esac
done

case "$TO" in
  codex|claude|antigravity|agy) ;;
  "") fail "--to is required" ;;
  *) fail "unsupported provider: $TO" ;;
esac
[ "$TO" = "agy" ] && TO="antigravity"

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || fail "prompt file not found: $PROMPT_FILE"
elif [ -z "$PROMPT" ]; then
  fail "--prompt or --prompt-file is required"
fi

REPO="$(cd "$REPO" && pwd)"
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/call}"

load_user_tool_paths
mkdir -p "$ARTIFACT_DIR"

prompt_file="$(mktemp)" || fail "mktemp failed"
cleanup() { rm -f "$prompt_file"; }
trap cleanup EXIT

{
  printf 'You are %s, called by an agent harness for an independent read-only pass.\n' "$TO"
  printf 'Do not modify files. Do not run git commit or git push.\n'
  printf 'Use the shared memory only as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n\n'
  if [ "$INCLUDE_MEMORY" -eq 1 ]; then
    ma_write_shared_memory_context "$REPO"
  fi
  printf 'Prompt:\n'
  if [ -n "$PROMPT_FILE" ]; then
    cat "$PROMPT_FILE"
  else
    printf '%s\n' "$PROMPT"
  fi
  printf '\nReturn a concise answer with evidence, assumptions, and recommended next action.\n'
} > "$prompt_file"

slug_src="$PROMPT"
[ -n "$slug_src" ] || slug_src="$(head -c 200 "$PROMPT_FILE")"
slug="$(slugify "$slug_src")"
[ -n "$slug" ] || slug="call"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.md"

if run_provider "$TO" "$prompt_file" "$artifact"; then
  echo "artifact: $artifact"
else
  echo "artifact: $artifact"
  exit 1
fi
