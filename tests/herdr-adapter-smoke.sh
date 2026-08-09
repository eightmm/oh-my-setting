#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-herdr-adapter.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "herdr-adapter-smoke: $*" >&2; exit 1; }
ADAPTER="$ROOT/scripts/herdr-adapter.sh"
FAKE="$TMP/herdr"
LOG="$TMP/argv.jsonl"
ENV_LOG="$TMP/env.jsonl"

[ -x "$ADAPTER" ] || fail "missing executable: scripts/herdr-adapter.sh"

cat > "$FAKE" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
python3 - "$HERDR_TEST_LOG" "$@" <<'PY'
import json, sys
with open(sys.argv[1], "a", encoding="utf-8") as f:
    f.write(json.dumps(sys.argv[2:], separators=(",", ":")) + "\n")
PY
python3 - "$HERDR_TEST_ENV_LOG" <<'PY'
import json, os, sys
keys = ["OMS_PLAN_LEASE_ID", "OMS_LEASE_ID", "OMS_HARNESS_CHILD",
        "OMS_APPROVAL_ID", "OMS_LANDING_ID"]
with open(sys.argv[1], "a", encoding="utf-8") as f:
    f.write(json.dumps({k: os.environ.get(k) for k in keys}, sort_keys=True) + "\n")
PY

case "$*" in
  'api schema --json')
    printf '{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}\n';;
  'workspace create --help')
    echo 'workspace create --cwd PATH --label TEXT --no-focus';;
  'pane split --help')
    echo 'pane split PANE --direction right|down --cwd PATH --no-focus';;
  'agent start --help')
    echo 'agent start NAME --kind KIND --pane PANE --timeout MS -- ARGS';;
  'agent prompt --help')
    echo 'agent prompt TARGET TEXT --wait --timeout MS --until STATE';;
  'agent wait --help')
    echo 'agent wait TARGET --until STATE --timeout MS';;
  'agent read --help')
    echo 'agent read TARGET --source SOURCE --lines N';;
  'agent get --help')
    echo 'agent get TARGET';;
  workspace\ create\ --cwd\ *\ --label\ *\ --no-focus)
    printf '{"result":{"workspace":{"workspace_id":"w1"},"tab":{"tab_id":"w1:t1"},"root_pane":{"pane_id":"w1:p1"}}}\n';;
  pane\ split\ *\ --direction\ *\ --no-focus|pane\ split\ *\ --direction\ *\ --cwd\ *\ --no-focus)
    printf '{"result":{"pane":{"pane_id":"w1:p2"}}}\n';;
  agent\ start\ *)
    printf '{"result":{"agent":{"name":"reviewer","state":"idle"}}}\n';;
  agent\ prompt\ *)
    printf '{"result":{"agent":{"name":"reviewer","state":"done"}}}\n';;
  agent\ wait\ *)
    printf '{"result":{"agent":{"name":"reviewer","state":"blocked"}}}\n';;
  agent\ read\ *)
    printf 'bounded agent output\n';;
  agent\ get\ *)
    if [ "${HERDR_TEST_BAD_JSON:-0}" = 1 ]; then
      printf 'not-json\n'
    else
      printf '{"result":{"agent":{"name":"reviewer","state":"idle"}}}\n'
    fi
    ;;
  *) echo "unexpected fake herdr argv: $*" >&2; exit 11;;
esac
EOF
chmod +x "$FAKE"
export HERDR_TEST_LOG="$LOG" HERDR_TEST_ENV_LOG="$ENV_LOG"
export OMS_PLAN_LEASE_ID=secret-plan OMS_LEASE_ID=secret-lease
export OMS_APPROVAL_ID=approval-secret OMS_LANDING_ID=landing-secret
unset OMS_HARNESS_CHILD

run_adapter() {
  OMS_HERDR_BIN="$FAKE" "$ADAPTER" "$@"
}

run_adapter check --json > "$TMP/check.json"
python3 - "$TMP/check.json" <<'PY' || fail "feature check output is invalid"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["ok"] is True and d["adapter"] == "herdr", d
assert set(d["features"]) == {
    "workspace_create", "pane_split", "agent_start", "agent_prompt",
    "agent_wait", "agent_read", "agent_status"}, d
assert d["authority"] == {"approval": False, "landing": False}, d
assert d["frontend_authority"] == "none", d
assert d["managed_by_oms"] is False, d
assert d["success_authority"] is False, d
assert d["state_semantics"] == "presence_only", d
assert d["contract_probe"] == "api_schema", d
assert d["native_session_identity"] == "conditional", d
PY
python3 - "$LOG" <<'PY' || fail "feature detection used unexpected commands"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
assert rows == [
  ["api", "schema", "--json"],
  ["workspace", "create", "--help"],
  ["pane", "split", "--help"],
  ["agent", "start", "--help"],
  ["agent", "prompt", "--help"],
  ["agent", "wait", "--help"],
  ["agent", "read", "--help"],
  ["agent", "get", "--help"],
], rows
PY
: > "$LOG"

run_adapter workspace-create --cwd "$ROOT" --label 'OMS review' > "$TMP/workspace.json"
run_adapter pane-split --pane w1:p1 --direction right --cwd "$ROOT" > "$TMP/pane.json"
run_adapter agent-start --name reviewer --kind codex --pane w1:p2 \
  --timeout-ms 9000 -- -m gpt-test > "$TMP/start.json"
run_adapter agent-prompt --target reviewer --prompt 'Review current diff' \
  --timeout-ms 12000 > "$TMP/prompt.json"
run_adapter agent-wait --target reviewer --until blocked \
  --timeout-ms 13000 > "$TMP/wait.json"
