#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-file-lock-boundary.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "file-lock-boundary-smoke: $*" >&2
  exit 1
}

mkdir -p "$TMP/home" "$TMP/locks"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"

# Locks for state under the temp root stay under the temp root (a per-user
# directory the system clears), not in the per-user cache that suites run
# outside check.sh used to fill with empty flock files; production state
# under HOME keeps the cache directory, and a planted symlink or a directory
# someone else owns is never opened through.
test_temp_state_locks_under_the_temp_root() {
  local home="$TMP/home" tmp_root="$TMP/tmp-root" lock
  mkdir -p "$home" "$tmp_root"
  lock="$(cd "$tmp_root" && HOME="$home" TMPDIR="$tmp_root" bash -c '
    unset OMS_LOCK_DIR
    . "$1/scripts/lib/file-lock.sh"
    oms_file_lock_path_for_file "$TMPDIR/fixture/.oms/plan/tasks.json"' _ "$ROOT")"
  case "$lock" in
    "$tmp_root/oh-my-setting-locks-"*/tasks.json.*.lock) ;;
    *) fail "temp-root state must lock under the temp root, got $lock" ;;
  esac
  [ -d "$tmp_root/oh-my-setting-locks-$(id -u)" ] ||
    fail "the temp lock directory was not created"
  lock="$(HOME="$home" TMPDIR="$tmp_root" bash -c '
    unset OMS_LOCK_DIR
    . "$1/scripts/lib/file-lock.sh"
    oms_file_lock_path_for_file "$HOME/project/.oms/plan/tasks.json"' _ "$ROOT")"
  case "$lock" in
    "$home/.cache/oh-my-setting/locks/"tasks.json.*.lock) ;;
    *) fail "state under HOME must keep the cache lock directory, got $lock" ;;
  esac
  rm -rf "$tmp_root/oh-my-setting-locks-$(id -u)"
  ln -s "$TMP/elsewhere" "$tmp_root/oh-my-setting-locks-$(id -u)"
  lock="$(HOME="$home" TMPDIR="$tmp_root" bash -c '
    unset OMS_LOCK_DIR
    . "$1/scripts/lib/file-lock.sh"
    oms_file_lock_path_for_file "$TMPDIR/fixture/.oms/plan/tasks.json"' _ "$ROOT")"
  case "$lock" in
    "$home/.cache/oh-my-setting/locks/"*) ;;
    *) fail "a planted symlink lock directory must fall back to the cache, got $lock" ;;
  esac
  rm -f "$tmp_root/oh-my-setting-locks-$(id -u)"
  lock="$(HOME="$home" TMPDIR="$tmp_root" OMS_LOCK_DIR="$TMP/explicit" bash -c '
    . "$1/scripts/lib/file-lock.sh"
    oms_file_lock_path_for_file "$TMPDIR/fixture/.oms/plan/tasks.json"' _ "$ROOT")"
  case "$lock" in
    "$TMP/explicit/"*) ;;
    *) fail "OMS_LOCK_DIR must still win for temp-root state, got $lock" ;;
  esac
}
export OMS_LOCK_FORCE_MKDIR=1

# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

test_old_live_holder_is_not_reclaimed() {
  local state="$TMP/live/state"
  local lock_dir marker rc=0

  mkdir -p "$(dirname "$state")"
  lock_dir="$(oms_file_lock_path_for_file "$state")"
  mkdir -p "$lock_dir"
  printf '%s\n' "$$" > "$lock_dir/pid"
  printf '1\n' > "$lock_dir/started"
  printf 'live-generation\n' > "$lock_dir/owner"

  marker="$TMP/live-entered"
  OMS_LOCK_TIMEOUT=1 oms_try_file_lock "$state" touch "$marker" || rc=$?
  [ "$rc" = 75 ] || fail "an old but live holder was reclaimed (status $rc)"
  [ ! -e "$marker" ] || fail "a contender entered an old live holder's critical section"
}

test_reused_pid_token_is_reclaimed_when_supported() {
  local state="$TMP/reused/state"
  local lock_dir marker start_identity

  start_identity="$(oms_file_lock_process_start_token "$$" 2>/dev/null || true)"
  [ -n "$start_identity" ] || return 0

  mkdir -p "$(dirname "$state")"
  lock_dir="$(oms_file_lock_path_for_file "$state")"
  mkdir -p "$lock_dir"
  printf '%s\n' "$$" > "$lock_dir/pid"
  printf '%s\n' "not-$start_identity" > "$lock_dir/process-start"
  printf '%s\n' "$(date +%s)" > "$lock_dir/started"
  printf 'reused-generation\n' > "$lock_dir/owner"

  marker="$TMP/reused-entered"
  OMS_LOCK_TIMEOUT=2 oms_try_file_lock "$state" touch "$marker" ||
    fail "a lock whose PID start token changed was not reclaimed"
  [ -e "$marker" ] || fail "the reused-PID contender did not enter"
}

