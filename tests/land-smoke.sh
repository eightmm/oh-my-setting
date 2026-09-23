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
assert r["gate"]["command"] == "bash scripts/check.sh", r
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

# --- 2b. the gate checks the committed tree, not the live checkout -----------
# A session driving the checkout writes its own .oms during the gate, and an
# untracked file must not sway a verdict the push attributes to the commit.
echo scratch > "$repo/untracked-scratch"
gate 'test ! -e untracked-scratch || exit 9; mkdir -p .oms; touch .oms/gate-wrote; echo isolated gate ok'
repo_oms_before="$(find "$repo/.oms" -print | sort)"
"$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/isolated.out" 2>&1 ||
  fail "the gate must run in a tree without untracked files: $(cat "$TMP/isolated.out")"
[ "$repo_oms_before" = "$(find "$repo/.oms" -print | sort)" ] ||
  fail "a gate's .oms writes must not reach the landing checkout"
git -C "$repo" worktree list --porcelain | grep -q 'oms-land-gate\.' &&
  fail "the gate worktree must be removed after the gate"
rm -f "$repo/untracked-scratch"

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
echo ci >> "$OMS_TEST_LAND_EVENTS"
[ "${OMS_TEST_CI_DELAY:-0}" = 0 ] || sleep "$OMS_TEST_CI_DELAY"
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
  gate "echo CI $ci_result probe"
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
gate 'echo update failure probe'
if OMS_TEST_CI_RESULT=success OMS_INSTALL_RECEIPT="$TMP/install.json" "$LAND" --repo "$repo" --wait --ci-wait 1 \
  > "$TMP/update-failed.out" 2>&1; then
  fail "installation failure was reported as a successful landing"
fi
[ "$(remote_tip)" = "$(git -C "$repo" rev-parse HEAD)" ] || fail "update failure lost the successful push"
grep -q 'install update exit 42' "$TMP/update-failed.out" || fail "update failure reason missing"
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

# A different gate or destination cannot borrow a prior successful receipt.
OMS_TEST_CI_RC=4 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --gate true --ci-wait 1 > "$TMP/other-gate.out" 2>&1 ||
  fail "already-pushed commit with another gate should be a no-op"
grep -q 'no matching push receipt' "$TMP/other-gate.out" || fail "different gate reused prior evidence"
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
  : > "$TMP/events"
  OMS_TEST_CI_RESULT=success "$LAND" --repo "$repo" --wait --no-update --ci-wait 1 \
    > "$TMP/query-resume.out" 2>&1 || fail "CI error could not resume"
  [ "$(cat "$TMP/events")" = ci ] || fail "CI resume omitted its query"
  receipt="$("$LAND" status --repo "$repo" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["log"])')"
  [ "$(grep -c "^$ci_failure resume probe$" "$receipt")" = 1 ] || fail "CI resume repeated the gate"
done

# One active landing across worktrees, with flock and the portable mkdir lock.
for lock_kind in 0 1; do
  export OMS_TEST_LAND_COUNT="$TMP/count-$lock_kind" OMS_TEST_LAND_RELEASE="$TMP/release-$lock_kind"
  # flock leaves a file; mkdir uses a directory at that path. Model separate hosts.
  export OMS_LOCK_DIR="$TMP/locks-$lock_kind"
  gate 'echo gate >> "$OMS_TEST_LAND_COUNT"; for ((i=0;i<200;i++)); do [ ! -f "$OMS_TEST_LAND_RELEASE" ] || exit 0; sleep 0.05; done; exit 1'" # lock $lock_kind"
  OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" --repo "$repo" --wait --no-update --ci-wait 0 > "$TMP/first.out" 2>&1 &
  first_pid=$!
  for _ in $(seq 1 100); do [ ! -f "$OMS_TEST_LAND_COUNT" ] || break; sleep 0.05; done
  [ -f "$OMS_TEST_LAND_COUNT" ] || fail "first landing did not start: $(cat "$TMP/first.out")"
  second_rc=0
  OMS_LOCK_FORCE_MKDIR="$lock_kind" "$LAND" --repo "$repo" --no-update --ci-wait 0 \
    > "$TMP/second.out" 2>&1 || second_rc=$?
  touch "$OMS_TEST_LAND_RELEASE"
  wait "$first_pid" || fail "first landing failed: $(cat "$TMP/first.out")"
  [ "$second_rc" = 75 ] || fail "duplicate landing did not report lock contention: $(cat "$TMP/second.out")"
  ! grep -q '^receipt:' "$TMP/second.out" || fail "rejected background launch advertised a receipt"
  [ "$(wc -l < "$OMS_TEST_LAND_COUNT" | tr -d ' ')" = 1 ] || fail "duplicate landing repeated the gate"
done

gate 'echo successful CI and update probe'
: > "$TMP/events"
OMS_TEST_CI_RESULT=success OMS_TEST_UPDATE_RC=0 OMS_INSTALL_RECEIPT="$TMP/install.json" \
  "$LAND" --repo "$repo" --wait --ci-wait 1 > "$TMP/update-pass.out" 2>&1 ||
  fail "successful CI and update failed to land"
[ "$(cat "$TMP/events")" = "$(printf 'ci\nupdate')" ] || fail "successful update did not follow CI"
grep -q ': passed' "$TMP/update-pass.out" || fail "successful update receipt was not passed"
grep -rqs '"command": *"bash scripts/check.sh --parallel"' "$receipt_dir" ||
  fail "the harness checkout's default gate must run --parallel"

# Probe the real job function with deterministic wait/push interleavings.
# Git is stubbed: this cannot publish or mutate the caller's repository.
(
  export REPO="$repo" LAND_DIR="$TMP/race" LOG_DIR="$TMP/race" STAMP=verified-probe
  export REMOTE=origin TARGET=main GATE=true IGNORE_SIBLINGS=0 UPDATE=0 CI_WAIT=0
  probe_head=verified
  now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
  rset() { :; }
  can_resume() { return 1; }
  request_stamp() { printf '%s\n' "$STAMP"; }
  collect_live_siblings() { export SIBLING_LIVE=''; }
  clean_tree() { return 0; }
  run_gate() { return 0; }
  finish() { printf '%s\n' "$1" > "$TMP/race-result"; }
  wait_for_live_siblings() { probe_head=unverified; }
  git() {
    case "$1" in
      rev-parse) printf '%s\n' "$probe_head" ;;
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
  grep -Fq 'verified:refs/heads/main' "$TMP/race-push" || fail "push uses mutable HEAD instead of verified SHA"
) || exit 1

echo "land-smoke: ok"
