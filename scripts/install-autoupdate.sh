#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"
MODE="${OH_MY_SETTING_AUTO_UPDATE_MODE:-apply}"
METHOD="${OH_MY_SETTING_AUTO_UPDATE_METHOD:-auto}"
DRY_RUN="${OH_MY_SETTING_DRY_RUN:-0}"
SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.service"
TIMER_FILE="$SYSTEMD_DIR/oh-my-setting-autoupdate.timer"
CRON_FILE="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"
CLAUDE_HOOKS="${OH_MY_SETTING_CLAUDE_HOOKS:-1}"
CODEX_PLUGIN="${OH_MY_SETTING_CODEX_PLUGIN:-1}"

usage() {
  cat <<'EOF'
Usage: install-autoupdate.sh [--apply|--check-only] [--method auto|systemd|cron] [--dry-run] [-h|--help]

Install a user-level oh-my-setting auto-update trigger. Default mode is
apply: when the checkout is behind upstream and can fast-forward, the
schedule applies it (dirty or diverged checkouts are skipped). Pass
--check-only (or set the env) to only record that an update exists.

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
case "$CLAUDE_HOOKS:$CODEX_PLUGIN" in
  0:0|0:1|1:0|1:1) ;;
  *) echo "error: hook/plugin opt-outs must be 0 or 1" >&2; exit 2 ;;
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
Environment=OH_MY_SETTING_CLAUDE_HOOKS=$CLAUDE_HOOKS
Environment=OH_MY_SETTING_CODEX_PLUGIN=$CODEX_PLUGIN
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

install_cron() {
  local current
  local filtered
  local line
  local state
  line="17 6 * * * OH_MY_SETTING_CLAUDE_HOOKS=$CLAUDE_HOOKS OH_MY_SETTING_CODEX_PLUGIN=$CODEX_PLUGIN \"$ROOT/scripts/auto-update.sh\" $MODE >/dev/null 2>&1"

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
    printf 'would install cron trigger: %s\n' "$line"
    return 0
  fi
  if ! {
    cat "$filtered"
    printf '%s\n' "$OMS_INSTALL_AUTOUPDATE_CRON_MARK_BEGIN"
    printf '%s\n' "$line"
    printf '%s\n' "$OMS_INSTALL_AUTOUPDATE_CRON_MARK_END"
  } | oms_install_autoupdate_cron_write "$CRON_FILE"; then
    rm -f "$filtered"
    echo "error: failed to write crontab" >&2
    return 1
  fi
  rm -f "$filtered"
  echo "auto-update trigger: cron installed ($MODE)"
}

preflight_cron_state() {
  local state

  state="$(oms_install_autoupdate_cron_state "$CRON_FILE")" || {
    echo "error: failed to inspect crontab" >&2
    return 1
  }
  if [ "$state" = "malformed" ]; then
    echo "error: malformed auto-update cron block; refusing to install scheduler trigger" >&2
    return 1
  fi
}

# Cron is one part of the scheduler ownership state even when systemd wins
# automatic selection. Refuse malformed ownership before probing another
# scheduler or writing its units/receipt. install_cron still reclassifies a
# fresh snapshot immediately before its own mutation to close the read gap.
preflight_cron_state
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
oms_install_record_auto_update true "$ROOT" "$DRY_RUN" "$MODE"
