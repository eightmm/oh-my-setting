#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"

fail() {
  echo "source-distribution-smoke: $*" >&2
  exit 1
}

case "${1:-}" in
  ''|--docs-only) ;;
  *) fail "usage: source-distribution-smoke.sh [--docs-only]" ;;
esac

for path in \
  .github/workflows/release.yml \
  .github/workflows/agent-snapshot.yml \
  docs/RELEASE.md \
  scripts/gen-checksums.sh \
  tests/release-contract-smoke.sh; do
  [ ! -e "$ROOT/$path" ] || fail "obsolete GitHub Release surface remains: $path"
done

# Naming the notes froze this guard at 0.5: the 0.6 and 0.7 notes were added
# afterwards and never reached it. Enumerate whatever exists instead, so a
# release cannot add a note that goes unscanned.
migration_notes=()
for note in "$ROOT"/docs/MIGRATION-*.md; do
  [ -e "$note" ] || continue
  migration_notes+=("docs/${note##*/}")
done
[ "${#migration_notes[@]}" -gt 0 ] ||
  fail "no migration note found to scan for Release-surface references"

for file in README.md README.ko.md docs/COMPONENTS.md "${migration_notes[@]}" \
    .github/workflows/test.yml scripts/check.sh; do
  # This guard exists for THIS repository's retired GitHub-Release surface;
  # a third-party tool pinned from its own releases page (CI's shellcheck
  # binary) is distribution hygiene, not a Release-surface regression, so
  # the download pattern is scoped to this repo's slug.
  if grep -Eiq 'docs/RELEASE|oh-my-setting/releases/(latest|download)|releases/latest|release-contract-smoke|gen-checksums|tag-driven release|tag 기반 릴리스' "$ROOT/$file"; then
    fail "obsolete Release reference remains: $file"
  fi
done

grep -Fq 'raw.githubusercontent.com/eightmm/oh-my-setting/main/install.sh' "$ROOT/README.md" ||
  fail "README must retain the main source installer"
grep -Fq 'INSTALLER_DEFAULT_REF="edge"' "$ROOT/install.sh" ||
  fail "source installer must retain the edge channel default"

if [ "${1:-}" = --docs-only ]; then
  echo "source-distribution-smoke: ok (documentation references only)"
  exit 0
fi

# The docs route must not start runtime probes, providers or downloads.
(
  bash() { return 97; }
  python3() { return 97; }
  curl() { return 97; }
  gh() { return 97; }
  export -f bash python3 curl gh
  "$BASH" "$ROOT/tests/source-distribution-smoke.sh" --docs-only >/dev/null
) || fail "documentation checks depend on executable probes"

workflow="$ROOT/.github/workflows/test.yml"
# Keep the role identifiers and authority requirements in a bounded contract
# probe; integration fixtures need not pin editorial wording or punctuation.
python3 - "$ROOT" <<'PY' || fail "panel role contract lost identifiers or authority limits"
import pathlib, re, runpy, sys
hooks = runpy.run_path(str(pathlib.Path(sys.argv[1]) / 'scripts/lib/hook_state.py'))
tower = hooks['panel_main_base']({'agent': 'codex'}, 'fixture-room')
for role in ('security-auditor', 'verifier', 'integrator', 'performance-engineer',
             'documenter', 'release-engineer', 'advisor', 'architect', 'critic'):
    assert role in tower.lower(), role
for requirement in (r'publication.*authorized', r'Never commit.*shared checkout',
                    r'independent', r'patch admission', r'oms room scope',
                    r'role.{0,60}instance', r'sole panel main'):
    assert re.search(requirement, tower, re.I), requirement
PY
for host in ubuntu-latest macos-latest windows-latest; do
  grep -Fq "\"os\":\"$host\"" "$workflow" ||
    fail "install lifecycle matrix must cover $host"
done

# The focused CI lanes are a positional partition of the stage list. If the
# lane count here and in the workflow drift, or the partition ever drops or
# doubles a stage, a registered suite silently stops running in CI — the
# exact failure mode stage registration exists to prevent.
grep -Fq -- '--focused-lane "${{ matrix.lane }}/4"' "$workflow" ||
  fail "focused CI job must run the 4-lane partition"
grep -Fq 'lane: [1, 2, 3, 4]' "$workflow" ||
  fail "focused CI matrix must declare lanes 1..4"
full_stages="$(bash "$ROOT/scripts/check.sh" --focused-only --list-stages | sort)"
[ -n "$full_stages" ] || fail "focused stage listing is empty"
lane_union="$( (bash "$ROOT/scripts/check.sh" --focused-only --focused-lane 1/4 --list-stages
  bash "$ROOT/scripts/check.sh" --focused-only --focused-lane 2/4 --list-stages
  bash "$ROOT/scripts/check.sh" --focused-only --focused-lane 3/4 --list-stages
  bash "$ROOT/scripts/check.sh" --focused-only --focused-lane 4/4 --list-stages) | sort)"
[ "$full_stages" = "$lane_union" ] ||
  fail "focused lanes must partition the stage list exactly (no drop, no double)"
printf '%s\n' "$full_stages" | grep >/dev/null -Fx install-lifecycle ||
  fail "Linux auto lifecycle must remain in the focused gate"
