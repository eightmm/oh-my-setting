#!/usr/bin/env bash
set -euo pipefail

# UserPromptSubmit hook: deterministic skill routing. Skills often
# go un-invoked because nothing at prompt time reminds the model they exist;
# this matches the prompt against the trigger phrases in skills.manifest.json
# and prints a one-line hint (stdout becomes injected context). Precision over
# recall: at most OMS_ROUTER_MAX suggestions per prompt, each skill suggested
# at most once per turn, silence on no match, and system-ish prompts
# (tool notifications, slash commands) are skipped entirely. Fail-open: this
# hook must never block a prompt.
#
# Automatic task recording is opt-in with OMS_AUTO_TASK=1. Disable the router
# entirely with OMS_SKILL_ROUTER_OFF=1. Claude Code installs this directly;
# Codex installs it through the repo-local oh-my-setting plugin.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

# Rollover/catch-up on each top-level agent prompt. This stays independent of
# skill routing (which users may disable): local materialization runs first,
# then at most one pending mirror item gets a two-second retry. On the first
# prompt of a local day it also prints a bounded journal digest — hook stdout
# becomes agent context, which is what makes the journal self-referencing.
if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] &&
  git -C "${OMS_STATE_REPO:-$PWD}" rev-parse --git-dir >/dev/null 2>&1; then
  work_journal_prompt_tick "${OMS_STATE_REPO:-$PWD}"
fi

# State-conditional hints: inject the one thing native skill matching cannot
# know — this repo's harness state. A stale claimed task outranks a repeated
# unresolved failure; one line, at most once per local day per repo, adopted
# repos only. The day marker lives under .oms/hooks/, the subtree already
# treated as live-session state. OMS_STATE_HINTS=0 disables. Fail-open.
state_hint() {
  local repo="$1"
  local day marker task failures
  day="$(date +%Y-%m-%d)"
  marker="$repo/.oms/hooks/state-hint.$day"
  [ -e "$marker" ] && return 0
  mkdir -p "$repo/.oms/hooks" 2>/dev/null || return 0
  : > "$marker" 2>/dev/null || return 0
  find "$repo/.oms/hooks" -maxdepth 1 -name 'state-hint.*' \
    ! -name "state-hint.$day" -delete 2>/dev/null || true
  task="$(cd "$repo" && bash "$ROOT/scripts/agent-task.sh" status --json 2>/dev/null || true)"
  failures="$(cd "$repo" && bash "$ROOT/scripts/fail-ledger.sh" list --json 2>/dev/null || true)"
  OMS_SH_TASK="$task" OMS_SH_FAILURES="$failures" python3 - <<'PY' 2>/dev/null || true
import json, os

def load(name):
    try:
        return json.loads(os.environ.get(name) or "{}")
    except ValueError:
        return {}

task = load("OMS_SH_TASK")
if task.get("present") and task.get("status") == "active" and task.get("stale"):
    print(
        "[oms] stale active task %s — read `oms agent-task status` before new"
        " work; the agent-harness skill covers resume/handoff."
        % (task.get("task_id") or "?")
    )
    raise SystemExit(0)
failures = load("OMS_SH_FAILURES")
rows = [f for f in failures.get("failures", []) if not f.get("resolved")]
if len(rows) >= 2:
    print(
        "[oms] %d unresolved fail-ledger rows here — run `oms fail-ledger"
        " list` before retrying anything that already failed."
        % len(rows)
    )
PY
}

if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ "${OMS_STATE_HINTS:-1}" = "1" ] &&
  [ -d "${OMS_STATE_REPO:-$PWD}/.oms" ]; then
  state_hint "${OMS_STATE_REPO:-$PWD}" || true
fi

[ "${OMS_SKILL_ROUTER_OFF:-0}" = "1" ] && exit 0

MANIFEST="${OMS_SKILL_MANIFEST:-$ROOT/skills.manifest.json}"
HELPER="$ROOT/scripts/lib/hook_state.py"
[ -f "$MANIFEST" ] || exit 0
[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# The helper owns routing state and fail-opens on malformed hook payloads.
OMS_HOOK_PAYLOAD="$(cat)" python3 "$HELPER" route --manifest "$MANIFEST" || exit 0
