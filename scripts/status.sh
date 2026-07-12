#!/usr/bin/env bash
set -euo pipefail

# Print install status: link identity, tools, active task, and update state.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
VERBOSE=0

usage() {
  cat <<'EOF'
Usage: status.sh [--verbose]

Show install ownership, links, tool paths, snapshots, task, plugin, and update
state. --verbose also runs tool version and Codex plugin probes.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --verbose) VERBOSE=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

RECEIPT="$(oms_install_receipt_path)"
RECEIPT_STATE="missing"
INSTALL_ROOT="$ROOT"
if [ -f "$RECEIPT" ]; then
  if INSTALL_ROOT="$(oms_install_receipt_owner "$RECEIPT")"; then
    RECEIPT_STATE="valid"
  else
    RECEIPT_STATE="invalid"
    INSTALL_ROOT="$ROOT"
  fi
fi

load_user_tool_paths() {
  export PATH="$HOME/.local/bin:$PATH"
  export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"

  if [ "$VERBOSE" = "1" ] && [ -s "$NVM_DIR/nvm.sh" ]; then
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
  local version=""
  if command -v "$name" >/dev/null 2>&1; then
    if [ "$VERBOSE" = "1" ]; then
      version="$(tool_version "$name")"
    fi
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
  local state_file="${OH_MY_SETTING_AUTO_UPDATE_STATE:-$INSTALL_ROOT/local/auto-update.status}"
  local status
  local value
  local recorded_local
  local recorded_root
  local current_commit
  local stale=0

  if [ ! -f "$state_file" ]; then
    printf -- '- status: not checked\n'
    printf -- '- command: %s/scripts/auto-update.sh check\n' "$INSTALL_ROOT"
    return 0
  fi

  status="$(auto_update_value "$state_file" status)"
  recorded_local="$(auto_update_value "$state_file" local)"
  recorded_root="$(auto_update_value "$state_file" source_root)"
  current_commit="$(git -C "$INSTALL_ROOT" rev-parse HEAD 2>/dev/null || true)"
  if [ -n "$recorded_root" ]; then
    [ "$(oms_install_physical_root "$recorded_root" 2>/dev/null || true)" = "$INSTALL_ROOT" ] || stale=1
  fi
  if [ -n "$recorded_local" ] && [ -n "$current_commit" ]; then
    case "$current_commit" in "$recorded_local"*) ;; *) stale=1 ;; esac
  fi
  if [ "$stale" = "1" ]; then
    printf -- '- status: stale\n'
    printf -- '- recorded_status: %s\n' "${status:-unknown}"
  else
    printf -- '- status: %s\n' "${status:-unknown}"
  fi
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
  return 0
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

codex_plugin_status() {
  local plugin_version
  local marketplace_name
  local cache
  local expected_hash
  local actual_hash
  local marker_hash=""
  local marker_root=""

  if ! command -v codex >/dev/null 2>&1; then
    printf -- '- status: codex missing\n'
    return 0
  fi

  if codex plugin list --json 2>/dev/null |
     python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; sys.exit(0 if any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", [])) else 1)' 2>/dev/null; then
    plugin_version="$(oms_install_plugin_version "$INSTALL_ROOT")"
    marketplace_name="$(python3 - "$INSTALL_ROOT/.agents/plugins/marketplace.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    print(json.load(fh)["name"])
PY
)"
    cache="${CODEX_HOME:-$HOME/.codex}/plugins/cache/$marketplace_name/oh-my-setting/$plugin_version"
    expected_hash="$(oms_install_receipt_field plugin.sha256 "$RECEIPT" 2>/dev/null || oms_install_plugin_hash "$INSTALL_ROOT")"
    if [ -d "$cache" ]; then
      actual_hash="$(oms_install_tree_hash "$cache")"
      [ ! -f "$cache/.oh-my-setting-source-sha256" ] ||
        marker_hash="$(sed -n '1p' "$cache/.oh-my-setting-source-sha256")"
      [ ! -f "$cache/.oh-my-setting-source-root" ] ||
        marker_root="$(sed -n '1p' "$cache/.oh-my-setting-source-root")"
    else
      actual_hash="missing"
    fi
    if [ "$actual_hash" = "$expected_hash" ] &&
       [ "$marker_hash" = "$expected_hash" ] &&
       [ "$marker_root" = "$INSTALL_ROOT" ]; then
      printf -- '- status: installed (cache current)\n'
    else
      printf -- '- status: installed (cache stale)\n'
      printf -- '- command: %s/scripts/install-codex-plugin.sh\n' "$INSTALL_ROOT"
    fi
  else
    printf -- '- status: not installed\n'
    printf -- '- command: %s/scripts/install-codex-plugin.sh\n' "$INSTALL_ROOT"
  fi
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

printf '\n## Install Ownership\n\n'
case "$RECEIPT_STATE" in
  valid)
    printf -- '- receipt: %s\n' "$RECEIPT"
    printf -- '- canonical_root: %s\n' "$INSTALL_ROOT"
    printf -- '- receipt_schema: %s\n' "$(oms_install_receipt_field schema "$RECEIPT")"
    printf -- '- canonical_commit: %s\n' "$(oms_install_receipt_field commit "$RECEIPT")"
    printf -- '- channel: %s\n' "$(oms_install_receipt_field channel "$RECEIPT")"
    value="$(oms_install_receipt_field profile "$RECEIPT" 2>/dev/null || true)"
    [ -z "$value" ] || printf -- '- profile: %s\n' "$value"
    value="$(oms_install_receipt_field ref "$RECEIPT" 2>/dev/null || true)"
    [ -z "$value" ] || printf -- '- ref: %s\n' "$value"
    value="$(oms_install_receipt_field previous_commit "$RECEIPT" 2>/dev/null || true)"
    [ -z "$value" ] || printf -- '- rollback_commit: %s\n' "$value"
    printf -- '- installed_at: %s\n' "$(oms_install_receipt_field installed_at "$RECEIPT")"
    if [ "$INSTALL_ROOT" = "$ROOT" ]; then
      printf -- '- current_checkout: canonical\n'
    else
      printf -- '- current_checkout: foreign but canonical install is recorded\n'
    fi
    ;;
  invalid)
    printf -- '- receipt: invalid (%s)\n' "$RECEIPT"
    ;;
  missing)
    printf -- '- receipt: missing (legacy install)\n'
    printf -- '- expected_root: %s\n' "$ROOT"
    ;;
esac

printf '\n## Agent config links\n\n'
link_status "$HOME/.codex/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
link_status "$HOME/.claude/CLAUDE.md" "$INSTALL_ROOT/rules/global-AGENTS.md"
link_status "$HOME/.gemini/AGENTS.md" "$INSTALL_ROOT/rules/global-AGENTS.md"

printf '\n## Required tools\n\n'
for tool in git curl node npm uv claude codex agy gh; do
  tool_status "$tool"
done

printf '\n## Optional tools\n\n'
for tool in timeout sbatch srun squeue sinfo scancel; do
  tool_status "$tool"
done

printf '\n## Snapshots\n\n'
file_status "$INSTALL_ROOT/local/machine.md"
file_status "$INSTALL_ROOT/custom-skills/slurm-hpc/references/cluster.generated.md"

printf '\n## Active Task\n\n'
active_task_status

printf '\n## Codex Plugin\n\n'
if [ "$VERBOSE" = "1" ]; then
  codex_plugin_status
else
  printf -- '- status: not probed (use --verbose)\n'
fi

printf '\n## Auto Update\n\n'
auto_update_status
auto_update_trigger_status
