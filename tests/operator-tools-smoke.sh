#!/usr/bin/env bash
set -euo pipefail

# Focused regressions for the read-only operator cockpit, content-free OTLP
# JSONL export, explicit editor launch adapters, and retired semantic-eval
# migration. Every fixture lives below TMP; no real provider or GUI is called.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-operator-tools.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

repo="$TMP/repo with space"
mkdir -p "$repo/.oms/artifacts" "$repo/.oms/hooks" "$repo/.oms/plan" \
  "$repo/.oms/delegations" "$repo/.oms/lifecycle" \
  "$TMP/state/oh-my-setting/approvals"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
printf 'old\n' > "$repo/value.txt"
printf '*\n' > "$repo/.oms/.gitignore"
git -C "$repo" add value.txt
git -C "$repo" commit -qm init
base_sha="$(git -C "$repo" rev-parse HEAD)"

# One real patch subject for the artifact readers. Restore the fixture
# immediately so read-only tools must leave the primary checkout unchanged.
printf 'new\n' > "$repo/value.txt"
git -C "$repo" diff --binary -- value.txt > "$repo/.oms/artifacts/change.patch"
git -C "$repo" checkout -- value.txt

OMS_FIXTURE_REPO="$repo" OMS_FIXTURE_BASE="$base_sha" OMS_FIXTURE_PID="$$" \
  OMS_FIXTURE_STATE="$TMP/state" \
python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path

repo = Path(os.environ["OMS_FIXTURE_REPO"])
state_home = Path(os.environ["OMS_FIXTURE_STATE"])
base = os.environ["OMS_FIXTURE_BASE"]
patch = repo / ".oms" / "artifacts" / "change.patch"
patch_hash = hashlib.sha256(patch.read_bytes()).hexdigest()
rows = [
    {
        "schema": 1,
        "event_id": "evt_call",
        "operation_id": "op_one",
        "artifact_id": "sha256:" + "1" * 64,
        "ts": "2026-08-01T00:00:05Z",
        "kind": "call",
        "provider": "codex",
        "exit": 0,
        "verify_exit": "PRIVATE-VERIFY-EXIT",
        "run_id": "run_one",
        "selected_model": "gpt-test",
        "served_model": "gpt-served",
        "model_attribution": "transport",
        "cost_usd": 0.25,
        "model_class": "explicit",
        "reasoning_effort": "medium",
        "fallback_reason": "SECRET FALLBACK CONTENT",
        "duration_s": 5.0,
        "tokens": 120,
        "trace_id": "a" * 32,
        "span_id": "b" * 16,
        "parent_span_id": "c" * 16,
        "trace_flags": "01",
        "task_goal": "PRIVATE GOAL MUST NOT REACH OTLP",
    },
    {
        "schema": 1,
        "event_id": "evt_review",
        "parent_event_id": "evt_call",
        "operation_id": "op_one",
        "artifact_id": "sha256:" + "2" * 64,
        "ts": "2026-08-01T00:00:07Z",
        "kind": "review",
        "provider": "claude",
        "exit": 1,
        "run_id": "run_other",
        "duration_s": 2.0,
    },
    {
        "schema": 1,
        "event_id": "evt_subject",
        "operation_id": "op_subject",
        "artifact_id": "sha256:" + patch_hash,
        "ts": "2026-08-01T00:00:10Z",
        "kind": "delegate",
        "provider": "codex",
        "exit": 0,
        "base_sha": base,
        "patch": ".oms/artifacts/change.patch",
        "patch_sha256": patch_hash,
        "verify_exit": 0,
    },
]
with (repo / ".oms/artifacts/index.jsonl").open("w", encoding="utf-8") as handle:
    for row in rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")

