#!/usr/bin/env bash
set -euo pipefail

# First move for an agent landing in a repo with no .oms/ state. repo-state is
# read-only and assumes state exists; every family has its own init but nothing
# seeds them together or tells the agent what to do next. This creates the
# .oms/ skeleton (idempotent, non-destructive) and prints a next-actions
# checklist tailored to the detected project type. It never overwrites state.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"

usage() {
  cat <<'EOF'
Usage: oms-init.sh [--repo PATH]

Seed repo-local .oms/ state (idempotent, non-destructive) and print a
next-actions checklist for an agent starting work in this repo.

Options:
  --repo PATH   Repo to initialize (default: PWD, git-root anchored).
  -h, --help    Show help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
OMS="$STATE_ROOT/.oms"

# Skeleton + the git-ignore guard (never commit repo-local agent state).
mkdir -p "$OMS"
agent_memory_ensure_oms_ignore "$STATE_ROOT" 2>/dev/null || true

style="general"
if [ -x "$ROOT/scripts/detect-project-style.sh" ]; then
  style="$("$ROOT/scripts/detect-project-style.sh" "$STATE_ROOT" 2>/dev/null || echo general)"
fi

# Seed shared memory if absent (holds durable cross-agent facts). Do NOT seed
# an active task — whether there is one is the agent's call.
mem="$OMS/memory/shared.md"
mem_state="present"
if [ ! -f "$mem" ]; then
  "$ROOT/scripts/agent-memory.sh" --repo "$STATE_ROOT" init >/dev/null 2>&1 || true
  mem_state="seeded"
fi

task_state="none"
[ -s "$OMS/task/current.md" ] && task_state="active"
plan_state="none"
[ -f "$OMS/plan/tasks.json" ] && plan_state="present"

echo "oms init: $STATE_ROOT (style=$style, memory=$mem_state)"
echo
echo "Next actions:"
echo "- Run 'oms state' to see task/plan/board/runs at a glance."
if [ "$task_state" = "none" ]; then
  echo "- No active task: 'oms agent-task init --goal \"...\" --verify \"bash scripts/check.sh\"'."
else
  echo "- An active task exists: 'oms agent-task context' to resume it."
fi
if [ "$plan_state" = "none" ]; then
  echo "- To split work across agents: 'oms agent-plan add ...' then 'next --claim --provider NAME'."
else
  echo "- A plan exists: 'oms agent-plan ready' for actionable tasks."
fi
if [ "$style" = "ml" ]; then
  echo "- ML repo: 'oms data-manifest check --name <manifest>' then 'oms data-manifest leakage --name <manifest>' before training when registered splits exist;"
  echo "  wrap runs in 'oms run-ledger' / 'oms run-capsule' and claim on 'oms experiment-board'."
fi
echo "- Reusable worker personas: 'oms agent-role list' (create with 'agent-role --name NAME init')."
echo "- Before claiming done: 'bash scripts/check.sh' (if present) or the project's verify."
