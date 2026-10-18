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

# Reject process-group PIDs before creating a scratch root or registration.
for invalid_pid in 0 00 000; do
  if OMS_HARNESS_CHILD=0 OMS_HARNESS_DELEGATE_DEPTH=0 \
    bash "$ROOT/scripts/scratch-worktree.sh" add --repo "$repo" --owner-pid "$invalid_pid" > "$TMP/invalid-pid" 2>&1; then
    fail "scratch accepted process-group pid $invalid_pid"
  fi
  grep -Fq 'positive process id' "$TMP/invalid-pid" || fail "scratch PID refusal is unclear"
  [ ! -e "$XDG_CACHE_HOME/oh-my-setting/worktrees" ] || fail "invalid PID created scratch root"
done

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

# A settled delegate worktree is removable only when its one recorded run
# finished and the patch the run preserved reproduces the worktree's tree.
# Scratch worktrees carry no such proof.
sha() { python3 -c 'import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"; }
events() { bash "$ROOT/scripts/oms" agent-events --repo "$repo" "$@"; }
run_pid=999999000
delegate_run() {  # NAME TASK STATE [OUTCOME]: dirty delegate worktree, its run and its preserved patch
  local dir="$TMPDIR/oh-my-setting-delegate.$1" art="$repo/.oms/artifacts/delegate" rel attempt
  local tmpindex="$TMP/index.$1"
  run_pid=$((run_pid + 1))
  dead_residue "$dir"
  sed "s/^pid=.*/pid=$run_pid/" "$dir/.oh-my-setting-tmp" > "$TMP/marker" && mv "$TMP/marker" "$dir/.oh-my-setting-tmp"
  printf 'work %s\n' "$1" > "$dir/wt/new-$1.txt"
  printf 'edit %s\n' "$1" >> "$dir/wt/file.txt"
  mkdir -p "$art" "$repo/.oms/artifacts/panel-results"
  # Two-digit abbreviations: preserved patches written when the repo held fewer objects.
  GIT_INDEX_FILE="$tmpindex" git -C "$dir/wt" read-tree HEAD
  GIT_INDEX_FILE="$tmpindex" git -C "$dir/wt" add -A
  GIT_INDEX_FILE="$tmpindex" git -C "$dir/wt" diff --no-ext-diff --no-textconv --abbrev=4 \
    --src-prefix=a/ --dst-prefix=b/ --cached --binary HEAD > "$art/claude-$1-20261018T000000Z-$run_pid.patch"
  printf '# claude delegate\n' > "$art/claude-$1-20261018T000000Z-$run_pid.md"
  attempt="$(events start --provider claude --tool peer-delegate --task-id "$2" \
    --ref "panel_location=worktree:oh-my-setting-delegate.$1/wt" --then starting --then working)"
  if [ "$3" = "done" ]; then
    events transition --attempt "$attempt" --state review --then "done" >/dev/null
  elif [ "$3" != working ]; then
    events transition --attempt "$attempt" --state "$3" >/dev/null
  fi
  if [ -n "${4:-}" ]; then
    rel=".oms/artifacts/panel-results/result-$2-0123456789abcdef01234567.json"
    printf '{"schema":1,"kind":"oms-panel-result","task_id":"%s","revision":"0123456789abcdef01234567","owner":"claude","outcome":"%s","summary":"s","evidence":[]}\n' \
      "$2" "$4" > "$repo/$rel"
    index_row event_id="evt_result_$2" kind=panel-result provider=claude exit=0 task_id="$2" \
      artifact="$rel" artifact_sha256="$(sha "$repo/$rel")"
  fi
}
index_row() {  # JSON_FIELDS...: append one index row built from key=value pairs
  python3 - "$repo/.oms/artifacts/index.jsonl" "$@" <<'PY'
import json, sys
row = dict(arg.split("=", 1) for arg in sys.argv[2:])
with open(sys.argv[1], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row) + "\n")
PY
}
cleanup_out() { oms_harness_residue_reset; oms_harness_cleanup_temp_dirs "$1"; }
drop() { rm -rf "$TMPDIR/oh-my-setting-$1"; git -C "$repo" worktree prune; }
kept_with() {  # NAME FRAGMENT MESSAGE
  cleanup_out 0 | grep >/dev/null -F "$2" || fail "$3"
  [ -d "$TMPDIR/oh-my-setting-delegate.$1/wt" ] || fail "$1: kept delegate was removed"
  drop "delegate.$1"
}

delegate_run settled task-settled "done"
cleanup_out 1 | grep >/dev/null -Fx "would remove: $TMPDIR/oh-my-setting-delegate.settled (dead harness temp dir)" ||
  fail "a finished delegate whose patch reproduces its tree must be removable"
