#!/usr/bin/env bash
set -euo pipefail

DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.service"
TIMER_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.timer"
CRON_MARK_BEGIN="# oh-my-setting autoupdate:begin"
CRON_MARK_END="# oh-my-setting autoupdate:end"
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

read_cron() {
  if [ -n "$CRON_FILE" ]; then
    [ -f "$CRON_FILE" ] && cat "$CRON_FILE"
    return 0
  fi
  command -v crontab >/dev/null 2>&1 || return 0
  crontab -l 2>/dev/null || true
}

write_cron() {
  if [ -n "$CRON_FILE" ]; then
    cat > "$CRON_FILE"
  else
    crontab -
  fi
}

remove_systemd() {
  if [ "$DRY_RUN" = "1" ]; then
    printf 'would remove systemd user timer: %s\n' "$TIMER_FILE"
    return 0
  fi

  if command -v systemctl >/dev/null 2>&1 && [ -n "${XDG_RUNTIME_DIR:-}" ]; then
    systemctl --user disable --now oh-my-setting-autoupdate.timer >/dev/null 2>&1 || true
  fi
  rm -f "$TIMER_FILE" "$SERVICE_FILE"
  if command -v systemctl >/dev/null 2>&1 && [ -n "${XDG_RUNTIME_DIR:-}" ]; then
    systemctl --user daemon-reload >/dev/null 2>&1 || true
  fi
}

remove_cron() {
  local tmp
  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove cron trigger"
    return 0
  fi

  tmp="$(mktemp)"
  read_cron | awk -v begin="$CRON_MARK_BEGIN" -v end="$CRON_MARK_END" '
    $0 == begin { skip = 1; changed = 1; next }
    $0 == end { skip = 0; next }
    !skip { print }
    END { if (skip) exit 2; if (!changed) exit 3 }
  ' > "$tmp" || {
    code="$?"
    rm -f "$tmp"
    [ "$code" -eq 3 ] && return 0
    return "$code"
  }
  write_cron < "$tmp"
  rm -f "$tmp"
}

remove_systemd
remove_cron
echo "auto-update trigger: removed"