events = [
    {
        "schema": 1,
        "ts": "2026-08-01T00:00:11Z",
        "action": "telemetry",
        "agent": "codex",
        "hook": "PostToolUse",
        "session": "a" * 32,
        "tool_name": "shell",
        "success": False,
        "duration_ms": 125,
        "input_tokens": 7,
        "output_tokens": "PRIVATE-HOOK-TOKENS",
        "model": "SECRET HOOK MODEL",
        "private": "SECRET-HOOK-CONTENT",
    }
]
with (repo / ".oms/hooks/events.jsonl").open("w", encoding="utf-8") as handle:
    for row in events:
        handle.write(json.dumps(row, sort_keys=True) + "\n")

lifecycle = [
    {
        "schema": 1,
        "event_id": "levt_created",
        "attempt_id": "att_one",
        "seq": 1,
        "ts": "2026-08-01T00:00:01Z",
        "event_type": "attempt.created",
        "from_state": None,
        "to_state": "queued",
        "provider": "codex",
        "tool": "agent-supervisor",
        "run_id": "run_one",
        "task_id": "task_one",
    },
    {
        "schema": 1,
        "event_id": "levt_usage",
        "attempt_id": "att_one",
        "seq": 2,
        "ts": "2026-08-01T00:00:02Z",
        "event_type": "attempt.usage",
        "usage": {"tokens": 12, "cost_microusd": 7, "duration_ms": 25},
        "actor": {"kind": "provider", "name": "provider-output-parser"},
        "private": "PRIVATE LIFECYCLE CONTENT",
    },
    {
        "schema": 1,
        "event_id": "levt_cancelled",
        "attempt_id": "att_one",
        "seq": 3,
        "ts": "2026-08-01T00:00:03Z",
        "event_type": "attempt.state_changed",
        "from_state": "queued",
        "to_state": "cancelled",
    },
]
with (repo / ".oms/lifecycle/events.jsonl").open("w", encoding="utf-8") as handle:
    for row in lifecycle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")

repo_hash = hashlib.sha256(str(repo.resolve()).encode("utf-8")).hexdigest()
approval_rows = [
    {
        "schema": 1,
        "event_id": "apevt_requested",
        "approval_id": "apr_one",
        "version": 1,
        "ts": "2026-08-01T00:00:04Z",
        "event_type": "approval.requested",
        "state": "requested",
        "attempt_id": "att_one",
        "task_id": "task_one",
        "action": "patch-land",
        "summary": "PRIVATE APPROVAL SUMMARY",
    },
    {
        "schema": 1,
        "event_id": "apevt_consumed",
        "approval_id": "apr_one",
        "version": 2,
        "ts": "2026-08-01T00:00:09Z",
        "event_type": "approval.consumed",
        "state": "consumed",
        "expected_version": 1,
    },
]
with (state_home / "oh-my-setting" / "approvals" / (repo_hash + ".jsonl")).open(
    "w", encoding="utf-8"
) as handle:
    for row in approval_rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")

landing_rows = [
    {
        "schema": 1,
        "landing_id": "land_one",
        "ts": "2026-08-01T00:00:08Z",
        "event": "intent",
        "task": "task_one",
        "approval": "apr_one",
        "patch": "PRIVATE PATCH PATH",
    },
    {
        "schema": 1,
        "landing_id": "land_one",
        "ts": "2026-08-01T00:00:10Z",
        "event": "complete",
    },
]
with (repo / ".oms/landings.jsonl").open("w", encoding="utf-8") as handle:
    for row in landing_rows:
        handle.write(json.dumps(row, sort_keys=True) + "\n")

plan = {
    "schema": 2,
    "goal": "operator fixture",
    "tasks": {
        "review_task": {
            "id": "review_task",
            "state": "review",
            "updated": "2099-01-01T00:00:00Z",
            "depends": [],
        },
        "ready_task": {"id": "ready_task", "state": "ready", "depends": []},
    },
}
(repo / ".oms/plan/tasks.json").write_text(
    json.dumps(plan, sort_keys=True) + "\n", encoding="utf-8"
)
for name, pid in (("live", int(os.environ["OMS_FIXTURE_PID"])), ("dead", 99999999)):
    marker = {
        "schema": 2,
        "id": name,
        "provider": "codex",
        "pid": pid,
        "started_at": "2026-08-01T00:00:00Z",
        "state": "running",
    }
    (repo / ".oms/delegations" / (name + ".json")).write_text(
        json.dumps(marker, sort_keys=True) + "\n", encoding="utf-8"
    )
