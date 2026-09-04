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
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-land.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
export OMS_WORK_JOURNAL_SUPPRESS=1 XDG_STATE_HOME="$TMP/state" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
log_of() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["log"])' "$1"; }
LAND="$ROOT/scripts/land.sh"
AUTOPILOT_RECEIPT="$ROOT/scripts/lib/autopilot-receipt.py"
state_land="$XDG_STATE_HOME/oh-my-setting/land"

fail() { echo "FAIL: $*" >&2; exit 1; }

git init -q --bare -b main "$TMP/remote.git"
repo="$TMP/work.. tree"
git clone -q "$TMP/remote.git" "$repo" 2>/dev/null
mkdir -p "$repo/scripts" "$repo/.oms"
printf '*\n' > "$repo/.oms/.gitignore"
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
gate 'echo gate ok'
git -C "$repo" push -q origin HEAD:main
remote_tip() { git -C "$TMP/remote.git" rev-parse main; }

# --- 1. nothing to land ------------------------------------------------------
out="$("$LAND" --repo "$repo" --wait --ci-wait 0)"
printf '%s' "$out" | grep -q 'nothing to land' || fail "an up-to-date tree lands nothing: $out"
[ ! -e "$state_land" ] || fail "an up-to-date landing must not leave receipt state"

# --- 2. green worktree gate: state survives worktree removal ------------------
peer="$TMP/landing-worktree"
git -C "$repo" worktree add -q -b land-receipt "$peer" HEAD
mkdir -p "$peer/.oms"
printf '*\n' > "$peer/.oms/.gitignore"
peer_oms_before="$(find "$peer/.oms" -mindepth 1 -print | sort)"
echo one > "$peer/one"; git -C "$peer" add one; git -C "$peer" commit -q -m one
head="$(git -C "$peer" rev-parse HEAD)"
"$LAND" --repo "$peer" --wait --ci-wait 0 > "$TMP/pass.out" || fail "green gate must land: $(cat "$TMP/pass.out")"
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
assert "siblings" not in r, r
PY
grep -q 'gate ok' "$(log_of "$receipt")" || fail "gate output must land in the receipt log"
case "$(log_of "$receipt")" in "$state_land"/*) ;; *) fail "the gate log must live outside the repo: $(log_of "$receipt")" ;; esac
[ "$(dirname "$(log_of "$receipt")")" = "$receipt_dir" ] || fail "receipt and gate log must share a state directory"
[ "$peer_oms_before" = "$(find "$peer/.oms" -mindepth 1 -print | sort)" ] ||
  fail "a landing must not add receipt state under its worktree .oms"
"$LAND" status --repo "$peer" | grep -q '^land .*: passed' || fail "status must read a worktree receipt"
git -C "$repo" worktree remove --force "$peer"
[ ! -e "$peer" ] || fail "landing worktree removal must succeed"
"$LAND" status --repo "$repo" | grep -q '^land .*: passed' ||
  fail "main checkout must read the removed worktree's receipt"
git -C "$repo" pull -q --ff-only origin main

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
python3 - "$receipt_dir" "$sibling_physical" <<'PY' || fail "blocked sibling receipt is wrong"
import glob, json, os, sys
directory, sibling = sys.argv[1:]
for path in glob.glob(os.path.join(directory, "*.json")):
    row = json.load(open(path))
    if row.get("state") != "blocked":
        continue
    assert row.get("siblings", {}).get("live") == sibling + " driving", row
    assert row.get("push", {}).get("sibling_wait_seconds") == 0, row
    assert "rc" not in row.get("push", {}), row
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
git -C "$repo" worktree remove --force "$sibling"

# --- 4. red gate: failure recorded, nothing pushed -----------------------------
gate 'echo boom; exit 3'
before="$(remote_tip)"
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/fail.out" 2>&1; then
  fail "a red gate must exit nonzero: $(cat "$TMP/fail.out")"
fi
[ "$(remote_tip)" = "$before" ] || fail "a red gate must not push"
grep -q 'gate exit 3' "$TMP/fail.out" || fail "the failure reason must be shown: $(cat "$TMP/fail.out")"
"$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved | grep -q 'land: gate failed' ||
  fail "a red gate must be recorded in the fail ledger"

# --- 5. HEAD moves during the gate: nothing pushed -----------------------------
gate 'sleep 3; echo slow ok'
( sleep 1; echo two > "$repo/two"; git -C "$repo" add two; git -C "$repo" commit -q -m two ) &
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
echo other > "$other/other"; git -C "$other" add other
git -C "$other" commit -q -m other; git -C "$other" push -q origin HEAD:main
"$LAND" --repo "$repo" --wait --ci-wait 0 2>"$TMP/diverged.err" && fail "a diverged remote must be refused"
grep -q 'rebase first' "$TMP/diverged.err" || fail "diverged refusal must say so: $(cat "$TMP/diverged.err")"
[ "$receipts_before_refusal" = "$(find "$receipt_dir" -maxdepth 1 -type f -name '*.json' -print | sort)" ] ||
  fail "a refused landing must not leave a receipt"

# --- 7. detached mode: receipt appears, status reads it -----------------------
git -C "$repo" pull -q --rebase origin main
out="$("$LAND" --repo "$repo" --ci-wait 0)"
printf '%s' "$out" | grep -q 'receipt: ' || fail "detached mode must print the receipt path: $out"
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

echo "land-smoke: ok"
