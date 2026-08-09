#!/usr/bin/env bash
set -euo pipefail

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
printf 'tokens used\n3\n'
printf 'provider answer\n'
printf 'tokens used\n17\n'
EOF
chmod +x "$TMP/bin/codex"

env -u NVM_DIR HOME="$TMP/home" XDG_STATE_HOME="$TMP/state" \
  OMS_LOCK_DIR="$TMP/locks" PATH="$TMP/bin:/usr/bin:/bin" \
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
  PATH="$TMP/bin:/usr/bin:/bin" "$ROOT/scripts/agent-call.sh" \
  --repo "$TMP/repo" --to codex --prompt 'nested lifecycle probe' >/dev/null ||
  fail "nested agent-call failed"

"$ROOT/scripts/agent-events.sh" --repo "$TMP/repo" show \
  --attempt "$outer" --json > "$TMP/outer.json"
python3 - "$TMP/outer.json" "$TMP/repo/.oms/artifacts/index.jsonl" <<'PY' || fail "supervisor-owned lifecycle was not preserved"
import json, sys

outer = json.load(open(sys.argv[1], encoding="utf-8"))
assert outer["state"] == "working", outer
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
"$ROOT/scripts/repo-state.sh" --repo "$TMP/repo" --json > "$TMP/state.json"
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
