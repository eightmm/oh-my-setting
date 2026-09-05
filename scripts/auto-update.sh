#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "$ROOT/scripts/lib/python-runtime.sh" ]; then
  # shellcheck source=scripts/lib/python-runtime.sh
  . "$ROOT/scripts/lib/python-runtime.sh"
  oms_python_runtime_activate_if_present
fi

# A delegated worker may inspect the last updater state, but fetching,
# applying, or wiring a future unmarked timer spends parent host authority.
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  case "${1:-check}" in
    status|attention|-h|--help) ;;
    check|apply|install|remove)
      echo "error: a harness child cannot mutate OMS host lifecycle authority" >&2
      exit 2
      ;;
  esac
fi

# The trigger installers are part of this capability, not separate tools:
# auto-update install|remove owns the user-level trigger lifecycle.
case "${1:-}" in
  install) shift; exec "$ROOT/scripts/install-autoupdate.sh" "$@" ;;
  remove) shift; exec "$ROOT/scripts/uninstall-autoupdate.sh" "$@" ;;
esac

# The install contract is the canonical home of the trigger predicate status
# shares with attention. This script also runs from minimal trees — update
# transaction fixtures, a checkout mid-upgrade — that carry only its lock and
# poll helpers, and an unattended timer job must not die there, so the
# contract is optional and the one reading it needs travels with the script.
if [ -f "$ROOT/scripts/lib/install-contract.sh" ]; then
  # shellcheck source=scripts/lib/install-contract.sh
  . "$ROOT/scripts/lib/install-contract.sh"
fi
if ! declare -F oms_install_autoupdate_systemd_state >/dev/null; then
  oms_install_autoupdate_systemd_state() {
    local unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

    if [ ! -f "$unit_dir/oh-my-setting-autoupdate.timer" ]; then
      printf 'absent\n'
    elif [ -L "$unit_dir/timers.target.wants/oh-my-setting-autoupdate.timer" ]; then
      printf 'enabled\n'
    else
      printf 'disabled\n'
    fi
  }
fi

# This script's check/apply modes run unattended (cron, systemd timer). Git's
# credential and host-key questions read /dev/tty, so an interactive fallback
# is never answerable here: fail fast into the existing failure branches
# instead of waiting on a prompt no one will see.
export GIT_TERMINAL_PROMPT=0

MODE="${1:-check}"
STATE_FILE="${OH_MY_SETTING_AUTO_UPDATE_STATE:-$ROOT/local/auto-update.status}"
LOG_FILE="${OH_MY_SETTING_AUTO_UPDATE_LOG:-$ROOT/local/auto-update.log}"
SKIP_DOCTOR="${OH_MY_SETTING_AUTO_UPDATE_SKIP_DOCTOR:-0}"
# Stable per-checkout target; file-lock.sh maps this path into runtime lock storage.
APPLY_LOCK_TARGET="$ROOT/local/auto-update.apply"

usage() {
  cat <<'EOF'
Usage: auto-update.sh [check|apply|status|install|remove] [-h|--help]

Check for or apply oh-my-setting updates.

Modes:
  check   Fetch the configured upstream and record whether an update exists.
  apply   Apply only fast-forward updates; skips dirty/diverged checkouts.
          Re-runs link.sh, but intentionally skips tool (re)installation;
          use update.sh --tools when install-tools.sh should be covered too.
  status  Print the last recorded auto-update state.
  attention
          One-line verdict over intent, wiring, and outcome: disabled,
          unwired, no-run, failed, blocked, session-only, overdue, or ok. Shared by status,
          repo-state, inbox, and the resume hook; always exits 0.
  install Register the user-level auto-update trigger (systemd timer/cron).
  remove  Remove the auto-update trigger.

Environment:
  OH_MY_SETTING_AUTO_UPDATE_STATE=/path  Override state file.
  OH_MY_SETTING_AUTO_UPDATE_LOG=/path    Override log file.
  OH_MY_SETTING_AUTO_UPDATE_SKIP_DOCTOR=1 Skip doctor after apply.
  OH_MY_SETTING_CLAUDE_HOOKS=0          Skip Claude hook refresh.
  OH_MY_SETTING_CODEX_PLUGIN=0          Skip Codex plugin refresh.
EOF
}

