#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-provider-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "lifecycle-provider-integration: $*" >&2; exit 1; }

mkdir -p "$TMP/bin" "$TMP/home" "$TMP/state" "$TMP/locks" "$TMP/repo"
export HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" OMS_LOCK_DIR="$TMP/locks"
export XDG_CACHE_HOME="$TMP/home/.cache" XDG_CONFIG_HOME="$TMP/home/.config"
git -C "$TMP/repo" init -q
git -C "$TMP/repo" config user.email test@example.com
git -C "$TMP/repo" config user.name test
printf 'base\n' > "$TMP/repo/README.md"
git -C "$TMP/repo" add README.md
git -C "$TMP/repo" commit -qm base

cat > "$TMP/bin/codex" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = exec ] && [ -n "${OMS_TEST_MAIL_PROMPT:-}" ]; then
  cat > "$OMS_TEST_MAIL_PROMPT"
  printf '%s\n' "$@" > "$OMS_TEST_MAIL_PROMPT.argv"
  [ -z "${OMS_ROOM_RELAY_QUEUE:-}${OMS_ROOM_RELAY_ALLOWED:-}${OMS_ATTEMPT_ID:-}" ] || exit 98
else
  cat >/dev/null
fi
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
[ "${1:-}" != exec ] || exit "${OMS_TEST_MAIL_EXIT:-0}"
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

# A read participant never gets a relay or write sandbox. Only its parent
# records receipts after success; failure leaves the addressed mail pending.
mkdir -p "$TMP/mail-repo"
git -C "$TMP/mail-repo" init -q
git -C "$TMP/mail-repo" -c user.name=test -c user.email=test@example.com commit --allow-empty -qm base
python3 - "$ROOT" "$TMP/mail-repo" <<'PY'
import pathlib, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]) / "scripts/lib"))
import room
repo = pathlib.Path(sys.argv[2])
room.create(repo, "read-mail", "Read call fixture")
room.join(repo, "read-mail", "main", "codex")
room.join(repo, "read-mail", "reader", "codex", "reviewer", parent="main")
room.join(repo, "read-mail", "other", "claude", "researcher", parent="main")
room.send(repo, "read-mail", "main", "reader", "Assignment ZEBRA-7342", message_id="assignment")
room.send(repo, "read-mail", "main", "other", "Other assignment", message_id="other-assignment")
PY
mail_python="$(command -v python3)"
for mail_case in failed blocked ack-failed capture-failed "done" delegate; do
  mail_exit=0 mail_block="" mail_fail_op=complete
  rm -f "$TMP/mail-prompt" "$TMP/mail-prompt.argv"
  mail_script=agent-call
  mail_options=()
  [ "$mail_case" != failed ] || mail_exit=7
  [ "$mail_case" != blocked ] || mail_block="$TMP/mail-repo"
  [ "$mail_case" != capture-failed ] || mail_fail_op=capture
  if [ "$mail_case" = ack-failed ] || [ "$mail_case" = capture-failed ]; then
    cat > "$TMP/bin/python3" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = - ] && [ "${3:-}" = "$OMS_TEST_MAIL_FAIL_OP" ]; then
  cat >/dev/null
  echo 'forced receipt failure' >&2
  exit 2
fi
exec "$OMS_TEST_REAL_PYTHON" "$@"
EOF
    chmod +x "$TMP/bin/python3"
  fi
  if [ "$mail_case" = delegate ]; then
    mail_script=peer-delegate
    mail_options=(--read-only --no-verify)
    "$ROOT/scripts/room.sh" send --repo "$TMP/mail-repo" --id read-mail --participant main \
      --to reader --text 'Delegate assignment KOALA-19' --message-id delegate-assignment >/dev/null
  fi
  mail_status=0
  env -u NVM_BIN -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
    OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
    OMS_ROOM_ID=read-mail OMS_ROOM_PARTICIPANT=reader OMS_ROOM_ADMITTED_PARTICIPANT=reader \
    OMS_ROOM_RELAY_QUEUE="$TMP/inherited-relay" OMS_ROOM_RELAY_ALLOWED=1 \
    OMS_TEST_OPERATOR_BLOCK="$mail_block" OMS_TEST_ROOT="$ROOT" \
    OMS_TEST_REAL_PYTHON="$mail_python" OMS_TEST_MAIL_FAIL_OP="$mail_fail_op" \
    OMS_ROOM_REPO="$TMP/mail-repo" OMS_TEST_MAIL_PROMPT="$TMP/mail-prompt" OMS_TEST_MAIL_EXIT="$mail_exit" \
    "$ROOT/scripts/$mail_script.sh" --repo "$TMP/mail-repo" --to codex ${mail_options[@]+"${mail_options[@]}"} \
    --prompt 'read mail completion' > "$TMP/mail-output" 2>&1 || mail_status=$?
  rm -f "$TMP/bin/python3"
  if [ "$mail_case" = capture-failed ]; then
    # Unreadable mail must stop the call before the provider runs.
    [ "$mail_status" != 0 ] && grep -Fq 'forced receipt failure' "$TMP/mail-output" ||
      fail "mail capture failure did not stop the call"
    [ ! -e "$TMP/mail-prompt" ] || fail "provider ran without its room mail"
  fi
  if [ "$mail_case" = ack-failed ]; then
    [ "$mail_status" = 2 ] && grep -Fq 'forced receipt failure' "$TMP/mail-output" ||
      fail "receipt failure was not propagated"
    "$ROOT/scripts/agent-events.sh" --repo "$TMP/mail-repo" list --json > "$TMP/mail-attempts.json"
    python3 - "$TMP/mail-attempts.json" "$TMP/mail-repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "receipt failure recorded false success"
