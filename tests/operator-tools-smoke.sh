#!/usr/bin/env bash
set -euo pipefail

# Focused regressions for the read-only operator cockpit, content-free OTLP
# JSONL export, explicit editor launch adapters, and deterministic semantic
# evaluation. Every fixture lives below TMP; no real provider or GUI is called.

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

# One real patch subject. Restore the fixture immediately so every assertion
# can prove semantic evaluation never mutates the primary checkout.
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
        "model_class": "explicit",
        "reasoning_effort": "medium",
        "fallback_reason": "SECRET FALLBACK CONTENT",
        "duration_s": 5.0,
        "tokens": 120,
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
            "state": "review",
            "updated": "2099-01-01T00:00:00Z",
            "depends": [],
        },
        "ready_task": {"state": "ready", "depends": []},
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

# --- Unified cockpit -------------------------------------------------------

before="$(git -C "$repo" status --porcelain=v1 --untracked-files=all)"
cockpit="$TMP/cockpit.json"
bash "$ROOT/scripts/ops-cockpit.sh" --repo "$repo" --json > "$cockpit"
python3 - "$cockpit" <<'PY' || fail "cockpit JSON contract failed"
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 1 and row["action"] == "ops-cockpit", row
assert row["lifecycle"]["live_delegations"] == 1, row
assert row["lifecycle"]["orphan_delegations"] == 1, row
assert row["lifecycle"]["phase"] == "attention", row
assert row["lifecycle"]["active_attempts"] == 0, row
assert isinstance(row["lifecycle"]["attempts"], list), row
assert row["approval"]["required"] is True, row
assert isinstance(row["approval"]["items"], list), row
assert row["telemetry"]["operations"]["eligible"] == 3, row
assert row["telemetry"]["usage"]["provider_reported_tokens"]["total"] == 120, row
assert row["inbox"]["items"][0]["priority"] == "P1", row
PY
text="$(bash "$ROOT/scripts/ops-cockpit.sh" --repo "$repo")"
printf '%s\n' "$text" | grep -Fq 'ops cockpit:' || fail "cockpit text header missing"
printf '%s\n' "$text" | grep -Fq 'approval=yes' || fail "cockpit approval missing"
after="$(git -C "$repo" status --porcelain=v1 --untracked-files=all)"
[ "$before" = "$after" ] || fail "cockpit mutated the fixture repository"

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

# --- Deterministic semantic evaluation ------------------------------------

python3 - "$TMP/spec.json" "$TMP/judge.json" "$TMP/fail-spec.json" \
  "$TMP/same-judge.json" "$TMP/independent-spec.json" \
  "$TMP/background.pid" "$TMP/background-marker" <<'PY'
import json
import sys

spec = {
    "schema": 1,
    "id": "patch-quality",
    "checks": [
        {
            "id": "content",
            "argv": ["grep", "-qx", "new", "value.txt"],
            "required": True,
            "timeout_s": 10,
        },
        {
            "id": "descendant-cleanup",
            "argv": [
                "bash",
                "-c",
                "(trap '' TERM; printf '%s\\n' \"$BASHPID\" > \"$1\"; sleep 5; printf leaked > \"$2\") & while [ ! -s \"$1\" ]; do :; done",
                "_",
                sys.argv[6],
                sys.argv[7],
            ],
            "required": True,
            "timeout_s": 10,
        },
        {
            "id": "output-cap",
            "argv": [sys.executable, "-c", "import sys; sys.stdout.write('x' * 2097152)"],
            "required": False,
            "timeout_s": 10,
        },
    ],
    "rubric": [{"id": "correctness", "weight": 1.0}],
    "threshold": 0.8,
    "require_independent_judge": False,
}
judge = {
    "schema": 1,
    "provider": "claude",
    "model": "fixture",
    "verdict": "pass",
    "scores": {"correctness": 0.9},
}
fail_spec = dict(spec)
fail_spec["id"] = "patch-quality-fail"
fail_spec["checks"] = [
    {
        "id": "content",
        "argv": ["grep", "-qx", "missing", "value.txt"],
        "required": True,
        "timeout_s": 10,
    }
]
same = dict(judge)
same["provider"] = "codex"
independent = dict(spec)
independent["id"] = "patch-quality-independent"
independent["require_independent_judge"] = True
for path, value in zip(sys.argv[1:6], (spec, judge, fail_spec, same, independent)):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, sort_keys=True)
        handle.write("\n")
