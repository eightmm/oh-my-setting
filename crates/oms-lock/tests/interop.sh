#!/usr/bin/env bash
# Development oracle only; all state and locks are synthetic and temporary.
set -euo pipefail
native="$1"
repo="$2"
work="$(mktemp -d "${TMPDIR:-/tmp}/oms-native-interop.XXXXXX")"
children=()
cleanup() {
  local child
  for child in "${children[@]}"; do kill "$child" 2>/dev/null || true; done
  for child in "${children[@]}"; do wait "$child" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT
umask 077
mkdir "$work/home" "$work/locks"
: > "$work/synthetic-fixture"
export HOME="$work/home" OMS_LOCK_DIR="$work/locks"
# shellcheck source=scripts/lib/file-lock.sh
. "$repo/scripts/lib/file-lock.sh"
wait_ready() {
  local path="$1" start="$SECONDS"
  until [ -s "$path" ]; do
    [ "$((SECONDS - start))" -lt "${OMS_TEST_CALL_TIMEOUT:-30}" ] || { echo "fixture wait timed out" >&2; return 1; }
    sleep 0.01
  done
}
for mode in flock mkdir; do
  if [ "$mode" = flock ]; then
    command -v flock >/dev/null || { echo 'flock oracle unavailable' >&2; exit 1; }
    export OMS_LOCK_FORCE_MKDIR=0
  else
    export OMS_LOCK_FORCE_MKDIR=1
  fi
  state="$work/state-$mode"
  # Exact checksum/path oracle, including non-UTF8, newlines and separators.
  for spelling in "$state" "$work//state-$mode/" "$work/space $(printf '\377')
state"; do
    expected="$(oms_file_lock_path_for_file "$spelling")"
    actual="$("$native" "$work" "$mode" "$spelling" path)"
    [ "$actual" = "$expected" ] || { echo 'path authority mismatch' >&2; exit 1; }
  done
  mkfifo "$work/native.in"
  exec 3<> "$work/native.in"
  "$native" "$work" "$mode" "$state" hold < "$work/native.in" > "$work/native.out" &
  holder=$!; children+=("$holder")
  wait_ready "$work/native.out"
  status=0
  oms_try_file_lock "$state" true || status=$?
  [ "$status" = 75 ] || { echo 'Bash entered native critical section' >&2; exit 1; }
  printf '\n' >&3
  wait "$holder"
  exec 3>&-
  rm "$work/native.in" "$work/native.out"
  mkfifo "$work/legacy.in"
  exec 4<> "$work/legacy.in"
  hold_legacy() { printf 'ready\n' > "$work/legacy.ready"; IFS= read -r _ < "$work/legacy.in"; }
  oms_with_file_lock "$state" hold_legacy &
  holder=$!; children+=("$holder")
  wait_ready "$work/legacy.ready"
  status=0
  "$native" "$work" "$mode" "$state" try 2> "$work/busy.err" || status=$?
  [ "$status" = 75 ] || { echo 'native entered Bash critical section' >&2; exit 1; }
  printf '\n' >&4
  wait "$holder"
  exec 4>&-
  rm "$work/legacy.in" "$work/legacy.ready"
  # A SIGKILLed native holder leaves recoverable protocol residue.
  mkfifo "$work/crash.in"
  exec 5<> "$work/crash.in"
  "$native" "$work" "$mode" "$state" hold < "$work/crash.in" > "$work/crash.out" &
  crashed=$!; children+=("$crashed")
  wait_ready "$work/crash.out"
  kill -KILL "$crashed"
  wait "$crashed" 2>/dev/null || true
  exec 5>&-
  oms_try_file_lock "$state" true
  rm "$work/crash.in" "$work/crash.out"
  printf '0' > "$state"
  if [ "$mode" = mkdir ]; then
    stale_path="$(oms_file_lock_path_for_file "$state")"
    mkdir -m 700 "$stale_path"
    printf 'mixed-stale-generation\n' > "$stale_path/owner"
    printf '2147483647\n' > "$stale_path/pid"
    printf '1\n' > "$stale_path/started"
  fi
  "$native" "$work" "$mode" "$state" increment &
  first=$!; children+=("$first")
  increment_legacy() {
    local count
    if ! mkdir "$work/critical"; then : > "$work/overlap"; return 1; fi
    count="$(cat "$state")"
    printf '%s' "$((count + 1))" > "$state"
    rmdir "$work/critical"
  }
  for ((i=0; i<20; i++)); do oms_with_file_lock "$state" increment_legacy; done
  wait "$first"
  [ "$(cat "$state")" = 40 ] && [ ! -e "$work/overlap" ] || { echo 'mixed writer overlap/lost update' >&2; exit 1; }
  printf 'interop %s: bidirectional exclusion and 40 serialized updates passed\n' "$mode"
done
