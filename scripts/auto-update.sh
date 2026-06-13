#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-check}"
STATE_FILE="${OH_MY_SETTING_AUTO_UPDATE_STATE:-$ROOT/local/auto-update.status}"
LOG_FILE="${OH_MY_SETTING_AUTO_UPDATE_LOG:-$ROOT/local/auto-update.log}"
SKIP_DOCTOR="${OH_MY_SETTING_AUTO_UPDATE_SKIP_DOCTOR:-0}"
# Stable per-checkout target; file-lock.sh maps this path into runtime lock storage.
APPLY_LOCK_TARGET="$ROOT/local/auto-update.apply"

usage() {
  cat <<'EOF'
Usage: auto-update.sh [check|apply|status] [-h|--help]

Check for or apply oh-my-setting updates.

Modes:
  check   Fetch the configured upstream and record whether an update exists.
  apply   Apply only fast-forward updates; skips dirty/diverged checkouts.
          Re-runs link.sh, but intentionally skips tool (re)installation;
          use update.sh when install-tools.sh should be covered too.
  status  Print the last recorded auto-update state.

Environment:
  OH_MY_SETTING_AUTO_UPDATE_STATE=/path  Override state file.
  OH_MY_SETTING_AUTO_UPDATE_LOG=/path    Override log file.
  OH_MY_SETTING_AUTO_UPDATE_SKIP_DOCTOR=1 Skip doctor after apply.
EOF
}

now_utc() {
  date -u '+%Y-%m-%dT%H:%M:%SZ'
}

log_msg() {
  mkdir -p "$(dirname "$LOG_FILE")"
  printf '%s %s\n' "$(now_utc)" "$*" >> "$LOG_FILE"
}

write_state() {
  local status="$1"
  local message="$2"
  local local_commit="${3:-}"
  local remote_commit="${4:-}"
  local upstream="${5:-}"

  mkdir -p "$(dirname "$STATE_FILE")"
  {
    printf 'last_run=%s\n' "$(now_utc)"
    printf 'mode=%s\n' "$MODE"
    printf 'status=%s\n' "$status"
    printf 'message=%s\n' "$message"
    [ -n "$local_commit" ] && printf 'local=%s\n' "$local_commit"
    [ -n "$remote_commit" ] && printf 'remote=%s\n' "$remote_commit"
    [ -n "$upstream" ] && printf 'upstream=%s\n' "$upstream"
  } > "$STATE_FILE"
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

  if [ "$SKIP_DOCTOR" != "1" ]; then
    set +e
    "$ROOT/scripts/doctor.sh"
    doctor_status=$?
    set -e
  fi

  state_message="updated: $old_short -> $new_short"
  if [ "$doctor_status" -ne 0 ]; then
    state_message="$state_message (doctor reported warnings)"
    echo "auto-update: doctor reported warnings after apply" >&2
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
  check|apply) ;;
  *)
    usage >&2
    exit 2
    ;;
esac

require_git_checkout
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
  local_commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  write_state skipped "another auto-update run is in progress" "$local_commit" "" "$upstream"
  echo "auto-update: skipped (another run in progress)"
  exit 0
fi
exit "$apply_status"
