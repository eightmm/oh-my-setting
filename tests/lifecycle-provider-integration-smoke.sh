#!/usr/bin/env bash
set -euo pipefail
# A delegated verify runs this suite one delegate level deep; its fixtures
# need a top-level delegate so each round opens its own attempt.
unset OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-provider-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "lifecycle-provider-integration: $*" >&2; exit 1; }

mkdir -p "$TMP/bin" "$TMP/home" "$TMP/state" "$TMP/locks" "$TMP/repo"
git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.email test@example.com
git -C "$TMP/repo" config user.name test
printf 'base\n' > "$TMP/repo/README.md"
git -C "$TMP/repo" add README.md
git -C "$TMP/repo" commit -qm base

cat > "$TMP/bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
if [ "${1:-}" = exec ] && [ -n "${OMS_TEST_OPERATOR_BLOCK:-}" ]; then
  # The read worker correctly receives no owner's attempt capability. This
  # fixture simulates an independent operator using the parent's local ledger.
  attempt="$(python3 - "$OMS_TEST_OPERATOR_BLOCK/.oms/lifecycle/events.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    rows = [json.loads(line) for line in handle if line.strip()]
print([row["attempt_id"] for row in rows if row["event_type"] == "attempt.created"][-1])
PY
)"
  "$OMS_TEST_ROOT/scripts/agent-events.sh" --repo "$OMS_TEST_OPERATOR_BLOCK" \
    transition --attempt "$attempt" --state blocked \
    --actor operator --reason-code operator_hold >/dev/null || exit 1
fi
if [ "${1:-}" = exec ] && [ -n "${OMS_TEST_RECONCILE_REPO:-}" ]; then
  python3 - "$OMS_TEST_RECONCILE_REPO/.oms/lifecycle/events.jsonl" <<'PY'
import json, sys, time
deadline = time.monotonic() + 8
while time.monotonic() < deadline:
    with open(sys.argv[1], encoding="utf-8") as handle:
        rows = [json.loads(line) for line in handle if line.strip()]
    if sum(row["event_type"] == "attempt.heartbeat" for row in rows) >= 2:
        break
    time.sleep(.05)
PY
  "$OMS_TEST_ROOT/scripts/agent-events.sh" --repo "$OMS_TEST_RECONCILE_REPO" \
    reconcile --apply --stale-seconds 2 >/dev/null
fi
[ "${1:-}" != exec ] || [ -z "${OMS_TEST_PROVIDER_SLEEP:-}" ] || sleep "$OMS_TEST_PROVIDER_SLEEP"
printf 'tokens used\n3\n'
printf 'provider answer\n'
printf 'tokens used\n17\n'
EOF
chmod +x "$TMP/bin/codex"

env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_ROUTED_HEARTBEAT_SECONDS=1 OMS_TEST_RECONCILE_REPO="$TMP/repo" OMS_TEST_ROOT="$ROOT" \
  "$ROOT/scripts/agent-call.sh" --repo "$TMP/repo" --to codex \
  --prompt 'lifecycle probe' >/dev/null || fail "agent-call failed"

"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json > "$TMP/attempts.json"
python3 - "$TMP/attempts.json" "$TMP/repo/.oms/artifacts/index.jsonl" <<'PY' || fail "provider attempt or artifact lineage is incomplete"
import json, sys

attempts = json.load(open(sys.argv[1], encoding="utf-8"))
assert len(attempts) == 1, attempts
attempt = attempts[0]
assert attempt["provider"] == "codex", attempt
assert attempt["tool"] == "call", attempt
assert attempt["state"] == "done", attempt
assert attempt["usage"]["tokens"] == 20, attempt
assert attempt["usage_reports"]["duration_ms"] == 1, attempt

rows = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert rows[-1]["attempt_id"] == attempt["attempt_id"], rows[-1]
events = [json.loads(line) for line in open(sys.argv[2].replace("artifacts/index.jsonl", "lifecycle/events.jsonl"), encoding="utf-8") if line.strip()]
routed = [(e["to_state"], e.get("idempotency_key")) for e in events
          if e.get("event_type") == "attempt.state_changed" and e.get("actor", {}).get("name") == "provider-router"]
assert routed[-3:] == [("verifying", "routed-verifying"), ("review", "routed-review"), ("done", "routed-done")], routed
own_events = [row for row in events if row["attempt_id"] == attempt["attempt_id"]]
assert sum(row["event_type"] == "attempt.heartbeat" for row in own_events) >= 2, own_events
assert not any(row.get("reason_code") == "heartbeat_expired" for row in own_events), own_events
PY

