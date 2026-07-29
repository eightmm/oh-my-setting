#!/usr/bin/env bash
set -euo pipefail

# Install rules, skills, prompts, and the oms dispatcher into all three agent
# CLIs. POSIX hosts use symlinks; Windows Git Bash uses ownership-marked copies.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
STAMP="$(date +%Y%m%d%H%M%S)"
# shellcheck source=scripts/lib/agent-install-state.sh
. "$ROOT/scripts/lib/agent-install-state.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

backup_if_needed() {
  local target="$1"
  local source="$2"
  local state="$3"
  local backup

  case "$state" in
    symlink-current|symlink-owned|copy-current|copy-owned)
      oms_install_remove_managed_target "$target"
      return 0
      ;;
  esac

  # Pre-split installs pointed at the repository overlay. It is an old managed
  # target, not user data, so migrate it without manufacturing a backup.
  if [ -L "$target" ] &&
     [ "$source" = "$ROOT/rules/global-AGENTS.md" ] &&
     [ "$(readlink "$target")" = "$ROOT/AGENTS.md" ]; then
    rm -f "$target"
    return 0
  fi

  if [ -L "$target" ] || [ -e "$target" ]; then
    backup="$target.backup.$STAMP"
    mv "$target" "$backup"
    # A modified managed copy is now preserved as user data. Its ownership
    # marker must not travel into the backup or claim that it is removable.
    oms_install_remove_marker "$target"
    oms_install_remove_marker "$backup"
  fi
}

link_target() {
  local source="$1"
  local target="$2"
  local desired_mode
  local state

  mkdir -p "$(dirname "$target")"
  desired_mode="$(oms_install_link_mode)" || return
  state="$(oms_install_target_state "$source" "$target")" || return
  case "$desired_mode:$state" in
    symlink:symlink-current|copy:copy-current)
      echo "$desired_mode current: $target"
      return 0
      ;;
  esac
  backup_if_needed "$target" "$source" "$state"
  oms_install_materialize "$source" "$target"
  if [ "$desired_mode" = copy ]; then
    echo "copied $source -> $target"
  else
    echo "linked $target -> $source"
  fi
}

link_skills() {
  local target_root="$1"
  local skill
  local name
  local source
  local enabled_sources
  local link

  mkdir -p "$target_root"
  oms_ops_clean_backup_skill_links "$target_root" 0

  enabled_sources="$(python3 - "$ROOT/skills.manifest.json" <<'PY'
import json
import sys
for skill in json.load(open(sys.argv[1], encoding="utf-8")).get("skills", []):
    source = str(skill.get("source", ""))
    if skill.get("enabled") is True and source.startswith("custom-skills/"):
        print(source)
PY
)"
  while IFS= read -r source; do
    # Windows Python prints CRLF, so this line would name a directory with a
    # trailing carriage return and the copy would fail on a source that is
    # right there. Confirmed on CI against custom-skills/oh-my-setting-ops.
    source="${source%%$'\r'}"
    [ -n "$source" ] || continue
    skill="$ROOT/$source"
    name="$(basename "$skill")"
    link_target "$skill" "$target_root/$name"
  done <<< "$enabled_sources"

  # Remove only disabled links owned by this checkout. User directories and
  # foreign links are preserved for explicit cleanup/recovery.
  for skill in "$ROOT"/custom-skills/*; do
    [ -d "$skill" ] || continue
    source="${skill#"$ROOT"/}"
    printf '%s\n' "$enabled_sources" | grep -Fxq "$source" && continue
    name="$(basename "$skill")"
    if oms_install_target_owned "$skill" "$target_root/$name"; then
      oms_install_remove_managed_target "$target_root/$name"
      echo "unlinked disabled skill $target_root/$name"
    fi
  done

  # Remove stale links or copies owned by this checkout: a renamed/deleted
  # skill leaves an entry whose recorded source no longer exists. Foreign and
  # user-modified entries are preserved.
  for link in "$target_root"/*; do
    [ -L "$link" ] || [ -e "$link" ] || continue
    source="$(oms_install_target_owned_source_under \
      "$ROOT/custom-skills" "$link")" || continue
    if [ ! -e "$source" ]; then
      oms_install_remove_managed_target "$link"
      echo "unlinked stale skill $link (target removed)"
    fi
  done
}

link_all() {
  local skill_validation
  local receipt

  if ! skill_validation="$("$ROOT/scripts/install-skills.sh" 2>&1)"; then
    printf '%s\n' "$skill_validation" >&2
    return 1
  fi
  oms_ops_cleanup_legacy_links 0

  link_target "$ROOT/rules/global-AGENTS.md" "$HOME/.codex/AGENTS.md"
  link_target "$ROOT/rules/global-AGENTS.md" "$HOME/.claude/CLAUDE.md"
  # Antigravity global customizations root: rules at ~/.gemini/AGENTS.md,
  # skills under ~/.gemini/antigravity/skills.
  link_target "$ROOT/rules/global-AGENTS.md" "$HOME/.gemini/AGENTS.md"
  link_skills "$HOME/.codex/skills"
  link_skills "$HOME/.claude/skills"
  link_skills "$HOME/.gemini/antigravity/skills"
  link_target "$ROOT/prompts" "$HOME/.oh-my-setting-prompts"
  # The one-name dispatcher: `oms <tool>` from any agent CLI, no script paths.
  link_target "$ROOT/scripts/oms" "$HOME/.local/bin/oms"

  # This is the batch commit marker: interrupted relinks retain the previous
  # owner and are diagnosed as drift rather than certifying a mixed install.
  receipt="$(oms_install_receipt_path)"
  oms_install_write_receipt "$ROOT" "$receipt"
  echo "install receipt: $receipt"
}

# The receipt path is stable across checkouts, so concurrent link.sh runs for
# the same HOME cannot interleave their global agent configuration.
oms_with_file_lock "$(oms_install_receipt_path)" link_all
