#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"

printf '# oh-my-setting skill doctor\n\n'
oms_ops_reset_check_state
oms_ops_check_skill_root "Codex skills" "$HOME/.codex/skills"
oms_ops_check_skill_root "Claude skills" "$HOME/.claude/skills"
oms_ops_check_skill_root "Antigravity skills" "$HOME/.gemini/antigravity/skills"

if [ "$OMS_OPS_FAILED" -ne 0 ]; then
  printf 'skill-doctor: failed\n'
  exit 1
fi

printf 'skill-doctor: ok\n'
