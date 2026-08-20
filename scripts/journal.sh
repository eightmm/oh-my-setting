#!/usr/bin/env bash
set -euo pipefail

# Read (show), inspect, rebuild, configure, or explicitly sync the local-first
# Work Journal. `show --today|--week|--blockers|--recent N [--json]` is the
# agent read path over the derived summaries and event index.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PYTHON_ENTRY="$ROOT/scripts/lib/work_journal.py"

if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  case "${1:-}" in
    configure|disconnect|sync)
      echo "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent" >&2
      exit 2
      ;;
  esac
fi

case "${1:-}" in
  status|show|rebuild|sync|distill|identity)
    command_name="$1"
    repo="."
    previous=""
    for argument in "$@"; do
      if [ "$previous" = "--repo" ]; then
        repo="$argument"
        previous=""
        continue
      fi
      case "$argument" in
        --repo) previous="--repo" ;;
        --repo=*) repo="${argument#--repo=}" ;;
      esac
    done
    # shellcheck source=scripts/lib/work-journal.sh
    . "$ROOT/scripts/lib/work-journal.sh"
    repo="$(oms_repo_root "$repo" 2>/dev/null || printf '%s' "$repo")"
    repo="${repo//$'\r'/}"
    repo="$(cd "$repo" 2>/dev/null && pwd -P || printf '%s' "$repo")"
    if [ "$command_name" = "sync" ]; then
      shift
      force_args=""
      today_args=""
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --repo)
            [ "$#" -ge 2 ] || {
              echo "error: --repo requires a value" >&2
              exit 2
            }
            shift 2
            ;;
          --repo=*) shift ;;
          --force)
            force_args="--force"
            shift
            ;;
          --today)
            today_args="--today"
            shift
            ;;
          *)
            echo "error: unrecognized sync argument: $1" >&2
            exit 2
            ;;
        esac
      done
      work_journal_call_local "$repo" materialize --repo "$repo"
      set --
      [ -z "$force_args" ] || set -- "$@" "$force_args"
      [ -z "$today_args" ] || set -- "$@" "$today_args"
      work_journal_sync "$repo" "$@"
      exit $?
    fi
    work_journal_call_local "$repo" "$@"
    ;;
  *)
    exec python3 "$PYTHON_ENTRY" "$@"
    ;;
esac
