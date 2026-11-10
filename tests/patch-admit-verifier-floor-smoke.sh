#!/usr/bin/env bash
set -euo pipefail

# A changed helper below tests/ can make the patched verifier pass without
# changing the verifier entrypoint or removing an assertion-looking line. Pin
# the second, base-owned verification run that closes that indirect bypass.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
# Quota fixtures assert native defaults; operator capacity overrides belong to
# the outer admission, while each limit case below supplies its own override.
unset OMS_LANDING_CAPTURE_COUNT OMS_LANDING_CAPTURE_BYTES
ADMIT="$ROOT/scripts/patch-admit.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-admit-verifier-floor.XXXXXX")"
cleanup() {
  local pid
  for pid in ${reuse_pids:-}; do wait "$pid" || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT HUP INT TERM
mkdir -p "$TMP/tmp"
export TMPDIR="$TMP/tmp"

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

# BEGIN regression-proof
printf 'fixed\n' > "$repo/src/value.txt"
cat > "$repo/tests/regression.sh" <<'EOF'
grep -qx fixed src/value.txt
EOF
git -C "$repo" add -N tests/regression.sh
git -C "$repo" diff > "$TMP/regression.patch"
git -C "$repo" reset -q -- tests/regression.sh
rm "$repo/tests/regression.sh"
git -C "$repo" checkout -q -- src/value.txt
for regression_case in proven not-failing-before failing-after; do
  case "$regression_case" in
    proven) regression_cmd='bash tests/regression.sh'; expected_rc=0 ;;
    not-failing-before) regression_cmd='true'; expected_rc=1 ;;
    failing-after) regression_cmd='false'; expected_rc=1 ;;
  esac
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/regression.patch" --verify true \
    --regression-test "$regression_cmd" --report "$TMP/regression-$regression_case.md" >/dev/null 2>&1 || rc=$?
  [ "$rc" = "$expected_rc" ] || fail "regression $regression_case returned $rc"
  contains "$TMP/regression-$regression_case.md" "regression-proof: $regression_case"
done
for regression_code in 123 124 125 126 127 128; do
  rc=0
  "$ADMIT" --repo "$repo" --patch "$TMP/regression.patch" --verify true \
    --regression-test "exit $regression_code" --report "$TMP/regression-code.md" >/dev/null 2>&1 || rc=$?
  if [ "$regression_code" = 123 ] || [ "$regression_code" = 128 ]; then
    [ "$rc" = 1 ] || fail "ordinary failure must reject"
    contains "$TMP/regression-code.md" 'regression-proof: failing-after'
  else
    [ "$rc" = 0 ] || fail "inconclusive exit $regression_code must not reject"
    contains "$TMP/regression-code.md" 'regression-proof: inconclusive:'
  fi
  contains "$TMP/regression-code.md" "regression-base-exit: $regression_code"
done
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/regression.patch" --verify true \
  --regression-test 'kill -TERM $$' --report "$TMP/regression-signal.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "signal must be inconclusive"
contains "$TMP/regression-signal.md" 'regression-proof: inconclusive:'
printf 'fixed\n' > "$repo/src/value.txt"
git -C "$repo" diff > "$TMP/no-tests.patch"
git -C "$repo" checkout -q -- src/value.txt
"$ADMIT" --repo "$repo" --patch "$TMP/no-tests.patch" --verify true \
  --regression-test false --report "$TMP/no-tests.md" >/dev/null 2>&1 || fail "missing test hunks must be inconclusive"
contains "$TMP/no-tests.md" 'regression-proof: inconclusive:'
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/regression.patch" --verify true \
  --regression-test '' --report "$TMP/regression-empty.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "empty regression command must be rejected"
# END regression-proof

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

