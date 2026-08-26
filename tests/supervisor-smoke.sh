#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-supervisor.XXXXXX")"
ESCAPED_PID=""
cleanup() {
  case "$ESCAPED_PID" in
    ''|*[!0-9]*) ;;
    *) kill -KILL "$ESCAPED_PID" 2>/dev/null || true ;;
  esac
  rm -rf "$TMP"
}
trap cleanup EXIT HUP INT TERM

fail() { echo "supervisor-smoke: $*" >&2; exit 1; }

export HOME="$TMP/home"
export XDG_STATE_HOME="$TMP/state"
export OMS_LOCK_DIR="$TMP/locks"
mkdir -p "$HOME" "$XDG_STATE_HOME" "$OMS_LOCK_DIR"

REPO="$TMP/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" config user.email test@example.com
git -C "$REPO" config user.name test
printf 'base\n' > "$REPO/file.txt"
git -C "$REPO" add file.txt
git -C "$REPO" commit -qm base

SUP="$ROOT/scripts/agent-supervisor.sh"
EVENTS="$ROOT/scripts/agent-events.sh"
[ -x "$SUP" ] || fail "missing executable: scripts/agent-supervisor.sh"

submit() {
  "$SUP" --repo "$REPO" submit --provider local --profile trusted-local \
    --completion-state 'done' "$@"
}

ok_attempt="$(submit --max-wall-seconds 10 -- bash -c \
  'test "${OMS_ATTEMPT_SUPERVISED:-}" = 1 && printf worker-ok')"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$ok_attempt" --timeout 15 >/dev/null ||
  fail "successful supervised job did not finish"
"$SUP" --repo "$REPO" status --attempt "$ok_attempt" --json > "$TMP/ok.json"
python3 - "$TMP/ok.json" <<'PY' || fail "successful job projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "done", row
assert row["runtime_state"] == "finished", row
assert row["exit_code"] == 0, row
assert row["pid_alive"] is False, row
assert row["usage_reports"]["duration_ms"] == 1, row
assert row["resume_mode"] == "child_attempt", row
assert row["sandbox"] is False, row
assert row["supervision_scope"] in {"process_group", "best_effort_tree"}, row
assert row["input_resume_supported"] is False, row
assert row["approval_scope"] == "patch_land_only", row
PY
"$SUP" --repo "$REPO" attach --attempt "$ok_attempt" --tail 20 > "$TMP/log"
grep -Fq worker-ok "$TMP/log" || fail "attach did not return bounded worker output"

# Worker output is drained without allowing a noisy or malicious command to
# grow the private runtime store without bound.
noisy_attempt="$(submit --max-wall-seconds 10 --max-log-bytes 4096 -- \
  python3 -c 'import sys; sys.stdout.write("HEAD:" + "x" * 20000 + ":TAIL\n")')"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$noisy_attempt" --timeout 15 >/dev/null ||
  fail "log-capped supervised job did not finish"
"$SUP" --repo "$REPO" status --attempt "$noisy_attempt" --json > "$TMP/noisy.json"
python3 - "$TMP/noisy.json" <<'PY' || fail "worker log was not capped"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "done", row
assert row["log_truncated"] is True, row
assert row["log_bytes"] <= 4096, row
assert row["max_log_bytes"] == 4096, row
PY
"$SUP" --repo "$REPO" attach --attempt "$noisy_attempt" --tail 5 > "$TMP/noisy.log"
python3 - "$TMP/noisy.log" <<'PY' || fail "truncated log marker is missing or oversized"
import pathlib, re, sys
data = pathlib.Path(sys.argv[1]).read_bytes()
assert len(data) <= 4096, len(data)
assert data.startswith(b"HEAD:"), data[:100]
assert data.endswith(b":TAIL\n"), data[-100:]
markers = re.findall(rb"\n\[oms: output truncated at log byte limit; ([0-9]+) bytes dropped\]\n", data)
assert len(markers) == 1, data
marker_size = len(markers[0]) + len(b"\n[oms: output truncated at log byte limit;  bytes dropped]\n")
assert int(markers[0]) == 20011 - (len(data) - marker_size), (markers, len(data))
PY

# Load-sensitive: with a 1-second wall, a heavily loaded machine can surface
# the runner's exception path (blocked/supervisor_error) instead of a clean
# timed_out — observed once under a live autopilot drive (2026-08-18 campaign,
# parked cycle 1) while baseline and standalone runs stayed green. If this
# fails inside a parallel drive, rerun it standalone before blaming the change.
timeout_attempt="$(submit --max-wall-seconds 1 -- bash -c 'sleep 20 & wait')"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
if "$SUP" --repo "$REPO" wait --attempt "$timeout_attempt" --timeout 10 >/dev/null 2>&1; then
  fail "timed-out job reported success"
fi
"$EVENTS" --repo "$REPO" show --attempt "$timeout_attempt" --json > "$TMP/timeout.json"
python3 - "$TMP/timeout.json" <<'PY' || fail "wall timeout was not terminal"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "timed_out", row
assert row["reason_code"] == "wall_budget_exceeded", row
PY