import json, sys
attempt = json.load(open(sys.argv[1], encoding="utf-8"))[-1]
assert attempt["state"] == "failed", attempt
events = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8")]
states = [row.get("to_state") for row in events if row.get("attempt_id") == attempt["attempt_id"]]
assert "done" not in states and states[-1] == "failed", states
PY
  fi
  if { [ "$mail_case" != ack-failed ] && [ "$mail_case" != capture-failed ] && [ "$mail_exit" = 0 ] && [ -z "$mail_block" ] && [ "$mail_status" != 0 ]; } ||
    { { [ "$mail_exit" != 0 ] || [ -n "$mail_block" ]; } && [ "$mail_status" = 0 ]; }; then
    cat "$TMP/mail-output" >&2
    fail "read mail exit is incorrect ($mail_case -> $mail_status)"
  fi
  if [ "$mail_case" != capture-failed ]; then
    grep -Fxq read-only "$TMP/mail-prompt.argv" || fail "read call lost its read-only sandbox"
    if grep -Eq 'oms room (send|ack)' "$TMP/mail-prompt"; then fail "read call was instructed to write mail"; fi
    case "$mail_case" in
      done) grep -Fq 'ZEBRA-7342' "$TMP/mail-prompt" || fail "read call receipt names mail its prompt never carried" ;;
      delegate) grep -Fq 'KOALA-19' "$TMP/mail-prompt" || fail "read delegate receipt names mail its prompt never carried" ;;
    esac
    if grep -Fq 'Other assignment' "$TMP/mail-prompt"; then fail "read call carried another participant's mail"; fi
  fi
  python3 - "$ROOT" "$TMP/mail-repo" "$mail_case" <<'PY' || fail "read call receipt is missing or overbroad"
import pathlib, sys
sys.path.insert(0, str(pathlib.Path(sys.argv[1]) / "scripts/lib"))
import room
mail = {m["id"]: m for m in room.messages(pathlib.Path(sys.argv[2]), "read-mail")}
assert ("reader" in mail["assignment"]["received_by"]) == (sys.argv[3] in ("done", "delegate")), mail
if sys.argv[3] == "delegate":
    assert mail["delegate-assignment"]["received_by"] == ["reader"], mail
assert mail["other-assignment"]["received_by"] == [], mail
PY
done

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

# A structurally valid JSON resolution with missing lineage is still damage,
# even when no outcome contributes to the unresolved count.
mkdir -p "$TMP/artifact-health/.oms/artifacts"
git -C "$TMP/artifact-health" init -q
printf '%s\n' '{"schema":1,"kind":"artifact-resolution","event_id":"evt_orphan","parent_event_id":"evt_missing","resolves_event_id":"evt_missing","resolution":"resolved","operation_id":"op_health","artifact_id":"art_health","exit":0}' > "$TMP/artifact-health/.oms/artifacts/index.jsonl"
"$ROOT/scripts/state.sh" --repo "$TMP/artifact-health" --json > "$TMP/artifact-health-state.json"
"$ROOT/scripts/inbox.sh" --repo "$TMP/artifact-health" --json > "$TMP/artifact-health-inbox.json"
python3 - "$ROOT" "$TMP/artifact-health-state.json" "$TMP/artifact-health-inbox.json" <<'PYHEALTH' || fail "inbox hid semantic artifact corruption"
import json, runpy, sys
from pathlib import Path
root = Path(sys.argv[1])
state, inbox = [json.loads(Path(path).read_bytes()) for path in sys.argv[2:]]
assert not state['artifacts']['healthy'] and state['artifacts']['invalid_rows'] > 0, state
assert state['artifacts']['counts']['unresolved'] == 0, state
item = next(item for item in inbox['items'] if item['code'] == 'artifact-store-corrupt')
assert 'validate' in item['command'] and item['command'].endswith('salvage'), item
project = runpy.run_path(str(root / 'scripts/lib/inbox_projection.py'))['project_inbox']
report = project(dict(artifacts=state['artifacts'], runtime=dict(healthy=True)))
assert [item['code'] for item in report['items']] == ['artifact-store-corrupt'], report
assert project(dict(artifacts=dict(healthy=True, invalid_rows=0), runtime=dict(healthy=True)))['actionable'] == 0
PYHEALTH