# Separate parent admission retains both verification runs for landing, but
# only while every bound input and its native producer proof is unchanged.
reuse_scenario() {
  local reuse_case="$1" reuse_source="$TMP"
  local TMP="$TMP/reuse-$reuse_case"
  mkdir -p "$TMP"
  cp "$reuse_source/benign.patch" "$reuse_source/benign.md" "$TMP/"
  reuse_repo="$TMP/repo"
  git clone -q --no-hardlinks "$repo" "$reuse_repo"
  cp "$TMP/benign.patch" "$TMP/reuse.patch"
  printf 'AGENTS.md\n' >> "$reuse_repo/.git/info/exclude"
  printf 'local rules\n' > "$reuse_repo/AGENTS.md"
  export OMS_TEST_REUSE_INPUT=original
  reuse_count="$TMP/reuse-count"
  : > "$reuse_count"
  reuse_verify="printf 'run\\n' >> '$reuse_count'; bash scripts/check.sh"
  reuse_flags=(--allow-verifier-change --allow-test-reduction)
  case "$reuse_case" in plan-*)
    "$ROOT/scripts/agent-plan.sh" --repo "$reuse_repo" init --goal reuse >/dev/null
    "$ROOT/scripts/agent-plan.sh" --repo "$reuse_repo" add --id reuse-task --title reuse \
      --allowed tests --verify "$reuse_verify" >/dev/null
    "$ROOT/scripts/agent-plan.sh" --repo "$reuse_repo" claim --id reuse-task --provider codex >/dev/null
    "$ROOT/scripts/agent-plan.sh" --repo "$reuse_repo" review --id reuse-task \
      --artifact "$TMP/benign.md" --patch "$TMP/reuse.patch" >/dev/null
    reuse_flags+=(--plan-task reuse-task)
    ;;
  esac
  "$ADMIT" --repo "$reuse_repo" --patch "$TMP/reuse.patch" --verify "$reuse_verify" \
    "${reuse_flags[@]}" >"$TMP/reuse-admit.out" 2>&1 || { cat "$TMP/reuse-admit.out" >&2; fail "separate admission failed: $reuse_case"; }
  [ "$(wc -l < "$reuse_count" | tr -d " ")" = 2 ] || fail "separate admission skipped a verification run"
  case "$reuse_case" in
    plan-change)
      python3 - "$reuse_repo/.oms/plan/tasks.json" <<'PY_REUSE_PLAN'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
plan = json.loads(path.read_text())
plan['tasks']['reuse-task']['forbidden_paths'] = ['unrelated']
path.write_text(json.dumps(plan))
PY_REUSE_PLAN
      ;;
    command) reuse_verify="$reuse_verify; :" ;;
    ml) reuse_flags+=(--ml) ;;
    policy) reuse_flags+=(--allow-restructure) ;;
    base) git -C "$reuse_repo" -c user.name=test -c user.email=test@example.com commit -q --allow-empty -m moved ;;
    patch) sed 's/Keep the product contract explicit/Keep this product contract explicit/' "$TMP/benign.patch" > "$TMP/reuse.patch" ;;
    environment) export OMS_TEST_REUSE_INPUT=changed ;;
    seed) printf 'changed local rules\n' > "$reuse_repo/AGENTS.md" ;;
    report|index|generation|floor|harness)
      python3 - "$reuse_repo" "$reuse_case" <<'PY_REUSE'
import json, os, pathlib, sys
repo, change = pathlib.Path(sys.argv[1]), sys.argv[2]
row = next(json.loads(line) for line in (repo / '.oms/landings.jsonl').read_text().splitlines()
           if json.loads(line)['event'] == 'capture-reserved')
item = row['managed_capture']
owner_path = pathlib.Path(item['directory']) / 'owner.json'
owner = json.loads(owner_path.read_text())
proof = owner['admission']
if change == 'report':
    with (repo / proof['report']).open('a') as handle:
        handle.write('\nchanged report\n')
elif change == 'index':
    path = repo / '.oms/artifacts/index.jsonl'
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    rows[-1]['verify_exit'] = 9
    path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
elif change == 'generation':
    capture = pathlib.Path(owner['objects']['capture']['path'])
    replacement = capture.with_suffix('.replacement')
    replacement.write_bytes(capture.read_bytes())
    os.replace(replacement, capture)
else:
    if change == 'floor':
        proof['floor_sha256'] = '0' * 64
    else:
        proof['binding']['harness_sha256'] = '0' * 64
    owner_path.write_text(json.dumps(owner))
PY_REUSE
      ;;
  esac
  "$ROOT/scripts/patch-land.sh" --repo "$reuse_repo" --patch "$TMP/reuse.patch" \
    --verify "$reuse_verify" "${reuse_flags[@]}" >"$TMP/reuse-land.out" 2>&1 ||
    { cat "$TMP/reuse-land.out" >&2; fail "landing failed: $reuse_case"; }
  expected_runs=4
  case "$reuse_case" in same|plan-same) expected_runs=2 ;; esac
  [ "$(wc -l < "$reuse_count" | tr -d " ")" = "$expected_runs" ] ||
    fail "$reuse_case admission ran the verifier an unexpected number of times"
  expected_captures=2
  case "$reuse_case" in same|plan-same) expected_captures=1 ;; esac
  [ "$(grep -c capture-reserved "$reuse_repo/.oms/landings.jsonl")" = "$expected_captures" ] ||
    fail "$reuse_case landing borrowed a different capture generation"
  grep -Fq 'Keep' "$reuse_repo/tests/check-helper.sh" || fail "reused admission did not apply the patch"
}

