#!/usr/bin/env bash
set -euo pipefail

# Smoke tests for the harness-state surfaces added around the MCP server:
# the read-only oms MCP server itself, its claude/codex registration, the
# Antigravity plugin packaging, the router's state-conditional hints, and
# the generate-slurm-reference rename.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-state-surfaces.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q
  mkdir -p "$dir/.oms"
  printf '*\n' > "$dir/.oms/.gitignore"
}

# --- MCP server protocol ----------------------------------------------------

test_mcp_server_protocol() {
  local repo="$TMP/mcp-repo"
  local out="$TMP/mcp-out"

  make_repo "$repo"
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","id":6,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
    printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"oms_task_state","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"oms_handoff_show","arguments":{"repo":"%s","file":"../escape"}}}\n' "$repo"
    printf '%s\n' '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}'
    printf '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"oms_inbox","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"oms_repo_state","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"oms_agent_operations","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"oms_approvals","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"oms_runtime_release","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"oms_runtime_profile","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"oms_runtime_failures","arguments":{"repo":"%s"}}}\n' "$repo"
  } | python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"

  OMS_T_OUT="$out" python3 - <<'PY' || fail "MCP protocol exchange did not match the contract"
import json, os

by_id = {}
with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    for line in fh:
        msg = json.loads(line)
        by_id[msg.get("id")] = msg

init = by_id[1]["result"]
# An unimplemented revision is clamped to the newest supported one, never
# echoed back as false conformance; a supported revision is honored.
assert init["protocolVersion"] == "2025-06-18", init
assert init["serverInfo"]["name"] == "oh-my-setting", init
assert by_id[6]["result"]["protocolVersion"] == "2025-03-26", by_id[6]
listed = {t["name"]: t for t in by_id[2]["result"]["tools"]}
tools = set(listed)
assert {"oms_inbox", "oms_task_state", "oms_fail_ledger", "oms_handoffs",
        "oms_handoff_show", "oms_journal", "oms_repo_state",
        "oms_agent_operations", "oms_approvals", "oms_runtime_release",
        "oms_runtime_profile", "oms_runtime_failures"} <= tools, tools
# Every tool tells a client what it costs to call. A reader that advertised
# itself as a write would be approved by hand forever; a consultation that
# advertised itself as a read would spend a peer's wall clock unattended.
for name, tool in listed.items():
    hints = tool.get("annotations")
    assert hints, (name, tool)
    assert set(hints) == {"readOnlyHint", "destructiveHint", "idempotentHint",
                          "openWorldHint"}, (name, hints)
    assert hints["destructiveHint"] is False, (name, hints)
    if name == "oms_peer_start":
        assert hints["readOnlyHint"] is False, hints
        assert hints["idempotentHint"] is False, hints  # two calls, two runs
        assert hints["openWorldHint"] is True, hints    # it reaches a provider
    else:
        assert hints["readOnlyHint"] is True, (name, hints)
        assert hints["openWorldHint"] is False, (name, hints)
task = by_id[3]["result"]
assert not task["isError"], task
assert json.loads(task["content"][0]["text"])["schema"] == 1, task
guard = by_id[4]["result"]
assert guard["isError"], guard
assert "bare digest file name" in guard["content"][0]["text"], guard
assert by_id[5]["error"]["code"] == -32602, by_id[5]
inbox = by_id[7]["result"]
assert not inbox["isError"], inbox
assert json.loads(inbox["content"][0]["text"])["schema"] == 1, inbox
repo_state = by_id[8]["result"]
assert not repo_state["isError"], repo_state
assert "agent_operations" in json.loads(repo_state["content"][0]["text"]), repo_state
operations = by_id[9]["result"]
assert not operations["isError"], operations
assert json.loads(operations["content"][0]["text"]) == [], operations
approvals = by_id[10]["result"]
assert not approvals["isError"], approvals
assert json.loads(approvals["content"][0]["text"]) == [], approvals
# The typed runtime readers return the runtime command's own JSON unchanged:
# the same schemas the shell front door prints, no reconstruction, and no
# mutation surface (release status names the apply command, never runs it).
release = by_id[11]["result"]
assert not release["isError"], release
release_body = json.loads(release["content"][0]["text"])
assert release_body["schema"] == 1, release_body
assert release_body["stable"]["auto_apply"] is False, release_body
# Passthrough only: whether the pinned stable SHA resolves depends on the
# clone (a shallow CI checkout lacks it), so assert the projection's shape,
# never this environment's readiness.
assert release_body["stable"]["channel"] == "stable", release_body
assert "resolved_commit" in release_body["stable"], release_body
# isError mirrors the runtime command's own exit: profile current is
# nonzero wherever no provider CLI is installed (CI runners), and that
# structured refusal passing through unchanged IS the contract. Assert the
# body's shape in both worlds, never this environment's readiness.
profile = by_id[12]["result"]
profile_body = json.loads(profile["content"][0]["text"])
assert profile_body["schema"] == 1, profile_body
assert "configured" in profile_body, profile_body
assert profile_body["check"]["required"].get("git") is True, profile_body
assert profile["isError"] == (not profile_body["check"]["ready"]), profile
failures = by_id[13]["result"]
assert not failures["isError"], failures
failure_body = json.loads(failures["content"][0]["text"])
assert "provider_timeout" in failure_body, sorted(failure_body)
assert failure_body["provider_timeout"]["recovery"], failure_body
assert len(failure_body) >= 15, sorted(failure_body)
PY
}

test_mcp_server_bounds_requests_before_effects() {
  local repo="$TMP/mcp-bounds-repo"
  local out="$TMP/mcp-bounds-out"

  mkdir -p "$repo"
  OMS_T_REPO="$repo" python3 - <<'PY' |
import json, os, sys

repo = os.environ["OMS_T_REPO"]
stream = sys.stdout.buffer
# This line is larger than the server's request cap. The following valid ping
# proves it drains only that line and keeps serving without retaining it.
stream.write(b"{" + (b"x" * 262144) + b"\n")
stream.write(b"[]\n")
stream.write(b"\xff\n")
oversized_prompt = {
    "jsonrpc": "2.0",
    "id": 91,
    "method": "tools/call",
    "params": {
        "name": "oms_peer_start",
        "arguments": {
            "repo": repo,
            "kind": "ask",
            "prompt": "p" * 65537,
        },
    },
}
stream.write(json.dumps(oversized_prompt).encode("utf-8") + b"\n")
# Both records fit under the line cap but used to terminate the whole server:
# the first exceeds the JSON decoder's nesting depth, and the second reaches
# the platform with a single overlong path component.
stream.write((b"[" * 100000) + b"0" + (b"]" * 100000) + b"\n")
long_repo = {
    "jsonrpc": "2.0",
    "id": 92,
    "method": "tools/call",
    "params": {
        "name": "oms_inbox",
        "arguments": {"repo": "r" * 10000},
    },
}
stream.write(json.dumps(long_repo).encode("utf-8") + b"\n")
stream.write(b'{"jsonrpc":"2.0","id":93,"method":"ping"}\n')
PY
    python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"

  OMS_T_OUT="$out" python3 - <<'PY' || fail "MCP request bounds did not fail closed"
import json, os

rows = [json.loads(line) for line in open(os.environ["OMS_T_OUT"], encoding="utf-8")]
assert len(rows) == 7, rows
assert rows[0]["error"]["code"] == -32600, rows[0]
assert "byte limit" in rows[0]["error"]["message"], rows[0]
assert rows[1]["error"]["code"] == -32600, rows[1]
assert rows[2]["error"]["code"] == -32700, rows[2]
prompt = rows[3]
assert prompt["id"] == 91 and prompt["result"]["isError"], prompt
assert "prompt exceeds" in prompt["result"]["content"][0]["text"], prompt
assert rows[4]["error"]["code"] == -32700, rows[4]
long_path = rows[5]
assert long_path["id"] == 92 and long_path["result"]["isError"], long_path
assert "repo exceeds" in long_path["result"]["content"][0]["text"], long_path
assert rows[6] == {"jsonrpc": "2.0", "id": 93, "result": {}}, rows[6]
PY
  [ ! -e "$repo/.oms" ] ||
    fail "oversized prompt created MCP state before it was rejected"
}

# --- MCP peer action tools --------------------------------------------------

# One JSON-RPC exchange against the server in the fixture's world: the stub
# provider on PATH, HOME and locks inside TMP so a real consult run cannot
# reach the developer's own state, and the session markers unset so provider
# selection is the test's choice rather than the shell it runs in.
peer_rpc() {
  env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX \
    HOME="$TMP/peer-home" PATH="$TMP/peer-bin:$PATH" \
    OMS_LOCK_DIR="$TMP/peer-locks" OMS_LOCK_FORCE_MKDIR=1 \
    STUB_GATE="$TMP/peer-gate" \
    python3 "$ROOT/scripts/oms-mcp-server.py" > "$1"
}

peer_field() {  # peer_field FILE FIELD -> that field of a single tool result
  OMS_T_OUT="$1" OMS_T_FIELD="$2" python3 - <<'PY'
import json, os
msg = json.loads(open(os.environ["OMS_T_OUT"], encoding="utf-8").read())
print(json.loads(msg["result"]["content"][0]["text"])[os.environ["OMS_T_FIELD"]])
PY
}

test_mcp_peer_actions_start_detached_and_poll() {
  local repo="$TMP/peer-repo"
  local out="$TMP/peer-out"
  local operation
  local waited=0

  make_repo "$repo"
  mkdir -p "$TMP/peer-bin" "$TMP/peer-home" "$TMP/peer-locks"
  # The fixture peer waits for the gate before answering, which is what makes
  # "started but not finished" a state the test can observe instead of race.
  cat > "$TMP/peer-bin/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
tries=0
while [ ! -f "$STUB_GATE" ] && [ "$tries" -lt 120 ]; do
  sleep 1
  tries=$((tries + 1))
done
printf 'STUB-ANSWER: the fixture peer replied\n'
EOF
  chmod +x "$TMP/peer-bin/codex"
  rm -f "$TMP/peer-gate"

  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"consult","prompt":"   "}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"consult","prompt":"q","providers":"--sandbox"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"oms_peer_result","arguments":{"repo":"%s","operation":"../../../etc"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"consult","prompt":"is the gate open","providers":"codex"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"advise","prompt":"q","new_thread":true}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"consult","prompt":"q","thread":"--print-timeout"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"oms_peer_operations","arguments":{"repo":"%s"}}}\n' "$repo"
  } | peer_rpc "$out"

  OMS_T_OUT="$out" OMS_T_REPO="$repo" python3 - <<'PY' || fail "peer action tools did not match the contract"
