#!/usr/bin/env bash
set -euo pipefail

# The path-scope gate is the only thing in the admission ladder that says WHERE
# a patch may write, and an unscoped call has nothing to enforce. These cases
# pin the floor that stands in for it: an unscoped patch may edit this tree but
# not rearrange it, and a declared scope still outranks the floor.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADMIT="$ROOT/scripts/patch-admit.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-admit-structural.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "$1 missing: $2"; }
lacks() { if grep -Fq -- "$2" "$1"; then fail "$1 should not contain: $2"; fi; }

make_repo() {
  mkdir -p "$1"
  git -C "$1" init -q -b main
  git -C "$1" config user.email test@example.com
  git -C "$1" config user.name test
  mkdir -p "$1/src" "$1/tests"
  printf 'base\n' > "$1/file.txt"
  printf 'echo a\n' > "$1/src/a.sh"
  printf 'assert_ok 1\n' > "$1/tests/foo.sh"
  git -C "$1" add -A
  git -C "$1" commit -qm init
}

# Commit the working-tree change, capture it as a patch with rename detection
# on, then rewind: the patch has to be a real one git produced, not a
# hand-rolled diff. Patches live outside the repo so `add -A` cannot eat them.
capture_patch() {  # capture_patch REPO MESSAGE OUT
  git -C "$1" add -A
  git -C "$1" commit -qm "$2"
  git -C "$1" diff -M HEAD~1 HEAD > "$3"
  git -C "$1" reset -q --hard HEAD~1
}

repo="$TMP/repo"
make_repo "$repo"

# A move out of tests/ to the repo root: the shape of the incident this gate
# exists for (malformed worker headers that git applied as a rename).
git -C "$repo" mv tests/foo.sh foo.sh
capture_patch "$repo" move "$TMP/rename.patch"

printf 'echo a\necho more\n' > "$repo/src/a.sh"
capture_patch "$repo" edit "$TMP/inplace.patch"

printf 'echo b\n' > "$repo/src/b.sh"
capture_patch "$repo" newfile "$TMP/subdir-add.patch"

# (1) Unscoped restructure is rejected, and the floor is the only gate that
# objects — everything the older ladder checked is green here.
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
contains "$TMP/r1.md" "verify: PASS"

# (2) The same patch is admissible once the restructure is declared.
"$ADMIT" --repo "$repo" --patch "$TMP/rename.patch" --verify true \
  --allow-restructure --report "$TMP/r2.md" >/dev/null 2>&1 ||
  fail "--allow-restructure should admit the same patch"
contains "$TMP/r2.md" "Patch admission: ADMIT"
contains "$TMP/r2.md" "structure: PASS"
contains "$TMP/r2.md" "restructure permitted by --allow-restructure"

# (3) An in-place edit under an existing directory is ordinary work.
"$ADMIT" --repo "$repo" --patch "$TMP/inplace.patch" --verify true \
  --report "$TMP/r3.md" >/dev/null 2>&1 ||
  fail "an unscoped in-place patch should still be admitted"
contains "$TMP/r3.md" "Patch admission: ADMIT"
contains "$TMP/r3.md" "structure: PASS"

# (4) The floor is top-level and cross-directory only: growing an existing
# directory must not need an override, or every worker learns to pass one.
"$ADMIT" --repo "$repo" --patch "$TMP/subdir-add.patch" --verify true \
  --report "$TMP/r4.md" >/dev/null 2>&1 ||
  fail "a new file in an existing subdirectory should be admitted"
contains "$TMP/r4.md" "Patch admission: ADMIT"
contains "$TMP/r4.md" "structure: PASS"

# (5) Regression guard: a declared scope decides on its own terms, and the
# floor stands down rather than double-judging the same paths.
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

echo "patch-admit-structural-smoke: ok"