# A supervisor owns its outer lifecycle. A provider invoked inside it reports
# usage to that attempt and must not create or terminalize a nested attempt.
outer="$($ROOT/scripts/agent-events.sh --repo "$TMP/repo" start \
  --provider codex --tool agent-supervisor)"
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition \
  --attempt "$outer" --state starting >/dev/null
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition \
  --attempt "$outer" --state working >/dev/null
env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" OMS_ATTEMPT_ID="$outer" OMS_ATTEMPT_SUPERVISED=1 \
  OMS_ROUTED_HEARTBEAT_SECONDS=1 OMS_TEST_PROVIDER_SLEEP=2 \
  PATH="$TMP/bin:/usr/bin:/bin" "$ROOT/scripts/agent-call.sh" \
  --repo "$TMP/repo" --to codex --prompt 'nested lifecycle probe' >/dev/null ||
  fail "nested agent-call failed"

"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" show \
  --attempt "$outer" --json > "$TMP/outer.json"
python3 - "$TMP/outer.json" "$TMP/repo/.oms/artifacts/index.jsonl" <<'PY' || fail "supervisor-owned lifecycle was not preserved"
import json, sys

outer = json.load(open(sys.argv[1], encoding="utf-8"))
assert outer["state"] == "working", outer
with open(sys.argv[2].replace("artifacts/index.jsonl", "lifecycle/events.jsonl"), encoding="utf-8") as handle:
    events = [json.loads(line) for line in handle if line.strip()]
assert not any(row["attempt_id"] == outer["attempt_id"] and row["event_type"] == "attempt.heartbeat"
               for row in events), events
assert outer["usage"]["tokens"] == 20, outer
assert outer["usage"]["duration_ms"] == 0, outer
assert outer["usage_reports"]["duration_ms"] == 0, outer
rows = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert rows[-1]["attempt_id"] == outer["attempt_id"], rows[-1]
PY

"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition \
  --attempt "$outer" --state waiting_approval >/dev/null
pending="$($ROOT/scripts/approval-inbox.sh --repo "$TMP/repo" request \
  --attempt "$outer" --action patch-land --object-id patch:pending \
  --summary 'Review pending patch')"
"$ROOT/scripts/state.sh" --repo "$TMP/repo" --json > "$TMP/state.json"
"$ROOT/scripts/inbox.sh" --repo "$TMP/repo" --json > "$TMP/inbox.json"
python3 - "$TMP/state.json" "$TMP/inbox.json" "$pending" <<'PY' || fail "operation state is absent from shared views"
import json, sys
state = json.load(open(sys.argv[1], encoding="utf-8"))
inbox = json.load(open(sys.argv[2], encoding="utf-8"))
assert state["agent_operations"]["by_state"]["done"] == 1, state
assert state["agent_operations"]["by_state"]["waiting_approval"] == 1, state
assert state["approvals"]["pending"] == 1, state
assert state["approvals"]["latest_pending"][0]["approval_id"] == sys.argv[3], state
codes = {item["code"] for item in inbox["items"]}
assert "agent-waiting-approval" in codes, inbox
assert "pending-approval" in codes, inbox
PY

# A direct write attempt belongs to the whole delegated worker round, not just
# the provider subprocess. Provider exit 0 must not become review before the
# declared verifier finishes, and a verifier failure must terminalize the
# attempt instead of leaving false-success lifecycle evidence.
if env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'write lifecycle verify failure' --verify false \
  >/dev/null 2>&1; then
  fail "delegation with a failing verifier succeeded"
fi
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json \
  > "$TMP/write-failed-attempts.json"
python3 - "$TMP/write-failed-attempts.json" "$TMP/repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "failed write lifecycle reported false success"
import json, sys

attempts = json.load(open(sys.argv[1], encoding="utf-8"))
writes = [row for row in attempts if row.get("tool") == "peer-delegate"]
assert len(writes) == 1, writes
attempt = writes[0]
assert attempt["state"] == "failed", attempt
assert attempt["reason_code"] == "verification_failed", attempt
events = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
states = [row.get("to_state") for row in events if row.get("attempt_id") == attempt["attempt_id"]]
assert states[-2:] == ["verifying", "failed"], states
assert "review" not in states, states
PY

