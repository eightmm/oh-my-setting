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
contains() { grep -Fq -- "$2" "$1" || { cat "$1" >&2; fail "$1 missing: $2"; }; }

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
  grep -qx "$expected" src/value.txt || { echo "expected product value: $expected"; return 1; }
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

# An owner can explicitly accept an intentional contract change, with both the
# reason and the failed baseline preserved. Candidate verification still runs.
reason='Owner reviewed the expected product value change'
"$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-floor-change "$reason" --report "$TMP/floor-change.md" >/dev/null 2>&1 ||
  fail "explicit owner floor override did not admit the verified candidate"
contains "$TMP/floor-change.md" 'Patch admission: ADMIT'
contains "$TMP/floor-change.md" 'verify: PASS'
contains "$TMP/floor-change.md" 'verify-floor: PASS'
contains "$TMP/floor-change.md" "$reason"
contains "$TMP/floor-change.md" 'floor_change_used: true'
contains "$TMP/floor-change.md" 'base floor failed (exit 1)'
contains "$TMP/floor-change.md" 'expected product value: good'

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh; exit 9' --allow-verifier-change \
  --allow-floor-change "$reason" --report "$TMP/candidate-failed.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "floor override accepted a failing candidate"
contains "$TMP/candidate-failed.md" 'verify: FAIL'
contains "$TMP/candidate-failed.md" 'floor_change_used: false'
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-floor-change "$reason" \
  --report "$TMP/verifier-consent.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "floor override bypassed verifier consent"
contains "$TMP/verifier-consent.md" 'verifier: FAIL'
contains "$TMP/verifier-consent.md" 'verify: SKIP'
for invalid_reason in '' '   ' $'line one\nline two'; do
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
    --verify 'bash scripts/check.sh' --allow-verifier-change \
    --allow-floor-change "$invalid_reason" >"$TMP/invalid-reason.out" 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "empty or multiline floor reason accepted"
  contains "$TMP/invalid-reason.out" '--allow-floor-change requires'
done
rc=0
OMS_HARNESS_CHILD=1 "$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --allow-floor-change "$reason" >"$TMP/child-override.out" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "worker could grant itself a floor override"
contains "$TMP/child-override.out" 'parent-only'

# Approval binds the reason as well as forwarding it. A grant cannot be used
# for another exception, and rejecting that mismatch must preserve the grant.
verify_command='bash scripts/check.sh'
approval="$("$ROOT/scripts/patch-land.sh" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify "$verify_command" --allow-verifier-change --allow-floor-change "$reason" --request-approval)"
"$ROOT/scripts/approval-inbox.sh" --repo "$repo" show --approval "$approval" --json > "$TMP/floor-approval.json"
python3 - "$TMP/floor-approval.json" "$reason" <<'PY_FLOOR_APPROVAL'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as handle:
    request = json.load(handle)
assert request["parameters"]["allow_floor_change_reason"] == sys.argv[2], request
PY_FLOOR_APPROVAL
grant="$("$ROOT/scripts/approval-inbox.sh" --repo "$repo" decide --approval "$approval" \
  --decision approve --expected-version 1 --actor operator)"
rc=0
OMS_REQUIRE_LANDING_APPROVAL=1 "$ROOT/scripts/patch-land.sh" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify "$verify_command" --allow-verifier-change --allow-floor-change 'Different exception' \
  --approval "$approval" --approval-version 2 --approval-token "$grant" \
  >"$TMP/reason-mismatch.out" 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "landing approval accepted a different floor exception"
contains "$repo/src/value.txt" 'good'
OMS_REQUIRE_LANDING_APPROVAL=1 "$ROOT/scripts/patch-land.sh" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-floor-change "$reason" --approval "$approval" --approval-version 2 --approval-token "$grant" \
  >"$TMP/land-floor-change.out" 2>&1 ||
  fail "patch-land did not forward the floor override"
contains "$repo/src/value.txt" 'bad'
git -C "$repo" checkout -q -- src/value.txt pyproject.toml

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
case "${1:-fast}" in
  fast) bash scripts/check-python.sh ;;
  *) exit 2 ;;
esac
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

