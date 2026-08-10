#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-lifecycle-lock.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "install-lifecycle-lock-smoke: $*" >&2
  exit 1
}

export OMS_INSTALL_LIFECYCLE_LOCK="$TMP/locks/install-lifecycle.lock.d"
export OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT=1

# shellcheck source=scripts/lib/install-lifecycle-lock.sh
. "$ROOT/scripts/lib/install-lifecycle-lock.sh"

exercise_subshell_boundary() {
  local identity_mode="$1"
  local child_status=0
  local child_log="$TMP/child-acquire.log"
  local lock

  oms_install_lifecycle_lock_acquire "test parent ownership ($identity_mode)" ||
    fail "parent could not acquire the lifecycle lock ($identity_mode)"
  lock="$OMS_INSTALL_LIFECYCLE_LOCK_PATH"
  [ -d "$lock" ] || fail "acquire did not create the lifecycle lock ($identity_mode)"

  # Bash keeps $$ unchanged in a (...) subshell. Process ownership must use the
  # actual executing shell identity, or the child can silently adopt the parent
  # lock and enter the supposedly exclusive mutation section.
  ( oms_install_lifecycle_lock_acquire "test inherited ownership ($identity_mode)" ) \
    > "$child_log" 2>&1 || child_status=$?
  [ "$child_status" = 75 ] ||
    fail "a subshell adopted its parent's lock ($identity_mode, status $child_status): $(cat "$child_log")"
  [ -d "$lock" ] ||
    fail "the failed child acquisition removed the parent lock ($identity_mode)"

  # The same inherited locals previously let a child run the parent's release
  # path. Releasing from a different process must be a no-op.
  ( oms_install_lifecycle_lock_release )
  [ -d "$lock" ] ||
    fail "a subshell released its parent's live lock ($identity_mode)"

  oms_install_lifecycle_lock_release
  [ ! -e "$lock" ] ||
    fail "the owning process could not release its lock ($identity_mode)"
}

exercise_subshell_boundary "BASHPID"

# The standalone installer deliberately execs the selected checkout while
# holding this lock. Unlike a subshell, that handoff keeps the same process and
# must still adopt and release the lock successfully.
cat > "$TMP/exec-owner.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_acquire "test exec owner"
exec bash "$OMS_TEST_LOCK_ADOPTER"
EOF
cat > "$TMP/exec-adopter.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_acquire "test exec adoption"
oms_install_lifecycle_lock_release
EOF
OMS_TEST_LOCK_HELPER="$ROOT/scripts/lib/install-lifecycle-lock.sh" \
  OMS_TEST_LOCK_ADOPTER="$TMP/exec-adopter.sh" bash "$TMP/exec-owner.sh" ||
  fail "an exec handoff could not adopt and release its own lock"
[ ! -e "$OMS_INSTALL_LIFECYCLE_LOCK" ] ||
  fail "the exec adoption path leaked its lifecycle lock"

# Stock macOS Bash 3.2 has no BASHPID, so identity falls back to a child
# probe. A probe measured through a substitution fork returns a different,
# already-dead pid on every call: adopt and release then never match, the
# installer stale-takes-over its own live lock mid-run, and the exit path
# leaves the lock behind (the 2026-08-10 macOS e2e leak). `unset BASHPID` is
# sticky for a whole modern-Bash session, so a child script exercises the
# fallback on every host.
cat > "$TMP/fallback-identity.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
unset BASHPID
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_current_identity || exit 75
first="$OMS_INSTALL_LIFECYCLE_CURRENT_PID"
oms_install_lifecycle_lock_current_identity || exit 75
second="$OMS_INSTALL_LIFECYCLE_CURRENT_PID"
[ "$first" = "$second" ] || { echo "identity unstable: $first vs $second" >&2; exit 1; }
[ "$first" = "$$" ] || { echo "identity is not this shell: $first vs $$" >&2; exit 1; }
EOF
OMS_TEST_LOCK_HELPER="$ROOT/scripts/lib/install-lifecycle-lock.sh" \
  bash "$TMP/fallback-identity.sh" ||
  fail "the Bash 3.2 identity fallback is not a stable shell identity"

cat > "$TMP/fallback-owner.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
unset BASHPID
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_acquire "test fallback exec owner"
exec bash "$OMS_TEST_LOCK_ADOPTER"
EOF
cat > "$TMP/fallback-adopter.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
unset BASHPID
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_acquire "test fallback exec adoption"
oms_install_lifecycle_lock_release
EOF
OMS_TEST_LOCK_HELPER="$ROOT/scripts/lib/install-lifecycle-lock.sh" \
  OMS_TEST_LOCK_ADOPTER="$TMP/fallback-adopter.sh" bash "$TMP/fallback-owner.sh" ||
  fail "the fallback exec handoff could not adopt and release its own lock"
[ ! -e "$OMS_INSTALL_LIFECYCLE_LOCK" ] ||
  fail "the fallback exec adoption path leaked its lifecycle lock"

