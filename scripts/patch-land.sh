#!/usr/bin/env bash
set -euo pipefail

# Land a delegated patch onto the main tree, safely. patch-admit is the trust
# boundary (does it apply, parse, not touch its own verifier, and verify?); this
# is the one mutating step that composes it: clean-tree check -> patch-admit
# ADMIT gate -> git apply -> record a land row in the artifact index -> optional
# agent-plan finish. Nothing lands unless admission passes and the tree is clean,
# and the land is recorded so "which patch was applied for task X" is answerable.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/multi-agent-common.sh
. "$ROOT_LIB/multi-agent-common.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
ML=0
PLAN_TASK=""
ALLOW_VERIFIER_CHANGE=0

usage() {
  cat <<'EOF'
Usage: patch-land.sh --patch FILE [options]

Admit a delegated patch and, only if it passes, apply it to the main tree.

Options:
  --patch FILE     Patch file to land (required).
  --repo PATH      Target git repo (default: current directory).
  --verify CMD     Verification command for the admission gate (forwarded to
                   patch-admit.sh; default: scripts/check.sh when present).
  --ml             Prefer ml-smoke verification when auto-detecting.
  --plan-task ID   On a successful land, mark this agent-plan task done.
  --allow-verifier-change  Forward to patch-admit: permit a patch that touches
                   its own verifier (normally rejected).
  -h, --help       Show this help.

Sequence: main tree must be clean -> patch-admit returns ADMIT -> git apply
--binary -> land row appended to .oms/artifacts/index.jsonl. Exit is nonzero
(and nothing is applied) if the tree is dirty or admission fails.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --patch) [ "$#" -ge 2 ] || fail "--patch requires a file"; PATCH="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires a command"; VERIFY="$2"; shift 2 ;;
    --ml) ML=1; shift ;;
    --plan-task)
      [ "$#" -ge 2 ] || fail "--plan-task requires id"
      case "$2" in
        *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;;
      esac
      PLAN_TASK="$2"; shift 2 ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$PATCH" ] || fail "--patch is required"
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"

# Pre-flight: never apply onto a dirty tree — a half-applied patch on top of
# unrelated edits is the mess this whole gate exists to avoid.
if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
  fail "refusing to land: main tree has uncommitted changes"
fi

# --- Admission gate (the trust boundary) ------------------------------------
admit_cmd=("$ROOT/scripts/patch-admit.sh" --patch "$PATCH" --repo "$REPO")
[ -n "$VERIFY" ] && admit_cmd+=(--verify "$VERIFY")
[ "$ML" = 1 ] && admit_cmd+=(--ml)
[ "$ALLOW_VERIFIER_CHANGE" = 1 ] && admit_cmd+=(--allow-verifier-change)
if ! "${admit_cmd[@]}" >/dev/null; then
  echo "patch-land: REJECTED by admission gate; not applied" >&2
  exit 1
fi

# --- Apply ------------------------------------------------------------------
if ! git -C "$REPO" apply --binary "$PATCH"; then
  fail "admission passed but git apply failed (base moved?); tree unchanged"
fi

changed_files="$(git -C "$REPO" apply --numstat "$PATCH" 2>/dev/null | awk '{print $NF}' | tr '\n' ' ' | sed 's/ *$//')"
echo "patch-land: applied $PATCH" >&2
[ -n "$changed_files" ] && echo "patch-land: changed $changed_files" >&2

# Record the land so "what patch was applied for task X" is answerable. Stamp
# the plan/task id via OMS_TASK_ID so the row carries lineage.
[ -n "$PLAN_TASK" ] && export OMS_TASK_ID="$PLAN_TASK"
ma_append_artifact_index "$REPO" patch-land "" 0 "" "$PATCH" || true

# --- Optional plan lifecycle ------------------------------------------------
if [ -n "$PLAN_TASK" ]; then
  if "$ROOT/scripts/agent-plan.sh" --repo "$REPO" finish --id "$PLAN_TASK" \
       --patch "$PATCH" >/dev/null 2>&1; then
    echo "patch-land: plan task $PLAN_TASK -> done" >&2
  else
    echo "warning: could not finish plan task $PLAN_TASK (wrong state?)" >&2
  fi
fi

echo "LANDED"