PY

# --- One attention view over shared state ----------------------------------

before="$(git -C "$repo" status --porcelain=v1 --untracked-files=all)"
bash "$ROOT/scripts/state.sh" --repo "$repo" --json > "$TMP/shared-state.json"
bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json > "$TMP/inbox.json"
python3 - "$TMP/shared-state.json" "$TMP/inbox.json" "$ROOT" <<'PY' || fail "inbox projection failed"
import copy
import json
import os
import sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[3]) / "scripts/lib"))
from inbox_projection import project_inbox

snapshot = json.load(open(sys.argv[1], encoding="utf-8"))
inbox = json.load(open(sys.argv[2], encoding="utf-8"))
assert project_inbox(snapshot, include_threads=os.environ.get("OMS_THREAD_ATTENTION") != "0") == inbox
assert inbox["items"][0]["priority"] == "P1", inbox
assert "runtime-evidence-missing" in [item["code"] for item in inbox["items"]]
snapshot["threads"] = {"stale_open": 2}
original = copy.deepcopy(snapshot)
assert any(item["code"] == "stale-threads" for item in project_inbox(snapshot)["items"])
muted = project_inbox(snapshot, include_threads=False, safe_actions=["reclaimed-stale-plan"])
assert not any(item["code"] == "stale-threads" for item in muted["items"])
assert muted["safe_actions"] == ["reclaimed-stale-plan"]
assert snapshot == original
PY
after="$(git -C "$repo" status --porcelain=v1 --untracked-files=all)"
[ "$before" = "$after" ] || fail "attention queries mutated the fixture repository"

# --- Content-free OTLP JSONL ----------------------------------------------

otel="$TMP/traces.jsonl"
XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/otel-export.sh" \
  --repo "$repo" --limit 20 > "$otel"
OMS_OTEL_FILE="$otel" OMS_OTEL_REPO="$repo" python3 - <<'PY' || fail "OTLP JSONL contract failed"
import json
import os
import re

raw = open(os.environ["OMS_OTEL_FILE"], encoding="utf-8").read()
assert "PRIVATE GOAL" not in raw, raw
assert "SECRET-HOOK-CONTENT" not in raw, raw
assert "SECRET FALLBACK CONTENT" not in raw, raw
assert "SECRET HOOK MODEL" not in raw, raw
assert "PRIVATE-VERIFY-EXIT" not in raw, raw
assert "PRIVATE-HOOK-TOKENS" not in raw, raw
assert "PRIVATE LIFECYCLE CONTENT" not in raw, raw
assert "PRIVATE APPROVAL SUMMARY" not in raw, raw
assert "PRIVATE PATCH PATH" not in raw, raw
assert os.environ["OMS_OTEL_REPO"] not in raw, raw
rows = [json.loads(line) for line in raw.splitlines() if line.strip()]
assert len(rows) == 11, len(rows)
spans = [r["resourceSpans"][0]["scopeSpans"][0]["spans"][0] for r in rows]
assert all(re.fullmatch(r"[0-9a-f]{32}", s["traceId"]) for s in spans), spans
assert all(re.fullmatch(r"[0-9a-f]{16}", s["spanId"]) for s in spans), spans
by_name = {s["name"]: s for s in spans}
assert "parentSpanId" not in by_name["oms.review"], by_name
assert by_name["oms.review"]["status"]["code"] == 2, by_name
assert by_name["oms.call"]["status"]["code"] == 0, by_name
assert by_name["oms.call"]["traceId"] == "a" * 32, by_name
assert by_name["oms.call"]["spanId"] == "b" * 16, by_name
assert by_name["oms.call"]["parentSpanId"] == "c" * 16, by_name
assert by_name["oms.call"]["flags"] == 1, by_name
assert by_name["oms.hook.PostToolUse"]["status"]["code"] == 2, by_name
assert by_name["oms.lifecycle.attempt.usage"]["status"]["code"] == 0, by_name
assert by_name["oms.approval.consumed"]["status"]["code"] == 0, by_name
assert by_name["oms.landing.complete"]["status"]["code"] == 0, by_name

