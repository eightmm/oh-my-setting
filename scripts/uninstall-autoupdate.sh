#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.service"
TIMER_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.timer"
CRON_FILE="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"

usage() {
  cat <<'EOF'
Usage: uninstall-autoupdate.sh [--dry-run] [-h|--help]

Remove the oh-my-setting user-level auto-update trigger.

Options:
  --dry-run   Print actions without making changes.
  -h, --help  Show this help.

Environment:
  OH_MY_SETTING_AUTO_UPDATE_CRON_FILE=/path  Test-only cron file override.
  OH_MY_SETTING_DRY_RUN=1
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

physical_file_path() {
  local dir
  dir="$(cd "$(dirname "$1")" 2>/dev/null && pwd -P)" || {
    printf '%s\n' "$1"
    return 0
  }
  printf '%s/%s\n' "$dir" "$(basename "$1")"
}

# The unit name is global to the user's manager while the unit file lives under
# XDG_CONFIG_HOME, so the unit is ours only when the fragment the manager loaded
# is this file. A foreign config home (a test fixture, another profile) shares
# the name and nothing else; disabling by name from there switched off the
# operator's real updater on every gate run.
systemd_owns_unit() {
  local fragment
  command -v systemctl >/dev/null 2>&1 || return 1
  [ -n "${XDG_RUNTIME_DIR:-}" ] || return 1
  fragment="$(systemctl --user show -p FragmentPath --value \
    oh-my-setting-autoupdate.timer 2>/dev/null)" || return 1
  fragment="${fragment#FragmentPath=}"
  [ -n "$fragment" ] || return 1
  [ "$(physical_file_path "$fragment")" = "$(physical_file_path "$TIMER_FILE")" ]
}

remove_systemd() {
  if [ "$DRY_RUN" = "1" ]; then
    printf 'would remove systemd user timer: %s\n' "$TIMER_FILE"
    return 0
  fi

  if [ ! -e "$TIMER_FILE" ] && [ ! -e "$SERVICE_FILE" ]; then
    return 0
  fi
  if systemd_owns_unit; then
    systemctl --user disable --now oh-my-setting-autoupdate.timer >/dev/null 2>&1 || true
    rm -f "$TIMER_FILE" "$SERVICE_FILE"
    systemctl --user daemon-reload >/dev/null 2>&1 || true
    return 0
  fi
  rm -f "$TIMER_FILE" "$SERVICE_FILE"
}

remove_cron() {
  local current
  local filtered
  local state

  current="$(mktemp)" || return
  filtered="$(mktemp)" || {
    rm -f "$current"
    return 1
  }
  if ! oms_install_autoupdate_cron_read "$CRON_FILE" > "$current"; then
    rm -f "$current" "$filtered"
    echo "error: failed to read crontab" >&2
    return 1
  fi
  state="$(oms_install_autoupdate_cron_prepare "$current" "$filtered")" || {
    rm -f "$current" "$filtered"
    echo "error: failed to inspect crontab" >&2
    return 1
  }
  rm -f "$current"
  if [ "$state" = "malformed" ]; then
    rm -f "$filtered"
    echo "error: malformed auto-update cron block; refusing to modify crontab" >&2
    return 1
  fi
  if [ "$DRY_RUN" = "1" ]; then
    rm -f "$filtered"
    echo "would remove cron trigger"
    return 0
  fi
  case "$state" in
    absent)
      rm -f "$filtered"
      return 0
      ;;
  esac
  if ! oms_install_autoupdate_cron_write "$CRON_FILE" < "$filtered"; then
    rm -f "$filtered"
    echo "error: failed to write crontab" >&2
    return 1
  fi
  rm -f "$filtered"
}

# Classify and remove cron first. A malformed marker block must stop the whole
# uninstall before systemd files or the install receipt are changed.
remove_cron
remove_systemd
oms_install_record_auto_update false "$ROOT" "$DRY_RUN"
echo "auto-update trigger: removed"
