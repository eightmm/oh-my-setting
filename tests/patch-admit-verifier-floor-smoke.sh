#!/usr/bin/env bash
set -euo pipefail

# A changed helper below tests/ can make the patched verifier pass without
# changing the verifier entrypoint or removing an assertion-looking line. Pin
# the second, base-owned verification run that closes that indirect bypass.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADMIT="$ROOT/scripts/patch-admit.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-admit-verifier-floor.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "$1 missing: $2"; }

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q -b main
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
mkdir -p "$repo/scripts" "$repo/src" "$repo/tests"
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
. tests/check-helper.sh
verify_product
EOF
cat > "$repo/tests/check-helper.sh" <<'EOF'
verify_product() {
  expected="$(sed -n 's/^expected = "\(.*\)"$/\1/p' pyproject.toml)"
  [ -n "$expected" ]
  grep -qx "$expected" src/value.txt
}
EOF
printf 'good\n' > "$repo/src/value.txt"
printf 'expected = "good"\n' > "$repo/pyproject.toml"
chmod +x "$repo/scripts/check.sh"
git -C "$repo" add -A
git -C "$repo" commit -qm init

# The candidate passes only because its indirect test helper was weakened. The
# assertion-count heuristic cannot see this, and both historical overrides are
# present to prove they cannot disable the base verification floor.
printf 'bad\n' > "$repo/src/value.txt"
cat > "$repo/tests/check-helper.sh" <<'EOF'
verify_product() {
  return 0
}
EOF
git -C "$repo" diff > "$TMP/bypass.patch"
git -C "$repo" checkout -q -- src/value.txt tests/check-helper.sh

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/bypass.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/bypass.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "indirect verifier helper bypass should be rejected, got exit $rc"
contains "$TMP/bypass.md" "Patch admission: REJECT"
contains "$TMP/bypass.md" "verify: PASS"
contains "$TMP/bypass.md" "verify-floor: FAIL"
contains "$TMP/bypass.md" "tests/check-helper.sh"

# A harmless helper edit still runs both checks and remains admissible.
cat > "$repo/tests/check-helper.sh" <<'EOF'
# Keep the product contract explicit.
verify_product() {
  expected="$(sed -n 's/^expected = "\(.*\)"$/\1/p' pyproject.toml)"
  [ -n "$expected" ]
  grep -qx "$expected" src/value.txt
}
EOF
git -C "$repo" diff > "$TMP/benign.patch"
git -C "$repo" checkout -q -- tests/check-helper.sh

"$ADMIT" --repo "$repo" --patch "$TMP/benign.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/benign.md" >/dev/null 2>&1 ||
  fail "a harmless verification-surface edit should pass both verification runs"
contains "$TMP/benign.md" "Patch admission: ADMIT"
contains "$TMP/benign.md" "verify: PASS"
contains "$TMP/benign.md" "verify-floor: PASS"

# A common verifier config is in the floor even when VERIFY does not name it.
printf 'bad\n' > "$repo/src/value.txt"
printf 'expected = "bad"\n' > "$repo/pyproject.toml"
git -C "$repo" diff > "$TMP/common-config.patch"
git -C "$repo" checkout -q -- src/value.txt pyproject.toml

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/common-config.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "common verifier config bypass should be rejected, got exit $rc"
contains "$TMP/common-config.md" "verifier change permitted"
contains "$TMP/common-config.md" "verify: PASS"
contains "$TMP/common-config.md" "verify-floor: FAIL"
contains "$TMP/common-config.md" "pyproject.toml"

# The same floor remains mandatory for the explicitly named entrypoint.
printf 'bad\n' > "$repo/src/value.txt"
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
git -C "$repo" diff > "$TMP/entrypoint.patch"
git -C "$repo" checkout -q -- scripts/check.sh src/value.txt

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/entrypoint.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/entrypoint.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "direct verifier entrypoint bypass should be rejected, got exit $rc"
contains "$TMP/entrypoint.md" "verifier change permitted"
contains "$TMP/entrypoint.md" "verify: PASS"
contains "$TMP/entrypoint.md" "verify-floor: FAIL"
contains "$TMP/entrypoint.md" "scripts/check.sh"

# Repo-owned verifier helpers are part of the floor even when they live outside
# tests/ and the command names only the top-level entrypoint. This is the common
# check.sh -> check-python.sh shape used by the harness itself.
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
bash scripts/check-python.sh
EOF
cat > "$repo/scripts/check-python.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
grep -qx good src/value.txt
EOF
chmod +x "$repo/scripts/check.sh" "$repo/scripts/check-python.sh"
git -C "$repo" add scripts/check.sh scripts/check-python.sh
git -C "$repo" commit -qm verifier-helper

printf 'bad\n' > "$repo/src/value.txt"
cat > "$repo/scripts/check-python.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
git -C "$repo" diff > "$TMP/indirect-script-helper.patch"
git -C "$repo" checkout -q -- scripts/check-python.sh src/value.txt

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/indirect-script-helper.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/indirect-script-helper.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "indirect scripts/check-* helper bypass should be rejected, got exit $rc"
contains "$TMP/indirect-script-helper.md" "verify: PASS"
contains "$TMP/indirect-script-helper.md" "verify-floor: FAIL"
contains "$TMP/indirect-script-helper.md" "scripts/check-python.sh"

# A verifier integrity rejection must happen before executing the patched
# verifier. Rejection alone is not enough if untrusted verifier bytes already
# ran with host permissions and produced an outside side effect.
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
: > "${OMS_TEST_SIDE_EFFECT:?}"
exit 0
EOF
git -C "$repo" diff > "$TMP/rejected-verifier.patch"
git -C "$repo" checkout -q -- scripts/check.sh
rm -f "$TMP/rejected-verifier-ran"

rc=0
OMS_TEST_SIDE_EFFECT="$TMP/rejected-verifier-ran" \
  "$ADMIT" --repo "$repo" --patch "$TMP/rejected-verifier.patch" \
  --verify 'bash scripts/check.sh' --report "$TMP/rejected-verifier.md" \
  >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "self-modified verifier should be rejected before execution"
[ ! -e "$TMP/rejected-verifier-ran" ] ||
  fail "rejected candidate verifier still executed with host permissions"
contains "$TMP/rejected-verifier.md" "verifier: FAIL"
contains "$TMP/rejected-verifier.md" "verify: SKIP"

# Auto-detection must come from HEAD. If it inspected the patched tree, deleting
# check.sh would delete both the verifier and the command that should run it.
printf 'bad\n' > "$repo/src/value.txt"
git -C "$repo" rm -q scripts/check.sh
git -C "$repo" diff HEAD > "$TMP/delete-entrypoint.patch"
git -C "$repo" checkout -q HEAD -- scripts/check.sh src/value.txt

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/delete-entrypoint.patch" \
  --allow-verifier-change --allow-test-reduction \
  --report "$TMP/delete-entrypoint.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "deleting the auto-detected verifier should be rejected, got exit $rc"
contains "$TMP/delete-entrypoint.md" "verifier change permitted"
contains "$TMP/delete-entrypoint.md" "verify: FAIL"
contains "$TMP/delete-entrypoint.md" "verify-floor: FAIL"
contains "$TMP/delete-entrypoint.md" "scripts/check.sh"

echo "patch-admit-verifier-floor-smoke: ok"
