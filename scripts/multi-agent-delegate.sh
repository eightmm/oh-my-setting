#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/multi-agent-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/multi-agent-common.sh"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/harness-residue.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/harness-residue.sh"

MA_KIND="delegate"

REPO="$PWD"
TO=""
PROMPT=""
BRIEF_FILE=""
VERIFY_CMD=""
NO_VERIFY=0
ARTIFACT_DIR=""
APPLY=0
KEEP_WORKTREE=0
INCLUDE_MEMORY=1
INCLUDE_TASK=1
INCLUDE_ML_CONTEXT=1
TASK_ID=""
PLAN_TASK_ID=""
REPAIR=0
DRY_RUN="${OH_MY_SETTING_DELEGATE_DRY_RUN:-0}"

usage() {
  cat <<'EOF'
Usage: multi-agent-delegate.sh --to PROVIDER (--prompt TEXT | --brief-file PATH) [options]

Delegate a write task to another agent CLI. The worker runs non-interactively
inside an isolated git worktree; the result comes back as a patch artifact that
the caller reviews before applying. The worker never touches the main tree and
never commits or pushes.

Options:
  --to PROVIDER        Worker: codex, claude, or antigravity. Required.
  --prompt TEXT        Short task brief.
  --brief-file PATH    File with a structured brief (Task/Context/Constraints/
                       Files/Success criteria). Preferred for non-trivial tasks.
  --repo PATH          Git repo to work on. Default: current directory.
  --verify CMD         Command run inside the worktree after the worker
                       finishes (e.g. "uv run pytest tests/"). Non-zero exit
                       marks the delegation failed. Default: when the project
                       has executable scripts/check.sh, ML projects prefer
                       "bash scripts/check.sh ml-smoke" when available, else
                       "bash scripts/check.sh fast".
  --no-verify          Skip the default scripts/check.sh verification.
  --repair N           On worker or verify failure, re-run the same worker in
                       the same worktree up to N times (1-3), feeding back the
                       original brief, its rejected patch, and the failing
                       verify output tail. Missing-CLI and outbound-gate
                       failures are never retried. Each round adds another
                       OMS_MULTI_AGENT_TIMEOUT of wall-clock budget.
                       Default: 0 (one-shot).
  --apply              Apply the resulting patch to the main tree when the
                       worker and --verify succeed. Requires a clean main tree.
  --keep-worktree      Keep the worktree for manual inspection.
  --no-memory          Do not attach shared harness memory.
  --no-task            Do not attach the active task handoff packet.
  --no-ml-context      Do not attach the compact ML context digest.
  --task-id ID         Plan/task id (agent-plan.sh) to stamp on this run's
                       artifact-index rows for lineage. [A-Za-z0-9._-]+.
  --plan-task ID       Couple this delegation to an agent-plan.sh task: on
                       worker/verify failure (or an outbound-gate block) the
                       claim is released back to ready; on success the task
                       moves to review with the artifact and patch (or to done
                       when --apply landed the patch). Implies --task-id ID.
  --artifact-dir PATH  Artifact directory. Default: REPO/.oms/artifacts/delegate.
  --print-timeout DUR  Timeout for print mode wait (agy). Default: 5m.
  --dry-run            Write prompt and empty patch without calling the CLI.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_DELEGATE_DRY_RUN=1  Same as --dry-run.
  OMS_MULTI_AGENT_TIMEOUT=5m        Worker wall-clock timeout (GNU timeout).
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
    --brief-file)
      [ "$#" -ge 2 ] || fail "--brief-file requires path"
      BRIEF_FILE="$2"
      shift 2
      ;;
    --repo)
      [ "$#" -ge 2 ] || fail "--repo requires path"
      REPO="$2"
      shift 2
      ;;
    --verify)
      [ "$#" -ge 2 ] || fail "--verify requires command"
      VERIFY_CMD="$2"
      shift 2
      ;;
    --no-verify)
      NO_VERIFY=1
      shift
      ;;
    --repair)
      [ "$#" -ge 2 ] || fail "--repair requires round count"
      case "$2" in
        0|1|2|3) REPAIR="$2" ;;
        *) fail "--repair must be 0-3" ;;
      esac
      shift 2
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
    --task-id)
      [ "$#" -ge 2 ] || fail "--task-id requires id"
      case "$2" in
        *[!A-Za-z0-9._-]*|"") fail "--task-id must match [A-Za-z0-9._-]+" ;;
      esac
      TASK_ID="$2"
      shift 2
      ;;
    --plan-task)
      [ "$#" -ge 2 ] || fail "--plan-task requires id"
      case "$2" in
        *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;;
      esac
      PLAN_TASK_ID="$2"
      shift 2
      ;;
    --artifact-dir)
      [ "$#" -ge 2 ] || fail "--artifact-dir requires path"
      ARTIFACT_DIR="$2"
      shift 2
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
      fail "unknown argument: $1"
      ;;
  esac
