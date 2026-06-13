#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-task-common.sh
. "$ROOT/scripts/lib/agent-task-common.sh"

REPO="$PWD"
TASK_FILE=""
ACTION=""
AGENT="agent"
GOAL=""
CONSTRAINT=""
DONE_CRITERIA=""
VERIFY=""
LOOP_ATTEMPTS=""
LOOP_MAX=""
DIFF_BUDGET=""
VERIFY_LEVEL=""
LAST_FAILURE=""
VERIFICATION_NOTE=""
HYPOTHESIS=""
RESULT_NOTE=""
DECISION=""
STATE=""
NEXT_STEP=""
TEXT=""
USE_STDIN=0

usage() {
  cat <<'EOF'
Usage: agent-task.sh [options] [path|init|show|context|update|append|close]

Maintain the active handoff packet shared by Codex, Claude Code, and
Antigravity. Project task defaults to REPO/.oms/task/current.md.

Options:
  --repo PATH       Project repo/directory. Default: PWD.
  --file PATH       Explicit task file path.
  --agent NAME      Agent label for appended notes. Default: agent.
  --goal TEXT       init/update: replace Goal.
  --constraint TEXT update: append a Constraints bullet.
  --done TEXT       update: append a Done Criteria bullet.
  --verify CMD      init/update: replace Verify.
  --loop-attempts N init/update: set Loop State attempts.
  --loop-max N      init/update: set Loop State max_attempts.
  --diff-budget N   init/update: set Loop State diff_budget_lines.
  --verify-level L  init/update: set Loop State verification_level.
  --last-failure T  update: append a Last Failure bullet.
  --verification T  update: append a Verification bullet.
  --hypothesis T    update: append a Current State hypothesis bullet.
  --result T        update: append a Current State result bullet.
  --decision TEXT   update: append a Decisions bullet.
  --state TEXT      init/update: replace Current State.
  --next TEXT       init/update: replace Next Step.
  --text TEXT       append: append a Current State bullet.
  --stdin           Read append text from stdin.
  -h, --help        Show help.

Commands:
  path              Print resolved task file path.
  init              Create the task file if missing and apply provided fields.
  show              Print task file if it exists. Default.
  context           Print provider context view.
  update            Apply section updates.
  append            Append --text/--stdin to Current State.
  close             Archive current.md under .oms/task/archive/ and remove it.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { echo "error: --repo requires path" >&2; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || { echo "error: --file requires path" >&2; exit 2; }
      TASK_FILE="$2"
      shift 2
      ;;
    --agent)
      [ "$#" -ge 2 ] || { echo "error: --agent requires name" >&2; exit 2; }
      AGENT="$2"
      shift 2
      ;;
    --goal)
      [ "$#" -ge 2 ] || { echo "error: --goal requires text" >&2; exit 2; }
      GOAL="$2"
      shift 2
      ;;
    --constraint)
      [ "$#" -ge 2 ] || { echo "error: --constraint requires text" >&2; exit 2; }
      CONSTRAINT="$2"
      shift 2
      ;;
    --done)
      [ "$#" -ge 2 ] || { echo "error: --done requires text" >&2; exit 2; }
      DONE_CRITERIA="$2"
      shift 2
      ;;
    --verify)
      [ "$#" -ge 2 ] || { echo "error: --verify requires command" >&2; exit 2; }
      VERIFY="$2"
      shift 2
      ;;
    --loop-attempts)
      [ "$#" -ge 2 ] || { echo "error: --loop-attempts requires number" >&2; exit 2; }
      LOOP_ATTEMPTS="$2"
      shift 2
      ;;
    --loop-max)
      [ "$#" -ge 2 ] || { echo "error: --loop-max requires number" >&2; exit 2; }
      LOOP_MAX="$2"
      shift 2
      ;;
    --diff-budget)
      [ "$#" -ge 2 ] || { echo "error: --diff-budget requires number" >&2; exit 2; }
      DIFF_BUDGET="$2"
      shift 2
      ;;
    --verify-level)
      [ "$#" -ge 2 ] || { echo "error: --verify-level requires text" >&2; exit 2; }
      VERIFY_LEVEL="$2"
      shift 2
      ;;
    --last-failure)
      [ "$#" -ge 2 ] || { echo "error: --last-failure requires text" >&2; exit 2; }
      LAST_FAILURE="$2"
      shift 2
      ;;
    --verification)
      [ "$#" -ge 2 ] || { echo "error: --verification requires text" >&2; exit 2; }
      VERIFICATION_NOTE="$2"
      shift 2
      ;;
    --hypothesis)
      [ "$#" -ge 2 ] || { echo "error: --hypothesis requires text" >&2; exit 2; }
      HYPOTHESIS="$2"
      shift 2
      ;;
    --result)
      [ "$#" -ge 2 ] || { echo "error: --result requires text" >&2; exit 2; }
      RESULT_NOTE="$2"
      shift 2
      ;;
    --decision)
      [ "$#" -ge 2 ] || { echo "error: --decision requires text" >&2; exit 2; }
      DECISION="$2"
      shift 2
      ;;
    --state)
      [ "$#" -ge 2 ] || { echo "error: --state requires text" >&2; exit 2; }
      STATE="$2"
      shift 2
      ;;
    --next)
      [ "$#" -ge 2 ] || { echo "error: --next requires text" >&2; exit 2; }
      NEXT_STEP="$2"
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
    path|init|show|context|update|append|close)
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
  append) ;;
  *)
    [ -z "$TEXT" ] || { echo "error: unknown argument: $TEXT" >&2; usage >&2; exit 2; }
    ;;