# Exercise the local orchestrator without recursively running the full gate.
(
  unset OMS_CHECK_JOBS
  probe="$(mktemp -d "${TMPDIR:-/tmp}/oms-parallel-probe.XXXXXX")"
  trap 'rm -rf "$probe"' EXIT
  export OMS_PARALLEL_PROBE="$probe"
  OMS_PARALLEL_QUIT="$(python3 -c 'import signal; print(int(signal.getsignal(signal.SIGQUIT) == signal.SIG_IGN))')"
  export OMS_PARALLEL_QUIT
  cat > "$probe/check.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
if [ -n "${OMS_PARALLEL_BUDGET_PROBE:-}" ]; then
  [ "$OMS_LINT_JOBS_MAX:$OMS_SMOKE_JOBS:$OMS_OPERATOR_JOBS" = 1:1:1 ] || exit 12
  python3 - <<'PY'
import os, time
with open(os.environ['OMS_PARALLEL_BUDGET_PROBE'], 'a') as log:
    log.write('%s start\n' % time.monotonic())
time.sleep(.1)
with open(os.environ['OMS_PARALLEL_BUDGET_PROBE'], 'a') as log:
    log.write('%s end\n' % time.monotonic())
PY
fi
python3 - <<'PY'
import os, signal
assert signal.getsignal(signal.SIGINT) != signal.SIG_IGN
assert int(signal.getsignal(signal.SIGQUIT) == signal.SIG_IGN) == int(os.environ["OMS_PARALLEL_QUIT"])
PY
if [ "$1" = --shard ]; then key="operator-${2%/*}"
else key="${1#--}${3:+-${3%/*}}"; fi
touch "$OMS_PARALLEL_PROBE/$key.ran"
if [ "${OMS_PARALLEL_CANCEL:-0}" = 1 ]; then
  printf '%s\n' "$$" > "$OMS_PARALLEL_PROBE/$key.pid"
  case "$key" in operator-2|focused-only-1)
    touch "$OMS_PARALLEL_PROBE/$key.ready"; exit 0 ;; esac
  trap 'wait "$child" 2>/dev/null || :; exit 143' TERM
  sleep 60 &
  child=$!
  printf '%s\n' "$child" > "$OMS_PARALLEL_PROBE/$key.child"
  touch "$OMS_PARALLEL_PROBE/$key.ready"
  wait "$child"
fi
if [ "${OMS_PARALLEL_FAIL:-0}" = 1 ] && [ "$key" = focused-only-2 ]; then exit 11; fi
if [ "${OMS_PARALLEL_SLOW:-0}" = 1 ]; then
  case "$key" in
    focused-only-1)
      # Hold the first slot until the third lane starts, bounded so a
      # scheduler that reaps in launch order still terminates.
      for _ in $(seq 1 300); do
        [ -e "$OMS_PARALLEL_PROBE/third.started" ] && break
        sleep 0.1
      done
      touch "$OMS_PARALLEL_PROBE/first.done" ;;
    focused-only-3)
      [ -e "$OMS_PARALLEL_PROBE/first.done" ] || touch "$OMS_PARALLEL_PROBE/third.early"
      touch "$OMS_PARALLEL_PROBE/third.started" ;;
  esac
fi
EOF
  # shellcheck source=scripts/lib/check-parallel.sh
  . "$ROOT/scripts/lib/check-parallel.sh"
  oms_check_parallel "$probe/check.sh" "$probe/logs" 0 > "$probe/pass.log" 2>&1 ||
    fail "parallel gate failed or ignored signals: $(cat "$probe/pass.log")"
  [ "$(find "$probe" -name '*.ran' | wc -l | tr -d ' ')" = 10 ] || fail "parallel gate omitted a partition"
  OMS_CHECK_JOBS=2 OMS_PARALLEL_BUDGET_PROBE="$probe/budget" \
    oms_check_parallel "$probe/check.sh" "$probe/budget-logs" 0 > "$probe/budget.log" 2>&1 ||
    fail "constrained gate failed: $(cat "$probe/budget.log")"
  python3 - "$probe/budget" <<'PY' || fail "constrained gate exceeded or failed to use its shared budget"
import pathlib, sys
events = sorted((float(t), kind) for t, kind in
                (line.split() for line in pathlib.Path(sys.argv[1]).read_text().splitlines()))
active = peak = 0
for _, kind in events:
    active += 1 if kind == 'start' else -1
    peak = max(peak, active)
