# shellcheck shell=bash
# Internal fail-open Work Journal observer shared by lifecycle scripts.

WORK_JOURNAL_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$WORK_JOURNAL_LIB_DIR/agent-memory-common.sh"

work_journal_enabled() {
  case "${OMS_WORK_JOURNAL:-1}" in
    0|false|FALSE|no|NO|off|OFF) return 1 ;;
    *) return 0 ;;
  esac
}

work_journal_python() {
  printf '%s\n' "${OMS_WORK_JOURNAL_PYTHON:-$WORK_JOURNAL_LIB_DIR/work_journal.py}"
}

work_journal_config_path() {
  local path="${OMS_WORK_JOURNAL_CONFIG:-}"
  if [ -z "$path" ] && [ -n "${XDG_CONFIG_HOME:-}" ]; then
    path="$XDG_CONFIG_HOME/oh-my-setting/work-journal.json"
  fi
  if [ -z "$path" ] && [ -n "${LOCALAPPDATA:-}" ] &&
    command -v cygpath >/dev/null 2>&1; then
    path="$LOCALAPPDATA/oh-my-setting/work-journal.json"
    path="$(cygpath -u "$path")"
  fi
  printf '%s\n' "${path:-$HOME/.config/oh-my-setting/work-journal.json}"
}

work_journal_run_locked() {
  local repo="$1"
  shift

  OMS_WORK_JOURNAL_ACTIVE=1 python3 "$(work_journal_python)" "$@"
}

work_journal_call_local() {
  local repo="$1"
  shift
  local state_root="$repo/.oms/work-journal"

  agent_memory_ensure_oms_ignore "$repo" >/dev/null 2>&1 || return 1
  mkdir -p "$state_root" || return 1
  # One lock serializes immutable event admission, materialized views, and sync
  # metadata. The lock itself remains in the harness's per-user lock directory.
  oms_with_file_lock "$state_root/state" work_journal_run_locked "$repo" "$@"
}

work_journal_sync() {
  local repo="$1"
  local state_root="$repo/.oms/work-journal"
  local rc=0
  shift

  if [ -z "${OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID:-}" ] &&
    [ -z "${OMS_WORK_JOURNAL_NOTION_DATABASE_ID:-}" ] &&
    [ ! -f "$(work_journal_config_path)" ]; then
    return 0
  fi

  # Remote work never owns the canonical event/materialization lock. If another
  # lifecycle is already syncing, leave the pending state for a later tick
  # instead of queueing a primary task behind network I/O.
  oms_try_file_lock "$state_root/notion-sync" \
    work_journal_run_locked "$repo" sync --repo "$repo" "$@" || rc=$?
  [ "$rc" = 75 ] && return 0
  return "$rc"
}

work_journal_observe() {
  local repo="$1"
  local source_type="$2"
  local source_file="$3"
  shift 3

  work_journal_enabled || return 0
  [ "${OMS_WORK_JOURNAL_ACTIVE:-0}" != 1 ] || return 0
  [ "${OMS_WORK_JOURNAL_SUPPRESS:-0}" != 1 ] || return 0
  repo="$(oms_repo_root "$repo" 2>/dev/null || printf '%s' "$repo")"
  repo="${repo//$'\r'/}"
  repo="$(cd "$repo" 2>/dev/null && pwd -P || printf '%s' "$repo")"
  if ! work_journal_call_local "$repo" observe --repo "$repo" \
    --source-type "$source_type" --source-file "$source_file" "$@" >/dev/null 2>&1; then
    echo "warning: Work Journal observer degraded; primary lifecycle result is unchanged" >&2
    return 0
  fi
  return 0
}

