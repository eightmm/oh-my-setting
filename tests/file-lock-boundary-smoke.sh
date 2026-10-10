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
  # A fixture under /tmp outside TMPDIR (a suite's sibling state directory)
  # still locks under the temp root, not in the cache.
  lock="$(HOME="$home" TMPDIR="$tmp_root" bash -c '
    unset OMS_LOCK_DIR
    . "$1/scripts/lib/file-lock.sh"
    oms_file_lock_path_for_file "/tmp/oms-lock-sibling-$$/state/tasks.json"' _ "$ROOT")"
  case "$lock" in
    /tmp/oh-my-setting-locks-"$(id -u)"/tasks.json.*.lock) ;;
    *) fail "a /tmp fixture outside TMPDIR must lock under /tmp, got $lock" ;;
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

test_observation_routing_matches_acquisition_without_mutation() {
  local home="$TMP/routing-home" tmp_root="$TMP/routing-tmp" sibling="$TMP/routing-sibling"
  local unsafe_root="$TMP/routing-unsafe" missing_root="$TMP/routing-missing"
  local explicit="$TMP/routing-explicit" state observed before after
  mkdir -p "$home" "$tmp_root" "$sibling" "$unsafe_root" "$explicit"
  printf 'preserve me\n' > "$tmp_root/state"
  printf 'sibling state\n' > "$sibling/state"
  chmod 640 "$tmp_root/state" "$sibling/state"
  ln -s "$TMP/elsewhere" "$unsafe_root/oh-my-setting-locks-$(id -u)"

  check_route() {
    local env_root="$1" env_state="$2" expected_kind="$3" env_override="${4:-}"
    local acquisition observation
    acquisition="$(HOME="$home" TMPDIR="$env_root" bash -c '
      . "$1/scripts/lib/file-lock.sh"
      unset OMS_LOCK_DIR
      [ -z "$3" ] || OMS_LOCK_DIR="$3"
      oms_file_lock_path_for_file "$2"' _ "$ROOT" "$env_state" "$env_override")"
    observation="$(HOME="$home" TMPDIR="$env_root" bash -c '
      . "$1/scripts/lib/file-lock.sh"
      unset OMS_LOCK_DIR
      [ -z "$3" ] || OMS_LOCK_DIR="$3"
      oms_file_lock_path_for_observation "$2"' _ "$ROOT" "$env_state" "$env_override")"
    [ "$acquisition" = "$observation" ] || fail "observation route differs from acquisition: $observation != $acquisition"
    case "$expected_kind:$observation" in
      explicit:"$env_override"/*) ;;
      tmp:"$env_root/oh-my-setting-locks-"*/*) ;;
      sibling:"/tmp/oh-my-setting-locks-"*/*) ;;
      cache:"$home/.cache/oh-my-setting/locks/"*) ;;
      *) fail "unexpected $expected_kind route: $observation" ;;
    esac
  }
  check_route "$tmp_root" "$tmp_root/state" tmp
  check_route "$tmp_root" "$sibling/state" sibling
  check_route "$tmp_root" "$home/project/state" cache
  check_route "$tmp_root" "$tmp_root/state" explicit "$explicit"
  check_route "$unsafe_root" "$unsafe_root/state" cache
  check_route "$missing_root" "$missing_root/state" cache

  rm -rf "$tmp_root/oh-my-setting-locks-$(id -u)"
  state="$tmp_root/state"
  before="$(cksum "$state") $(stat -c '%a' "$state" 2>/dev/null || stat -f '%Lp' "$state")"
  observed="$(HOME="$home" TMPDIR="$tmp_root" OMS_LOCK_FORCE_MKDIR=1 bash -c '
    . "$1/scripts/lib/file-lock.sh"; oms_file_lock_probe "$2"' _ "$ROOT" "$state")"
  [ "$observed" = free ] || fail "unresolved-but-safe missing lock should be free, got $observed"
  after="$(cksum "$state") $(stat -c '%a' "$state" 2>/dev/null || stat -f '%Lp' "$state")"
  [ "$before" = "$after" ] || fail "observation changed fixture bytes or mode"
  [ ! -e "$tmp_root/oh-my-setting-locks-$(id -u)" ] ||
    fail "observation created the temp-root lock directory"

  observed="$(HOME="$home" TMPDIR="$unsafe_root" OMS_LOCK_DIR="$TMP/no-parent/locks" OMS_LOCK_FORCE_MKDIR=1 bash -c '
    . "$1/scripts/lib/file-lock.sh"; oms_file_lock_probe "$2"' _ "$ROOT" "$unsafe_root/state")"
  [ "$observed" = free ] || fail "safe cache fallback missing lock should be free, got $observed"
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


test_flock_observation_is_conservative_and_read_only() {
  local state="$TMP/flock/state" lock_path proc_fixture key result real_python holder_pid attempts
  local saved_lock_dir="${OMS_LOCK_DIR:-}"
  mkdir -p "$TMP/flock" "$TMP/proc-bin" "$TMP/observe-tmp"
  unset OMS_LOCK_FORCE_MKDIR
  OMS_LOCK_DIR="$TMP/flock-locks"
  mkdir -p "$OMS_LOCK_DIR"
  lock_path="$(oms_file_lock_path_for_file "$state")"
  : > "$lock_path"
  proc_fixture="$TMP/proc-locks"
  : > "$proc_fixture"
  real_python="$(command -v python3)"
  key="$(python3 - "$lock_path" <<'PY'
import os, sys
st = os.stat(sys.argv[1])
print("%02x:%02x:%d" % (os.major(st.st_dev), os.minor(st.st_dev), st.st_ino))
PY
)"
  cat > "$TMP/proc-bin/python3" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "-" ] && [ -n "${OMS_TEST_PROC_LOCKS:-}" ]; then
  shift
  sed 's|open("/proc/locks"|open(os.environ["OMS_TEST_PROC_LOCKS"]|' |
    "$OMS_TEST_REAL_PYTHON" - "$@"
else
  exec "$OMS_TEST_REAL_PYTHON" "$@"
fi
EOF
  chmod +x "$TMP/proc-bin/python3"
  result="$(PATH="$TMP/proc-bin:$PATH" OMS_TEST_REAL_PYTHON="$real_python" \
    OMS_TEST_PROC_LOCKS="$proc_fixture" oms_file_lock_probe "$state")"
  [ "$result" = unknown ] || fail "an empty flock view must be unknown, got $result"
  cat > "$TMP/flock/leave-holder.sh" <<EOF
#!/bin/sh
sleep 1 >/dev/null 2>&1 &
echo \$! > "$TMP/flock/holder.pid"
EOF
  chmod +x "$TMP/flock/leave-holder.sh"
  oms_try_file_lock "$state" "$TMP/flock/leave-holder.sh" ||
    fail "the flock helper could not launch its inheriting holder"
  holder_pid="$(sed -n '1p' "$TMP/flock/holder.pid")"
  kill -0 "$holder_pid" 2>/dev/null || fail "the post-helper lock holder already exited"
  printf '17: FLOCK ADVISORY WRITE 99 %s\n' "$key" > "$proc_fixture"
  result="$(PATH="$TMP/proc-bin:$PATH" OMS_TEST_REAL_PYTHON="$real_python" \
    OMS_TEST_PROC_LOCKS="$proc_fixture" oms_file_lock_probe "$state")"
  [ "$result" = held ] || fail "a matching current kernel row must prove held, got $result"
  attempts=0
  while kill -0 "$holder_pid" 2>/dev/null && [ "$attempts" -lt 50 ]; do
    attempts=$((attempts + 1))
    sleep 0.05
  done
  kill -0 "$holder_pid" 2>/dev/null && fail "the test holder did not release its lock"
  printf '17: FLOCK ADVISORY WRITE 99 00:00:1\n' > "$proc_fixture"
  result="$(PATH="$TMP/proc-bin:$PATH" OMS_TEST_REAL_PYTHON="$real_python" \
    OMS_TEST_PROC_LOCKS="$proc_fixture" oms_file_lock_probe "$state")"
  [ "$result" = unknown ] || fail "an unrelated flock row must be unknown, got $result"
  result="$(PATH="$TMP/proc-bin:$PATH" OMS_TEST_REAL_PYTHON="$real_python" \
    OMS_TEST_PROC_LOCKS="$TMP/no-such-proc-view" oms_file_lock_probe "$state")"
  [ "$result" = unknown ] || fail "an unreadable flock view must be unknown, got $result"

  rm -f "$lock_path"
  result="$(oms_file_lock_probe "$state")"
  [ "$result" = free ] || fail "a genuinely missing lock path must be free, got $result"
  ln -s "$TMP/no-such-lock-target" "$lock_path"
  result="$(oms_file_lock_probe "$state")"
  [ "$result" = unknown ] || fail "a lock path with a stat error must be unknown, got $result"
  result="$(OMS_LOCK_FORCE_MKDIR=1 oms_file_lock_probe "$state")"
  [ "$result" = unknown ] || fail "a broken mkdir-lock path must be unknown, got $result"
  rm -f "$lock_path"

  # Resolving a temp-root observation must not create its per-user lock dir.
  unset OMS_LOCK_DIR
  result="$(HOME="$TMP/home" TMPDIR="$TMP/observe-tmp" \
    bash -c '. "$1/scripts/lib/file-lock.sh"; oms_file_lock_probe "$2"' \
      _ "$ROOT" "$TMP/observe-tmp/state")"
  [ "$result" = free ] || fail "a missing temp-root lock must be free, got $result"
  [ ! -e "$TMP/observe-tmp/oh-my-setting-locks-$(id -u)" ] ||
    fail "observation created the temp-root lock directory"
  OMS_LOCK_DIR="$saved_lock_dir"
  export OMS_LOCK_DIR
  export OMS_LOCK_FORCE_MKDIR=1
}

test_old_live_holder_is_not_reclaimed
test_reused_pid_token_is_reclaimed_when_supported
test_bash32_fallback_records_the_holder_process
test_crashed_reclaimer_does_not_wedge_the_generation
test_two_stale_contenders_do_not_reclaim_the_winner
if command -v flock >/dev/null 2>&1; then
  test_flock_observation_is_conservative_and_read_only
fi
test_temp_state_locks_under_the_temp_root
test_observation_routing_matches_acquisition_without_mutation
echo "file-lock-boundary-smoke: ok"