import json, os

by_id = {}
with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    for line in fh:
        msg = json.loads(line)
        by_id[msg.get("id")] = msg

tools = {t["name"]: t for t in by_id[1]["result"]["tools"]}
# The read-only surface every client already binds must survive the addition.
assert {"oms_inbox", "oms_task_state", "oms_fail_ledger", "oms_handoffs",
        "oms_handoff_show", "oms_journal"} <= set(tools), sorted(tools)
start = tools["oms_peer_start"]["inputSchema"]
assert set(start["required"]) == {"kind", "prompt"}, start
assert sorted(start["properties"]["kind"]["enum"]) == ["advise", "ask", "consult"], start
assert "providers" in start["properties"], start
assert "thread" in start["properties"], start
assert start["properties"]["new_thread"]["type"] == "boolean", start
result = tools["oms_peer_result"]["inputSchema"]
assert result["required"] == ["operation"], result
# The description has to teach the pattern, or a model blocks on a 25-minute run.
assert "oms_peer_result" in tools["oms_peer_start"]["description"], tools
listing = tools["oms_peer_operations"]["inputSchema"]
assert listing["required"] == [], listing

empty = by_id[2]["result"]
assert empty["isError"], empty
assert "prompt is required" in empty["content"][0]["text"], empty
# A target becomes argv: a flag-shaped one must never reach the verb.
bad_target = by_id[3]["result"]
assert bad_target["isError"], bad_target
assert "PROVIDER" in bad_target["content"][0]["text"], bad_target
escape = by_id[4]["result"]
assert escape["isError"], escape
assert "operation must be an id" in escape["content"][0]["text"], escape

started = by_id[5]["result"]
assert not started["isError"], started
payload = json.loads(started["content"][0]["text"])
assert payload["started"] is True, payload
assert payload["kind"] == "consult", payload
assert payload["artifact_dir"].endswith("/.oms/artifacts/consult"), payload
assert payload["operation"] in payload["run_dir"], payload
assert os.path.isdir(payload["run_dir"]), payload
assert os.path.isfile(payload["log"]), payload
assert payload["targets"] == ["codex"], payload

# Thread control is offered only where the verb has it, and a thread id
# becomes argv: neither may reach the verb unchecked.
fresh_advise = by_id[6]["result"]
assert fresh_advise["isError"], fresh_advise
assert "new_thread applies to kind='consult'" in fresh_advise["content"][0]["text"], fresh_advise
flag_thread = by_id[7]["result"]
assert flag_thread["isError"], flag_thread
assert "thread must be an id" in flag_thread["content"][0]["text"], flag_thread
assert len(os.listdir(os.path.dirname(payload["run_dir"]))) == 1, "a rejected start left a run directory"

# The id lives on disk, not in the conversation that started it.
listed = by_id[8]["result"]
assert not listed["isError"], listed
rows = json.loads(listed["content"][0]["text"])
assert rows["total"] == 1 and rows["shown"] == 1, rows
row = rows["operations"][0]
assert row["operation"] == payload["operation"], row
assert row["status"] == "running", row
assert row["kind"] == "consult", row
assert row["targets"] == ["codex"], row
assert row["title"] == "is the gate open", row
PY

  operation="$(
    OMS_T_OUT="$out" python3 - <<'PY'
import json, os
with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    for line in fh:
        msg = json.loads(line)
        if msg.get("id") == 5:
            print(json.loads(msg["result"]["content"][0]["text"])["operation"])
PY
  )"
  [ -n "$operation" ] || fail "oms_peer_start returned no operation id"

  # The run outlives the server that started it: the process above has exited,
  # and this is a new one reading nothing but the filesystem.
  printf '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"oms_peer_result","arguments":{"repo":"%s","operation":"%s"}}}\n' \
    "$repo" "$operation" | peer_rpc "$out"
  [ "$(peer_field "$out" status)" = running ] ||
    fail "a gated consult must report running, not done"
  peer_field "$out" log_tail >/dev/null ||
    fail "a running consult must carry a log tail"

  : > "$TMP/peer-gate"
  while :; do
    printf '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"oms_peer_result","arguments":{"repo":"%s","operation":"%s"}}}\n' \
      "$repo" "$operation" | peer_rpc "$out"
    [ "$(peer_field "$out" status)" = "done" ] && break
    waited=$((waited + 1))
    [ "$waited" -lt 60 ] || fail "the released consult never completed"
    sleep 1
  done

  OMS_T_OUT="$out" python3 - <<'PY' || fail "a finished consult did not report its answer"
import json, os

msg = json.loads(open(os.environ["OMS_T_OUT"], encoding="utf-8").read())
assert not msg["result"]["isError"], msg
payload = json.loads(msg["result"]["content"][0]["text"])
assert payload["exit"] == 0, payload
assert "STUB-ANSWER: the fixture peer replied" in payload["answer"], payload
assert payload["artifacts"], payload
for path in payload["artifacts"]:
    assert "/.oms/artifacts/consult/" in path, payload
    assert os.path.isfile(path), payload
# The follow-up address: without it a second question starts from nothing.
assert payload["thread"], payload
PY

  printf '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"oms_peer_operations","arguments":{"repo":"%s"}}}\n' \
    "$repo" | peer_rpc "$out"
  OMS_T_OUT="$out" python3 - <<'PY' || fail "the finished run was not listed as done"
import json, os

rows = json.loads(
    json.loads(open(os.environ["OMS_T_OUT"], encoding="utf-8").read())
    ["result"]["content"][0]["text"]
)
row = rows["operations"][0]
assert row["status"] == "done" and row["exit"] == 0, row
assert row["thread"], row
PY
}

# An advisor prompt tells the peer what shape to answer in, so the artifact
# quotes a "## Output" heading long before the peer says anything. Reading the
# sections forward returned that template as the answer.
test_mcp_peer_result_reads_the_answer_not_the_quoted_prompt() {
  local repo="$TMP/peer-sections"
  local run="$repo/.oms/artifacts/mcp/advise-20200101T000000Z-abcdef01"
  local artifact="$repo/.oms/artifacts/advise/codex-probe.md"
  local out="$TMP/peer-sections-out"

  make_repo "$repo"
  mkdir -p "$run" "$repo/.oms/artifacts/advise" "$TMP/peer-home" "$TMP/peer-bin"
  cat > "$artifact" <<'EOF'
# codex advise

- started: 2020-01-01T00:00:00Z

## Prompt

Answer in this shape:

## Output

VERDICT: proceed | revise | stop

## Output

REAL-ANSWER: the advisor spoke

## Exit

0
EOF
  printf 'artifact: %s\n' "$artifact" > "$run/run.log"
  printf '0\n' > "$run/status"

  printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"oms_peer_result","arguments":{"repo":"%s","operation":"advise-20200101T000000Z-abcdef01"}}}\n' \
    "$repo" | peer_rpc "$out"

  OMS_T_OUT="$out" python3 - <<'PY' || fail "the answer must be the peer's, not the prompt it was given"
import json, os

payload = json.loads(
    json.loads(open(os.environ["OMS_T_OUT"], encoding="utf-8").read())
    ["result"]["content"][0]["text"]
)
assert payload["status"] == "done", payload
assert payload["exit"] == 0, payload
assert payload["answer"] == "REAL-ANSWER: the advisor spoke", payload
PY
}

# A run whose process died without writing an exit used to poll as "running"
# forever, and the only honest answer — nothing is coming — was invisible.
# Runs started before the pid file exists must still list.
test_mcp_peer_operations_report_dead_and_legacy_runs() {
  local repo="$TMP/peer-liveness"
  local dead_run="$repo/.oms/artifacts/mcp/consult-20200101T000000Z-deadbeef"
  local old_run="$repo/.oms/artifacts/mcp/ask-20190101T000000Z-0badc0de"
  local empty="$TMP/peer-no-runs"
  local out="$TMP/peer-liveness-out"
  local dead

  make_repo "$repo"
  make_repo "$empty"
  mkdir -p "$dead_run" "$old_run" "$TMP/peer-home" "$TMP/peer-bin"

  # A pid that is certainly not a live run: started, reaped, then recorded.
  sh -c 'exit 0' &
  dead=$!
  wait "$dead" 2>/dev/null || true
  printf '%s\n' "$dead" > "$dead_run/pid"
  printf 'consult: the peer was launched\n' > "$dead_run/run.log"
  printf '{"operation":"consult-20200101T000000Z-deadbeef","kind":"consult","targets":["claude"],"started_at":"2020-01-01T00:00:00Z","title":"did the run survive"}\n' \
    > "$dead_run/meta.json"
  # The legacy shape: no meta.json, no pid, just the exit the run recorded.
  printf '0\n' > "$old_run/status"

  {
    printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"oms_peer_operations","arguments":{"repo":"%s"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oms_peer_result","arguments":{"repo":"%s","operation":"consult-20200101T000000Z-deadbeef"}}}\n' "$repo"
    printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"oms_peer_operations","arguments":{"repo":"%s"}}}\n' "$empty"
  } | peer_rpc "$out"

  OMS_T_OUT="$out" OMS_T_EMPTY="$empty" python3 - <<'PY' || fail "peer run liveness did not match the contract"
import json, os

by_id = {}
with open(os.environ["OMS_T_OUT"], encoding="utf-8") as fh:
    for line in fh:
        msg = json.loads(line)
        by_id[msg.get("id")] = msg