# A custom acceptance command can depend on a non-conventional data/helper
# file that command-token heuristics cannot discover. The reviewed plan binds
# that explicit surface, and admission must restore it for the base-owned run.
mkdir -p "$repo/quality"
cat > "$repo/quality/oracle.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
expected="$(sed -n 's/^expected=//p' quality/contract.data)"
grep -qx "$expected" src/value.txt
EOF
printf 'expected=good\n' > "$repo/quality/contract.data"
printf 'good\n' > "$repo/src/value.txt"
chmod +x "$repo/quality/oracle.sh"
git -C "$repo" add quality src/value.txt
git -C "$repo" commit -qm custom-oracle

# A reviewed plan task exposes its project contract through `show`. Build the
# minimal frozen fixture directly: apply-proposal owns creation in production,
# while this focused admission test does not need a provider/proposal round.
mkdir -p "$repo/.oms/plan"
cat > "$repo/.oms/plan/tasks.json" <<'EOF'
{
  "schema": 3,
  "goal": "keep the custom oracle honest",
  "accept": "bash quality/oracle.sh",
  "project_contract": {
    "schema": 1,
    "spec_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "allowed_envelope": ["src", "quality"],
    "acceptance_files": ["quality/contract.data", "quality/oracle.sh"]
  },
  "tasks": {
    "custom-oracle-task": {
      "id": "custom-oracle-task", "title": "change the product", "state": "ready",
      "depends": [], "allowed_paths": ["src", "quality"], "forbidden_paths": [],
      "verify": "bash quality/oracle.sh", "role": "", "provider": "", "ttl": "",
      "artifact": "", "patch": "", "reason": "", "executor_id": "",
      "executor_soul_sha256": "", "lease_epoch": 0, "lease_id": "",
      "review_lease_id": "", "repair_count": 0, "repair_artifact": "",
      "created": "2026-01-01T00:00:00Z", "updated": "2026-01-01T00:00:00Z"
    }
  }
}
EOF

printf 'bad\n' > "$repo/src/value.txt"
printf 'expected=bad\n' > "$repo/quality/contract.data"
git -C "$repo" diff > "$TMP/custom-oracle.patch"
git -C "$repo" checkout -q -- src/value.txt quality/contract.data

rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/custom-oracle.patch" \
  --plan-task custom-oracle-task --verify 'bash quality/oracle.sh' \
  --allow-verifier-change --report "$TMP/custom-oracle.md" \
  >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "reviewed custom acceptance surface bypass should be rejected, got exit $rc"
contains "$TMP/custom-oracle.md" "verify: PASS"
contains "$TMP/custom-oracle.md" "verify-floor: FAIL"
contains "$TMP/custom-oracle.md" "quality/contract.data"

# Exact executable paths distinguish the test from same-named product files.
cat > "$repo/tests/feature.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
fail() { echo "$*" >&2; exit 1; }
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
out='plain text'
[ -s src/value.txt ] || fail "missing value"
EOF
printf '# product\n' > "$repo/src/feature.sh"
printf 'value = 1\n' > "$repo/src/feature.py"
printf 'assert True\n' > "$repo/tests/feature.py"
printf '$0 != "good" { exit 1 }\n' > "$repo/quality/oracle.awk"
printf 'select(. == "good") // error("bad value")\n' > "$repo/quality/oracle.jq"
printf 'p\n' > "$repo/quality/oracle.sed"
cp "$repo/quality/oracle.sh" "$repo/quality/env"
git -C "$repo" add tests/feature.sh tests/feature.py src/feature.sh src/feature.py quality
git -C "$repo" commit -qm exact-verifier-fixtures
printf '# changed product\n' >> "$repo/src/feature.sh"
git -C "$repo" diff > "$TMP/same-name.patch"
git -C "$repo" checkout -q -- src/feature.sh
"$ADMIT" --repo "$repo" --patch "$TMP/same-name.patch" \
  --verify 'bash ./tests/feature.sh' --report "$TMP/same-name.md" >/dev/null 2>&1 ||
  fail "same-named product file was mistaken for its test"
