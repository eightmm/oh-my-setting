#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/peer-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/peer-common.sh"

MA_KIND="call"
MA_SHOW_REPO=1

REPO="$PWD"
TO=""
PROMPT=""
PROMPT_FILE=""
ARTIFACT_DIR=""
INCLUDE_MEMORY=1
INCLUDE_TASK=1
INCLUDE_ML_CONTEXT=1
EXPORT_ONLY=0
DRY_RUN="${OH_MY_SETTING_CALL_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: agent-call.sh --to PROVIDER (--prompt TEXT | --prompt-file PATH) [options]

Call one local agent CLI for a read-only independent pass. For write tasks, use
peer-delegate.sh so edits happen in an isolated worktree.

Options:
  --to PROVIDER        codex, claude, or antigravity. Required.
  --prompt TEXT        Prompt/question to send.
  --prompt-file PATH   Prompt file to send.
  --repo PATH          Repo/directory for context and artifacts. Default: PWD.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/call.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --no-ml-context      Do not attach the compact ML context digest.
  --export-only        Write the provider prompt artifact and do not call CLI.
                       Import the answer later with import-agent-result.sh.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write prompt artifact without calling the CLI.
  -h, --help           Show help.

Environment:
  OH_MY_SETTING_CALL_DRY_RUN=1    Same as --dry-run.
  OMS_PEER_TIMEOUT=5m      Provider wall-clock timeout (GNU timeout).
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
    --no-task)
      INCLUDE_TASK=0
      shift
      ;;
    --no-ml-context)
      INCLUDE_ML_CONTEXT=0
      shift
      ;;
    --export-only)
      EXPORT_ONLY=1
      shift
      ;;
    --print-timeout)
      [ "$#" -ge 2 ] || fail "--print-timeout requires duration"
      OMS_PEER_PRINT_TIMEOUT="$2"
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
agent_memory_ensure_oms_ignore_for_path "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

# All temp files (the prompt and any library temps from ma_write_harness_context)
# land in one trapped dir, so a TERM mid-run cannot leak a stray temp. Removing
# the dir as a whole closes the race the previous rm -f "$prompt_file" left open
# (helper temps created by agent_memory_mktemp went to bare TMPDIR and survived).
call_tmpdir="$(mktemp -d)" || fail "mktemp failed"
export OMS_LIB_TMPDIR="$call_tmpdir"
prompt_file="$(mktemp "$call_tmpdir/prompt.XXXXXX")" || fail "mktemp failed"
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -rf "$call_tmpdir"
}
cleanup_signal() {
  local code="$1"
  trap - EXIT HUP INT TERM
  ma_kill_jobs
  cleanup
  exit "$code"
}
trap cleanup EXIT
trap 'cleanup_signal 129' HUP
trap 'cleanup_signal 130' INT
trap 'cleanup_signal 143' TERM

{
  printf 'You are %s, called by an agent harness for an independent read-only pass.\n' "$TO"
  printf 'Do not modify files. Do not run git commit or git push.\n'
  printf 'Use the shared memory only as soft recall; explicit prompt, AGENTS.md, and repo docs override it.\n\n'
  ma_write_harness_context "$REPO" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT"
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
export OMS_OPERATION_ID="${OMS_OPERATION_ID:-call-$timestamp}"
artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.md"

if [ "$EXPORT_ONLY" -eq 1 ]; then
  if ! ma_validate_outbound_prompt "$prompt_file"; then
    echo "export blocked: no export artifacts were written" >&2
    exit 3
  fi

  artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.export.md"
  {
    printf '# %s %s export\n\n' "$TO" "$MA_KIND"
    printf -- '- exported: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if [ "${MA_SHOW_REPO:-0}" = "1" ]; then
      printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
    fi
    printf '\n## Prompt\n\n'
    cat "$prompt_file"
    printf '\n\n## Output\n\n'
    printf 'EXPORTED: paste the Prompt section into %s, then import the answer with import-agent-result.sh.\n' "$TO"
    printf '\n\n## Exit\n\n0\n'
  } > "$artifact"
  ma_append_artifact_index "$REPO" "call-export" "$TO" 0 "$artifact" "" "$prompt_file" || true
  echo "exported: $TO -> $artifact"
  exit 0
fi

if run_provider "$TO" "$prompt_file" "$artifact"; then
  echo "artifact: $artifact"
else
  rc=$?
  echo "artifact: $artifact"
  # Propagate run_provider's code: 3 = blocked by scrubber, 1 = provider failed.
  exit "$rc"
fi
