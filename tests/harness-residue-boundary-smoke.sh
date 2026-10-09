#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-residue-boundary.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "harness-residue-boundary-smoke: $*" >&2
  exit 1
}

export HOME="$TMP/home"
export XDG_CACHE_HOME="$HOME/.cache"
export TMPDIR="$TMP/scratch"
export OMS_LOCK_DIR="$TMP/locks"
mkdir -p "$HOME" "$TMPDIR" "$OMS_LOCK_DIR"

# shellcheck source=scripts/lib/harness-residue.sh
. "$ROOT/scripts/lib/harness-residue.sh"

repo="$TMP/repo"
victim="$TMP/unrelated-linked-worktree"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.name test
git -C "$repo" config user.email test@example.com
printf 'base\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base
git -C "$repo" worktree add --detach "$victim" HEAD >/dev/null 2>&1

# The residue directory is worker-writeable. A planted marker must never turn
# an unrelated registered worktree into git-worktree-remove's target.
planted="$TMPDIR/oh-my-setting-delegate.planted"
mkdir -p "$planted"
oms_harness_mark_tmpdir "$planted" "$repo" "$victim"
printf '999999999\n' > "$planted/.oh-my-setting-tmp.tmp"
sed 's/^pid=.*/pid=999999999/' "$planted/.oh-my-setting-tmp" \
  > "$planted/.oh-my-setting-tmp.tmp"
mv "$planted/.oh-my-setting-tmp.tmp" "$planted/.oh-my-setting-tmp"

oms_harness_cleanup_temp_dirs 0 >/dev/null
[ ! -e "$planted" ] || fail "dead planted residue directory was not removed"
[ -d "$victim" ] || fail "a planted marker removed an unrelated linked worktree"
victim_physical="$(cd "$victim" && pwd -P)"
git -C "$repo" worktree list --porcelain | grep -Fxq "worktree $victim_physical" ||
  fail "a planted marker removed the unrelated worktree registration"

# Production shape: one exact child named wt, registered to the marker
# repository's common dir, whose owner is dead.
dead_residue() {
  mkdir -p "$1"
  git -C "$repo" worktree add --detach "$1/wt" HEAD >/dev/null 2>&1
  oms_harness_mark_tmpdir "$1" "$repo" "$1/wt"
  sed 's/^pid=.*/pid=999999999/' "$1/.oh-my-setting-tmp" > "$1/.oh-my-setting-tmp.tmp"
  mv "$1/.oh-my-setting-tmp.tmp" "$1/.oh-my-setting-tmp"
}

# A dead owner may have left the only copy of its work; cleanup keeps it.
# .oms is ignored as in real repos, so only the record check can see it.
printf '.oms/\n' >> "$repo/.git/info/exclude"
dead_residue "$TMPDIR/oh-my-setting-scratch.dirty"
printf 'edit\n' >> "$TMPDIR/oh-my-setting-scratch.dirty/wt/file.txt"
dead_residue "$TMPDIR/oh-my-setting-scratch.untracked"
printf 'new\n' > "$TMPDIR/oh-my-setting-scratch.untracked/wt/new.txt"
dead_residue "$TMPDIR/oh-my-setting-scratch.detached"
git -C "$TMPDIR/oh-my-setting-scratch.detached/wt" commit -q --allow-empty -m orphan
dead_residue "$TMPDIR/oh-my-setting-delegate.plan"
mkdir -p "$TMPDIR/oh-my-setting-delegate.plan/wt/.oms/plan"
printf '[]\n' > "$TMPDIR/oh-my-setting-delegate.plan/wt/.oms/plan/tasks.json"

[ "$(oms_harness_tmp_residue_count)" = 0 ] ||
  fail "doctor must not count kept work as removable residue"
