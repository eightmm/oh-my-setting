#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-lifecycle-events.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "lifecycle-events-smoke: $*" >&2
  exit 1
}

new_repo() {
  local path="$1"
  mkdir -p "$path"
  git -C "$path" init -q
  git -C "$path" config user.email test@example.com
  git -C "$path" config user.name test
  printf 'base\n' > "$path/file.txt"
  git -C "$path" add file.txt
  git -C "$path" commit -qm base
}

EVENTS="$ROOT/scripts/agent-events.sh"
APPROVALS="$ROOT/scripts/approval-inbox.sh"
REPO="$TMP/repo"
export XDG_STATE_HOME="$TMP/state"
export OMS_LOCK_DIR="$TMP/locks"
new_repo "$REPO"

[ -x "$EVENTS" ] || fail "missing executable: scripts/agent-events.sh"
[ -x "$APPROVALS" ] || fail "missing executable: scripts/approval-inbox.sh"

# Lock acquisition has a bounded wait, but a live owner is never reclaimed
# merely because that wait elapsed. A positively dead owner remains recoverable.
if ! python3 - "$ROOT/scripts/lib/agent-events.py" "$TMP/lock-target" <<'PY'
import importlib.util
import json
import os
import sys
import threading
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location("agent_events", sys.argv[1])
ae = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(ae)
target = Path(sys.argv[2])
os.environ["OMS_LOCK_TIMEOUT"] = "1"
os.environ["OMS_LOCK_STALE_SECONDS"] = "60"

acquired = False
with ae.file_lock(target):
    try:
        with ae.file_lock(target):
            acquired = True
    except ae.OpsError:
        pass
assert not acquired, "a live lock owner was reclaimed after the acquisition timeout"

lock = ae.runtime_lock_dir(target)
dead_pid = 99999999
while ae.pid_alive(dead_pid):
    dead_pid += 1
lock.mkdir()
(lock / "owner.json").write_text(json.dumps({
    "pid": dead_pid, "owner_nonce": "dead-owner", "started": time.time(),
}), encoding="utf-8")
with ae.file_lock(target):
    pass
assert not lock.exists(), "a dead lock owner was not reclaimed"

# Two contenders may both observe one dead generation. Pause B after that
# observation, let A reclaim and acquire a new live generation, then resume B.
# B must revalidate under a crash-safe recovery serialization boundary instead
# of renaming A's canonical lock and entering concurrently.
aba_target = Path(str(target) + "-aba")
aba_lock = ae.runtime_lock_dir(aba_target)
aba_lock.mkdir()
(aba_lock / "owner.json").write_text(json.dumps({
    "pid": dead_pid, "owner_nonce": "aba-dead-owner", "started": time.time(),
}), encoding="utf-8")
original_reclaim = ae.reclaim_lock
b_at_reclaim = threading.Event()
release_b = threading.Event()
a_entered = threading.Event()
release_a = threading.Event()
b_entered = threading.Event()
active_guard = threading.Lock()
active = [0]
overlap = [False]
errors = []
b_paused = [False]

def controlled_reclaim(lock_path, *args):
    if threading.current_thread().name == "stale-B" and not b_paused[0]:
        b_paused[0] = True
        b_at_reclaim.set()
        if not release_b.wait(5):
            raise AssertionError("timed out releasing stale reclaimer B")
    return original_reclaim(lock_path, *args)

def lock_worker(name, entered, release=None):
    try:
        with ae.file_lock(aba_target):
            with active_guard:
                if active[0]:
                    overlap[0] = True
                active[0] += 1
            entered.set()
            if release is not None and not release.wait(5):
                raise AssertionError("timed out releasing %s" % name)
            with active_guard:
                active[0] -= 1
    except BaseException as exc:
        errors.append((name, exc))

os.environ["OMS_LOCK_TIMEOUT"] = "3"
ae.reclaim_lock = controlled_reclaim
thread_b = threading.Thread(target=lock_worker, name="stale-B", args=("B", b_entered))
thread_b.start()
assert b_at_reclaim.wait(2), "B did not reach stale reclaim"
thread_a = threading.Thread(
    target=lock_worker, name="fresh-A", args=("A", a_entered, release_a)
)
thread_a.start()
assert a_entered.wait(2), "A did not acquire the reclaimed generation"
release_b.set()
b_entered_while_a_live = b_entered.wait(0.3)
release_a.set()
thread_a.join(5)
thread_b.join(5)
ae.reclaim_lock = original_reclaim
assert not thread_a.is_alive() and not thread_b.is_alive(), "lock race threads did not finish"
assert not errors, errors
assert not b_entered_while_a_live and not overlap[0], (
    "a stale observer renamed a new live lock generation", overlap
)