def attrs(span):
    values = {}
    for item in span["attributes"]:
        wrapped = item["value"]
        values[item["key"]] = next(iter(wrapped.values()))
    return values

# The served model, its attribution, and the cost ride the artifact span so a
# per-model view exists outside the plane too; the model class vocabulary
# includes the role preset.
call_attrs = attrs(by_name["oms.call"])
assert call_attrs["oms.served_model"] == "gpt-served", call_attrs
assert call_attrs["oms.model_attribution"] == "transport", call_attrs
assert float(call_attrs["oms.cost_usd"]) == 0.25, call_attrs
usage_attrs = attrs(by_name["oms.lifecycle.attempt.usage"])
assert usage_attrs["oms.usage.trust"] == "advisory_provider_reported", usage_attrs
assert usage_attrs["oms.correlation.attempt"] == attrs(
    by_name["oms.approval.requested"]
)["oms.correlation.attempt"], by_name
assert attrs(by_name["oms.landing.intent"])["oms.correlation.approval"] == attrs(
    by_name["oms.approval.requested"]
)["oms.correlation.approval"], by_name
PY

XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/otel-export.sh" \
  --repo "$repo" --limit 20 --gen-ai > "$TMP/gen-ai.jsonl"
OMS_OTEL_FILE="$TMP/gen-ai.jsonl" python3 - <<'PY' || fail "GenAI OTLP mapping failed"
import json, os

raw = open(os.environ["OMS_OTEL_FILE"], encoding="utf-8").read()
for forbidden in ("PRIVATE GOAL", "SECRET-HOOK-CONTENT", "gen_ai.input.messages", "gen_ai.output.messages"):
    assert forbidden not in raw, forbidden
spans = [
    row["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
    for row in map(json.loads, raw.splitlines()) if row
]

def attrs(span):
    return {
        item["key"]: next(iter(item["value"].values()))
        for item in span["attributes"]
    }

by_name = {span["name"]: attrs(span) for span in spans}
assert by_name["oms.call"]["gen_ai.operation.name"] == "invoke_agent", by_name
assert by_name["oms.call"]["gen_ai.provider.name"] == "openai", by_name
assert by_name["oms.call"]["gen_ai.request.model"] == "gpt-test", by_name
assert by_name["oms.call"]["gen_ai.response.model"] == "gpt-served", by_name
assert by_name["oms.hook.PostToolUse"]["gen_ai.operation.name"] == "execute_tool", by_name
assert by_name["oms.hook.PostToolUse"]["gen_ai.usage.input_tokens"] == "7", by_name
PY

trace_repo="$TMP/trace-repo"
mkdir -p "$trace_repo"
git -C "$trace_repo" init -q
git -C "$trace_repo" config user.email test@example.com
git -C "$trace_repo" config user.name test
printf 'trace\n' > "$trace_repo/README.md"
git -C "$trace_repo" add README.md
git -C "$trace_repo" commit -qm init
incoming="00-11111111111111111111111111111111-2222222222222222-01"
attempt="$(OMS_TRACEPARENT="$incoming" OMS_TRACESTATE='vendor=must-not-persist' \
  bash "$ROOT/scripts/agent-events.sh" --repo "$trace_repo" start \
    --provider codex --tool fixture)" || fail "traced attempt start failed"
bash "$ROOT/scripts/agent-events.sh" --repo "$trace_repo" transition \
  --attempt "$attempt" --state starting >/dev/null || fail "traced transition failed"
OMS_TRACEPARENT='00-00000000000000000000000000000000-2222222222222222-01' \
  bash "$ROOT/scripts/agent-events.sh" --repo "$trace_repo" start \
    --provider codex --tool invalid-trace >/dev/null || fail "invalid context should be ignored"
python3 - "$trace_repo/.oms/lifecycle/events.jsonl" <<'PY' || fail "persisted trace contract failed"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
first, second, invalid = rows
assert first["trace_id"] == "1" * 32 and first["parent_span_id"] == "2" * 16, first
assert len(first["span_id"]) == 16 and first["trace_flags"] == "01", first
assert second["trace_id"] == first["trace_id"], second
assert second["parent_span_id"] == first["span_id"], (first, second)
assert not any(key in first for key in ("traceparent", "tracestate", "baggage")), first
assert not any(key in invalid for key in ("trace_id", "span_id", "parent_span_id", "trace_flags")), invalid
PY

# The trace-continuity scan in append_row must stay bounded and tolerant: a
# torn tail line neither blocks the append nor swallows it (the guard opens
# a fresh line), and the traced identity still inherits its trace across the
# fragment. Library-level on purpose — the CLI transition path additionally
# strict-reads the whole projection, a separate long-standing posture.
python3 - "$ROOT/scripts/lib/agent-events.py" "$trace_repo/.oms/lifecycle/events.jsonl" "$attempt" <<'PY' || fail "append across a torn events tail failed"
import importlib.util, json, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("ae_probe", sys.argv[1])
ae = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ae)
path = Path(sys.argv[2])
attempt = sys.argv[3]
with path.open("a", encoding="utf-8") as handle:
    handle.write('{"attempt_id":"torn-fragment","state":"work')  # no newline