contains "$TMP/same-name.md" 'verifier: PASS'
printf 'value = 2\n' > "$repo/src/feature.py"
git -C "$repo" diff > "$TMP/reader.patch"
git -C "$repo" checkout -q -- src/feature.py
"$ADMIT" --repo "$repo" --patch "$TMP/reader.patch" \
  --verify 'grep -qx "value = 2" src/feature.py' --report "$TMP/reader.md" >/dev/null 2>&1 ||
  fail "a content-reader argument was mistaken for its verifier"
contains "$TMP/reader.md" 'verify: PASS'
for reader in cat head tail wc ls stat file shasum md5sum cksum sum \
  sha1sum sha224sum sha256sum sha384sum sha512sum b2sum diff cmp; do
  command -v "$reader" >/dev/null 2>&1 || { echo "skip: $reader data-reader execution (not installed)"; continue; }
  verify="$reader src/feature.py"
  case "$reader" in diff|cmp) verify="$verify src/feature.py" ;; esac
  "$ADMIT" --repo "$repo" --patch "$TMP/reader.patch" --verify "$verify" \
    --report "$TMP/reader.md" >/dev/null 2>&1 || fail "$reader data operand was mistaken for a verifier"
  contains "$TMP/reader.md" 'verify: PASS'
done

# Program tools treat every existing file operand as a verifier. Inline input
# keeps the changed program as the only surface restored for these floor checks.
for verify in 'awk -f quality/oracle.awk src/value.txt' \
  'awk -fquality/oracle.awk src/value.txt' 'awk -f ./quality/oracle.awk src/value.txt' \
  'sed -f quality/oracle.sed src/value.txt | grep -qx good' \
  'sed -fquality/oracle.sed src/value.txt | grep -qx good' \
  'sed -nf quality/oracle.sed src/value.txt | grep -qx good' \
  'sed -bf quality/oracle.sed src/value.txt | grep -qx good' \
  'jq -eR -f quality/oracle.jq src/value.txt' \
  'jq -Rf -e quality/oracle.jq src/value.txt' \
  'jq -eR --from-file quality/oracle.jq src/value.txt'; do
  case "$verify" in
    awk*) program=quality/oracle.awk; printf 'BEGIN { exit 0 }\n' > "$repo/$program" ;;
    sed*) program=quality/oracle.sed; printf 's/^bad$/good/\np\n' > "$repo/$program" ;;
    jq*) program=quality/oracle.jq; printf 'true\n' > "$repo/$program" ;;
  esac
  verify="printf 'bad\\n' | ${verify/ src\/value.txt/}"
  git -C "$repo" diff > "$TMP/program.patch"
  git -C "$repo" checkout -q -- "$program" src/value.txt
  for consent in denied allowed; do
    if [ "$consent" = allowed ]; then
      case "$program" in *.jq)
        command -v jq >/dev/null 2>&1 || { echo 'skip: jq floor execution (jq not installed)'; continue; } ;;
        *.sed)
        case "$verify" in *'sed -bf '*)
          sed -b '' /dev/null >/dev/null 2>&1 || { echo 'skip: sed -b floor execution (not supported)'; continue; } ;;
        esac ;;
      esac
    fi
    rc=0
    extra=(--report "$TMP/program.md")
    [ "$consent" = denied ] || extra+=(--allow-verifier-change)
    "$ADMIT" --repo "$repo" --patch "$TMP/program.patch" --verify "$verify" \
      "${extra[@]}" >/dev/null 2>&1 || rc=$?
    [ "$rc" = 1 ] || fail "program file bypassed $consent consent/floor: $verify"
    if [ "$consent" = denied ]; then
      contains "$TMP/program.md" 'verifier: FAIL'
      contains "$TMP/program.md" 'verify: SKIP'
    else
      contains "$TMP/program.md" 'verify: PASS'
      contains "$TMP/program.md" 'verify-floor: FAIL'
    fi
  done
done

# Even a data operand or a file following unfamiliar grouped flags is protected,
# without depending on the program tool being installed or accepting those flags.
for verify in 'awk -v ignored=1 src/feature.py' 'gawk -f src/feature.py' \
  'mawk -f src/feature.py' 'nawk -f src/feature.py' \
  'sed -bf src/feature.py' 'jq -Rf src/feature.py' \
  'sed -n p src/feature.py' 'jq . src/feature.py' \
  'cd src && sed -bf ./feature.py' 'command sed -bf src/feature.py'; do
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/reader.patch" --verify "$verify" \
    --report "$TMP/program-operand.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "program operand bypassed consent: $verify"
  contains "$TMP/program-operand.md" 'verifier: FAIL'
  contains "$TMP/program-operand.md" 'verify: SKIP'