# Reuse binds repository paths and environment bytes: isolate both scratch
# files and shell state, and bound concurrency on smaller CI hosts.
wait_reuse_scenarios() {
  local pid log failed=0
  for pid in $reuse_pids; do
    wait "$pid" || failed=1
  done
  if [ "$failed" != 0 ]; then
    for log in "$TMP"/reuse-*.out; do
      [ -s "$log" ] || continue
      echo "${log##*/}:" >&2
      cat "$log" >&2
    done
    fail "separate admission/landing scenario failed"
  fi
}
reuse_pids=""
reuse_jobs=0
for reuse_case in same plan-same plan-change command ml policy base patch report index generation environment seed floor harness; do
  reuse_scenario "$reuse_case" >"$TMP/reuse-$reuse_case.out" 2>&1 &
  reuse_pids="$reuse_pids $!"
  reuse_jobs=$((reuse_jobs + 1))
  if [ "$reuse_jobs" = 3 ]; then
    wait_reuse_scenarios
    reuse_pids=""
    reuse_jobs=0
  fi
done
wait_reuse_scenarios
unset OMS_TEST_REUSE_INPUT
python3 - "$ROOT/scripts/lib/landing-capture.py" "$TMP" "$repo" <<'PY_REUSE_HARNESS'
import pathlib, runpy, sys
module = runpy.run_path(sys.argv[1])
root = pathlib.Path(sys.argv[2]) / 'binding-harness/scripts/lib'
root.mkdir(parents=True)
for name in ('config', 'rules', 'templates'):
    (root.parent.parent / name).mkdir()
binding = module['admission_binding']
binding.__globals__['HERE'] = root
args = (sys.argv[3], sys.argv[3], 'bash scripts/check.sh', '0', '', '1', '1', '', '0', '0')
before = binding(*args)
helper = root / 'floor.py'
helper.write_text('original\n')
after_add = binding(*args)
helper.write_text('changed\n')
after_edit = binding(*args)
assert len({v['harness_sha256'] for v in (before, after_add, after_edit)}) == 3

# Exercise the public intake dispatch with isolated stand-ins for state writes.
import contextlib, io, os
os.environ.pop('OMS_HARNESS_CHILD', None)
os.environ.pop('OMS_HARNESS_DELEGATE_DEPTH', None)
intake = module['intake']
g = intake.__globals__
original = {name: g[name] for name in ('admission_binding', 'reservations', 'admission_is_current', 'allocate')}
calls = []
item = dict(sha256='expected', directory='old', landing_id='prior')
g['reservations'] = lambda repo: calls.append('reservations') or {'prior': item}
g['admission_is_current'] = lambda repo, candidate, value: calls.append('current') or True
g['allocate'] = lambda *args: calls.append('allocate') or 'fresh.patch'
def dispatch():
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        module['main'](['intake', sys.argv[3], 'source', 'landing', 'expected', sys.argv[3]])
    return output.getvalue().strip()
for error in (OSError, KeyError, ImportError):
    def fail_binding(*args, error=error):
        raise error('binding unavailable')
    g['admission_binding'] = fail_binding
    calls.clear()
    assert dispatch() == 'fresh.patch'
    assert calls == ['allocate'], calls
g['admission_binding'] = lambda *args: {'valid': True}
calls.clear()
assert dispatch() == 'old/prior.patch'
assert calls == ['reservations', 'current'], calls
for error in (RuntimeError, SystemExit, KeyboardInterrupt):
    g['admission_binding'] = lambda *args, error=error: (_ for _ in ()).throw(error('unexpected'))
    calls.clear()
    try:
        dispatch()
    except error:
        pass
    else:
        raise AssertionError('unexpected binding failure suppressed')
    assert not calls
# Allocation failures remain refusals and never fall back to a prior verdict.
g['admission_binding'] = lambda *args: (_ for _ in ()).throw(KeyError('missing task'))
g['allocate'] = lambda *args: (_ for _ in ()).throw(ValueError('allocation refused'))
calls.clear()
try:
    dispatch()
except ValueError as exc:
    assert str(exc) == 'allocation refused'
else:
    raise AssertionError('allocation refusal suppressed')