before = path.read_bytes().count(b"\n")
ae.append_row(path, {"attempt_id": attempt, "state": "probe-after-torn"})
rows = []
for line in path.read_bytes().split(b"\n"):
    try:
        rows.append(json.loads(line.decode("utf-8")))
    except ValueError:
        continue
last = rows[-1]
assert last["state"] == "probe-after-torn", last
assert last.get("trace_id") == rows[0].get("trace_id"), (rows[0], last)
assert path.read_bytes().count(b"\n") == before + 2, "fresh-line guard missing"
PY

XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/otel-export.sh" \
  --repo "$repo" --limit 20 --output "$TMP/export.jsonl"
[ -s "$TMP/export.jsonl" ] || fail "OTLP file output is empty"
if XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/otel-export.sh" \
    --repo "$repo" --output "$TMP/export.jsonl" >/dev/null 2>&1; then
  fail "OTLP exporter overwrote an existing file without --force"
fi
XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/otel-export.sh" \
  --repo "$repo" --output "$TMP/export.jsonl" --force

# --- Editor / Orca / Codex launch plans -----------------------------------

bin="$TMP/bin"
mkdir -p "$bin"
cat > "$bin/code" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf '1.99.0\nfixture\n'; exit 0; fi
if [ "${1:-}" = --help ]; then printf '  --agents  Open the Agents window\n'; exit 0; fi
printf '%s\n' "$*" >> "$OMS_STUB_LOG"
EOF
cat > "$bin/orca" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = status ]; then printf '{"runtime":"orca","ok":true}\n'; exit 0; fi
printf '%s\n' "$*" >> "$OMS_STUB_LOG"
printf '{}\n'
EOF
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf 'codex-cli 9.9.9\n'; exit 0; fi
printf '%s\n' "$*" >> "$OMS_STUB_LOG"
EOF
cat > "$bin/not-orca" <<'EOF'
#!/usr/bin/env bash
printf 'Orca screen reader\n'
exit 2
EOF
chmod +x "$bin/code" "$bin/orca" "$bin/codex" "$bin/not-orca"
mkdir -p "$repo/dir"
printf 'x\n' > "$repo/dir/한글 file.py"
printf 'x\n' > "$repo/--worktree"

OMS_STUB_LOG="$TMP/gui.log" OMS_VSCODE_BIN="$bin/code" \
  bash "$ROOT/scripts/open-in.sh" --repo "$repo" --target vscode \
  --file 'dir/한글 file.py' --line 7 --column 2 --dry-run --json > "$TMP/vscode.json"
OMS_STUB_LOG="$TMP/gui.log" OMS_VSCODE_BIN="$bin/code" \
  bash "$ROOT/scripts/open-in.sh" --repo "$repo" --target vscode \
  --agents-window --dry-run --json > "$TMP/vscode-agents.json"