[ -d "$TMPDIR/oh-my-setting-delegate.settled/wt" ] || fail "dry-run removed a settled delegate"
cleanup_out 0 >/dev/null
[ ! -e "$TMPDIR/oh-my-setting-delegate.settled" ] || fail "apply kept a settled delegate"

# Ignored files are invisible to status and to the patch: only regenerable
# bytecode may go with the worktree.
printf '__pycache__/\n*.log\n' >> "$repo/.git/info/exclude"
delegate_run bytecode task-bytecode "done"
mkdir -p "$TMPDIR/oh-my-setting-delegate.bytecode/wt/pkg/__pycache__"
printf 'x' > "$TMPDIR/oh-my-setting-delegate.bytecode/wt/pkg/__pycache__/m.cpython-312.pyc"
cleanup_out 1 | grep >/dev/null -Fx "would remove: $TMPDIR/oh-my-setting-delegate.bytecode (dead harness temp dir)" ||
  fail "ignored bytecode alone must not keep a settled delegate"
cleanup_out 0 >/dev/null
[ ! -e "$TMPDIR/oh-my-setting-delegate.bytecode" ] || fail "apply kept a delegate holding only bytecode"

delegate_run ignored task-ignored "done"
printf 'only copy\n' > "$TMPDIR/oh-my-setting-delegate.ignored/wt/run.log"
kept_with ignored "not settled: holds ignored file run.log" "an ignored non-bytecode file must keep the worktree"

delegate_run fakecode task-fakecode "done"
mkdir -p "$TMPDIR/oh-my-setting-delegate.fakecode/wt/cache"
printf 'x' > "$TMPDIR/oh-my-setting-delegate.fakecode/wt/cache/m.pyc.log"
mkdir -p "$TMPDIR/oh-my-setting-delegate.fakecode/wt/other/__pycache__"
printf 'x' > "$TMPDIR/oh-my-setting-delegate.fakecode/wt/other/__pycache__/notes.log"
kept_with fakecode "not settled: holds ignored file " "only *.pyc/*.pyo inside __pycache__ may be discarded"

# A link standing where a tracked directory belongs is never followed.
mkdir -p "$repo/sub" "$TMP/outside"
printf 'tracked\n' > "$repo/sub/a.txt"; cp "$repo/sub/a.txt" "$TMP/outside/a.txt"
git -C "$repo" add sub && git -C "$repo" commit -qm sub
delegate_run linked task-linked "done"
rm -rf "$TMPDIR/oh-my-setting-delegate.linked/wt/sub"
ln -s "$TMP/outside" "$TMPDIR/oh-my-setting-delegate.linked/wt/sub"
kept_with linked "not settled: preserved patch" "a symlinked parent directory must keep the worktree"

delegate_run mismatch task-mismatch "done"
printf 'later\n' >> "$TMPDIR/oh-my-setting-delegate.mismatch/wt/file.txt"
kept_with mismatch "not settled: preserved patch .oms/artifacts/delegate/claude-mismatch-20261018T000000Z-$run_pid.patch does not reproduce the worktree's files" \
  "a delegate whose files moved past its patch must be kept with the mismatch named"

delegate_run working task-working "working"
kept_with working "is working, not finished" "an unfinished run must keep its worktree"

delegate_run failed task-failed "failed"
kept_with failed "is failed with no owner decision and younger than 7 days" "a recent failed run stays the salvage source"

delegate_run decided task-decided "failed" rejected
cleanup_out 0 | grep >/dev/null -Fx "removed: $TMPDIR/oh-my-setting-delegate.decided (dead harness temp dir)" ||
  fail "a failed run whose owner decided may go"

delegate_run aged task-aged "failed"
OMS_HARNESS_RESIDUE_AGED_DAYS=0 cleanup_out 0 | grep >/dev/null -Fx "removed: $TMPDIR/oh-my-setting-delegate.aged (dead harness temp dir)" ||
  fail "a failed run past the aged horizon may go"

delegate_run unbound task-unbound "done"
sed 's/^pid=.*/pid=999998999/' "$TMPDIR/oh-my-setting-delegate.unbound/.oh-my-setting-tmp" > "$TMP/marker" &&
  mv "$TMP/marker" "$TMPDIR/oh-my-setting-delegate.unbound/.oh-my-setting-tmp"
kept_with unbound "0 delegate artifacts carry pid 999998999, not one" "a run without its preserved patch must be kept"

delegate_run twice task-twice "done"
events start --provider claude --tool peer-delegate --task-id task-twice \
  --ref "panel_location=worktree:oh-my-setting-delegate.twice/wt" --then starting --then working >/dev/null
kept_with twice "2 attempts record this worktree, not one" "an ambiguous run binding must keep the worktree"

delegate_run empty task-empty "done"
: > "$repo/.oms/artifacts/delegate/claude-empty-20261018T000000Z-$run_pid.patch"
kept_with empty "is empty while the worktree has changes" "an empty preserved patch proves nothing"