exercise_reclaim_gate() {
  local mode="$1" driver="$2"
  local barrier="$TMP/$mode-barrier"
  local hold="$TMP/$mode-hold"
  local first_ready="$TMP/$mode-first-acquired"
  local second_ready="$TMP/$mode-second-acquired"
  local first_pid second_pid second_status=0 owner_before owner_after

  mkdir -p "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead"
  printf '%s\n' 99999999 > \
    "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead/pid"
  printf '%s\n' stale > \
    "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead/pid-start"
  printf '%s\n' 1 > \
    "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead/started"
  printf '%s\n' dead-claim > \
    "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead/owner"
  mkdir -p "$OMS_INSTALL_LIFECYCLE_LOCK"
  printf '%s\n' 99999999 > "$OMS_INSTALL_LIFECYCLE_LOCK/pid"
  printf '%s\n' stale > "$OMS_INSTALL_LIFECYCLE_LOCK/pid-start"
  printf '%s\n' 1 > "$OMS_INSTALL_LIFECYCLE_LOCK/started"
  printf '%s\n' stale-owner > "$OMS_INSTALL_LIFECYCLE_LOCK/owner"

  OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER="$barrier" \
    OMS_TEST_LOCK_ACQUIRED="$first_ready" OMS_TEST_LOCK_HOLD="$hold" \
    OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT=3 bash "$driver" \
    >"$TMP/$mode-first.log" 2>&1 &
  first_pid=$!
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -e "$barrier.ready" ] && break
    sleep 1
  done
  [ -e "$barrier.ready" ] || {
    kill "$first_pid" 2>/dev/null || true
    fail "$mode contender never reached the stale-reclaim barrier"
  }
  [ ! -e "$OMS_INSTALL_LIFECYCLE_LOCK.recovery-claims.d/1.99999999.dead" ] ||
    fail "$mode did not reclaim a dead unique recovery claim"

  OMS_TEST_INSTALL_LIFECYCLE_STALE_BARRIER="$barrier" \
    OMS_TEST_LOCK_ACQUIRED="$second_ready" OMS_TEST_LOCK_HOLD="$hold-second" \
    OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT=2 bash "$driver" \
    >"$TMP/$mode-second.log" 2>&1 &
  second_pid=$!
  : > "$barrier.release"
  for _ in 1 2 3 4 5; do
    [ -e "$first_ready" ] && break
    sleep 1
  done
  [ -e "$first_ready" ] || {
    kill "$first_pid" "$second_pid" 2>/dev/null || true
    fail "$mode first contender did not acquire after reclaim"
  }
  owner_before="$(sed -n '1p' "$OMS_INSTALL_LIFECYCLE_LOCK/owner")"
  wait "$second_pid" || second_status=$?
  [ "$second_status" = 75 ] || {
    : > "$hold"
    wait "$first_pid" 2>/dev/null || true
    fail "$mode second contender crossed the recovery gate (status $second_status)"
  }
  [ ! -e "$second_ready" ] || fail "$mode second contender entered the critical section"
  owner_after="$(sed -n '1p' "$OMS_INSTALL_LIFECYCLE_LOCK/owner")"
  [ "$owner_after" = "$owner_before" ] ||
    fail "$mode stale contender moved the newly acquired live lock"

  : > "$hold"
  wait "$first_pid" || fail "$mode first contender failed after holding its lock"
  [ ! -e "$OMS_INSTALL_LIFECYCLE_LOCK" ] ||
    fail "$mode first contender leaked its lifecycle lock"
}

cat > "$TMP/helper-contender.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$OMS_TEST_LOCK_HELPER"
oms_install_lifecycle_lock_acquire "barrier contender"
printf '%s\n' acquired > "$OMS_TEST_LOCK_ACQUIRED"
while [ ! -e "$OMS_TEST_LOCK_HOLD" ]; do sleep 1; done
oms_install_lifecycle_lock_release
EOF
OMS_TEST_LOCK_HELPER="$ROOT/scripts/lib/install-lifecycle-lock.sh"
export OMS_TEST_LOCK_HELPER
exercise_reclaim_gate helper "$TMP/helper-contender.sh"

# The curl/pipe installer carries an inline copy of the lock before cloning.
# Exercise that exact source by truncating only the later install work.
sed -n '1,/^oms_install_lifecycle_lock_acquire install || exit \$?$/p' \
  "$ROOT/install.sh" > "$TMP/standalone-contender.sh"
cat >> "$TMP/standalone-contender.sh" <<'EOF'
printf '%s\n' acquired > "$OMS_TEST_LOCK_ACQUIRED"
while [ ! -e "$OMS_TEST_LOCK_HOLD" ]; do sleep 1; done
oms_install_lifecycle_lock_release
trap - EXIT HUP INT TERM
EOF
exercise_reclaim_gate standalone "$TMP/standalone-contender.sh"

# When the host exposes a start token, a live PID alone is not enough: it may
# have been reused since a crashed owner wrote the lock. Model that state with
# this process's live PID and a different recorded start token.
oms_install_lifecycle_lock_current_identity ||
  fail "could not probe the current process identity"
if [ -n "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ]; then
  mkdir -p "$OMS_INSTALL_LIFECYCLE_LOCK"
  printf '%s\n' "$OMS_INSTALL_LIFECYCLE_CURRENT_PID" \
    > "$OMS_INSTALL_LIFECYCLE_LOCK/pid"
  printf '%s\n' "stale:$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" \
    > "$OMS_INSTALL_LIFECYCLE_LOCK/pid-start"
  printf '%s\n' "$(date +%s)" > "$OMS_INSTALL_LIFECYCLE_LOCK/started"
  printf '%s\n' "reused-pid-owner" > "$OMS_INSTALL_LIFECYCLE_LOCK/owner"
  oms_install_lifecycle_lock_acquire "test reused PID" ||
    fail "a reused PID with the wrong start token kept a stale lock live"
  [ "$(sed -n '1p' "$OMS_INSTALL_LIFECYCLE_LOCK/pid-start")" = \
    "$OMS_INSTALL_LIFECYCLE_CURRENT_PID_START" ] ||
    fail "reclaimed lock did not record the current process start token"
  oms_install_lifecycle_lock_release
fi

# Stock Bash 3.2 has no BASHPID. Exercise the PPID probe used by that supported
# runtime, not just the newer-Bash branch used on this Linux host.
unset BASHPID
exercise_subshell_boundary "Bash 3.2 fallback"

echo "install-lifecycle-lock-smoke: ok"
