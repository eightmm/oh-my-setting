#!/usr/bin/env bash
set -euo pipefail

# Read-only terminal dashboard of OMS-managed collaboration state. It shows
# goal and scope, task/plan and attempt states, provider/model attribution,
# acceptance evidence, and the inbox's next decision. Each refresh reads one
# state snapshot plus bounded artifact-index and lifecycle listings (the
# compact state omits per-attempt usage); it never writes .oms, calls a model,
# or touches the network.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"

REPO="$PWD"
AS_JSON=0
WATCH=0
INTERVAL=5
INTERVAL_SET=0
COUNT=0
MIN_INTERVAL=2

usage() {
  cat <<'EOF'
Usage: dashboard.sh [--repo PATH] [--json] [--watch [--interval N] [--count N]]

Show a bounded, read-only snapshot of OMS-managed collaboration records.
Native provider subagents that never touched OMS state are not observed.

  --repo PATH     Repository to inspect (default: current repository).
  --json          Emit one schema-1 JSON object (see docs/DASHBOARD.md).
  --watch         Refresh until interrupted (Ctrl-C). The screen is cleared
                  only when stdout is a terminal; otherwise snapshots are
                  appended with a separator line.
  --interval N    Seconds between refreshes (default 5, minimum 2).
  --count N       Stop after N refreshes (watch only).

Exit status: 0 when every source was collected, 1 when the view is degraded
(a source failed or returned an unsupported contract), 2 for usage errors,
130 on interrupt.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

positive_int() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 6 ] && [ "$1" -gt 0 ]
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    --watch) WATCH=1; shift ;;
    --interval)
      [ "$#" -ge 2 ] || fail "--interval requires seconds"
      positive_int "$2" || fail "--interval must be a whole number of seconds"
      [ "$2" -ge "$MIN_INTERVAL" ] || fail "--interval must be at least $MIN_INTERVAL seconds"
      INTERVAL="$2"; INTERVAL_SET=1; shift 2 ;;
    --count)
      [ "$#" -ge 2 ] || fail "--count requires a number"
      positive_int "$2" || fail "--count must be a positive whole number"
      COUNT="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ "$WATCH" = 1 ] || [ "$INTERVAL_SET" = 0 ] || fail "--interval requires --watch"
[ "$WATCH" = 1 ] || [ "$COUNT" = 0 ] || fail "--count requires --watch"
[ "$WATCH" = 0 ] || [ "$AS_JSON" = 0 ] || fail "--json is a single snapshot; drop --watch"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
[ -d "$REPO" ] || fail "repository not found: $REPO"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
REPO="$(cd "$REPO" && pwd -P)" || fail "bad --repo"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/oms-dashboard.XXXXXX")"
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT
trap 'cleanup; trap - EXIT; exit 130' INT
trap 'cleanup; trap - EXIT; exit 143' HUP TERM

collect() {  # NAME COMMAND... -> tmp/snap/NAME.{json,rc,err}
  local name="$1" rc=0
  shift
  "$@" > "$tmp/snap/$name.json" 2> "$tmp/snap/$name.err" || rc=$?
  printf '%s\n' "$rc" > "$tmp/snap/$name.rc"
}

set_dimensions() {  # stdout is only a terminal outside command substitution
  WIDTH="${COLUMNS:-}"
  HEIGHT=""
  if [ -z "$WIDTH" ] && [ -t 1 ]; then
    WIDTH="$(tput cols 2>/dev/null || true)"
  fi
  WIDTH="${WIDTH//$'\r'/}"
  positive_int "$WIDTH" || WIDTH=120
  if [ "$WATCH" = 1 ] && [ -t 1 ]; then
    HEIGHT="${LINES:-}"
    [ -n "$HEIGHT" ] || HEIGHT="$(tput lines 2>/dev/null || true)"
    HEIGHT="${HEIGHT//$'\r'/}"
    positive_int "$HEIGHT" || HEIGHT=24
  fi
}

snapshot() {  # -> tmp/view, returns the projection exit status
  local rc=0
  rm -rf "$tmp/snap"
  mkdir "$tmp/snap"
  collect state bash "$ROOT/scripts/state.sh" --repo "$REPO" --json
  collect artifacts bash "$ROOT/scripts/artifact-index.sh" --repo "$REPO" --json list 8
  collect attempts bash "$ROOT/scripts/agent-events.sh" --repo "$REPO" list --limit 8 --json
  set -- "$tmp/snap" --repo-name "$(basename "$REPO")" --repo-path "$REPO" \
    --width "$WIDTH"
  [ "$AS_JSON" = 0 ] || set -- "$@" --json
  [ "$WATCH" = 0 ] || set -- "$@" --interval "$INTERVAL"
  [ -z "$HEIGHT" ] || set -- "$@" --height "$HEIGHT"
  PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/scripts/lib/dashboard_projection.py" "$@" \
    > "$tmp/view" || rc=$?
  return "$rc"
}

if [ "$WATCH" = 0 ]; then
  rc=0
  set_dimensions
  snapshot || rc=$?
  cat "$tmp/view"
  exit "$rc"
fi

# Render into a buffer first so a slow or failed refresh never leaves a blank
# screen; only a terminal is cleared, so a piped or logged stream keeps every
# snapshot.
n=0
rc=0
while :; do
  rc=0
  set_dimensions
  snapshot || rc=$?
  if [ -t 1 ]; then
    tput clear 2>/dev/null || printf '\033[H\033[2J'
  elif [ "$n" -gt 0 ]; then
    printf -- '----\n'
  fi
  cat "$tmp/view"
  n=$((n + 1))
  if [ "$COUNT" -gt 0 ] && [ "$n" -ge "$COUNT" ]; then
    break
  fi
  sleep "$INTERVAL"
done
exit "$rc"