done

# Stamp every artifact-index row from this delegation with the plan/task id so
# the run can be traced back to its subtask (ma_append_artifact_index reads it).
[ -n "$PLAN_TASK_ID" ] && [ -z "$TASK_ID" ] && TASK_ID="$PLAN_TASK_ID"
[ -n "$TASK_ID" ] && export OMS_TASK_ID="$TASK_ID"

# Plan lifecycle coupling for --plan-task. Failures release the claim so the
# task never sticks in claimed/running when the worker dies or is rejected.
plan_transition() {
  local action="$1"
  shift
  [ -n "$PLAN_TASK_ID" ] || return 0
  [ "$DRY_RUN" != "1" ] || return 0
  "$(ma_scripts_dir)/agent-plan.sh" --repo "$REPO" "$action" --id "$PLAN_TASK_ID" "$@" ||
    echo "warning: plan $action failed for task $PLAN_TASK_ID" >&2
}

case "$TO" in
  codex|claude|antigravity|agy) ;;
  "") fail "--to is required" ;;
  *) fail "unsupported provider: $TO" ;;
esac
[ "$TO" = "agy" ] && TO="antigravity"

if [ -n "$BRIEF_FILE" ]; then
  [ -f "$BRIEF_FILE" ] || fail "brief file not found: $BRIEF_FILE"
elif [ -z "$PROMPT" ]; then
  fail "--prompt or --brief-file is required"
fi

REPO="$(cd "$REPO" && pwd)"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
git -C "$REPO" rev-parse --verify HEAD >/dev/null 2>&1 ||
  fail "repo needs at least one commit to delegate against"
ARTIFACT_DIR="${ARTIFACT_DIR:-$REPO/.oms/artifacts/delegate}"

load_user_tool_paths
agent_memory_ensure_oms_ignore_for_path "$ARTIFACT_DIR"
mkdir -p "$ARTIFACT_DIR"

