#!/usr/bin/env bash
set -euo pipefail

# `oms land` end to end against a bare remote: a green gate pushes and writes a
# passed receipt beside its XDG-state log, a red gate records the failure and
# pushes nothing, a HEAD that moves during the gate is never pushed, dirty or
# diverged trees are refused before any job starts, and the detached mode
# leaves a receipt the status verb reads. No CI (--ci-wait 0) and no install
# refresh (the fixture is not the harness checkout), so nothing here reaches
# the network.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-land.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
export OMS_WORK_JOURNAL_SUPPRESS=1 XDG_STATE_HOME="$TMP/state" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t TMPDIR="$TMP/tmp" OMS_LAND_SMOKE_ROOT="$ROOT" \
  OMS_LOCK_DIR="$TMP/locks"
mkdir -p "$TMPDIR"
log_of() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["log"])' "$1"; }
LAND="$ROOT/scripts/land.sh"
AUTOPILOT_RECEIPT="$ROOT/scripts/lib/autopilot-receipt.py"
state_land="$XDG_STATE_HOME/oh-my-setting/land"

fail() { echo "FAIL: $*" >&2; exit 1; }

if "$LAND" --sync-shared --wait > "$TMP/sync-path.out" 2>&1; then
  fail "--sync-shared accepted a missing PATH"
fi
grep -q -- '--sync-shared requires PATH' "$TMP/sync-path.out" || fail "missing sync PATH was not diagnosed"

registry_snapshot() {
  python3 - "$1" <<'PY'
import hashlib, json, pathlib, subprocess, sys
repo = pathlib.Path(sys.argv[1])
common = pathlib.Path(subprocess.check_output(
    ['git', '-C', str(repo), 'rev-parse', '--git-common-dir'], text=True).strip())
root = (repo / common).resolve() / 'worktrees'
rows = []
if root.exists():
    for path in [root, *sorted(root.rglob('*'))]:
        st = path.lstat()
        rows.append([str(path.relative_to(root)), st.st_mode, st.st_mtime_ns,
                     hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None])
print(json.dumps(rows))
PY
}
check_gate_clone() {
  [ "$(git rev-parse HEAD)" = "$OMS_LAND_SMOKE_SHA" ] || return 8
  ! git symbolic-ref -q HEAD || return 8
  [ -d .git ] && [ ! -e .git/objects/info/alternates ] || return 8
  [ "$(git rev-parse origin/main)" = "$OMS_LAND_SMOKE_BASE" ] || return 8
  registry_snapshot "$OMS_LAND_SMOKE_REPO" | cmp - "$OMS_LAND_SMOKE_REGISTRY" || return 8
  python3 - <<'PY' || return 8
from pathlib import Path
assert all(p.stat().st_nlink == 1 for p in Path('.git/objects').rglob('*') if p.is_file())
PY
}
export -f registry_snapshot check_gate_clone
export OMS_LAND_SMOKE_REPO OMS_LAND_SMOKE_SHA OMS_LAND_SMOKE_BASE
export OMS_LAND_SMOKE_REGISTRY="$TMP/registry.json"
gate_tree_left() { ls -d "$TMPDIR"/oh-my-setting-land.* >/dev/null 2>&1; }

git init -q --bare -b main "$TMP/remote.git"
repo="$TMP/work.. tree"
git clone -q "$TMP/remote.git" "$repo" 2>/dev/null
mkdir -p "$repo/scripts" "$repo/.oms" "$repo/.github/workflows"
printf '*\n' > "$repo/.oms/.gitignore"
printf 'on: push\n' > "$repo/.github/workflows/test.yml"
git -C "$repo" add .github/workflows/test.yml
gate() {  # gate BODY -> commits scripts/check.sh with that body
  printf '#!/usr/bin/env bash\n%s\n' "$1" > "$repo/scripts/check.sh"
  git -C "$repo" add scripts/check.sh
  git -C "$repo" commit -q -m "gate: $1"
}
write_autopilot_receipt() {  # WORKTREE STAGE -> a real typed outer receipt
  local worktree="$1" stage="$2" receipt expected=absent base_sha
  local spec_sha="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  local branch="oms/autopilot-${spec_sha:0:12}"

  receipt="$worktree/.oms/plan/autopilot-run.json"
  mkdir -p "$(dirname "$receipt")"
  if [ -e "$receipt" ] || [ -L "$receipt" ]; then
    expected="$(python3 "$AUTOPILOT_RECEIPT" digest "$receipt")" ||
      fail "cannot read fixture autopilot receipt"
    expected="${expected//$'\r'/}"
  fi
  base_sha="$(git -C "$worktree" rev-parse HEAD)"
  python3 "$AUTOPILOT_RECEIPT" write "$receipt" --expected "$expected" \
    --stage "$stage" --repo "$worktree" --spec-sha256 "$spec_sha" \
    --planner codex --worker codex --reviewer codex \
    --planner-reasoning-effort low --worker-reasoning-effort low \
    --reviewer-reasoning-effort low --provider-timeout 1m --planner-timeout 1m \
    --worker-timeout 1m --reviewer-timeout 1m --allowed . --base main \
    --base-sha "$base_sha" --remote origin --max-cycles 1 --initial-tasks 1 \
    --replan-tasks 1 --review-mode shadow --branch "$branch" \
    --owner-id "owner_00000000000000000000000000000000" \
    --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >/dev/null ||
      fail "cannot write fixture autopilot receipt"
}
gate 'check_gate_clone || exit $?; echo gate ok'
git -C "$repo" push -q origin HEAD:main
remote_tip() { git -C "$TMP/remote.git" rev-parse main; }

# --- 1. nothing to land ------------------------------------------------------
out="$("$LAND" --repo "$repo" --wait --ci-wait 0)"
printf '%s' "$out" | grep >/dev/null 'nothing to land' || fail "an up-to-date tree lands nothing: $out"
[ ! -e "$state_land" ] || fail "an up-to-date landing must not leave receipt state"
OMS_LOCK_FORCE_MKDIR=1 "$LAND" status --repo "$repo" --active-exit --json > "$TMP/fresh-lock.json" ||
  fail "a fresh repository must expose its free land lock without a receipt"
python3 -c 'import json,sys; assert json.load(open(sys.argv[1])) == {"active": False}' "$TMP/fresh-lock.json" ||
  fail "fresh land lock JSON must report active=false"

# --- 2. green worktree gate: state survives worktree removal ------------------
peer="$TMP/landing-worktree"
git -C "$repo" worktree add -q -b land-receipt "$peer" HEAD
mkdir -p "$peer/.oms"
printf '*\n' > "$peer/.oms/.gitignore"
peer_oms_before="$(find "$peer/.oms" -mindepth 1 -print | sort)"
echo one > "$peer/one"; git -C "$peer" add one; git -C "$peer" commit -q -m one
head="$(git -C "$peer" rev-parse HEAD)"
OMS_LAND_SMOKE_REPO="$(cd "$repo" && pwd -P)"
OMS_LAND_SMOKE_SHA="$head" OMS_LAND_SMOKE_BASE="$(remote_tip)"
registry_snapshot "$repo" > "$OMS_LAND_SMOKE_REGISTRY"
"$LAND" --repo "$peer" --wait --ci-wait 0 > "$TMP/pass.out" || fail "green gate must land: $(cat "$TMP/pass.out")"
registry_snapshot "$repo" | cmp - "$OMS_LAND_SMOKE_REGISTRY" || fail "linked land changed the source worktree registry"
gate_tree_left && fail "linked land left a gate clone"
grep -Fq 'land: this push changes tests +0 / code +1 lines' "$TMP/pass.out" ||
  fail "land must show the push's test and code growth: $(cat "$TMP/pass.out")"
grep -Fq '"growth": "tests +0 / code +1 lines"' "$(find "$state_land" -type f -name '*.json' -print | sort | tail -n 1)" ||
  fail "the land receipt must keep the push's test and code growth"