assert not calls
g.update(original)
PY_REUSE_HARNESS
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/bypass.patch" --verify 'bash scripts/check.sh' \
  --allow-verifier-change --allow-test-reduction >"$TMP/rejected-intake.out" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "separate captured rejection did not preserve its verdict"
python3 - "$repo/.oms/landings.jsonl" <<'PY_REUSE_REJECT'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1])]
assert any(row.get('event') == 'capture-terminal' and row.get('reason') == 'admission-rejected' for row in rows)
assert not any(row.get('event') == 'intent' for row in rows)
PY_REUSE_REJECT

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
contains "$TMP/common-config.md" "owner override: --allow-floor-change REASON"
"$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/hint-parent.md" >/dev/null 2>"$TMP/hint-parent.err" || true
contains "$TMP/hint-parent.err" "--allow-floor-change REASON"
OMS_HARNESS_CHILD=1 "$ADMIT" --repo "$repo" --patch "$TMP/common-config.patch" \
  --verify 'bash scripts/check.sh' --allow-verifier-change \
  --allow-test-reduction --report "$TMP/hint-child.md" >/dev/null 2>"$TMP/hint-child.err" || true
! grep -q -- '--allow-floor-change' "$TMP/hint-child.md" "$TMP/hint-child.err" ||
  fail "worker-facing floor failure must not mention the parent-only override"

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
  'sed -nfquality/oracle.sed src/value.txt | grep -qx good' \
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
  'awk -bfsrc/feature.py' 'gawk -bf./src/feature.py' \
  'mawk -bf src/feature.py' 'nawk -bf./src/feature.py' \
  'sed -nf./src/feature.py' 'jq -eRf./src/feature.py' \
  'awk --file=src/feature.py' 'gawk --file src/feature.py' \
  'sed --file=src/feature.py' 'sed --file src/feature.py' \
  'jq --from-file=src/feature.py' 'jq --from-file src/feature.py' \
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
for entry in "inserted:$indirect_assertion" \
  "appended:$indirect_assertion" 'appended:assert __import__("sys").exit(0)' 'appended:assert exit(0)' \
  'appended:assert eval("True")' 'appended:assert exec("pass")' 'appended:assert os.path.exists("src/value.txt")' \
  'appended:assert sys.exit(0)' 'appended:assert subprocess.call(["true"]) == 0' 'appended:assert quit()' \
  'appended:assert 1 < 2' 'appended:assert 2 > 1' 'appended:assert True & True' 'appended:assert True | False' \
  'appended:assert True; assert True' 'appended:assert True # `call`' 'appended:assert True # &&' \
  'appended:assert True # ('; do
  placement="${entry%%:*}"
  assertion="${entry#*:}"
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

# BEGIN candidate-only-targets
mkdir -p "$TMP/bin"
cat > "$TMP/bin/pytest" <<'EOF'
#!/usr/bin/env bash
set -eu
for path in "$@"; do python3 "$path"; done
EOF
cat > "$TMP/bin/node" <<'EOF'
#!/usr/bin/env bash
set -eu
[ "$1" = --test ]
shift
[ "$#" -gt 0 ]
for path in "$@"; do [ -f "$path" ]; done
EOF
chmod +x "$TMP/bin/pytest" "$TMP/bin/node"
export PATH="$TMP/bin:$PATH"
printf 'assert True\n' > "$repo/tests/test_existing.py"
cat > "$repo/tests/scripts-smoke.sh" <<'EOF'
#!/usr/bin/env bash
# run-smoke-shard.sh
test_unused() { :; }
test_existing() { grep -qx good src/value.txt; }
# SMOKE_TEST_CALLS_BEGIN
"$2"
# SMOKE_TEST_CALLS_END
EOF
cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
case "${1:-fast}" in
  fast) . tests/check-helper.sh; verify_product ;;
  *) exit 2 ;;