# A live PID may be a reused identity from a crashed owner. A mismatched
# process-start token is stale even though kill(pid, 0) succeeds.
reuse_target = Path(str(target) + "-pid-reuse")
reuse_lock = ae.runtime_lock_dir(reuse_target)
reuse_lock.mkdir()
(reuse_lock / "owner.json").write_text(json.dumps({
    "pid": os.getpid(),
    "process_start": "not-the-current-process-generation",
    "owner_nonce": "reused-pid-owner",
    "started": time.time(),
}), encoding="utf-8")
reused_reclaimed = False
try:
    with ae.file_lock(reuse_target):
        owner = json.loads((reuse_lock / "owner.json").read_text(encoding="utf-8"))
        assert owner.get("process_start"), owner
        reused_reclaimed = True
except ae.OpsError:
    pass
assert reused_reclaimed, "a reused live PID without the matching start token wedged the lock"
PY
then
  fail "lifecycle lock ownership or bounded reclaim is wrong"
fi

# Event persistence must tolerate short writes and sync parent directories
# after creating or atomically replacing durable state.
if ! python3 - "$ROOT/scripts/lib/agent-events.py" "$TMP/io" <<'PY'
import importlib.util
import json
import os
import stat
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("agent_events", sys.argv[1])
ae = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(ae)
base = Path(sys.argv[2])

short_path = base / "short" / "events.jsonl"
original_write = ae.os.write
def short_write(fd, data):
    return original_write(fd, memoryview(data)[:min(3, len(data))])
ae.os.write = short_write
try:
    ae.append_row(short_path, {"event": "short-write-safe"})
finally:
    ae.os.write = original_write
rows = [json.loads(line) for line in short_path.read_text(encoding="utf-8").splitlines()]
assert rows == [{"event": "short-write-safe"}], rows

if os.name != "nt":
    directory_syncs = []
    original_fsync = ae.os.fsync
    def tracked_fsync(fd):
        if stat.S_ISDIR(os.fstat(fd).st_mode):
            directory_syncs.append(fd)
        return original_fsync(fd)
    ae.os.fsync = tracked_fsync
    try:
        ae.write_json_atomic(base / "atomic" / "state.json", {"ok": True})
        ae.append_row(base / "append" / "events.jsonl", {"ok": True})
    finally:
        ae.os.fsync = original_fsync
    assert len(directory_syncs) >= 2, directory_syncs
PY
then
  fail "lifecycle durable writes are not short-write or directory-sync safe"
fi

# A start request key is repository-scoped: retrying the same request returns
# its original attempt, while reusing the key for another request fails.
idem_attempt="$($EVENTS --repo "$REPO" start --provider codex --tool agent-call \
  --task-id idem-task --idempotency-key idem-request)" ||
  fail "could not create idempotent attempt"
idem_retry="$($EVENTS --repo "$REPO" start --provider codex --tool agent-call \
  --task-id idem-task --idempotency-key idem-request)" ||
  fail "could not retry idempotent attempt"
[ "$idem_retry" = "$idem_attempt" ] ||
  fail "start idempotency returned a different attempt id"
if $EVENTS --repo "$REPO" start --provider codex --tool agent-run \
  --task-id idem-task --idempotency-key idem-request \
  >/dev/null 2>&1; then
  fail "conflicting start idempotency replay was accepted"
fi
if ! python3 - "$REPO/.oms/lifecycle/events.jsonl" "$idem_attempt" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
created = [row for row in rows if row.get("event_type") == "attempt.created"
           and row.get("idempotency_key") == "idem-request"]
assert len(created) == 1, created
assert created[0]["attempt_id"] == sys.argv[2], created
PY
then
  fail "start idempotency appended duplicate attempt.created rows"
fi

attempt="$($EVENTS --repo "$REPO" start --provider codex --tool agent-call \
  --task-id task-one --run-id run-one --max-wall-seconds 60)" ||
  fail "could not create an attempt"
case "$attempt" in att_*) ;; *) fail "unexpected attempt id: $attempt" ;; esac

$EVENTS --repo "$REPO" transition --attempt "$attempt" --state starting >/dev/null
$EVENTS --repo "$REPO" transition --attempt "$attempt" --state working >/dev/null
$EVENTS --repo "$REPO" heartbeat --attempt "$attempt" >/dev/null
$EVENTS --repo "$REPO" usage --attempt "$attempt" --tokens 12 \
  --duration-ms 25 --cost-microusd 7 >/dev/null