# Acceptance and autopilot phases run this whole flow under a Linux
# child-subreaper (autopilot-receipt supervise) that defers reaping adopted
# orphans until the phase exits. An orphaned group member then stays a zombie
# through every runner cleanup window, and killpg(pgid, 0) keeps succeeding;
# the completed timeout kill must still be judged terminal, not a supervisor
# error. The inner worker ignores TERM so the leader always dies first and
# the orphaned zombie is guaranteed, making the old misclassification
# deterministic rather than a kill-order race.
if [ "$(uname -s)" = Linux ]; then
  reaper_attempt="$(submit --max-wall-seconds 1 -- bash -c \
    'bash -c "trap \"\" TERM; sleep 20" & wait')"
  python3 - "$SUP" "$REPO" "$reaper_attempt" <<'PY' || fail "subreaper harness failed"
import ctypes, subprocess, sys
try:
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER; best effort
except (OSError, AttributeError):
    pass
sup, repo, attempt = sys.argv[1:4]
subprocess.run(
    [sup, "--repo", repo, "dispatch", "--max-running", "1", "--max-jobs", "1"],
    check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
# wait exits nonzero for a timed-out attempt; classification is what matters.
# Adopted zombies stay unreaped for this process's whole lifetime, exactly
# like a phase supervisor that only reaps after its supervised command exits.
subprocess.run(
    [sup, "--repo", repo, "wait", "--attempt", attempt, "--timeout", "15"],
    check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
PY
  "$EVENTS" --repo "$REPO" show --attempt "$reaper_attempt" --json \
    > "$TMP/subreaper-timeout.json"
  python3 - "$TMP/subreaper-timeout.json" <<'PY' || fail "wall timeout under a deferring subreaper was not terminal"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "timed_out", row
assert row["reason_code"] == "wall_budget_exceeded", row
PY
fi

# A descendant can deliberately leave the owned POSIX session with setsid and
# keep the inherited output pipe open. That process is outside trusted-local
# signal coverage, but it must not keep the supervisor itself nonterminal past
# the wall-clock decision.
if command -v setsid >/dev/null 2>&1; then
  escape_pid_file="$TMP/setsid-escape.pid"
  pipe_escape="$(submit --max-wall-seconds 1 -- bash -c \
    'setsid sleep 20 & printf "%s\n" "$!" > "$1"; sleep 20' \
    supervisor "$escape_pid_file")"
  "$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    [ ! -s "$escape_pid_file" ] || break
    sleep 0.05
  done
  [ -s "$escape_pid_file" ] || fail "setsid fixture did not publish its PID"
  ESCAPED_PID="$(tr -d '\r\n' < "$escape_pid_file")"
  if "$SUP" --repo "$REPO" wait --attempt "$pipe_escape" --timeout 6 \
    >/dev/null 2>&1; then
    fail "setsid pipe escape reported success"
  fi
  "$EVENTS" --repo "$REPO" show --attempt "$pipe_escape" --json \
    > "$TMP/pipe-escape.json"
  python3 - "$TMP/pipe-escape.json" <<'PY' || fail "setsid pipe escape kept the attempt nonterminal"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "timed_out", row
assert row["reason_code"] == "wall_budget_exceeded", row
PY
  cleanup_pid="$ESCAPED_PID"
  ESCAPED_PID=""
  kill -KILL "$cleanup_pid" 2>/dev/null || true
fi

# trusted-local has no authenticated provider-native token/cost limiter. A hard
# budget therefore fails before the worker starts; post-run telemetry would be
# too late to enforce a spending ceiling.
hard_budget_marker="$TMP/hard-budget-ran"
missing_attempt="$(submit --max-wall-seconds 10 --max-tokens 10 -- bash -c \
  'printf ran > "$1"' supervisor "$hard_budget_marker")"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
if "$SUP" --repo "$REPO" wait --attempt "$missing_attempt" --timeout 10 >/dev/null 2>&1; then
  fail "unsupported hard token budget reported success"
fi
"$EVENTS" --repo "$REPO" show --attempt "$missing_attempt" --json > "$TMP/missing.json"
python3 - "$TMP/missing.json" <<'PY' || fail "unsupported hard-budget reason is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "failed", row
assert row["reason_code"] == "hard_budget_unsupported", row
PY
[ ! -e "$hard_budget_marker" ] || fail "unsupported hard budget still launched its worker"

# Self-reported values remain cumulative observability data, but preloading
# them cannot authorize a hard-budget worker to start.
advisory_marker="$TMP/advisory-budget-ran"
over_attempt="$(submit --max-wall-seconds 10 --max-tokens 10 -- bash -c \
  'printf ran > "$1"' supervisor "$advisory_marker")"
"$EVENTS" --repo "$REPO" usage --attempt "$over_attempt" --tokens 6 \
  --actor provider-output-parser >/dev/null
"$EVENTS" --repo "$REPO" usage --attempt "$over_attempt" --tokens 5 \
  --actor provider-output-parser >/dev/null
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
if "$SUP" --repo "$REPO" wait --attempt "$over_attempt" --timeout 10 >/dev/null 2>&1; then
  fail "advisory telemetry authorized a hard token budget"
fi
"$EVENTS" --repo "$REPO" show --attempt "$over_attempt" --json > "$TMP/over.json"
python3 - "$TMP/over.json" <<'PY' || fail "advisory hard-budget reason is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "failed", row
assert row["reason_code"] == "hard_budget_unsupported", row
assert row["usage"]["tokens"] == 11, row
assert row["usage_reports"]["tokens"] == 2, row
PY
[ ! -e "$advisory_marker" ] || fail "advisory telemetry authorized a hard-budget worker"

# Two dispatchers share one cap. The first dispatch may start only one; the
# second queued attempt remains untouched until capacity is available.
one="$(submit --max-wall-seconds 10 -- bash -c 'sleep 2')"
two="$(submit --max-wall-seconds 10 -- bash -c 'sleep 2')"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 2 > "$TMP/dispatched"
[ "$(wc -l < "$TMP/dispatched" | tr -d ' ')" -eq 1 ] ||
  fail "dispatch exceeded max-running=1"
"$SUP" --repo "$REPO" status --attempt "$two" --json > "$TMP/two-queued.json"
python3 - "$TMP/two-queued.json" <<'PY' || fail "second attempt did not stay queued"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "queued", row
assert row["runtime_state"] == "queued", row
PY
"$SUP" --repo "$REPO" wait --attempt "$one" --timeout 10 >/dev/null ||
  fail "first capped attempt failed"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 2 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$two" --timeout 10 >/dev/null ||
  fail "queued attempt did not run after capacity opened"

cancelled="$(submit --max-wall-seconds 10 -- true)"
"$SUP" --repo "$REPO" cancel --attempt "$cancelled" >/dev/null
"$EVENTS" --repo "$REPO" show --attempt "$cancelled" --json > "$TMP/cancelled.json"
python3 - "$TMP/cancelled.json" <<'PY' || fail "queued cancellation is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "cancelled", row
PY

# A stale or corrupt PID record must never authorize a signal to a reused PID
# or process group. Cancellation reports the identity failure and clears the
# unrelated PID instead of trusting liveness alone.
if ! python3 - "$ROOT" "$REPO" <<'PY'
import argparse
import contextlib
import importlib.util
import io
import os
import sys
from pathlib import Path

root, repo = Path(sys.argv[1]), Path(sys.argv[2])
spec = importlib.util.spec_from_file_location(
    "oms_identity_cancel_test", str(root / "scripts/lib/attempt-runner.py")
)
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
args = argparse.Namespace(
    repo=str(repo), provider="local", profile="trusted-local",
    completion_state="done", cwd="", task_id="", run_id="",
    max_wall_seconds=10, max_tokens=None, max_cost_microusd=None,
    command_argv=["true"],
)
with contextlib.redirect_stdout(io.StringIO()) as output:
    runner.submit(args)
attempt = output.getvalue().strip()
runner.transition(repo, attempt, "starting", key="identity-test-starting")
runner.transition(repo, attempt, "working", key="identity-test-working")
with runner.ae.file_lock(runner.queue_lock_path(repo)):
    job = runner.load_job(repo, attempt)
    job.update({
        "runtime_state": "running",
        "pid": os.getpid(),
        "pid_identity": "0" * 64,
        "runner_pid": 0,
        "runner_pid_identity": "",
    })
    runner.save_job(repo, job)

signals = []
runner.terminate_process_group = lambda *a, **kw: signals.append((a, kw)) or True
with contextlib.redirect_stdout(io.StringIO()):
    runner.cancel(argparse.Namespace(repo=str(repo), attempt=attempt))
job = runner.load_job(repo, attempt)
state = runner.ae.load_projection(repo)[1][attempt]
assert not signals, signals
assert job["runtime_state"] == "interrupted", job
assert int(job.get("pid") or 0) == 0, job
assert state["state"] == "failed", state
assert state["reason_code"] == "process_identity_mismatch", state

# Cleanup helpers themselves also fail closed when no start token exists.
runner.process_group_alive = lambda pid: True
assert runner.stop_recorded_tree(12345, "") is False
assert not signals, signals

# Crash reconciliation applies the same rule; a reused child PID is evidence
# to clear and report, never authority to signal its numeric process group.
with contextlib.redirect_stdout(io.StringIO()) as output:
    runner.submit(args)
reconcile_attempt = output.getvalue().strip()
runner.transition(
    repo, reconcile_attempt, "starting", key="reconcile-identity-test-starting"
)
runner.transition(
    repo, reconcile_attempt, "working", key="reconcile-identity-test-working"
)
with runner.ae.file_lock(runner.queue_lock_path(repo)):
    job = runner.load_job(repo, reconcile_attempt)
    job.update({
        "runtime_state": "running",
        "pid": os.getpid(),
        "pid_identity": "f" * 64,
        "runner_pid": 0,
        "runner_pid_identity": "",
    })
    runner.save_job(repo, job)
runner.process_group_alive = lambda pid: False
with contextlib.redirect_stdout(io.StringIO()):
    runner.reconcile(argparse.Namespace(repo=str(repo), apply=True, json=False))
job = runner.load_job(repo, reconcile_attempt)
state = runner.ae.load_projection(repo)[1][reconcile_attempt]
assert not signals, signals
assert job["runtime_state"] == "interrupted", job
assert int(job.get("pid") or 0) == 0, job
assert state["state"] == "failed", state
assert state["reason_code"] == "process_identity_mismatch", state
PY
then
  fail "stale process identity authorized a cancellation signal"
fi

# A cancellation accepted while dispatch has reserved the slot but before the
# runner begins must become terminal without launching the command.
if ! python3 - "$ROOT" "$REPO" "$TMP/launch-cancel-marker" <<'PY'
import argparse
import contextlib
import importlib.util
import io
import sys
from pathlib import Path

root, repo, marker = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
spec = importlib.util.spec_from_file_location(
    "oms_launch_cancel_test", str(root / "scripts/lib/attempt-runner.py")
)
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
args = argparse.Namespace(
    repo=str(repo), provider="local", profile="trusted-local",
    completion_state="done", cwd="", task_id="", run_id="",
    max_wall_seconds=10, max_tokens=None, max_cost_microusd=None,
    command_argv=[sys.executable, "-c", "from pathlib import Path; Path(%r).write_text('ran')" % str(marker)],
)
with contextlib.redirect_stdout(io.StringIO()) as output:
    runner.submit(args)
attempt = output.getvalue().strip()
with runner.ae.file_lock(runner.queue_lock_path(repo)):
    job = runner.load_job(repo, attempt)
    job["runtime_state"] = "launching"
    runner.save_job(repo, job)
with contextlib.redirect_stdout(io.StringIO()):
    runner.cancel(argparse.Namespace(repo=str(repo), attempt=attempt))
rc = runner.runner(repo, attempt)
job = runner.load_job(repo, attempt)
state = runner.ae.load_projection(repo)[1][attempt]["state"]
assert rc == 130, (rc, job, state)
assert job["runtime_state"] == "cancelled", job
assert state == "cancelled", state
assert not marker.exists(), "launching cancellation still ran the command"
PY
then
  fail "launching cancellation did not converge"
fi

# Cancellation is a monotonic request. If it arrives after the runner has
# entered Popen but before the child PID is published, the runner must not
# overwrite it with "running" and let the command finish.
if ! python3 - "$ROOT" "$REPO" "$TMP/cancel-race-marker" <<'PY'
import argparse
import contextlib
import importlib.util
import io
import sys
import threading
from pathlib import Path

root, repo, marker = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
module_path = root / "scripts" / "lib" / "attempt-runner.py"
spec = importlib.util.spec_from_file_location("oms_cancel_race_test", str(module_path))
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

submit_args = argparse.Namespace(
    repo=str(repo),
    provider="local",
    profile="trusted-local",
    completion_state="done",
    cwd="",
    task_id="",
    run_id="",
    max_wall_seconds=10,
    max_tokens=None,
    max_cost_microusd=None,
    command_argv=[
        sys.executable,
        "-c",
        "import time; from pathlib import Path; time.sleep(.5); "
        "Path(%r).write_text('ran', encoding='utf-8')" % str(marker),
    ],
)
with contextlib.redirect_stdout(io.StringIO()) as output:
    runner.submit(submit_args)
attempt = output.getvalue().strip()

entered = threading.Event()
release = threading.Event()
original_popen = runner.subprocess.Popen

def gated_popen(*args, **kwargs):
    entered.set()
    if not release.wait(5):
        raise RuntimeError("cancel race test gate timed out")
    return original_popen(*args, **kwargs)

runner.subprocess.Popen = gated_popen
errors = []

def run_attempt():
    try:
        runner.runner(repo, attempt)
    except BaseException as exc:
        errors.append(repr(exc))

thread = threading.Thread(target=run_attempt)
thread.start()
assert entered.wait(5), "runner did not reach the launch barrier"
# importlib modules share the process-wide subprocess module. Restore Popen so
# cancel's Git lookup is not held at the launch barrier too.
runner.subprocess.Popen = original_popen
with contextlib.redirect_stdout(io.StringIO()):
    runner.cancel(argparse.Namespace(repo=str(repo), attempt=attempt))
release.set()
thread.join(10)
assert not thread.is_alive(), "cancelled runner did not exit"
assert not errors, errors
job = runner.load_job(repo, attempt)
state = runner.ae.load_projection(repo)[1][attempt]["state"]
assert state == "cancelled", (state, job)
assert not marker.exists(), "cancelled command still ran"
PY
then
  fail "starting cancellation was lost"
fi

# Once Popen succeeds, any supervisor/control-plane exception must terminate
# and reap the child tree before the runner unwinds.
if ! python3 - "$ROOT" "$REPO" "$TMP/control-plane-failure-marker" <<'PY'
import argparse
import contextlib
import importlib.util
import io
import sys
import time
from pathlib import Path

root, repo, marker = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
module_path = root / "scripts" / "lib" / "attempt-runner.py"
spec = importlib.util.spec_from_file_location("oms_runner_cleanup_test", str(module_path))
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

submit_args = argparse.Namespace(
    repo=str(repo),
    provider="local",
    profile="trusted-local",
    completion_state="done",
    cwd="",
    task_id="",
    run_id="",
    max_wall_seconds=10,
    max_tokens=None,
    max_cost_microusd=None,
    command_argv=[
        sys.executable,
        "-c",
        "import time; from pathlib import Path; time.sleep(.8); "
        "Path(%r).write_text('ran', encoding='utf-8')" % str(marker),
    ],
)
with contextlib.redirect_stdout(io.StringIO()) as output:
    runner.submit(submit_args)
attempt = output.getvalue().strip()
original_transition = runner.transition

def failing_transition(repo_arg, attempt_arg, state, reason="", key=""):
    if state == "working":
        raise runner.ae.OpsError("injected lifecycle write failure")
    return original_transition(repo_arg, attempt_arg, state, reason, key)

runner.transition = failing_transition
try:
    runner.runner(repo, attempt)
except runner.ae.OpsError:
    pass
else:
    raise AssertionError("injected lifecycle failure did not propagate")
time.sleep(1.1)
job = runner.load_job(repo, attempt)
assert job["runtime_state"] == "interrupted", job
assert int(job.get("pid") or 0) == 0, job
assert not marker.exists(), "child survived the supervisor exception"
PY
then
  fail "control-plane failure left a supervised process running"
fi

# Windows supervision must keep descendants in a Job Object even after poll()
# records the leader's return code. This mock regression runs on every host:
# the old returncode fast path skipped tree cleanup, while creation/assignment
# failures could otherwise release a runnable process outside any owned scope.
if ! python3 - "$ROOT" <<'PY'
import importlib.util
import inspect
import sys
from pathlib import Path

root = Path(sys.argv[1])
module_path = root / "scripts" / "lib" / "attempt-runner.py"
spec = importlib.util.spec_from_file_location("oms_windows_job_test", str(module_path))
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class FakeProcess:
    def __init__(self, returncode=None):
        self.pid = 4242
        self._handle = 2424
        self.returncode = returncode
        self.waited = 0
        self.killed = 0

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        self.waited += 1
        if self.returncode is None:
            self.returncode = -9
        return self.returncode

    def kill(self):
        self.killed += 1
        self.returncode = -9


class FakeJob:
    def __init__(self):
        self.events = []

    def assign(self, process):
        self.events.append(("assign", process._handle))

    def resume(self, process):
        self.events.append(("resume", process._handle))

    def terminate(self, exit_code=1):
        self.events.append(("terminate", exit_code))

    def close(self):
        self.events.append(("close",))


# poll() has already set returncode, but closing the owned job is still
# mandatory because background descendants can remain alive.
exited = FakeProcess(returncode=0)
exited_job = FakeJob()
assert runner.stop_process_tree(
    exited, "unused-on-job-path", windows_job=exited_job
) == 0
assert exited_job.events == [("terminate", 1), ("close",)], exited_job.events
assert exited.waited == 1, exited.waited

# An active leader follows the same Job Object cleanup path used by cancel and
# timeout; the process is reaped only after the whole job is terminated.
active = FakeProcess()
active_job = FakeJob()
assert runner.stop_process_tree(
    active, "unused-on-job-path", windows_job=active_job
) == -9
assert active_job.events == [("terminate", 1), ("close",)], active_job.events
assert active.waited == 1, active.waited


class FakeJobApi:
    def __init__(self):
        self.events = []

    def create_job(self):
        self.events.append(("create",))
        return 77

    def set_kill_on_close(self, handle):
        self.events.append(("kill_on_close", handle))

    def assign_process(self, handle, process_handle):
        self.events.append(("assign", handle, process_handle))

    def resume_process(self, process_handle):
        self.events.append(("resume", process_handle))

    def terminate_job(self, handle, exit_code):
        self.events.append(("terminate", handle, exit_code))

    def close_handle(self, handle):
        self.events.append(("close", handle))


api = FakeJobApi()
job = runner.WindowsJobObject.create(api=api)
process = FakeProcess()
job.assign(process)
job.resume(process)
job.terminate()
job.close()
assert api.events == [
    ("create",),
    ("kill_on_close", 77),
    ("assign", 77, 2424),
    ("resume", 2424),
    ("terminate", 77, 1),
    ("close", 77),
], api.events

# Windows launch is suspended until assignment succeeds. A failed assignment
# kills and reaps that suspended leader and closes the job before surfacing the
# failure; a failed job creation must not call Popen at all.
created = []
created_processes = []


def fake_popen(*args, **kwargs):
    created.append((args, kwargs))
    process = FakeProcess()
    created_processes.append(process)
    return process


original_popen = runner.subprocess.Popen
runner.subprocess.Popen = fake_popen
try:
    launch_job = FakeJob()
    launched, owned_job = runner.launch_supervised_process(
        ["fixture"], ".", {}, windows=True, job_factory=lambda: launch_job
    )
    assert launched is not None and owned_job is launch_job
    flags = created[-1][1]["creationflags"]
    assert flags & runner.WINDOWS_CREATE_SUSPENDED, flags
    assert flags & runner.WINDOWS_CREATE_NEW_PROCESS_GROUP, flags
    assert launch_job.events[:2] == [("assign", 2424), ("resume", 2424)]
    runner.stop_process_tree(launched, "", windows_job=owned_job)

    class AssignmentFailureJob(FakeJob):
        def assign(self, process):
            self.events.append(("assign_failed", process._handle))
            raise runner.ae.OpsError("injected assignment failure")

    failed_job = AssignmentFailureJob()
    before = len(created)
    try:
        runner.launch_supervised_process(
            ["fixture"], ".", {}, windows=True,
            job_factory=lambda: failed_job,
        )
    except runner.ae.OpsError:
        pass
    else:
        raise AssertionError("failed Job Object assignment was accepted")
    assert len(created) == before + 1
    failed_process = created_processes[before]
    assert failed_process.killed == 1, failed_process.killed
    assert failed_process.waited == 1, failed_process.waited
    assert ("close",) in failed_job.events, failed_job.events

    calls_before_create_failure = len(created)
    def fail_job_create():
        raise runner.ae.OpsError("injected job creation failure")
    try:
        runner.launch_supervised_process(
            ["fixture"], ".", {}, windows=True,
            job_factory=fail_job_create,
        )
    except runner.ae.OpsError:
        pass
    else:
        raise AssertionError("failed Job Object creation was accepted")
    assert len(created) == calls_before_create_failure, created
finally:
    runner.subprocess.Popen = original_popen

runner_source = inspect.getsource(runner.runner)
assert runner_source.count("windows_job=windows_job") == 3, runner_source
assert '"job_object" if os.name == "nt" else "process_group"' in inspect.getsource(
    runner.public_job
)
PY
then
  fail "Windows Job Object supervision contract is incomplete"
fi

# A crash between attempt.created and the runtime job write must not leave an
# agent-supervisor-owned attempt queued forever. Reconcile waits out the submit
# race, ignores other tools, preserves resume lineage, and converges exactly
# once. A late submit cannot resurrect an attempt that reconcile already owns.
if ! python3 - "$ROOT" "$TMP/orphan-reconcile-repo" <<'PY'
import argparse
import contextlib
import importlib.util
import inspect
import io
import subprocess
import sys
from pathlib import Path

root, repo = Path(sys.argv[1]), Path(sys.argv[2])
repo.mkdir()
subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
subprocess.run(
    ["git", "-C", str(repo), "config", "user.email", "test@example.com"],
    check=True,
)
subprocess.run(
    ["git", "-C", str(repo), "config", "user.name", "test"], check=True
)
(repo / "file").write_text("x\n", encoding="utf-8")
subprocess.run(["git", "-C", str(repo), "add", "file"], check=True)
subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)

module_path = root / "scripts" / "lib" / "attempt-runner.py"
spec = importlib.util.spec_from_file_location("oms_orphan_reconcile_test", str(module_path))
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def create(tool="agent-supervisor", parent=""):
    return runner.ae.create_attempt(
        repo,
        provider="local",
        tool=tool,
        parent_attempt_id=parent,
    )


parent = create(tool="fixture-owner")
supervisor_orphan = create()
child_orphan = create(parent=parent)
ordinary_queued = create(tool="agent-run")
job_backed = create()
job_intent = {
    "schema": 1,
    "attempt_id": job_backed,
    "runtime_state": "queued",
}
runner.save_new_supervisor_job(repo, job_intent)
runner.save_new_supervisor_job(repo, job_intent)

args = argparse.Namespace(repo=str(repo), apply=True, json=False)
runner.ORPHAN_JOB_GRACE_SECONDS = 3600
with contextlib.redirect_stdout(io.StringIO()):
    runner.reconcile(args)
attempts = runner.ae.load_projection(repo)[1]
assert attempts[supervisor_orphan]["state"] == "queued", attempts[supervisor_orphan]
assert attempts[child_orphan]["state"] == "queued", attempts[child_orphan]

runner.ORPHAN_JOB_GRACE_SECONDS = 0
with contextlib.redirect_stdout(io.StringIO()):
    runner.reconcile(args)
attempts = runner.ae.load_projection(repo)[1]
assert attempts[supervisor_orphan]["state"] == "blocked", attempts[supervisor_orphan]
assert attempts[supervisor_orphan]["reason_code"] == "runtime_job_missing"
assert attempts[child_orphan]["state"] == "blocked", attempts[child_orphan]
assert attempts[child_orphan]["parent_attempt_id"] == parent, attempts[child_orphan]
assert attempts[ordinary_queued]["state"] == "queued", attempts[ordinary_queued]
assert attempts[job_backed]["state"] == "queued", attempts[job_backed]

before_retry = {
    key: attempts[key]["sequence"] for key in (supervisor_orphan, child_orphan)
}
with contextlib.redirect_stdout(io.StringIO()):
    runner.reconcile(args)
attempts = runner.ae.load_projection(repo)[1]
assert {
    key: attempts[key]["sequence"] for key in (supervisor_orphan, child_orphan)
} == before_retry

late = create()
runner.transition(repo, late, "blocked", "runtime_job_missing", "late-race")
try:
    runner.save_new_supervisor_job(repo, {"attempt_id": late})
except runner.ae.OpsError:
    pass
else:
    raise AssertionError("late runtime job resurrected a blocked attempt")
assert not runner.job_path(repo, late).exists()

assert "save_new_supervisor_job" in inspect.getsource(runner.submit)
assert "save_new_supervisor_job" in inspect.getsource(runner.resume)
PY
then
  fail "orphaned supervisor attempts do not converge safely"
fi

# A successful direct child may not leave same-session background processes
# behind. Deliberate new-session escape needs an OS containment backend.
descendant_marker="$TMP/descendant-marker"
descendant_attempt="$(submit --max-wall-seconds 1 -- bash -c \
  '(sleep 2; printf escaped > "$1") &' supervisor "$descendant_marker")"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$descendant_attempt" --timeout 10 >/dev/null ||
  fail "descendant cleanup fixture did not finish"
# Outwait the fixture's 2s delayed write with a real margin.
sleep 4
[ ! -e "$descendant_marker" ] || fail "background descendant escaped supervision"

# If the runner itself disappears, reconcile owns cleanup of the recorded
# child group before it marks the attempt interrupted/blocked.
runner_lost_marker="$TMP/runner-lost-marker"
runner_lost="$(submit --max-wall-seconds 20 -- bash -c \
  'sleep 4; printf escaped > "$1"' supervisor "$runner_lost_marker")"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
runner_pid=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  runner_pid="$(python3 - "$ROOT" "$REPO" "$runner_lost" <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("oms_pid_read", str(Path(sys.argv[1]) / "scripts/lib/attempt-runner.py"))
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
job = runner.load_job(Path(sys.argv[2]), sys.argv[3])
if job.get("runtime_state") == "running":
    print(job.get("runner_pid", ""))
PY
)"
  [ -n "$runner_pid" ] && break
  sleep 0.1