PY

if bash "$ROOT/scripts/semantic-eval.sh" --repo "$repo" --spec "$TMP/spec.json" \
    --subject-event evt_subject --judge-result "$TMP/judge.json" \
    >/dev/null 2>"$TMP/host-check-denied.err"; then
  fail "semantic eval ran repository-provided host commands without explicit consent"
fi
grep -Fq -- '--allow-host-checks' "$TMP/host-check-denied.err" ||
  fail "host-check refusal did not name the explicit trust switch"

bash "$ROOT/scripts/semantic-eval.sh" --repo "$repo" --spec "$TMP/spec.json" \
  --allow-host-checks \
  --subject-event evt_subject --judge-result "$TMP/judge.json" > "$TMP/eval-pass.json"
python3 - "$TMP/eval-pass.json" <<'PY' || fail "semantic pass result is wrong"
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["semantic_outcome"] == "pass", row
assert row["score"] == 0.9, row
assert row["checks"][0]["exit"] == 0, row
assert row["subject"]["event_id"] == "evt_subject", row
assert "output" not in row["checks"][0], row
output_check = next(item for item in row["checks"] if item["id"] == "output-cap")
assert output_check["output_limited"] is True, output_check
assert output_check["exit"] != 0, output_check
PY
background_pid="$(cat "$TMP/background.pid")"
if kill -0 "$background_pid" 2>/dev/null; then
  fail "semantic eval left a background check descendant running"
fi
[ ! -e "$TMP/background-marker" ] || fail "background check escaped its evaluation lifetime"
[ "$(cat "$repo/value.txt")" = old ] || fail "semantic eval changed the primary checkout"
[ "$(git -C "$repo" worktree list --porcelain | grep -c '^worktree ')" -eq 1 ] ||
  fail "semantic eval leaked a temporary worktree"

if bash "$ROOT/scripts/semantic-eval.sh" --repo "$repo" --spec "$TMP/fail-spec.json" \
    --allow-host-checks \
    --subject-event evt_subject --judge-result "$TMP/judge.json" > "$TMP/eval-fail.json"; then
  fail "a required semantic check failure returned success"
fi
python3 - "$TMP/eval-fail.json" <<'PY' || fail "semantic fail result is wrong"
import json
import sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["semantic_outcome"] == "fail", row
assert row["checks"][0]["exit"] != 0, row
PY

set +e
bash "$ROOT/scripts/semantic-eval.sh" --repo "$repo" --spec "$TMP/independent-spec.json" \
  --allow-host-checks \
  --subject-event evt_subject --judge-result "$TMP/same-judge.json" > "$TMP/eval-incomplete.json"
eval_rc=$?
set -e
[ "$eval_rc" -eq 3 ] || fail "self-reported independent judge should return incomplete (3), got $eval_rc"
python3 - "$TMP/eval-incomplete.json" <<'PY' || fail "independent-judge rejection is wrong"
import json
import sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["semantic_outcome"] == "incomplete", row
assert any("provenance" in reason for reason in row["reasons"]), row
PY

printf '{not-json\n' > "$TMP/bad-judge.json"
set +e
bash "$ROOT/scripts/semantic-eval.sh" --repo "$repo" --spec "$TMP/spec.json" \
  --allow-host-checks \
  --subject-event evt_subject --judge-result "$TMP/bad-judge.json" > "$TMP/eval-bad-judge.json"
eval_rc=$?
set -e
[ "$eval_rc" -eq 3 ] || fail "malformed judge should return incomplete (3), got $eval_rc"
python3 - "$TMP/eval-bad-judge.json" <<'PY' || fail "malformed judge result is wrong"
import json
import sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["semantic_outcome"] == "incomplete", row
assert any("invalid JSON" in reason for reason in row["reasons"]), row
PY

echo 'operator-tools-smoke: ok'
