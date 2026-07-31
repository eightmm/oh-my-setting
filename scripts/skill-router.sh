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

# Local-only rollover/catch-up on the first top-level agent prompt. This stays
# independent of skill routing (which users may disable) and avoids network
# work inside the five-second provider hook budget. On the first prompt of a
# local day it also prints a bounded journal digest — hook stdout becomes
# agent context, which is what makes the journal self-referencing.
if [ "${OMS_HARNESS_CHILD:-0}" != 1 ] &&
  git -C "${OMS_STATE_REPO:-$PWD}" rev-parse --git-dir >/dev/null 2>&1; then
  work_journal_prompt_tick "${OMS_STATE_REPO:-$PWD}"
fi

[ "${OMS_SKILL_ROUTER_OFF:-0}" = "1" ] && exit 0

MANIFEST="${OMS_SKILL_MANIFEST:-$ROOT/skills.manifest.json}"
HELPER="$ROOT/scripts/lib/hook_state.py"
[ -f "$MANIFEST" ] || exit 0
[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# The helper owns routing state and fail-opens on malformed hook payloads.
OMS_HOOK_PAYLOAD="$(cat)" python3 "$HELPER" route --manifest "$MANIFEST" || exit 0