done

# Removing a named program cannot hide it from consent after it stops existing
# in the candidate worktree.
rm "$repo/quality/oracle.sed"
git -C "$repo" diff > "$TMP/deleted-program.patch"
git -C "$repo" checkout -q -- quality/oracle.sed
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/deleted-program.patch" \
  --verify 'sed -bf quality/oracle.sed src/value.txt' \
  --report "$TMP/deleted-program.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "deleted program bypassed verifier consent"
contains "$TMP/deleted-program.md" 'verifier: FAIL'
contains "$TMP/deleted-program.md" 'verify: SKIP'

# A repository executable named like a wrapper is the verifier itself.
printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/quality/env"
printf 'bad\n' > "$repo/src/value.txt"
git -C "$repo" diff > "$TMP/repo-wrapper.patch"
git -C "$repo" checkout -q -- quality/env src/value.txt
for wrapper in quality/env ./quality/env; do
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/repo-wrapper.patch" \
    --verify "$wrapper true" --report "$TMP/repo-wrapper.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "repository wrapper bypassed consent: $wrapper"
  contains "$TMP/repo-wrapper.md" 'verifier: FAIL'
  contains "$TMP/repo-wrapper.md" 'verify: SKIP'
done
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/repo-wrapper.patch" \
  --verify 'quality/env true' --allow-verifier-change \
  --report "$TMP/repo-wrapper.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "repository wrapper bypassed the base floor"
contains "$TMP/repo-wrapper.md" 'verify: PASS'
contains "$TMP/repo-wrapper.md" 'verify-floor: FAIL'

# Every change to an invoked suite needs consent, including pure assertions.
printf '[ -f src/feature.py ] || fail "missing product"\n' >> "$repo/tests/feature.sh"
printf '%s\n' 'contains "$out" text' >> "$repo/tests/feature.sh"
git -C "$repo" diff > "$TMP/assertion-extension.patch"
git -C "$repo" checkout -q -- tests/feature.sh
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/assertion-extension.patch" \
  --verify 'bash tests/feature.sh' --report "$TMP/assertion-extension.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "assertion-only verifier extension bypassed consent"
contains "$TMP/assertion-extension.md" 'verifier: FAIL'
contains "$TMP/assertion-extension.md" 'verify: SKIP'
"$ADMIT" --repo "$repo" --patch "$TMP/assertion-extension.patch" \
  --verify 'bash tests/feature.sh' --allow-verifier-change \
  --report "$TMP/assertion-extension.md" >/dev/null 2>&1 || fail "consented assertion extension rejected"
contains "$TMP/assertion-extension.md" 'verify: PASS'
contains "$TMP/assertion-extension.md" 'verify-floor: PASS'
printf 'assert 1 == 1\nassert "x" in "text"\n' >> "$repo/tests/feature.py"
git -C "$repo" diff > "$TMP/python-extension.patch"
git -C "$repo" checkout -q -- tests/feature.py
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/python-extension.patch" \
  --verify 'python3 tests/feature.py' --report "$TMP/python-extension.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "pure Python assertion append bypassed consent"
contains "$TMP/python-extension.md" 'verifier: FAIL'
contains "$TMP/python-extension.md" 'verify: SKIP'
"$ADMIT" --repo "$repo" --patch "$TMP/python-extension.patch" \
  --verify 'python3 tests/feature.py' --allow-verifier-change \
  --report "$TMP/python-extension.md" >/dev/null 2>&1 || fail "consented Python extension rejected"
