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
printf '%s' "$out" | grep -Fq 'floor_incompatible_verifier' ||
  fail "the field-defect verify shape must be rejected at admission: $out"
out="$(floor_proposal "python3 - < tests/suite.sh")"
printf '%s' "$out" | grep -Fq 'floor_incompatible_verifier' ||
  fail "a redirect read of a modified file must be rejected: $out"
out="$(floor_proposal "bash tests/suite.sh")"
printf '%s' "$out" | grep -Fq 'proposal applied' ||
  fail "executing the restored suite must stay admissible: $out"
out="$(floor_proposal "grep -q ok docs/other.md && bash tests/suite.sh")"
printf '%s' "$out" | grep -Fq 'proposal applied' ||
  fail "reading an unmodified path must stay admissible: $out"

echo "patch-admit-verifier-floor-smoke: ok"