esac
[ -n "$TASK_FILE" ] || TASK_FILE="$(agent_task_project_file "$REPO")"

require_uint() {
  local name="$1"
  local value="$2"

  [ -z "$value" ] && return 0
  case "$value" in
    *[!0-9]*)
      echo "error: $name must be a non-negative integer" >&2
      exit 2
      ;;
  esac
}

require_uint "--loop-attempts" "$LOOP_ATTEMPTS"
require_uint "--loop-max" "$LOOP_MAX"
require_uint "--diff-budget" "$DIFF_BUDGET"

# Lazily created: read-only actions (path, show, context) must not depend
# on a writable TMPDIR. Library temp files land here too once created.
OMS_TASK_TMPDIR=""
cleanup_done=0
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  if [ -n "$OMS_TASK_TMPDIR" ]; then rm -rf "$OMS_TASK_TMPDIR"; fi
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
  [ -n "$OMS_TASK_TMPDIR" ] && return 0
  OMS_TASK_TMPDIR="$(mktemp -d)" || exit 1
  export OMS_LIB_TMPDIR="$OMS_TASK_TMPDIR"
}

write_tmp_text() {
  local output="$1"
  local text="$2"
  printf '%s\n' "$text" > "$output"
}

replace_if_set() {
  local section="$1"
  local value="$2"
  local note_file

  [ -n "$value" ] || return 0
  note_file="$(mktemp "$OMS_TASK_TMPDIR/note.XXXXXX")" || return 1
  write_tmp_text "$note_file" "$value"
  agent_task_replace_section "$TASK_FILE" "$section" "$note_file"
  rm -f "$note_file"
}

append_if_set() {
  local section="$1"
  local value="$2"
  local note_file

  [ -n "$value" ] || return 0
  note_file="$(mktemp "$OMS_TASK_TMPDIR/note.XXXXXX")" || return 1
  write_tmp_text "$note_file" "$value"
  agent_task_append_bullet "$TASK_FILE" "$section" "$AGENT" "$note_file"
  rm -f "$note_file"
}