review_attempt="$(env HOME="$TMP/home" OMS_LOCK_DIR="$TMP/locks" "$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" start --provider codex --tool review-fixture)"
for review_state in starting working verifying review; do
  env HOME="$TMP/home" OMS_LOCK_DIR="$TMP/locks" "$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition \
    --attempt "$review_attempt" --state "$review_state" >/dev/null
done
"$ROOT/scripts/state.sh" --repo "$TMP/repo" --json > "$TMP/review-state.json"
"$ROOT/scripts/inbox.sh" --repo "$TMP/repo" --json > "$TMP/review-inbox.json"
env OMS_REVIEW_AGED_DAYS=0 "$ROOT/scripts/state.sh" --repo "$TMP/repo" --json > "$TMP/aged-review-state.json"
env OMS_REVIEW_AGED_DAYS=0 "$ROOT/scripts/inbox.sh" --repo "$TMP/repo" --json > "$TMP/aged-review-inbox.json"
env OMS_REVIEW_AGED_DAYS=invalid "$ROOT/scripts/state.sh" --repo "$TMP/repo" > "$TMP/invalid-review-days.txt"
cp "$TMP/repo/.oms/lifecycle/events.jsonl" "$TMP/lifecycle-events.backup"
python3 - "$TMP/repo/.oms/lifecycle/events.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], "a", encoding="utf-8") as handle:
    handle.write(json.dumps({"event_type": "attempt.state_changed", "attempt_id": [], "to_state": "review",
                             "ts": "2026-10-16T00:00:00Z"}) + "\n")
PY
"$ROOT/scripts/state.sh" --repo "$TMP/repo" --json > "$TMP/corrupt-state.json" || fail "state did not handle a corrupt lifecycle stream"
"$ROOT/scripts/inbox.sh" --repo "$TMP/repo" --json > "$TMP/corrupt-inbox.json" || fail "inbox did not handle a corrupt lifecycle stream"
mv "$TMP/lifecycle-events.backup" "$TMP/repo/.oms/lifecycle/events.jsonl"
python3 - "$TMP/review-state.json" "$TMP/review-inbox.json" "$TMP/aged-review-state.json" "$TMP/aged-review-inbox.json" \
  "$TMP/invalid-review-days.txt" "$TMP/corrupt-state.json" "$TMP/corrupt-inbox.json" <<'PY' || fail "review aging projection is incorrect"
import json, sys
fresh_state, fresh_inbox, aged_state, aged_inbox, invalid_text = [json.load(open(path, encoding="utf-8")) for path in sys.argv[1:5]] + [open(sys.argv[5], encoding="utf-8").read()]
corrupt_state = json.load(open(sys.argv[6], encoding="utf-8"))
corrupt_inbox = json.load(open(sys.argv[7], encoding="utf-8"))
assert fresh_state["agent_operations"]["review"] == 1, fresh_state
assert fresh_state["agent_operations"]["review_aged"] == 0, fresh_state
assert fresh_state["agent_operations"]["executing"] == 1, fresh_state
assert "agent-review-fresh" in {item["code"] for item in fresh_inbox["items"]}, fresh_inbox
assert aged_state["agent_operations"]["review_aged"] == 1, aged_state
aged = next(item for item in aged_inbox["items"] if item["code"] == "agent-review-aged")
assert aged["count"] == 1 and aged["command"] == "oms panel --settle-review", aged
assert "3d" in invalid_text, invalid_text
assert corrupt_state["agent_operations"]["healthy"] is False, corrupt_state
assert corrupt_state["agent_operations"]["review_aged"] == 0, corrupt_state
assert "agent-lifecycle-corrupt" in {item["code"] for item in corrupt_inbox["items"]}, corrupt_inbox
PY
env HOME="$TMP/home" OMS_LOCK_DIR="$TMP/locks" "$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" transition \
  --attempt "$review_attempt" --state "done" >/dev/null

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