$EVENTS --repo "$REPO" transition --attempt "$attempt" --state verifying >/dev/null
$EVENTS --repo "$REPO" transition --attempt "$attempt" --state review >/dev/null

if $EVENTS --repo "$REPO" transition --attempt "$attempt" --state queued >/dev/null 2>&1; then
  fail "illegal review -> queued transition was accepted"
fi

$EVENTS --repo "$REPO" transition --attempt "$attempt" --state "done" >/dev/null
if $EVENTS --repo "$REPO" transition --attempt "$attempt" --state working >/dev/null 2>&1; then
  fail "terminal attempt was revived in place"
fi

resumed="$($EVENTS --repo "$REPO" resume --attempt "$attempt")" ||
  fail "terminal attempt did not create a new resumable attempt"
[ "$resumed" != "$attempt" ] || fail "resume reused the terminal attempt id"

$EVENTS --repo "$REPO" show --attempt "$attempt" --json > "$TMP/show.json"
python3 - "$TMP/show.json" "$attempt" <<'PY' || fail "attempt projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["attempt_id"] == sys.argv[2], row
assert row["state"] == "done", row
assert row["sequence"] == 8, row
assert row["usage"] == {"tokens": 12, "duration_ms": 25, "cost_microusd": 7}, row
assert row["budget"]["max_wall_seconds"] == 60, row
PY

$EVENTS --repo "$REPO" show --attempt "$resumed" --json > "$TMP/resumed.json"
python3 - "$TMP/resumed.json" "$attempt" <<'PY' || fail "resume lineage is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "queued", row
assert row["parent_attempt_id"] == sys.argv[2], row
PY

# An event key is idempotent only for the same payload. A conflicting replay is
# a caller bug and must fail instead of silently losing one state change.
$EVENTS --repo "$REPO" transition --attempt "$resumed" --state starting \
  --idempotency-key transition-one >/dev/null
$EVENTS --repo "$REPO" transition --attempt "$resumed" --state starting \
  --idempotency-key transition-one >/dev/null
if $EVENTS --repo "$REPO" transition --attempt "$resumed" --state working \
  --idempotency-key transition-one >/dev/null 2>&1; then
  fail "conflicting idempotency replay was accepted"
fi

# Writers race on one append-only file. Sequence allocation and JSON writes
# must remain intact under the same contention provider councils create.
$EVENTS --repo "$REPO" transition --attempt "$resumed" --state working >/dev/null
pids=""
i=0
while [ "$i" -lt 40 ]; do
  $EVENTS --repo "$REPO" heartbeat --attempt "$resumed" \
    --idempotency-key "heartbeat-$i" >/dev/null &
  pids="$pids $!"
  i=$((i + 1))
done
for pid in $pids; do wait "$pid" || fail "concurrent writer failed"; done

$EVENTS --repo "$REPO" validate >/dev/null || fail "valid lifecycle log was rejected"
if ! python3 - "$REPO/.oms/lifecycle/events.jsonl" "$resumed" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
mine = [r for r in rows if r.get("attempt_id") == sys.argv[2]]
seq = [r["seq"] for r in mine]
assert seq == list(range(1, len(seq) + 1)), seq
assert len({r["event_id"] for r in rows}) == len(rows), "duplicate event ids"
PY
then
  fail "concurrent lifecycle rows are torn or out of sequence"
fi

# Reconciliation only owns live runner phases. Queue/input states must not be
# expired, and each newly stale generation of a resumed attempt is actionable.
RECON_REPO="$TMP/reconcile-repo"
new_repo "$RECON_REPO"
recon_working="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool agent-call \
  --task-id recon-working --run-id recon-run)"
recon_starting="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool agent-call \
  --task-id recon-starting --run-id recon-run)"
recon_queued="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool agent-call \
  --task-id recon-queued --run-id recon-run)"
recon_waiting="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool agent-call \
  --task-id recon-waiting --run-id recon-run)"
recon_verifying="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool peer-delegate \
  --task-id recon-verifying --run-id recon-run)"
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_working" --state starting >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_working" --state working >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_starting" --state starting >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_waiting" --state starting >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_waiting" --state working >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_waiting" --state waiting_input >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_verifying" --state starting >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_verifying" --state working >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_verifying" --state verifying >/dev/null
$EVENTS --repo "$RECON_REPO" reconcile --stale-seconds 0 --apply >/dev/null ||
  fail "stale attempt reconciliation failed"
if ! python3 - "$EVENTS" "$RECON_REPO" "$recon_working" "$recon_starting" \
  "$recon_queued" "$recon_waiting" "$recon_verifying" <<'PY'