apply_updates() {
  replace_if_set "## Goal" "$GOAL"
  replace_if_set "## Verify" "$VERIFY"
  if [ -n "$LOOP_ATTEMPTS" ] || [ -n "$LOOP_MAX" ] || [ -n "$DIFF_BUDGET" ] || [ -n "$VERIFY_LEVEL" ]; then
    agent_task_upsert_loop_state "$TASK_FILE" "$LOOP_ATTEMPTS" "$LOOP_MAX" "$DIFF_BUDGET" "$VERIFY_LEVEL"
  fi
  append_if_set "## Last Failure" "$LAST_FAILURE"
  append_if_set "## Verification" "$VERIFICATION_NOTE"
  replace_if_set "## Current State" "$STATE"
  append_if_set "## Current State" "${HYPOTHESIS:+Hypothesis: $HYPOTHESIS}"
  append_if_set "## Current State" "${RESULT_NOTE:+Result: $RESULT_NOTE}"
  replace_if_set "## Next Step" "$NEXT_STEP"
  append_if_set "## Constraints" "$CONSTRAINT"
  append_if_set "## Done Criteria" "$DONE_CRITERIA"
  append_if_set "## Decisions" "$DECISION"
}

append_text() {
  local note_file

  note_file="$(mktemp "$OMS_TASK_TMPDIR/note.XXXXXX")" || return 1
  if [ "$USE_STDIN" -eq 1 ]; then
    cat > "$note_file"
  else
    [ -n "$TEXT" ] || { echo "error: append requires --text or --stdin" >&2; exit 2; }
    write_tmp_text "$note_file" "$TEXT"
  fi
  agent_task_append_bullet "$TASK_FILE" "## Current State" "$AGENT" "$note_file"
  rm -f "$note_file"
}

case "$ACTION" in
  path)
    printf '%s\n' "$TASK_FILE"
    ;;
  init)
    ensure_tmpdir
    agent_task_init_file "$TASK_FILE"
    apply_updates
    echo "task: initialized $TASK_FILE"
    ;;
  show)
    if [ -s "$TASK_FILE" ]; then
      cat "$TASK_FILE"
    else
      echo "task: empty ($TASK_FILE)"
    fi
    ;;
  context)
    agent_task_emit_context "$REPO" "$TASK_FILE" || true
    ;;
  update)
    ensure_tmpdir
    agent_task_init_file "$TASK_FILE"
    apply_updates
    echo "task: updated $TASK_FILE"
    ;;
  append)
    ensure_tmpdir
    agent_task_init_file "$TASK_FILE"
    append_text
    echo "task: appended $TASK_FILE"
    ;;
  close)
    if [ ! -e "$TASK_FILE" ]; then
      echo "task: no active task ($TASK_FILE)"
      exit 0
    fi
    ensure_tmpdir
    # Promote a one-line outcome into project shared memory so the next
    # session (any agent) starts from the conclusion, not from scratch.
    if [ "${OMS_AGENT_TASK_CLOSE_MEMORY:-1}" = "1" ]; then
      goal_line="$(awk '/^## Goal$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$TASK_FILE")"
      next_line="$(awk '/^## Next Step$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$TASK_FILE")"
      if [ -n "$goal_line" ]; then
        note_file="$(mktemp "$OMS_TASK_TMPDIR/note.XXXXXX")"
        printf 'Closed task: %s%s\n' "$goal_line" "${next_line:+ | next: $next_line}" > "$note_file"
        if agent_memory_append_file "$(agent_memory_project_file "$REPO")" project "$AGENT" "$note_file" >/dev/null 2>&1; then
          echo "task: outcome noted in project shared memory"
        else
          echo "warning: task outcome not added to shared memory" >&2
        fi
        rm -f "$note_file"
      fi
    fi
    archive_dir="$(dirname "$TASK_FILE")/archive"
    archive_file="$archive_dir/current-$(date -u +%Y%m%dT%H%M%SZ).md"
    mkdir -p "$archive_dir"
    mv "$TASK_FILE" "$archive_file"
    echo "task: archived $archive_file"
    ;;
  *)
    echo "error: unknown command: $ACTION" >&2
    usage >&2
    exit 2
    ;;
esac
