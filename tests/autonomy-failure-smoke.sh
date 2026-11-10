#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-failure-tests.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name Test
printf 'one\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base

LEDGER="$repo/.oms/failures.jsonl"
tool="$ROOT/scripts/fail-ledger.sh"
caller="$TMP/caller"
mkdir -p "$caller"
(cd "$caller" && "$tool" --repo "$repo" record \
  --cmd 'repo-scoped failure' --exit 1 --summary scoped) >/dev/null 2>&1
grep -Fq 'repo-scoped failure' "$LEDGER" || fail "--repo should select the failure ledger state repo"
[ ! -e "$caller/.oms" ] || fail "--repo must not create failure state in the caller directory"
state_caller="$TMP/state-caller"
mkdir -p "$state_caller"
(cd "$state_caller" && OMS_STATE_REPO="$repo" "$tool" record \
  --cmd 'environment-scoped failure' --exit 1 --summary scoped) >/dev/null 2>&1
grep -Fq 'environment-scoped failure' "$LEDGER" ||
  fail "OMS_STATE_REPO should select the failure ledger state repo"
[ ! -e "$state_caller/.oms" ] || fail "OMS_STATE_REPO must not create caller-local failure state"
(cd "$repo" && "$tool" record --cmd 'bash scripts/check.sh' --exit 1 --summary failed) >/dev/null 2>&1
grep -Fq '"schema": 2' "$LEDGER" || fail "schema-2 failure row missing"
grep -Eq '"state_fingerprint": "[0-9a-f]{40,64}:[0-9a-f]{64}:[0-9a-f]{64}"' "$LEDGER" ||
  fail "content-free git state fingerprint missing"

rc=0
(cd "$repo" && "$tool" check --cmd 'bash scripts/check.sh') >"$TMP/same.out" 2>"$TMP/same.err" || rc=$?
[ "$rc" = 3 ] || fail "unchanged retry should exit 3, got $rc"
grep -Fq 'already failed' "$TMP/same.err" || fail "unchanged warning missing"

printf 'two\n' > "$repo/file.txt"
(cd "$repo" && "$tool" check --cmd 'bash scripts/check.sh') >"$TMP/changed.out" 2>"$TMP/changed.err" ||
  fail "changed tracked state should permit retry"
grep -Fq 'git state changed; retry allowed' "$TMP/changed.err" || fail "changed-state warning missing"

git -C "$repo" add file.txt
(cd "$repo" && "$tool" check --cmd 'bash scripts/check.sh') >/dev/null 2>"$TMP/staged.err" ||
  fail "staged tracked state should permit retry"

git -C "$repo" checkout -q -- file.txt
printf 'generated\n' > "$repo/generated.ok"
(cd "$repo" && "$tool" check --cmd 'bash scripts/check.sh') >/dev/null 2>"$TMP/untracked.err" ||
  fail "untracked output state should permit retry"
grep -Fq 'git state changed; retry allowed' "$TMP/untracked.err" || fail "untracked-state warning missing"

# A huge sparse untracked file must affect state without being read end-to-end.
python3 - "$repo" "$tool" <<'PY'
import os, subprocess, sys
with open(os.path.join(sys.argv[1], "huge.bin"), "wb") as handle:
    handle.seek(64 * 1024 * 1024 * 1024 - 1)
    handle.write(b"x")
subprocess.run([sys.argv[2], "check", "--cmd", "bash scripts/check.sh"],
               cwd=sys.argv[1], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
               timeout=3, check=True)
PY

# Legacy rows without state remain conservative instead of silently bypassing
# a known failure after an upgrade.
legacy="$TMP/legacy.jsonl"
OMS_FAIL_LEDGER="$legacy" bash -c 'cd "$1" && "$2" record --cmd legacy --exit 1' _ "$repo" "$tool" >/dev/null 2>&1
python3 - "$legacy" <<'PY'
import json, sys
p=sys.argv[1]
d=json.loads(open(p).readline())
d.pop("state_fingerprint", None); d["schema"]=1
open(p,"w").write(json.dumps(d)+"\n")
PY
rc=0
OMS_FAIL_LEDGER="$legacy" bash -c 'cd "$1" && "$2" check --cmd legacy' _ "$repo" "$tool" >/dev/null 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "legacy unresolved failure should remain blocking"

