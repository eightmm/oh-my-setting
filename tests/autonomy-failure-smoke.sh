#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
python3 - "$repo/huge.bin" <<'PY'
import sys
with open(sys.argv[1], "wb") as handle:
    handle.seek(64 * 1024 * 1024 * 1024 - 1)
    handle.write(b"x")
PY
python3 - "$repo" "$tool" <<'PY'
import subprocess, sys
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

echo "autonomy-failure-smoke: ok"
