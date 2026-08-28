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
INCLUDE_MEMORY=0
INCLUDE_TASK=0
INCLUDE_ML_CONTEXT=0
THREAD_ID=""
OPERATION=""
EXPORT_ONLY=0
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
DRY_RUN="${OH_MY_SETTING_CALL_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: agent-call.sh --to PROVIDER (--prompt TEXT | --prompt-file PATH) [options]

Call one local agent CLI for a read-only independent pass. For write tasks, use
peer-delegate.sh so edits happen in an isolated worktree.

Options:
  --to PROVIDER        Registered agent transport; inspect `oms models`.
                       Required.
  --prompt TEXT        Prompt/question to send.
  --prompt-file PATH   Prompt file to send.
  --repo PATH          Repo/directory for context and artifacts. Default: PWD.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/call.
  --model MODEL        Exact provider model; disables implicit fallback.
  --fallback-model M   Explicit one-shot capacity fallback model.
  --reasoning-effort E auto, low, medium, high, xhigh, max, or ultra (default: auto).
  --memory             Attach shared harness memory.
  --task               Attach the active task handoff packet.
  --ml-context         Attach the compact ML context digest.
  --thread ID          Join a cross-agent thread: prior turns are injected and
                       this exchange is appended (thread.sh).
  --operation NAME     Record the work phase in artifacts; it does not select a model.
  --no-memory          Disable --memory (compatibility).
  --no-task            Disable --task (compatibility).
  --no-ml-context      Disable --ml-context (compatibility).
  --export-only        Write the provider prompt artifact and do not call CLI.
                       Import the answer later with `oms artifact-index import`.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write prompt artifact without calling the CLI.
  -h, --help           Show help.

Environment:
  OH_MY_SETTING_CALL_DRY_RUN=1    Same as --dry-run.
  OMS_PEER_TIMEOUT         Provider wall-clock timeout (default: 20m).
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
    --model)
      [ "$#" -ge 2 ] || fail "--model requires value"
      MODEL="$2"
      shift 2
      ;;
    --fallback-model)
      [ "$#" -ge 2 ] || fail "--fallback-model requires value"
      FALLBACK_MODEL="$2"
      shift 2
      ;;
    --reasoning-effort)
      [ "$#" -ge 2 ] || fail "--reasoning-effort requires value"
      REASONING_EFFORT="$2"
      shift 2
      ;;
    --no-memory)
      INCLUDE_MEMORY=0
      shift
      ;;
    --memory)
      INCLUDE_MEMORY=1
      shift
      ;;
    --no-task)
      INCLUDE_TASK=0
      shift
      ;;
    --task)
      INCLUDE_TASK=1
      shift
      ;;
    --no-ml-context)
      INCLUDE_ML_CONTEXT=0
      shift
      ;;
    --ml-context)
      INCLUDE_ML_CONTEXT=1
      shift
      ;;
    --thread)
      [ "$#" -ge 2 ] || fail "--thread requires an id"
      THREAD_ID="$2"
      shift 2
      ;;
    --operation)
      [ "$#" -ge 2 ] || fail "--operation requires a name"
      OPERATION="$2"
      shift 2
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
      # A leading dash is a mistyped option, never an intended prompt: the
      # old fall-through silently adopted it as the question (or discarded
      # it once --prompt arrived), and a typo'd flag changed nothing.
      case "$1" in
        -*) fail "unknown option: $1" ;;
      esac
      if [ -z "$PROMPT" ]; then
        PROMPT="$1"
        shift
      else
        fail "unknown argument: $1"
      fi
      ;;
  esac
done

[ -n "$TO" ] || fail "--to is required"
TO="$(oms_provider_normalize "$TO")" || exit $?
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
export OMS_MODEL_EXPLICIT="$MODEL"
export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL"
export OMS_REASONING_EFFORT_REQUEST="$REASONING_EFFORT"
# Operation is retained as artifact context only.
export OMS_MODEL_OPERATION="${OPERATION:-call}"
[ -z "$OPERATION" ] || export OMS_MODEL_OPERATION_REQUEST="$OPERATION"

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || fail "prompt file not found: $PROMPT_FILE"
elif [ -z "$PROMPT" ]; then
  fail "--prompt or --prompt-file is required"
