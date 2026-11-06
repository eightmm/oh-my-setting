#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
ADMIT="$ROOT/scripts/patch-admit.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-admit-structural.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "$1 missing: $2"; }
lacks() { if grep -Fq -- "$2" "$1"; then fail "$1 should not contain: $2"; fi; }

capture_patch() {  # capture_patch REPO MESSAGE OUT
  git -C "$1" add -A
  git -C "$1" commit -qm "$2"
  git -C "$1" diff -M HEAD~1 HEAD > "$3"
  git -C "$1" reset -q --hard HEAD~1
}

repo="$TMP/repo"
mkdir -p "$repo/src" "$repo/tests"
git -C "$repo" init -q -b main
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
printf 'base\n' > "$repo/file.txt"
printf 'echo a\n' > "$repo/src/a.sh"
printf 'assert_ok 1\n' > "$repo/tests/foo.sh"
git -C "$repo" add -A
git -C "$repo" commit -qm init

git -C "$repo" mv tests/foo.sh foo.sh
capture_patch "$repo" move "$TMP/rename.patch"

printf 'echo a\necho more\n' > "$repo/src/a.sh"
capture_patch "$repo" edit "$TMP/inplace.patch"

printf 'echo b\n' > "$repo/src/b.sh"
capture_patch "$repo" newfile "$TMP/subdir-add.patch"

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/rename.patch" --verify true \
  --report "$TMP/r1.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "an unscoped restructuring patch should be rejected, got exit $rc"
contains "$TMP/r1.md" "Patch admission: REJECT"
contains "$TMP/r1.md" "structure: FAIL"
contains "$TMP/r1.md" "new top-level file: foo.sh"
contains "$TMP/r1.md" "moved across directories: tests/foo.sh -> foo.sh"
contains "$TMP/r1.md" "--allow-restructure"
contains "$TMP/r1.md" "scope: SKIP"
contains "$TMP/r1.md" "syntax: PASS"
contains "$TMP/r1.md" "tests: PASS"
contains "$TMP/r1.md" "verifier: PASS"
contains "$TMP/r1.md" "verify: SKIP"
contains "$TMP/r1.md" "pre-verification policy or integrity gate failed"

"$ADMIT" --repo "$repo" --patch "$TMP/rename.patch" --verify true \
  --allow-restructure --report "$TMP/r2.md" >/dev/null 2>&1 ||
  fail "--allow-restructure should admit the same patch"
contains "$TMP/r2.md" "Patch admission: ADMIT"
contains "$TMP/r2.md" "structure: PASS"
contains "$TMP/r2.md" "restructure permitted by --allow-restructure"

for p in "$TMP/inplace.patch" "$TMP/subdir-add.patch"; do
  "$ADMIT" --repo "$repo" --patch "$p" --verify true --report "$TMP/r-ok.md" >/dev/null 2>&1 ||
    fail "$p should be admitted"
  contains "$TMP/r-ok.md" "Patch admission: ADMIT"
  contains "$TMP/r-ok.md" "structure: PASS"
done

"$ROOT/scripts/agent-plan.sh" --repo "$repo" init --goal structural >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id t1 --title scoped \
  --allowed 'src/' --verify true >/dev/null
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/rename.patch" --verify true \
  --plan-task t1 --report "$TMP/r5.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "a scope-violating patch should still be rejected, got exit $rc"
contains "$TMP/r5.md" "scope: FAIL"
contains "$TMP/r5.md" "outside allowed paths: foo.sh"
contains "$TMP/r5.md" "structure: SKIP"
lacks "$TMP/r5.md" "structure: FAIL"
contains "$TMP/r5.md" "verify: SKIP"

# (6) Non-ASCII names: git C-quotes them by default ("\353\271..."), which no
# scope prefix matches, so a forbidden Korean directory used to pass the gate.
mkdir -p "$repo/비밀" "$repo/문서"
printf 'x\n' > "$repo/비밀/k.txt"
capture_patch "$repo" secret "$TMP/secret.patch"
printf 'x\n' > "$repo/문서/a.md"
capture_patch "$repo" docs "$TMP/docs.patch"
"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id t3 --title forbid-kr \
  --allowed '.' --forbidden '비밀/' --verify true >/dev/null
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/secret.patch" --verify true \
  --plan-task t3 --report "$TMP/r6.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "a patch into a forbidden non-ASCII directory should be rejected, got exit $rc"
contains "$TMP/r6.md" "scope: FAIL"
contains "$TMP/r6.md" "forbidden: 비밀/k.txt"
"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id t4 --title allow-kr \
  --allowed '문서/' --verify true >/dev/null
"$ADMIT" --repo "$repo" --patch "$TMP/docs.patch" --verify true \
  --plan-task t4 --report "$TMP/r7.md" >/dev/null 2>&1 ||
  fail "a patch into an allowed non-ASCII directory should be admitted"
contains "$TMP/r7.md" "scope: PASS"