env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'write lifecycle review' --no-verify >/dev/null ||
  fail "unverified delegation failed"
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json \
  > "$TMP/write-review-attempts.json"
python3 - "$TMP/write-review-attempts.json" "$TMP/repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "unverified write lifecycle skipped its review boundary"
import json, sys

attempts = json.load(open(sys.argv[1], encoding="utf-8"))
writes = [row for row in attempts if row.get("tool") == "peer-delegate"]
assert len(writes) == 2, writes
attempt = writes[-1]
assert attempt["state"] == "review", attempt
events = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
states = [row.get("to_state") for row in events if row.get("attempt_id") == attempt["attempt_id"]]
assert states[-1] == "review" and "verifying" not in states, states
PY

# verify-heartbeat keeps a write attempt fresh while reconciliation runs
# concurrently beyond the tiny stale threshold.
cat > "$TMP/verify-heartbeat.sh" <<'EOF'
#!/usr/bin/env bash
set -eu
: > "$OMS_TEST_VERIFY_STARTED"
while [ ! -f "$OMS_TEST_VERIFY_RELEASE" ]; do sleep .05; done
EOF
chmod +x "$TMP/verify-heartbeat.sh"
verify_started="$TMP/verify-heartbeat.started"
verify_release="$TMP/verify-heartbeat.release"
env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_ROUTED_HEARTBEAT_SECONDS=0.2 OMS_TEST_VERIFY_STARTED="$verify_started" \
  OMS_TEST_VERIFY_RELEASE="$verify_release" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'verify-heartbeat stale reconciliation' \
  --verify "bash '$TMP/verify-heartbeat.sh'" > "$TMP/verify-heartbeat.out" 2>&1 &
verify_delegate_pid=$!
for _verify_wait in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40; do
  [ -f "$verify_started" ] && break
  sleep .05
done
[ -f "$verify_started" ] || fail "verify-heartbeat verifier did not reach its barrier"
(
  for _reconcile_round in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    "$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" reconcile --apply \
      --stale-seconds 2 >/dev/null || exit 1
    sleep .15
  done
  : > "$verify_release"
) &
reconcile_verify_pid=$!
if ! wait "$reconcile_verify_pid"; then
  kill "$verify_delegate_pid" 2>/dev/null || true
  wait "$verify_delegate_pid" 2>/dev/null || true
  fail "verify-heartbeat concurrent reconciliation failed"
fi
if ! wait "$verify_delegate_pid"; then
  cat "$TMP/verify-heartbeat.out" >&2
  fail "verify-heartbeat passing verification failed after reconciliation"
fi
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json \
  > "$TMP/verify-heartbeat-attempts.json"
python3 - "$TMP/verify-heartbeat-attempts.json" "$TMP/repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "verify-heartbeat did not end in review"
import json, sys
attempts = json.load(open(sys.argv[1], encoding="utf-8"))
writes = [row for row in attempts if row.get("tool") == "peer-delegate"]
attempt = writes[-1]
assert attempt["state"] == "review", attempt
events = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
own = [row for row in events if row["attempt_id"] == attempt["attempt_id"]]
assert any(row["event_type"] == "attempt.heartbeat" for row in own), own
assert not any(row.get("reason_code") == "heartbeat_expired" for row in own), own
PY

# If a stale decision wins exactly at verifier completion, the passing verify
# may recover only that heartbeat-expired generation through existing edges.
verify_started="$TMP/verify-heartbeat-race.started"
verify_release="$TMP/verify-heartbeat-race.release"
env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_ROUTED_HEARTBEAT_SECONDS=0.2 OMS_TEST_VERIFY_STARTED="$verify_started" \
  OMS_TEST_VERIFY_RELEASE="$verify_release" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'verify-heartbeat completion race' \
  --verify "bash '$TMP/verify-heartbeat.sh'" > "$TMP/verify-heartbeat-race.out" 2>&1 &
race_delegate_pid=$!
for _verify_wait in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40; do
  [ -f "$verify_started" ] && break
  sleep .05
done
[ -f "$verify_started" ] || fail "verify-heartbeat race verifier did not reach its barrier"
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" reconcile --apply \
  --stale-seconds 0 >/dev/null || fail "verify-heartbeat forced stale decision failed"
: > "$verify_release"
if ! wait "$race_delegate_pid"; then
  cat "$TMP/verify-heartbeat-race.out" >&2
  fail "verify-heartbeat completion race did not recover"
fi
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json \
  > "$TMP/verify-heartbeat-race-attempts.json"
