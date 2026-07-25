#!/usr/bin/env bash
set -euo pipefail

# First move for an agent landing in a repo with no .oms/ state. repo-state is
# read-only and assumes state exists; every family has its own init but nothing
# seeds them together or tells the agent what to do next. This creates the
# .oms/ skeleton (idempotent, non-destructive), hides the agent-facing files
# from git, and prints a next-actions checklist tailored to the detected
# project type and the harness files already in the repo. It never overwrites
# state and never writes project rules — applying a template stays an explicit
# call, because it depends on a confirmed project type.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
PRIVATE="${OH_MY_SETTING_PRIVATE_AGENT_FILES:-1}"

usage() {
  cat <<'EOF'
Usage: oms-init.sh [--repo PATH] [--no-private]

Seed repo-local .oms/ state (idempotent, non-destructive), hide the
agent-facing files from git, and print a next-actions checklist for an agent
starting work in this repo. Reports which project rules are missing but never
writes them: use apply-project-template once the project type is confirmed.

Options:
  --repo PATH   Repo to initialize (default: PWD, git-root anchored).
  --no-private  Do not hide AGENTS.md/CLAUDE.md/PROJECT.md from git
                (see project-private.sh).
  -h, --help    Show help.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --no-private) PRIVATE=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
case "$PRIVATE" in
  0|1) ;;
  *) fail "OH_MY_SETTING_PRIVATE_AGENT_FILES must be 0 or 1" ;;
esac

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

# Project rules the agents actually read. `oms init` reports them but never
# writes them: which template applies depends on a confirmed project type.
rules_state="missing"
if grep -lq "<!-- oh-my-setting:" "$STATE_ROOT/AGENTS.md" "$STATE_ROOT/CLAUDE.md" 2>/dev/null; then
  rules_state="present"
  grep -lq "<!-- oh-my-setting:" "$STATE_ROOT/AGENTS.md" 2>/dev/null &&
    grep -lq "<!-- oh-my-setting:" "$STATE_ROOT/CLAUDE.md" 2>/dev/null ||
    rules_state="partial"
fi
spec_state="missing"
if [ -f "$STATE_ROOT/PROJECT.md" ]; then
  spec_state="$(sed -n 's/^- State:[[:space:]]*//p' "$STATE_ROOT/PROJECT.md" | sed -n '1p')"
  spec_state="${spec_state:-unset}"
fi

# The template step no-ops on a repo that had no git yet (the common bootstrap
# order is scaffold -> git init), so this is where the exclusion actually lands
# for a new project. Writing .git/info/exclude is local and reversible, like
# the .oms/.gitignore guard above.
private_state="n/a"
if git -C "$STATE_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  private_state="hidden"
  if "$ROOT/scripts/project-private.sh" --repo "$STATE_ROOT" status 2>/dev/null |
    grep -q '^exposed'; then
    if [ "$PRIVATE" = "1" ] &&
      "$ROOT/scripts/project-private.sh" --repo "$STATE_ROOT" apply >/dev/null 2>&1; then
      private_state="just-hidden"
    else
      private_state="exposed"
    fi
  fi
fi

task_state="none"
[ -s "$OMS/task/current.md" ] && task_state="active"
plan_state="none"
[ -f "$OMS/plan/tasks.json" ] && plan_state="present"

echo "oms init: $STATE_ROOT (style=$style, memory=$mem_state, rules=$rules_state, spec=$spec_state)"
[ "$private_state" = "just-hidden" ] &&
  echo "oms init: hid the agent files from git (.git/info/exclude)"
echo
echo "Next actions:"
# The project rules are what every agent reads first, so a missing or half
# applied harness outranks task/plan advice.
case "$rules_state" in
  missing)
    echo "- No project rules yet: 'oms apply-project-template $style .' (auto|general|ml|slurm), then fill PROJECT.md."
    ;;
  partial)
    echo "- Project rules are only in one of AGENTS.md/CLAUDE.md: re-run 'oms apply-project-template auto .' and check 'oms project-doctor .'."
    ;;
  present)
    case "$spec_state" in
      missing) echo "- Rules applied but PROJECT.md is missing: 'oms apply-project-template auto .' recreates it." ;;
      draft|unset) echo "- PROJECT.md state is $spec_state: confirm the spec (goal, commands, verification) before broad work." ;;
      *) echo "- Project rules and PROJECT.md ($spec_state) are in place: 'oms project-doctor .' verifies drift." ;;
    esac
    ;;
esac
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
if [ "$private_state" = "exposed" ]; then
  echo "- Agent files are still visible to git: 'oms project-private apply' keeps a public repo clean (--untrack for committed ones)."
elif [ "$private_state" = "n/a" ]; then
  echo "- Not a git repo yet: after 'git init', re-run 'oms init' so the agent files stay out of the history."
fi
echo "- Before claiming done: 'bash scripts/check.sh' (if present) or the project's verify."
