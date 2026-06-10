#!/usr/bin/env bash
set -euo pipefail

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/multi-agent-common.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/multi-agent-common.sh"

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
                       has an executable scripts/check.sh, "bash scripts/check.sh fast".
  --no-verify          Skip the default scripts/check.sh verification.
  --apply              Apply the resulting patch to the main tree when the
                       worker and --verify succeed. Requires a clean main tree.
  --keep-worktree      Keep the worktree for manual inspection.
  --no-memory          Do not attach shared harness memory.
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
mkdir -p "$ARTIFACT_DIR"

prompt_file="$(mktemp)" || fail "mktemp failed"
worktree_parent="$(mktemp -d)" || fail "mktemp failed"
worktree="$worktree_parent/wt"
worktree_created=0
cleanup() {
  rm -f "$prompt_file"
  if [ "$worktree_created" = 1 ] && [ "$KEEP_WORKTREE" = 0 ]; then
    git -C "$REPO" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  fi
  if [ "$KEEP_WORKTREE" = 0 ]; then
    rm -rf "$worktree_parent"
  fi
}
trap cleanup EXIT

{
  printf 'You are %s, a delegated worker agent.\n' "$TO"
  printf 'Work only inside the current directory; it is an isolated git worktree of the repository.\n'
  printf 'Follow AGENTS.md, CLAUDE.md, and PROJECT.md in this directory if present.\n'
  printf 'Do not ask questions. If the task is ambiguous or blocked, stop and report the blocker explicitly.\n'
  printf 'Do not run git commit, git push, or change git config.\n'
  printf 'Do not add dependencies or change the toolchain unless the brief explicitly allows it.\n\n'
  if [ "$INCLUDE_MEMORY" -eq 1 ]; then
    ma_write_shared_memory_context "$REPO"
  fi
  printf '## Brief\n\n'
  if [ -n "$BRIEF_FILE" ]; then
    cat "$BRIEF_FILE"
  else
    printf '%s\n' "$PROMPT"
  fi
  printf '\nWhen done, report: changed files, what you verified, what you did not verify, and any blockers.\n'
} > "$prompt_file"

slug_src="$PROMPT"
[ -n "$slug_src" ] || slug_src="$(head -c 200 "$BRIEF_FILE")"
slug="$(slugify "$slug_src")"
[ -n "$slug" ] || slug="delegate"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)-$$"
artifact="$ARTIFACT_DIR/$TO-$slug-$timestamp.md"
patch_file="$ARTIFACT_DIR/$TO-$slug-$timestamp.patch"

git -C "$REPO" worktree add --detach "$worktree" HEAD >/dev/null 2>&1
worktree_created=1

# Verification contract: default to the project's check.sh when present.
if [ -z "$VERIFY_CMD" ] && [ "$NO_VERIFY" = 0 ] && [ -x "$worktree/scripts/check.sh" ]; then
  VERIFY_CMD="bash scripts/check.sh fast"
  echo "auto-verify: scripts/check.sh fast (disable with --no-verify)"
fi

{
  printf '# %s delegate\n\n' "$TO"
  printf -- '- started: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- repo: %s\n' "$REPO"
  printf -- '- worktree: %s\n\n' "$worktree"
  printf '## Prompt\n\n'
  cat "$prompt_file"
  printf '\n\n## Output\n\n'
} > "$artifact"

worker_status=0
if [ "$DRY_RUN" = "1" ]; then
  printf 'DRY RUN: worker command skipped.\n' >> "$artifact"
  echo "dry-run: $TO -> $artifact"
else
  binary="$TO"
  [ "$TO" = "antigravity" ] && binary="agy"
  command -v "$binary" >/dev/null 2>&1 || fail "command not found: $binary"

  set +e
  case "$TO" in
    codex)
      (cd "$worktree" && run_with_timeout codex exec --sandbox workspace-write - < "$prompt_file") >> "$artifact" 2>&1
      worker_status=$?
      ;;
    claude)
      (cd "$worktree" && run_with_timeout claude -p --permission-mode acceptEdits < "$prompt_file") >> "$artifact" 2>&1
      worker_status=$?
      ;;
    antigravity)
      (cd "$worktree" && run_with_timeout agy --print --sandbox --print-timeout "${OMS_MULTI_AGENT_PRINT_TIMEOUT:-5m}" < "$prompt_file") >> "$artifact" 2>&1
      worker_status=$?
      ;;
  esac
  set -e
fi

# Capture the patch before running --verify so verification byproducts
# (caches, build output) do not leak into the patch.
git -C "$worktree" add -A
git -C "$worktree" diff --cached --binary > "$patch_file"

verify_status=0
if [ -n "$VERIFY_CMD" ]; then
  printf '\n\n## Verify\n\n- command: %s\n\n' "$VERIFY_CMD" >> "$artifact"
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY RUN: verify skipped.\n' >> "$artifact"
  else
    set +e
    (cd "$worktree" && bash -c "$VERIFY_CMD") >> "$artifact" 2>&1
    verify_status=$?
    set -e
    printf '\n- verify exit: %s\n' "$verify_status" >> "$artifact"
  fi
fi

printf '\n\n## Exit\n\n%s\n' "$worker_status" >> "$artifact"

applied=0
if [ "$APPLY" = 1 ] && [ "$DRY_RUN" = 0 ]; then
  if [ "$worker_status" -ne 0 ] || [ "$verify_status" -ne 0 ]; then
    echo "apply skipped: worker or verify failed" >&2
  elif [ ! -s "$patch_file" ]; then
    echo "apply skipped: empty patch" >&2
  elif [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
    # Untracked files are fine: git apply fails on collision anyway, and the
    # artifact dir itself lives untracked inside the repo.
    fail "refusing --apply: main tree has uncommitted changes"
  else
    git -C "$REPO" apply --binary "$patch_file"
    applied=1
  fi
fi

echo "worker: $TO exit $worker_status"
if [ -n "$VERIFY_CMD" ]; then
  echo "verify: exit $verify_status"
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
  exit 1
fi