done
[ -n "$runner_pid" ] || fail "runner-loss fixture never entered running"
kill -KILL "$runner_pid"
"$SUP" --repo "$REPO" reconcile --apply >/dev/null ||
  fail "reconcile could not clean a lost runner"
"$EVENTS" --repo "$REPO" show --attempt "$runner_lost" --json > "$TMP/runner-lost.json"
python3 - "$TMP/runner-lost.json" <<'PY' || fail "lost runner did not become blocked"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "blocked", row
assert row["reason_code"] == "runner_lost", row
PY
sleep 5
[ ! -e "$runner_lost_marker" ] || fail "lost runner's child escaped reconciliation"

# Serve counts the IDs dispatch accepted, even when a job finishes too quickly
# to appear in an after-state snapshot. Runner launch failures converge to a
# blocked record, and a malformed runtime job fails the capacity calculation.
if ! python3 - "$ROOT" "$TMP/control-repo" <<'PY'
import argparse
import contextlib
import importlib.util
import io
import json
import subprocess
import sys
from pathlib import Path

root, repo = Path(sys.argv[1]), Path(sys.argv[2])
repo.mkdir()
subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
subprocess.run(["git", "-C", str(repo), "config", "user.name", "test"], check=True)
(repo / "file").write_text("x\n", encoding="utf-8")
subprocess.run(["git", "-C", str(repo), "add", "file"], check=True)
subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
spec = importlib.util.spec_from_file_location(
    "oms_supervisor_contract_test", str(root / "scripts/lib/attempt-runner.py")
)
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