import json, subprocess, sys
events, repo = sys.argv[1:3]
expected = dict(zip(
    sys.argv[3:], ["blocked", "blocked", "queued", "waiting_input", "blocked"]
))
for attempt, state in expected.items():
    row = json.loads(subprocess.check_output([
        events, "--repo", repo, "show", "--attempt", attempt, "--json",
    ], text=True))
    assert row["state"] == state, (attempt, row)
PY
then
  fail "reconcile expired a state without heartbeat ownership"
fi
$EVENTS --repo "$RECON_REPO" transition --attempt "$recon_working" --state working >/dev/null
$EVENTS --repo "$RECON_REPO" reconcile --stale-seconds 0 --apply >/dev/null ||
  fail "second stale generation reconciliation failed"
if ! python3 - "$RECON_REPO/.oms/lifecycle/events.jsonl" "$recon_working" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
expired = [row for row in rows if row.get("attempt_id") == sys.argv[2]
           and row.get("reason_code") == "heartbeat_expired"]
assert len(expired) == 2, expired
keys = [row.get("idempotency_key") for row in expired]
assert all(keys) and len(set(keys)) == 2, keys
PY
then
  fail "reconcile idempotency did not distinguish stale generations"
fi

# A heartbeat committed while reconcile waits for the event lock must be part
# of the projection used for the apply decision.
race_attempt="$($EVENTS --repo "$RECON_REPO" start --provider codex --tool agent-call \
  --task-id recon-race --run-id recon-run)"
$EVENTS --repo "$RECON_REPO" transition --attempt "$race_attempt" --state starting >/dev/null
$EVENTS --repo "$RECON_REPO" transition --attempt "$race_attempt" --state working >/dev/null
sleep 2.2
python3 - "$ROOT/scripts/lib/agent-events.py" "$RECON_REPO" "$race_attempt" \
  "$TMP/reconcile-lock-ready" <<'PY' &
import importlib.util
import sys
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location("agent_events", sys.argv[1])
ae = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(ae)
repo = Path(sys.argv[2])
attempt = sys.argv[3]
ready = Path(sys.argv[4])
path = ae.event_path(repo)
with ae.file_lock(path):
    ready.write_text("ready\n", encoding="utf-8")
    time.sleep(2)
    rows = ae.read_rows(path)
    event = ae.new_event(
        attempt, ae.next_seq(rows, attempt), "attempt.heartbeat",
        actor={"kind": "runner", "name": "race-holder"},
        idempotency_key="race-fresh-heartbeat",
    )
    ae.validate_event_row(event)
    ae.append_row(path, event)
PY
holder_pid=$!
ready_checks=0
while [ ! -f "$TMP/reconcile-lock-ready" ] && [ "$ready_checks" -lt 100 ]; do
  sleep 0.05
  ready_checks=$((ready_checks + 1))
done
[ -f "$TMP/reconcile-lock-ready" ] || fail "reconcile race holder did not acquire the lock"
$EVENTS --repo "$RECON_REPO" reconcile --stale-seconds 2 --apply >/dev/null &
reconcile_pid=$!
wait "$holder_pid" || fail "could not append the fresh heartbeat under lock"
wait "$reconcile_pid" || fail "reconcile failed after waiting for event lock"
$EVENTS --repo "$RECON_REPO" show --attempt "$race_attempt" --json > "$TMP/race.json"
python3 - "$TMP/race.json" <<'PY' || fail "reconcile ignored a heartbeat committed under its lock"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "working", row
PY

# Shared lifecycle rows carry identifiers and bounded reason codes, never raw
# commands, prompts, absolute worktree paths, or secret-bearing detail.
if $EVENTS --repo "$REPO" transition --attempt "$resumed" --state blocked \
  --reason-code '/private/machine/path' >/dev/null 2>&1; then
  fail "absolute path entered lifecycle reason_code"
fi
if grep -Fq "$REPO" "$REPO/.oms/lifecycle/events.jsonl"; then
  fail "absolute repository path leaked into lifecycle events"
fi

# Python's JSON parser accepts NaN/Infinity extensions unless callers reject
# them. Approval digests are canonical JSON, so non-finite parameters must be
# refused as a normal contract error rather than producing a traceback.
if $APPROVALS --repo "$REPO" request --action remote-write \
  --object-id invalid-number --summary 'Invalid number' \
  --parameters-json '{"risk":NaN}' >"$TMP/nonfinite.out" 2>"$TMP/nonfinite.err"; then
  fail "non-finite approval parameter was accepted"
fi
grep -Fq 'finite' "$TMP/nonfinite.err" ||
  fail "non-finite approval error is not actionable: $(cat "$TMP/nonfinite.err")"