python3 - "$TMP/verify-heartbeat-race-attempts.json" "$TMP/repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "verify-heartbeat completion race did not use the expected recovery"
import json, sys
attempts = json.load(open(sys.argv[1], encoding="utf-8"))
attempt = [row for row in attempts if row.get("tool") == "peer-delegate"][-1]
assert attempt["state"] == "review", attempt
events = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
own = [row for row in events if row["attempt_id"] == attempt["attempt_id"]]
states = [row.get("to_state") for row in own if row["event_type"] == "attempt.state_changed"]
assert "heartbeat_expired" in [row.get("reason_code") for row in own], own
assert states[-4:] == ["blocked", "working", "verifying", "review"], states
PY

# The heartbeat recovery path must leave a concurrent operator block intact.
verify_started="$TMP/verify-heartbeat-operator.started"
verify_release="$TMP/verify-heartbeat-operator.release"
env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_ROUTED_HEARTBEAT_SECONDS=0.2 OMS_TEST_VERIFY_STARTED="$verify_started" \
  OMS_TEST_VERIFY_RELEASE="$verify_release" \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'verify-heartbeat operator block' \
  --verify "bash '$TMP/verify-heartbeat.sh'" > "$TMP/verify-heartbeat-operator.out" 2>&1 &
operator_delegate_pid=$!
for _verify_wait in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40; do
  [ -f "$verify_started" ] && break
  sleep .05
done
[ -f "$verify_started" ] || fail "verify-heartbeat operator verifier did not reach its barrier"
operator_attempt="$("$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json | \
  python3 -c 'import json,sys; print([row["attempt_id"] for row in json.load(sys.stdin) if row.get("tool") == "peer-delegate"][-1])')"
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition --attempt "$operator_attempt" \
  --state blocked --actor operator --reason-code operator_hold >/dev/null ||
  fail "operator block could not be recorded during verification"
: > "$verify_release"
if wait "$operator_delegate_pid"; then
  fail "verify-heartbeat operator block was reported as successful"
fi
python3 -c 'import json,sys; rows=[json.loads(x) for x in open(sys.argv[1]) if x.strip()]; own=[r for r in rows if r["attempt_id"]==sys.argv[2]]; states=[r.get("to_state") for r in own if r["event_type"]=="attempt.state_changed"]; assert any(r.get("reason_code")=="operator_hold" for r in own), own; assert states[-1] != "review", states' \
  "$TMP/repo/.oms/lifecycle/events.jsonl" "$operator_attempt" ||
  fail "verify-heartbeat recovery overrode operator block"

# A successful provider response cannot revive an explicit operator block.
if env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_TEST_OPERATOR_BLOCK="$TMP/repo" OMS_TEST_ROOT="$ROOT" \
  OMS_ROUTED_HEARTBEAT_SECONDS=00 \
  "$ROOT/scripts/agent-call.sh" --repo "$TMP/repo" --to codex \
  --prompt 'operator blocked read' > "$TMP/operator-block.out" 2>&1; then
  fail "successful provider revived an operator block"
fi
"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" list --json > "$TMP/operator-block.json"
python3 - "$TMP/operator-block.json" <<'PY' || fail "operator block was not preserved"
import json, sys
attempts = json.load(open(sys.argv[1]))
assert attempts[-1]["state"] == "blocked", attempts[-1]
PY

# The aggregate verifier must include both the shared lifecycle stream and the
# private approval store. Corruption in either cannot be hidden behind the
# older run/artifact validators.
printf '{}\n' >> "$TMP/repo/.oms/lifecycle/events.jsonl"
approval_store="$($ROOT/scripts/approval-inbox.sh --repo "$TMP/repo" path)"
mkdir -p "$(dirname "$approval_store")"
printf '{}\n' > "$approval_store"
if "$ROOT/scripts/state-verify.sh" --repo "$TMP/repo" --json > "$TMP/verify.json" 2>/dev/null; then
  fail "state-verify accepted corrupt lifecycle and approval streams"
fi
python3 - "$TMP/verify.json" <<'PY' || fail "state-verify omitted lifecycle findings"
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
families = {item["family"] for item in row["findings"]}
assert "lifecycle" in families, row
assert "approvals" in families, row
assert row["delegated"]["lifecycle_exit"] != 0, row
assert row["delegated"]["approval_exit"] != 0, row
PY

echo "lifecycle-provider-integration: ok"