def submit_one():
    args = argparse.Namespace(
        repo=str(repo), provider="local", profile="trusted-local",
        completion_state="done", cwd="", task_id="", run_id="",
        max_wall_seconds=10, max_tokens=None, max_cost_microusd=None,
        command_argv=["true"],
    )
    with contextlib.redirect_stdout(io.StringIO()) as output:
        runner.submit(args)
    return output.getvalue().strip()

for _ in range(3):
    submit_one()
calls = []
original_dispatch = runner.dispatch
def instant_dispatch(args):
    queued = [job for job in runner.all_jobs(repo, strict=True) if job["runtime_state"] == "queued"]
    assert queued
    job = queued[0]
    job["runtime_state"] = "finished"
    runner.save_job(repo, job)
    args.launched_ids = [job["attempt_id"]]
    calls.append(job["attempt_id"])
    return 0
runner.dispatch = instant_dispatch
runner.serve(argparse.Namespace(
    repo=str(repo), max_running=1, max_jobs=1, provider_limit=[],
    max_total_tokens=None, max_total_cost_microusd=None,
    wall_seconds=2, until_idle=True, interval=0.01,
))
assert len(calls) == 1, calls
runner.dispatch = original_dispatch

# Restore a clean runtime root for the launch-failure and corruption checks.
for path in (runner.runtime_root(repo) / "jobs").glob("*.json"):
    path.unlink()
