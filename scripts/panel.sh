#!/usr/bin/env bash
set -euo pipefail

# Open the terminal control panel for Codex and Claude Code, using existing OMS state and peer workflows.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
export PYTHONDONTWRITEBYTECODE=1
board=0
for arg in "$@"; do
  [ "$arg" != "--watch" ] || board=1
done
# A board inside an OMS panel session must never vanish: a crash, an interrupt,
# or a reload into a half-written source tree restarts it in place. Only a clean
# exit or the pane closing ends it.
case "${OMS_PANEL_SESSION:-}" in
  oms-*) ;;
  *) board=0 ;;
esac
if [ "$board" = 1 ] && [ -n "${TMUX:-}" ]; then
  # Closing the pane or window hangs up the whole group; that ends the loop too.
  trap 'exit 0' HUP TERM
  delay=2
  while :; do
    status=0
    python3 "$ROOT/scripts/lib/terminal_panel.py" "$@" || status=$?
    [ "$status" -ne 0 ] || exit 0
    printf '\nOMS board stopped (exit %s); restarting in %ss\n' "$status" "$delay" >&2
    sleep "$delay"
    [ "$delay" -ge 30 ] || delay=$((delay * 2))
  done
fi
exec python3 "$ROOT/scripts/lib/terminal_panel.py" "$@"