test_bash32_fallback_records_the_holder_process() {
  local state="$TMP/bash32/state"
  local lock_dir holder_pid launcher marker i=0

  mkdir -p "$(dirname "$state")"
  lock_dir="$(oms_file_lock_path_for_file "$state")"

  fallback_hold() {
    : > "$TMP/bash32-ready"
    while [ ! -e "$TMP/bash32-release" ]; do
      sleep 0.02
    done
  }

  (
    # Model stock Bash 3.2, where BASHPID is absent and $$ remains the parent
    # value inside (...). The fallback must probe this lock-owning subshell.
    unset BASHPID
    OMS_LOCK_TIMEOUT=2 oms_with_file_lock "$state" fallback_hold
  ) 2>"$TMP/bash32-holder.err" &
  launcher=$!
  while [ ! -e "$TMP/bash32-ready" ] && kill -0 "$launcher" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 500 ] || break
    sleep 0.01
  done
  [ -e "$TMP/bash32-ready" ] || {
    : > "$TMP/bash32-release"
    wait "$launcher" || true
    fail "Bash 3.2 fallback holder did not acquire the lock"
  }
  holder_pid="$(sed -n '1p' "$lock_dir/pid")"
  if [ "$holder_pid" = "$$" ]; then
    : > "$TMP/bash32-release"
    wait "$launcher" || true
    fail "Bash 3.2 fallback recorded the living parent instead of its holder"
  fi

  kill -KILL "$holder_pid" 2>/dev/null ||
    fail "could not kill the recorded Bash 3.2 fallback holder"
  wait "$launcher" 2>/dev/null || true
  marker="$TMP/bash32-reclaimed"
  OMS_LOCK_TIMEOUT=2 oms_try_file_lock "$state" touch "$marker" ||
    fail "a killed Bash 3.2 fallback holder was not reclaimable"
  [ -e "$marker" ] || fail "the Bash 3.2 recovery contender did not enter"
}

test_crashed_reclaimer_does_not_wedge_the_generation() {
  local state="$TMP/reclaimer-crash/state"
  local lock_dir generation generation_sum marker

  mkdir -p "$(dirname "$state")"
  lock_dir="$(oms_file_lock_path_for_file "$state")"
  mkdir -p "$lock_dir"
  printf '999999999\n' > "$lock_dir/pid"
  printf '1\n' > "$lock_dir/started"
  printf 'crashed-reclaimer-generation\n' > "$lock_dir/owner"

  generation='owner:crashed-reclaimer-generation'
  generation_sum="$(printf '%s' "$generation" | cksum | awk '{print $1 "-" $2}')"
  # This is the durable residue left if the former generation-guard owner was
  # killed between election and rename. It must not block all future recovery.
  mkdir "$lock_dir.reclaim.$generation_sum"
  mkdir -p "$lock_dir.reclaim-queue/999999999.none.1.1"
  printf '0\n' > "$lock_dir.reclaim-queue/999999999.none.1.1/choosing"
  printf '1\n' > "$lock_dir.reclaim-queue/999999999.none.1.1/ticket"
  printf '%s\n' "$generation" > \
    "$lock_dir.reclaim-queue/999999999.none.1.1/generation"

  marker="$TMP/reclaimer-crash-entered"
  OMS_LOCK_TIMEOUT=2 oms_try_file_lock "$state" touch "$marker" ||
    fail "a crashed stale reclaimer permanently wedged its lock generation"
  [ -e "$marker" ] || fail "the post-crash reclaimer did not enter"
}

test_two_stale_contenders_do_not_reclaim_the_winner() {
  local state="$TMP/race/state"
  local lock_dir real_mv p1 p2

  mkdir -p "$(dirname "$state")" "$TMP/race-bin" "$TMP/race-mv-ready"
  lock_dir="$(oms_file_lock_path_for_file "$state")"
  mkdir -p "$lock_dir"
  printf '999999999\n' > "$lock_dir/pid"
  printf '1\n' > "$lock_dir/started"
  printf 'stale-generation\n' > "$lock_dir/owner"

  real_mv="$(command -v mv)"
  cat > "$TMP/race-bin/mv" <<'EOF'
#!/usr/bin/env bash
set -eu
touch "$OMS_TEST_MV_READY/$OMS_TEST_ROLE"
i=0
while [ "$(find "$OMS_TEST_MV_READY" -type f 2>/dev/null | wc -l | tr -d ' ')" -lt 2 ] && [ "$i" -lt 30 ]; do
  i=$((i + 1))
  sleep 0.01
done
[ "$OMS_TEST_ROLE" != B ] || sleep 0.20
exec "$OMS_TEST_REAL_MV" "$@"
EOF
  chmod +x "$TMP/race-bin/mv"

  cat > "$TMP/critical.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
if mkdir "$1" 2>/dev/null; then
  sleep 0.50
  rmdir "$1"
else
  : > "$2"
fi
EOF
  chmod +x "$TMP/critical.sh"

  run_contender() {
    local role="$1"
    OMS_TEST_ROLE="$role" OMS_TEST_REAL_MV="$real_mv" \
      OMS_TEST_MV_READY="$TMP/race-mv-ready" \
      PATH="$TMP/race-bin:$PATH" OMS_LOCK_TIMEOUT=3 \
      oms_with_file_lock "$state" "$TMP/critical.sh" \
        "$TMP/critical" "$TMP/overlap"
  }

  run_contender A &
  p1=$!
  run_contender B &
  p2=$!
  wait "$p1" || fail "stale contender A failed"
  wait "$p2" || fail "stale contender B failed"
  [ ! -e "$TMP/overlap" ] ||
    fail "a stale observer renamed the new winner's live lock"
}

test_old_live_holder_is_not_reclaimed
test_reused_pid_token_is_reclaimed_when_supported
test_bash32_fallback_records_the_holder_process
test_crashed_reclaimer_does_not_wedge_the_generation
test_two_stale_contenders_do_not_reclaim_the_winner

test_temp_state_locks_under_the_temp_root
echo "file-lock-boundary-smoke: ok"