run_adapter agent-read --target reviewer --source recent-unwrapped \
  --lines 80 > "$TMP/read.txt"
run_adapter agent-status --target reviewer > "$TMP/status.json"

if ! python3 - "$TMP/workspace.json" "$TMP/pane.json" "$TMP/start.json" \
  "$TMP/prompt.json" "$TMP/wait.json" "$TMP/status.json" <<'PY'
import json, sys
for path in sys.argv[1:]:
    row = json.load(open(path, encoding="utf-8"))
    assert row["frontend_authority"] == "none", (path, row)
    assert row["managed_by_oms"] is False, (path, row)
    assert row["success_authority"] is False, (path, row)
    assert row["state_semantics"] == "presence_only", (path, row)
PY
then
  fail "Herdr JSON must disclose its non-authoritative state semantics"
fi

grep -Fxq 'bounded agent output' "$TMP/read.txt" || fail "agent read output changed"
python3 - "$LOG" "$ROOT" <<'PY' || fail "adapter argv contract changed"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
root = sys.argv[2]
assert rows == [
  ["workspace", "create", "--help"],
  ["workspace", "create", "--cwd", root, "--label", "OMS review", "--no-focus"],
  ["pane", "split", "--help"],
  ["pane", "split", "w1:p1", "--direction", "right", "--cwd", root, "--no-focus"],
  ["agent", "start", "--help"],
  ["agent", "start", "reviewer", "--kind", "codex", "--pane", "w1:p2", "--timeout", "9000", "--", "-m", "gpt-test"],
  ["agent", "prompt", "--help"],
  ["agent", "prompt", "reviewer", "Review current diff", "--wait", "--timeout", "12000"],
  ["agent", "wait", "--help"],
  ["agent", "wait", "reviewer", "--until", "blocked", "--timeout", "13000"],
  ["agent", "read", "--help"],
  ["agent", "read", "reviewer", "--source", "recent-unwrapped", "--lines", "80"],
  ["agent", "get", "--help"],
  ["agent", "get", "reviewer"],
], rows
PY

python3 - "$ENV_LOG" <<'PY' || fail "authority environment leaked to Herdr"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
assert rows, rows
for row in rows:
    assert all(value is None for value in row.values()), row
PY

: > "$LOG"
run_adapter agent-prompt --target reviewer --prompt bounded > /dev/null
run_adapter agent-wait --target reviewer --until 'done' > /dev/null
run_adapter agent-start --name reviewer --kind codex --pane w1:p2 -- true > /dev/null
python3 - "$LOG" <<'PY' || fail "default timeouts were not enforced"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
actual = [row for row in rows if "--help" not in row]
assert actual == [
  ["agent", "prompt", "reviewer", "bounded", "--wait", "--timeout", "120000"],
  ["agent", "wait", "reviewer", "--until", "done", "--timeout", "120000"],
  ["agent", "start", "reviewer", "--kind", "codex", "--pane", "w1:p2", "--timeout", "30000", "--", "true"],
], actual
PY

# A delegated worker may observe existing Herdr state, but it cannot use this
# wrapper to fan out another costly/mutating agent flow. That is a recursion
# and budget policy, not an OS sandbox claim.
for child_action in \
  "workspace-create --cwd $ROOT --label child" \
  "pane-split --pane w1:p1 --direction right" \
  "agent-start --name child --kind codex --pane w1:p2 -- true" \
  "agent-prompt --target reviewer --prompt child"; do
  set +e
  # shellcheck disable=SC2086 # Each fixture row is an intentional argv vector.
  OMS_HARNESS_CHILD=1 run_adapter $child_action \
    > "$TMP/child.out" 2> "$TMP/child.err"
  rc=$?
  set -e
  [ "$rc" -eq 2 ] || fail "delegated child mutation should exit 2: $child_action"
  grep -Fq 'delegated child cannot start or mutate a Herdr flow' "$TMP/child.err" ||
    fail "delegated child mutation refusal is unclear: $child_action"
done
OMS_HARNESS_CHILD=1 run_adapter agent-status --target reviewer > "$TMP/child-status.json"
OMS_HARNESS_CHILD=1 run_adapter agent-wait --target reviewer --until idle \
  --timeout-ms 9000 > "$TMP/child-wait.json"

set +e
run_adapter approve --target reviewer > "$TMP/approve.out" 2> "$TMP/approve.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "approval command should not exist"
grep -Fq 'unknown command: approve' "$TMP/approve.err" ||
  fail "approval refusal is unclear"

cat > "$TMP/herdr-old" <<'EOF'
#!/usr/bin/env bash
if [ "$*" = 'agent prompt --help' ]; then echo 'agent prompt TARGET TEXT'; exit 0; fi
echo 'unsupported' >&2
exit 1
EOF
chmod +x "$TMP/herdr-old"
set +e
OMS_HERDR_BIN="$TMP/herdr-old" "$ADAPTER" agent-prompt \
  --target reviewer --prompt bounded > "$TMP/old.out" 2> "$TMP/old.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "old Herdr should fail feature detection"
grep -Fq 'missing required feature' "$TMP/old.err" ||
  fail "old Herdr feature failure is unclear"

set +e
HERDR_TEST_BAD_JSON=1 run_adapter agent-status --target reviewer \
  > "$TMP/bad-json.out" 2> "$TMP/bad-json.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "malformed Herdr JSON should exit 2, got $rc"
grep -Fq 'Herdr returned invalid agent JSON' "$TMP/bad-json.err" ||
  fail "malformed Herdr JSON failure is unclear"

echo "herdr-adapter-smoke: ok"