now_utc() {
  date -u '+%Y-%m-%dT%H:%M:%SZ'
}

auto_update_receipt_field() {
  local receipt="$1"
  local key="$2"
  local value
  [ -f "$receipt" ] || return 1
  # Windows Python writes CRLF, and this value is compared to a literal and used
  # as a path. This runs from a timer with no operator watching, so a stray
  # carriage return here reads as a foreign checkout and silently skips.
  value="$(python3 - "$receipt" "$key" <<'PY'
import json, sys
try:
    value = json.load(open(sys.argv[1], encoding="utf-8"))
    for part in sys.argv[2].split("."):
        value = value[part]
    if isinstance(value, bool):
        print("true" if value else "false")
    elif isinstance(value, (str, int, float)):
        print(value)
    else:
        raise ValueError
except Exception:
    raise SystemExit(1)
PY
)" || return
  printf '%s\n' "${value//$'\r'/}"
}

log_msg() {
  mkdir -p "$(dirname "$LOG_FILE")"
  printf '%s %s\n' "$(now_utc)" "$*" >> "$LOG_FILE"
}

# One line naming why an update run failed, safe for the key=value state
# file: the last error:/fatal: line of the captured output, byte-capped;
# the exit code when the output carries no such line.
auto_update_error_line() {
  local output="$1"
  local status="$2"
  local line
  line="$(printf '%s\n' "$output" | grep -E '^(error|fatal):' | tail -n 1 | head -c 200)"
  printf '%s' "${line:-exit $status}"
}

# One shared verdict over intent (receipt), wiring (trigger), and outcome
# (state file), so status, repo-state, inbox, and the resume hook can never
# disagree about whether the updater needs attention. Field incident behind
# it: a daily apply timer failed for weeks and the only trace was this
# file's status line, which nobody reads. Reporting only; always exit 0.
# Intent is judged first — an opted-out or non-owner checkout is `disabled`
# no matter what a historical status file says.
print_attention() {
  local receipt="${OMS_INSTALL_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/install.json}"
  local cron_file="${OH_MY_SETTING_AUTO_UPDATE_CRON_FILE:-}"
  local overdue="${OMS_AUTO_UPDATE_OVERDUE:-172800}"
  local owner enabled wired=0 systemd_state status last_run message age now epoch_last

  owner="$(auto_update_receipt_field "$receipt" source_root 2>/dev/null || true)"
  if [ -z "$owner" ] ||
     [ "$(cd "$owner" 2>/dev/null && pwd -P || true)" != "$(cd "$ROOT" && pwd -P)" ]; then
    echo "attention: disabled — this checkout does not own the install receipt"
    return 0
  fi
  enabled="$(auto_update_receipt_field "$receipt" components.auto_update 2>/dev/null || true)"
  if [ "$enabled" = "false" ]; then
    echo "attention: disabled — receipt opts out of auto-update"
    return 0
  fi

  systemd_state="$(oms_install_autoupdate_systemd_state)"
  [ "$systemd_state" = "enabled" ] && wired=1
  if [ "$wired" = 0 ]; then
    if [ -n "$cron_file" ]; then
      grep -Fq "auto-update.sh" "$cron_file" 2>/dev/null && wired=1
    elif command -v crontab >/dev/null 2>&1; then
      crontab -l 2>/dev/null | grep -Fq "auto-update.sh" && wired=1
    fi
  fi
  if [ "$wired" = 0 ]; then
    if [ "$systemd_state" = "disabled" ]; then
      echo "attention: unwired — the systemd timer unit exists but is not enabled, so nothing fires it (run: auto-update.sh install)"
    else
      echo "attention: unwired — auto-update is enabled but no timer or cron trigger is installed (run: auto-update.sh install)"
    fi
    return 0
  fi

  if [ ! -f "$STATE_FILE" ]; then
    echo "attention: no-run — trigger installed but no update run recorded yet"
    return 0
  fi
  # One file open for all three fields: three separate opens could mix two
  # generations of the file even after the writer became atomic.
  state_parsed="$(awk -F= '
    $1 == "status" && !s { s = substr($0, index($0, "=") + 1) }
    $1 == "last_run" && !l { l = substr($0, index($0, "=") + 1) }
    $1 == "message" && !m { m = substr($0, index($0, "=") + 1) }
    END { print s; print l; print m }
  ' "$STATE_FILE")"
  status="$(printf '%s\n' "$state_parsed" | sed -n 1p)"
  last_run="$(printf '%s\n' "$state_parsed" | sed -n 2p)"
  message="$(printf '%s\n' "$state_parsed" | sed -n 3p)"
  if [ -z "$status" ]; then
    # No readable status is a torn write or corruption, never a green light.
    echo "attention: unknown — state file has no readable status (empty or mid-write); recheck after the next run"
    return 0
  fi
  if [ "$status" = "failed" ]; then
    echo "attention: failed — ${message:-last update run failed}${last_run:+ (last run $last_run)}"
    return 0
  fi
  if [ "$status" = "skipped" ]; then
    echo "attention: blocked — ${message:-last update run was skipped}${last_run:+ (last run $last_run)}"
    return 0
  fi
  if [ "$systemd_state" = enabled ] &&
     [ "$(oms_install_autoupdate_linger 2>/dev/null || true)" = no ]; then
    echo "attention: session-only — user timer can stop after logout (linger disabled)"
    return 0
  fi
  if [ -n "$last_run" ]; then
    now="$(date -u +%s)"
    epoch_last="$(python3 -c 'import calendar,sys,time
try:
    print(calendar.timegm(time.strptime(sys.argv[1], "%Y-%m-%dT%H:%M:%SZ")))
except Exception:
    raise SystemExit(1)' "$last_run" 2>/dev/null || true)"
    if [ -n "$epoch_last" ]; then
      age=$((now - epoch_last))
      if [ "$age" -ge "$overdue" ]; then
        echo "attention: overdue — last run $last_run is older than ${overdue}s; the trigger may be dead"
        return 0
      fi
    fi
  fi
  echo "attention: ok${last_run:+ — last run $last_run}"
}

