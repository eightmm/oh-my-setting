#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ -s "$NVM_DIR/nvm.sh" ]; then
    # shellcheck disable=SC1091
    . "$NVM_DIR/nvm.sh"
    nvm use default >/dev/null 2>&1 || true
  fi
}

tool_version() {
  local name="$1"
  local version=""
  if command -v timeout >/dev/null 2>&1; then
    version="$(timeout 10 "$name" --version 2>/dev/null | head -n 1 || true)"
  else
    version="$("$name" --version 2>/dev/null | head -n 1 || true)"
  fi
  printf '%s' "$version" | cut -c1-64
}

tool_status() {
  local name="$1"
  local version
  if command -v "$name" >/dev/null 2>&1; then
    version="$(tool_version "$name")"
    if [ -n "$version" ]; then
      printf -- '- %s: %s (%s)\n' "$name" "$(command -v "$name")" "$version"
    else
      printf -- '- %s: %s\n' "$name" "$(command -v "$name")"
    fi
  else
    printf -- '- %s: missing\n' "$name"
  fi
}

link_status() {
  local target="$1"
  local expected="$2"
  local current

  if [ -L "$target" ]; then
    current="$(readlink "$target")"
    if [ "$current" = "$expected" ]; then
      printf -- '- %s: linked\n' "$target"
    else
      printf -- '- %s: linked elsewhere -> %s\n' "$target" "$current"
    fi
  elif [ -e "$target" ]; then
    printf -- '- %s: regular file\n' "$target"
  else
    printf -- '- %s: missing\n' "$target"
  fi
}

file_status() {
  local path="$1"
  if [ -e "$path" ] || [ -L "$path" ]; then
    if modified="$(stat -c '%y' "$path" 2>/dev/null)"; then
      printf -- '- %s: present, updated %s\n' "$path" "${modified%%.*}"
    else
      printf -- '- %s: present\n' "$path"
    fi
  else
    printf -- '- %s: missing\n' "$path"
  fi
}

auto_update_value() {
  local file="$1"
  local key="$2"
  awk -F= -v key="$key" '$1 == key { print substr($0, index($0, "=") + 1); exit }' "$file"
}

auto_update_status() {
  local state_file="${OH_MY_SETTING_AUTO_UPDATE_STATE:-$ROOT/local/auto-update.status}"
  local status
  local value

  if [ ! -f "$state_file" ]; then
    printf -- '- status: not checked\n'
    printf -- '- command: %s/scripts/auto-update.sh check\n' "$ROOT"
    return 0
  fi

  status="$(auto_update_value "$state_file" status)"
  printf -- '- status: %s\n' "${status:-unknown}"
  for key in last_run mode upstream local remote message; do
    value="$(auto_update_value "$state_file" "$key")"
    [ -n "$value" ] || continue
    printf -- '- %s: %s\n' "$key" "$value"
  done
}

task_section_value() {
  local file="$1"
  local section="$2"
  local key="${3:-}"

  [ -s "$file" ] || return 1
  if [ -n "$key" ]; then
    awk -v section="$section" -v key="$key" '
      $0 == section { in_section = 1; next }
      in_section == 1 && /^## / { in_section = 0 }
      in_section == 1 {
        pattern = "^- " key ":[[:space:]]*"
        if ($0 ~ pattern) {
          sub(pattern, "")
          print
          found = 1
          exit
        }
      }
      END { exit found ? 0 : 1 }
    ' "$file"
  else
    awk -v section="$section" '
      $0 == section { in_section = 1; next }
      in_section == 1 && /^## / { in_section = 0 }
      in_section == 1 && NF { print; found = 1; exit }
      END { exit found ? 0 : 1 }
    ' "$file"
  fi
}

active_task_status() {
  local task_file="${OH_MY_SETTING_TASK_FILE:-$ROOT/.oms/task/current.md}"
  local value

  if [ ! -s "$task_file" ]; then
    printf -- '- status: none\n'
    return 0
  fi

  printf -- '- status: active\n'
  value="$(grep -E '^- updated:' "$task_file" | head -n 1 | sed 's/^- updated:[[:space:]]*//' || true)"
  [ -n "$value" ] && printf -- '- updated: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Goal" 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- goal: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Next Step" 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- next: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Loop State" attempts 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- loop_attempts: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Loop State" max_attempts 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- loop_max: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Loop State" verification_level 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- verification_level: %s\n' "$value"
  value="$(task_section_value "$task_file" "## Loop State" diff_budget_lines 2>/dev/null || true)"
  [ -n "$value" ] && printf -- '- diff_budget_lines: %s\n' "$value"
}

auto_update_trigger_status() {
  local systemd_timer="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/oh-my-setting-autoupdate.timer"
  local cron_file="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"

  if [ -f "$systemd_timer" ]; then
    printf -- '- trigger: systemd user timer\n'
    return 0
  fi

  if [ -n "$cron_file" ] && [ -f "$cron_file" ] && grep -Fq '# oh-my-setting autoupdate:begin' "$cron_file"; then
    printf -- '- trigger: cron\n'
    return 0
  fi
  if [ -z "$cron_file" ] && command -v crontab >/dev/null 2>&1 && crontab -l 2>/dev/null | grep -Fq '# oh-my-setting autoupdate:begin'; then
    printf -- '- trigger: cron\n'
    return 0
  fi

  printf -- '- trigger: not installed\n'
}

load_user_tool_paths

printf '# oh-my-setting status\n\n'
printf -- '- root: %s\n' "$ROOT"
if [ -f "$ROOT/VERSION" ]; then
  printf -- '- version: %s\n' "$(head -n 1 "$ROOT/VERSION")"
fi
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  printf -- '- branch: %s\n' "$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)"
  printf -- '- commit: %s\n' "$(git -C "$ROOT" rev-parse --short HEAD)"
fi

printf '\n## Agent config links\n\n'
link_status "$HOME/.codex/AGENTS.md" "$ROOT/AGENTS.md"
link_status "$HOME/.claude/CLAUDE.md" "$ROOT/AGENTS.md"
link_status "$HOME/.gemini/AGENTS.md" "$ROOT/AGENTS.md"

printf '\n## Required tools\n\n'
for tool in git curl node npm uv claude codex agy gh; do
  tool_status "$tool"
done

printf '\n## Optional tools\n\n'
for tool in timeout sbatch srun squeue sinfo scancel; do
  tool_status "$tool"
done

printf '\n## Snapshots\n\n'
file_status "$ROOT/local/machine.md"
file_status "$ROOT/custom-skills/slurm-hpc/references/cluster.generated.md"

printf '\n## Active Task\n\n'
active_task_status

printf '\n## Auto Update\n\n'
auto_update_status
auto_update_trigger_status