# Mechanical advisor triggers beyond the repeat count (lib/advise_triggers.py):
# a conflict in the index and a critical-path change each add one suggestion
# line, once per fingerprint, and never when the tree is ordinary.
adv="$TMP/adv"
mkdir -p "$adv"
git -C "$adv" init -q -b main
git -C "$adv" config user.email test@example.com
git -C "$adv" config user.name Test
printf 'one\n' > "$adv/file.txt"
git -C "$adv" add file.txt
git -C "$adv" commit -qm base
adv_record() {  # adv_record CMD -> combined output
  (cd "$adv" && "$tool" record --cmd "$1" --exit 1 2>&1) || fail "record failed for $1"
}
adv_lines() { printf '%s\n' "$1" | grep -c 'fail-ledger: advise:' || true; }

out="$(adv_record 'adv ordinary one')"
[ "$(adv_lines "$out")" = 0 ] || fail "an ordinary tree must add no trigger line: $out"
printf 'two\n' > "$adv/file.txt"
mkdir -p "$adv/docs"
printf 'doc\n' > "$adv/docs/note.md"
out="$(adv_record 'adv docs only')"
[ "$(adv_lines "$out")" = 0 ] || fail "a docs/source edit is not a contract change: $out"

mkdir -p "$adv/config"
printf '{}\n' > "$adv/config/x.json"
out="$(adv_record 'adv contract one')"
printf '%s\n' "$out" | grep -Fq 'fail-ledger: advise: the tree changes a contract or public-surface path (contract-boundary:config/x.json)' ||
  fail "a critical-path change should name its reason: $out"
printf '%s\n' "$out" | grep -F 'advise:' | grep -Fq 'oms advise' ||
  fail "the suggestion must name the advisor command: $out"
printf '%s\n' "$out" | grep -F 'advise:' | grep -Fq 'if authorized' ||
  fail "the suggestion must not authorize a model call: $out"
out="$(adv_record 'adv contract one')"
[ "$(adv_lines "$out")" = 0 ] || fail "a trigger prints once per fingerprint: $out"
out="$(adv_record 'adv contract two')"
[ "$(adv_lines "$out")" = 1 ] || fail "a different fingerprint gets its own suggestion: $out"
out="$(cd "$adv" && OMS_ADVISE_AFTER_FAILURES=0 "$tool" record --cmd 'adv contract three' --exit 1 2>&1)"
[ "$(adv_lines "$out")" = 0 ] || fail "OMS_ADVISE_AFTER_FAILURES=0 must silence triggers: $out"
grep -F '"advised": ["contract_surface"]' "$adv/.oms/failures.jsonl" > /dev/null ||
  fail "the printing row should record which trigger it printed"
rm -rf "$adv/config" "$adv/docs"

git -C "$adv" checkout -q -- file.txt
git -C "$adv" checkout -q -b side
printf 'side\n' > "$adv/file.txt"
git -C "$adv" commit -qam side
git -C "$adv" checkout -q main
printf 'main\n' > "$adv/file.txt"
git -C "$adv" commit -qam mainline
out="$(adv_record 'git merge side before')"
[ "$(adv_lines "$out")" = 0 ] || fail "no conflict yet, so no suggestion: $out"
git -C "$adv" merge side >/dev/null 2>&1 || true
out="$(adv_record 'git merge side')"
printf '%s\n' "$out" | grep -Fq 'advise: a merge/rebase conflict is unresolved in the tree' ||
  fail "an unmerged index should suggest the advisor: $out"
out="$(adv_record 'git merge side')"
[ "$(adv_lines "$out")" = 0 ] || fail "the conflict suggestion prints once per fingerprint: $out"
git -C "$adv" merge --abort
out="$(adv_record 'git merge side after abort')"
[ "$(adv_lines "$out")" = 0 ] || fail "a resolved or aborted conflict must not suggest: $out"

echo "autonomy-failure-smoke: ok"