rows = json.loads(by_id[1]["result"]["content"][0]["text"])
assert rows["total"] == 2 and rows["shown"] == 2, rows
# Newest first by the timestamp in the id, not by the kind that prefixes it.
dead, legacy = rows["operations"]
assert dead["operation"].startswith("consult-2020"), rows
assert legacy["operation"].startswith("ask-2019"), rows
assert dead["targets"] == ["claude"], dead
assert dead["title"] == "did the run survive", dead
# The id carries kind and start time, so a run with no meta.json still reads.
assert legacy["kind"] == "ask", legacy
assert legacy["started_at"] == "2019-01-01T00:00:00Z", legacy
assert legacy["status"] == "done" and legacy["exit"] == 0, legacy

if os.name == "posix":  # elsewhere os.kill would terminate the pid, not test it
    assert dead["status"] == "stalled", dead
    stalled = by_id[2]["result"]
    assert stalled["isError"], stalled
    payload = json.loads(stalled["content"][0]["text"])
    assert payload["status"] == "stalled", payload
    assert "no answer is coming" in payload["next"], payload
    assert "the peer was launched" in payload["log_tail"], payload
else:
    assert dead["status"] == "running", dead

# A repository that never consulted anyone lists nothing and gains nothing.
empty = json.loads(by_id[3]["result"]["content"][0]["text"])
assert empty["operations"] == [] and empty["total"] == 0, empty
assert not os.path.exists(os.path.join(os.environ["OMS_T_EMPTY"], ".oms", "artifacts")), empty
PY
}

# The gate's .oms purity inventory. What the live session writes on its own
# schedule must stay out of it — a CI row recorded for a push mid-gate read as
# a suite defect and cost a full re-run — while everything a forgotten --repo
# would touch stays covered.
test_oms_state_inventory_excludes_only_session_owned_entries() {
  local state="$TMP/inventory/.oms"
  local out="$TMP/inventory-out"

  mkdir -p "$state/hooks" "$state/work-journal" "$state/plan"
  printf 'row\n' > "$state/hooks/turn.jsonl"
  printf 'row\n' > "$state/work-journal/today.jsonl"
  printf 'row\n' > "$state/ci.jsonl"
  printf '*\n' > "$state/.gitignore"
  printf 'row\n' > "$state/failures.jsonl"
  printf 'row\n' > "$state/plan/tasks.json"
  python3 "$ROOT/scripts/lib/oms-state-inventory.py" "$state" > "$out" ||
    fail "the inventory should read a plain .oms directory"

  grep -q '"failures.jsonl"' "$out" ||
    fail "failures.jsonl must stay covered: a forgotten --repo writes it"
  grep -q '"plan/tasks.json"' "$out" || fail "plan state must stay covered"
  for ambient in ci.jsonl .gitignore hooks work-journal; do
    if grep -q "\"$ambient" "$out"; then
      fail "$ambient is written by the live session and must not be inventoried"
    fi
  done

  # A nested path that merely repeats an ambient name is not ambient.
  printf 'row\n' > "$state/plan/ci.jsonl"
  printf '*\n' > "$state/plan/.gitignore"
  python3 "$ROOT/scripts/lib/oms-state-inventory.py" "$state" > "$out"
  grep -q '"plan/ci.jsonl"' "$out" ||
    fail "the exclusion must be anchored at the top level, not by name anywhere"
  grep -q '"plan/.gitignore"' "$out" ||
    fail "a nested .gitignore is state, not the top-level ownership marker"

  # The shadow-judgment ledger is the one path-precise ambient entry: any
  # session starting in this checkout appends a row mid-gate. Only the
  # ledger itself — a same-named file elsewhere stays covered.
  printf 'row\n' > "$state/plan/autopilot-shadow.jsonl"
  printf 'row\n' > "$state/autopilot-shadow.jsonl"
  python3 "$ROOT/scripts/lib/oms-state-inventory.py" "$state" > "$out"
  if grep -q '"plan/autopilot-shadow.jsonl"' "$out"; then
    fail "the shadow ledger is written at session start and must not be inventoried"
  fi
  grep -q '"autopilot-shadow.jsonl"' "$out" ||
    fail "the ambient shadow entry is path-precise, not a name match"
}

# --- claude/codex registration ---------------------------------------------

test_install_mcp_registers_and_is_idempotent() {
  local bin="$TMP/mcp-bin"
  local log="$TMP/mcp-log"

  mkdir -p "$bin"
  : > "$log"
  # Stubs: `mcp get` succeeds (printing the registered command) only after a
  # marker that `mcp add` writes — the real CLIs' visible contract.
  local cli status out
  for cli in claude codex; do
    cat > "$bin/$cli" <<EOF
#!/usr/bin/env bash
echo "$cli \$*" >> "$log"
case "\$1 \$2" in
  "mcp get")
    if [ "\${OMS_TEST_MCP_LOOKUP_FAIL:-}" = "$cli" ]; then
      echo "could not read $cli MCP configuration" >&2
      exit 24
    fi
    if [ ! -f "$TMP/$cli.registered" ]; then
      echo "No MCP server named oh-my-setting" >&2
      exit 1
    fi
    cat "$TMP/$cli.registered"
    ;;
  "mcp add")
    printf '%s\n' "\$*" > "$TMP/$cli.registered"
    ;;
  "mcp remove")
    [ "\${OMS_TEST_MCP_REMOVE_FAIL:-}" != "$cli" ] || exit 23
    [ -f "$TMP/$cli.registered" ] || exit 25
    rm -f "$TMP/$cli.registered"
    ;;
esac
exit 0
EOF
    chmod +x "$bin/$cli"
  done

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-first" ||
    fail "install-mcp should succeed with both CLIs present"
  grep -Fq "mcp: claude registered" "$TMP/mcp-first" || fail "claude was not registered"
  grep -Fq "mcp: codex registered" "$TMP/mcp-first" || fail "codex was not registered"
  grep -Fq "scripts/oms-mcp-server.py" "$TMP/claude.registered" ||
    fail "claude registration does not point at the server"

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-second" ||
    fail "re-registration should succeed"
  grep -Fq "mcp: claude already registered" "$TMP/mcp-second" ||
    fail "second run should detect the existing claude registration"

  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove >/dev/null ||
    fail "removal should succeed"
  [ ! -f "$TMP/claude.registered" ] || fail "claude registration was not removed"
  [ ! -f "$TMP/codex.registered" ] || fail "codex registration was not removed"

  : > "$log"
  PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove >/dev/null ||
    fail "already-absent MCP removal should be idempotent"
  if grep -Fq "mcp remove" "$log"; then
    fail "already-absent MCP removal still invoked a destructive command"
  fi

  for cli in claude codex; do
    printf 'registered\n' > "$TMP/claude.registered"
    printf 'registered\n' > "$TMP/codex.registered"
    : > "$log"
    status=0
    out="$(OMS_TEST_MCP_REMOVE_FAIL="$cli" PATH="$bin:/usr/bin:/bin" \
      bash "$ROOT/scripts/install-mcp.sh" --remove 2>&1)" || status=$?
    [ "$status" -ne 0 ] || fail "$cli MCP removal failure was reported as success"
    [ -f "$TMP/$cli.registered" ] || fail "$cli failure fixture unexpectedly removed its registration"
    grep -Fq "claude mcp remove" "$log" && grep -Fq "codex mcp remove" "$log" ||
      fail "MCP removal did not attempt every present provider after $cli failed"
    if printf '%s' "$out" | grep -Fq "mcp: $cli registration removed"; then
      fail "$cli MCP removal printed false success"
    fi

    PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove >/dev/null ||
      fail "MCP removal could not recover after a partial failure for $cli"
    [ ! -f "$TMP/claude.registered" ] && [ ! -f "$TMP/codex.registered" ] ||
      fail "MCP retry left a registration after $cli failed"
  done

  for cli in claude codex; do
    printf 'registered\n' > "$TMP/claude.registered"
    printf 'registered\n' > "$TMP/codex.registered"
    : > "$log"
    status=0
    out="$(OMS_TEST_MCP_LOOKUP_FAIL="$cli" PATH="$bin:/usr/bin:/bin" \
      bash "$ROOT/scripts/install-mcp.sh" --remove 2>&1)" || status=$?
    [ "$status" -ne 0 ] || fail "$cli MCP state lookup failure was ignored"
    [ -f "$TMP/$cli.registered" ] || fail "$cli lookup failure mutated its registration"
    grep -Fq "claude mcp get" "$log" && grep -Fq "codex mcp get" "$log" ||
      fail "MCP state lookup failure stopped the independent provider check"

    PATH="$bin:/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove >/dev/null ||
      fail "MCP removal could not recover after a state lookup failure for $cli"
  done

  # No CLIs on PATH is non-fatal while installing, but removal cannot claim it
  # cleaned state it had no command with which to inspect.
  PATH="/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" > "$TMP/mcp-none" ||
    fail "missing CLIs must be notes, not failures"
  grep -Fq "claude CLI absent" "$TMP/mcp-none" || fail "missing claude should be noted"
  status=0
  out="$(PATH="/usr/bin:/bin" bash "$ROOT/scripts/install-mcp.sh" --remove 2>&1)" || status=$?
  [ "$status" -ne 0 ] || fail "missing MCP CLIs were accepted during removal"
  printf '%s' "$out" | grep -Fq "claude CLI is required" ||
    fail "missing claude removal did not explain its recovery requirement"
  printf '%s' "$out" | grep -Fq "codex CLI is required" ||
    fail "missing codex removal did not attempt every provider"
}

# --- antigravity plugin -----------------------------------------------------