contains "$TMP/python-extension.md" 'verify: PASS'
contains "$TMP/python-extension.md" 'verify-floor: PASS'
# Replacing an assertion with a weaker one is not an extension, even when the
# assertion count is unchanged. Inserting an early exit is not one either.
for edit in weaken early-exit inserted-assert assertion-side-effect assertion-branch assertion-expansion; do
  if [ "$edit" = weaken ]; then
    sed 's/\[ -s src\/value.txt \]/[ -n src\/value.txt ]/' "$repo/tests/feature.sh" > "$TMP/feature.new"
  elif [ "$edit" = inserted-assert ]; then
    sed '/set -eu/a\
[ -f src/feature.py ] || fail "missing product"' "$repo/tests/feature.sh" > "$TMP/feature.new"
  elif [ "$edit" = assertion-side-effect ]; then
    cat "$repo/tests/feature.sh" > "$TMP/feature.new"
    printf '[ -n "$(touch "$OMS_TEST_SIDE_EFFECT")" ] || fail "side effect"\n' >> "$TMP/feature.new"
  elif [ "$edit" = assertion-branch ]; then
    cat "$repo/tests/feature.sh" > "$TMP/feature.new"
    printf '%s\n' 'contains "$out" absent || touch "$OMS_TEST_SIDE_EFFECT"' >> "$TMP/feature.new"
  elif [ "$edit" = assertion-expansion ]; then
    cat "$repo/tests/feature.sh" > "$TMP/feature.new"
    cat >> "$TMP/feature.new" <<'EOF_EXPANSION'
contains text text ${probe:=$'\x24\x28touch "$OMS_TEST_SIDE_EFFECT"\x29'}
contains text text ${probe@P}
EOF_EXPANSION
  else
    sed '/set -eu/a\
exit 0' "$repo/tests/feature.sh" > "$TMP/feature.new"
  fi
  cp "$TMP/feature.new" "$repo/tests/feature.sh"
  git -C "$repo" diff > "$TMP/$edit.patch"
  git -C "$repo" checkout -q -- tests/feature.sh
  rc=0
  OMS_TEST_SIDE_EFFECT="$TMP/assertion-ran" "$ADMIT" --repo "$repo" --patch "$TMP/$edit.patch" \
    --verify 'bash tests/feature.sh' --report "$TMP/$edit.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "$edit unexpectedly bypassed verifier consent"
  contains "$TMP/$edit.md" 'verifier: FAIL'
  contains "$TMP/$edit.md" 'verify: SKIP'
  [ ! -e "$TMP/assertion-ran" ] || fail "rejected assertion executed a side effect"
done

printf -v indirect_assertion 'assert [(open)("%s", "w")]' "$TMP/python-assertion-ran"
for placement in inserted appended; do
  for assertion in "$indirect_assertion" 'assert __import__("sys").exit(0)' 'assert exit(0)' \
    'assert eval("True")' 'assert exec("pass")' 'assert os.path.exists("src/value.txt")' \
    'assert sys.exit(0)' 'assert subprocess.call(["true"]) == 0' 'assert quit()' \
    'assert 1 < 2' 'assert 2 > 1' 'assert True & True' 'assert True | False' \
    'assert True; assert True' 'assert True # `call`' 'assert True # &&' \
    'assert True # ('; do
    : > "$repo/tests/feature.py"
    [ "$placement" != appended ] || printf 'assert True\n' >> "$repo/tests/feature.py"
    printf '%s\n' "$assertion" >> "$repo/tests/feature.py"
    [ "$placement" != inserted ] || printf 'assert True\n' >> "$repo/tests/feature.py"
    git -C "$repo" diff > "$TMP/python-assertion.patch"
    git -C "$repo" checkout -q -- tests/feature.py
    rc=0
    "$ADMIT" --repo "$repo" --patch "$TMP/python-assertion.patch" \
      --verify 'python3 tests/feature.py' --report "$TMP/python-assertion.md" >/dev/null 2>&1 || rc=$?
    [ "$rc" = 1 ] || fail "$placement assertion bypassed consent: $assertion"
    contains "$TMP/python-assertion.md" 'verifier: FAIL'
    contains "$TMP/python-assertion.md" 'verify: SKIP'
    [ ! -e "$TMP/python-assertion-ran" ] || fail "rejected Python assertion executed a host-side effect"
  done