[ "$(remote_tip)" = "$head" ] || fail "remote main must be the landed commit"
receipt="$(find "$state_land" -type f -name '*.json' -print | sort | tail -n 1)"
[ -n "$receipt" ] || fail "green gate must write an XDG-state receipt"
receipt_dir="$(dirname "$receipt")"
case "$receipt_dir" in "$state_land"/*) ;; *) fail "receipt must live under XDG state: $receipt" ;; esac
slug="${receipt_dir#"$state_land"/}"
case "$slug" in ''|.|..|*/*|*..*|*[!A-Za-z0-9._-]*) fail "unsafe receipt slug: $slug" ;; esac
python3 - "$receipt" "$head" <<'PY' || fail "passed receipt is wrong: $(cat "$receipt")"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["state"] == "passed" and r["sha"] == sys.argv[2], r
assert r["gate"]["rc"] == 0 and r["push"]["rc"] == 0, r
assert r["update"]["rc"] == "skipped" and r["ci"]["conclusion"] == "skipped", r
assert r["update"].get("reason") == "no-install-for-remote", r
assert "siblings" not in r, r
assert r["gate"]["command"] == "bash scripts/check.sh", r
PY
grep -q 'gate ok' "$(log_of "$receipt")" || fail "gate output must land in the receipt log"
case "$(log_of "$receipt")" in "$state_land"/*) ;; *) fail "the gate log must live outside the repo: $(log_of "$receipt")" ;; esac
[ "$(dirname "$(log_of "$receipt")")" = "$receipt_dir" ] || fail "receipt and gate log must share a state directory"
"$LAND" status --repo "$peer" | grep -q 'update: skipped (no-install-for-remote)' ||
  fail "text status must display the install skip reason"
[ "$peer_oms_before" = "$(find "$peer/.oms" -mindepth 1 -print | sort)" ] ||
  fail "a landing must not add receipt state under its worktree .oms"
"$LAND" status --repo "$peer" | grep -q '^land .*: passed' || fail "status must read a worktree receipt"
git -C "$repo" worktree remove --force "$peer"
[ ! -e "$peer" ] || fail "landing worktree removal must succeed"
"$LAND" status --repo "$repo" | grep -q '^land .*: passed' ||
  fail "main checkout must read the removed worktree's receipt"
git -C "$repo" pull -q --ff-only origin main

# --- 2b. the gate checks the committed tree, not the live checkout -----------
# A session driving the checkout writes its own .oms during the gate, and an
# untracked file must not sway a verdict the push attributes to the commit.
echo scratch > "$repo/untracked-scratch"
# A stale registration must also survive: land must not sweep or prune the
# canonical registry while workers may be snapshotting it.
git -C "$repo" worktree add -q --detach "$TMP/oms-land-gate.stale" HEAD
rm -rf "$TMP/oms-land-gate.stale"
gate 'test ! -e untracked-scratch || exit 9; mkdir -p .oms; touch .oms/gate-wrote
check_gate_clone || exit $?
echo isolated gate ok'
OMS_LAND_SMOKE_SHA="$(git -C "$repo" rev-parse HEAD)" OMS_LAND_SMOKE_BASE="$(remote_tip)"
registry_snapshot "$repo" > "$OMS_LAND_SMOKE_REGISTRY"
repo_oms_before="$(find "$repo/.oms" -print | sort)"
"$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/isolated.out" 2>&1 ||
  fail "the gate must run in a tree without untracked files: $(cat "$TMP/isolated.out")"
[ "$repo_oms_before" = "$(find "$repo/.oms" -print | sort)" ] ||
  fail "a gate's .oms writes must not reach the landing checkout"
registry_snapshot "$repo" | cmp - "$OMS_LAND_SMOKE_REGISTRY" || fail "standalone land changed the source worktree registry"
gate_tree_left && fail "the gate clone must be removed after the gate"
rm -f "$repo/untracked-scratch"

# Setup failures and catchable termination use the same cleanup path as a red
# gate. Extract only run_gate so these probes cannot push or change receipts.
(
  export REPO="$repo" SHA="$OMS_LAND_SMOKE_SHA" LOG="$TMP/setup.log" GATE=true
  . "$ROOT/scripts/lib/harness-residue.sh"
  eval "$(sed -n '/^run_gate() (/,/^)/p' "$LAND")"
  for failed_step in clone fetch checkout; do
    git() {
      local arg
      for arg in "$@"; do
        if [ "$arg" = "$failed_step" ]; then
          [ "$arg" != clone ] || mkdir -p "${!#}/.git/objects"
          return 17
        fi
      done
      command git "$@"
    }
    if run_gate; then fail "$failed_step failure was ignored"; fi
    gate_tree_left && fail "$failed_step failure leaked the clone"
  done
  unset -f git
  SHA="$(git -C "$repo" rev-parse HEAD^)"
  OMS_LAND_SMOKE_SHA="$SHA" OMS_LAND_SMOKE_BASE="$(git -C "$repo" rev-parse origin/main)"
  GATE=check_gate_clone
  run_gate || fail "gate did not check the pinned SHA independently of source HEAD"
  gate_tree_left && fail "pinned SHA gate leaked the clone"
  for signal in HUP INT TERM; do
    GATE="kill -$signal \"\$PPID\""
    rc=0
    run_gate || rc=$?
    case "$signal:$rc" in HUP:129|INT:130|TERM:143) ;; *) fail "incorrect signal exit: $signal:$rc" ;; esac
    gate_tree_left && fail "$signal leaked the clone"
  done
  :
) || exit 1
registry_snapshot "$repo" | cmp - "$OMS_LAND_SMOKE_REGISTRY" || fail "failed setup changed the source registry"

# --- 3. live siblings are recorded, waited for, or explicitly ignored ---------
sibling="$TMP/sibling.. worktree"
git -C "$repo" worktree add -q -b oms/autopilot-aaaaaaaaaaaa "$sibling" HEAD
sibling_physical="$(cd "$sibling" && pwd -P)"
write_autopilot_receipt "$sibling" driving
python3 "$AUTOPILOT_RECEIPT" metadata "$sibling/.oms/plan/autopilot-run.json" |
  grep -q '^driving	' || fail "fixture sibling must carry a typed driving receipt"
gate 'echo sibling gate ok'
before="$(remote_tip)"
if "$LAND" --repo "$repo" --wait --ci-wait 0 --sibling-wait 0 \
  > "$TMP/sibling-blocked.out" 2>&1; then
  fail "a live sibling must block before push: $(cat "$TMP/sibling-blocked.out")"
fi
[ "$(remote_tip)" = "$before" ] || fail "a blocked sibling landing must not push"
grep -Fq "live sibling worktree $sibling_physical (driving)" "$TMP/sibling-blocked.out" ||
  fail "intake must report the live sibling: $(cat "$TMP/sibling-blocked.out")"
grep -Fq "sibling worktree $sibling_physical still driving" "$TMP/sibling-blocked.out" ||
  fail "a blocked sibling must be visible in land status: $(cat "$TMP/sibling-blocked.out")"
grep -Fq "siblings: $sibling_physical driving (waited 0s)" "$TMP/sibling-blocked.out" ||
  fail "status must show the recorded siblings: $(cat "$TMP/sibling-blocked.out")"
"$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved 2>/dev/null | grep -q sibling &&
  fail "a blocked sibling landing must not record a fail-ledger row"
python3 - "$receipt_dir" "$sibling_physical" <<'PY' || fail "blocked sibling receipt is wrong"
import glob, json, os, sys
directory, sibling = sys.argv[1:]
for path in glob.glob(os.path.join(directory, "*.json")):
    row = json.load(open(path))
    if row.get("state") != "blocked":
        continue
    assert row.get("siblings", {}).get("live") == sibling + " driving", row
    assert row.get("push", {}).get("sibling_wait_seconds") == 0, row
    assert row.get("push", {}).get("rc") == "pending", row
    break
else:
    raise AssertionError("no blocked sibling receipt")
PY

wait_marker="$TMP/sibling-gate-finished"
gate "echo sibling wait gate ok; touch \"$wait_marker\""
(
  while [ ! -f "$wait_marker" ]; do sleep 1; done
  sleep 1
  write_autopilot_receipt "$sibling" reviewing
) &
sibling_rewriter=$!
if ! "$LAND" --repo "$repo" --wait --ci-wait 0 --sibling-wait 3 \
  > "$TMP/sibling-wait.out" 2>&1; then
  wait "$sibling_rewriter" || true
  fail "a sibling leaving live stages must release the push: $(cat "$TMP/sibling-wait.out")"
fi
wait "$sibling_rewriter" || fail "could not update fixture sibling receipt"
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] ||
  fail "a released sibling landing must push"
python3 - "$receipt_dir" "$sibling_physical" <<'PY' || fail "sibling wait receipt is wrong"
import glob, json, os, sys
directory, sibling = sys.argv[1:]
for path in glob.glob(os.path.join(directory, "*.json")):
    row = json.load(open(path))
    if row.get("state") != "passed" or row.get("siblings", {}).get("live") != sibling + " driving":
        continue
    assert row.get("push", {}).get("rc") == 0, row
    assert row["push"].get("sibling_wait_seconds", 0) > 0, row
    break
else:
    raise AssertionError("no passed sibling-wait receipt")
PY

write_autopilot_receipt "$sibling" approved
write_autopilot_receipt "$sibling" driving
gate 'echo ignore sibling gate ok'
"$LAND" --repo "$repo" --wait --ci-wait 0 --sibling-wait 0 --ignore-siblings \
  > "$TMP/sibling-ignore.out" 2> "$TMP/sibling-ignore.err" ||
  fail "--ignore-siblings must permit a push: $(cat "$TMP/sibling-ignore.out")"
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] ||
  fail "--ignore-siblings must push"
grep -Fq "$sibling_physical" "$TMP/sibling-ignore.err" &&
  fail "--ignore-siblings must skip the intake report"
python3 - "$receipt_dir" <<'PY' || fail "ignored sibling receipt is wrong"
import glob, json, os, sys
for path in glob.glob(os.path.join(sys.argv[1], "*.json")):
    row = json.load(open(path))
    if row.get("state") == "passed" and row.get("siblings", {}).get("ignored") == "true":
        assert row.get("push", {}).get("rc") == 0, row
        break
else:
    raise AssertionError("no ignored sibling receipt")
PY
# an unreadable sibling receipt is noted once and never blocks
printf '{' > "$sibling/.oms/plan/autopilot-run.json"
gate 'echo invalid sibling gate ok'
"$LAND" --repo "$repo" --wait --ci-wait 0 --sibling-wait 0 \
  > "$TMP/sibling-invalid.out" 2> "$TMP/sibling-invalid.err" ||
  fail "an invalid sibling receipt must not block: $(cat "$TMP/sibling-invalid.out" "$TMP/sibling-invalid.err")"
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] || fail "an invalid sibling receipt must not stop the push"
[ "$(grep -c "ignoring invalid sibling autopilot receipt: $sibling_physical" "$TMP/sibling-invalid.err")" = 1 ] ||
  fail "the invalid receipt must be noted exactly once: $(cat "$TMP/sibling-invalid.err")"
git -C "$repo" worktree remove --force "$sibling"

# --- 4. red gate: failure recorded, nothing pushed -----------------------------
gate 'echo boom; echo "check: affected:tests/demo-smoke.sh FAILED (1s)"; echo "check: scripts-smoke.test_demo FAILED"; exit 3'
before="$(remote_tip)"
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/fail.out" 2>&1; then
  fail "a red gate must exit nonzero: $(cat "$TMP/fail.out")"
fi
[ "$(remote_tip)" = "$before" ] || fail "a red gate must not push"
grep -q 'gate exit 3' "$TMP/fail.out" || fail "the failure reason must be shown: $(cat "$TMP/fail.out")"
gate_tree_left && fail "a red gate must remove its gate clone"
grep -Fq 'in affected:tests/demo-smoke.sh scripts-smoke.test_demo' "$repo/.oms/failures.jsonl" ||
  fail "a red gate must be recorded in the fail ledger with its failing stages"

# A same-request retry keeps both log attempts while summarizing only the pass.
retry_count="$TMP/retry-count"
gate "if [ -f '$retry_count' ]; then echo 'check: ok'; else touch '$retry_count'; echo 'check: affected:tests/old-smoke.sh FAILED (1s)'; exit 3; fi"
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/retry-first.out" 2>&1; then
  fail "the first retry probe must fail"
fi
"$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/retry-second.out" 2>&1 ||
  fail "the same landing request must pass on retry: $(cat "$TMP/retry-second.out")"
retry_sha="$(git -C "$repo" rev-parse HEAD)"
retry_receipt="$(find "$receipt_dir" -type f -name "$retry_sha-*.json" -print)"
[ -n "$retry_receipt" ] || fail "retry must leave a passed receipt"
retry_log="$(log_of "$retry_receipt")"
[ "$(grep -c '^land attempt: ' "$retry_log")" = 2 ] || fail "both attempts must be marked in the appended log"
python3 - "$retry_receipt" <<'PY' || fail "retry receipt must summarize the successful attempt only"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["state"] == "passed" and "pushed " in r["summary"] and "old-smoke" not in r["summary"], r
PY
grep -q 'land attempt: 1 started_at=.* sha=' "$retry_log" || fail "attempt one marker is missing"
grep -q 'land attempt: 2 started_at=.* sha=' "$retry_log" || fail "attempt two marker is missing"

# --- 5. HEAD moves during the gate: nothing pushed -----------------------------
before="$(remote_tip)"
export OMS_TEST_LAND_COUNT="$TMP/moved-gate-started" OMS_TEST_LAND_RELEASE="$TMP/moved-gate-release"
gate 'echo slow ok; touch "$OMS_TEST_LAND_COUNT"; for ((i=0;i<400;i++)); do [ ! -f "$OMS_TEST_LAND_RELEASE" ] || exit 0; sleep 0.05; done; exit 1'
# Commit only after the gate runs (so after the landing stamped HEAD), then release it.
(
  for _ in $(seq 1 400); do [ ! -f "$OMS_TEST_LAND_COUNT" ] || break; sleep 0.05; done
  echo two > "$repo/two"; git -C "$repo" add two; git -C "$repo" commit -q -m two
  touch "$OMS_TEST_LAND_RELEASE"
) &
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/moved.out" 2>&1; then
  fail "a moved HEAD must fail the landing: $(cat "$TMP/moved.out")"
fi
wait
[ "$(remote_tip)" = "$before" ] || fail "a moved HEAD must not push"
grep -q 'HEAD or the tree changed' "$TMP/moved.out" || fail "moved-HEAD reason missing: $(cat "$TMP/moved.out")"

# --- 6. refusals before any job: dirty tree, diverged remote -----------------
gate 'echo gate ok'
receipts_before_refusal="$(find "$receipt_dir" -maxdepth 1 -type f -name '*.json' -print | sort)"
echo dirty >> "$repo/one"
"$LAND" --repo "$repo" --wait --ci-wait 0 2>"$TMP/dirty.err" && fail "a dirty tree must be refused"
grep -q 'commit or stash' "$TMP/dirty.err" || fail "dirty refusal must say so: $(cat "$TMP/dirty.err")"
git -C "$repo" checkout -q -- one
other="$TMP/other-clone/work.. tree"
mkdir -p "$(dirname "$other")"
git clone -q "$TMP/remote.git" "$other" 2>/dev/null
if "$LAND" status --repo "$other" > "$TMP/other-status.out" 2>&1; then
  fail "a distinct clone must not read this repository's receipt"
fi
other_dir="$(sed -n 's/^no landing recorded under //p' "$TMP/other-status.out" | sed -n '1p')"
if [ -z "$other_dir" ] || [ "$other_dir" = "$receipt_dir" ]; then
  fail "distinct clones with the same basename must have separate state directories"
fi
broken="$receipt_dir/$(git -C "$repo" rev-parse HEAD)-zzzz-broken.json"
printf '{broken' > "$broken"
if "$LAND" status --repo "$repo" >/dev/null 2>&1; then
  fail "a malformed receipt must fail land status"
fi
rm -f "$broken"
# Another main's newer land must not stand in for this worktree's HEAD.
cp "$(ls -t "$receipt_dir"/*.json | head -n 1)" "$receipt_dir/0000000000000000000000000000000000000000-other.json"
"$LAND" status --repo "$repo" | grep -q '^land 0000000' &&
  fail "land status must prefer the receipt for this worktree's HEAD"
rm -f "$receipt_dir/0000000000000000000000000000000000000000-other.json"
echo other > "$other/other"; git -C "$other" add other
git -C "$other" commit -q -m other; git -C "$other" push -q origin HEAD:main
"$LAND" --repo "$repo" --wait --ci-wait 0 2>"$TMP/diverged.err" && fail "a diverged remote must be refused"
grep -q 'rebase first' "$TMP/diverged.err" || fail "diverged refusal must say so: $(cat "$TMP/diverged.err")"
[ "$receipts_before_refusal" = "$(find "$receipt_dir" -maxdepth 1 -type f -name '*.json' -print | sort)" ] ||
  fail "a refused landing must not leave a receipt"

# --- 7. detached mode: receipt appears, status reads it -----------------------
git -C "$repo" pull -q --rebase origin main
out="$("$LAND" --repo "$repo" --ci-wait 0)"
printf '%s' "$out" | grep >/dev/null 'receipt: ' || fail "detached mode must print the receipt path: $out"
receipt="$(printf '%s\n' "$out" | sed -n 's/^receipt: //p')"
for _ in $(seq 1 60); do
  [ -f "$receipt" ] && grep -q '"state": "passed"' "$receipt" && break
  sleep 0.5
done
grep -q '"state": "passed"' "$receipt" || fail "detached landing did not pass in time: $(cat "$receipt" 2>/dev/null)"
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] || fail "detached landing must push"

# --- 8. the detached job must not inherit ignored SIGINT/SIGQUIT --------------
gate 'grep SigIgn /proc/$$/status'
echo three > "$repo/three"; git -C "$repo" add three; git -C "$repo" commit -q -m three
out="$("$LAND" --repo "$repo" --ci-wait 0)"
receipt="$(printf '%s\n' "$out" | sed -n 's/^receipt: //p')"
for _ in $(seq 1 60); do
  [ -f "$receipt" ] && grep -q '"state": "passed"' "$receipt" && break
  sleep 0.5
done
grep -q '"state": "passed"' "$receipt" || fail "signal probe landing did not pass: $(cat "$receipt" 2>/dev/null)"
grep -Eq 'SigIgn:.*[0-9a-f]*[01489]$' "$(log_of "$receipt")" ||
  fail "the detached gate must see SIGINT and SIGQUIT unignored: $(grep SigIgn "$(log_of "$receipt")")"

# The source checkout selects affected checks even before an install receipt
# exists. A project template without this engine retains its plain default.
mkdir -p "$repo/scripts/lib"
: > "$repo/scripts/lib/check-parallel.sh"
git -C "$repo" add scripts/lib/check-parallel.sh
gate 'test "$*" = "--affected --ci-parity --changed-from origin/main --changed-to HEAD"'
OMS_INSTALL_RECEIPT="$TMP/no-install-receipt.json" "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 \
  > "$TMP/source-default.out" 2>&1 || fail "source checkout did not select its affected gate"

# A successful push does not hide a failed installation refresh.
install="$TMP/install"
git init -q "$install"
git -C "$install" remote add origin "$TMP/remote.git"
mkdir -p "$install/scripts"
cat > "$install/scripts/update.sh" <<'EOF'
#!/usr/bin/env bash
echo update >> "$OMS_TEST_LAND_EVENTS"
[ "$OH_MY_SETTING_UPDATE_EXPECTED_TARGET" = "$(git -C "$OMS_TEST_LAND_REPO" rev-parse HEAD)" ] || exit 43
[ "${OMS_TEST_UPDATE_RC:-42}" != 0 ] || {
  # Advance the disposable installed Git state while retaining this stub updater.
  root="$(cd "$(dirname "$0")/.." && pwd)"
  git -C "$root" fetch -q origin
  git -C "$root" update-ref HEAD "$OH_MY_SETTING_UPDATE_EXPECTED_TARGET"
  git -C "$root" read-tree HEAD
  git -C "$root" checkout-index -a -f
  printf '/scripts/update.sh\n' > "$root/.git/info/exclude"
}
exit "${OMS_TEST_UPDATE_RC:-42}"
EOF
chmod +x "$install/scripts/update.sh"
python3 - "$TMP/install.json" "$install" <<'PY'
import json, sys
with open(sys.argv[1], "w") as fh:
    json.dump({"source_root": sys.argv[2]}, fh)
PY
mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$2" = view ]; then
  # A run's jobs: the full selection, an affected-only run, or neither. The
  # full job set also arrives for another SHA, a failed run, with a duplicate
  # job or beside a failed job; none of those proves this commit.
  echo jobs >> "$OMS_TEST_LAND_EVENTS"
  [ "${OMS_TEST_CI_SCOPE:-affected}" != error ] || exit 4
  sha="$(git -C "$OMS_TEST_LAND_REPO" rev-parse HEAD)"
  full='"lint","affected_plan","gate","native (ubuntu-latest, copy)","focused (1/4)","focused (2/4)","focused (3/4)","focused (4/4)","scripts-smoke (1/4)","scripts-smoke (2/4)","scripts-smoke (3/4)","scripts-smoke (4/4)"'
  count_file=/dev/null
  case "${OMS_TEST_CI_SCOPE:-affected}" in
    full|wrong-sha|failed-run|duplicate|extra-failure|settles-full) jobs="$full" ;;
    ambiguous|ambiguous-null) jobs='"lint","affected_plan","gate","focused (1/4)"' ;;
    lagging)
      count_file="$OMS_TEST_LAND_EVENTS.lagging-count"
      count=0
      [ ! -f "$count_file" ] || count="$(cat "$count_file")"
      count=$((count + 1))
      printf '%s\n' "$count" > "$count_file"
      jobs='"affected_plan","gate"'
      ;;
    *) jobs='"affected_plan","gate"' ;;
  esac
  python3 -c 'import json, sys
sha, names, scope = sys.argv[1], json.loads("[" + sys.argv[2] + "]"), sys.argv[3]
rows = [{"name": name, "conclusion": "success"} for name in names]
if "lint" not in names:
    rows.append({"name": "lint", "conclusion": "skipped"})
if scope == "lagging" and open(sys.argv[4]).read().strip() == "1":
    rows[0]["conclusion"] = None
if scope == "settles-full":
    count_file = __import__("os").environ["OMS_TEST_LAND_EVENTS"] + ".settles-full-count"
    try: count = int(open(count_file).read().strip())
    except (FileNotFoundError, ValueError): count = 0
    count += 1
    open(count_file, "w").write(str(count))
    if count <= 3: rows[0]["conclusion"] = None
if scope == "ambiguous-null":
    rows[2]["conclusion"] = None
if scope == "duplicate":
    rows.append({"name": "gate", "conclusion": "success"})
if scope == "extra-failure":
    rows.append({"name": "native (ubuntu-latest, auto)", "conclusion": "failure"})
print(json.dumps({"headSha": "0" * 40 if scope == "wrong-sha" else sha, "status": "completed",
                  "conclusion": "failure" if scope == "failed-run" else "success", "jobs": rows}))' \
    "$sha" "$jobs" "${OMS_TEST_CI_SCOPE:-affected}" "$count_file"
  exit 0
fi
echo ci >> "$OMS_TEST_LAND_EVENTS"
[ "${OMS_TEST_CI_DELAY:-0}" = 0 ] || sleep "$OMS_TEST_CI_DELAY"
if [ -n "${OMS_TEST_LAND_CHANGE_PUSH_URL:-}" ]; then
  git -C "$OMS_TEST_LAND_REPO" remote set-url --push origin "$OMS_TEST_LAND_CHANGE_PUSH_URL"
fi
if [ "${OMS_TEST_CI_RC:-0}" != 0 ]; then echo 'authentication required' >&2; exit "$OMS_TEST_CI_RC"; fi
sha=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = --commit ]; then sha="$2"; break; fi
  shift
done
if [ "$OMS_TEST_CI_RESULT" = missing ]; then echo '[]'; exit 0; fi
if [ "$OMS_TEST_CI_RESULT" = malformed ]; then echo '{}'; exit 0; fi
[ "$OMS_TEST_CI_RESULT" != wrong-sha ] || sha=wrong
printf '[{"databaseId":1,"headSha":"%s","status":"completed","conclusion":"%s"}]\n' "$sha" "$OMS_TEST_CI_RESULT"
EOF
chmod +x "$TMP/bin/gh"
export PATH="$TMP/bin:$PATH" OMS_TEST_LAND_EVENTS="$TMP/events" OMS_TEST_LAND_REPO="$repo"
for ci_result in malformed wrong-sha; do
  query_rc=0
  OMS_TEST_CI_RESULT="$ci_result" python3 "$ROOT/scripts/lib/ci-query.py" \
    fixture/repo "$head" 1 main test.yml > "$TMP/query.out" 2>&1 || query_rc=$?
  [ "$query_rc" = 2 ] || fail "invalid CI evidence must be rejected: $ci_result/$query_rc"
done
out="$(OMS_TEST_CI_RESULT=missing python3 "$ROOT/scripts/lib/ci-query.py" fixture/repo "$head" 1 main test.yml)"
[ "$out" = '0 missing' ] || fail "absent CI must not look green"
for ci_result in failure skipped success; do
  gate "echo CI $ci_result probe; echo 'check: ok'"
  : > "$TMP/events"
  ci_wait=1
  [ "$ci_result" != skipped ] || ci_wait=0
  if OMS_TEST_CI_RESULT="$ci_result" OMS_INSTALL_RECEIPT="$TMP/install.json" \
      "$LAND" --repo "$repo" --wait --ci-wait "$ci_wait" > "$TMP/ci.out" 2>&1; then
    fail "failed, absent CI or failed update was reported as successful"
  fi
  if [ "$ci_result" = success ]; then
    [ "$(cat "$TMP/events")" = "$(printf 'ci\nupdate')" ] || fail "install ran before CI"
  else
    ! grep -q update "$TMP/events" || fail "install ran without successful CI"
  fi
done
# A run GitHub skipped verified nothing; only the no-wait path may say skipped.
gate 'echo run skipped probe'
if OMS_TEST_CI_RESULT=skipped "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/run-skipped.out" 2>&1; then
  fail "a skipped GitHub run passed as CI: $(cat "$TMP/run-skipped.out")"
fi
grep -q 'ci run-skipped' "$TMP/run-skipped.out" || fail "skipped run reason missing: $(cat "$TMP/run-skipped.out")"
# Each clone has a different receipt history. A recorded push.base fixes the
# third landing's whole range even when the first clone has an older receipt.
python3 - "$ROOT" "$TMP" <<'PYRANGE' || fail "cross-clone receipt range regression"
import ast
import contextlib
import hashlib
import importlib.util as _importlib_util
import io
import json
import os
import re
import runpy
from pathlib import Path
import stat
import subprocess
import sys

root, tmp = map(Path, sys.argv[1:])
land = str(root / 'scripts/land.sh')
helper = str(root / 'scripts/lib/land_receipts.py')
remote = tmp / 'range-remote.git'

def run(*args, ok=True):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True,
                            env=dict(os.environ, OMS_TEST_CI_RESULT='success'))
    if ok:
        assert result.returncode == 0, (args, result.stdout, result.stderr)
    else:
        assert result.returncode != 0, (args, result.stdout, result.stderr)
    return result.stdout.strip() if ok else result.stderr

run('git', 'clone', '--quiet', '--bare', tmp / 'remote.git', remote)
clones, records = [], []
for number in range(1, 4):
    clone = tmp / ('range-clone-%d' % number)
    run('git', 'clone', '--quiet', remote, clone)
    base = run('git', '-C', clone, 'rev-parse', 'HEAD')
    push_url = run('git', '-C', clone, 'remote', 'get-url', '--push', 'origin')
    def request_digest(command):
        return hashlib.sha256(json.dumps(
            [push_url, 'origin', 'main', command], ensure_ascii=True).encode()).hexdigest()[:24]
    reverify_gate, landing_gate = sorted(('true', 'true # reverify'), key=request_digest)
    for part in ('a', 'b'):
        name = '%d-%s.txt' % (number, part)
        (clone / name).write_text(name + '\n')
        run('git', '-C', clone, 'add', name)
        run('git', '-C', clone, 'commit', '-qm', name)
    sha = run('git', '-C', clone, 'rev-parse', 'HEAD')
    run(land, '--repo', clone, '--gate', landing_gate, '--wait', '--no-update', '--ci-wait', '1')
    status = json.loads(run(land, 'status', '--repo', clone, '--json'))
    receipt = Path(status['log']).with_suffix('.json')
    assert json.loads(receipt.read_bytes())['push']['base'] == base
    clones.append(clone)
    records.append((receipt, sha, base))

source, target = clones[2], clones[0]
receipt, sha, base = records[2]
directory = records[0][0].parent
original = receipt.read_bytes()
run(land, '--repo', source, '--gate', landing_gate, '--wait', '--no-update',
    '--ci-wait', '1', '--handoff-receipt-to', target)
imported = directory / receipt.name
assert imported.read_bytes() == original

# Exercise the actual plan range consumer without running its CLI dispatcher.
core = root / 'scripts/lib/agent_plan_core.py'
tree = ast.parse(core.read_text())
functions = [node for node in tree.body if isinstance(node, ast.FunctionDef) and
             node.name in {'_load', 'read_regular_bytes', 'landed_range_paths'}]
settings = {'OMS_REPO': str(target), 'OMS_LAND_RECEIPTS_DIR': str(directory)}
def die(message):
    raise ValueError(message)
namespace = dict(globals(), __file__=str(core), env=lambda key: settings.get(key, ''), die=die)
exec(compile(ast.Module(body=functions, type_ignores=[]), str(core), 'exec'), namespace)
def paths(raw):
    return namespace['landed_range_paths'](sha, hashlib.sha256(raw).hexdigest())
span, changed = paths(original)
assert span == base + '..' + sha, span
assert set(changed) == {'3-a.txt', '3-b.txt'}, changed

# A new gate on the same SHA must not shadow the real landing, even when its
# request filename sorts first. Completion retains the original receipt proof.
run(land, '--repo', source, '--gate', reverify_gate, '--wait', '--no-update', '--ci-wait', '1')
status = json.loads(run(land, 'status', '--repo', source, '--json'))
reverified = Path(status['log']).with_suffix('.json')
assert reverified.name < receipt.name
assert 'base' not in json.loads(reverified.read_bytes())['push']
assert json.loads(reverified.read_bytes())['push']['moved'] == 0
# Even without its landing receipt beside it, a reverify is never handed off as a landing.
alone = tmp / 'reverify-alone'
alone.mkdir()
(alone / reverified.name).write_bytes(reverified.read_bytes())
refused = run('python3', helper, source, target, alone / reverified.name, directory, ok=False)
assert 'hand off its landing receipt instead' in refused, refused
# A reverify has no range of its own; handing it off is refused in favour of the landing receipt.
refused = run('python3', helper, source, target, reverified, directory, ok=False)
assert 'hand off its landing receipt instead' in refused, refused
assert not (directory / reverified.name).exists()
plan = root / 'scripts/agent-plan.sh'
run(plan, '--repo', source, 'init', '--goal', 'reverify preserves landing range')
def finish_task(task):
    run(plan, '--repo', source, 'add', '--id', task, '--title', task, '--allowed', '3-a.txt')
    run(plan, '--repo', source, 'claim', '--id', task, '--provider', 'codex')
    run(plan, '--repo', source, 'review', '--id', task, '--artifact', 'result.md', '--patch', 'result.patch')
    run(plan, '--repo', source, 'finish', '--id', task, '--landed-commit', sha)
    row = json.loads(run(plan, '--repo', source, 'show', '--id', task))
    assert row['state'] == 'done'
    assert row['landing']['receipt_sha256'] == hashlib.sha256(original).hexdigest(), row
finish_task('after-reverify')

# Also tolerate an earlier receipt made by the old bug, without selecting its
# explicit SHA..SHA range or allowing the range consumer to evaluate it.
buggy = json.loads(reverified.read_bytes())
buggy['push']['base'] = sha
reverified.write_text(json.dumps(buggy) + '\n')
finish_task('after-empty-receipt')
settings.update(OMS_REPO=str(source), OMS_LAND_RECEIPTS_DIR=str(receipt.parent))
try:
    paths(reverified.read_bytes())
except ValueError as error:
    assert 'nonempty landed range' in str(error), error
else:
    raise AssertionError('range consumer accepted SHA..SHA')
reverified.unlink()

# Legacy bytes retain the old fallback and its target-history refusal.
legacy = json.loads(original)
del legacy['push']['base']
receipt.write_text(json.dumps(legacy) + '\n')
imported.unlink()
error = run('python3', helper, source, target, receipt, directory, ok=False)
assert "target receipts would change the producer's landed range" in error, error
assert not imported.exists()
settings.update(OMS_REPO=str(source), OMS_LAND_RECEIPTS_DIR=str(receipt.parent))
span, changed = paths(receipt.read_bytes())
assert span == run('git', '-C', source, 'rev-parse', sha + '^') + '..' + sha, span
assert changed == ['3-b.txt'], changed

# Even a locally available commit cannot serve as a non-ancestor base.
tree_sha = run('git', '-C', source, 'rev-parse', sha + '^{tree}')
unrelated = run('git', '-C', source, 'commit-tree', tree_sha, '-m', 'unrelated base')
run('git', '-C', target, 'fetch', '--quiet', '--no-write-fetch-head', source, unrelated)
forged = json.loads(original)
for forged_base in (unrelated, 'HEAD', None):
    forged['push']['base'] = forged_base
    receipt.write_text(json.dumps(forged) + '\n')
    run('python3', helper, source, target, receipt, directory, ok=False)
    assert not imported.exists()
    try:
        paths(receipt.read_bytes())
    except ValueError:
        pass
    else:
        raise AssertionError('range consumer accepted forged push.base')
receipt.write_bytes(original)

# A landing made from another checkout of the same repository proves a finish
# recorded in a checkout that never landed. The sibling state directory counts
# only when the receipt's request stamp binds this checkout's push URL.
def plan_finish(repo, task, ok, landed=None, allowed='3-a.txt'):
    landed = landed or sha
    run(plan, '--repo', repo, 'init', '--goal', 'sibling ' + task)
    run(plan, '--repo', repo, 'add', '--id', task, '--title', task, '--allowed', allowed)
    run(plan, '--repo', repo, 'claim', '--id', task, '--provider', 'codex')
    run(plan, '--repo', repo, 'review', '--id', task, '--artifact', 'result.md', '--patch', 'result.patch')
    return run(plan, '--repo', repo, 'finish', '--id', task, '--landed-commit', landed, ok=ok)
sibling = tmp / 'sibling-plan'
run('git', 'clone', '--quiet', remote, sibling)
plan_finish(sibling, 'sibling-same-remote', True)
row = json.loads(run(plan, '--repo', sibling, 'show', '--id', 'sibling-same-remote'))
assert row['state'] == 'done' and row['landing']['receipt_sha256'] == hashlib.sha256(original).hexdigest(), row
foreign = tmp / 'foreign-plan'
run('git', 'clone', '--quiet', '--bare', remote, tmp / 'range-foreign.git')
run('git', 'clone', '--quiet', tmp / 'range-foreign.git', foreign)
refused = plan_finish(foreign, 'sibling-other-remote', False)
assert 'no passed oms land receipt' in refused, refused
stale = tmp / 'stale-plan'
run('git', 'clone', '--quiet', remote, stale)
run('git', '-C', stale, 'update-ref', 'refs/remotes/origin/main', sha + '^')
refused = plan_finish(stale, 'sibling-stale-ref', False)
assert 'fetch origin first' in refused, refused

# A sibling candidate that is dangling, oversized or not a regular file belongs
# to another checkout and must not hide a valid receipt that sorts after it.
junk = receipt.parent.parent / 'a-junk-0-0'
junk.mkdir()
os.symlink(str(junk / 'missing'), str(junk / (sha + '-dangling.json')))
(junk / (sha + '-big.json')).write_bytes(b' ' * (1024 * 1024 + 1))
if hasattr(os, 'mkfifo'):
    os.mkfifo(str(junk / (sha + '-fifo.json')))
assert min(p.name for p in receipt.parent.parent.iterdir()) == junk.name
unblocked = tmp / 'junk-plan'
run('git', 'clone', '--quiet', remote, unblocked)
plan_finish(unblocked, 'sibling-junk', True)

# satisfy resolves its commit receipt through the same lookup as finish, so a
# receipt that only a sibling checkout holds is found; without one it is refused.
completion = runpy.run_path(str(root / 'scripts/lib/plan-completion.py'))
core_names = {'_load', 'read_regular_bytes', 'read_receipt', 'receipt_directories', 'sibling_receipt_binds_repo',
              'read_sibling_receipt', 'commit_landing_proof', 'split_list'}
core_ns = dict(globals(), __file__=str(core), env=lambda key: os.environ.get(key, ''), die=die)
exec(compile(ast.Module(body=[n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in core_names],
                        type_ignores=[]), str(core), 'exec'), core_ns)
product = completion['commit_product']
product.__globals__['_COMMIT_PROOF'] = core_ns['commit_landing_proof']
def satisfy_receipt(checkout, digest):
    old = {k: os.environ.get(k) for k in ('OMS_REPO', 'OMS_LAND_RECEIPTS_DIR')}
    canonical = completion['canonical_land_directory'](str(checkout))
    os.environ.update(OMS_REPO=str(checkout), OMS_LAND_RECEIPTS_DIR=canonical)
    try:
        product(str(checkout), {'successor': {'commit_sha': sha, 'land_receipt_sha256': digest},
                                'base_commit': '0' * 40},
                {'landing': {'kind': 'commit', 'sha': sha, 'receipt_sha256': digest}})
    except (ValueError, SystemExit) as error:
        return str(error)
    finally:
        for k, v in old.items():
            os.environ.pop(k, None) if v is None else os.environ.__setitem__(k, v)
    raise AssertionError('commit_product accepted a bogus base')
sibling_only = satisfy_receipt(unblocked, hashlib.sha256(original).hexdigest())
assert sibling_only == 'completion: local Git proof unavailable', sibling_only
refused = satisfy_receipt(foreign, hashlib.sha256(original).hexdigest())
assert 'no passed oms land receipt' in refused, refused

# A FIFO swapped in for a checked receipt, right before the leaf open, must be
# refused instead of blocking the reader.
if hasattr(os, 'mkfifo'):
    swapped = tmp / 'swapped-fifo'
    swapped.write_text('{}\n')
    probe = (
        'import importlib.util as u, os, sys\n'
        'spec = u.spec_from_file_location("durable_jsonl", sys.argv[1])\n'
        'm = u.module_from_spec(spec); spec.loader.exec_module(m)\n'
        'check = m.components_unchanged\n'
        'def swap(*args):\n'
        '    check(*args)\n'
        '    os.unlink(sys.argv[3]); os.mkfifo(sys.argv[3])\n'
        'm.components_unchanged = swap\n'
        'm.read_no_follow(sys.argv[2], sys.argv[3])\n')
    done = subprocess.run([sys.executable, '-c', probe, str(root / 'scripts/lib/durable-jsonl.py'),
                           str(tmp), str(swapped)], capture_output=True, text=True, timeout=30)
    assert done.returncode != 0 and 'regular' in done.stderr, done
    swapped.unlink()

# Both lookups read candidates through the durable no-follow reader, so a
# hard-linked receipt is refused, and the schema must be the integer 1.
def refusal(call):
    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        try:
            outcome = call()
        except (ValueError, SystemExit) as error:
            return str(error) + err.getvalue()
        if isinstance(outcome, str):
            return outcome + err.getvalue()
    raise AssertionError('receipt lookup accepted a refused candidate')
strict = tmp / 'strict-receipts'
strict.mkdir()
candidate = strict / receipt.name
candidate.write_bytes(original)
assert core_ns['read_receipt'](str(candidate)) == original
os.link(str(candidate), str(tmp / 'strict-receipt-link'))
assert 'hard-linked' in refusal(lambda: core_ns['read_receipt'](str(candidate)))
os.environ['OMS_LAND_RECEIPTS_DIR'] = str(strict)
try:
    assert 'hard-linked' in refusal(lambda: core_ns['commit_landing_proof'](sha))
finally:
    os.environ.pop('OMS_LAND_RECEIPTS_DIR', None)
canonical = Path(completion['canonical_land_directory'](str(unblocked)))
canonical.mkdir(parents=True, exist_ok=True)
(canonical / receipt.name).write_bytes(original)
os.link(str(canonical / receipt.name), str(tmp / 'canonical-receipt-link'))
try:
    assert 'hard-linked' in refusal(lambda: satisfy_receipt(unblocked, hashlib.sha256(original).hexdigest()))
finally:
    (canonical / receipt.name).unlink()
candidate.unlink()
os.unlink(str(tmp / 'strict-receipt-link'))
os.environ['OMS_LAND_RECEIPTS_DIR'] = str(strict)
try:
    for bad in (True, 1.0):
        candidate.write_text(json.dumps(dict(json.loads(original), schema=bad)) + '\n')
        assert 'no passed oms land receipt' in refusal(lambda: core_ns['commit_landing_proof'](sha))
        candidate.unlink()
finally:
    os.environ.pop('OMS_LAND_RECEIPTS_DIR', None)

# Relative remote URLs name different repositories in different checkouts, so
# identical request stamps prove nothing: no receipt is shared between them.
rel = {}
for label in ('a', 'b'):
    base_dir = tmp / ('rel-' + label)
    base_dir.mkdir()
    if label == 'a':
        run('git', 'clone', '--quiet', '--bare', remote, base_dir / 'remote.git')
    else:
        run('git', 'clone', '--quiet', '--bare', rel['a'].parent / 'remote.git', base_dir / 'remote.git')
    work = base_dir / 'work'
    run('git', 'clone', '--quiet', base_dir / 'remote.git', work)
    run('git', '-C', work, 'remote', 'set-url', 'origin', '../remote.git')
    rel[label] = work
    if label == 'a':
        (work / 'r.txt').write_text('r\n')
        run('git', '-C', work, 'add', 'r.txt')
        run('git', '-C', work, 'commit', '-qm', 'r.txt')
        rsha = run('git', '-C', work, 'rev-parse', 'HEAD')
        run(land, '--repo', work, '--gate', 'true', '--wait', '--no-update', '--ci-wait', '1')
        run('git', '-C', work, 'fetch', '--quiet', 'origin')
refused = plan_finish(rel['b'], 'relative-url', False, landed=rsha, allowed='r.txt')
assert 'no passed oms land receipt' in refused, refused
PYRANGE
# Explicit handoff preserves clone isolation until gate, push and CI pass.
# The target can remain dirty and behind: only its destination ref is fetched.
handoff="$TMP/handoff clone"
git clone -q "$TMP/remote.git" "$handoff" 2>/dev/null
handoff_head="$(git -C "$handoff" rev-parse HEAD)"
printf 'target work\n' > "$handoff/local.txt"
handoff_status="$(git -C "$handoff" status --porcelain)"
handoff_registry="$(registry_snapshot "$handoff")"
handoff_dir="$("$LAND" status --repo "$handoff" 2>/dev/null | sed -n 's/^no landing recorded under //p' || true)"
[ -n "$handoff_dir" ] || fail "cannot resolve handoff namespace"
for handoff_failure in gate ci skipped; do
  if [ "$handoff_failure" = gate ]; then gate 'exit 3'; else gate "echo handoff $handoff_failure"; fi
  handoff_wait=1
  handoff_ci=failure
  [ "$handoff_failure" != skipped ] || handoff_wait=0
  if OMS_TEST_CI_RESULT="$handoff_ci" "$LAND" --repo "$repo" --wait --no-update \
      --ci-wait "$handoff_wait" --handoff-receipt-to "$handoff" --sync-shared "$handoff" > "$TMP/handoff-failed.out" 2>&1; then
    fail "handoff accepted $handoff_failure: $(cat "$TMP/handoff-failed.out")"
  fi
  [ ! -d "$handoff_dir" ] || [ -z "$(find "$handoff_dir" -name '*.json' -print)" ] ||
    fail "failed/skipped-CI land imported receipts"
  [ ! -d "$receipt_dir/sync" ] || fail "failed land/handoff attempted shared sync"
done
predecessor="$(git -C "$repo" rev-parse HEAD)"
predecessor_receipt="$(find "$receipt_dir" -name "$predecessor-*.json" -print)"
gate 'echo handoff success'
OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  --handoff-receipt-to "$handoff" > "$TMP/handoff.out" 2>&1 || fail "handoff failed: $(cat "$TMP/handoff.out")"
handoff_sha="$(git -C "$repo" rev-parse HEAD)"
handoff_receipt="$(find "$receipt_dir" -name "$handoff_sha-*.json" -print)"
cmp "$handoff_receipt" "$handoff_dir/$(basename "$handoff_receipt")" || fail "receipt handoff changed bytes"
cmp "$predecessor_receipt" "$handoff_dir/$(basename "$predecessor_receipt")" || fail "predecessor handoff changed bytes"
[ "$(find "$handoff_dir" -name '*.json' -print | wc -l | tr -d ' ')" = 2 ] || fail "handoff imported unrelated receipts"
[ "$(git -C "$handoff" rev-parse HEAD)" = "$handoff_head" ] || fail "handoff moved target HEAD"
[ ! -d "$receipt_dir/sync" ] || fail "handoff without --sync-shared attempted sync"
[ "$(git -C "$handoff" status --porcelain)" = "$handoff_status" ] || fail "handoff changed target work"
[ "$(registry_snapshot "$handoff")" = "$handoff_registry" ] || fail "handoff changed target worktree registry"
git -C "$handoff" merge-base --is-ancestor "$handoff_sha" origin/main || fail "target destination was not fetched"
# Replaying a completed producer request reuses the exact immutable bytes.
OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  --handoff-receipt-to "$handoff" > "$TMP/handoff-repeat.out" 2>&1 || fail "identical handoff not idempotent"
cmp "$handoff_receipt" "$handoff_dir/$(basename "$handoff_receipt")" || fail "handoff retry changed receipt"
printf 'conflict\n' > "$handoff_dir/$(basename "$handoff_receipt")"
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 --handoff-receipt-to "$handoff" \
    > "$TMP/handoff-conflict.out" 2>&1; then fail "handoff overwrote conflicting receipt"; fi
[ "$(cat "$handoff_dir/$(basename "$handoff_receipt")")" = conflict ] || fail "conflicting receipt changed"
rm "$handoff_dir/$(basename "$handoff_receipt")"
# A matching history does not authorize a different remote destination.
foreign="$TMP/foreign"
git clone -q "$TMP/remote.git" "$foreign" 2>/dev/null
git init -q --bare -b main "$TMP/foreign.git"
git -C "$foreign" remote set-url origin "$TMP/foreign.git"
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 --handoff-receipt-to "$foreign" \
    > "$TMP/handoff-foreign.out" 2>&1; then fail "handoff accepted foreign remote"; fi
foreign_state="$("$LAND" status --repo "$foreign" 2>/dev/null | sed -n 's/^no landing recorded under //p' || true)"
[ -n "$foreign_state" ] && [ -z "$(find "$foreign_state" -name '*.json' -print 2>/dev/null)" ] || fail "foreign target received receipts"
# A prior --no-update receipt cannot suppress an installation requested now.
: > "$TMP/events"
OMS_TEST_CI_RC=4 OMS_TEST_CI_SCOPE=full OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 --handoff-receipt-to "$handoff" \
  > "$TMP/handoff-install.out" 2>&1 || fail "installing handoff retry failed: $(cat "$TMP/handoff-install.out")"
[ "$(cat "$TMP/events")" = "$(printf 'jobs\nupdate')" ] ||
  fail "handoff retry skipped the requested installation"
python3 - "$handoff_receipt" <<'PYINSTALL' || fail "handoff retry omitted install evidence"
import json, sys
receipt = json.load(open(sys.argv[1]))
assert receipt['update']['rc'] == 0 and receipt['update']['reason'] == 'installed', receipt
PYINSTALL
cmp "$handoff_receipt" "$handoff_dir/$(basename "$handoff_receipt")" || fail "installed handoff changed receipt bytes"

# Keep a pushed SHA with failed CI, then advance main before its CI-only retry.
gate 'echo advanced CI resume probe'
: > "$TMP/events"
if OMS_TEST_CI_RESULT=cancelled "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/ci-ancestor-initial.out" 2>&1; then
  fail "failed CI unexpectedly passed before the ancestor retry"
fi
ci_retry_sha="$(git -C "$repo" rev-parse HEAD)"
ci_retry_receipt="$(find "$receipt_dir" -name "$ci_retry_sha-*.json" -print)"
[ -n "$ci_retry_receipt" ] || fail "failed-CI retry receipt was not recorded"
python3 - "$ci_retry_receipt" <<'PYCIFAILED' || fail "cancelled-CI receipt did not retain its conclusion"
import json, sys
assert json.load(open(sys.argv[1]))['ci']['conclusion'] == 'cancelled'
PYCIFAILED
# A tracked-tree change during the remote-tip lookup must refuse before
# changing the receipt, even while the remote still equals the verified SHA.
ci_retry_request="$(basename "$ci_retry_receipt" .json)"
ci_retry_gate="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["gate"]["command"])' "$ci_retry_receipt")"
ci_retry_log="$(log_of "$ci_retry_receipt")"
cp "$ci_retry_receipt" "$TMP/ci-dirty-receipt.json"
cp "$repo/scripts/check.sh" "$TMP/ci-dirty-check.sh"
real_git="$(command -v git)"
mkdir -p "$TMP/intercept"
cat > "$TMP/intercept/git" <<'EOF'
#!/usr/bin/env bash
"$OMS_TEST_REAL_GIT" "$@"
rc=$?
if [ "$1" = ls-remote ] && [ -n "${OMS_TEST_DIRTY_LOOKUP_FILE:-}" ] &&
    [ ! -e "$OMS_TEST_DIRTY_LOOKUP_MARKER" ]; then
  : > "$OMS_TEST_DIRTY_LOOKUP_MARKER"
  printf 'dirty during lookup\n' >> "$OMS_TEST_DIRTY_LOOKUP_FILE"
fi
exit "$rc"
EOF
chmod +x "$TMP/intercept/git"
: > "$TMP/events"
if PATH="$TMP/intercept:$PATH" OMS_TEST_REAL_GIT="$real_git" \
    OMS_TEST_DIRTY_LOOKUP_FILE="$repo/scripts/check.sh" \
    OMS_TEST_DIRTY_LOOKUP_MARKER="$TMP/dirty-lookup-once" OMS_TEST_CI_RC=4 \
    "$LAND" --run-job "$ci_retry_request" --repo "$repo" --gate "$ci_retry_gate" \
      --no-update --ci-wait 1 > "$TMP/ci-dirty-resume.out" 2>&1; then
  fail "resume accepted a tracked-tree change during remote lookup"
fi
cmp "$ci_retry_receipt" "$TMP/ci-dirty-receipt.json" ||
  fail "dirty-tree resume changed receipt bytes before refusing"
[ "$(remote_tip)" = "$ci_retry_sha" ] || fail "dirty-tree resume changed the remote tip"
cp "$TMP/ci-dirty-check.sh" "$repo/scripts/check.sh"
# Another main can advance the branch before an explicit receipt-only retry.
advanced_main="$TMP/advancing-main"
advanced_target="$TMP/handoff-after-advance"
git clone -q "$TMP/remote.git" "$advanced_main" 2>/dev/null
git clone -q "$TMP/remote.git" "$advanced_target" 2>/dev/null
printf 'later main\n' > "$advanced_main/later-main.txt"
git -C "$advanced_main" add later-main.txt
git -C "$advanced_main" commit -qm 'later main'
git -C "$advanced_main" push -q origin HEAD:main
advanced_tip="$(git -C "$advanced_main" rev-parse HEAD)"
advanced_dir="$("$LAND" status --repo "$advanced_target" 2>/dev/null | sed -n 's/^no landing recorded under //p' || true)"
[ -n "$advanced_dir" ] || fail "cannot resolve advanced handoff namespace"
: > "$TMP/events"
git -C "$repo" checkout -q "$handoff_sha"
OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  --handoff-receipt-to "$advanced_target" > "$TMP/handoff-ancestor.out" 2>&1 ||
  fail "reachable ancestor handoff refused: $(cat "$TMP/handoff-ancestor.out")"
[ ! -s "$TMP/events" ] || fail "receipt-only ancestor retry repeated CI or install"
cmp "$handoff_receipt" "$advanced_dir/$(basename "$handoff_receipt")" || fail "ancestor handoff changed receipt bytes"
[ "$(git -C "$advanced_target" rev-parse origin/main)" = "$advanced_tip" ] || fail "ancestor handoff did not fetch the descendant"
[ "$(git -C "$repo" rev-parse HEAD)" = "$handoff_sha" ] || fail "ancestor handoff moved producer HEAD"
git -C "$repo" checkout -q "$ci_retry_sha"
[ "$(git -C "$repo" rev-parse HEAD)" = "$ci_retry_sha" ] && [ "$(remote_tip)" = "$advanced_tip" ] ||
  fail "receipt-only ancestor retry moved a local or remote branch"
# Zero remains the documented no-query budget. An ancestor CI-only retry must
# refuse before mutating the receipt or invoking the CI query.
cp "$ci_retry_receipt" "$TMP/ci-zero-wait-receipt.json"
: > "$TMP/events"
if OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 \
    > "$TMP/ci-zero-wait.out" 2>&1; then
  fail "ancestor CI-only retry accepted --ci-wait 0"
fi
grep -q -- '--ci-wait 0 skips CI queries' "$TMP/ci-zero-wait.out" ||
  fail "zero-wait ancestor refusal did not explain the skipped query"
[ ! -s "$TMP/events" ] || fail "zero-wait ancestor retry queried CI"
cmp "$ci_retry_receipt" "$TMP/ci-zero-wait-receipt.json" ||
  fail "zero-wait ancestor refusal mutated the receipt"
# A matching SHA and request are insufficient when gate/push evidence is not
# successful; restore the original bytes before the actual CI resume.
cp "$ci_retry_receipt" "$TMP/ci-ancestor-receipt.json"
python3 - "$ci_retry_receipt" <<'PYNONRESUMABLE'
import json, sys
path = sys.argv[1]
receipt = json.load(open(path))
receipt['push']['rc'] = 1
with open(path, 'w') as stream:
    json.dump(receipt, stream)
PYNONRESUMABLE
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/ci-ancestor-nonresumable.out" 2>&1; then
  fail "non-resumable ancestor receipt was accepted"
fi
grep -q 'rebase first' "$TMP/ci-ancestor-nonresumable.out" ||
  fail "non-resumable receipt lost its intake ancestry guard"
mv "$TMP/ci-ancestor-receipt.json" "$ci_retry_receipt"
# The failed-CI receipt may resume only the exact pushed SHA, without rerunning
# its gate or push. Ordinary and installing retries retain the ancestry refusal.
if "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/ci-ancestor-ordinary.out" 2>&1; then
  fail "ordinary retry accepted a pushed SHA behind main"
fi
grep -q 'rebase first' "$TMP/ci-ancestor-ordinary.out" ||
  fail "ordinary retry lost its intake ancestry guard"
if "$LAND" --repo "$repo" --wait --ci-wait 1 --handoff-receipt-to "$advanced_target" \
    > "$TMP/ci-ancestor-install.out" 2>&1; then
  fail "installing retry accepted a pushed SHA behind main"
fi
grep -q 'rebase first' "$TMP/ci-ancestor-install.out" ||
  fail "installing retry lost its intake ancestry guard"
: > "$TMP/events"
ci_retry_log="$(log_of "$ci_retry_receipt")"
gate_lines_before="$(grep -c '^advanced CI resume probe$' "$ci_retry_log" || true)"
push_lines_before="$(grep -c '^To ' "$ci_retry_log" || true)"
real_python3="$(command -v python3)"
mkdir -p "$TMP/interrupt-success"
cat > "$TMP/interrupt-success/python3" <<'EOF'
#!/usr/bin/env bash
"$OMS_TEST_REAL_PYTHON3" "$@"
rc=$?
if [ "$rc" -eq 0 ] && [ "${OMS_TEST_INTERRUPT_CI_SUCCESS:-0}" = 1 ]; then
  for arg in "$@"; do
    if [ "$arg" = ci.conclusion=success ]; then
      kill -KILL "$PPID"
    fi
  done
fi
exit "$rc"
EOF
chmod +x "$TMP/interrupt-success/python3"
if PATH="$TMP/interrupt-success:$PATH" OMS_TEST_REAL_PYTHON3="$real_python3" \
    OMS_TEST_INTERRUPT_CI_SUCCESS=1 OMS_TEST_CI_RESULT=success \
    "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
      > "$TMP/ci-ancestor-interrupted.out" 2>&1; then
  fail "interrupted ancestor CI retry unexpectedly finished"
fi
[ "$(cat "$TMP/events")" = ci ] || fail "interrupted ancestor resume omitted or repeated its CI query"
python3 - "$ci_retry_receipt" "$ci_retry_sha" <<'PYCIINTERRUPTED' || fail "CI-success interruption did not leave a running exact-SHA receipt"
import json, sys
receipt = json.load(open(sys.argv[1]))
assert receipt['sha'] == sys.argv[2], receipt
assert receipt['ci']['conclusion'] == 'success', receipt
assert receipt['state'] == 'running', receipt
PYCIINTERRUPTED
grep -c '^advanced CI resume probe$' "$ci_retry_log" | grep -qx "$gate_lines_before" ||
  fail "interrupted ancestor CI resume repeated its gate"
grep -c '^To ' "$ci_retry_log" | grep -qx "$push_lines_before" ||
  fail "interrupted ancestor CI resume repeated its push"
[ "$(git -C "$repo" rev-parse HEAD)" = "$ci_retry_sha" ] &&
  [ "$(remote_tip)" = "$advanced_tip" ] || fail "interrupted ancestor CI resume moved a branch"
: > "$TMP/events"
OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  > "$TMP/ci-ancestor-finalize.out" 2>&1 ||
  fail "running ancestor receipt with CI success did not finalize: $(cat "$TMP/ci-ancestor-finalize.out")"
[ ! -s "$TMP/events" ] || fail "CI-success finalization queried CI or installed"
grep -c '^advanced CI resume probe$' "$ci_retry_log" | grep -qx "$gate_lines_before" ||
  fail "CI-success finalization repeated its gate"
grep -c '^To ' "$ci_retry_log" | grep -qx "$push_lines_before" ||
  fail "CI-success finalization repeated its push"
[ "$(git -C "$repo" rev-parse HEAD)" = "$ci_retry_sha" ] &&
  [ "$(remote_tip)" = "$advanced_tip" ] || fail "ancestor CI finalization moved a branch"
python3 - "$ci_retry_receipt" "$ci_retry_sha" <<'PYCIRESUME' || fail "ancestor CI result was not bound to its SHA"
import json, sys
receipt = json.load(open(sys.argv[1]))
assert receipt['sha'] == sys.argv[2], receipt
assert receipt['ci']['conclusion'] == 'success', receipt
assert receipt['state'] == 'passed', receipt
PYCIRESUME
: > "$TMP/events"
OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  --handoff-receipt-to "$advanced_target" > "$TMP/ci-ancestor-handoff.out" 2>&1 ||
  fail "passed ancestor CI receipt did not hand off: $(cat "$TMP/ci-ancestor-handoff.out")"
[ ! -s "$TMP/events" ] || fail "passed receipt handoff re-queried CI"
cmp "$ci_retry_receipt" "$advanced_dir/$(basename "$ci_retry_receipt")" ||
  fail "ancestor CI handoff changed receipt bytes"
# The relaxed reachability rule grants no authority to push or install an old SHA.
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 > "$TMP/ancestor-push.out" 2>&1; then
  fail "ordinary landing accepted an old ancestor"
fi
grep -q 'rebase first' "$TMP/ancestor-push.out" || fail "ordinary landing lost its intake ancestry guard"
if "$LAND" --repo "$repo" --wait --ci-wait 1 --handoff-receipt-to "$advanced_target" \
    > "$TMP/ancestor-install.out" 2>&1; then fail "installing retry accepted an old ancestor"; fi
grep -q 'rebase first' "$TMP/ancestor-install.out" || fail "installing retry lost its intake ancestry guard"
request="$(basename "$ci_retry_receipt" .json)"
recorded_gate="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["gate"]["command"])' "$ci_retry_receipt")"
if "$LAND" --run-job "$request" --repo "$repo" --gate "$recorded_gate" --no-update --ci-wait 1 \
    > "$TMP/ancestor-job.out" 2>&1; then fail "ordinary job accepted a different remote tip"; fi
grep -q 'cannot resume: remote tip' "$TMP/ancestor-job.out" || fail "ordinary job lost tip equality"
cmp "$handoff_receipt" "$advanced_dir/$(basename "$handoff_receipt")" || fail "refused old-SHA work changed the receipt"
# A CI-resumed receipt is insufficient once its SHA is no longer reachable.
git --git-dir="$TMP/remote.git" update-ref refs/heads/main "$predecessor"
unreachable_target="$TMP/handoff-unreachable"
git clone -q "$TMP/remote.git" "$unreachable_target" 2>/dev/null
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 --handoff-receipt-to "$unreachable_target" \
    > "$TMP/handoff-unreachable.out" 2>&1; then fail "handoff accepted an unreachable landed SHA"; fi
if "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/ci-unreachable.out" 2>&1; then fail "CI-only resume accepted an unreachable SHA"; fi
grep -q 'cannot resume: remote tip' "$TMP/ci-unreachable.out" ||
  fail "unreachable CI-only resume lost its remote-tip refusal"
[ ! -s "$TMP/events" ] || fail "unreachable CI-only resume queried CI"
unreachable_dir="$("$LAND" status --repo "$unreachable_target" 2>/dev/null | sed -n 's/^no landing recorded under //p' || true)"
[ -n "$unreachable_dir" ] && [ ! -e "$unreachable_dir/$(basename "$handoff_receipt")" ] ||
  fail "unreachable target received a receipt"
[ "$(remote_tip)" = "$predecessor" ] || fail "unreachable receipt retry attempted to push"
git --git-dir="$TMP/remote.git" update-ref refs/heads/main "$advanced_tip"
git -C "$repo" merge -q --ff-only "$advanced_tip"

# Changing the producer push URL during CI cannot redirect an earlier receipt.
gate 'echo handoff changed destination'
if OMS_TEST_CI_RESULT=success OMS_TEST_LAND_CHANGE_PUSH_URL="$TMP/foreign.git" \
    "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 --handoff-receipt-to "$foreign" \
    > "$TMP/handoff-changed.out" 2>&1; then fail "handoff followed a changed push destination"; fi
changed_receipt="$(find "$receipt_dir" -name "$(git -C "$repo" rev-parse HEAD)-*.json" -print)"
grep -q 'producer receipt push destination changed' "$(log_of "$changed_receipt")" ||
  fail "handoff did not bind the original push destination"
git -C "$repo" remote set-url --delete --push origin '.*'

# A leading-zero budget is decimal, not an octal error after the push.
gate 'echo zero padded wait probe'
OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 08 \
  > "$TMP/padded.out" 2>&1 || fail "--ci-wait 08 must mean eight seconds: $(cat "$TMP/padded.out")"
"$LAND" status --repo "$repo" | grep -q 'update: skipped (no-update)' ||
  fail "a skipped update must name its reason: $("$LAND" status --repo "$repo")"
# A commit without workflows has no run to wait for.
git -C "$repo" rm -q .github/workflows/test.yml
gate 'echo no workflow probe'
: > "$TMP/events"
OMS_TEST_CI_RESULT=missing "$LAND" --repo "$repo" --wait --no-update --ci-wait 30 \
  > "$TMP/no-workflow.out" 2>&1 || fail "a repo without workflows must not wait for CI: $(cat "$TMP/no-workflow.out")"
! grep -q ci "$TMP/events" || fail "CI was queried for a repo without workflows"
mkdir -p "$repo/.github/workflows"
printf 'on: push\n' > "$repo/.github/workflows/test.yml"
git -C "$repo" add .github/workflows/test.yml
gate 'echo update failure probe; echo "check: ok"'
if OMS_TEST_CI_RESULT=success OMS_INSTALL_RECEIPT="$TMP/install.json" "$LAND" --repo "$repo" --wait --ci-wait 1 \
  --handoff-receipt-to "$handoff" > "$TMP/update-failed.out" 2>&1; then
  fail "installation failure was reported as a successful landing"
fi
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] || fail "update failure lost the successful push"
grep -q 'install update exit 42' "$TMP/update-failed.out" || fail "update failure reason missing"
[ -z "$(find "$handoff_dir" -name "$(git -C "$repo" rev-parse HEAD)-*.json" -print)" ] ||
  fail "failed install transferred a receipt"
python3 - "$receipt_dir" "$(git -C "$repo" rev-parse HEAD)" <<'PY' || fail "update failure receipt is wrong"
import json, pathlib, sys
rows = [json.loads(path.read_text()) for path in pathlib.Path(sys.argv[1]).glob("*.json")]
matches = [row for row in rows if row.get("sha") == sys.argv[2]]
assert len(matches) == 1, matches
r = matches[0]
assert r["state"] == "failed" and r["push"]["rc"] == 0 and r["update"]["rc"] == 42, r
PY

# Retry the already-pushed commit: neither its gate nor successful CI repeats.
: > "$TMP/events"
OMS_TEST_CI_RC=4 OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/resume.out" 2>&1 ||
  fail "failed update could not resume: $(cat "$TMP/resume.out")"
[ "$(cat "$TMP/events")" = update ] || fail "resume repeated successful CI"
receipt="$("$LAND" status --repo "$repo" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["log"])')"
[ "$(grep -c '^update failure probe$' "$receipt")" = 1 ] || fail "resume repeated the gate"
: > "$TMP/events"
OMS_TEST_CI_RC=4 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/resume-complete.out" 2>&1 ||
  fail "completed landing retry failed"
[ ! -s "$TMP/events" ] || fail "completed landing repeated CI/install"

# A receipt cannot hide a later rollback of the installed checkout.
git -C "$install" update-ref HEAD "$(git -C "$repo" rev-parse HEAD^)"
: > "$TMP/events"
OMS_TEST_CI_RC=4 OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/resume-rollback.out" 2>&1 ||
  fail "rolled-back installation did not resume"
[ "$(cat "$TMP/events")" = update ] || fail "old receipt hid installation rollback"
: > "$TMP/events"

# A different gate or destination cannot borrow a prior successful receipt:
# an explicit gate re-verifies the pushed SHA itself, and a gate without full
# proof installs nothing.
OMS_TEST_CI_RC=4 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --gate 'echo other gate ran' --ci-wait 0 > "$TMP/other-gate.out" 2>&1 ||
  fail "an explicit gate on an already-pushed commit failed: $(cat "$TMP/other-gate.out")"
! grep -q 'already pushed and installed' "$TMP/other-gate.out" || fail "different gate reused prior evidence"
grep -q 'install needs a full gate' "$TMP/other-gate.out" || fail "a gate without full proof installed"
git clone -q --bare "$TMP/remote.git" "$TMP/other-remote.git"
git -C "$repo" remote set-url origin "$TMP/other-remote.git"
OMS_TEST_CI_RC=4 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/other-remote.out" 2>&1 ||
  fail "already-pushed commit with another destination should be a no-op"
git -C "$repo" remote set-url origin "$TMP/remote.git"
grep -q 'no matching push receipt' "$TMP/other-remote.out" || fail "different destination reused prior evidence"
[ ! -s "$TMP/events" ] || fail "mismatched evidence triggered CI/install"

for ci_failure in auth timeout; do
  gate "echo $ci_failure resume probe"
  : > "$TMP/events"
  ci_rc=4 ci_delay=0
  [ "$ci_failure" != timeout ] || { ci_rc=0; ci_delay=10; }
  started="$(date +%s)"
  if OMS_TEST_CI_RESULT=pending OMS_TEST_CI_RC="$ci_rc" OMS_TEST_CI_DELAY="$ci_delay" \
      "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 > "$TMP/query-fail.out" 2>&1; then
    fail "CI $ci_failure should fail explicitly"
  fi
  [ "$(( $(date +%s) - started ))" -lt 8 ] || fail "CI query did not obey the deadline"
  [ "$(cat "$TMP/events")" = ci ] || fail "CI query errors were polled repeatedly"
  grep -q 'CI query exit' "$TMP/query-fail.out" || fail "CI error reason missing"
  grep -q '^  install: not reached (land failed before install)$' "$TMP/query-fail.out" ||
    fail "failed land must say install was not reached: $(cat "$TMP/query-fail.out")"
  ! grep -q 'reason not recorded' "$TMP/query-fail.out" ||
    fail "failed land must not report an unrecorded install skip"
  ! grep -q '^  active: unknown$' "$TMP/query-fail.out" ||
    fail "terminal land must not report unknown lock activity"
  : > "$TMP/events"
  OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/query-resume.out" 2>&1 || fail "CI error could not resume"
  [ "$(cat "$TMP/events")" = ci ] || fail "CI resume omitted its query"
  receipt="$("$LAND" status --repo "$repo" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["log"])')"
  [ "$(grep -c "^$ci_failure resume probe$" "$receipt")" = 1 ] || fail "CI resume repeated the gate"
done

# One active landing across worktrees, with flock and the portable mkdir lock.
# Contenders wait up to the shared file-lock timeout; detached startup reports
# waiting, then confirms only after lock acquisition and request admission.
for lock_kind in 0 1; do
  export OMS_TEST_LAND_COUNT="$TMP/count-$lock_kind" OMS_TEST_LAND_RELEASE="$TMP/release-$lock_kind"
  # flock leaves a file; mkdir uses a directory at that path. Model separate hosts.
  export OMS_LOCK_DIR="$TMP/locks-$lock_kind"
  gate 'echo gate >> "$OMS_TEST_LAND_COUNT"; for ((i=0;i<200;i++)); do [ ! -f "$OMS_TEST_LAND_RELEASE" ] || exit 0; sleep 0.05; done; exit 1'" # lock $lock_kind"
  OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 > "$TMP/first.out" 2>&1 &
  first_pid=$!
  for _ in $(seq 1 100); do [ ! -f "$OMS_TEST_LAND_COUNT" ] || break; sleep 0.05; done
  [ -f "$OMS_TEST_LAND_COUNT" ] || fail "first landing did not start: $(cat "$TMP/first.out")"
  # The probe sees the held lock without taking, truncating or removing it.
  for _ in $(seq 1 100); do
    OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" status --repo "$repo" --json 2>/dev/null | grep -q '"active": true' && break
    # Some Linux containers expose an empty /proc/locks even to a process
    # holding flock; in that environment the documented probe cannot observe
    # this lock, so validate the running receipt and the actual contention below.
    if [ "$lock_kind" = 0 ] && python3 -c 'import json,sys; assert json.load(sys.stdin)["active"] is None' \
        < <(OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" --json 2>/dev/null); then
      break
    fi
    sleep 0.05
  done
  OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" status --repo "$repo" --json > "$TMP/active.json" ||
    fail "status failed while a land was active"
  if python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["active"] is True' "$TMP/active.json" 2>/dev/null; then
    python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["step"]=="gate" and r["minutes"]>=0, r' "$TMP/active.json" ||
      fail "status must report the active gate step: $(cat "$TMP/active.json")"
    OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" status --repo "$repo" | grep -q '^land .*: running (lock held)' ||
      fail "text status must say the lock is held"
    active_rc=0
    OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" status --repo "$repo" --active-exit >/dev/null || active_rc=$?
    [ "$active_rc" = 3 ] || fail "--active-exit must exit 3 while a land is active: $active_rc"
  elif [ "$lock_kind" = 0 ] && python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["active"] is None' "$TMP/active.json" 2>/dev/null; then
    python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["state"]=="running" and r["gate"]["rc"]=="pending", r' "$TMP/active.json" ||
      fail "flock receipt must show a running gate when lock observation is unknown: $(cat "$TMP/active.json")"
    unknown_rc=0
    OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" --active-exit >/dev/null || unknown_rc=$?
    [ "$unknown_rc" = 4 ] || fail "--active-exit must distinguish unknown lock state: $unknown_rc"
    OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" | grep -q '^  active: unknown$' ||
      fail "text status must identify unknown lock activity"
  else
    fail "status failed to detect the held landing lock: $(cat "$TMP/active.json")"
  fi
  second_rc=0
  OMS_LOCK_FORCE_MKDIR="$lock_kind" OMS_LOCK_TIMEOUT=1 "$LAND" --repo "$repo" \
    --no-update --ci-wait 0 > "$TMP/second-timeout.out" 2>&1 || second_rc=$?
  [ "$second_rc" = 75 ] || fail "land lock wait must honor OMS_LOCK_TIMEOUT: $(cat "$TMP/second-timeout.out")"
  grep -q 'land not started (exit 75)' "$TMP/second-timeout.out" ||
    fail "detached lock timeout must report its actual exit code: $(cat "$TMP/second-timeout.out")"
  OMS_LOCK_FORCE_MKDIR="$lock_kind" OMS_LOCK_TIMEOUT=10 "$LAND" --repo "$repo" \
    --no-update --ci-wait 0 > "$TMP/second.out" 2>&1 &
  second_pid=$!
  for _ in $(seq 1 100); do
    grep -q 'waiting for active landing lock' "$TMP/second.out" && break
    sleep 0.05
  done
  grep -q 'waiting for active landing lock' "$TMP/second.out" || {
    touch "$OMS_TEST_LAND_RELEASE"
    wait "$first_pid" || true
    wait "$second_pid" || true
    fail "detached contender must report bounded lock waiting: $(cat "$TMP/second.out")"
  }
  ! grep -q '^receipt:' "$TMP/second.out" || {
    touch "$OMS_TEST_LAND_RELEASE"
    wait "$first_pid" || true
    wait "$second_pid" || true
    fail "a waiting contender must not advertise a receipt"
  }
  touch "$OMS_TEST_LAND_RELEASE"
  wait "$first_pid" || fail "first landing failed: $(cat "$TMP/first.out")"
  wait "$second_pid" || fail "queued same-commit retry did not resume: $(cat "$TMP/second.out")"
  grep -q '^receipt:' "$TMP/second.out" || fail "admitted queued landing must report its receipt"
  [ "$(wc -l < "$OMS_TEST_LAND_COUNT" | tr -d ' ')" = 1 ] || fail "duplicate landing repeated the gate"
  if [ "$lock_kind" = 0 ]; then
    OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" --json | grep -q '"active": null' ||
      fail "a released flock file without a matching row must remain unknown"
    unknown_rc=0
    OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" --active-exit >/dev/null || unknown_rc=$?
    [ "$unknown_rc" = 4 ] || fail "--active-exit must distinguish unknown lock state: $unknown_rc"
  else
    OMS_LOCK_FORCE_MKDIR=1 "$LAND" status --repo "$repo" --json | grep -q '"active": false' ||
      fail "a released mkdir lock must read inactive"
    OMS_LOCK_FORCE_MKDIR=1 "$LAND" status --repo "$repo" --active-exit >/dev/null ||
      fail "--active-exit must exit 0 when no mkdir lock is active"
  fi
  OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" status --repo "$repo" | grep -q '^land .*: passed' ||
    fail "the queued retry must leave its passed receipt visible"
  # A receipt that says running without a lock holder is stale.
  stale_receipt="$(ls -t "$receipt_dir"/*.json | head -n 1)"
  cp "$stale_receipt" "$TMP/stale-backup.json"
  sed 's/"state": "[a-z]*"/"state": "running"/' "$TMP/stale-backup.json" > "$stale_receipt"
  if [ "$lock_kind" = 0 ]; then
    OMS_LOCK_FORCE_MKDIR=0 "$LAND" status --repo "$repo" | grep -q 'lock activity is unknown' ||
      fail "a running receipt with unknown lock state must remain unknown"
  else
    OMS_LOCK_FORCE_MKDIR=1 "$LAND" status --repo "$repo" | grep -q 'no land holds the lock (stale receipt)' ||
      fail "a running receipt without a mkdir lock must read stale"
  fi
  cp "$TMP/stale-backup.json" "$stale_receipt"
done

# A queued request revalidates its admitted SHA and tracked tree before it may
# run the gate. These changes are made after the detached caller reports that
# it is waiting, while an independent first landing still owns the lock.
wait_for_queue_notice() {  # OUTPUT_FILE SECOND_PID RELEASE_FILE
  local output="$1" second_pid="$2" release_file="$3"
  for _ in $(seq 1 100); do
    grep -q 'waiting for active landing lock' "$output" && break
    sleep 0.05
  done
  if ! grep -q 'waiting for active landing lock' "$output"; then
    touch "$release_file"
    wait "$first_pid" || true
    wait "$second_pid" || true
    fail "queued land did not report waiting: $(cat "$output")"
  fi
  if grep -q '^receipt:' "$output"; then
    touch "$release_file"
    wait "$first_pid" || true
    wait "$second_pid" || true
    fail "waiting land advertised a receipt: $(cat "$output")"
  fi
}

export OMS_TEST_LAND_COUNT="$TMP/queued-head-count" OMS_TEST_LAND_RELEASE="$TMP/queued-head-release"
gate 'echo head-gate; echo gate >> "$OMS_TEST_LAND_COUNT"; for ((i=0;i<200;i++)); do [ ! -f "$OMS_TEST_LAND_RELEASE" ] || exit 0; sleep 0.05; done; exit 1' # queued HEAD change
before="$(remote_tip)"
OMS_LOCK_FORCE_MKDIR=0 "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 \
  > "$TMP/queued-head-first.out" 2>&1 &
first_pid=$!
for _ in $(seq 1 100); do [ ! -f "$OMS_TEST_LAND_COUNT" ] || break; sleep 0.05; done
[ -f "$OMS_TEST_LAND_COUNT" ] || fail "queued-HEAD fixture did not start its gate"
OMS_LOCK_FORCE_MKDIR=0 OMS_LOCK_TIMEOUT=10 "$LAND" --repo "$repo" --no-update --ci-wait 0 \
  > "$TMP/queued-head-second.out" 2>&1 &
second_pid=$!
wait_for_queue_notice "$TMP/queued-head-second.out" "$second_pid" "$OMS_TEST_LAND_RELEASE"
echo queued-head-change > "$repo/queued-head-change"
git -C "$repo" add queued-head-change
git -C "$repo" commit -q -m 'test: move HEAD while land waits'
touch "$OMS_TEST_LAND_RELEASE"
first_rc=0 second_rc=0
wait "$first_pid" || first_rc=$?
wait "$second_pid" || second_rc=$?
[ "$first_rc" -ne 0 ] || fail "the active land must reject a HEAD change during its gate"
[ "$second_rc" -ne 0 ] || fail "the queued land must reject its stale request SHA"
[ "$(remote_tip)" = "$before" ] || fail "queued HEAD change must not push"
[ "$(wc -l < "$OMS_TEST_LAND_COUNT" | tr -d ' ')" = 1 ] || fail "queued stale-HEAD land ran the gate"
! grep -q '^receipt:' "$TMP/queued-head-second.out" || fail "stale-HEAD land advertised a receipt"
grep -q 'land not started (exit 1)' "$TMP/queued-head-second.out" ||
  fail "stale-HEAD startup must preserve its failure status: $(cat "$TMP/queued-head-second.out")"

export OMS_TEST_LAND_COUNT="$TMP/queued-tree-count" OMS_TEST_LAND_RELEASE="$TMP/queued-tree-release"
gate 'echo tree-gate; echo gate >> "$OMS_TEST_LAND_COUNT"; for ((i=0;i<200;i++)); do [ ! -f "$OMS_TEST_LAND_RELEASE" ] || exit 0; sleep 0.05; done; exit 1' # queued tree change
before="$(remote_tip)"
OMS_LOCK_FORCE_MKDIR=0 "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 \
  > "$TMP/queued-tree-first.out" 2>&1 &
first_pid=$!
for _ in $(seq 1 100); do [ ! -f "$OMS_TEST_LAND_COUNT" ] || break; sleep 0.05; done
[ -f "$OMS_TEST_LAND_COUNT" ] || fail "queued-tree fixture did not start its gate"
OMS_LOCK_FORCE_MKDIR=0 OMS_LOCK_TIMEOUT=10 "$LAND" --repo "$repo" --no-update --ci-wait 0 \
  > "$TMP/queued-tree-second.out" 2>&1 &
second_pid=$!
wait_for_queue_notice "$TMP/queued-tree-second.out" "$second_pid" "$OMS_TEST_LAND_RELEASE"
printf '# queued tracked edit\n' >> "$repo/scripts/check.sh"
touch "$OMS_TEST_LAND_RELEASE"
first_rc=0 second_rc=0
wait "$first_pid" || first_rc=$?
wait "$second_pid" || second_rc=$?
[ "$first_rc" -ne 0 ] || fail "the active land must reject a tracked-tree change during its gate"
[ "$second_rc" -ne 0 ] || fail "the queued land must reject its dirty tracked tree"
[ "$(remote_tip)" = "$before" ] || fail "queued tracked-tree change must not push"
[ "$(wc -l < "$OMS_TEST_LAND_COUNT" | tr -d ' ')" = 1 ] || fail "queued dirty-tree land ran the gate"
! grep -q '^receipt:' "$TMP/queued-tree-second.out" || fail "dirty-tree land advertised a receipt"
grep -q 'land not started (exit 1)' "$TMP/queued-tree-second.out" ||
  fail "dirty-tree startup must preserve its failure status: $(cat "$TMP/queued-tree-second.out")"
git -C "$repo" checkout -- scripts/check.sh

gate 'echo successful CI and update probe; echo "check: ok"'
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/update-pass.out" 2>&1 ||
  fail "successful CI and update failed to land"
[ "$(cat "$TMP/events")" = "$(printf 'ci\nupdate')" ] || fail "successful update did not follow CI"
grep -q ': passed' "$TMP/update-pass.out" || fail "successful update receipt was not passed"
grep -rqs '"command": *"bash scripts/check.sh --affected --ci-parity --changed-from origin/main --changed-to HEAD"' "$receipt_dir" ||
  fail "the harness checkout's default gate must select against the destination"
# The default gate caps a full selection to lint and the quick lane: that
# verdict names mode=full but is not install proof; an affected-only CI run is not either.
gate 'if [ "$#" = 0 ]; then echo "check: ok"; else echo "check: ok (affected tests; mode=full capped to lint and quick lane; ci-parity)"; fi'
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/affected-pass.out" 2>&1 ||
  fail "an affected gate failed to land: $(cat "$TMP/affected-pass.out")"
[ "$(cat "$TMP/events")" = "$(printf 'ci\njobs')" ] || fail "an affected gate and affected CI must push without installing"
grep -q 'install needs a full gate or a full-selection CI run' "$TMP/affected-pass.out" ||
  fail "the skipped install must name the missing proof"
land_field() { "$LAND" status --repo "$repo" --json | python3 -c 'import json,sys
r = json.load(sys.stdin)
for key in sys.argv[1].split("."): r = r.get(key, {}) if isinstance(r, dict) else {}
print(r if r != {} else "")' "$1"; }
[ "$(land_field gate.scope)" = partial ] && [ "$(land_field ci.scope)" = affected ] ||
  fail "capped gate or affected CI was recorded as full proof"
# Missing jobs and unsettled conclusions remain distinct in ambiguity details.
query_rc=0
query_sha="$(git -C "$repo" rev-parse HEAD)"
OMS_TEST_CI_SCOPE=ambiguous-null OMS_TEST_CI_RESULT=success \
  python3 "$ROOT/scripts/lib/ci-query.py" scope fixture/repo 1 "$query_sha" 5 \
  > "$TMP/scope-diagnostics.out" 2>&1 || query_rc=$?
[ "$query_rc" = 2 ] || fail "ambiguous diagnostics unexpectedly classified CI: $query_rc"
grep -Fq "('gate', None)" "$TMP/scope-diagnostics.out" || fail "null conclusion was not diagnosed distinctly"
grep -Fq "('focused (2/4)', 'missing')" "$TMP/scope-diagnostics.out" ||
  fail "missing job was not diagnosed distinctly"
: > "$TMP/events"
query_rc=0
query_out="$(OMS_TEST_CI_SCOPE=settles-full OMS_TEST_CI_RESULT=success \
  python3 "$ROOT/scripts/lib/ci-query.py" scope fixture/repo 1 "$query_sha" 30 \
  2> "$TMP/scope-settles-full.err")" || query_rc=$?
[ "$query_rc" = 0 ] && [ "$query_out" = full ] ||
  fail "settled full scope query failed: rc=$query_rc output=$query_out stderr=$(cat "$TMP/scope-settles-full.err")"
[ "$(grep -c '^jobs$' "$TMP/events")" = 4 ] ||
  fail "settled full scope query did not poll four times: $(cat "$TMP/events")"
# Unreadable, ambiguous or misattributed CI scope blocks the install; the retry
# resumes and a full-selection run for the exact SHA installs it without
# re-running the gate.
for scope in error ambiguous wrong-sha failed-run duplicate extra-failure; do
  : > "$TMP/events"
  if OMS_TEST_CI_SCOPE="$scope" OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
      "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/scope-$scope.out" 2>&1; then
    fail "a $scope CI scope passed: $(cat "$TMP/scope-$scope.out")"
  fi
  [ "$(cat "$TMP/events")" = jobs ] || fail "a $scope CI scope installed or re-ran CI: $(cat "$TMP/events")"
  [ "$(land_field ci.scope)" = unknown ] || fail "a $scope CI scope was not recorded as unknown"
  if [ "$scope" = ambiguous ]; then
    scope_log="$(land_field log)"
    grep -q 'focused (2/4)' "$scope_log" || fail "ambiguous stderr omitted the offending job from $scope_log"
  fi
done
: > "$TMP/events"
OMS_TEST_CI_SCOPE=lagging OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/scope-lagging.out" 2>&1 ||
  fail "a lagging job conclusion did not settle: $(cat "$TMP/scope-lagging.out")"
[ "$(land_field ci.scope)" = affected ] || fail "settled lagging scope was not recorded as affected"
[ "$(cat "$TMP/events")" = "$(printf 'jobs\njobs')" ] || fail "lagging affected scope installed or re-ran CI"
: > "$TMP/events"
OMS_TEST_CI_SCOPE=full OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/scope-full.out" 2>&1 ||
  fail "a full-selection CI run did not install: $(cat "$TMP/scope-full.out")"
[ "$(cat "$TMP/events")" = "$(printf 'jobs\nupdate')" ] || fail "full CI scope did not install after its query"
[ "$(land_field update.authority)" = ci.scope=full ] && [ "$(land_field update.reason)" = installed ] ||
  fail "install did not record its CI proof or kept the blocked reason"
[ "$(grep -c '^check: ok (affected tests; mode=full capped' "$(land_field log)")" = 1 ] || fail "resume re-ran the gate"
# The advertised recovery verifies the already-pushed SHA in full and installs it.
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --gate 'bash scripts/check.sh' --wait --ci-wait 1 > "$TMP/full-recovery.out" 2>&1 ||
  fail "full-gate recovery of a pushed SHA failed: $(cat "$TMP/full-recovery.out")"
[ "$(cat "$TMP/events")" = "$(printf 'ci\nupdate')" ] || fail "full-gate recovery did not install after CI"
[ "$(land_field update.authority)" = gate.scope=full ] || fail "install did not record its full-gate proof"
gate 'echo "check: ok (quick local checks; full CI gate required)"'
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --gate 'bash scripts/check.sh --quick' --wait --ci-wait 1 > "$TMP/quick-pass.out" 2>&1 ||
  fail "a quick gate failed to land: $(cat "$TMP/quick-pass.out")"
[ "$(cat "$TMP/events")" = "$(printf 'ci\njobs')" ] || fail "a gate without full proof must not install"
gate 'echo "check: ok (parallel gate without lint)"'
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --gate 'bash scripts/check.sh --parallel --no-lint' --wait --ci-wait 1 > "$TMP/no-lint.out" 2>&1 ||
  fail "a no-lint gate failed to land: $(cat "$TMP/no-lint.out")"
[ "$(cat "$TMP/events")" = "$(printf 'ci\njobs')" ] || fail "a gate that skipped lint must not install"

# Probe the real job function with deterministic wait/push interleavings.
# Git is stubbed: this cannot publish or mutate the caller's repository.
(
  export REPO="$repo" LAND_DIR="$TMP/race" LOG_DIR="$TMP/race" STAMP=verified-probe
  export READY_FILE="$TMP/race-ready"
  export REMOTE=origin TARGET=main GATE=true IGNORE_SIBLINGS=0 UPDATE=0 CI_WAIT=0
  # run_job is extracted from land.sh below, so load its real test-growth
  # helper and initialize the same optional readiness setting as land.sh.
  . "$ROOT/scripts/lib/test-growth.sh"
  probe_head=verified probe_base_valid=1
  now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
  rset() { :; }
  receipt_value() { :; }
  oms_test_growth() { :; }
  can_resume() { return 1; }
  request_stamp() { printf '%s\n' "$STAMP"; }
  collect_live_siblings() { export SIBLING_LIVE=''; }
  clean_tree() { return 0; }
  run_gate() { return 0; }
  finish() { printf '%s\n' "$1" > "$TMP/race-result"; }
  wait_for_live_siblings() { probe_head=unverified; }
  git() {
    if [ "${1:-}" = -C ] && [ "${3:-}" = diff ]; then return 0; fi
    case "$1" in
      rev-parse) printf '%s\n' "$probe_head" ;;
      remote) printf '%s\n' origin ;;
      ls-remote) printf '%s\n' base ;;
      merge-base) [ "$probe_base_valid" -eq 1 ] ;;
      push) printf '%s\n' "$*" > "$TMP/race-push" ;;
      *) return 99 ;;
    esac
  }
  eval "$(sed -n '/^run_job() {/,/^}/p' "$LAND")"
  if run_job; then fail "HEAD moving during sibling wait was accepted"; fi
  [ ! -e "$TMP/race-push" ] || fail "unverified HEAD was pushed"
  probe_head=verified
  wait_for_live_siblings() { :; }
  run_job || fail "stable HEAD failed to land"
  [ "$(cat "$READY_FILE")" = 0 ] || fail "admitted job did not report startup readiness"
  grep -Fq 'verified:refs/heads/main' "$TMP/race-push" || fail "push uses mutable HEAD instead of verified SHA"
  grep -Fq -- '--force-with-lease=refs/heads/main:base' "$TMP/race-push" ||
    fail "push does not bind the observed destination base"
  rm "$TMP/race-push"
  probe_base_valid=0
  if run_job; then fail "non-ancestor destination base was accepted"; fi
  [ ! -e "$TMP/race-push" ] || fail "non-ancestor destination was pushed over"
) || exit 1

# A one-second CI wait still owes one query when the clock ticks between
# setting and checking its deadline (CI run 36540685415 flaked on this).
real_date="$(command -v date)"
mkdir -p "$TMP/tick-bin"
cat > "$TMP/tick-bin/date" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = +%s ]; then
  n="\$(cat "$TMP/tick" 2>/dev/null || "$real_date" +%s)"
  n=\$((n + 1)); printf '%s\n' "\$n" > "$TMP/tick"; printf '%s\n' "\$n"; exit 0
fi
exec "$real_date" "\$@"
EOF
chmod +x "$TMP/tick-bin/date"
gate 'echo tick probe; echo "check: ok"'
if PATH="$TMP/tick-bin:$PATH" OMS_TEST_CI_RESULT=success OMS_INSTALL_RECEIPT="$TMP/install.json" \
    "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/tick.out" 2>&1; then
  fail "the tick probe's failing update was reported as success"
fi
grep -q 'install update exit 42' "$TMP/tick.out" ||
  fail "a one-second CI wait skipped its query: $(cat "$TMP/tick.out")"

# Refresh is a separate, opt-in step after successful land and handoff. Keep
# its mutable result beside immutable landing evidence, never in its digest.
for sync_mode in passed deferred refused; do
  sync_target="$TMP/sync-$sync_mode"
  git clone -q "$TMP/remote.git" "$sync_target" 2>/dev/null
  sync_head="$(git -C "$sync_target" rev-parse HEAD)"
  gate "echo sync $sync_mode"
  sync_sha="$(git -C "$repo" rev-parse HEAD)"
  if [ "$sync_mode" = refused ]; then
    printf 'local dirty work\n' > "$sync_target/local.txt"
  fi
  (
    . "$ROOT/scripts/lib/canonical-sync.sh"
    if [ "$sync_mode" = deferred ]; then
      oms_canonical_call_acquire "$sync_target" held-land-sync
    fi
    OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
      --handoff-receipt-to "$sync_target" --sync-shared "$sync_target" > "$TMP/sync-$sync_mode.out" 2>&1 ||
      fail "sync $sync_mode changed land success: $(cat "$TMP/sync-$sync_mode.out")"
    oms_canonical_call_release
  ) || exit 1
  "$LAND" status --repo "$repo" --json > "$TMP/sync-status.json"
  sync_receipt="$(find "$receipt_dir" -maxdepth 1 -name "$sync_sha-*.json" -print)"
  sync_dir="$("$LAND" status --repo "$sync_target" --json | python3 -c 'import json,sys,pathlib; print(pathlib.Path(json.load(sys.stdin)["log"]).parent)')"
  cmp "$sync_receipt" "$sync_dir/$(basename "$sync_receipt")" || fail "sync changed handed-off receipt bytes"
  python3 - "$TMP/sync-status.json" "$receipt_dir/sync/$(basename "$sync_receipt")" "$sync_mode" <<'PY' || fail "wrong sync receipt/status"
import json, sys
status, companion = [json.load(open(path)) for path in sys.argv[1:3]]
assert status['state'] == 'passed', status
assert status['sync'] == companion['sync'], (status, companion)
assert status['sync']['state'] == sys.argv[3], status
assert status['sync']['rc'] == {'passed': 0, 'deferred': 75, 'refused': 2}[sys.argv[3]], status
assert status['sync']['reason'], status
PY
  grep -q "sync $sync_mode:" "$TMP/sync-$sync_mode.out" || fail "sync result missing from status line"
  grep -q "sync $sync_mode:" "$(log_of "$sync_receipt")" || fail "sync result missing from land log"
  if [ "$sync_mode" = passed ]; then
    [ "$(git -C "$sync_target" rev-parse HEAD)" = "$sync_sha" ] || fail "sync did not fast-forward target"
  else
    [ "$(git -C "$sync_target" rev-parse HEAD)" = "$sync_head" ] || fail "sync $sync_mode moved target HEAD"
    if [ "$sync_mode" = deferred ]; then
      grep -q 'sync deferred: 1 calls running (held-land-sync)' "$TMP/sync-deferred.out" || fail "defer reason missing"
      # Handoff retries keep the same landing bytes after a mutable sync result.
      OMS_TEST_CI_RC=4 "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
        --handoff-receipt-to "$sync_target" --sync-shared "$sync_target" > "$TMP/sync-retry.out" 2>&1 ||
        fail "deferred sync retry failed: $(cat "$TMP/sync-retry.out")"
      [ "$(git -C "$sync_target" rev-parse HEAD)" = "$sync_sha" ] || fail "retry did not refresh idle target"
      cmp "$sync_receipt" "$sync_dir/$(basename "$sync_receipt")" || fail "sync retry changed handoff bytes"
    else
      grep -q 'sync refused: dirty shared checkout' "$TMP/sync-refused.out" || fail "dirty refusal reason missing"
      [ "$(cat "$sync_target/local.txt")" = 'local dirty work' ] || fail "refused sync changed dirty work"
    fi
  fi
done
# A linked sync namespace could alias the landing receipt: sync is refused, the land and its bytes stay intact.
link_target="$TMP/sync-linked"
git clone -q "$TMP/remote.git" "$link_target" 2>/dev/null
gate "echo sync linked"
link_sha="$(git -C "$repo" rev-parse HEAD)"
rm -rf "$receipt_dir/sync"
ln -s "$receipt_dir" "$receipt_dir/sync"
OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
  --sync-shared "$link_target" > "$TMP/sync-linked.out" 2>&1 || fail "linked sync namespace failed the land: $(cat "$TMP/sync-linked.out")"
grep -q 'sync refused: sync receipt namespace is not a plain directory' "$TMP/sync-linked.out" || fail "linked sync namespace not refused"
link_receipt="$(find "$receipt_dir" -maxdepth 1 -name "$link_sha-*.json" -print)"
python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["state"] == "passed" and "sync" not in r, r' "$link_receipt" ||
  fail "linked sync namespace altered the landing receipt"
rm -f "$receipt_dir/sync"

echo "land-smoke: ok"