test_install_agy_plugin_bakes_absolute_paths() {
  local bin="$TMP/agy-bin"
  local log="$TMP/agy-log"
  local capture="$TMP/agy-plugin-copy"
  local status

  # A throwaway HOME: the installer reaches into the installed plugin to
  # retract stale hooks, and a test must never do that to the real one.
  mkdir -p "$bin" "$capture" "$TMP/agy-home"
  cat > "$bin/agy" <<EOF
#!/usr/bin/env bash
echo "agy \$*" >> "$log"
if [ "\$1 \$2" = "plugin install" ]; then
  cp "\$3"/* "$capture/"
  touch "$TMP/agy.registered"
fi
if [ "\$1 \$2" = "plugin list" ]; then
  if [ "\${OMS_TEST_AGY_LOOKUP_FAIL:-0}" = 1 ]; then
    echo "could not read Antigravity plugin state" >&2
    exit 24
  fi
  if [ -f "$TMP/agy.registered" ]; then
    printf '%s\n' '{"imports":[{"name":"oh-my-setting"}]}'
  else
    printf '%s\n' '{"imports":[]}'
  fi
fi
if [ "\$1 \$2" = "plugin uninstall" ]; then
  [ "\${OMS_TEST_AGY_REMOVE_FAIL:-0}" != 1 ] || exit 23
  [ -f "$TMP/agy.registered" ] || exit 25
  rm -f "$TMP/agy.registered"
fi
exit 0
EOF
  chmod +x "$bin/agy"

  # Uncertified is the default: an agy nobody has probed gets the MCP server
  # and nothing else, whatever a previous probe on this machine concluded about
  # a different binary.
  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" OMS_CAPABILITY_DIR="$TMP/agy-caps-default" \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-out" ||
    fail "agy plugin install should succeed with agy present"
  grep -Fq "agy-plugin: installed" "$TMP/agy-out" || fail "install was not reported"
  grep -Fq "run \`oms update --probe-agy-surfaces\`" "$TMP/agy-out" ||
    fail "an uncertified agy should say how to certify it"
  [ -f "$capture/plugin.json" ] || fail "plugin.json was not built"
  [ -f "$capture/mcp_config.json" ] || fail "mcp_config.json was not built"
  [ ! -e "$capture/hooks.json" ] || fail "hooks.json must not ship uncertified"
  [ ! -e "$capture/agy-hook.sh" ] || fail "the hook adapter must not ship uncertified"
  OMS_T_DIR="$capture" OMS_T_ROOT="$ROOT" python3 - <<'PY' || fail "generated plugin content is wrong"
import json, os
d = os.environ["OMS_T_DIR"]
root = os.environ["OMS_T_ROOT"]
plugin = json.load(open(os.path.join(d, "plugin.json")))
assert plugin["name"] == "oh-my-setting", plugin
assert plugin["version"] not in ("", "0.0.0"), plugin
mcp = json.load(open(os.path.join(d, "mcp_config.json")))
server = mcp["mcpServers"]["oh-my-setting"]
assert server["command"] == "python3", server
assert server["args"] == [os.path.join(root, "scripts", "oms-mcp-server.py")], server
PY

  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove >/dev/null ||
    fail "plugin removal should succeed"
  grep -Fq "agy plugin uninstall oh-my-setting" "$log" ||
    fail "removal should call agy plugin uninstall"

  : > "$log"
  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove >/dev/null ||
    fail "already-absent Antigravity removal should be idempotent"
  if grep -Fq "plugin uninstall" "$log"; then
    fail "already-absent Antigravity removal still invoked uninstall"
  fi

  touch "$TMP/agy.registered"
  status=0
  OMS_TEST_AGY_REMOVE_FAIL=1 PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove > "$TMP/agy-remove-fail" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "agy plugin removal failure was reported as success"
  if grep -Fq 'agy-plugin: uninstalled' "$TMP/agy-remove-fail"; then
    fail "agy plugin removal printed false success"
  fi

  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove >/dev/null ||
    fail "Antigravity removal could not recover after a partial failure"

  touch "$TMP/agy.registered"
  : > "$log"
  status=0
  OMS_TEST_AGY_LOOKUP_FAIL=1 PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove > "$TMP/agy-lookup-fail" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "Antigravity state lookup failure was ignored"
  [ -f "$TMP/agy.registered" ] || fail "Antigravity lookup failure mutated the plugin"
  if grep -Fq "plugin uninstall" "$log"; then
    fail "Antigravity lookup failure still attempted uninstall"
  fi
  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove >/dev/null ||
    fail "Antigravity removal could not recover after a lookup failure"

  PATH="$bin:/usr/bin:/bin" HOME="$TMP/agy-home" OH_MY_SETTING_AGY_PLUGIN=0 \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-skip" ||
    fail "explicit skip should exit 0"
  grep -Fq "skipped" "$TMP/agy-skip" || fail "skip should be reported"

  PATH="/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/agy-absent" ||
    fail "absent agy in auto mode must be a note"
  grep -Fq "agy CLI absent" "$TMP/agy-absent" || fail "absent agy should be noted"
  status=0
  PATH="/usr/bin:/bin" HOME="$TMP/agy-home" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --remove > "$TMP/agy-remove-absent" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "missing agy CLI was accepted during removal"
}

# --- antigravity surface certification --------------------------------------

# A mock agy. FIRE=1 delivers plugin hooks the way the CLI documents them:
# handlers run through `sh -c` with the working directory set to the directory
# holding hooks.json, the payload arrives as camelCase JSON on stdin, and a
# declared timeout is enforced. FIRE=0 accepts the same plugin and runs a
# headless turn that fires nothing, which is what 1.1.9 did.
make_agy_mock() {
  local path="$1" fire="$2"
  mkdir -p "$(dirname "$path")"
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf 'FIRE=%s\n' "$fire"
    cat <<'MOCK'
case "${1:-}" in
  --version) printf '1.1.10-mock\n'; exit 0 ;;
esac
case "${1:-} ${2:-}" in
  "plugin validate")
    if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$3/hooks.json" 2>/dev/null; then
      printf '          hooks       : 1 processed\n'
    else
      printf '          hooks       : skipped (not found)\n'
    fi
    exit 0
    ;;
  "plugin install")
    name="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' \
      "$3/plugin.json")" || exit 1
    mkdir -p "$HOME/.gemini/config/plugins/$name"
    cp "$3"/* "$HOME/.gemini/config/plugins/$name/" || exit 1
    exit 0
    ;;
  "plugin uninstall") rm -rf "$HOME/.gemini/config/plugins/$3"; exit 0 ;;
esac
case "${1:-}" in
  -p|--print|--prompt) ;;
  *) exit 0 ;;
esac
if [ "$FIRE" = 1 ]; then
  for plugin in "$HOME"/.gemini/config/plugins/*/; do
    [ -f "$plugin/hooks.json" ] || continue
    OMS_MOCK_WS="$PWD" python3 - "$plugin" <<'PY'
import json, os, subprocess, sys

plugin = sys.argv[1]
workspace = os.environ["OMS_MOCK_WS"]
with open(os.path.join(plugin, "hooks.json"), encoding="utf-8") as handle:
    hooks = json.load(handle)
common = {
    "conversationId": "mock-conversation",
    "workspacePaths": [workspace],
    "transcriptPath": os.path.join(workspace, ".gemini/antigravity-cli/transcript.jsonl"),
    "artifactDirectoryPath": os.path.join(workspace, ".gemini/antigravity-cli/artifacts"),
    "modelName": "auto",
}
events = [
    ("PreInvocation", dict(common, invocationNum=1, initialNumSteps=3)),
    ("Stop", dict(common, executionNum=1, terminationReason="model_stop", fullyIdle=True)),
]
for event, payload in events:
    for named in hooks.values():
        for handler in named.get(event) or []:
            command = handler.get("command")
            if not command:
                continue
            try:
                subprocess.run(
                    ["sh", "-c", command],
                    cwd=plugin,
                    input=json.dumps(payload),
                    text=True,
                    capture_output=True,
                    timeout=handler.get("timeout") or 60,
                )
            except subprocess.TimeoutExpired:
                pass
PY
  done
fi
printf 'ok\n'
exit 0
MOCK
  } > "$path"
  chmod +x "$path"
}

test_agy_surfaces_are_certified_before_hooks_ship() {
  local bin="$TMP/cert-bin"
  local caps="$TMP/cert-caps"
  local home="$TMP/cert-home"
  local plugin="$home/.gemini/config/plugins/oh-my-setting"
  local repo="$TMP/cert-repo"
  local out

  make_agy_mock "$bin/agy" 1
  mkdir -p "$home"

  # 1. Probing certifies the surface, and does it without touching the caller's
  #    Antigravity configuration.
  PATH="$bin:/usr/bin:/bin" HOME="$home" OMS_CAPABILITY_DIR="$caps" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --probe-surfaces > "$TMP/cert-probe" ||
    fail "the surface probe should exit 0"
  grep -Fq "agy-surfaces: verified" "$TMP/cert-probe" ||
    fail "a firing agy should certify: $(cat "$TMP/cert-probe")"
  [ ! -e "$home/.gemini/config/plugins" ] ||
    fail "the probe must install its fixture under its own HOME, not the caller's"
  grep -Fq "verdict=verified" "$caps/agy-surfaces.env" ||
    fail "the verdict was not cached"
  grep -Fq "handler_timeout_enforced=yes" "$caps/agy-surfaces.env" ||
    fail "the timeout check should have passed against a mock that enforces it"

  # 2. Only now does the installer generate hooks.
  PATH="$bin:/usr/bin:/bin" HOME="$home" OMS_CAPABILITY_DIR="$caps" \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/cert-install" ||
    fail "installing a certified plugin should succeed"
  grep -Fq "certified PreInvocation/Stop hooks" "$TMP/cert-install" ||
    fail "the certified install was not reported: $(cat "$TMP/cert-install")"
  [ -f "$plugin/hooks.json" ] || fail "a certified agy should get hooks.json"
  [ -f "$plugin/agy-hook.sh" ] || fail "hooks.json needs its adapter beside it"
  OMS_T_HOOKS="$plugin/hooks.json" python3 - <<'PY' || fail "generated hooks.json is wrong"
import json, os

with open(os.environ["OMS_T_HOOKS"], encoding="utf-8") as handle:
    hooks = json.load(handle)
named = hooks["oh-my-setting"]
assert set(named) == {"PreInvocation", "Stop"}, named
for event, argument in (("PreInvocation", "preinvocation"), ("Stop", "stop")):
    handlers = named[event]
    assert len(handlers) == 1, handlers
    handler = handlers[0]
    assert handler["type"] == "command", handler
    # Relative to the directory holding hooks.json: the adapter travels with
    # the plugin instead of pointing at a path this checkout guessed.
    assert handler["command"] == "bash ./agy-hook.sh %s" % argument, handler
    assert handler["timeout"] == 5, handler
PY

  # 3. The adapter answers in Antigravity's shapes: a router hint becomes an
  #    injected ephemeral step, and Stop never returns a decision, so this
  #    surface can observe a turn but never block one.
  make_repo "$repo"
  ( cd "$repo" &&
    bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make test" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record \
      --cmd "python train.py" --exit 2 >/dev/null )

  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"modelName":"auto","invocationNum":1}' "$repo" |
    TMPDIR="$TMP" bash "$plugin/agy-hook.sh" preinvocation)" ||
    fail "the preinvocation adapter must exit 0"
  OMS_T_OUT="$out" python3 - <<'PY' || fail "preinvocation did not inject the router hint: $out"