done
# Sourcing or wrapping the suite still names it as the verifier.
for wrapped in '. tests/feature.sh' 'timeout 10 bash tests/feature.sh' \
  'nice -n 5 bash tests/feature.sh' '/usr/bin/env bash tests/feature.sh'; do
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/early-exit.patch" \
    --verify "$wrapped" --report "$TMP/wrapped.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = 1 ] || fail "'$wrapped' let an early exit bypass verifier consent"
  contains "$TMP/wrapped.md" 'verifier: FAIL'
done

# Newly added, directly invoked tests run on the candidate; unchanged HEAD
# checks run on the floor instead of attempting to execute the removed file.
printf 'from pathlib import Path\nassert Path("src/value.txt").read_text().strip() == "good"\n' > "$repo/tests/test_new.py"
git -C "$repo" add -N tests/test_new.py
git -C "$repo" diff > "$TMP/new-test.patch"
git -C "$repo" reset -q HEAD -- tests/test_new.py
rm "$repo/tests/test_new.py"
"$ADMIT" --repo "$repo" --patch "$TMP/new-test.patch" \
  --verify 'python3 tests/test_new.py' --report "$TMP/new-test.md" >/dev/null 2>&1 ||
  fail "new directly invoked regression test should be admissible"
contains "$TMP/new-test.md" 'verify: PASS'
contains "$TMP/new-test.md" 'verify-floor: PASS'
# A new passing test must not replace the unchanged product baseline.
printf 'bad\n' > "$repo/src/value.txt"
printf 'assert True\n' > "$repo/tests/test_new.py"
git -C "$repo" add -N tests/test_new.py
git -C "$repo" diff > "$TMP/new-test-bypass.patch"
git -C "$repo" reset -q HEAD -- tests/test_new.py
rm "$repo/tests/test_new.py"
git -C "$repo" checkout -q -- src/value.txt
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/new-test-bypass.patch" \
  --verify 'python3 tests/test_new.py' --report "$TMP/new-test-bypass.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "candidate-only test replaced the baseline"
contains "$TMP/new-test-bypass.md" 'verify: PASS'
contains "$TMP/new-test-bypass.md" 'verify-floor: FAIL'

# Worktree creation/checkouts can invoke worker-planted fsmonitor or content
# filters under the admitting parent's authority. Refuse executable Git config
# before the first such Git operation; the marker proves refusal came first.
cat > "$repo/.git/hostile-fsmonitor" <<EOF
#!/usr/bin/env bash
: > "$TMP/admit-fsmonitor-fired"
exit 0
EOF
chmod +x "$repo/.git/hostile-fsmonitor"
git -C "$repo" config --local core.fsmonitor "$repo/.git/hostile-fsmonitor"
rm -f "$TMP/admit-fsmonitor-fired"
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/benign.patch" \
  --verify 'bash scripts/check.sh' --report "$TMP/hostile-config.md" \
  > "$TMP/hostile-config.out" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "executable Git config should fail admission closed, got $rc"
[ ! -e "$TMP/admit-fsmonitor-fired" ] ||
  fail "patch admission executed worker-planted fsmonitor"

# --- Proposal admission rejects floor-incompatible verifies ------------------
# The live 0.6.0 campaign parked on a planner-authored verify that greps a
# file the task itself modifies: the base floor restores the verifier from
# HEAD, so such a check can never pass. The generator of that dead state is
# now refused at the single admission door (three-family council, 2026-08-18).
floor_repo="$TMP/floor-admission"
mkdir -p "$floor_repo/tests" "$floor_repo/docs"
git -C "$floor_repo" init -q -b main
git -C "$floor_repo" config user.email test@example.com
git -C "$floor_repo" config user.name test
printf '#!/usr/bin/env bash\necho ok\n' > "$floor_repo/tests/suite.sh"
printf 'ok\n' > "$floor_repo/docs/other.md"
printf '# probe\n\n- State: confirmed\n\n## Goal\n\nprobe\n' > "$floor_repo/PROJECT.md"
git -C "$floor_repo" add -A
git -C "$floor_repo" commit -qm fixtures