attempt = submit_one()
original_popen = runner.subprocess.Popen
runner.subprocess.Popen = lambda *args, **kwargs: (_ for _ in ()).throw(OSError("injected"))
try:
    runner.dispatch(argparse.Namespace(
        repo=str(repo), max_running=1, max_jobs=1, provider_limit=[],
        max_total_tokens=None, max_total_cost_microusd=None,
    ))
except runner.ae.OpsError:
    pass
else:
    raise AssertionError("runner launch failure was accepted")
finally:
    runner.subprocess.Popen = original_popen
job = runner.load_job(repo, attempt)
state = runner.ae.load_projection(repo)[1][attempt]
assert job["runtime_state"] == "interrupted", job
assert state["state"] == "blocked" and state["reason_code"] == "runner_launch_failed", state

for path in (runner.runtime_root(repo) / "jobs").glob("*.json"):
    path.unlink()
attempt = submit_one()
original_transition = runner.transition
def fail_review(repo_arg, attempt_arg, state_name, reason="", key=""):
    if state_name == "review":
        raise runner.ae.OpsError("injected review transition failure")
    return original_transition(repo_arg, attempt_arg, state_name, reason, key)
runner.transition = fail_review
try:
    runner.runner(repo, attempt)
except runner.ae.OpsError:
    pass