import json, os

result = json.loads(os.environ["OMS_T_OUT"])
steps = result["injectSteps"]
assert len(steps) == 1, steps
assert "unresolved fail-ledger rows" in steps[0]["ephemeralMessage"], steps
PY

  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"terminationReason":"model_stop","executionNum":1}' "$repo" |
    TMPDIR="$TMP" bash "$plugin/agy-hook.sh" stop)" ||
    fail "the stop adapter must exit 0"
  [ "$out" = "{}" ] || fail "stop must never return a decision, got: $out"

  # Later invocations of the same turn stay quiet, and every kill switch is
  # honoured without reaching the harness at all.
  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"invocationNum":7}' "$repo" |
    TMPDIR="$TMP" bash "$plugin/agy-hook.sh" preinvocation)"
  [ "$out" = "{}" ] || fail "only the first invocation of a turn should route: $out"
  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"invocationNum":1}' "$repo" |
    OH_MY_SETTING_AGY_HOOKS=0 TMPDIR="$TMP" bash "$plugin/agy-hook.sh" preinvocation)"
  [ "$out" = "{}" ] || fail "OH_MY_SETTING_AGY_HOOKS=0 should silence the adapter: $out"
  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"invocationNum":1}' "$repo" |
    OMS_SKILL_ROUTER_OFF=1 TMPDIR="$TMP" bash "$plugin/agy-hook.sh" preinvocation)"
  [ "$out" = "{}" ] || fail "OMS_SKILL_ROUTER_OFF=1 should silence the router path: $out"
  out="$(printf '{"conversationId":"c1","workspacePaths":["%s"],"invocationNum":1}' "$repo" |
    OMS_HARNESS_CHILD=1 TMPDIR="$TMP" bash "$plugin/agy-hook.sh" preinvocation)"
  [ "$out" = "{}" ] || fail "a harness child must not route: $out"

  # 4. A different binary is a different question. Changing the mock changes
  #    its fingerprint, and the hooks the previous binary earned are actively
  #    retracted from the installed plugin rather than merely left out of the
  #    next build.
  printf '\n# rebuilt\n' >> "$bin/agy"
  PATH="$bin:/usr/bin:/bin" HOME="$home" OMS_CAPABILITY_DIR="$caps" \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/cert-refresh" ||
    fail "installing against a new binary should still succeed"
  [ ! -e "$plugin/hooks.json" ] ||
    fail "a new agy fingerprint must retract the certified hooks.json"
  [ ! -e "$plugin/agy-hook.sh" ] ||
    fail "a new agy fingerprint must retract the hook adapter"
  [ -f "$plugin/mcp_config.json" ] ||
    fail "retracting hooks must leave the MCP server installed"
  grep -Fq "retracted hooks" "$TMP/cert-refresh" ||
    fail "the retraction should be reported: $(cat "$TMP/cert-refresh")"
  grep -Fq "run \`oms update --probe-agy-surfaces\`" "$TMP/cert-refresh" ||
    fail "the fallback should point at re-certification"
}

test_agy_surfaces_stay_mcp_only_when_hooks_never_fire() {
  local bin="$TMP/silent-bin"
  local caps="$TMP/silent-caps"
  local home="$TMP/silent-home"

  make_agy_mock "$bin/agy" 0
  mkdir -p "$home"

  PATH="$bin:/usr/bin:/bin" HOME="$home" OMS_CAPABILITY_DIR="$caps" \
    bash "$ROOT/scripts/install-agy-plugin.sh" --probe-surfaces > "$TMP/silent-probe" ||
    fail "a silent agy should still exit 0"
  grep -Fq "agy-surfaces: unsupported" "$TMP/silent-probe" ||
    fail "a completed run that fires nothing is unsupported: $(cat "$TMP/silent-probe")"
  grep -Fq "reason=preinvocation" "$caps/agy-surfaces.env" ||
    fail "the cached reason should name the surface that never arrived"

  PATH="$bin:/usr/bin:/bin" HOME="$home" OMS_CAPABILITY_DIR="$caps" \
    bash "$ROOT/scripts/install-agy-plugin.sh" > "$TMP/silent-install" ||
    fail "an unsupported verdict must not fail the install"
  [ ! -e "$home/.gemini/config/plugins/oh-my-setting/hooks.json" ] ||
    fail "an unsupported agy must not get hooks.json"
  grep -Fq "failed surface certification" "$TMP/silent-install" ||
    fail "the omission should be reported: $(cat "$TMP/silent-install")"
}

test_update_probe_flag_reaches_the_probe() {
  local out
  out="$(PATH="/usr/bin:/bin" bash "$ROOT/scripts/update.sh" --probe-agy-surfaces 2>&1)" ||
    fail "the update flag should exit 0 with no agy installed: $out"
  printf '%s\n' "$out" | grep -Fq "agy-surfaces: unverified" ||
    fail "an absent agy is unverified, never a guess: $out"
  out="$(bash "$ROOT/scripts/update.sh" --probe-agy-surfaces --check 2>&1)" && rc=0 || rc=$?
  [ "${rc:-0}" = 2 ] || fail "--probe-agy-surfaces with --check should be misuse"
  bash "$ROOT/scripts/update.sh" --help | grep -Fq -- "--probe-agy-surfaces" ||
    fail "the flag should be documented in usage"
}

# --- router state hints -----------------------------------------------------

test_router_state_hint_on_unresolved_failures() {
  local repo="$TMP/hint-repo"
  local payload out

  make_repo "$repo"
  ( cd "$repo" &&
    bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make test" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record \
      --cmd "python train.py" --exit 2 >/dev/null )

  payload='{"prompt":"continue the work","session_id":"s","turn_id":"t"}'
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows" ||
    fail "two unresolved failures should produce a state hint: $out"

  # Same day: the marker suppresses a second hint.
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows"; then
    fail "the state hint must fire at most once per day"
  fi

  # Opt-out silences it even on a fresh day marker.
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" | OMS_STATE_REPO="$repo" OMS_STATE_HINTS=0 \
    TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "unresolved fail-ledger rows"; then
    fail "OMS_STATE_HINTS=0 must disable the hint"
  fi
}

test_router_state_hint_surfaces_parked_goal() {
  local repo="$TMP/hint-goal-repo"
  local payload out

  make_repo "$repo"
  ( cd "$repo" &&
    bash "$ROOT/scripts/agent-plan.sh" init --goal "unmet" --accept "false" >/dev/null )
  mkdir -p "$repo/.oms/plan"
  printf '%s\n' \
    '{"schema": 1, "kind": "acceptance", "status": "fail", "cycle": 1}' \
    '{"schema": 1, "kind": "terminal", "status": "park", "reason": "tasks-exhausted", "cycle": 1}' \
    > "$repo/.oms/plan/progress.jsonl"

  payload='{"prompt":"continue the work","session_id":"s","turn_id":"t"}'
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "goal parked (tasks-exhausted)" ||
    fail "a parked goal run should surface in the daily hint: $out"

  # When an outer autopilot receipt exists, the hook must not tell the next
  # parent to bypass that contract and invoke goal-drive directly.
  printf '%s\n' \
    '{"schema":1,"kind":"autopilot-run","stage":"parked"}' \
    > "$repo/.oms/plan/autopilot-run.json"
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq 'parent agent: inspect and resume the validated autopilot receipt internally' ||
    fail "a parked outer run should assign recovery to the parent agent: $out"
  if printf '%s' "$out" | grep -Eq 'run `oms|resume with `oms|review \.oms'; then
    fail "a parked outer run should not expose recovery commands to the user: $out"
  fi
  if printf '%s' "$out" | grep -Fq 'resume with `oms goal-drive`'; then
    fail "outer autopilot park was downgraded to a bare goal-drive resume"
  fi

  # A goal that finished (terminal done) stays quiet.
  printf '%s\n' \
    '{"schema": 1, "kind": "terminal", "status": "done", "reason": "acceptance-pass", "cycle": 2}' \
    >> "$repo/.oms/plan/progress.jsonl"
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "goal parked"; then
    fail "a completed goal must not keep hinting"
  fi
}

test_router_state_hint_skips_unadopted_repo() {
  local repo="$TMP/hint-plain"
  mkdir -p "$repo"
  local out
  out="$(printf '{"prompt":"hello","session_id":"s","turn_id":"t"}' |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  [ ! -d "$repo/.oms" ] || fail "the hint must not create .oms in a plain directory"
}

test_router_state_hint_offers_forge_for_resolved_repeats() {
  local repo="$TMP/hint-forge-repo"
  local payload out

  make_repo "$repo"
  ( cd "$repo" &&
    bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make test" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record \
      --cmd "make test" --exit 1 >/dev/null &&
    bash "$ROOT/scripts/fail-ledger.sh" resolve --cmd "make test" >/dev/null 2>&1 )

  payload='{"prompt":"continue the work","session_id":"s","turn_id":"t"}'
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "skill-forge add" ||
    fail "a resolved repeated failure with no project skill should hint at the forge: $out"

  # Once any project skill exists, the forge hint stays quiet.
  printf -- '---\nname: oms-lesson\ndescription: %s\n---\n\nBody.\n' \
    "A test lesson description long enough to pass the validation gate." |
    ( cd "$repo" && bash "$ROOT/scripts/skill-forge.sh" add --name oms-lesson >/dev/null )
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "skill-forge add"; then
    fail "an existing project skill must silence the forge hint"
  fi
}

test_router_hints_on_recurring_uncovered_usage() {
  local repo="$TMP/usage-hint-repo"
  local payload out yesterday roots

  make_repo "$repo"
  roots="$TMP/usage-empty-roots"
  mkdir -p "$roots"
  payload='{"prompt":"continue the work","session_id":"s","turn_id":"t"}'

  # The hook writes the counter: a live PostToolUse payload appends a
  # content-free {family, day} row — never the command itself.
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"obabel lig.sdf -O out.pdbqt"},"tool_response":{"exit_code":0},"hook_event_name":"PostToolUse"}' |
    OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh" >/dev/null
  grep -q '"family": "chem"' "$repo/.oms/usage.jsonl" ||
    fail "the hook should append a chem usage row"
  # A read-only command that merely mentions a family token is not use.
  printf '%s' '{"tool_name":"Bash","tool_input":{"command":"grep -rn rdkit src/"},"tool_response":{"exit_code":0},"hook_event_name":"PostToolUse"}' |
    OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh" >/dev/null
  [ "$(grep -c '"family": "chem"' "$repo/.oms/usage.jsonl")" = 1 ] ||
    fail "a grep mentioning rdkit must not count as chem usage"

  # Threshold needs recurrence across days; seed yesterday, raw and count rows.
  yesterday="$(python3 -c 'import datetime; print((datetime.date.today()-datetime.timedelta(days=1)).isoformat())')"
  for _ in 1 2 3 4; do
    printf '{"schema": 1, "family": "chem", "day": "%s"}\n' "$yesterday" \
      >> "$repo/.oms/usage.jsonl"
  done
  printf '{"schema": 1, "family": "chem", "day": "%s", "count": 2}\n' "$yesterday" \
    >> "$repo/.oms/usage.jsonl"
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" OMS_USAGE_SKILL_ROOTS="$roots" TMPDIR="$TMP" \
    bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "'chem' tooling recurred" ||
    fail "recurring uncovered usage should surface the forge proposal: $out"

  # A forged skill silences its family by pattern match, whatever the agent
  # named it — this SKILL.md never says "chem".
  mkdir -p "$repo/.oms/skills/ligand-ingestion"
  cat > "$repo/.oms/skills/ligand-ingestion/SKILL.md" <<'EOF'