for dry in 1 0; do
  oms_harness_residue_reset
  out="$(oms_harness_cleanup_temp_dirs "$dry"; echo "kept=$OMS_HARNESS_RESIDUE_KEPT")"
  for want in \
    "kept: $TMPDIR/oh-my-setting-scratch.dirty (dead harness temp dir; worktree has uncommitted or untracked files)" \
    "kept: $TMPDIR/oh-my-setting-scratch.untracked (dead harness temp dir; worktree has uncommitted or untracked files)" \
    "kept: $TMPDIR/oh-my-setting-delegate.plan (dead harness temp dir; worktree holds .oms/plan/tasks.json)" \
    "kept=4"; do
    printf '%s\n' "$out" | grep >/dev/null -Fx "$want" || fail "dry=$dry missing '$want': $out"
  done
  printf '%s\n' "$out" | grep >/dev/null -E '^kept: .*scratch\.detached \(dead harness temp dir; worktree HEAD [0-9a-f]{12} is on no branch, tag or remote ref\)$' ||
    fail "dry=$dry must keep an unreferenced commit: $out"
  if printf '%s\n' "$out" | grep >/dev/null -E '(would remove|removed): '; then
    fail "dry=$dry must not offer kept work for removal: $out"
  fi
done
for kept in scratch.dirty scratch.untracked scratch.detached delegate.plan; do
  [ -d "$TMPDIR/oh-my-setting-$kept/wt" ] || fail "cleanup destroyed kept work in $kept"
done
[ "$(tail -n 1 "$TMPDIR/oh-my-setting-scratch.dirty/wt/file.txt")" = edit ] ||
  fail "cleanup rewrote kept work"
rm -rf "$TMPDIR"/oh-my-setting-scratch.* "$TMPDIR/oh-my-setting-delegate.plan"
git -C "$repo" worktree prune

# A clean worktree at a referenced commit is still removed.
legitimate="$TMPDIR/oh-my-setting-delegate.legitimate"
dead_residue "$legitimate"

oms_harness_cleanup_temp_dirs 0 >/dev/null
[ ! -e "$legitimate" ] || fail "valid dead delegate residue was not removed"
if git -C "$repo" worktree list --porcelain |
    grep -Fq "worktree $legitimate/wt"; then
  fail "valid dead delegate worktree registration survived cleanup"
fi

# A delegate killed by SIGKILL leaves its liveness marker. The next delegate
# start reaps it only when the pid is proven dead and no plan recovery needs it.
markers="$repo/.oms/delegations"
mkdir -p "$markers"
sleep 30 &
dead_pid=$!
dead_native="$(oms_process_native_pid "$dead_pid")"
kill "$dead_pid"
wait "$dead_pid" 2>/dev/null || true
source_tag="$(oms_process_native_pid_source)"
write_marker() {  # NAME PID NATIVE_PID TASK LEASE
  printf '{"schema":4,"id":"%s","pid":%s,"native_pid":%s,"native_pid_source":"%s","task_id":"%s","lease_id":"%s"}\n' \
    "${1%.json}" "$2" "$3" "$source_tag" "$4" "$5" > "$markers/$1"
}
write_marker dead.json "$dead_pid" "$dead_native" "" ""
write_marker coupled.json "$dead_pid" "$dead_native" task-1 lease-1
write_marker live.json "$$" "$(oms_process_native_pid "$$")" "" ""
printf '{not json\n' > "$markers/malformed.json"
ln -s coupled.json "$markers/link.json"
reap_out="$(oms_harness_reap_dead_delegation_markers "$repo" 2>&1)"
[ -z "$reap_out" ] || fail "marker reaping printed output: $reap_out"
[ ! -e "$markers/dead.json" ] || fail "dead unlinked delegation marker was kept"
[ -f "$markers/coupled.json" ] || fail "plan-coupled dead marker was reaped"
[ -f "$markers/live.json" ] || fail "live delegation marker was reaped"
[ -f "$markers/malformed.json" ] || fail "malformed delegation marker was reaped"
[ -L "$markers/link.json" ] || fail "symlinked delegation marker was reaped"

echo "harness-residue-boundary-smoke: ok"
