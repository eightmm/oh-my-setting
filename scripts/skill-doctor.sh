#!/usr/bin/env bash
set -euo pipefail

# Diagnose duplicate or missing skill entries across all three agents' skill
# roots, and rightsize the skills themselves: a SKILL.md that keeps growing
# stops being a router into detail and becomes context an agent pays for on
# every load, whether or not the task needs it. Progressive disclosure is the
# fix — a short SKILL.md that points at references/ read only when relevant.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"

printf '# oh-my-setting skill doctor\n\n'
oms_ops_reset_check_state
oms_ops_check_skill_root "Codex skills" "$HOME/.codex/skills"
oms_ops_check_skill_root "Claude skills" "$HOME/.claude/skills"
oms_ops_check_skill_root "Antigravity skills" "$HOME/.gemini/antigravity/skills"

# Rightsizing is advisory: an oversized skill still works, it just costs every
# agent that loads it. Budgets are overridable because a domain skill with no
# sensible split is a legitimate exception.
printf '\n## Skill size\n\n'
skill_budget="${OMS_SKILL_WORDS:-900}"
skills_dir="$ROOT/custom-skills"
oversized=0
if [ -d "$skills_dir" ]; then
  for skill_md in "$skills_dir"/*/SKILL.md; do
    [ -f "$skill_md" ] || continue
    skill_name="$(basename "$(dirname "$skill_md")")"
    words="$(wc -w < "$skill_md" | tr -d ' ')"
    if [ "$words" -gt "$skill_budget" ]; then
      oversized=$((oversized + 1))
      if [ -d "$(dirname "$skill_md")/references" ]; then
        printf 'warn: %s SKILL.md is %s words (budget %s); move detail into its references/\n' \
          "$skill_name" "$words" "$skill_budget"
      else
        printf 'warn: %s SKILL.md is %s words (budget %s); split it into references/ and link them\n' \
          "$skill_name" "$words" "$skill_budget"
      fi
    elif [ -d "$(dirname "$skill_md")/references" ] &&
      ! grep -q 'references/' "$skill_md"; then
      printf 'warn: %s has references/ that SKILL.md never links; they will not be read\n' \
        "$skill_name"
      oversized=$((oversized + 1))
    fi
  done
fi
if [ "$oversized" -eq 0 ]; then
  printf 'ok: every SKILL.md is within the load budget and links its references\n'
fi

if [ "$OMS_OPS_FAILED" -ne 0 ]; then
  printf 'skill-doctor: failed\n'
  exit 1
fi

printf 'skill-doctor: ok\n'