---
name: ligand-ingestion
description: How this repository loads docking outputs through meeko and rdkit conversions, including malformed-molecule defense.
---

Use meeko RDKitMolCreate for DLG/PDBQT conversion.
EOF
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" OMS_USAGE_SKILL_ROOTS="$roots" TMPDIR="$TMP" \
    bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "tooling recurred"; then
    fail "a project skill matching the family pattern must silence the hint: $out"
  fi

  # A family owned by a linked global skill (covered_by) never hints.
  for _ in 1 2 3 4; do
    printf '{"schema": 1, "family": "gpu", "day": "%s"}\n' "$yesterday" \
      >> "$repo/.oms/usage.jsonl"
  done
  for _ in 1 2 3; do
    printf '{"schema": 1, "family": "gpu", "day": "%s"}\n' "$(date +%Y-%m-%d)" \
      >> "$repo/.oms/usage.jsonl"
  done
  mkdir -p "$roots/oms-gpu-workstation"
  rm -f "$repo/.oms/hooks/state-hint."*
  out="$(printf '%s' "$payload" |
    OMS_STATE_REPO="$repo" OMS_USAGE_SKILL_ROOTS="$roots" TMPDIR="$TMP" \
    bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "tooling recurred"; then
    fail "a linked global skill must silence its family: $out"
  fi

  # gc compacts under the reader's own TTL predicate: expired rows drop,
  # same-day rows collapse into count rows the reader sums.
  printf '{"schema": 1, "family": "gpu", "day": "2020-01-01"}\n' \
    >> "$repo/.oms/usage.jsonl"
  out="$(bash "$ROOT/scripts/gc.sh" --repo "$repo" --apply)"
  printf '%s' "$out" | grep -q 'usage: compact' ||
    fail "gc should compact usage rows: $out"
  if grep -q '2020-01-01' "$repo/.oms/usage.jsonl"; then
    fail "gc must drop usage rows past the TTL"
  fi
  grep -q '"count": 6' "$repo/.oms/usage.jsonl" ||
    fail "gc should collapse same-day rows into summed count rows: $(cat "$repo/.oms/usage.jsonl")"
}

# --- machine-conditional skills ---------------------------------------------

# A deterministic PATH: real tools by symlink, nothing else — so a command's
# absence can be asserted even on machines that actually have it.
make_toolbox() {
  local dir="$1"
  local cmd path

  mkdir -p "$dir"
  for cmd in bash sh python3 git ln mkdir rm rmdir mv cp ls basename dirname \
    grep sed find sort tr cat printf echo date mktemp chmod readlink id \
    uname wc head tail awk cmp touch env cut cksum sha256sum flock xargs \
    sleep; do
    path="$(command -v "$cmd" 2>/dev/null)" || continue
    ln -sf "$path" "$dir/$cmd"
  done
}

test_conditional_skills_link_only_where_required_commands_exist() {
  local toolbox="$TMP/cond-toolbox"
  local stub="$TMP/cond-stub"
  local home="$TMP/cond-home"
  local out

  make_toolbox "$toolbox"
  mkdir -p "$stub" "$home"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/sinfo"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/nvidia-smi"
  chmod +x "$stub/sinfo" "$stub/nvidia-smi"

  # Without the commands: base skills link, conditional skills do not.
  PATH="$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh" >/dev/null
  [ -e "$home/.codex/skills/oms-agent-harness" ] || fail "base skill missing"
  [ ! -e "$home/.codex/skills/oms-slurm" ] ||
    fail "oms-slurm skill linked without sinfo"
  [ ! -e "$home/.codex/skills/oms-gpu-workstation" ] ||
    fail "oms-gpu-workstation linked without nvidia-smi"

  # With the commands: both conditional skills appear.
  PATH="$stub:$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh" >/dev/null
  [ -e "$home/.codex/skills/oms-slurm" ] || fail "oms-slurm skill not linked with sinfo"
  [ -e "$home/.codex/skills/oms-gpu-workstation" ] ||
    fail "oms-gpu-workstation not linked with nvidia-smi"

  # Machine lost the commands: the next link converges by removing them.
  out="$(PATH="$toolbox" HOME="$home" bash "$ROOT/scripts/link.sh")"
  [ ! -e "$home/.codex/skills/oms-slurm" ] ||
    fail "oms-slurm link should be removed once sinfo is gone"
  printf '%s' "$out" | grep -Fq "unlinked disabled skill" ||
    fail "removal should be reported: $out"
}

test_router_skips_conditional_skill_without_command() {
  local toolbox="$TMP/router-toolbox"
  local stub="$TMP/router-stub"
  local repo="$TMP/router-cond-repo"
  local payload out

  make_toolbox "$toolbox"
  mkdir -p "$stub" "$repo"
  git -C "$repo" init -q
  printf '#!/usr/bin/env bash\nexit 0\n' > "$stub/sinfo"
  chmod +x "$stub/sinfo"
  payload='{"prompt":"sbatch로 잡 제출하고 squeue 봐줘","session_id":"s","turn_id":"t"}'

  out="$(printf '%s' "$payload" | PATH="$stub:$toolbox" \
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  printf '%s' "$out" | grep -Fq "oms-slurm" ||
    fail "router should suggest oms-slurm when sinfo exists: $out"

  out="$(printf '%s' "$payload" | PATH="$toolbox" \
    OMS_STATE_REPO="$repo" TMPDIR="$TMP" bash "$ROOT/scripts/skill-router.sh")"
  if printf '%s' "$out" | grep -Fq "oms-slurm"; then
    fail "router must not suggest oms-slurm without sinfo: $out"
  fi
}

# --- project skill forge ----------------------------------------------------

test_skill_forge_stores_links_and_hides() {
  local repo="$TMP/forge repo"
  local original="$TMP/original-skill"
  local out

  make_repo "$repo"
  cat <<'EOF' | bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name oms-build-quirks
---
name: oms-build-quirks
description: How this repository actually builds and tests, including the nonstandard invocations discovered by inspection rather than guessed.
---

# Build Quirks

- Tests run with `make check-fast`, not pytest directly.
EOF
  [ -f "$repo/.oms/skills/oms-build-quirks/SKILL.md" ] || fail "skill not stored"
  [ -L "$repo/.agents/skills/oms-build-quirks" ] || fail "not linked for codex/agy"
  [ -L "$repo/.claude/skills/oms-build-quirks" ] || fail "not linked for claude"
  grep -Fq ".agents/skills/oms-build-quirks" "$repo/.git/info/exclude" ||
    fail "codex/agy link not hidden from git"
  grep -Fq ".claude/skills/oms-build-quirks" "$repo/.git/info/exclude" ||
    fail "claude link not hidden from git"

  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status | grep -Fq "1 project skill(s) valid" ||
    fail "status should count the valid skill"

  cp "$repo/.oms/skills/oms-build-quirks/SKILL.md" "$original"
  if printf -- '---\nname: oms-build-quirks\ndescription: short\n---\nbody\n' |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name oms-build-quirks \
      >/dev/null 2>&1; then
    fail "add must refuse to overwrite an existing project skill"
  fi
  cmp -s "$original" "$repo/.oms/skills/oms-build-quirks/SKILL.md" ||
    fail "rejected replacement destroyed the existing project skill"

  # Invalidate the stored skill: the next link pass must withdraw the links
  # and status must fail loudly.
  printf 'no frontmatter\n' > "$repo/.oms/skills/oms-build-quirks/SKILL.md"
  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" link >/dev/null 2>&1
  [ ! -e "$repo/.agents/skills/oms-build-quirks" ] ||
    fail "invalid skill must be unlinked"
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status >/dev/null 2>&1; then
    fail "status must be nonzero with an invalid skill"
  fi

  bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" remove oms-build-quirks >/dev/null
  [ ! -d "$repo/.oms/skills/oms-build-quirks" ] || fail "remove left the skill"
}

