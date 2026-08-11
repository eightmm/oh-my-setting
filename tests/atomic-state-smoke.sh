#!/usr/bin/env bash
set -euo pipefail

# Shared-state writers must replace files with a same-directory rename. The
# scratch file used to live under OMS_LIB_TMPDIR, so whenever that was a
# different filesystem from the repo (tmpfs /tmp against an ext4 checkout —
# the ordinary case), `mv` degraded to copy+unlink and every concurrent
# reader had a window where the task packet was empty or truncated.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-atomic-state.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/locks"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# shellcheck source=scripts/lib/agent-task-common.sh
. "$ROOT/scripts/lib/agent-task-common.sh"

# The contract: replace scratch lands in the target's own directory even when
# OMS_LIB_TMPDIR points somewhere else entirely.
mkdir -p "$TMP/elsewhere" "$TMP/repo/.oms/task"
export OMS_LIB_TMPDIR="$TMP/elsewhere"
scratch="$(agent_memory_mktemp_beside "$TMP/repo/.oms/task/current.md")"
case "$scratch" in
  "$TMP/repo/.oms/task"/.oms-replace.*) ;;
  *) fail "replace scratch landed outside the target directory: $scratch" ;;
esac
rm -f "$scratch"

# A real write path round-trips and leaves no scratch behind.
file="$TMP/repo/.oms/task/current.md"
agent_task_init_file "$file"
agent_task_set_metadata "$file" status verified
[ "$(agent_task_metadata_value "$file" status)" = verified ] ||
  fail "metadata write lost the value"
leftovers="$(find "$TMP/repo/.oms/task" -name '.oms-replace.*' | wc -l | tr -d ' ')"
[ "$leftovers" = 0 ] || fail "replace scratch leaked into the state directory"

# Canary, not proof (same filesystem here): a reader polling during a burst of
# writes must never observe an empty task file.
(
  i=1
  while [ "$i" -le 100 ]; do
    agent_task_set_metadata "$file" status "s$i" >/dev/null
    i=$((i + 1))
  done
) &
writer=$!
empty=0
i=1
while [ "$i" -le 300 ]; do
  [ -s "$file" ] || empty=1
  i=$((i + 1))
done
wait "$writer"
[ "$empty" = 0 ] || fail "a reader observed an empty task file during writes"

# The derived memory summary has unlocked prompt readers, so its refresh must
# also land as one same-directory rename: truncate-then-append exposed a
# header-only summary to any reader racing the refresh. Canary, not proof,
# like the task-file leg above.
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
mkdir -p "$TMP/repo/.oms/memory"
memory_file="$TMP/repo/.oms/memory/shared.md"
i=1
while [ "$i" -le 200 ]; do
  printf '## note-%s\nbody line %s for the summary canary\n\n' "$i" "$i"
  i=$((i + 1))
done > "$memory_file"
summary_file="$(agent_memory_summary_file "$memory_file")"
agent_memory_refresh_summary "$memory_file" project >/dev/null 2>&1 ||
  fail "summary refresh failed"
grep -q '^- ' "$summary_file" || fail "summary refresh produced no body"
(
  i=1
  while [ "$i" -le 60 ]; do
    agent_memory_refresh_summary "$memory_file" project >/dev/null 2>&1
    i=$((i + 1))
  done
) &
summary_writer=$!
summary_partial=0
i=1
while [ "$i" -le 400 ]; do
  if [ -s "$summary_file" ] && ! grep -q '^- ' "$summary_file"; then
    summary_partial=1
  fi
  [ -s "$summary_file" ] || summary_partial=1
  i=$((i + 1))
done
wait "$summary_writer"
[ "$summary_partial" = 0 ] ||
  fail "a reader observed a header-only or empty summary during refresh"
leftovers="$(find "$TMP/repo/.oms/memory" -name '.oms-replace.*' | wc -l | tr -d ' ')"
[ "$leftovers" = 0 ] || fail "summary refresh leaked replace scratch"

# A crashed writer's scratch is reclaimed by gc rather than left forever; a
# fresh scratch (a writer that may still be alive) is not.
crashed="$TMP/repo/.oms/task/.oms-replace.crashed"
fresh="$TMP/repo/.oms/task/.oms-replace.alive"
printf 'half-written\n' > "$crashed"
touch -t 202601010000 "$crashed"
printf 'mid-write\n' > "$fresh"
bash "$ROOT/scripts/gc.sh" --repo "$TMP/repo" --apply >/dev/null 2>&1 ||
  fail "gc apply failed"
[ ! -e "$crashed" ] || fail "gc left crashed replace scratch behind"
[ -e "$fresh" ] || fail "gc removed a scratch its writer may still hold"

echo "atomic-state-smoke: ok"