# Content staged in the real index and then deleted from the tree is work too.
delegate_run staged task-staged "done"
printf 'staged only\n' > "$TMPDIR/oh-my-setting-delegate.staged/wt/ghost.txt"
git -C "$TMPDIR/oh-my-setting-delegate.staged/wt" add ghost.txt
rm "$TMPDIR/oh-my-setting-delegate.staged/wt/ghost.txt"
kept_with staged "not settled: staged content in the worktree is not reproduced by preserved patch" \
  "staged-only divergence must keep the delegate"

# The same preserved evidence never frees a scratch worktree.
delegate_run scratchlike task-scratchlike "done"
mv "$TMPDIR/oh-my-setting-delegate.scratchlike" "$TMPDIR/oh-my-setting-scratch.like"
git -C "$repo" worktree repair "$TMPDIR/oh-my-setting-scratch.like/wt" >/dev/null 2>&1
oms_harness_mark_tmpdir "$TMPDIR/oh-my-setting-scratch.like" "$repo" "$TMPDIR/oh-my-setting-scratch.like/wt"
sed "s/^pid=.*/pid=$run_pid/" "$TMPDIR/oh-my-setting-scratch.like/.oh-my-setting-tmp" > "$TMP/marker" &&
  mv "$TMP/marker" "$TMPDIR/oh-my-setting-scratch.like/.oh-my-setting-tmp"
cleanup_out 0 | grep >/dev/null -Fx "kept: $TMPDIR/oh-my-setting-scratch.like (dead harness temp dir; worktree has uncommitted or untracked files)" ||
  fail "a scratch worktree with uncommitted files must stay kept"
drop scratch.like

# The owning repo must be a distinct, surviving checkout.
delegate_run selfowner task-selfowner "done"
oms_harness_mark_tmpdir "$TMPDIR/oh-my-setting-delegate.selfowner" "$TMPDIR/oh-my-setting-delegate.selfowner/wt" "$TMPDIR/oh-my-setting-delegate.selfowner/wt"
sed "s/^pid=.*/pid=$run_pid/" "$TMPDIR/oh-my-setting-delegate.selfowner/.oh-my-setting-tmp" > "$TMP/marker" &&
  mv "$TMP/marker" "$TMPDIR/oh-my-setting-delegate.selfowner/.oh-my-setting-tmp"
kept_with selfowner "not settled: the owning repo is the worktree, around or inside it, or another harness temp dir" \
  "a worktree that is its own owning repo must be kept"

# The worktree is read as raw bytes: an ident attribute makes the filtered blob
# equal the patch while the file differs, and a filter command must never run.
delegate_run ident task-ident "done"
git -C "$repo" config filter.sentinel.clean "touch $TMP/filter-ran; cat"
git -C "$repo" config filter.sentinel.smudge "touch $TMP/filter-ran; cat"
printf 'new-ident.txt ident\nfile.txt filter=sentinel\n' > "$TMPDIR/oh-my-setting-delegate.ident/wt/.gitattributes"
# shellcheck disable=SC2016
printf "%s\n" '$Id: 0123456789abcdef $' > "$TMPDIR/oh-my-setting-delegate.ident/wt/new-ident.txt"
# shellcheck disable=SC2016
sed 's/^+work ident$/+$Id$/' "$repo/.oms/artifacts/delegate/claude-ident-20261018T000000Z-$run_pid.patch" > "$TMP/ident.patch" &&
  mv "$TMP/ident.patch" "$repo/.oms/artifacts/delegate/claude-ident-20261018T000000Z-$run_pid.patch"
kept_with ident "does not reproduce the worktree's files" "raw bytes that differ from the patch must keep the worktree"
[ ! -e "$TMP/filter-ran" ] || fail "cleanup executed a configured filter command"
git -C "$repo" config --unset filter.sentinel.clean
git -C "$repo" config --unset filter.sentinel.smudge

# The proof is read again at removal: a change after classification keeps it.
eval "real_$(declare -f oms_harness_classify_dead_residue)"
mutate_after=""
# shellcheck disable=SC2317
oms_harness_classify_dead_residue() {
  real_oms_harness_classify_dead_residue "$@" || return 1
  if [ -n "$mutate_after" ] && [ -z "$OMS_HARNESS_RESIDUE_KEEP_REASON" ]; then
    printf 'raced\n' >> "$mutate_after"
    mutate_after=""
  fi
}
delegate_run raced task-raced "done"
mutate_after="$TMPDIR/oh-my-setting-delegate.raced/wt/new-raced.txt"
kept_with raced "worktree changed during cleanup" "content changed between classification and removal must keep the worktree"
unset -f oms_harness_classify_dead_residue
eval "$(declare -f real_oms_harness_classify_dead_residue | sed '1s/^real_//')"

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