test_skill_forge_status_flags_stale_skills() {
  local repo="$TMP/forge-stale"
  local skill="$repo/.oms/skills/oms-stale-lesson/SKILL.md"
  local out

  make_repo "$repo"
  cat <<'EOF' | bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name oms-stale-lesson >/dev/null
---
name: oms-stale-lesson
description: A durable project lesson that is long enough to meet the skill routing-quality validation requirement.
---

# Stale Lesson

Review this lesson periodically.
EOF
  touch -t 202001010000 "$skill"
  out="$(OMS_SKILL_STALE_DAYS=1 bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status)"
  printf '%s' "$out" | grep -Fq "1 project skill(s) untouched >1d" ||
    fail "stale skill warning should name the count: $out"

  out="$(OMS_SKILL_STALE_DAYS=0 bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status)"
  if printf '%s' "$out" | grep -Fq "untouched >"; then
    fail "OMS_SKILL_STALE_DAYS=0 must silence stale warnings: $out"
  fi

  touch "$skill"
  out="$(OMS_SKILL_STALE_DAYS=1 bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" status)"
  if printf '%s' "$out" | grep -Fq "untouched >"; then
    fail "fresh skills must not produce stale warnings: $out"
  fi
}

test_skill_forge_rejects_thin_and_sensitive() {
  local repo="$TMP/forge-reject"
  local protected="$repo/.oms/protected"
  local vector

  make_repo "$repo"
  if printf -- '---\nname: oms-bad\ndescription: short\n---\nbody\n' |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name oms-bad 2>/dev/null; then
    fail "a thin description must be rejected"
  fi
  [ ! -d "$repo/.oms/skills/oms-bad" ] || fail "rejected skill must not be stored"

  # Assembled at runtime so this test file stays scrubber-clean.
  vector="AK""IAIOSFODNN7EXAMPLE"
  if printf -- '---\nname: oms-leaky\ndescription: a sufficiently long description that satisfies the routing-quality floor\n---\nkey: %s\n' "$vector" |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name oms-leaky 2>/dev/null; then
    fail "secret-shaped content must be rejected"
  fi
  [ ! -d "$repo/.oms/skills/oms-leaky" ] || fail "sensitive skill must not be stored"

  # Shared-surface provenance: a new project skill without the oms- prefix
  # is refused at add with the corrected name, even when otherwise valid.
  local prefix_out
  if prefix_out="$(printf -- '---\nname: plain-lesson\ndescription: a sufficiently long description that satisfies the routing-quality floor\n---\n\nBody.\n' |
    bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" add --name plain-lesson 2>&1)"; then
    fail "an unprefixed skill name must be rejected at add"
  fi
  printf '%s' "$prefix_out" | grep -Fq "oms-plain-lesson" ||
    fail "prefix rejection should suggest the corrected name: $prefix_out"
  [ ! -d "$repo/.oms/skills/plain-lesson" ] ||
    fail "rejected unprefixed skill must not be stored"

  mkdir -p "$protected"
  printf 'must survive\n' > "$protected/SKILL.md"
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" show ../protected \
    >/dev/null 2>&1; then
    fail "show must reject a traversal name"
  fi
  if bash "$ROOT/scripts/skill-forge.sh" --repo "$repo" remove ../protected \
    >/dev/null 2>&1; then
    fail "remove must reject a traversal name"
  fi
  [ -f "$protected/SKILL.md" ] ||
    fail "traversal remove escaped the project skills directory"
}

test_task_close_hints_at_forging_learned_lessons() {
  local repo="$TMP/forge-hint"
  local out

  make_repo "$repo"
  ( cd "$repo" &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make flaky" --exit 1 >/dev/null &&
    OMS_ADVISE_AFTER_FAILURES=0 bash "$ROOT/scripts/fail-ledger.sh" record --cmd "make flaky" --exit 1 >/dev/null &&
    bash "$ROOT/scripts/fail-ledger.sh" resolve --cmd "make flaky" >/dev/null )
  bash "$ROOT/scripts/agent-task.sh" --repo "$repo" init --goal "lesson" >/dev/null
  out="$(bash "$ROOT/scripts/agent-task.sh" --repo "$repo" close 2>&1 || true)"
  printf '%s' "$out" | grep -Fq "skill-forge add" ||
    fail "close should hint at promoting a resolved repeated failure: $out"
}

test_template_style_switch_retires_the_old_block() {
  local project="$TMP/style-switch"

  mkdir -p "$project"
  git -C "$project" init -q 2>/dev/null || true
  bash "$ROOT/scripts/apply-project-template.sh" general "$project" >/dev/null 2>&1 ||
    fail "general template apply failed"
  grep -Fq "oh-my-setting:general:begin" "$project/AGENTS.md" ||
    fail "general block missing after apply"

  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "ml template apply failed"
  grep -Fq "oh-my-setting:ml:begin" "$project/AGENTS.md" ||
    fail "ml block missing after switch"
  # Two loaders with different rules is the bug this guards: the retired
  # style must be gone from every managed file, not merely superseded.
  local f
  for f in AGENTS.md CLAUDE.md; do
    if grep -Fq "oh-my-setting:general:begin" "$project/$f"; then
      fail "stale general block survived the switch in $f"
    fi
  done

  # The slurm overlay is additive and follows machine detection, so a
  # re-apply from a machine without Slurm must not strip cluster rules a
  # project deliberately carries. Only an explicit remove does that.
  bash "$ROOT/scripts/apply-project-template.sh" slurm "$project" >/dev/null 2>&1 ||
    fail "slurm overlay apply failed"
  grep -Fq "oh-my-setting:slurm:begin" "$project/AGENTS.md" ||
    fail "slurm overlay missing after apply"
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "re-applying ml failed"
  grep -Fq "oh-my-setting:slurm:begin" "$project/AGENTS.md" ||
    fail "re-applying a base style must not strip the slurm overlay"
}

test_existing_gemini_md_is_kept_in_sync() {
  local project="$TMP/gemini-sync"

  mkdir -p "$project"
  git -C "$project" init -q 2>/dev/null || true
  # A project that already carries GEMINI.md: Antigravity reads it and
  # prefers it over AGENTS.md, so leaving it unmanaged makes one CLI follow
  # rules the other two retired.
  printf '<!-- oh-my-setting:general:begin -->\n\nstale loader\n\n<!-- oh-my-setting:general:end -->\n' \
    > "$project/GEMINI.md"
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "ml template apply failed"

  grep -Fq "project-ml-AGENTS.md" "$project/GEMINI.md" ||
    fail "an existing GEMINI.md must receive the new loader"
  if grep -Fq "oh-my-setting:general:begin" "$project/GEMINI.md"; then
    fail "the retired style must not survive in GEMINI.md"
  fi

  # A project without one stays at two loader files: agy reads AGENTS.md, so
  # a third file would be footprint without benefit.
  local plain="$TMP/gemini-absent"
  mkdir -p "$plain"
  bash "$ROOT/scripts/apply-project-template.sh" ml "$plain" >/dev/null 2>&1 ||
    fail "ml template apply failed for the plain project"
  [ ! -e "$plain/GEMINI.md" ] || fail "the template must not create GEMINI.md"
}

test_ml_template_installs_project_skills() {
  local project="$TMP/ml-skills-proj"

  mkdir -p "$project"
  git -C "$project" init -q 2>/dev/null || true
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "ml template apply failed"
  [ -f "$project/.oms/skills/oms-ml-experiment/SKILL.md" ] ||
    fail "oms-ml-experiment project skill not installed"
  [ -f "$project/.oms/skills/oms-dataset-safety/SKILL.md" ] ||
    fail "oms-dataset-safety project skill not installed"
  [ -L "$project/.agents/skills/oms-ml-experiment" ] ||
    fail "oms-ml-experiment not linked for native discovery"
  bash "$ROOT/scripts/skill-forge.sh" --repo "$project" status |
    grep -Fq "2 project skill(s) valid" || fail "installed skills should validate"

  # Idempotent: a second apply must not fail on the existing skills.
  bash "$ROOT/scripts/apply-project-template.sh" ml "$project" >/dev/null 2>&1 ||
    fail "re-applying the ml template should be idempotent"

  # A project that applied the template before the oms- rename keeps its
  # stored skill under the legacy name; re-applying must not install a
  # second copy of the same lesson beside it.
  local legacy="$TMP/ml-skills-legacy"
  mkdir -p "$legacy/.oms/skills/ml-experiment"
  git -C "$legacy" init -q 2>/dev/null || true
  cp "$ROOT/templates/project-skills/oms-ml-experiment/SKILL.md" \
    "$legacy/.oms/skills/ml-experiment/SKILL.md"
  sed -i.bak 's/^name: oms-ml-experiment$/name: ml-experiment/' \
    "$legacy/.oms/skills/ml-experiment/SKILL.md" &&
    rm -f "$legacy/.oms/skills/ml-experiment/SKILL.md.bak"
  bash "$ROOT/scripts/apply-project-template.sh" ml "$legacy" >/dev/null 2>&1 ||
    fail "ml template apply over a legacy-name skill failed"
  [ ! -e "$legacy/.oms/skills/oms-ml-experiment" ] ||
    fail "legacy ml-experiment must suppress the prefixed duplicate"
  [ -f "$legacy/.oms/skills/oms-dataset-safety/SKILL.md" ] ||
    fail "absent skills must still install beside a legacy one"

  # A general project gets none.
  local plain="$TMP/general-skills-proj"
  mkdir -p "$plain"
  bash "$ROOT/scripts/apply-project-template.sh" general "$plain" >/dev/null 2>&1 ||
    fail "general template apply failed"
  [ ! -d "$plain/.oms/skills" ] || fail "general template must not install ML skills"
}