# Prompt-hook entry: a local rollover tick and once-per-local-day digest on
# stdout. UserPromptSubmit stdout becomes
# agent context, so this is the one place journal content surfaces without an
# explicit command. OMS_WORK_JOURNAL_DIGEST=0 keeps the tick but drops the
# injection. The tick also carries the once-per-local-day lesson distill, which
# owns its own day marker: OMS_JOURNAL_AUTODISTILL=0 opts out of that alone.
work_journal_prompt_tick() {
  local repo="$1"
  local out

  work_journal_enabled || return 0
  [ "${OMS_WORK_JOURNAL_ACTIVE:-0}" != 1 ] || return 0
  repo="$(oms_repo_root "$repo" 2>/dev/null || printf '%s' "$repo")"
  repo="${repo//$'\r'/}"
  repo="$(cd "$repo" 2>/dev/null && pwd -P || printf '%s' "$repo")"
  # Passive observer: a prompt in a repo the harness was never adopted into
  # must not seed .oms — every other hook already follows adopted-repos-only,
  # and seeding here is how merely-cloned repos ended up mirrored to Notion.
  # Deliberate front doors (oms journal, run-ledger, agent-task) still seed.
  [ -d "$repo/.oms" ] || return 0
  set -- tick --repo "$repo" --local-only
  case "${OMS_WORK_JOURNAL_DIGEST:-1}" in
    0|false|FALSE|no|NO|off|OFF) ;;
    *) set -- "$@" --digest ;;
  esac
  case "${OMS_JOURNAL_AUTODISTILL:-1}" in
    0|false|FALSE|no|NO|off|OFF) ;;
    *) set -- "$@" --autodistill ;;
  esac
  if ! out="$(work_journal_call_local "$repo" "$@" 2>/dev/null)"; then
    echo "warning: Work Journal materialization degraded; primary lifecycle result is unchanged" >&2
    return 0
  fi
  [ -z "$out" ] || printf '%s\n' "$out"
  return 0
}

# Top-level Stop captures the final HEAD locally before deferred publication.
work_journal_finish() {
  local repo="$1"

  work_journal_enabled || return 0
  [ "${OMS_WORK_JOURNAL_ACTIVE:-0}" != 1 ] || return 0
  repo="$(oms_repo_root "$repo" 2>/dev/null || printf '%s' "$repo")"
  repo="${repo//$'\r'/}"
  repo="$(cd "$repo" 2>/dev/null && pwd -P || printf '%s' "$repo")"
  # Same adopted-repos-only rule as the prompt tick: a Stop in an unadopted
  # repo must not seed .oms.
  [ -d "$repo/.oms" ] || return 0
  if ! work_journal_call_local "$repo" tick --repo "$repo" --local-only >/dev/null 2>&1; then
    echo "warning: Work Journal finish materialization degraded" >&2
    return 0
  fi
  return 0
}

# Keep automatic publishing on hosts without a maintenance timer, without
# holding the provider's Stop pipe open. Sync locks and content hashes also
# deduplicate this publisher against periodic maintenance.
work_journal_defer_finish() {
  local repo="$1"
  [ "${OMS_HARNESS_CHILD:-0}" != 1 ] || return 0
  [ "${OMS_WORK_JOURNAL_ACTIVE:-0}" != 1 ] || return 0
  [ -d "$repo/.oms" ] || return 0
  if [ "${OMS_CI_TICK:-1}" != 1 ]; then
    work_journal_enabled || return 0
    [ "${OMS_WORK_JOURNAL_SUPPRESS:-0}" != 1 ] || return 0
  fi
  python3 - "$WORK_JOURNAL_LIB_DIR/work-journal.sh" "$repo" <<'PY' 2>/dev/null || true
import subprocess
import sys

command = '''
. "$1"
repo="$2"
if work_journal_enabled && [ "${OMS_WORK_JOURNAL_SUPPRESS:-0}" != 1 ]; then
  OMS_WORK_JOURNAL_NOTION_MAX_PER_TICK=2 \
    OMS_WORK_JOURNAL_NOTION_TIMEOUT_SECONDS=4 \
    OMS_WORK_JOURNAL_NOTION_BUDGET_SECONDS=8 \
    work_journal_sync "$repo" --force --today || true
fi
if [ "${OMS_CI_TICK:-1}" = 1 ]; then
  remote="$(git -C "$repo" remote get-url origin 2>/dev/null)"
  case "$remote" in
    *github.com:*|*github.com/*)
      (cd "$repo" && OMS_CI_TICK_QUIET=1 bash "$WORK_JOURNAL_LIB_DIR/../ci-status.sh" tick) || true ;;
  esac
fi
'''
try:
    subprocess.Popen(
        ["bash", "-c", command, "oms-stop-publish", *sys.argv[1:]],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, start_new_session=True,
    )
except OSError:
    pass
PY
}
