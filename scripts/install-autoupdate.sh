#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${OH_MY_SETTING_AUTO_UPDATE_MODE:-apply}"
METHOD="${OH_MY_SETTING_AUTO_UPDATE_METHOD:-auto}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.service"
TIMER_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.timer"
CRON_MARK_BEGIN="# oh-my-setting autoupdate:begin"
CRON_MARK_END="# oh-my-setting autoupdate:end"
CRON_FILE="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"

usage() {
  cat <<'EOF'
Usage: install-autoupdate.sh [--apply|--check-only] [--method auto|systemd|cron] [--dry-run] [-h|--help]

Install a user-level oh-my-setting auto-update trigger. Default mode is apply:
it updates only when the checkout is behind upstream and can fast-forward.

Options:
  --apply              Run auto-update.sh apply on the schedule. Default.
  --check-only         Only fetch/check and record update availability.
  --method METHOD      auto, systemd, or cron. Default: auto.
  --dry-run            Print what would be installed without writing.
  -h, --help           Show this help.

Environment:
  OH_MY_SETTING_AUTO_UPDATE_MODE=apply|check
  OH_MY_SETTING_AUTO_UPDATE_METHOD=auto|systemd|cron
  OH_MY_SETTING_AUTO_UPDATE_CRON_FILE=/path  Test-only cron file override.
  OH_MY_SETTING_DRY_RUN=1
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --apply) MODE=apply; shift ;;
    --check-only) MODE=check; shift ;;
    --method)
      [ "$#" -ge 2 ] || { echo "error: --method needs a value" >&2; exit 2; }
      METHOD="$2"
      shift 2
      ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$MODE" in
  apply|check) ;;
  *) echo "error: mode must be apply or check" >&2; exit 2 ;;
esac
case "$METHOD" in
  auto|systemd|cron) ;;
  *) echo "error: method must be auto, systemd, or cron" >&2; exit 2 ;;
esac

systemd_available() {
  command -v systemctl >/dev/null 2>&1 &&
    [ -n "${XDG_RUNTIME_DIR:-}" ] &&
    systemctl --user show-environment >/dev/null 2>&1
}

cron_available() {
  [ -n "$CRON_FILE" ] || command -v crontab >/dev/null 2>&1
}

choose_method() {
  if [ "$METHOD" != "auto" ]; then
    printf '%s\n' "$METHOD"
    return 0
  fi
  if systemd_available; then
    printf 'systemd\n'
  elif cron_available; then
    printf 'cron\n'
  else
    printf 'none\n'
  fi
}

install_systemd() {
  if [ "$DRY_RUN" = "1" ]; then
    printf 'would install systemd user timer: %s (%s)\n' "$TIMER_FILE" "$MODE"
    return 0
  fi

  mkdir -p "$SYSTEMD_DIR"
  cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=oh-my-setting auto-update

[Service]
Type=oneshot
ExecStart="$ROOT/scripts/auto-update.sh" $MODE
EOF
  cat > "$TIMER_FILE" <<'EOF'
[Unit]
Description=Run oh-my-setting auto-update daily

[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=30m

[Install]
WantedBy=timers.target
EOF

  systemctl --user daemon-reload
  systemctl --user enable --now oh-my-setting-autoupdate.timer >/dev/null
  echo "auto-update trigger: systemd timer installed ($MODE)"
}

read_cron() {
  if [ -n "$CRON_FILE" ]; then
    [ -f "$CRON_FILE" ] && cat "$CRON_FILE"
    return 0
  fi
  crontab -l 2>/dev/null || true
}

write_cron() {
  if [ -n "$CRON_FILE" ]; then
    cat > "$CRON_FILE"
  else
    crontab -
  fi
}

install_cron() {
  local line
  line="17 6 * * * \"$ROOT/scripts/auto-update.sh\" $MODE >/dev/null 2>&1"

  if [ "$DRY_RUN" = "1" ]; then
    printf 'would install cron trigger: %s\n' "$line"
    return 0
  fi

  tmp="$(mktemp)"
  read_cron | awk -v begin="$CRON_MARK_BEGIN" -v end="$CRON_MARK_END" '
    $0 == begin { skip = 1; next }
    $0 == end { skip = 0; next }
    !skip { print }
  ' > "$tmp"
  if ! {
    cat "$tmp"
    printf '%s\n' "$CRON_MARK_BEGIN"
    printf '%s\n' "$line"
    printf '%s\n' "$CRON_MARK_END"
  } | write_cron; then
    rm -f "$tmp"
    echo "error: failed to write crontab" >&2
    exit 1
  fi
  rm -f "$tmp"
  echo "auto-update trigger: cron installed ($MODE)"
}

chosen="$(choose_method)"
case "$chosen" in
  systemd)
    if ! systemd_available && [ "$METHOD" = "systemd" ]; then
      echo "auto-update trigger: systemd unavailable" >&2
      exit 1
    fi
    install_systemd
    ;;
  cron)
    if ! cron_available; then
      echo "auto-update trigger: cron unavailable" >&2
      exit 1
    fi
    install_cron
    ;;
  none)
    echo "auto-update trigger: no supported scheduler found"
    exit 0
    ;;
esac