floor_proposal() {  # floor_proposal VERIFY -> apply-proposal stderr tail
  local verify="$1" spec base psha
  spec="$(sha256sum "$floor_repo/PROJECT.md" | cut -d' ' -f1)"
  base="$(git -C "$floor_repo" rev-parse HEAD)"
  OMS_T_VERIFY="$verify" OMS_T_SPEC="$spec" OMS_T_BASE="$base" python3 - "$floor_repo/p.json" <<'PY'
import json, os, sys
doc = {"schema": 1, "kind": "agent-plan-proposal",
       "spec_sha256": os.environ["OMS_T_SPEC"], "plan_sha256": "absent",
       "base_sha": os.environ["OMS_T_BASE"], "id_prefix": "",
       "allowed_envelope": ["tests/suite.sh"],
       "acceptance_files": ["tests/suite.sh"],
       "tasks": [{"id": "t1", "title": "probe", "allowed": ["tests/suite.sh"],
                  "verify": os.environ["OMS_T_VERIFY"], "depends": []}]}
open(sys.argv[1], "w").write(json.dumps(doc))
PY
  psha="$(sha256sum "$floor_repo/p.json" | cut -d' ' -f1)"
  rm -rf "$floor_repo/.oms/plan"
  "$ROOT/scripts/agent-plan.sh" --repo "$floor_repo" apply-proposal \
    --proposal "$floor_repo/p.json" --expected-proposal-sha256 "$psha" \
    --expected-plan-sha256 absent --allowed-envelope "tests/suite.sh" \
    --accept-files "tests/suite.sh" --goal probe --accept "bash tests/suite.sh" \
    2>&1 | tail -1 || true
}

out="$(floor_proposal "bash -c 'grep -q endswith tests/suite.sh && bash tests/suite.sh'")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "the field-defect verify shape must be rejected at admission: $out"
out="$(floor_proposal "python3 - < tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a redirect read of a modified file must be rejected: $out"

# Hash, text, and binary inspection siblings are content readers too. Keep
# this static so the regression does not depend on any reader being installed.
for reader in jq sha256sum md5sum shasum cksum comm join paste nl tac rev xxd hexdump base64 \
  sha1sum sha224sum sha384sum sha512sum b2sum sum base32 basenc; do
  out="$(floor_proposal "$reader tests/suite.sh")"
  printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
    fail "$reader reading a modified file must be rejected: $out"
done
out="$(floor_proposal "/usr/bin/jq tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a path-qualified reader must be rejected: $out"
out="$(floor_proposal "jq ./tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a reader using the canonical ./ spelling must be rejected: $out"
out="$(floor_proposal "python3 - <./tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a redirect using the canonical ./ spelling must be rejected: $out"
out="$(floor_proposal "CHECK_MODE=strict sha256sum tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a reader after an environment assignment must be rejected: $out"
out="$(floor_proposal "bash -c 'base64 tests/suite.sh'")"
printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
  fail "a reader inside bash -c must be rejected: $out"
for verify in \
  "command jq tests/suite.sh" \
  "command -p jq tests/suite.sh" \
  "command -- jq tests/suite.sh" \
  "env jq tests/suite.sh" \
  "env CHECK_MODE=strict jq tests/suite.sh" \
  "env -- CHECK_MODE=strict jq tests/suite.sh" \
  "/usr/bin/env jq tests/suite.sh"; do
  out="$(floor_proposal "$verify")"
  printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
    fail "a strict command wrapper must not hide a reader ($verify): $out"
done

out="$(floor_proposal "bash tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'proposal applied' ||
  fail "executing the restored suite must stay admissible: $out"
for verify in \
  "command -v jq tests/suite.sh" \
  "command -V jq tests/suite.sh" \
  "command bash tests/suite.sh" \
  "env bash tests/suite.sh" \
  "./env jq tests/suite.sh" \
  "/tmp/command jq tests/suite.sh" \
  "jq 'tests/*.sh'"; do
  out="$(floor_proposal "$verify")"
  printf '%s' "$out" | grep >/dev/null -F 'proposal applied' ||
    fail "a non-reader wrapper/query must stay admissible ($verify): $out"
done
out="$(floor_proposal "grep -q ok docs/other.md && bash tests/suite.sh")"
printf '%s' "$out" | grep >/dev/null -F 'proposal applied' ||
  fail "reading an unmodified path must stay admissible: $out"

echo "patch-admit-verifier-floor-smoke: ok"