# The router owns the terminal failure reason for direct write attempts.
mkdir -p "$TMP/routed-repo"
git -C "$TMP/routed-repo" init -q
git -C "$TMP/routed-repo" -c user.name=test -c user.email=test@example.com commit --allow-empty -qm base
fixture_write_guard() { return 1; }
export -f fixture_write_guard
for routed_failure in guard timeout provider; do
  failure_status=0
  failure_timeout="${OMS_PEER_TIMEOUT:-}"
  failure_sleep=""
  failure_exit=0
  failure_guard=""
  case "$routed_failure" in
    guard) failure_guard=fixture_write_guard ;;
    timeout) failure_timeout=1; failure_sleep=3 ;;
    provider) failure_exit=7 ;;
  esac
  printf 'routed failure probe\n' > "$TMP/routed-prompt"
  touch "$TMP/routed-artifact"
  env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
    OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
    OMS_PEER_TIMEOUT="$failure_timeout" OMS_TEST_PROVIDER_SLEEP="$failure_sleep" \
    OMS_TEST_MAIL_EXIT="$failure_exit" OMS_WRITE_POST_ATTEMPT_GUARD_FN="$failure_guard" \
    OMS_TEST_ROOT="$ROOT" bash -c '
      . "$OMS_TEST_ROOT/scripts/lib/peer-common.sh"
      REPO="$1"; MA_KIND=delegate; MA_SHOW_REPO=0; MA_QUORUM_FALLBACK=answer
      MA_DEBATE_ROLE=advisors; MA_DEBATE_TOPIC=question; MA_DEBATE_SECTIONS=""
      ma_run_routed_provider codex write "$2" "$3" "$1" peer-delegate "$1" ""
    ' _ "$TMP/routed-repo" "$TMP/routed-prompt" "$TMP/routed-artifact" \
    >/dev/null 2>&1 || failure_status=$?
  [ "$failure_status" -ne 0 ] || fail "routed $routed_failure unexpectedly succeeded"
  failure_attempt="$(python3 - "$TMP/routed-repo/.oms/lifecycle/events.jsonl" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
print([row["attempt_id"] for row in rows if row.get("event_type") == "attempt.created" and
       row.get("tool") == "peer-delegate"][-1])
PY
)"
  failure_attempt="${failure_attempt//$'\r'/}"
  "$ROOT/scripts/agent-events.sh" --repo "$TMP/routed-repo" show --attempt "$failure_attempt" --json \
    > "$TMP/routed-$routed_failure.json"
done
python3 - "$TMP/routed-guard.json" "$TMP/routed-timeout.json" "$TMP/routed-provider.json" <<'PY' || fail "routed failure reason code was lost"
import json, sys
expected = ("worker_authority_violation", "provider_timeout", "provider_failed")
for path, reason in zip(sys.argv[1:], expected):
    attempt = json.load(open(path, encoding="utf-8"))
    if isinstance(attempt, list):
        attempt = attempt[-1]
    assert attempt["state"] == "failed", attempt
    assert attempt["reason_code"] == reason, (reason, attempt)
PY

# The worker's model, panel task and room are routing for its provider call
# only. The declared verifier starts as clean as a top-level verify, so suites
# it runs cannot record fixture attempts into the live room under this task.
env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
  OMS_ROOM_REPO="$TMP/repo" OMS_PANEL_LABEL=fixture-label \
  "$ROOT/scripts/peer-delegate.sh" --repo "$TMP/repo" --to codex \
  --prompt 'write lifecycle verify environment' --model fixture-model \
  --verify 'v="${OMS_MODEL_EXPLICIT-unset}${OMS_ROOM_REPO-unset}${OMS_PANEL_LABEL-unset}"; [ "$v" = unsetunsetunset ] && echo unset' \
  >/dev/null || fail "delegation verify inherited the worker model"
verify_artifact="$(ls -t "$TMP/repo"/.oms/artifacts/delegate/*.md 2>/dev/null | head -1)"
[ -n "$verify_artifact" ] && grep -Fqx 'unset' "$verify_artifact" ||
  fail "verify output did not report an unset explicit model"

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
assert len(writes) == 3, writes
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
deadline=$((SECONDS + 180))
while [ ! -f "$OMS_TEST_VERIFY_RELEASE" ]; do
  [ "$SECONDS" -lt "$deadline" ] || { echo 'verify release barrier timed out' >&2; exit 124; }
  sleep .05
done
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
_verify_wait=0
while [ ! -f "$verify_started" ] && [ "$_verify_wait" -lt 1200 ]; do
  sleep .05; _verify_wait=$((_verify_wait + 1))
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
_verify_wait=0
while [ ! -f "$verify_started" ] && [ "$_verify_wait" -lt 1200 ]; do
  sleep .05; _verify_wait=$((_verify_wait + 1))
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
_verify_wait=0
while [ ! -f "$verify_started" ] && [ "$_verify_wait" -lt 1200 ]; do
  sleep .05; _verify_wait=$((_verify_wait + 1))
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