OMS_STUB_LOG="$TMP/gui.log" OMS_ORCA_BIN="$bin/orca" \
  bash "$ROOT/scripts/open-in.sh" --repo "$repo" --target orca \
  --file 'dir/한글 file.py' --dry-run --json > "$TMP/orca.json"
OMS_STUB_LOG="$TMP/gui.log" OMS_ORCA_BIN="$bin/orca" \
  bash "$ROOT/scripts/open-in.sh" --repo "$repo" --target orca \
  --file=--worktree --dry-run --json > "$TMP/orca-option.json"
OMS_STUB_LOG="$TMP/gui.log" OMS_CODEX_BIN="$bin/codex" \
  bash "$ROOT/scripts/open-in.sh" --repo "$repo" --target codex \
  --thread '019f-test-thread' --dry-run --json > "$TMP/codex.json"
python3 - "$TMP/vscode.json" "$TMP/vscode-agents.json" "$TMP/orca.json" "$TMP/orca-option.json" "$TMP/codex.json" <<'PY' || fail "open-in plans are wrong"
import json
import sys

v, va, o, option, c = [json.load(open(path, encoding="utf-8")) for path in sys.argv[1:]]
assert all(row["frontend_authority"] == "none" for row in (v, va, o, option, c)), (v, va, o, option, c)
assert v["target"] == "vscode" and "--goto" in v["command"], v
assert v["uri"].startswith("vscode://file/") and v["uri"].endswith(":7:2"), v
assert va["target"] == "vscode" and va["mode"] == "agents", va
assert va["command"][1:] == ["--agents", va["repo"]], va
assert o["target"] == "orca" and o["command"][1:3] == ["file", "open"], o
assert "--worktree" in o["command"] and "./dir/한글 file.py" in o["command"], o
assert option["command"][3] == "./--worktree", option
assert c["target"] == "codex" and c["command"][1:] == ["resume", "019f-test-thread"], c
assert "uri" not in c, c
PY
if OMS_ORCA_BIN="$bin/not-orca" bash "$ROOT/scripts/open-in.sh" --repo "$repo" \
    --target orca --file value.txt --dry-run >/dev/null 2>&1; then
  fail "open-in mistook a non-Stably Orca binary for the Orca IDE"
fi

# Outside an Orca-managed Linux terminal, `orca` commonly names the GNOME
# screen reader. Prefer the documented `orca-ide` binary and still verify its
# JSON identity before constructing a plan.
cat > "$bin/orca-ide" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = status ]; then printf '{"runtime":"orca-ide","ok":true}\n'; exit 0; fi
exit 0
EOF
cat > "$bin/orca" <<'EOF'
#!/usr/bin/env bash
printf 'GNOME Orca screen reader\n'
exit 2
EOF
chmod +x "$bin/orca" "$bin/orca-ide"
PATH="$bin:$PATH" OMS_ORCA_BIN='' bash "$ROOT/scripts/open-in.sh" --repo "$repo" \
  --target orca --file 'dir/한글 file.py' --dry-run --json > "$TMP/orca-linux.json"
python3 - "$TMP/orca-linux.json" <<'PY' || fail "Linux orca-ide selection is wrong"
import json, pathlib, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert pathlib.Path(row["command"][0]).name == "orca-ide", row
PY
[ ! -e "$TMP/gui.log" ] || fail "--dry-run launched a GUI command"

# --- Retired semantic evaluation migration ---------------------------------

# Help and refusal must work without Python, Git, a provider, or any other
# external executable. In particular, even --force cannot overwrite history.
python3 - "$repo" "$TMP/spec.json" "$TMP/host-check-ran" <<'PY'
import json
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
report = {"schema": 1, "action": "semantic-eval", "semantic_outcome": "pass",
          "subject": {"base_sha": "historical-base", "event_id": "evt_subject"}}
(repo / ".oms/artifacts/historical-eval.json").write_text(
    json.dumps(report) + "\n", encoding="utf-8")