write_state() {
  local status="$1"
  local message="$2"
  local local_commit="${3:-}"
  local remote_commit="${4:-}"
  local upstream="${5:-}"

  mkdir -p "$(dirname "$STATE_FILE")"
  # Stage beside the file and publish with one rename: status.sh, the inbox
  # ranker, and attention read this without a lock, and a truncate-in-place
  # write let a torn read mask a failed run as ok. Overlapping check/apply
  # runs resolve by last completion; the rename keeps every observed state a
  # complete one.
  {
    printf 'last_run=%s\n' "$(now_utc)"
    printf 'mode=%s\n' "$MODE"
    printf 'status=%s\n' "$status"
    printf 'message=%s\n' "$message"
    printf 'source_root=%s\n' "$ROOT"
    [ -n "$local_commit" ] && printf 'local=%s\n' "$local_commit"
    [ -n "$remote_commit" ] && printf 'remote=%s\n' "$remote_commit"
    [ -n "$upstream" ] && printf 'upstream=%s\n' "$upstream"
    :
  } > "$STATE_FILE.tmp.$$"
  mv -f "$STATE_FILE.tmp.$$" "$STATE_FILE"
  log_msg "$MODE: $status: $message"
}

state_value() {
  local key="$1"
  [ -f "$STATE_FILE" ] || return 1
  awk -F= -v key="$key" '$1 == key { print substr($0, index($0, "=") + 1); exit }' "$STATE_FILE"
}

print_status() {
  if [ ! -f "$STATE_FILE" ]; then
    echo "auto-update: not checked"
    return 0
  fi

  printf 'auto-update: %s\n' "$(state_value status || true)"
  printf 'last run: %s\n' "$(state_value last_run || true)"
  printf 'message: %s\n' "$(state_value message || true)"
  if upstream="$(state_value upstream || true)" && [ -n "$upstream" ]; then
    printf 'upstream: %s\n' "$upstream"
  fi
  if local_commit="$(state_value local || true)" && [ -n "$local_commit" ]; then
    printf 'local: %s\n' "$local_commit"
  fi
  if remote_commit="$(state_value remote || true)" && [ -n "$remote_commit" ]; then
    printf 'remote: %s\n' "$remote_commit"
  fi
}

