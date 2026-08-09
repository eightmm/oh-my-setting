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

# Preserve the intended recovery path: the production delegate shape is one
# exact child named wt, registered to the marker repository's common dir.
legitimate="$TMPDIR/oh-my-setting-delegate.legitimate"
mkdir -p "$legitimate"
git -C "$repo" worktree add --detach "$legitimate/wt" HEAD >/dev/null 2>&1
oms_harness_mark_tmpdir "$legitimate" "$repo" "$legitimate/wt"
sed 's/^pid=.*/pid=999999999/' "$legitimate/.oh-my-setting-tmp" \
  > "$legitimate/.oh-my-setting-tmp.tmp"
mv "$legitimate/.oh-my-setting-tmp.tmp" "$legitimate/.oh-my-setting-tmp"

oms_harness_cleanup_temp_dirs 0 >/dev/null
[ ! -e "$legitimate" ] || fail "valid dead delegate residue was not removed"
if git -C "$repo" worktree list --porcelain |
    grep -Fq "worktree $legitimate/wt"; then
  fail "valid dead delegate worktree registration survived cleanup"
fi

echo "harness-residue-boundary-smoke: ok"