else:
    raise AssertionError("verifying control-plane failure was accepted")
finally:
    runner.transition = original_transition
job = runner.load_job(repo, attempt)
state = runner.ae.load_projection(repo)[1][attempt]
assert job["runtime_state"] == "interrupted", job
assert state["state"] == "blocked" and state["reason_code"] == "supervisor_error", state

bad = runner.runtime_root(repo) / "jobs" / "att_broken.json"
bad.write_text("{not-json\n", encoding="utf-8")
try:
    runner.dispatch(argparse.Namespace(
        repo=str(repo), max_running=1, max_jobs=1, provider_limit=[],
        max_total_tokens=None, max_total_cost_microusd=None,
    ))
except runner.ae.OpsError:
    pass
else:
    raise AssertionError("malformed runtime job was silently ignored")
PY
then
  fail "supervisor control-plane bounds are wrong"
fi

# Runtime logs and command records live outside the repository. GC is a dry-run
# by default, keeps every nonterminal/review attempt, and deletes only terminal
# records older than the explicit retention window. Lifecycle evidence remains.
review_attempt="$("$SUP" --repo "$REPO" submit --provider local \
  --profile trusted-local --completion-state review --max-wall-seconds 10 -- true)"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$review_attempt" --timeout 10 >/dev/null ||
  fail "review fixture did not finish"