fi

oms_require_peer_owner || exit $?

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
  # Human-read answers may carry the operator's language policy; machine
  # contracts (--operation plan: planner JSON, intent skeletons) never do.
  [ "$OPERATION" = plan ] || ma_answer_language_block
  ma_write_harness_context "$REPO" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT" \
    "$(if [ -n "$PROMPT_FILE" ]; then head -c 300 "$PROMPT_FILE" 2>/dev/null; else printf '%s' "$PROMPT"; fi)"
  ma_write_thread_context "$REPO" "$THREAD_ID"
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
  oms_model_prepare "$TO" || exit $?
  if ! ma_validate_outbound_prompt "$prompt_file"; then
    echo "export blocked: no export artifacts were written" >&2
    exit 3
  fi

  artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.export.md"
  {
    printf '# %s %s export\n\n' "$TO" "$MA_KIND"
    printf -- '- exported: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf -- '- model-class: %s\n' "$OMS_MODEL_RESOLVED_CLASS"
    printf -- '- selected-model: %s\n' "$OMS_MODEL_PRIMARY"
    [ -z "$OMS_REASONING_RESOLVED" ] || printf -- '- reasoning-effort: %s\n' "$OMS_REASONING_RESOLVED"
    if [ "${MA_SHOW_REPO:-0}" = "1" ]; then
      printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
    fi
    printf '\n## Prompt\n\n'
    cat "$prompt_file"
    printf '\n\n## Output\n\n'
    printf 'EXPORTED: paste the Prompt section into %s, then import with `oms artifact-index import`.\n' "$TO"
    printf 'Preserve the selected model route recorded above during the manual call.\n'
    printf '\n\n## Exit\n\n0\n'
  } > "$artifact"
  ma_append_artifact_index "$REPO" "call-export" "$TO" 0 "$artifact" "" "$prompt_file" || true
  echo "exported: $TO -> $artifact"
  exit 0
fi

# The question is recorded from the operator prompt, not the composed file, so
# a thread stays a conversation instead of a pile of harness boilerplate.
# With FAILED_EXIT, the call died and the turn is one typed line naming it: a
# thread that keeps only the calls that worked is not the conversation it claims
# to be, and the next agent cannot see what it never got an answer to.
record_thread_exchange() {  # record_thread_exchange [FAILED_EXIT]
  local status="${1:-}"
  local question
  [ -n "$THREAD_ID" ] || return 0
  question="$(agent_memory_mktemp)" || return 0
  if [ -n "$PROMPT_FILE" ]; then
    cat "$PROMPT_FILE" > "$question"
  else
    printf '%s\n' "$PROMPT" > "$question"
  fi
  if [ -n "$status" ]; then
    [ "${OMS_THREAD_QUESTION_RECORDED:-0}" = "1" ] ||
      ma_thread_append "$REPO" "$THREAD_ID" question "$question"
    ma_thread_append_nonanswer "$REPO" "$THREAD_ID" "$TO" "exit $status" "$artifact"
  else
    ma_thread_record_exchange "$REPO" "$THREAD_ID" "$TO" "$OMS_MODEL_SELECTED" \
      "$artifact" "$question"
  fi
  rm -f "$question"
}

if run_provider "$TO" "$prompt_file" "$artifact"; then
  record_thread_exchange
  echo "artifact: $artifact"
else
  rc=$?
  record_thread_exchange "$rc"
  ma_record_seat_failure "$TO" "$rc"
  echo "artifact: $artifact"
  # Propagate run_provider's code: 3 = blocked by scrubber, 1 = provider failed.
  exit "$rc"
fi