if grep -Fq 'Traceback' "$TMP/nonfinite.err"; then
  fail "non-finite approval parameter escaped as a traceback"
fi

# Durable approvals use compare-and-set plus a one-time grant. An approved UI
# state alone is not authorization, and replaying the grant must fail closed.
approval="$($APPROVALS --repo "$REPO" request --attempt "$resumed" \
  --action patch-land --object-id patch-one --summary 'Land reviewed patch' \
  --expires-in 120)" || fail "approval request failed"
case "$approval" in apr_*) ;; *) fail "unexpected approval id: $approval" ;; esac

grant="$($APPROVALS --repo "$REPO" decide --approval "$approval" \
  --decision approve --expected-version 1 --actor human)" ||
  fail "approval decision failed"
case "$grant" in grant_*.*) ;; *) fail "approve did not return a one-time grant" ;; esac

if $APPROVALS --repo "$REPO" decide --approval "$approval" \
  --decision reject --expected-version 1 --actor other >/dev/null 2>&1; then
  fail "stale approval decision won a CAS race"
fi

$APPROVALS --repo "$REPO" begin-consume --approval "$approval" --token "$grant" \
  --expected-version 2 --consumer patch-land >/dev/null || fail "valid grant was rejected"
$APPROVALS --repo "$REPO" finish-consume --approval "$approval" \
  --expected-version 3 --result consumed --consumer patch-land >/dev/null ||
  fail "approval consumption could not be finalized"
if $APPROVALS --repo "$REPO" begin-consume --approval "$approval" --token "$grant" \
  --expected-version 4 --consumer replay >/dev/null 2>&1; then
  fail "approval grant replay was accepted"
fi

$APPROVALS --repo "$REPO" show --approval "$approval" --json > "$TMP/approval.json"
python3 - "$TMP/approval.json" <<'PY' || fail "approval projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "consumed", row
assert row["version"] == 4, row
assert "token" not in row and "grant_hash" not in row, row
PY

expired="$($APPROVALS --repo "$REPO" request --action remote-write \
  --object-id remote-one --summary 'Remote mutation' --expires-in 1)"
approval_store="$($APPROVALS --repo "$REPO" path)"
python3 - "$approval_store" "$expired" <<'PY'
import json, os, sys, time
path, target = sys.argv[1:]
rows = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
row = next(item for item in rows if item.get("approval_id") == target)
row["expires_at"] = "2000-01-01T00:00:00Z"
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps({
        "schema": 1, "event_id": "apevt_test_expire", "ts": "2000-01-01T00:00:01Z",
        "approval_id": target, "version": 2, "event_type": "approval.expired",
        "state": "expired", "expected_version": 1,
    }, separators=(",", ":")) + "\n")
PY
if $APPROVALS --repo "$REPO" decide --approval "$expired" \
  --decision approve --expected-version 2 --actor human >/dev/null 2>&1; then
  fail "expired approval was approved"
fi

# Approval expiry also terminalizes an approved grant that was never reserved.
# Otherwise the inbox reports it as pending forever even though begin-consume
# correctly refuses to use it after the deadline.
approved_expired="$($APPROVALS --repo "$REPO" request --action remote-write \
  --object-id remote-approved-expired --summary 'Approved but unused' --expires-in 120)"
approved_expired_grant="$($APPROVALS --repo "$REPO" decide \
  --approval "$approved_expired" --decision approve --expected-version 1 --actor human)"
requested_expired="$($APPROVALS --repo "$REPO" request --action remote-write \
  --object-id remote-requested-expired --summary 'Requested but expired' --expires-in 120)"
python3 - "$approval_store" "$approved_expired" "$requested_expired" <<'PY'
import json, os, sys

path, approved, requested = sys.argv[1:]
rows = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
for row in rows:
    if (row.get("approval_id") in {approved, requested} and
            row.get("event_type") == "approval.requested"):
        row["expires_at"] = "2000-01-01T00:00:00Z"