spec = {"schema": 1, "id": "retired",
        "checks": [{"id": "must-not-run",
                    "argv": ["touch", sys.argv[3]]}]}
pathlib.Path(sys.argv[2]).write_text(json.dumps(spec), encoding="utf-8")
PY
cp "$repo/.oms/artifacts/historical-eval.json" "$TMP/historical-before.json"
cp "$repo/.oms/artifacts/index.jsonl" "$TMP/index-before.jsonl"
before_status="$(git -C "$repo" status --porcelain)"
before_worktrees="$(git -C "$repo" worktree list --porcelain)"
bash_bin="$(command -v bash)"
PATH="$TMP/no-programs" "$bash_bin" "$ROOT/scripts/semantic-eval.sh" --help > "$TMP/eval-help.txt"
grep -Fq 'patch-admit' "$TMP/eval-help.txt" || fail "migration help omitted deterministic route"
grep -Fq 'peer-review --gate --prompt' "$TMP/eval-help.txt" || fail "migration help omitted rubric route"
grep -Fq 'historical-base' "$TMP/eval-help.txt" || fail "migration help omitted historical boundary"

for mode in bare legacy mixed-help; do
  set --
  if [ "$mode" != bare ]; then
    set -- --repo "$repo" --spec "$TMP/spec.json" --subject-event evt_subject \
      --allow-host-checks --output "$repo/.oms/artifacts/historical-eval.json" --force
  fi
  [ "$mode" != mixed-help ] || set -- "$@" --help
  eval_rc=0
  PATH="$TMP/no-programs" "$bash_bin" "$ROOT/scripts/semantic-eval.sh" "$@" \
    > "$TMP/eval-out.txt" 2> "$TMP/eval-err.txt" || eval_rc=$?
  [ "$eval_rc" -eq 2 ] || fail "retired $mode invocation returned $eval_rc, expected 2"
  [ ! -s "$TMP/eval-out.txt" ] || fail "retired command emitted a result"
  grep -Fq 'retired; no evaluation is performed' "$TMP/eval-err.txt" ||
    fail "retired command did not explain refusal"
done
cmp "$TMP/historical-before.json" "$repo/.oms/artifacts/historical-eval.json" ||
  fail "migration changed a historical report"
cmp "$TMP/index-before.jsonl" "$repo/.oms/artifacts/index.jsonl" ||
  fail "migration changed the artifact index"
[ ! -e "$TMP/host-check-ran" ] || fail "migration executed a host check"
[ "$(cat "$repo/value.txt")" = old ] || fail "migration changed the primary checkout"
[ "$(git -C "$repo" status --porcelain)" = "$before_status" ] || fail "migration changed worktree status"
[ "$(git -C "$repo" worktree list --porcelain)" = "$before_worktrees" ] ||
  fail "migration changed registered worktrees"


# Persisted trace context is authority metadata, not arbitrary artifact data.
# The validator must reject malformed IDs and raw propagation headers rather
# than letting exporters silently synthesize a different trace.
trace_repo="$TMP/trace-validation-repo"
mkdir -p "$trace_repo/.oms/artifacts"
git -C "$trace_repo" init -q
cat > "$trace_repo/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_bad_trace","operation_id":"op_bad_trace","artifact_id":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","ts":"2026-08-27T00:00:00Z","kind":"call","provider":"codex","exit":0,"trace_id":"00000000000000000000000000000000","span_id":"bbbbbbbbbbbbbbbb","trace_flags":"01","traceparent":"00-11111111111111111111111111111111-2222222222222222-01"}
EOF
if bash "$ROOT/scripts/artifact-index.sh" --repo "$trace_repo" validate \
    > "$TMP/bad-trace.out" 2>&1; then
  fail "artifact validation accepted malformed/raw trace context"
fi
grep -Fq 'persisted trace context' "$TMP/bad-trace.out" ||
  fail "artifact trace refusal was not explicit: $(cat "$TMP/bad-trace.out")"

echo 'operator-tools-smoke: ok'