assert len(events) == 20 and active == 0 and 1 <= peak <= 2, (events, peak)
print("parallel-budget: peak=%s, limit=2, partitions=10" % peak)
PY
  rm -f "$probe"/*.ran
  OMS_CHECK_JOBS=1 oms_check_parallel "$probe/check.sh" "$probe/no-lint-logs" 1 > "$probe/no-lint.log" 2>&1 ||
    fail "serial constrained gate failed without lint: $(cat "$probe/no-lint.log")"
  [ "$(find "$probe" -name '*.ran' | wc -l | tr -d ' ')" = 9 ] && [ ! -e "$probe/lint-only.ran" ] ||
    fail "constrained no-lint gate changed partition coverage"
  # With 2 slots the slow first lane must not hold the slot the fast second
  # lane freed: the third lane has to start before the first lane finishes.
  rm -f "$probe/first.done" "$probe/third.started" "$probe/third.early"
  OMS_CHECK_JOBS=2 OMS_PARALLEL_SLOW=1 oms_check_parallel "$probe/check.sh" "$probe/slow-logs" 1 > "$probe/slow.log" 2>&1 ||
    fail "slow-lane gate failed: $(cat "$probe/slow.log")"
  [ -e "$probe/third.early" ] || fail "parallel gate idled a free slot behind a slow lane"
  if OMS_PARALLEL_FAIL=1 oms_check_parallel "$probe/check.sh" "$probe/fail-logs" 0 > "$probe/fail.log" 2>&1; then
    fail "parallel gate swallowed a failing lane"
  fi
  cat > "$probe/cancel.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
trap 'exit 143' TERM
. "$1/scripts/lib/check-parallel.sh"
kill() {
  printf '%s\n' "$*" >> "$OMS_PARALLEL_PROBE/signals"
  # Record the regression without forwarding a stale negative PID.
  [ "$*" != "-TERM -- -$(cat "$OMS_PARALLEL_PROBE/$OMS_PARALLEL_FAST.pid")" ] || return 0
  builtin kill "$@"
}
if [ "$2" = operator ]; then
  oms_operator_parallel "$OMS_PARALLEL_PROBE/check.sh" 3
else
  oms_check_parallel "$OMS_PARALLEL_PROBE/check.sh" "$OMS_PARALLEL_PROBE/logs" 0
fi
EOF
  python3 - "$ROOT" "$probe" <<'PY' || fail "parallel cancellation reached a finished group or missed live descendants"
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

root, probe = map(Path, sys.argv[1:])
if os.name != 'posix':
    print('SKIP: cancellation probe requires POSIX process groups')
    sys.exit(0)
def alive(pid):
    # Linux zombies have terminated but may await their adopting reaper.
    stat = Path('/proc') / str(pid) / 'stat'
    try:
        if stat.read_text().split(') ', 1)[1].startswith('Z '):
            return False
    except FileNotFoundError:
        pass
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True

for mode, fast, count in (('operator', 'operator-2', 3), ('check', 'focused-only-1', 10)):
    fixture = probe / mode
    fixture.mkdir()
    for name in ('check.sh', 'cancel.sh'):
        (fixture / name).write_bytes((probe / name).read_bytes())
    env = dict(os.environ, OMS_PARALLEL_PROBE=str(fixture), OMS_PARALLEL_CANCEL='1', OMS_PARALLEL_FAST=fast)
    with (fixture / 'cancel.log').open('w') as output:
        process = subprocess.Popen(['bash', str(fixture / 'cancel.sh'), str(root), mode],
                                   env=env, stdout=output, stderr=output, start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while True:
                pids = list(fixture.glob('*.pid'))
                children = list(fixture.glob('*.child'))
                fast_path = fixture / (fast + '.pid')
                if (len(list(fixture.glob('*.ready'))) == count and len(pids) == count
                        and len(children) == count - 1 and fast_path.exists()
                        and not alive(int(fast_path.read_text()))):
                    break
                assert process.poll() is None and time.monotonic() < deadline, (mode, output.name)
                time.sleep(.02)
            exited = int(fast_path.read_text())
            descendants = [int(path.read_text()) for path in children]
            assert all(alive(pid) for pid in descendants), mode
            os.killpg(process.pid, signal.SIGTERM)
            assert process.wait(timeout=5) == 143, (mode, (fixture / 'cancel.log').read_text())
            signals = (fixture / 'signals').read_text().splitlines()
            assert '-TERM -- -%s' % exited not in signals, (mode, exited, signals)
            expected = {'-TERM -- -' + path.read_text().strip() for path in pids if path != fast_path}
            assert set(signals) == expected, (mode, signals, expected)
            deadline = time.monotonic() + 5
            while any(alive(pid) for pid in descendants) and time.monotonic() < deadline:
                time.sleep(.02)
            assert not any(alive(pid) for pid in descendants), (mode, descendants)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
PY
) || exit 1
# Copy mode is the Windows ownership contract. Proving it only on the Windows
# runner means the slowest leg in the matrix is the first to report a broken
# marker or a lost backup, so a Linux leg forces the same path early.
grep -Fq '"link_mode":"copy"' "$workflow" ||
  fail "install lifecycle matrix must exercise copy mode outside Windows"
# The macOS job is the only stock Bash 3.2 parser and the only BSD userland in
# CI. Both catch a class nothing else does, and both have already shipped
# breakage, so neither may quietly disappear again.
grep -Fq 'bsd-portability-smoke.sh' "$workflow" ||
  fail "macOS portability job must run the BSD userland fixtures"
grep -Fq 'OMS_BASH32_BIN=/bin/bash' "$workflow" ||
  fail "macOS portability job must parse with the stock Bash 3.2"
# check.sh exits at the first failing stage, so lint, focused suites, and each
# large smoke shard are independent jobs. That preserves failure evidence and
# lets GitHub run the four expensive shards on separate runners.
grep -Fq -- '--lint-only' "$workflow" ||
  fail "lint must run as its own job"
grep -Fq 'focused:' "$workflow" ||
  fail "focused suites must run as their own job"
grep -Fq -- '--focused-only' "$workflow" ||
  fail "the focused job must use the focused-only gate mode"
grep -Fq 'smoke_shard:' "$workflow" ||
  fail "scripts-smoke must use a native matrix job"
grep -Fq 'shard: [1, 2, 3, 4]' "$workflow" ||
  fail "scripts-smoke matrix must retain four deterministic shards"
grep -Fq -- '--scripts-smoke-only' "$workflow" ||
  fail "each matrix child must run only its assigned scripts-smoke shard"
grep -Fq 'OMS_SMOKE_TIMINGS: "1"' "$workflow" ||
  fail "CI smoke shards must emit bounded timing evidence"
# oms land installs on a CI run whose jobs show the full selection; ci-query.py
# reads that from these exact names, so a rename must fail here, not silently
# make every run look partial.
for job_name in 'name: focused (${{ matrix.lane }}/4)' 'name: scripts-smoke (${{ matrix.shard }}/4)' \
    'name: native (${{ matrix.os }}, ${{ matrix.link_mode }})' '"os":"ubuntu-latest","link_mode":"copy"'; do
  grep -Fq -- "$job_name" "$workflow" || fail "ci-query.py full-selection evidence needs: $job_name"
done
for job_name in '"focused (%d/4)"' '"scripts-smoke (%d/4)"' '"native (ubuntu-latest, copy)"' '"lint"' '"gate"' '"affected_plan"'; do
  grep -Fq -- "$job_name" "$ROOT/scripts/lib/ci-query.py" || fail "ci-query.py no longer reads $job_name"
done
# Pull requests first project the graph mode. Positive evidence runs one narrow
# job; a fail-open result reuses the existing focused and smoke matrices rather
# than serializing the complete gate on one runner.
grep -Fq 'affected_plan:' "$workflow" || fail "workflow must expose an affected-plan job"
if grep -q '^  affected:' "$workflow"; then fail "affected selection must not run twice"; fi
grep -Fq -- '--affected --changed-from "$DIFF_BASE"' "$workflow" ||
  fail "the affected job must include changed-file lint and use the planned base"
grep -Fq -- '--ci-output "$GITHUB_OUTPUT"' "$workflow" ||
  fail "the plan job must execute narrow checks or defer full checks in one pass"
grep -Fq "needs.affected_plan.outputs.mode == 'full'" "$workflow" ||
  fail "full fallback must reuse the parallel focused and smoke matrices"
grep -Fq 'github.event.pull_request.base.sha || github.event.before' "$workflow" ||
  fail "PRs and pushes must both select the changed range"
grep -Fq 'workflow_dispatch:' "$workflow" || fail "manual full verification is missing"
grep -Fq 'cron:' "$workflow" || fail "periodic full verification is missing"

# The public floor is Python 3.9, so syntax and the parser-less Codex HUD path
# need a real 3.9 interpreter in CI rather than only a modern-parser promise.
grep -Fq 'python39:' "$workflow" || fail "Python 3.9 compatibility job must exist"
grep -Fq 'python-version: "3.9"' "$workflow" || fail "CI must install Python 3.9 explicitly"
grep -Fq 'codex-hud-config-smoke.sh' "$workflow" ||
  fail "Python 3.9 job must exercise the Codex HUD fallback"

# Matrix child names are unstable branch-protection targets. One fixed gate
# depends on every required job and fails closed even when a dependency is
# cancelled or skipped.
grep -Fq 'gate:' "$workflow" || fail "workflow must expose one stable gate job"
grep -Fq 'if: always()' "$workflow" || fail "gate must evaluate failed and cancelled needs"
# The requirement is semantic, not textual: every job the workflow defines is a
# verification job, so the gate must depend on all of them and read each one's
# result. Matching one rendered `needs:` line instead made the contract hostage
# to YAML formatting and to the job list of the day — adding a job passed the
# test while leaving the gate blind to it.
python3 - "$workflow" <<'PY' || fail "gate must depend on every verification job and inspect each result"
import pathlib
import re
import sys

lines = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
workflow_text = "\n".join(lines)
concurrency = workflow_text.split("\nconcurrency:\n", 1)[1].split("\njobs:", 1)[0]
group = re.search(r"^  group: (.+)$", concurrency, re.M).group(1)
def concurrency_key(event, ref):
    values = {"workflow": "test", "event_name": event, "ref": ref}
    result = re.sub(r"\$\{\{\s*github\.(\w+)\s*\}\}", lambda match: values[match.group(1)], group)
    assert "${{" not in result, "unhandled concurrency expression"
    return result.lower()
keys = {concurrency_key(event, "refs/heads/main")
        for event in ("push", "schedule", "workflow_dispatch", "pull_request")}
assert len(keys) == 4, "scheduled/manual checks must not cancel a landing's push CI"
assert concurrency_key("push", "refs/heads/main") != concurrency_key("push", "refs/heads/feature")
assert "  cancel-in-progress: true" in concurrency, "new pushes should still replace obsolete push runs"
try:
    start = next(i for i, line in enumerate(lines) if line.rstrip() == "jobs:")
except StopIteration:
    raise SystemExit("workflow defines no jobs block")

jobs = []
gate_body = []
current = None
for line in lines[start + 1:]:
    if line.strip() and not line.startswith(" "):
        break
    header = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
    if header:
        current = header.group(1)
        jobs.append(current)
        continue
    if current == "gate":
        gate_body.append(line)

# Every executable step has an explicit outer bound; the heavy lanes retain
# headroom above the recorded 1,179s operator stage and 35m affected timeout.
for step in workflow_text.split('      - name: ')[1:]:
    step = step.split('      - ', 1)[0].split('\n  #', 1)[0]
    if re.search(r'^        run:', step, re.M):
        limit = re.search(r'^        timeout-minutes: ([0-9]+)$', step, re.M)
        assert limit and int(limit.group(1)) > 0, step.splitlines()[0]

# Each job also has its own bound: action steps (checkout, setup) carry none, and
# an unbounded job runs to GitHub's 360-minute default. The bound covers the
# job's step limits so it never cuts a healthy run short.
for name in jobs:
    job_body = workflow_text.split("\n  %s:\n" % name, 1)[1].split("\n  #", 1)[0]
    job_body = re.split(r"^  [a-z0-9_]+:$", job_body, maxsplit=1, flags=re.M)[0]
    limit = re.search(r"^    timeout-minutes: ([0-9]+)$", job_body, re.M)
    steps = sum(int(v) for v in re.findall(r"^        timeout-minutes: ([0-9]+)$", job_body, re.M))
    assert limit and int(limit.group(1)) >= steps, (name, steps)

if "gate" not in jobs:
    raise SystemExit("workflow has no gate job")
required = set(jobs) - {"gate"}
if not required:
    raise SystemExit("workflow defines no verification jobs to gate on")
assert "portability_macos" not in jobs and "windows_durable_writer" not in jobs, \
    "native checks should reuse their lifecycle runner, not provision duplicate hosts"
native_body = workflow_text.split("\n  install_e2e:\n", 1)[1].split("\n  python39:\n", 1)[0]
for command, host in (
    ("bash tests/windows-durable-writer-smoke.sh", "Windows"),
    ("bash tests/scripts-smoke.sh --only test_process_liveness_uses_non_destructive_windows_probe", "Windows"),
    ("bash tests/scripts-smoke.sh --only test_autopilot_windows_reenter_launch_keeps_parent_anchor", "Windows"),
    ("OMS_BASH32_BIN=/bin/bash bash scripts/check-bash32.sh", "macOS"),
    ("OMS_BASH32_BIN=/bin/bash bash tests/scripts-smoke.sh --only test_shared_fast_mode_detection_gates_auto_verify", "macOS"),
    ("bash tests/bsd-portability-smoke.sh", "macOS"),
):
    step = next((part for part in native_body.split("      - name:")
                 if "run: " + command in part or
                 ("run: |\n" in part and "\n          " + command + "\n" in part)), "")
    assert step and "!cancelled() && runner.os == '%s'" % host in step, (command, host)
    assert "continue-on-error" not in step, "native failure must fail the merged job"

needs = set()
for index, line in enumerate(gate_body):
    inline = re.match(r"^\s{4}needs:\s*\[(.*)\]\s*$", line)
    if inline:
        needs = {name.strip() for name in inline.group(1).split(",") if name.strip()}
        break
    if re.match(r"^\s{4}needs:\s*$", line):
        for entry in gate_body[index + 1:]:
            item = re.match(r"^\s{6}-\s*([A-Za-z0-9_-]+)\s*$", entry)
            if not item:
                break
            needs.add(item.group(1))
        break

missing = sorted(required - needs)
if missing:
    raise SystemExit("gate does not depend on: %s" % ", ".join(missing))
unknown = sorted(needs - required)
if unknown:
    raise SystemExit("gate depends on jobs the workflow does not define: %s" % ", ".join(unknown))

body = "\n".join(gate_body)
unread = sorted(name for name in required if "needs.%s.result" % name not in body)
if unread:
    raise SystemExit("gate does not inspect the result of: %s" % ", ".join(unread))

# Execute the gate's real shell, not a second implementation of its policy.
import os
import subprocess
script = body.split("        run: |\n", 1)[1]
script = "\n".join(line[10:] for line in script.splitlines())
full_jobs = required - {"affected_plan", "affected_verify", "affected_operator"}
operator_job = workflow_text.split("\n  affected_operator:\n", 1)[1].split("\n  #", 1)[0]
operator_job = re.split(r"\n  [A-Za-z0-9_-]+:\n", operator_job, maxsplit=1)[0]
assert "    needs: affected_plan\n" in operator_job
assert "    if: needs.affected_plan.outputs.mode == 'affected' && needs.affected_plan.outputs.operator != ''\n" in operator_job
assert "    name: operator-tools (affected)\n" in operator_job
assert 'OPERATOR: ${{ needs.affected_plan.outputs.operator }}' in operator_job
assert 'bash tests/operator-tools-smoke.sh --jobs 4' in operator_job
assert 'bash tests/operator-tools-smoke.sh --section "$OPERATOR"' in operator_job
assert "outputs.operator }}\"" not in operator_job.split("run: |", 1)[1]
for mode in ("full", "affected"):
  for operator in ("", "section-shell", "all"):
    for native in ("true", "false"):
        env = dict(os.environ, AFFECTED_MODE=mode, AFFECTED_NATIVE=native, AFFECTED_PLAN_RESULT="success",
                   AFFECTED_OPERATOR=operator)
        env["AFFECTED_OPERATOR_RESULT"] = "success" if mode == "affected" and operator else "skipped"
        env["AFFECTED_VERIFY_RESULT"] = "success" if mode == "affected" else "skipped"
        env.update({name.upper() + "_RESULT": "success" if mode == "full" else "skipped"
                    for name in full_jobs})
        env["PYTHON39_RESULT"] = "success" if native == "true" else "skipped"
        env["INSTALL_E2E_RESULT"] = "success" if native == "true" or mode == "full" else "skipped"
        def passes(values):
            return subprocess.run(["bash", "-eu", "-c", script], env=values,
                                  capture_output=True).returncode == 0
        assert passes(env), (mode, native, "valid planned skips rejected")
        for name in required:
            key = name.upper() + "_RESULT"
            for result in ("success", "skipped", "failure", "cancelled", ""):
                if result != env[key]:
                    assert not passes(dict(env, **{key: result})), (mode, native, name, result)
        assert not passes(dict(env, AFFECTED_MODE="unknown"))
        for invalid in ("", "unknown"):
            assert not passes(dict(env, AFFECTED_NATIVE=invalid))

# Execute the native matrix alternatives as data and pin the plan expression.
import json
matrix = native_body.split("        include: ", 1)[1].splitlines()[0]
assert "fromJSON(needs.affected_plan.outputs.native == 'true' && " in matrix
alternatives = re.findall(r"'(\[.*?\])'", matrix)
assert len(alternatives) == 2
assert json.loads(alternatives[0]) == [
    {"os": "ubuntu-latest", "link_mode": "copy"},
    {"os": "macos-latest", "link_mode": "auto"},
    {"os": "windows-latest", "link_mode": "auto"}]
assert json.loads(alternatives[1]) == [{"os": "ubuntu-latest", "link_mode": "copy"}]

# Every expensive job follows the same decision. Run the planner shell with
# bounded Git/check stubs to verify events and absent-base fallback offline.
import tempfile
workflow_text = "\n".join(lines)
for name in full_jobs:
    job = workflow_text.split("\n  " + name + ":\n", 1)[1].split("\n  #", 1)[0]
    job = re.split(r"\n  [A-Za-z0-9_-]+:\n", job, maxsplit=1)[0]
    assert "    needs: affected_plan\n" in job, name
    condition = {"install_e2e": "needs.affected_plan.outputs.mode == 'full' || needs.affected_plan.outputs.native == 'true'",
                 "python39": "needs.affected_plan.outputs.native == 'true'"}.get(name, "needs.affected_plan.outputs.mode == 'full'")
    assert "    if: %s\n" % condition in job, name
plan_body = workflow_text.split("\n  affected_plan:\n", 1)[1].split("\n  install_e2e:\n", 1)[0]
plan_body, verify_body = plan_body.split("\n  affected_verify:\n", 1)
assert "    needs: affected_plan\n" in verify_body
assert "    if: needs.affected_plan.outputs.mode == 'affected'\n" in verify_body
assert "    name: affected checks\n" in verify_body
assert "--print-affected-mode" in plan_body and "--ci-output \"$GITHUB_OUTPUT\"" in plan_body
plan_script = plan_body.split("      - name: select affected changes\n", 1)[1].split("        run: |\n", 1)[1]
plan_script = "\n".join(line[10:] for line in plan_script.splitlines()
                        if line.startswith("          "))
setup_script = verify_body.split('      - name: prepare lazy shellcheck\n', 1)[1].split('      - name: verify affected changes\n', 1)[0].split('        run: |\n', 1)[1]
setup_script = '\n'.join(line[10:] for line in setup_script.splitlines() if line.startswith('          '))
subprocess.run(['bash', '-n'], input=setup_script, text=True, check=True)
# The launcher is Linux CI only; no network or package install in this test.
import shutil
if sys.platform.startswith('linux') and shutil.which('flock'):
    with tempfile.TemporaryDirectory(prefix='oms-ci-linter-') as tmp:
        root = pathlib.Path(tmp)
        binary = root / 'bin'
        binary.mkdir()
        scripts = {
            'curl': 'echo fetched >> "$PROBE_LOG"\n',
            'sha256sum': 'cat >/dev/null\n[ "${PROBE_BAD_CHECKSUM:-0}" != 1 ]\n',
            'tar': 'd="$RUNNER_TEMP/oms-shellcheck-cache/shellcheck-v0.10.0"\nmkdir -p "$d"\nprintf "#!/bin/sh\\nexit 0\\n" > "$d/shellcheck"\nchmod +x "$d/shellcheck"\n',
        }
        for name, body in scripts.items():
            path = binary / name
            path.write_text('#!/bin/sh\n' + body, encoding='utf-8')
            path.chmod(0o755)
        env = dict(os.environ, RUNNER_TEMP=tmp, GITHUB_ENV=str(root/'env'),
                   PROBE_LOG=str(root/'downloads'), PATH=str(binary)+os.pathsep+os.environ['PATH'])
        subprocess.run(['bash', '-eu', '-c', setup_script], env=env, check=True)
        assert not (root/'downloads').exists(), 'planning eagerly downloaded a linter'
        workers = [subprocess.Popen([str(root/'oms-shellcheck'), '--version'], env=env) for _ in range(3)]
        assert all(worker.wait() == 0 for worker in workers)
        assert (root/'downloads').read_text().splitlines() == ['fetched'], 'parallel lint downloaded more than once'
        bad = root/'bad'
        bad.mkdir()
        env.update(RUNNER_TEMP=str(bad), PROBE_BAD_CHECKSUM='1')
        for _ in range(2):
            assert subprocess.run([str(root/'oms-shellcheck'), '--version'], env=env).returncode != 0
        assert not (bad/'oms-shellcheck-cache/shellcheck-v0.10.0/shellcheck').exists()
        assert (root/'downloads').read_text().splitlines() == ['fetched', 'fetched'], 'failed acquisition retried per lint batch'
with tempfile.TemporaryDirectory(prefix="oms-ci-plan-") as tmp:
    root = pathlib.Path(tmp)
    (root / "scripts").mkdir()
    (root / "scripts/check.sh").write_text(
        'echo invoked >> "$PROBE_LOG"\n[ "$PROBE_MODE" != invalid ] || exit 2\n'
        'printf "mode=%s\\nnative=false\\n" "$PROBE_MODE" >> "$GITHUB_OUTPUT"\n', encoding="utf-8")
    for event, base, selected, expected, invoked in (
        ("pull_request", "valid", "affected", "affected", True),
        ("push", "valid", "affected", "affected", True),
        ("push", "valid", "full", "full", True),
        ("push", "missing", "affected", "full", False),
        ("push", "0" * 40, "affected", "full", False),
        ("schedule", "valid", "affected", "full", False),
        ("workflow_dispatch", "valid", "affected", "full", False),
        ("push", "valid", "invalid", None, True),
    ):
        output, log = root / "output", root / "invoked"
        output.write_text("", encoding="utf-8")
        log.write_text("", encoding="utf-8")
        env = dict(os.environ, EVENT_NAME=event, DIFF_BASE=base, PROBE_MODE=selected,
                   GITHUB_OUTPUT=str(output), PROBE_LOG=str(log))
        result = subprocess.run(["bash", "-eu", "-c",
                                 'git() { [ "$3" = "valid^{tree}" ]; };\n' + plan_script],
                                cwd=tmp, env=env, capture_output=True)
        assert (result.returncode == 0) == (expected is not None), (event, result.stderr)
        if expected is not None:
            assert "mode=" + expected in output.read_text(), (event, base)
            assert "native=" + ("false" if invoked else "true") in output.read_text(), (event, base)
        assert log.read_text().splitlines() == (["invoked"] if invoked else []), (event, base, "repeated or missing selection")
verify_script = verify_body.split("      - name: verify affected changes\n", 1)[1].split("        run: |\n", 1)[1]
verify_script = "\n".join(line[10:] for line in verify_script.splitlines() if line.startswith("          "))
with tempfile.TemporaryDirectory(prefix="oms-ci-verify-") as tmp:
    root = pathlib.Path(tmp)
    (root / "scripts").mkdir()
    (root / "scripts/check.sh").write_text(
        'printf "%s" "$PROBE_OUT" >> "${@: -1}"\n', encoding="utf-8")
    planned = "mode=affected\nnative=false\noperator=section-shell\n"
    for written, ok in ((planned, True),
                        ("mode=full\nnative=true\n", False),
                        ("mode=affected\nnative=false\noperator=all\n", False),
                        ("mode=affected\nnative=false\noperator=\n", False),
                        ("", False)):
        (root / "verify-output").unlink(missing_ok=True)
        env = dict(os.environ, RUNNER_TEMP=tmp, DIFF_BASE="base", PLAN_MODE="affected",
                   PLAN_NATIVE="false", PLAN_OPERATOR="section-shell", PROBE_OUT=written)
        result = subprocess.run(["bash", "-eu", "-c", verify_script], cwd=tmp, env=env, capture_output=True)
        assert (result.returncode == 0) == ok, (written, result.stderr)
    # An empty operator plan must still match the empty operator line.
    (root / "verify-output").unlink(missing_ok=True)
    env.update(PLAN_OPERATOR="", PROBE_OUT="mode=affected\nnative=false\noperator=\n")
    assert subprocess.run(["bash", "-eu", "-c", verify_script], cwd=tmp, env=env, capture_output=True).returncode == 0
checker = (pathlib.Path(sys.argv[1]).parents[2] / "scripts/check.sh").read_text()
selector = checker.split("prepare_affected_gate() {", 1)[1].split("\nif [ \"$RUN_QUICK\"", 1)[0]
selector = "prepare_affected_gate() {" + selector
with tempfile.TemporaryDirectory(prefix="oms-native-plan-") as tmp:
    root = pathlib.Path(tmp)
    changed = root / "paths"
    for paths, expected in ((["README.md"], "false"), (["AGENTS.md"], "false"),
                            (["AGENTS.md", "scripts/install-tools.sh"], "true"),
                            (["scripts/lib/room.py"], "true"), (["tests/bsd-portability-smoke.sh"], "true"),
                            ([".github/workflows/test.yml"], "true"), (["scripts/lib/oms-common.sh"], "true"),
                            (['"scripts/lib/quoted path.py"'], "true")):
        changed.write_text("\n".join(paths) + "\n")
        probe = r'''git() {
          case "$1 $2" in
            'diff --name-only') cat "$PROBE_PATHS" ;;
            'diff --quiet'|'ls-files --others') return 0 ;;
            'rev-parse '* ) echo identical-tree ;;
            *) return 2 ;;
          esac
        }
        AFFECTED_MODE=; AFFECTED_REASONS=; AFFECTED_TESTS=(); AFFECTED_CASE_LANGS=()
        AFFECTED_CASE_PATHS=(); AFFECTED_CASE_SELECTORS=(); QUICK_FROM=base; QUICK_TO=HEAD
        CHECK_RUNTIME="$PROBE_ROOT"; SKIP_LINT=0; CI_PARITY=0
        ''' + selector + '\nprepare_affected_gate\nprintf "%s\\n" "$AFFECTED_NATIVE"\n'
        result = subprocess.run(["bash", "-eu", "-c", probe], cwd=tmp,
                                env=dict(os.environ, PROBE_PATHS=str(changed), PROBE_ROOT=tmp),
                                capture_output=True, text=True)
        assert result.returncode == 0 and result.stdout.strip() == expected, (paths, result.stdout, result.stderr)

PY

for mode in --focused-only --scripts-smoke-only --quick --affected; do
  grep -Fq -- "$mode" "$ROOT/scripts/check.sh" ||
    fail "check.sh does not expose $mode"
done

# Standalone smoke suites belong to the focused gate. The full local gate
# composes that same block, while lint already covers tests/*.sh. Keep each
# suite in exactly one execution list so full mode cannot run it twice.
python3 - "$ROOT/scripts/check.sh" "$workflow" "$ROOT/tests" <<'PY' || fail "standalone smoke registration is incomplete or duplicated"
import pathlib
import re
import subprocess
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
workflow = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8")
tests = pathlib.Path(sys.argv[3])
expected = {
    "autopilot": "autopilot-smoke.sh",
    "draft-pr": "draft-pr-smoke.sh",
    "lifecycle-events": "lifecycle-events-smoke.sh",
    "supervisor": "supervisor-smoke.sh",
    "lifecycle-provider-integration": "lifecycle-provider-integration-smoke.sh",
    "install-lifecycle-lock": "install-lifecycle-lock-smoke.sh",
    "install-lifecycle": "install-lifecycle-smoke.sh",
    "file-lock-boundary": "file-lock-boundary-smoke.sh",
    "harness-residue-boundary": "harness-residue-boundary-smoke.sh",
    "patch-land-approval": "patch-land-approval-smoke.sh",
    "land": "land-smoke.sh",
    "tick": "tick-smoke.sh",
    "tool-lock": "tool-lock-smoke.sh",
    "provider-permissions-mcp-boundary": "provider-permissions-mcp-boundary-smoke.sh",
    "operator-tools": "operator-tools-smoke.sh",
    "skill-lifecycle": "skill-lifecycle-smoke.sh",
    "runtime-core": "runtime-core-smoke.sh",
    "runtime-core-integration": "runtime-core-integration-smoke.sh",
    "install-profile": "install-profile-smoke.sh",
}
stages = subprocess.check_output(['bash', sys.argv[1], '--focused-only', '--list-stages'], text=True).splitlines()
for stage in expected:
    assert stages.count(stage) == 1, (stage, stages)
assert len(stages) == len(set(stages)), 'focused gate duplicates a registered stage'
if "tests/*.sh" not in text:
    raise SystemExit("shellcheck lint no longer covers tests/*.sh")

# Follow literal suite invocations from the gate and workflow through any
# registered suite that composes another one. This catches an orphaned test
# file without maintaining a second hand-written catalog.
pattern = re.compile(r"tests/([A-Za-z0-9_.-]+-smoke\.sh)")
all_suites = {path.name for path in tests.glob("*-smoke.sh")}

def invocations(source):
    rows = []
    for line in source.splitlines():
        if line.lstrip().startswith("#"):
            continue
        rows.extend(pattern.findall(line))
    return rows

reachable = set(invocations(text + "\n" + workflow))
pending = list(reachable & all_suites)
while pending:
    owner = pending.pop()
    for child in invocations((tests / owner).read_text(encoding="utf-8")):
        if child in all_suites and child not in reachable:
            reachable.add(child)
            pending.append(child)
missing = sorted(all_suites - reachable)
if missing:
    raise SystemExit("standalone smoke suite is never executed: %s" % ", ".join(missing))
PY

# An env prefix would export into every test the suite runs; a flag cannot.
if grep -qE 'OMS_CHECK_(LINT|TESTS)=' "$workflow"; then
  fail "the gate split must be passed as a flag, not an inherited variable"
fi
if grep -Fq 'OH_MY_SETTING_UPGRADE_GH' "$ROOT/scripts/install-tools.sh"; then
  fail "test-only gh upgrade controls must not be part of the product interface"
fi

# The installed product is capability-scoped: a fresh install provides the
# core runtime plus exactly one coding-agent provider and records a private
# capability receipt; --full remains the explicit compatibility footprint.
# The old all-tools contract had no partial mode; the new one has no silent
# one — a missing capability is reported unavailable, never faked as success,
# and there is still no escape hatch that skips tool installation entirely.
if grep -Fq -- '--no-tools' "$ROOT/install.sh"; then
  fail "initial install must not expose a no-tools escape hatch"
fi
grep -Fq 'CAPABILITY_PROFILES="${OH_MY_SETTING_CAPABILITY_PROFILES:-core}"' "$ROOT/install.sh" ||
  fail "fresh install must default to the core capability profile"
grep -Fq '"$DEST/scripts/install-profile.sh"' "$ROOT/install.sh" ||
  fail "capability installs must go through the selective installer"
grep -Fq '"$DEST/scripts/install-tools.sh"' "$ROOT/install.sh" ||
  fail "--full must keep the legacy all-tools path reachable"
grep -Fq 'CONNECT_SERVICES=0' "$ROOT/install.sh" ||
  fail "a non-full install must not run unselected service logins"
# Updating never silently shrinks an existing install: no capability receipt
# means the legacy full-tool refresh; a receipt updates only what was chosen.
grep -Fq -- '--reapply --upgrade' "$ROOT/scripts/update.sh" ||
  fail "update must reapply the recorded capability selection"
grep -Fq 'install-tools.sh" --upgrade' "$ROOT/scripts/update.sh" ||
  fail "receipt-less installs must keep the legacy full-tool update"
# The selective installer itself must enforce the capability semantics the
# runtime promises: one provider for core, Notion optional, council plural.
grep -Fq 'selected capabilities remain unavailable' "$ROOT/scripts/install-profile.sh" ||
  fail "a failed capability check must fail the apply, not masquerade as success"
if grep -Eq -- 'install\.sh[^[:cntrl:]]*--no-tools' "$ROOT/README.md" "$ROOT/README.ko.md"; then
  fail "README still documents a partial initial install"
fi

# Exercise the environment escape hatch as behavior, but put network-facing
# commands behind failing stubs so a future regression cannot download or
# install anything during this source-contract probe.
install_probe="$(mktemp -d "${TMPDIR:-/tmp}/oms-required-tools.XXXXXX")"
trap 'rm -rf "$install_probe"' EXIT HUP INT TERM
mkdir -p "$install_probe/bin" "$install_probe/home"
for blocked in git curl; do
  cat > "$install_probe/bin/$blocked" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$0 $*" >> "$OMS_TEST_NETWORK_LOG"
exit 99
EOF
  chmod +x "$install_probe/bin/$blocked"
done
install_status=0
install_out="$(HOME="$install_probe/home" PATH="$install_probe/bin:/usr/bin:/bin" \
  OMS_TEST_NETWORK_LOG="$install_probe/network.log" \
  OH_MY_SETTING_DIR="$install_probe/checkout" OH_MY_SETTING_INSTALL_TOOLS=0 \
  bash "$ROOT/install.sh" 2>&1)" || install_status=$?
[ "$install_status" -eq 2 ] ||
  fail "OH_MY_SETTING_INSTALL_TOOLS=0 must be rejected as invalid configuration: $install_out"
printf '%s' "$install_out" | grep >/dev/null -F 'tool installation is required' ||
  fail "tool-install rejection did not explain the required contract"
[ ! -s "$install_probe/network.log" ] ||
  fail "tool-install rejection reached a network-facing command"

python3 - "$ROOT/skills.manifest.json" <<'PY' || fail "start phrase must deterministically trigger the spec interview"
import json, sys
rows = json.load(open(sys.argv[1], encoding="utf-8"))["skills"]
row = next(item for item in rows if item["name"] == "oms-spec-interview")
assert "start this project" in [str(value).casefold() for value in row.get("triggers", [])]
PY

for command in agent-call peer-ask peer-review peer-delegate plan-run; do
  help="$(bash "$ROOT/scripts/$command.sh" --help 2>&1)" ||
    fail "$command --help failed"
  grep -Fq 'xhigh' <<< "$help" || fail "$command help omits xhigh effort"
  grep -Fq 'max' <<< "$help" || fail "$command help omits max effort"
  grep -Fq 'ultra' <<< "$help" || fail "$command help omits ultra effort"
done

if grep -Fq 'oms run-capsule' "$ROOT/scripts/init.sh"; then
  fail "oms init still recommends the removed run-capsule front door"
fi
if grep -Fq -- '--effort' "$ROOT/custom-skills/oms-agent-harness/references/model-routing.md"; then
  fail "model routing skill still advertises the wrong public effort option"
fi
if grep -Eq 'Legacy tier|deep-tier|`oms check`' "$ROOT/docs/COMPONENTS.md"; then
  fail "components guide still describes removed model tiers or command names"
fi
if grep -Eq 'agent-consult|model tier below|deep planning/gates' "$ROOT/rules/global-AGENTS.md"; then
  fail "global agent rules still direct agents to removed commands or model tiers"
fi

# Shared-surface artifacts this repo ships carry the oms marker so their
# provenance is visible next to user-owned files in the same namespace
# (skills, output styles) and template installs stay collision-free.
# Repo-internal files referenced by path (roles/, prompts/, config/) and
# provider-mandated names are out of scope.
for surface in custom-skills templates/project-skills; do
  [ -d "$ROOT/$surface" ] || continue
  for entry in "$ROOT/$surface"/*/; do
    [ -d "$entry" ] || continue
    case "$(basename "$entry")" in
      oms-*) : ;;
      *) fail "$surface entry lacks the oms- prefix: $(basename "$entry")" ;;
    esac
  done
done
for entry in "$ROOT/output-styles"/*.md; do
  [ -f "$entry" ] || continue
  case "$(basename "$entry")" in
    oms-*) : ;;
    *) fail "output style lacks the oms- prefix: $(basename "$entry")" ;;
  esac
done
python3 - "$ROOT/skills.manifest.json" <<'PY' || fail "skills.manifest.json entries must keep the oms- prefix"
import json, os, sys
for row in json.load(open(sys.argv[1], encoding="utf-8"))["skills"]:
    assert row["name"].startswith("oms-"), row["name"]
    assert os.path.basename(row["source"]).startswith("oms-"), row["source"]
PY

grep -Fxq 'keep-coding-instructions: true' "$ROOT/output-styles/oms-korean.md" ||
  fail "language style must preserve native Claude coding instructions"

echo "source-distribution-smoke: ok"