tmp = path + ".test-rewrite"
with open(tmp, "w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row, separators=(",", ":")) + "\n")
os.chmod(tmp, 0o600)
os.replace(tmp, path)
PY
# Durable history still says approved until expire --apply appends an event,
# but every read path must agree that the unreserved grant is already
# effectively expired and therefore not pending or consumable.
$APPROVALS --repo "$REPO" show --approval "$approved_expired" --json \
  > "$TMP/approved-expired-effective.json"
$APPROVALS --repo "$REPO" show --approval "$approved_expired" \
  > "$TMP/approved-expired-effective.txt"
grep -Fxq 'state/version: approved/2' "$TMP/approved-expired-effective.txt" ||
  fail "approval human durable state/version line changed"
grep -Fxq 'effective state: expired' "$TMP/approved-expired-effective.txt" ||
  fail "approval human output omitted additive effective state"
$APPROVALS --repo "$REPO" list --pending --json \
  > "$TMP/pending-before-expire-apply.json"
$APPROVALS --repo "$REPO" list --state approved --json \
  > "$TMP/durable-approved-before-expire-apply.json"
$APPROVALS --repo "$REPO" list --state requested --json \
  > "$TMP/durable-requested-before-expire-apply.json"
$APPROVALS --repo "$REPO" list --effective-state expired --json \
  > "$TMP/effective-expired-before-apply.json"
$ROOT/scripts/state.sh --repo "$REPO" --json > "$TMP/state-before-expire-apply.json"
$ROOT/scripts/inbox.sh --repo "$REPO" --json > "$TMP/inbox-before-expire-apply.json"
if ! python3 - "$TMP/approved-expired-effective.json" \
  "$TMP/pending-before-expire-apply.json" \
  "$TMP/durable-approved-before-expire-apply.json" \
  "$TMP/durable-requested-before-expire-apply.json" \
  "$TMP/effective-expired-before-apply.json" \
  "$TMP/state-before-expire-apply.json" \
  "$TMP/inbox-before-expire-apply.json" "$approved_expired" "$requested_expired" <<'PY'
import json, sys
show = json.load(open(sys.argv[1], encoding="utf-8"))
pending = json.load(open(sys.argv[2], encoding="utf-8"))
durable_approved = json.load(open(sys.argv[3], encoding="utf-8"))
durable_requested = json.load(open(sys.argv[4], encoding="utf-8"))
expired = json.load(open(sys.argv[5], encoding="utf-8"))
state = json.load(open(sys.argv[6], encoding="utf-8"))["approvals"]
inbox = json.load(open(sys.argv[7], encoding="utf-8"))["items"]
approved, requested = sys.argv[8:]
assert show["state"] == "approved", show
assert show["effective_state"] == "expired", show
assert all(row["approval_id"] not in {approved, requested} for row in pending), pending
assert any(row["approval_id"] == approved for row in durable_approved), durable_approved
assert any(row["approval_id"] == requested for row in durable_requested), durable_requested
assert {row["approval_id"] for row in expired} >= {approved, requested}, expired
assert state["pending"] == 0, state
assert state["effective_expired"] == 2, state
assert state["by_state"].get("approved") == 1, state
assert state["by_state"].get("requested", 0) == 0, state
assert state["by_state"].get("expired") == 2, state
assert state["by_durable_state"].get("approved") == 1, state
assert state["by_durable_state"].get("requested") == 1, state
assert state["by_effective_state"].get("expired") == state["by_durable_state"].get("expired", 0) + 2, state
assert not any(row["code"] == "pending-approval" for row in inbox), inbox
PY
then
  fail "approval read surfaces disagreed before durable expiry was applied"
fi
if $APPROVALS --repo "$REPO" begin-consume --approval "$approved_expired" \
  --token "$approved_expired_grant" --expected-version 2 --consumer late-consumer \
  >/dev/null 2>&1; then
  fail "an effectively expired approved grant was consumable before expire --apply"
fi
$APPROVALS --repo "$REPO" expire --apply >/dev/null ||
  fail "approved-but-unused approval could not be expired"
$APPROVALS --repo "$REPO" show --approval "$approved_expired" --json > "$TMP/approved-expired.json"
python3 - "$TMP/approved-expired.json" <<'PY' || fail "approved expiry projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "expired", row
assert row["version"] == 3, row
PY
if $APPROVALS --repo "$REPO" begin-consume --approval "$approved_expired" \
  --token "$approved_expired_grant" --expected-version 3 --consumer late-consumer \
  >/dev/null 2>&1; then
  fail "expired approved grant was consumable"
fi

# A consuming reservation has an unknown outcome after its consumer
# disappears. Reconcile is an explicit, dry-run-first terminalization: it must
# not guess consumed/failed and must not make the one-time grant reusable.
stale_consuming="$($APPROVALS --repo "$REPO" request --action remote-write \
  --object-id stale-consumer --summary 'Stale consuming effect' --expires-in 120)"
stale_grant="$($APPROVALS --repo "$REPO" decide --approval "$stale_consuming" \
  --decision approve --expected-version 1 --actor human)"
$APPROVALS --repo "$REPO" begin-consume --approval "$stale_consuming" \
  --token "$stale_grant" --expected-version 2 --consumer remote-writer >/dev/null
python3 - "$approval_store" "$stale_consuming" <<'PY'
import json, os, sys

path, target = sys.argv[1:]
rows = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
for row in rows:
    if row.get("approval_id") == target and row.get("event_type") == "approval.consuming":
        row["ts"] = "2000-01-01T00:00:00Z"
tmp = path + ".test-rewrite"
with open(tmp, "w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row, separators=(",", ":")) + "\n")
os.chmod(tmp, 0o600)
os.replace(tmp, path)
PY
$APPROVALS --repo "$REPO" reconcile --older-than-seconds 60 --json > "$TMP/reconcile-dry.json" ||
  fail "approval reconcile dry-run failed"
$APPROVALS --repo "$REPO" show --approval "$stale_consuming" --json > "$TMP/stale-before-apply.json"
python3 - "$TMP/reconcile-dry.json" "$TMP/stale-before-apply.json" "$stale_consuming" \
  <<'PY' || fail "approval reconcile was not a dry run"
import json, sys
candidates = json.load(open(sys.argv[1], encoding="utf-8"))
row = json.load(open(sys.argv[2], encoding="utf-8"))
target = sys.argv[3]
candidate = next(item for item in candidates if item["approval_id"] == target)
assert candidate["state"] == "consuming", candidate
assert candidate["outcome"] == "unknown", candidate
assert row["state"] == "consuming" and row["version"] == 3, row
PY
if $APPROVALS --repo "$REPO" reconcile --older-than-seconds 0 >/dev/null 2>&1; then
  fail "approval reconcile accepted an unbounded zero age"
fi
if $APPROVALS --repo "$REPO" reconcile --older-than-seconds 604801 >/dev/null 2>&1; then
  fail "approval reconcile accepted an excessive age"
fi
$APPROVALS --repo "$REPO" reconcile --older-than-seconds 60 --apply >/dev/null ||
  fail "stale consuming approval could not be terminalized"
$APPROVALS --repo "$REPO" show --approval "$stale_consuming" --json > "$TMP/stale-interrupted.json"
python3 - "$TMP/stale-interrupted.json" <<'PY' || fail "interrupted approval projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "interrupted", row
assert row["version"] == 4, row
assert row["outcome"] == "unknown", row
PY
if $APPROVALS --repo "$REPO" finish-consume --approval "$stale_consuming" \
  --expected-version 4 --result consumed --consumer late-recovery >/dev/null 2>&1; then
  fail "interrupted unknown-outcome approval was rewritten as consumed"
fi
if $APPROVALS --repo "$REPO" begin-consume --approval "$stale_consuming" \
  --token "$stale_grant" --expected-version 4 --consumer replay >/dev/null 2>&1; then
  fail "interrupted approval grant became reusable"
fi
$APPROVALS --repo "$REPO" reconcile --older-than-seconds 60 --apply --json \
  > "$TMP/reconcile-repeat.json" || fail "approval reconcile repeat failed"
$APPROVALS --repo "$REPO" list --pending --json > "$TMP/pending-after-reconcile.json"
python3 - "$TMP/reconcile-repeat.json" "$TMP/pending-after-reconcile.json" \
  "$stale_consuming" <<'PY' || fail "approval reconcile did not converge"
import json, sys
repeat = json.load(open(sys.argv[1], encoding="utf-8"))
pending = json.load(open(sys.argv[2], encoding="utf-8"))
target = sys.argv[3]
assert repeat == [], repeat
assert all(row["approval_id"] != target for row in pending), pending
PY

# patch-land has a stronger domain recovery contract: its durable intent and
# the Git tree can determine whether the effect happened. Generic unknown-
# outcome reconciliation must defer to that recovery instead of terminalizing
# the reservation first and preventing a precise consumed/failed receipt.
landing_consuming="$($APPROVALS --repo "$REPO" request --action patch-land \
  --object-id patch:recovery-test --summary 'Recover exact patch landing' --expires-in 120)"
landing_grant="$($APPROVALS --repo "$REPO" decide --approval "$landing_consuming" \
  --decision approve --expected-version 1 --actor human)"
$APPROVALS --repo "$REPO" begin-consume --approval "$landing_consuming" \
  --token "$landing_grant" --expected-version 2 --consumer patch-land >/dev/null
python3 - "$approval_store" "$landing_consuming" <<'PY'
import json, os, sys

path, target = sys.argv[1:]
rows = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
for row in rows:
    if row.get("approval_id") == target and row.get("event_type") == "approval.consuming":
        row["ts"] = "2000-01-01T00:00:00Z"
tmp = path + ".test-rewrite"
with open(tmp, "w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row, separators=(",", ":")) + "\n")
os.chmod(tmp, 0o600)
os.replace(tmp, path)
PY
$APPROVALS --repo "$REPO" reconcile --older-than-seconds 60 --apply --json \
  > "$TMP/landing-reconcile.json" || fail "patch-land approval reconcile failed"
$APPROVALS --repo "$REPO" show --approval "$landing_consuming" --json \
  > "$TMP/landing-after-reconcile.json"
python3 - "$TMP/landing-reconcile.json" "$TMP/landing-after-reconcile.json" \
  "$landing_consuming" <<'PY' || fail "generic reconcile preempted patch-land recovery"
import json, sys
candidates = json.load(open(sys.argv[1], encoding="utf-8"))
row = json.load(open(sys.argv[2], encoding="utf-8"))
target = sys.argv[3]
candidate = next(item for item in candidates if item["approval_id"] == target)
assert candidate["recovery"] == "patch-land", candidate
assert row["state"] == "consuming" and row["version"] == 3, row
PY
$APPROVALS --repo "$REPO" finish-consume --approval "$landing_consuming" \
  --expected-version 3 --result consumed --consumer patch-land-recovery >/dev/null ||
  fail "patch-land recovery could not finish after generic reconcile"

$APPROVALS --repo "$REPO" validate >/dev/null || fail "valid approval log was rejected"

# --- compact: long-terminal attempts leave the stream; live ones never do ---
compact_repo="$TMP/compact-repo"
new_repo "$compact_repo"
dead="$($EVENTS --repo "$compact_repo" start --provider codex --tool agent-call)" ||
  fail "compact fixture: could not create the terminal attempt"
$EVENTS --repo "$compact_repo" transition --attempt "$dead" --state starting >/dev/null
$EVENTS --repo "$compact_repo" transition --attempt "$dead" --state working >/dev/null
$EVENTS --repo "$compact_repo" transition --attempt "$dead" --state failed >/dev/null
live="$($EVENTS --repo "$compact_repo" start --provider codex --tool agent-call)" ||
  fail "compact fixture: could not create the live attempt"
# The cutoff has one-second granularity: the terminal attempt must be
# strictly older than "now" before --days 0 may see it.
sleep 2
out="$($EVENTS --repo "$compact_repo" compact --days 0)" || fail "compact dry-run failed"
printf '%s' "$out" | grep -Fq 'would drop 4 event(s) across 1 terminal attempt(s)' ||
  fail "compact dry-run should name the drop: $out"
[ "$(wc -l < "$compact_repo/.oms/lifecycle/events.jsonl")" -eq 5 ] ||
  fail "a dry run must not touch the stream"
out="$($EVENTS --repo "$compact_repo" compact --days 0 --apply)" || fail "compact apply failed"
printf '%s' "$out" | grep -Fq 'dropped 4 event(s)' || fail "compact apply should report the drop: $out"
$EVENTS --repo "$compact_repo" validate >/dev/null || fail "the compacted stream must still validate"
$EVENTS --repo "$compact_repo" show --attempt "$live" >/dev/null 2>&1 ||
  fail "the live attempt must survive compaction"
if $EVENTS --repo "$compact_repo" show --attempt "$dead" >/dev/null 2>&1; then
  fail "the terminal attempt must be gone after compaction"
fi

# start --then appends the routed opening rows in the creating process: the
# projection reads exactly what start + two transitions produced, and a replay
# with the same keys is idempotent rather than a second opening.
batched="$($EVENTS --repo "$REPO" start --provider codex --tool agent-call \
  --idempotency-key batched-open --then starting:routed-starting \
  --then working:routed-working --then-actor provider-router)" ||
  fail "batched start failed"
$EVENTS --repo "$REPO" show --attempt "$batched" --json > "$TMP/batched.json"
python3 - "$TMP/batched.json" <<'PY' || fail "batched start projection is wrong"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "working", row
assert row["sequence"] == 3, row
PY
replay="$($EVENTS --repo "$REPO" start --provider codex --tool agent-call \
  --idempotency-key batched-open --then starting:routed-starting \
  --then working:routed-working --then-actor provider-router)" ||
  fail "batched start replay failed"
[ "$replay" = "$batched" ] || fail "batched start replay minted a second attempt"
$EVENTS --repo "$REPO" show --attempt "$batched" --json > "$TMP/batched2.json"
python3 - "$TMP/batched2.json" <<'PY' || fail "batched start replay appended rows again"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["state"] == "working" and row["sequence"] == 3, row
PY
if $EVENTS --repo "$REPO" start --provider codex --tool agent-call \
    --then nonsense:key >/dev/null 2>&1; then
  fail "start --then accepted an invalid lifecycle state"
fi

echo "lifecycle-events-smoke: ok"
