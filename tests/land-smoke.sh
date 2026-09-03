#!/usr/bin/env bash
set -euo pipefail

# `oms land` end to end against a bare remote: a green gate pushes and writes a
# passed receipt, a red gate records the failure and pushes nothing, a HEAD
# that moves during the gate is never pushed, dirty or diverged trees are
# refused before any job starts, and the detached mode leaves a receipt the
# status verb reads. No CI (--ci-wait 0) and no install refresh (the fixture
# is not the harness checkout), so nothing here reaches the network.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-land.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
export OMS_WORK_JOURNAL_SUPPRESS=1 XDG_STATE_HOME="$TMP/state" GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
log_of() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["log"])' "$1"; }
LAND="$ROOT/scripts/land.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }

git init -q --bare -b main "$TMP/remote.git"
repo="$TMP/work"
git clone -q "$TMP/remote.git" "$repo" 2>/dev/null
mkdir -p "$repo/scripts" "$repo/.oms"
printf '*\n' > "$repo/.oms/.gitignore"
gate() {  # gate BODY -> commits scripts/check.sh with that body
  printf '#!/usr/bin/env bash\n%s\n' "$1" > "$repo/scripts/check.sh"
  git -C "$repo" add scripts/check.sh
  git -C "$repo" commit -q -m "gate: $1"
}
gate 'echo gate ok'
git -C "$repo" push -q origin HEAD:main
remote_tip() { git -C "$TMP/remote.git" rev-parse main; }

# --- 1. nothing to land ------------------------------------------------------
out="$("$LAND" --repo "$repo" --wait --ci-wait 0)"
printf '%s' "$out" | grep -q 'nothing to land' || fail "an up-to-date tree lands nothing: $out"

# --- 2. green gate: pushed, receipt passed, update skipped --------------------
echo one > "$repo/one"; git -C "$repo" add one; git -C "$repo" commit -q -m one
head="$(git -C "$repo" rev-parse HEAD)"
"$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/pass.out" || fail "green gate must land: $(cat "$TMP/pass.out")"
[ "$(remote_tip)" = "$head" ] || fail "remote main must be the landed commit"
receipt="$(ls "$repo"/.oms/land/*.json | tail -n 1)"
python3 - "$receipt" "$head" <<'PY' || fail "passed receipt is wrong: $(cat "$receipt")"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["state"] == "passed" and r["sha"] == sys.argv[2], r
assert r["gate"]["rc"] == 0 and r["push"]["rc"] == 0, r
assert r["update"]["rc"] == "skipped" and r["ci"]["conclusion"] == "skipped", r
PY
grep -q 'gate ok' "$(log_of "$receipt")" || fail "gate output must land in the receipt log"
case "$(log_of "$receipt")" in "$TMP/state/"*) ;; *) fail "the gate log must live outside the repo: $(log_of "$receipt")" ;; esac
"$LAND" status --repo "$repo" | grep -q '^land .*: passed' || fail "status must read the newest receipt"

# --- 3. red gate: failure recorded, nothing pushed -----------------------------
gate 'echo boom; exit 3'
before="$(remote_tip)"
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/fail.out" 2>&1; then
  fail "a red gate must exit nonzero: $(cat "$TMP/fail.out")"
fi
[ "$(remote_tip)" = "$before" ] || fail "a red gate must not push"
grep -q 'gate exit 3' "$TMP/fail.out" || fail "the failure reason must be shown: $(cat "$TMP/fail.out")"
"$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved | grep -q 'land: gate failed' ||
  fail "a red gate must be recorded in the fail ledger"

# --- 4. HEAD moves during the gate: nothing pushed -----------------------------
gate 'sleep 3; echo slow ok'
( sleep 1; echo two > "$repo/two"; git -C "$repo" add two; git -C "$repo" commit -q -m two ) &
if "$LAND" --repo "$repo" --wait --ci-wait 0 > "$TMP/moved.out" 2>&1; then
  fail "a moved HEAD must fail the landing: $(cat "$TMP/moved.out")"
fi
wait
[ "$(remote_tip)" = "$before" ] || fail "a moved HEAD must not push"
grep -q 'HEAD or the tree changed' "$TMP/moved.out" || fail "moved-HEAD reason missing: $(cat "$TMP/moved.out")"

# --- 5. refusals before any job: dirty tree, diverged remote -----------------
gate 'echo gate ok'
echo dirty >> "$repo/one"
"$LAND" --repo "$repo" --wait --ci-wait 0 2>"$TMP/dirty.err" && fail "a dirty tree must be refused"
grep -q 'commit or stash' "$TMP/dirty.err" || fail "dirty refusal must say so: $(cat "$TMP/dirty.err")"
git -C "$repo" checkout -q -- one
git clone -q "$TMP/remote.git" "$TMP/other" 2>/dev/null
echo other > "$TMP/other/other"; git -C "$TMP/other" add other
git -C "$TMP/other" commit -q -m other; git -C "$TMP/other" push -q origin HEAD:main
"$LAND" --repo "$repo" --wait --ci-wait 0 2>"$TMP/diverged.err" && fail "a diverged remote must be refused"
grep -q 'rebase first' "$TMP/diverged.err" || fail "diverged refusal must say so: $(cat "$TMP/diverged.err")"
if ls "$repo"/.oms/land/*-"$(git -C "$repo" rev-parse --short HEAD)"-*.json >/dev/null 2>&1; then
  fail "a refused landing must not leave a receipt"
fi

# --- 6. detached mode: receipt appears, status reads it -----------------------
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

# --- 7. the detached job must not inherit ignored SIGINT/SIGQUIT --------------
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