require_git_checkout() {
  if [ ! -d "$ROOT/.git" ] || ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    write_state skipped "not a git checkout"
    echo "auto-update: skipped (not a git checkout)"
    exit 0
  fi
}

branch_upstream() {
  local branch
  local remote
  local merge_ref
  local remote_branch

  branch="$(git -C "$ROOT" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
  if [ -z "$branch" ]; then
    write_state skipped "detached HEAD; auto-update skipped"
    return 1
  fi

  remote="$(git -C "$ROOT" config "branch.$branch.remote" || true)"
  merge_ref="$(git -C "$ROOT" config "branch.$branch.merge" || true)"
  if [ -z "$remote" ] || [ -z "$merge_ref" ]; then
    write_state skipped "no upstream configured for $branch"
    return 1
  fi

  remote_branch="${merge_ref#refs/heads/}"
  printf '%s\t%s\t%s\n' "$remote" "refs/remotes/$remote/$remote_branch" "$remote/$remote_branch"
}

receipt_transaction_context() {
  local receipt="${OMS_INSTALL_RECEIPT:-${XDG_CONFIG_HOME:-$HOME/.config}/oh-my-setting/install.json}"
  local schema ref owner channel

  schema="$(auto_update_receipt_field "$receipt" schema 2>/dev/null || true)"
  owner="$(auto_update_receipt_field "$receipt" source_root 2>/dev/null || true)"
  case "$schema" in
    2)
      ref="$(auto_update_receipt_field "$receipt" ref 2>/dev/null || true)"
      ;;
    ""|1)
      # Pre-schema-2 receipts stored the update pin as channel. Route those
      # installs through update.sh too: it owns rollback, component refresh,
      # receipt migration, and the expected-target fence. Only a truly
      # receipt-less install stays on the legacy branch-upstream path below.
      channel="$(auto_update_receipt_field "$receipt" channel 2>/dev/null || true)"
      if [ "$channel" = detached ]; then
        ref="$(auto_update_receipt_field "$receipt" commit 2>/dev/null || true)"
      else
        ref="$channel"
      fi
      ;;
    *) return 1 ;;
  esac
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*)
      command -v cygpath >/dev/null 2>&1 && owner="$(cygpath -u "$owner")"
      ;;
  esac
  [ -n "$ref" ] || return 1
  [ -n "$owner" ] && [ "$(cd "$owner" 2>/dev/null && pwd -P || true)" = "$(cd "$ROOT" && pwd -P)" ] || return 1
  RECEIPT_TRANSACTION_REF="$ref"
}