esac
EOF
cp "$ROOT/tests/run-smoke-shard.sh" "$repo/tests/run-smoke-shard.sh"
mkdir -p "$repo/tests/operator"
printf 'SECTIONS = [("unit", 0, "cases.py"), ("shell", 0, "cases.sh")]\n' > "$repo/tests/operator/runner.py"
printf '# BEGIN old case\n# END old case\n' > "$repo/tests/operator/cases.py"
printf '# BEGIN old shell case\n# END old shell case\n' > "$repo/tests/operator/cases.sh"
cat > "$repo/tests/operator-tools-smoke.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
[ "$1" = --section ] && [ "$2" = unit ] && [ "$3" = --case ]
grep -Fxq "# BEGIN $4" tests/operator/cases.py
EOF
printf 'console.log("base");\n' > "$repo/tests/app.test.js"
printf 'console.log("base");\n' > "$repo/tests/other.test.js"
git -C "$repo" add tests/test_existing.py tests/scripts-smoke.sh scripts/check.sh tests/operator tests/operator-tools-smoke.sh tests/run-smoke-shard.sh tests/app.test.js tests/other.test.js
git -C "$repo" commit -qm target-fixtures
printf 'assert True\n' > "$repo/tests/test_new.py"
git -C "$repo" add -N tests/test_new.py
python3 - "$ROOT/scripts/lib" "$repo" <<'PYTARGETS'
import sys
sys.path.insert(0, sys.argv[1])
from verify_selectors import candidate_only_targets
repo = sys.argv[2]
changed = {"tests/test_new.py"}
for command in ("pytest tests/test_new.py", "pytest -v tests/test_new.py", "node --test tests/test_new.py"):
    assert candidate_only_targets(repo, command, changed), command
for command in ("pytest", "pytest ''", "pytest --bad tests/test_new.py", "pytest tests/test_new.py tests/test_existing.py",
                "node --test tests/test_new.py tests/test_existing.py", "pytest tests/test_new.py; true",
                "jest tests/*.py", "jest tests/test_new.py", "vitest tests/test_new.py",
                "vitest run tests/test_new.py", "node --test",
                "node --test -v tests/test_new.py", "node --test --version tests/test_new.py",
                "node --test -h tests/test_new.py", "node --test --help tests/test_new.py",
                "node --test -e tests/test_new.py", "node --test -p tests/test_new.py",
                "node --test --check tests/test_new.py", "node --test -q tests/test_new.py"):
    assert not candidate_only_targets(repo, command, changed), command
PYTARGETS
git -C "$repo" diff > "$TMP/target-new.patch"
git -C "$repo" reset -q HEAD -- tests/test_new.py
rm "$repo/tests/test_new.py"
"$ADMIT" --repo "$repo" --patch "$TMP/target-new.patch" --verify 'pytest tests/test_new.py' --report "$TMP/target-new.md" >/dev/null 2>&1 || fail "new pytest target should be admissible"
contains "$TMP/target-new.md" 'candidate-only'
contains "$TMP/target-new.md" 'tests/test_new.py'
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/target-new.patch" --verify 'pytest tests/test_new.py tests/test_existing.py' --report "$TMP/target-mixed.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "mixed pytest targets must retain their floor"
contains "$TMP/target-mixed.md" 'verify-floor: FAIL'
printf 'console.log("new");\n' > "$repo/tests/new.test.js"
git -C "$repo" add -N tests/new.test.js
git -C "$repo" diff > "$TMP/target-node-new.patch"
git -C "$repo" reset -q HEAD -- tests/new.test.js
rm "$repo/tests/new.test.js"
"$ADMIT" --repo "$repo" --patch "$TMP/target-node-new.patch" --verify 'node --test tests/new.test.js' --report "$TMP/target-node-new.md" >/dev/null 2>&1 || fail "new node test should be admissible"
contains "$TMP/target-node-new.md" 'candidate-only: PASS'
contains "$TMP/target-node-new.md" 'verify: PASS'
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/target-node-new.patch" --verify 'node --test tests/new.test.js tests/app.test.js' --report "$TMP/target-node-mixed.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "mixed node tests must retain their floor"
contains "$TMP/target-node-mixed.md" 'verify-floor: FAIL'
awk '/^# SMOKE_TEST_CALLS_BEGIN$/ { print "test_new() { :; }" } { print }' "$repo/tests/scripts-smoke.sh" > "$TMP/selector-suite"
cat "$TMP/selector-suite" > "$repo/tests/scripts-smoke.sh"
git -C "$repo" diff > "$TMP/target-selector.patch"
git -C "$repo" checkout -q -- tests/scripts-smoke.sh
"$ADMIT" --repo "$repo" --patch "$TMP/target-selector.patch" --verify 'bash tests/scripts-smoke.sh --only test_new' --allow-verifier-change --report "$TMP/target-selector.md" >/dev/null 2>&1 || fail "new smoke selector should be admissible"
contains "$TMP/target-selector.md" 'candidate-only'
contains "$TMP/target-selector.md" 'test_new'
printf 'test_late() { return 37; }\n' >> "$repo/tests/scripts-smoke.sh"
python3 - "$ROOT/scripts/lib" "$repo" <<'PYLATE'
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from verify_selectors import candidate_only_targets
repo = sys.argv[2]
path = Path(repo) / "tests/scripts-smoke.sh"
source = path.read_text()
begin = "# SMOKE_TEST_CALLS_BEGIN"
end = "# SMOKE_TEST_CALLS_END"
for suite in ("tests/scripts-smoke.sh", "tests/run-smoke-shard.sh"):
    command = "bash %s --only test_late" % suite
    assert not candidate_only_targets(repo, command, {"tests/scripts-smoke.sh"}), command
    leading = source.replace("test_late() { return 37; }\n", "").replace(begin, "test_late() { return 37; }\n" + begin)
    path.write_text(leading)
    assert candidate_only_targets(repo, command, {"tests/scripts-smoke.sh"}), command
    for broken in (leading.replace(begin, ""), leading.replace(end, ""),
                   leading.replace(begin, begin + "\n" + begin),
                   leading.replace(begin, "TEMP_MARKER").replace(end, begin).replace("TEMP_MARKER", end),
                   leading.replace("test_late() {", "test_late(){"),
                   *(source.replace(begin, "# x " + separator + "test_late() {\n" + begin)
                     for separator in ("\x0c", "\x0b", "\r", "\x1c", "\x1d", "\x1e", "\x85", "\u2028", "\u2029"))):
        path.write_text(broken)
        assert not candidate_only_targets(repo, command, {"tests/scripts-smoke.sh"}), command
    path.write_text(source)
