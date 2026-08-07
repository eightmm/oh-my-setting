#!/usr/bin/env bash
set -euo pipefail

# Diagnose duplicate or missing skill entries across all three agents' skill
# roots and Codex's shared ~/.agents overlay, and rightsize the skills
# themselves: a SKILL.md that keeps growing
# stops being a router into detail and becomes context an agent pays for on
# every load, whether or not the task needs it. Progressive disclosure is the
# fix — a short SKILL.md that points at references/ read only when relevant.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
usage: skill-doctor.sh

Report duplicate or missing skill entries across the Codex, shared ~/.agents,
Claude Code, and Antigravity skill roots, including names duplicated across
Codex's effective overlay, and skills a previous install left under a
superseded name. Flag any SKILL.md over the load budget
(OMS_SKILL_WORDS, default 900) or holding references/ nothing links to.
Read-only; takes no arguments.
EOF
      exit 0
      ;;
    *) echo "error: skill-doctor.sh takes no arguments: $1" >&2; exit 2 ;;
  esac
fi

# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"

printf '# oh-my-setting skill doctor\n\n'
oms_ops_reset_check_state
oms_ops_check_skill_root "Codex skills" "$HOME/.codex/skills"
oms_ops_check_skill_root "Shared agent skills" "$HOME/.agents/skills"
oms_ops_check_skill_overlay "Codex effective skill overlay" \
  "$HOME/.codex/skills" "$HOME/.agents/skills"
oms_ops_check_skill_root "Claude skills" "$HOME/.claude/skills"
oms_ops_check_skill_root "Antigravity skills" "$HOME/.gemini/antigravity/skills"

# A skill renamed into the `oms-` namespace leaves the old directory installed,
# and the name check cannot see it: the two names differ, so nothing is
# duplicated — the old entry just keeps offering a stale skill under a name the
# catalog no longer ships. Linking removes the ones this checkout owns, so what
# reaches this report is a copy, a user directory, or a root linking never
# revisits. Advisory: deleting what the install does not own is the user's call.
printf '\n## Legacy skill names\n\n'
legacy_found=0
for skill_root in "$HOME/.codex/skills" "$HOME/.agents/skills" \
  "$HOME/.claude/skills" "$HOME/.gemini/antigravity/skills"; do
  [ -d "$skill_root" ] || continue
  while IFS= read -r rename_row; do
    [ -n "$rename_row" ] || continue
    legacy_name="${rename_row%%:*}"
    legacy_target="$skill_root/$legacy_name"
    # A link into the renamed source dangles, and -e is false for it.
    [ -L "$legacy_target" ] || [ -e "$legacy_target" ] || continue
    legacy_found=$((legacy_found + 1))
    printf "warn: legacy skill name '%s' at %s — superseded by '%s'; run 'oms update' to relink, then remove the old directory if it survives\n" \
      "$legacy_name" "$legacy_target" "${rename_row#*:}"
  done <<< "$(oms_ops_legacy_skill_renames)"
done
if [ "$legacy_found" -eq 0 ]; then
  printf 'ok: no skills installed under a superseded name\n'
fi

# Rightsizing is advisory: an oversized skill still works, it just costs every
# agent that loads it. Budgets are overridable because a focused skill with no
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