receipt_transaction_update() {
  local ref="${RECEIPT_TRANSACTION_REF:-}"
  local current output status remote message upstream base latest expected

  [ -n "$ref" ] || return 2
  current="$(git -C "$ROOT" rev-parse HEAD)"
  upstream="ref:$ref"

  if [ "$MODE" = check ]; then
    set +e
    output="$($ROOT/scripts/update.sh --check 2>&1)"
    status=$?
    set -e
    if [ "$status" -ne 0 ]; then
      write_state failed "check failed: $(auto_update_error_line "$output" "$status")" \
        "$current" "" "$upstream"
      print_status
      return "$status"
    fi
    remote="$(printf '%s\n' "$output" | awk '/^update-check: up_to_date /{print $3} /^update-check: available /{print $NF}' | tail -n 1)"
    case "$output" in
      *"update-check: available "*)
        write_state update_available "update available: ${current:0:7} -> ${remote:0:7}" "$current" "$remote" "$upstream"
        ;;
      *)
        write_state up_to_date "already up to date" "$current" "${remote:-$current}" "$upstream"
        ;;
    esac
    print_status
    return 0
  fi

  # Schema-2 installs resolve their target through update.sh rather than a
  # branch upstream. Apply the same unattended safety contract as the legacy
  # path: dirty, ahead, and diverged checkouts are normal skips, not failures.
  # Check first so the target commit is fetched without mutating the checkout.
  if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    write_state skipped "dirty tree; auto-apply skipped" "$current" "" "$upstream"
    echo "auto-update: skipped (dirty tree)"
    return 0
  fi

  set +e
  output="$($ROOT/scripts/update.sh --check 2>&1)"
  status=$?
  set -e
  if [ "$status" -ne 0 ]; then
    write_state failed "apply failed: $(auto_update_error_line "$output" "$status")" \
      "$current" "" "$upstream"
    print_status
    return "$status"
  fi
  remote="$(printf '%s\n' "$output" | awk '/^update-check: up_to_date /{print $3} /^update-check: available /{print $NF}' | tail -n 1)"
  if [ -z "$remote" ] || ! git -C "$ROOT" cat-file -e "$remote^{commit}" 2>/dev/null; then
    write_state failed "apply failed: update target was not reported" \
      "$current" "$remote" "$upstream"
    print_status
    return 1
  fi
  if [ "$current" = "$remote" ]; then
    write_state up_to_date "already up to date" "$current" "$remote" "$upstream"
    print_status
    return 0
  fi
  expected="$remote"

  base="$(git -C "$ROOT" merge-base "$current" "$remote" 2>/dev/null || true)"
  if [ "$base" != "$current" ]; then
    if [ "$base" = "$remote" ]; then
      write_state skipped "local checkout is ahead of $upstream" "$current" "$remote" "$upstream"
      echo "auto-update: skipped (local checkout is ahead)"
    else
      write_state skipped "local checkout diverged from $upstream" "$current" "$remote" "$upstream"
      echo "auto-update: skipped (diverged from upstream)"
    fi
    return 0
  fi

  # Close the fetch-to-apply race. A user edit or another updater changing HEAD
  # during preflight should make this run stand down, never feed a stale target
  # into the mutating transaction.
  latest="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  if [ "$latest" != "$current" ] || [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    write_state skipped "checkout changed during preflight; auto-apply skipped" "$latest" "$remote" "$upstream"
    echo "auto-update: skipped (checkout changed during preflight)"
    return 0
  fi

  set +e
  output="$(OH_MY_SETTING_UPDATE_EXPECTED_TARGET="$expected" \
    "$ROOT/scripts/update.sh" --no-tools 2>&1)"
  status=$?
  set -e
  [ -z "$output" ] || printf '%s\n' "$output"
  remote="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  if [ "$status" -eq 75 ] &&
     printf '%s\n' "$output" | grep -Fq 'error: update target changed after preflight:'; then
    write_state skipped "target changed during preflight; auto-apply skipped" \
      "$current" "$expected" "$upstream"
    echo "auto-update: skipped (target changed during preflight)"
    return 0
  fi
  if [ "$status" -ne 0 ]; then
    # Name the actual failure. The old fixed string ("receipt ref apply
    # failed") buried a plain "codex command is required" under a message
    # about refs, and the misdiagnosis it invites costs more than the line.
    write_state failed "apply failed: $(auto_update_error_line "$output" "$status")" \
      "$current" "$remote" "$upstream"
    return "$status"
  fi
  if [ "$current" = "$remote" ]; then
    message="already up to date"
    write_state up_to_date "$message" "$remote" "$remote" "$upstream"
  else
    message="updated: ${current:0:7} -> ${remote:0:7}"
    write_state applied "$message" "$remote" "$remote" "$upstream"
  fi
  print_status
  return 0
}