PYLATE
python3 - "$repo/tests/scripts-smoke.sh" <<'PYFORMFEED'
import sys
from pathlib import Path
path = Path(sys.argv[1])
source = path.read_text()
path.write_text(source.replace("# SMOKE_TEST_CALLS_BEGIN", "# x \x0ctest_late() {\n# SMOKE_TEST_CALLS_BEGIN"))
PYFORMFEED
git -C "$repo" diff > "$TMP/target-late.patch"
git -C "$repo" checkout -q -- tests/scripts-smoke.sh
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/target-late.patch" --verify 'bash tests/run-smoke-shard.sh --only test_late' --report "$TMP/target-late.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "uninvoked late smoke definition must retain the floor"
contains "$TMP/target-late.md" 'verify: PASS'
contains "$TMP/target-late.md" 'verify-floor: FAIL'

printf '# BEGIN new case\n# END new case\n' >> "$repo/tests/operator/cases.py"
python3 - "$ROOT/scripts/lib" "$repo" <<'PYCASE'
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from verify_selectors import candidate_only_targets
repo = sys.argv[2]
changed = {"tests/operator/cases.py"}
command = "bash tests/operator-tools-smoke.sh --section unit --case 'new case'"
assert candidate_only_targets(repo, command, changed)
for command in ("bash tests/operator-tools-smoke.sh --section unit --case 'old case'",
                "bash tests/operator-tools-smoke.sh --section missing --case 'new case'",
                "bash tests/operator-tools-smoke.sh --section unit --case ''"):
    assert not candidate_only_targets(repo, command, changed), command
shell_case = Path(repo) / "tests/operator/cases.sh"
original = shell_case.read_text(encoding="utf-8")
try:
    shell_case.write_text(original + "# BEGIN new shell case\n# END new shell case\n", encoding="utf-8")
    command = "bash tests/operator-tools-smoke.sh --section shell --case 'new shell case'"
    assert not candidate_only_targets(repo, command, {"tests/operator/cases.sh"}), command
finally:
    shell_case.write_text(original, encoding="utf-8")
PYCASE
git -C "$repo" diff > "$TMP/target-operator.patch"
git -C "$repo" checkout -q -- tests/operator/cases.py
"$ADMIT" --repo "$repo" --patch "$TMP/target-operator.patch" --verify "bash tests/operator-tools-smoke.sh --section unit --case 'new case'" --report "$TMP/target-operator.md" >/dev/null 2>&1 || fail "new operator case should be admissible"
contains "$TMP/target-operator.md" 'candidate-only: PASS'
contains "$TMP/target-operator.md" 'verify: PASS'
contains "$TMP/target-operator.md" 'verify-floor: PASS'
for node_file in app other; do
  printf 'console.log("changed");\n' > "$repo/tests/$node_file.test.js"
  git -C "$repo" diff > "$TMP/target-node.patch"
  git -C "$repo" checkout -q -- "tests/$node_file.test.js"
  for node_verify in "node --test tests/$node_file.test.js" 'node --test tests/app.test.js tests/other.test.js'; do
    rc=0
    "$ADMIT" --repo "$repo" --patch "$TMP/target-node.patch" --verify "$node_verify" --report "$TMP/target-node.md" >/dev/null 2>&1 || rc=$?
    [ "$rc" = 1 ] || fail "existing node test must require verifier consent: $node_file"
    contains "$TMP/target-node.md" 'verifier: FAIL'
    contains "$TMP/target-node.md" '--allow-verifier-change'
  done