live_attempt="$(submit --max-wall-seconds 10 -- true)"

"$SUP" --repo "$REPO" gc --older-than-days 0 --json > "$TMP/gc-dry.json" ||
  fail "supervisor GC dry-run failed"
python3 - "$TMP/gc-dry.json" "$ok_attempt" "$review_attempt" "$live_attempt" <<'PY' || fail "supervisor GC selected a live attempt"
import json, sys
rows = json.load(open(sys.argv[1], encoding="utf-8"))
ids = {row["attempt_id"] for row in rows}
assert sys.argv[2] in ids, (ids, sys.argv[2])
assert sys.argv[3] not in ids, (ids, sys.argv[3])
assert sys.argv[4] not in ids, (ids, sys.argv[4])
assert all(row["terminal"] is True for row in rows), rows
PY
"$SUP" --repo "$REPO" status --attempt "$ok_attempt" >/dev/null ||
  fail "GC dry-run deleted a job"

"$SUP" --repo "$REPO" gc --older-than-days 0 --apply --json > "$TMP/gc-apply.json" ||
  fail "supervisor GC apply failed"
if "$SUP" --repo "$REPO" status --attempt "$ok_attempt" >/dev/null 2>&1; then
  fail "GC apply retained an expired terminal runtime record"
fi
"$SUP" --repo "$REPO" status --attempt "$review_attempt" >/dev/null ||
  fail "GC deleted a review attempt"