fetch_and_compare() {
  local remote="$1"
  local remote_ref="$2"
  local upstream="$3"
  local local_commit
  local remote_commit
  local base

  if ! git -C "$ROOT" fetch --quiet "$remote"; then
    write_state failed "fetch failed for $remote" "" "" "$upstream"
    echo "auto-update: failed (fetch failed for $remote)"
    exit 1
  fi

  local_commit="$(git -C "$ROOT" rev-parse HEAD)"
  remote_commit="$(git -C "$ROOT" rev-parse "$remote_ref" 2>/dev/null || true)"
  if [ -z "$remote_commit" ]; then
    write_state failed "remote ref missing after fetch: $remote_ref" "$local_commit" "" "$upstream"
    echo "auto-update: failed (remote ref missing)"
    exit 1
  fi

  base="$(git -C "$ROOT" merge-base HEAD "$remote_ref" || true)"
  if [ "$local_commit" = "$remote_commit" ]; then
    write_state up_to_date "already up to date" "$local_commit" "$remote_commit" "$upstream"
    echo "auto-update: up to date"
    return 1
  elif [ "$base" = "$local_commit" ]; then
    write_state update_available "update available: ${local_commit:0:7} -> ${remote_commit:0:7}" "$local_commit" "$remote_commit" "$upstream"
    echo "auto-update: update available (${local_commit:0:7} -> ${remote_commit:0:7})"
    return 0
  elif [ "$base" = "$remote_commit" ]; then
    write_state skipped "local checkout is ahead of $upstream" "$local_commit" "$remote_commit" "$upstream"
    echo "auto-update: skipped (local checkout is ahead)"
    return 1
  else
    write_state skipped "local checkout diverged from $upstream" "$local_commit" "$remote_commit" "$upstream"
    echo "auto-update: skipped (diverged from upstream)"
    return 1
  fi
}

auto_update_apply_locked() {
  local remote="$1"
  local remote_ref="$2"
  local upstream="$3"
  local old_short
  local new_short
  local new_full
  local local_full
  local remote_full
  local pull_text
  local pull_status
  local pull_detail
  local link_status
  local refresh_status
  local doctor_status=0
  local state_message

  if ! fetch_and_compare "$remote" "$remote_ref" "$upstream" >/dev/null; then
    print_status
    return 0
  fi

  # Re-check dirtiness right before pulling: edits may have landed since the
  # earlier check, and --ff-only still updates a non-conflicting dirty tree.
  if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    local_full="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
    write_state skipped "tree became dirty before pull; auto-apply skipped" "$local_full" "" "$upstream"
    echo "auto-update: skipped (tree became dirty)"
    return 0
  fi

  old_short="$(git -C "$ROOT" rev-parse --short HEAD)"
  set +e
  pull_text="$(git -C "$ROOT" pull --ff-only 2>&1)"
  pull_status=$?
  set -e
  [ -z "$pull_text" ] || printf '%s\n' "$pull_text"
  if [ "$pull_status" -ne 0 ]; then
    pull_detail="$pull_text"
    pull_detail="${pull_detail//$'\r'/ }"
    pull_detail="${pull_detail//$'\n'/ }"
    [ -n "$pull_detail" ] || pull_detail="git pull --ff-only exited $pull_status"
    local_full="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
    remote_full="$(git -C "$ROOT" rev-parse "$remote_ref" 2>/dev/null || true)"
    write_state failed "pull failed: $pull_detail" "$local_full" "$remote_full" "$upstream"
    echo "auto-update: failed (pull failed: $pull_detail)" >&2
    return "$pull_status"
  fi

  new_short="$(git -C "$ROOT" rev-parse --short HEAD)"
  new_full="$(git -C "$ROOT" rev-parse HEAD)"
  remote_full="$(git -C "$ROOT" rev-parse "$remote_ref" 2>/dev/null || true)"

  set +e
  "$ROOT/scripts/link.sh"
  link_status=$?
  set -e
  if [ "$link_status" -ne 0 ]; then
    write_state failed "post-update link failed at $new_short; install may be half-linked" "$new_full" "$remote_full" "$upstream"
    echo "auto-update: failed (post-update link failed at $new_short; install may be half-linked)" >&2
    return "$link_status"
  fi

  if [ "${OH_MY_SETTING_CLAUDE_HOOKS:-1}" = "1" ] &&
     [ -x "$ROOT/scripts/install-claude-hooks.sh" ]; then
    set +e
    "$ROOT/scripts/install-claude-hooks.sh"
    refresh_status=$?
    set -e
    if [ "$refresh_status" -ne 0 ]; then
      write_state failed "claude hook refresh failed after apply" "$new_full" "$remote_full" "$upstream"
      return "$refresh_status"
    fi
  fi

  if [ "${OH_MY_SETTING_CODEX_PLUGIN:-1}" = "1" ] &&
     [ -x "$ROOT/scripts/install-codex-plugin.sh" ]; then
    set +e
    "$ROOT/scripts/install-codex-plugin.sh"
    refresh_status=$?
    set -e
    if [ "$refresh_status" -ne 0 ]; then
      write_state failed "codex plugin refresh failed after apply" "$new_full" "$remote_full" "$upstream"
      return "$refresh_status"
    fi
  fi

  if [ "$SKIP_DOCTOR" != "1" ]; then
    set +e
    "$ROOT/scripts/doctor.sh"
    doctor_status=$?
    set -e
  fi

  state_message="updated: $old_short -> $new_short"
  if [ "$doctor_status" -ne 0 ]; then
    write_state failed "doctor failed after apply at $new_short" "$new_full" "$remote_full" "$upstream"
    echo "auto-update: doctor failed after apply" >&2
    return "$doctor_status"
  fi

  write_state applied "$state_message" "$new_full" "$remote_full" "$upstream"
  echo "auto-update: applied ($old_short -> $new_short)"
}