oms_harness_prune_stale_worktrees "$REPO" 0 >/dev/null
prompt_file="$(mktemp)" || fail "mktemp failed"
repair_prompt_file="$prompt_file.repair"
verify_out="$prompt_file.verify"
worktree_parent="$(mktemp -d "${TMPDIR:-/tmp}/oh-my-setting-delegate.XXXXXX")" || fail "mktemp failed"
worktree="$worktree_parent/wt"
worktree_created=0
cleanup_done=0
oms_harness_mark_tmpdir "$worktree_parent" "$REPO" "$worktree"
cleanup() {
  [ "$cleanup_done" = 0 ] || return 0
  cleanup_done=1
  rm -f "$prompt_file" "$repair_prompt_file" "$verify_out"
  if [ "$worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  if [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$worktree_parent"
  fi
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
  printf 'You are %s, a delegated worker agent.\n' "$TO"
  printf 'Work only inside the current directory; it is an isolated git worktree of the repository.\n'
  printf 'Follow AGENTS.md, CLAUDE.md, and PROJECT.md in this directory if present.\n'
  printf 'Do not ask questions. If the task is ambiguous or blocked, stop and report the blocker explicitly.\n'
  printf 'Do not run git commit, git push, or change git config.\n'
  printf 'Do not add dependencies or change the toolchain unless the brief explicitly allows it.\n\n'
  ma_write_harness_context "$REPO" "$INCLUDE_MEMORY" "$INCLUDE_TASK" "$INCLUDE_ML_CONTEXT"
  printf '## Brief\n\n'
  if [ -n "$BRIEF_FILE" ]; then
    cat "$BRIEF_FILE"
  else
    printf '%s\n' "$PROMPT"
  fi
  printf '\nWhen done, report: changed files, what you verified, what you did not verify, and any blockers.\n'
} > "$prompt_file"

# Pre-flight: fail before any worker runs, so no work is wasted. Untracked
# files are fine: git apply fails on collision anyway, and the artifact dir
# itself lives untracked inside the repo.
if [ "$APPLY" = 1 ] && [ "$DRY_RUN" = 0 ] \
  && [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
  fail "refusing --apply: main tree has uncommitted changes"
fi

slug_src="$PROMPT"
[ -n "$slug_src" ] || slug_src="$(head -c 200 "$BRIEF_FILE")"
slug="$(slugify "$slug_src")"
[ -n "$slug" ] || slug="delegate"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.md"
patch_file="$ARTIFACT_DIR/$TO-$slug-$timestamp.patch"

if ! ma_validate_outbound_prompt "$prompt_file"; then
  {
    printf '# %s delegate\n\n' "$TO"
    printf -- '- started: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '## Output\n\n'
    printf 'SKIPPED: outbound provider context contains sensitive-looking content.\n'
    printf 'No prompt content was written to this artifact and no worktree was created.\n'
    printf '\n\n## Exit\n\n3\n'
  } > "$artifact"
  : > "$patch_file"
  ma_append_artifact_index "$REPO" delegate "$TO" 3 "$artifact" "$patch_file" "$prompt_file" || true
  echo "blocked: $TO sensitive outbound context -> $artifact"
  echo "artifact: $artifact"
  echo "patch: $patch_file"
  plan_transition release
  exit 3
fi
git -C "$REPO" worktree add --detach "$worktree" HEAD >/dev/null 2>&1
worktree_created=1

# Verification contract: default to the project's check.sh when present.
if [ -z "$VERIFY_CMD" ] && [ "$NO_VERIFY" = 0 ] && [ -x "$worktree/scripts/check.sh" ]; then
  project_style="$("$(ma_scripts_dir)/detect-project-style.sh" "$worktree" 2>/dev/null || echo general)"
  if [ "$project_style" = "ml" ] && oms_check_sh_has_ml_smoke "$worktree/scripts/check.sh"; then
    VERIFY_CMD="bash scripts/check.sh ml-smoke"
  else
    VERIFY_CMD="bash scripts/check.sh fast"
  fi
  echo "auto-verify: $VERIFY_CMD (disable with --no-verify)"
fi

{
  printf '# %s delegate\n\n' "$TO"
  printf -- '- started: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- repo: %s\n' "$(ma_repo_label "$REPO")"
  if [ "$KEEP_WORKTREE" = 1 ]; then
    printf -- '- worktree: %s\n\n' "$worktree"
  else
    printf -- '- worktree: temporary (removed after run)\n\n'
  fi
  printf '## Prompt\n\n'
  cat "$prompt_file"
  printf '\n\n## Output\n\n'
} > "$artifact"

# Runs the worker CLI in the worktree on a prompt file; output is appended to
# the artifact. Sets worker_status. OMS_STATE_REPO points harness state tools
# (agent-memory/task/plan) at the primary repo's .oms — the throwaway worktree
# has none — and OMS_AGENT attributes any worker-written notes to the provider.
run_worker() {
  local prompt="$1"
  local worker_pid

  worker_status=0
  set +e
  case "$TO" in
    codex)
      (cd "$worktree" && OMS_STATE_REPO="$REPO" OMS_AGENT="$TO" run_with_timeout codex exec --sandbox workspace-write - < "$prompt") >> "$artifact" 2>&1 &
      worker_pid="$!"
      wait "$worker_pid"
      worker_status=$?
      ;;
    claude)
      (cd "$worktree" && OMS_STATE_REPO="$REPO" OMS_AGENT="$TO" run_with_timeout claude -p --permission-mode acceptEdits < "$prompt") >> "$artifact" 2>&1 &
      worker_pid="$!"
      wait "$worker_pid"
      worker_status=$?
      ;;
    antigravity)
      (cd "$worktree" && OMS_STATE_REPO="$REPO" OMS_AGENT="$TO" run_with_timeout agy --print --sandbox --print-timeout "${OMS_MULTI_AGENT_PRINT_TIMEOUT:-5m}" < "$prompt") >> "$artifact" 2>&1 &
      worker_pid="$!"
      wait "$worker_pid"
      worker_status=$?
      ;;
  esac
  set -e
}