"$SUP" --repo "$REPO" status --attempt "$live_attempt" >/dev/null ||
  fail "GC deleted a queued attempt"
"$EVENTS" --repo "$REPO" show --attempt "$ok_attempt" --json > "$TMP/gc-event.json" ||
  fail "GC deleted durable lifecycle evidence"
python3 - "$TMP/gc-event.json" <<'PY' || fail "GC changed lifecycle evidence"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "done", row
PY

"$SUP" --repo "$REPO" cancel --attempt "$live_attempt" >/dev/null ||
  fail "could not clear queued GC fixture"
top_gc_attempt="$(submit --max-wall-seconds 10 -- true)"
"$SUP" --repo "$REPO" dispatch --max-running 1 --max-jobs 1 >/dev/null
"$SUP" --repo "$REPO" wait --attempt "$top_gc_attempt" --timeout 10 >/dev/null ||
  fail "top-level GC fixture did not finish"
"$ROOT/scripts/gc.sh" --repo "$REPO" --days 0 --apply > "$TMP/top-gc.out" ||
  fail "top-level GC failed"
grep -Fq "$top_gc_attempt" "$TMP/top-gc.out" ||
  fail "top-level GC did not report the supervisor runtime record"
if "$SUP" --repo "$REPO" status --attempt "$top_gc_attempt" >/dev/null 2>&1; then
  fail "top-level GC did not prune terminal supervisor runtime"
fi

echo "supervisor-smoke: ok"