done
printf 'bad\n' > "$repo/src/value.txt"
git -C "$repo" diff > "$TMP/target-existing.patch"
git -C "$repo" checkout -q -- src/value.txt
rc=0
"$ADMIT" --repo "$repo" --patch "$TMP/target-existing.patch" --verify 'bash tests/scripts-smoke.sh --only test_existing' --report "$TMP/target-existing.md" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "existing smoke selector must still reject broken product"
# END candidate-only-targets

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

for reader in jq sha256sum md5sum shasum cksum comm join paste nl tac rev xxd hexdump base64 \
  sha1sum sha224sum sha384sum sha512sum b2sum sum base32 basenc; do
  out="$(floor_proposal "$reader tests/suite.sh")"
  printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
    fail "$reader reading a modified file must be rejected: $out"
done
for verify in \
  "bash -c 'grep -q endswith tests/suite.sh && bash tests/suite.sh'" \
  "python3 - < tests/suite.sh" \
  "/usr/bin/jq tests/suite.sh" \
  "jq ./tests/suite.sh" \
  "python3 - <./tests/suite.sh" \
  "CHECK_MODE=strict sha256sum tests/suite.sh" \
  "bash -c 'base64 tests/suite.sh'" \
  "command jq tests/suite.sh" \
  "command -p jq tests/suite.sh" \
  "command -- jq tests/suite.sh" \
  "env jq tests/suite.sh" \
  "env CHECK_MODE=strict jq tests/suite.sh" \
  "env -- CHECK_MODE=strict jq tests/suite.sh" \
  "/usr/bin/env jq tests/suite.sh"; do
  out="$(floor_proposal "$verify")"
  printf '%s' "$out" | grep >/dev/null -F 'floor_incompatible_verifier' ||
    fail "floor-incompatible verify must be rejected ($verify): $out"
done

for verify in \
  "bash tests/suite.sh" \
  "command -v jq tests/suite.sh" \
  "command -V jq tests/suite.sh" \
  "command bash tests/suite.sh" \
  "env bash tests/suite.sh" \
  "./env jq tests/suite.sh" \
  "/tmp/command jq tests/suite.sh" \
  "jq 'tests/*.sh'" \
  "grep -q ok docs/other.md && bash tests/suite.sh"; do
  out="$(floor_proposal "$verify")"
  printf '%s' "$out" | grep >/dev/null -F 'proposal applied' ||
    fail "admissible verify must be accepted ($verify): $out"
done

# Informational land hint: affected suites absent from --verify are named on
# stderr; a verify that already names them stays silent. Verdict is unchanged.
hint_repo="$TMP/hint-repo"
mkdir -p "$hint_repo/scripts" "$hint_repo/tests"
git -C "$hint_repo" init -q -b main
git -C "$hint_repo" config user.email test@example.com
git -C "$hint_repo" config user.name test
printf '#!/usr/bin/env bash\necho one\n' > "$hint_repo/scripts/widget.sh"
printf '#!/usr/bin/env bash\nbash scripts/widget.sh\n' > "$hint_repo/tests/widget-smoke.sh"
printf '#!/usr/bin/env bash\ntrue\n' > "$hint_repo/tests/other-smoke.sh"
git -C "$hint_repo" add -A
git -C "$hint_repo" commit -qm init
printf '#!/usr/bin/env bash\necho two\n' > "$hint_repo/scripts/widget.sh"
git -C "$hint_repo" diff > "$TMP/hint.patch"
git -C "$hint_repo" checkout -q -- scripts/widget.sh
"$ADMIT" --repo "$hint_repo" --patch "$TMP/hint.patch" --verify 'bash tests/other-smoke.sh' \
  --report "$TMP/hint-a.md" >/dev/null 2>"$TMP/hint-a.err" || { cat "$TMP/hint-a.err" >&2; fail "hint case should admit"; }