# Capture the patch before running --verify so verification byproducts
# (caches, build output) do not leak into the patch.
capture_patch() {
  git -C "$worktree" add -A
  git -C "$worktree" diff --cached --binary > "$patch_file"
}

# Runs VERIFY_CMD in the worktree; output goes to the artifact and is kept in
# $verify_out for repair prompts. Sets verify_status.
run_verify() {
  local verify_pid

  : > "$verify_out"
  set +e
  (cd "$worktree" && run_verify_with_timeout bash -c "$VERIFY_CMD") > "$verify_out" 2>&1 &
  verify_pid="$!"
  wait "$verify_pid"
  verify_status=$?
  set -e
  cat "$verify_out" >> "$artifact"
  printf '\n- verify exit: %s\n' "$verify_status" >> "$artifact"
}

# Repair brief: the original task plus the failure evidence, addressed to the
# same worker continuing in the same worktree.
write_repair_prompt() {
  local output="$1"

  {
    printf 'You are %s, a delegated worker agent, continuing your own previous attempt.\n' "$TO"
    printf 'Work only inside the current directory; it is the same isolated git worktree and it still contains your changes.\n'
    printf 'Your previous attempt did not pass. Fix it in place; do not start over unless necessary.\n'
    printf 'Do not ask questions. If the task is ambiguous or blocked, stop and report the blocker explicitly.\n'
    printf 'Do not run git commit, git push, or change git config.\n'
    printf 'Do not add dependencies or change the toolchain unless the brief explicitly allows it.\n\n'
    printf '## Original Brief\n\n'
    if [ -n "$BRIEF_FILE" ]; then
      cat "$BRIEF_FILE"
    else
      printf '%s\n' "$PROMPT"
    fi
    if [ "$worker_status" -ne 0 ]; then
      printf '\n## Previous Worker Exit\n\n- exit %s (interrupted or timed out; finish the remaining work)\n' "$worker_status"
    fi
    printf '\n## Your Previous Patch (captured, not accepted yet)\n\n'
    head -c 20000 "$patch_file"
    if [ -n "$VERIFY_CMD" ] && [ "$verify_status" -ne 0 ]; then
      printf '\n## Failing Verification\n\n- command: %s\n- exit: %s\n\nOutput tail:\n' "$VERIFY_CMD" "$verify_status"
      tail -c 4000 "$verify_out"
      printf '\n'
    fi
    printf '\nWhen done, report: changed files, what you verified, what you did not verify, and any blockers.\n'
  } > "$output"
}

worker_status=0
if [ "$DRY_RUN" = "1" ]; then
  printf 'DRY RUN: worker command skipped.\n' >> "$artifact"
  echo "dry-run: $TO -> $artifact"
else
  binary="$TO"
  [ "$TO" = "antigravity" ] && binary="agy"
  if ! command -v "$binary" >/dev/null 2>&1; then
    printf 'SKIPPED: command not found: %s\n' "$binary" >> "$artifact"
    worker_status=127
  else
    run_worker "$prompt_file"
  fi
fi

capture_patch

verify_status=0
if [ -n "$VERIFY_CMD" ]; then
  printf '\n\n## Verify\n\n- command: %s\n\n' "$VERIFY_CMD" >> "$artifact"
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY RUN: verify skipped.\n' >> "$artifact"
  else
    run_verify
  fi
fi