[ "$#" -le 1 ] || {
  echo "error: too many arguments" >&2
  usage >&2
  exit 2
}

case "$MODE" in
  -h|--help)
    usage
    exit 0
    ;;
  status)
    print_status
    exit 0
    ;;
  attention)
    print_attention
    exit 0
    ;;
  check|apply) ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [ "${OMS_AUTO_UPDATE_MANAGED:-0}" = 1 ]; then
  if ! oms_python_runtime_matches_lock "$ROOT"; then
    runtime_output="$("$ROOT/scripts/python-runtime.sh" ensure 2>&1)" || {
      write_state failed "private runtime setup failed: $(auto_update_error_line "$runtime_output" 1)"
      printf '%s\n' "$runtime_output" >&2
      exit 1
    }
  fi
  oms_python_runtime_activate || {
    write_state failed "private runtime activation failed"
    exit 1
  }
fi
require_git_checkout
# Both receipt-driven and legacy apply paths mutate the same checkout and
# install receipt. Resolve schema-2 ownership before taking their shared,
# non-blocking transaction lock; check mode stays read-only and lock-free.
if receipt_transaction_context; then
  if [ "$MODE" = apply ]; then
    # shellcheck source=scripts/lib/file-lock.sh
    . "$ROOT/scripts/lib/file-lock.sh"
    set +e
    oms_try_file_lock "$APPLY_LOCK_TARGET" receipt_transaction_update
    receipt_status=$?
    set -e
    if [ "$receipt_status" = 75 ]; then
      local_commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
      write_state skipped "another auto-update run is in progress" "$local_commit"
      echo "auto-update: skipped (another run in progress)"
      exit 0
    fi
  else
    set +e
    receipt_transaction_update
    receipt_status=$?
    set -e
  fi
  exit "$receipt_status"
fi
upstream_info="$(branch_upstream)" || {
  print_status
  exit 0
}
IFS=$'\t' read -r remote remote_ref upstream <<EOF
$upstream_info
EOF

if [ "$MODE" = "check" ]; then
  fetch_and_compare "$remote" "$remote_ref" "$upstream" >/dev/null || true
  print_status
  exit 0
fi

if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
  local_commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  write_state skipped "dirty tree; auto-apply skipped" "$local_commit" "" "$upstream"
  echo "auto-update: skipped (dirty tree)"
  exit 0
fi

# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

set +e
oms_try_file_lock "$APPLY_LOCK_TARGET" auto_update_apply_locked "$remote" "$remote_ref" "$upstream"
apply_status=$?
set -e
if [ "$apply_status" = "75" ]; then
  # The live run owns the state file; a loser that wrote "skipped" here could
  # land after the winner's real outcome and overwrite it.
  echo "auto-update: skipped (another run in progress)"
  exit 0
fi
exit "$apply_status"