contains "$TMP/hint-a.err" "note: land will also run 1 affected suite(s) not in --verify: tests/widget-smoke.sh"
contains "$TMP/hint-a.md" "Patch admission: ADMIT"
"$ADMIT" --repo "$hint_repo" --patch "$TMP/hint.patch" --verify 'bash tests/widget-smoke.sh' \
  --report "$TMP/hint-b.md" >/dev/null 2>"$TMP/hint-b.err" || { cat "$TMP/hint-b.err" >&2; fail "hint case b should admit"; }
! grep -Fq "land will also run" "$TMP/hint-b.err" || fail "no note when --verify names the consumer"

# Managed capture quota refusal in patch-admit is a single-line state refusal (exit 1)
# naming count/byte usage against limits and the remedy, with no usage hint.
"$ADMIT" --repo "$hint_repo" --patch "$TMP/hint.patch" --verify 'bash tests/widget-smoke.sh' \
  >/dev/null 2>"$TMP/quota-seed.err" || { cat "$TMP/quota-seed.err" >&2; fail "quota seed admission should succeed"; }
patch_bytes="$(wc -c < "$TMP/hint.patch" | tr -d ' \r')"
charged_bytes=$((2 * patch_bytes))

quota_rc=0
OMS_LANDING_CAPTURE_COUNT=1 "$ROOT/scripts/oms" patch-admit --repo "$hint_repo" \
  --patch "$TMP/hint.patch" --verify 'bash tests/widget-smoke.sh' \
  >"$TMP/quota-count.out" 2>"$TMP/quota-count.err" || quota_rc=$?
[ "$quota_rc" -eq 1 ] || fail "count quota refusal should exit 1, got $quota_rc: $(cat "$TMP/quota-count.err")"
[ ! -s "$TMP/quota-count.out" ] || fail "quota refusal should not write to stdout"
[ "$(wc -l < "$TMP/quota-count.err" | tr -d ' \r')" = 1 ] ||
  fail "quota refusal must be a single line on stderr: $(cat "$TMP/quota-count.err")"
contains "$TMP/quota-count.err" "managed landing capture quota exhausted (count 1/1, bytes ${charged_bytes}/134217728, need ${charged_bytes} bytes)"
contains "$TMP/quota-count.err" "oms gc"
contains "$TMP/quota-count.err" "OMS_LANDING_CAPTURE_COUNT"
contains "$TMP/quota-count.err" "OMS_LANDING_CAPTURE_BYTES"
! grep -Fq "hint:" "$TMP/quota-count.err" || fail "quota refusal must not print usage hint"
! grep -Fq "cannot capture admission input" "$TMP/quota-count.err" || fail "quota refusal must not print generic wrapper error"

for byte_limit in "$charged_bytes" "$((2 * charged_bytes - 1))"; do
  quota_rc=0
  OMS_LANDING_CAPTURE_BYTES="$byte_limit" "$ROOT/scripts/oms" patch-admit --repo "$hint_repo" \
    --patch "$TMP/hint.patch" --verify 'bash tests/widget-smoke.sh' \
    >"$TMP/quota-bytes.out" 2>"$TMP/quota-bytes.err" || quota_rc=$?
  [ "$quota_rc" -eq 1 ] || fail "byte quota refusal ($byte_limit) should exit 1, got $quota_rc: $(cat "$TMP/quota-bytes.err")"
  [ "$(wc -l < "$TMP/quota-bytes.err" | tr -d ' \r')" = 1 ] ||
    fail "byte quota refusal must be one line: $(cat "$TMP/quota-bytes.err")"
  contains "$TMP/quota-bytes.err" "managed landing capture quota exhausted (count 1/256, bytes ${charged_bytes}/${byte_limit}, need ${charged_bytes} bytes)"
  contains "$TMP/quota-bytes.err" "oms gc"
  ! grep -Fq "hint:" "$TMP/quota-bytes.err" || fail "byte quota refusal must not print usage hint"
done

for bad_limit in "" 0 -1 bad; do
  bad_rc=0
  OMS_LANDING_CAPTURE_COUNT="$bad_limit" "$ADMIT" --repo "$hint_repo" \
    --patch "$TMP/hint.patch" --verify 'bash tests/widget-smoke.sh' \
    >/dev/null 2>"$TMP/quota-bad.err" || bad_rc=$?
  [ "$bad_rc" -eq 2 ] || fail "malformed quota limit '$bad_limit' should exit 2, got $bad_rc"
done

echo "patch-admit-verifier-floor-smoke: ok"
