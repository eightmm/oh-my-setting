#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

REPO="$PWD"
TO=""
PROMPT=""
PROMPT_FILE=""
MODE="auto"
ARTIFACT_DIR=""
VERIFY_CMD=""
NO_VERIFY=0
APPLY=0
KEEP_WORKTREE=0
INCLUDE_MEMORY=1
INCLUDE_TASK=1
INCLUDE_ML_CONTEXT=1
DRY_RUN="${OH_MY_SETTING_AGENT_RUN_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: agent-run.sh --to PROVIDER (--prompt TEXT | --prompt-file PATH) [options]

Single-agent harness entrypoint. It routes read-only questions to agent-call.sh
and write tasks to multi-agent-delegate.sh. In --mode auto, routing is
conservative and based on task wording; the owning agent may pass --mode read or
--mode write when intent is already clear.

Options:
  --to PROVIDER        codex, claude, or antigravity. Required.
  --prompt TEXT        Prompt/task to send.
  --prompt-file PATH   Prompt/task file to send.
  --repo PATH          Repo/directory for context and artifacts. Default: PWD.
  --mode MODE          auto, read, or write. Default: auto.
  --artifact-dir PATH  Override artifact directory.
  --verify CMD         Write mode only: verification command in worker worktree.
  --no-verify          Write mode only: skip default scripts/check.sh verification.
  --apply              Write mode only: apply returned patch when worker and
                       verify pass and the main tree is clean.
  --keep-worktree      Write mode only: keep worker worktree.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --no-ml-context      Do not attach the compact ML context digest.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write artifacts without calling provider CLIs.
  -h, --help           Show help.

Environment:
  OH_MY_SETTING_AGENT_RUN_DRY_RUN=1  Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m         Provider wall-clock timeout (GNU timeout).
EOF
}

classify_mode() {
  local text="$1"
  local lower
  lower="$(printf '%s' "$text" | tr '[:upper:]' '[:lower:]')"

  if printf '%s' "$lower" | grep -Eq '(add|implement|fix|change|modify|update|refactor|remove|delete|create|generate|write|apply|migrate|rename|scaffold|build|install|구현|수정|추가|변경|삭제|제거|고쳐|만들|작성|적용|리팩터|정리)'; then
    printf 'write\n'
  else
    printf 'read\n'
  fi
}

prompt_text() {
  if [ -n "$PROMPT" ]; then
    printf '%s' "$PROMPT"
  else
    head -c 4000 "$PROMPT_FILE"
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --to)
      [ "$#" -ge 2 ] || { echo "error: --to requires provider" >&2; exit 2; }
      TO="$2"
      shift 2
      ;;
    --prompt)
      [ "$#" -ge 2 ] || { echo "error: --prompt requires text" >&2; exit 2; }
      PROMPT="$2"
      shift 2
      ;;
    --prompt-file)
      [ "$#" -ge 2 ] || { echo "error: --prompt-file requires path" >&2; exit 2; }
      PROMPT_FILE="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || { echo "error: --repo requires path" >&2; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --mode)
      [ "$#" -ge 2 ] || { echo "error: --mode requires auto|read|write" >&2; exit 2; }
      MODE="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || { echo "error: --artifact-dir requires path" >&2; exit 2; }
      ARTIFACT_DIR="$2"
      shift 2
      ;;
    --verify)
      [ "$#" -ge 2 ] || { echo "error: --verify requires command" >&2; exit 2; }
      VERIFY_CMD="$2"
      shift 2
      ;;
    --no-verify)
      NO_VERIFY=1
      shift
      ;;
    --apply)
      APPLY=1
      shift
      ;;
    --keep-worktree)
      KEEP_WORKTREE=1
      shift
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
    --print-timeout)
      [ "$#" -ge 2 ] || { echo "error: --print-timeout requires duration" >&2; exit 2; }
      OMS_MULTI_AGENT_PRINT_TIMEOUT="$2"
      export OMS_MULTI_AGENT_PRINT_TIMEOUT
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
        echo "error: unknown argument: $1" >&2
        exit 2
      fi
      ;;
  esac
done

case "$TO" in
  codex|claude|antigravity|agy) ;;
  "") echo "error: --to is required" >&2; exit 2 ;;
  *) echo "error: unsupported provider: $TO" >&2; exit 2 ;;
esac

case "$MODE" in
  auto|read|write) ;;
  *) echo "error: --mode must be auto, read, or write" >&2; exit 2 ;;
esac

if [ -n "$PROMPT_FILE" ]; then
  [ -f "$PROMPT_FILE" ] || { echo "error: prompt file not found: $PROMPT_FILE" >&2; exit 2; }
elif [ -z "$PROMPT" ]; then
  echo "error: --prompt or --prompt-file is required" >&2
  exit 2
fi

resolved_mode="$MODE"
if [ "$MODE" = "auto" ]; then
  resolved_mode="$(classify_mode "$(prompt_text)")"
fi

echo "agent-run: mode=$MODE resolved=$resolved_mode to=$TO" >&2

if [ "$resolved_mode" = "read" ]; then
  cmd=("$ROOT/scripts/agent-call.sh" --to "$TO" --repo "$REPO")
  if [ -n "$PROMPT_FILE" ]; then
    cmd+=(--prompt-file "$PROMPT_FILE")
  else
    cmd+=(--prompt "$PROMPT")
  fi
  [ -n "$ARTIFACT_DIR" ] && cmd+=(--artifact-dir "$ARTIFACT_DIR")
  [ "$INCLUDE_MEMORY" -eq 0 ] && cmd+=(--no-memory)
  [ "$INCLUDE_TASK" -eq 0 ] && cmd+=(--no-task)
  [ "$INCLUDE_ML_CONTEXT" -eq 0 ] && cmd+=(--no-ml-context)
  [ "$DRY_RUN" = "1" ] && cmd+=(--dry-run)
  "${cmd[@]}"
else
  cmd=("$ROOT/scripts/multi-agent-delegate.sh" --to "$TO" --repo "$REPO")
  if [ -n "$PROMPT_FILE" ]; then
    cmd+=(--brief-file "$PROMPT_FILE")
  else
    cmd+=(--prompt "$PROMPT")
  fi
  [ -n "$ARTIFACT_DIR" ] && cmd+=(--artifact-dir "$ARTIFACT_DIR")
  [ -n "$VERIFY_CMD" ] && cmd+=(--verify "$VERIFY_CMD")
  [ "$NO_VERIFY" -eq 1 ] && cmd+=(--no-verify)
  [ "$APPLY" -eq 1 ] && cmd+=(--apply)
  [ "$KEEP_WORKTREE" -eq 1 ] && cmd+=(--keep-worktree)
  [ "$INCLUDE_MEMORY" -eq 0 ] && cmd+=(--no-memory)
  [ "$INCLUDE_TASK" -eq 0 ] && cmd+=(--no-task)
  [ "$INCLUDE_ML_CONTEXT" -eq 0 ] && cmd+=(--no-ml-context)
  [ "$DRY_RUN" = "1" ] && cmd+=(--dry-run)
  "${cmd[@]}"
fi