# Bounded repair: on worker/verify failure, re-invoke the same worker in the
# same worktree with the failure fed back so it can correct its own attempt.
# A missing CLI (127) is not repairable, and a repair prompt that trips the
# outbound gate stops the loop — re-sending secret-laden context is futile.
repair_used=0
if [ "$REPAIR" -gt 0 ] && [ "$DRY_RUN" != "1" ] && [ "$worker_status" -ne 127 ]; then
  while [ "$repair_used" -lt "$REPAIR" ] && { [ "$worker_status" -ne 0 ] || [ "$verify_status" -ne 0 ]; }; do
    repair_used=$((repair_used + 1))
    write_repair_prompt "$repair_prompt_file"
    if ! ma_validate_outbound_prompt "$repair_prompt_file"; then
      printf '\n\n## Repair %s\n\nSKIPPED: repair context contains sensitive-looking content; repair stopped.\n' "$repair_used" >> "$artifact"
      echo "repair $repair_used blocked: sensitive outbound context" >&2
      break
    fi
    # Restore the worktree to the captured patch state: undo tracked-file
    # mutations from verify and drop its untracked byproducts, so the next
    # capture_patch cannot sweep verification residue into the patch.
    git -C "$worktree" checkout -- . 2>/dev/null || true
    git -C "$worktree" clean -fd >/dev/null 2>&1 || true
    {
      printf '\n\n## Repair %s\n\n### Prompt\n\n' "$repair_used"
      cat "$repair_prompt_file"
      printf '\n\n### Output\n\n'
    } >> "$artifact"
    run_worker "$repair_prompt_file"
    capture_patch
    if [ -n "$VERIFY_CMD" ]; then
      printf '\n\n## Verify (repair %s)\n\n- command: %s\n\n' "$repair_used" "$VERIFY_CMD" >> "$artifact"
      run_verify
    fi
    echo "repair $repair_used: worker exit $worker_status, verify exit $verify_status"
  done
fi

printf '\n\n## Exit\n\n%s\n' "$worker_status" >> "$artifact"

applied=0
if [ "$APPLY" = 1 ] && [ "$DRY_RUN" = 1 ]; then
  echo "apply skipped: dry run" >&2
elif [ "$APPLY" = 1 ]; then
  if [ "$worker_status" -ne 0 ] || [ "$verify_status" -ne 0 ]; then
    echo "apply skipped: worker or verify failed" >&2
  elif [ ! -s "$patch_file" ]; then
    echo "apply skipped: empty patch" >&2
  elif [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
    # Untracked files are fine: git apply fails on collision anyway, and the
    # artifact dir itself lives untracked inside the repo.
    # Warn instead of fail: the worker already ran, so still record and report.
    echo "apply skipped: main tree has uncommitted changes" >&2
  else
    # Landing gate: re-admit the patch in a throwaway worktree (applies cleanly,
    # parses, carries no secrets, passes verification) before it touches the
    # main tree. This catches stale patches and worker-environment illusions
    # that the worker's own in-worktree verify cannot.
    admit_script="$(ma_scripts_dir)/patch-admit.sh"
    admit_args=(--patch "$patch_file" --repo "$REPO")
    [ -n "$VERIFY_CMD" ] && admit_args+=(--verify "$VERIFY_CMD")
    if [ -x "$admit_script" ] && ! bash "$admit_script" "${admit_args[@]}" >/dev/null; then
      echo "apply skipped: patch-admit rejected the patch (see .oms/artifacts/admit/)" >&2
    elif git -C "$REPO" apply --binary "$patch_file"; then
      applied=1
    else
      echo "apply failed: patch did not apply cleanly; review $patch_file" >&2
    fi
  fi
fi

index_exit=0
if [ "$worker_status" -ne 0 ] || [ "$verify_status" -ne 0 ]; then
  index_exit=1
fi
ma_append_artifact_index "$REPO" delegate "$TO" "$index_exit" "$artifact" "$patch_file" "$prompt_file" "$verify_status" || true

echo "worker: $TO exit $worker_status"
if [ -n "$VERIFY_CMD" ]; then
  echo "verify: exit $verify_status"
fi
if [ "$REPAIR" -gt 0 ]; then
  echo "repair: $repair_used/$REPAIR round(s) used"
fi
echo "artifact: $artifact"
echo "patch: $patch_file"
if [ "$applied" = 1 ]; then
  echo "applied: yes (review with: git -C $REPO diff)"
else
  echo "applied: no (review patch, then: git -C $REPO apply --binary $patch_file)"
fi
if [ "$KEEP_WORKTREE" = 1 ]; then
  echo "worktree kept: $worktree"
fi

if [ "$worker_status" -ne 0 ] || [ "$verify_status" -ne 0 ]; then
  plan_transition release
  exit 1
fi
if [ "$applied" = 1 ]; then
  plan_transition finish --artifact "$artifact" --patch "$patch_file"
else
  plan_transition review --artifact "$artifact" --patch "$patch_file"
fi