# Retained structured plans may contain literal scope entries the current CSV
# CLI cannot create. Keep these snapshots inside this disposable fixture.
for literal_kind in space comma; do
  case "$literal_kind" in
    space) literal_path='docs/secret file.txt' ;;
    comma) literal_path='docs/secret,file.txt' ;;
  esac
  mkdir -p "$repo/docs"
  printf 'x\n' > "$repo/$literal_path"
  capture_patch "$repo" "literal-$literal_kind" "$TMP/literal-$literal_kind.patch"
  for scope_mode in broad literal deny-only positive; do
    literal_task="literal-$literal_kind-$scope_mode"
    verify_marker="$TMP/$literal_task.ran"
    literal_verify="printf ran > \"$verify_marker\""
    "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id "$literal_task" \
      --title literal-scope --allowed docs/ --verify "$literal_verify" >/dev/null
    python3 - "$repo/.oms/plan/tasks.json" "$literal_task" "$literal_path" "$scope_mode" <<'PY'
import json, sys
from pathlib import Path
path, task_id, name, mode = sys.argv[1:]
snapshot = Path(path)
plan = json.loads(snapshot.read_text())
task = plan['tasks'][task_id]
task['allowed_paths'] = [] if mode == 'deny-only' else ['docs/'] if mode == 'broad' else [name]
task['forbidden_paths'] = ['docs/unrelated.txt'] if mode == 'positive' else [name]
snapshot.write_text(json.dumps(plan, ensure_ascii=False) + '\n')
PY
    literal_report="$TMP/$literal_task.md"
    rc=0
    "$ADMIT" --repo "$repo" --patch "$TMP/literal-$literal_kind.patch" \
      --plan-task "$literal_task" --report "$literal_report" >/dev/null 2>&1 || rc=$?
    if [ "$scope_mode" = positive ]; then
      [ "$rc" = 0 ] || fail "a valid literal $literal_kind path should be admitted, got exit $rc"
      contains "$literal_report" 'scope: PASS'
      contains "$literal_report" 'verify: PASS'
      [ -f "$verify_marker" ] || fail "valid literal scope skipped its verifier"
    else
      [ "$rc" = 1 ] || fail "literal deny $literal_kind/$scope_mode should reject before verification, got exit $rc"
      contains "$literal_report" 'scope: FAIL'
      contains "$literal_report" "forbidden: $literal_path"
      contains "$literal_report" 'verify: SKIP'
      [ ! -e "$verify_marker" ] || fail "a forbidden literal path executed its verifier"
    fi
  done
done

"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id empty-scope \
  --title empty-scope --verify true >/dev/null
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/rename.patch" --plan-task empty-scope \
  --report "$TMP/empty-scope.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "empty task scope bypassed the structural floor, got exit $rc"
contains "$TMP/empty-scope.md" 'scope: SKIP'
contains "$TMP/empty-scope.md" 'structure: FAIL'
contains "$TMP/empty-scope.md" 'verify: SKIP'

# (7) U+2028 inside a name must not split it into two allowed-looking paths.
"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id t5 --title ls-split \
  --allowed 'docs/' --forbidden 'docs/*/private.md' --verify true >/dev/null
mkdir -p "$repo/docs"
ls_name="$(printf 'docs/a\xe2\x80\xa8docs/private.md')"
mkdir -p "$repo/docs/a"$'\xe2\x80\xa8'"docs"
printf 'x\n' > "$repo/$ls_name"
capture_patch "$repo" ls-split "$TMP/ls.patch"
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/ls.patch" --verify true \
  --plan-task t5 --report "$TMP/r8.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "a U+2028 name must not slip past the scope gate, got exit $rc"
contains "$TMP/r8.md" "scope: FAIL"

# (8) The unscoped floor cannot judge a name git still C-quotes: a new
# top-level q"new.txt must not look like a nested, already-present path.
printf 'x\n' > "$repo/q\"new.txt" 2>/dev/null && {
  capture_patch "$repo" quoted "$TMP/quoted.patch"
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/quoted.patch" --verify true \
    --report "$TMP/r9.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "an unscoped quoted top-level name should be rejected, got exit $rc"
  contains "$TMP/r9.md" "unquotable path"
}

# Admission adopts the stored verifier; an explicit mismatch fails before
# candidate code executes. Structural-only ADMIT remains inconclusive evidence.
"$ADMIT" --repo "$repo" --patch "$TMP/inplace.patch" \
  --plan-task t1 --report "$TMP/task-verify.md" >/dev/null 2>&1 ||
  fail "stored task verifier was not adopted"
contains "$TMP/task-verify.md" 'verification: command'
contains "$TMP/task-verify.md" 'verify: PASS'
"$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id t2 --title failing \
  --allowed src/ --verify false >/dev/null
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/inplace.patch" \
  --plan-task t2 --report "$TMP/task-failed.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "stored failing verifier was skipped"
contains "$TMP/task-failed.md" 'verify: FAIL'
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/inplace.patch" \
  --plan-task t1 --verify false --report "$TMP/mismatch.md" > "$TMP/mismatch.out" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "task verifier mismatch should be a usage error"
contains "$TMP/mismatch.out" 'does not match plan task t1 review contract'
"$ADMIT" --repo "$repo" --patch "$TMP/inplace.patch" \
  --report "$TMP/structural-only.md" >/dev/null 2>&1 || fail "structural admission failed"
contains "$TMP/structural-only.md" 'verification: none'
python3 - "$repo/.oms/artifacts/index.jsonl" <<'EOF'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1])]
assert rows[-1]["status"] == "inconclusive", rows[-1]
assert "verify_exit" not in rows[-1], rows[-1]
assert rows[-3]["status"] == "verified", rows[-3]
assert rows[-3]["verify_exit"] == 0, rows[-3]
EOF

echo "patch-admit-structural-smoke: ok"