# --- slurm generator rename -------------------------------------------------

test_slurm_generator_has_one_front_door() {
  bash "$ROOT/scripts/generate-slurm-reference.sh" --help >/dev/null ||
    fail "generate-slurm-reference --help failed"
  # The generator is internal implementation behind oms snapshot --cluster;
  # the old dispatcher names are refused, not aliased.
  bash "$ROOT/scripts/oms" snapshot --cluster --help >/dev/null ||
    fail "oms snapshot --cluster front door failed"
  rc=0
  bash "$ROOT/scripts/oms" generate-slurm-skill --help >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "the retired generate-slurm-skill alias must be refused, got $rc"
  rc=0
  bash "$ROOT/scripts/oms" generate-slurm-reference --help >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "the internal generator name must not dispatch, got $rc"
}

test_mcp_peer_start_refuses_harness_children() {
  local repo="$TMP/mcp-child-repo"
  local out="$TMP/mcp-child-out"

  make_repo "$repo"
  # A provider CLI this harness spawned carries OMS_HARNESS_CHILD, and the
  # MCP server it starts inherits it: a worker asking for another peer is
  # recursive delegation, which is the owner's decision. Server-side, so it
  # holds for every provider whatever the CLI's own flags allow.
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"consult","prompt":"probe"}}}\n' "$repo"
  } | OMS_HARNESS_CHILD=1 python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"
  grep -Fq 'a delegated worker cannot start peer consultations' "$out" ||
    fail "a harness child must be refused a peer start: $(cat "$out")"

  # Without the marker the guard must not fire: an invalid kind reaching kind
  # validation proves the request passed the guard.
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"oms_peer_start","arguments":{"repo":"%s","kind":"nope","prompt":"probe"}}}\n' "$repo"
  } | env -u OMS_HARNESS_CHILD python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"
  grep -Fq 'kind must be one of' "$out" ||
    fail "an owner session must pass the child guard: $(cat "$out")"
}

test_mcp_tasks_extension_is_opt_in_and_reuses_peer_operations() {
  local repo="$TMP/mcp-tasks-repo"
  local out="$TMP/mcp-tasks-out"
  local driver="$TMP/mcp-tasks-driver.py"

  make_repo "$repo"
  mkdir -p "$TMP/peer-bin" "$TMP/peer-home" "$TMP/peer-locks"
  cat > "$TMP/peer-bin/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
tries=0
while [ ! -f "$STUB_GATE" ] && [ "$tries" -lt 120 ]; do
  sleep 1
  tries=$((tries + 1))
done
printf 'TASK-STUB-ANSWER\n'
EOF
  chmod +x "$TMP/peer-bin/codex"
  rm -f "$TMP/peer-gate"

  cat > "$driver" <<'PY'
import json
import os
import subprocess
import sys
import time

root, repo, gate = sys.argv[1:]
env = dict(os.environ)
env["OMS_MCP_TASKS_EXTENSION"] = "1"
p = subprocess.Popen(
    [sys.executable, os.path.join(root, "scripts", "oms-mcp-server.py")],
    cwd=repo,
    env=env,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)

def call(message):
    p.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    p.stdin.flush()
    line = p.stdout.readline()
    assert line, p.stderr.read()
    return json.loads(line)

caps = {
    "io.modelcontextprotocol/clientCapabilities": {
        "extensions": {"io.modelcontextprotocol/tasks": {}}
    }
}
init = call({
    "jsonrpc": "2.0", "id": 1, "method": "initialize",
    "params": {"protocolVersion": "2026-07-28", "capabilities": {},
               "clientInfo": {"name": "fixture", "version": "1"}},
})
assert init["result"]["protocolVersion"] == "2026-07-28", init
assert "io.modelcontextprotocol/tasks" in init["result"]["capabilities"]["extensions"], init

# A task id is not authority to follow a repository-controlled directory
# alias. Cancellation writes one receipt, so every existing ancestry
# component must be a real directory before that method can be reached.
escape_id = "consult-20260827T000000Z-deadbeef"
outside = os.path.join(os.path.dirname(repo), "mcp-task-escape")
os.makedirs(outside)
os.makedirs(os.path.join(repo, ".oms", "artifacts", "mcp"), exist_ok=True)
os.symlink(outside, os.path.join(repo, ".oms", "artifacts", "mcp", escape_id))
escaped = call({
    "jsonrpc": "2.0", "id": 90, "method": "tasks/cancel",
    "params": {"taskId": escape_id, "_meta": caps},
})
assert escaped["error"]["code"] == -32602, escaped
assert not os.path.exists(os.path.join(outside, "cancel-request.json")), escaped

# A client that does not opt in on this individual call gets the legacy
# CallToolResult byte shape even though the server feature is enabled.
legacy = call({
    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
    "params": {"name": "oms_peer_start", "arguments": {
        "kind": "consult", "prompt": "   "}},
})
assert legacy["result"]["resultType"] == "complete", legacy
assert legacy["result"]["isError"] is True, legacy
assert "taskId" not in legacy["result"], legacy

created = call({
    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
    "params": {"name": "oms_peer_start", "arguments": {
        "kind": "consult", "prompt": "hold until cancelled", "providers": "codex"},
        "_meta": caps},
})
task = created["result"]
assert task["resultType"] == "task" and task["status"] == "working", task
task_id = task["taskId"]
assert os.path.isdir(os.path.join(repo, ".oms", "artifacts", "mcp", task_id)), task

missing = call({
    "jsonrpc": "2.0", "id": 4, "method": "tasks/get",
    "params": {"taskId": task_id},
})
assert missing["error"]["code"] == -32003, missing

working = call({
    "jsonrpc": "2.0", "id": 5, "method": "tasks/get",
    "params": {"taskId": task_id, "_meta": caps},
})
assert working["result"]["resultType"] == "complete", working
assert working["result"]["status"] == "working", working

update = call({
    "jsonrpc": "2.0", "id": 6, "method": "tasks/update",
    "params": {"taskId": task_id, "inputResponses": {}, "_meta": caps},
})
assert update["error"]["code"] == -32602, update
assert "input" in update["error"]["message"], update

cancelled = call({
    "jsonrpc": "2.0", "id": 7, "method": "tasks/cancel",
    "params": {"taskId": task_id, "_meta": caps},
})
assert cancelled["result"] == {"resultType": "complete"}, cancelled
after = call({
    "jsonrpc": "2.0", "id": 8, "method": "tasks/get",
    "params": {"taskId": task_id, "_meta": caps},
})
assert after["result"]["status"] == "cancelled", after
assert "result" not in after["result"] and "error" not in after["result"], after

# Cancellation is cooperative. Release the read-only fixture peer so no
# process survives the test even if it had already entered the provider CLI.
open(gate, "w", encoding="utf-8").close()
status_file = os.path.join(repo, ".oms", "artifacts", "mcp", task_id, "status")
deadline = time.time() + 10
while time.time() < deadline and not os.path.isfile(status_file):
    time.sleep(0.05)
assert os.path.isfile(status_file), "cancelled fixture peer did not exit after release"
p.stdin.close()
p.wait(timeout=10)
assert p.returncode == 0, p.stderr.read()
PY

  env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CODEX_SANDBOX \
    HOME="$TMP/peer-home" PATH="$TMP/peer-bin:$PATH" \
    OMS_LOCK_DIR="$TMP/peer-locks" OMS_LOCK_FORCE_MKDIR=1 \
    STUB_GATE="$TMP/peer-gate" \
    python3 "$driver" "$ROOT" "$repo" "$TMP/peer-gate" > "$out" 2>&1 ||
    fail "MCP Tasks extension contract failed: $(cat "$out")"

  # Disabling the server feature suppresses both discovery and task methods.
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{"taskId":"consult-20260827T000000Z-deadbeef","_meta":{"io.modelcontextprotocol/clientCapabilities":{"extensions":{"io.modelcontextprotocol/tasks":{}}}}}}'
  } | OMS_MCP_TASKS_EXTENSION=0 python3 "$ROOT/scripts/oms-mcp-server.py" > "$out"
  OMS_T_OUT="$out" python3 - <<'PY' || fail "disabled MCP Tasks surface was exposed"
import json, os
rows = [json.loads(line) for line in open(os.environ["OMS_T_OUT"], encoding="utf-8")]
assert "extensions" not in rows[0]["result"]["capabilities"], rows[0]
assert rows[1]["error"]["code"] == -32601, rows[1]
PY
}

test_mcp_server_protocol
test_mcp_peer_start_refuses_harness_children
test_mcp_tasks_extension_is_opt_in_and_reuses_peer_operations
test_mcp_server_bounds_requests_before_effects
test_mcp_peer_actions_start_detached_and_poll
test_mcp_peer_result_reads_the_answer_not_the_quoted_prompt
test_mcp_peer_operations_report_dead_and_legacy_runs
test_oms_state_inventory_excludes_only_session_owned_entries
test_install_mcp_registers_and_is_idempotent
test_install_agy_plugin_bakes_absolute_paths
test_agy_surfaces_are_certified_before_hooks_ship
test_agy_surfaces_stay_mcp_only_when_hooks_never_fire
test_update_probe_flag_reaches_the_probe
test_router_state_hint_on_unresolved_failures
test_router_state_hint_surfaces_parked_goal
test_router_state_hint_skips_unadopted_repo
test_router_state_hint_offers_forge_for_resolved_repeats
test_router_hints_on_recurring_uncovered_usage
test_conditional_skills_link_only_where_required_commands_exist
test_router_skips_conditional_skill_without_command
test_skill_forge_stores_links_and_hides
test_skill_forge_status_flags_stale_skills
test_skill_forge_rejects_thin_and_sensitive
test_task_close_hints_at_forging_learned_lessons
test_ml_template_installs_project_skills
test_template_style_switch_retires_the_old_block
test_existing_gemini_md_is_kept_in_sync
test_slurm_generator_has_one_front_door

echo "state-surfaces-smoke: ok"
