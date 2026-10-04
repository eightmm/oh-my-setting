#!/usr/bin/env bash
set -euo pipefail

# Focused regressions for the read-only collaboration dashboard, content-free OTLP
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

# --- Collaboration dashboard ----------------------------------------------

# .oms is gitignored, so read-only means every byte below .oms and the
# private approval store, not just git status.
tree_sums() {
  (cd "$1" && find . -type f | LC_ALL=C sort | while IFS= read -r f; do cksum "$f"; done)
}
dash() { COLUMNS=160 XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/dashboard.sh" "$@"; }
before="$(tree_sums "$repo/.oms"; tree_sums "$TMP/state")"
dash --repo "$repo" > "$TMP/dash.txt" || fail "dashboard snapshot failed"
dash --repo "$repo" --json > "$TMP/dash.json" || fail "dashboard JSON failed"
XDG_STATE_HOME="$TMP/state" bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json > "$TMP/dash-inbox.json"
after="$(tree_sums "$repo/.oms"; tree_sums "$TMP/state")"
[ "$before" = "$after" ] || fail "dashboard mutated .oms or the approval store"
for line in 'coverage: OMS-managed records only; native provider subagents are not observed' \
  'goal: operator fixture (plan)' 'plan: 2 task(s)  ready=1 review=1  actionable=ready_task' \
  'model=gpt-test route=explicit fallback=other exit=0 success served=gpt-served tokens=120' \
  'codex delegate model=unknown route=unrecorded exit=0 success served=unreported tokens=unknown' \
  'review gate: none recorded in window (not a pass)  seat answers=1' \
  'acceptance: 0/2 verified  missing=2  (worker completion is not acceptance)'; do
  grep -Fq -- "$line" "$TMP/dash.txt" || fail "dashboard text lacks: $line"$'\n'"$(cat "$TMP/dash.txt")"
done
python3 - "$TMP/dash.json" "$TMP/dash.txt" "$TMP/dash-inbox.json" "$repo" <<'PY' || fail "dashboard projection contract failed"
import json, sys
report = json.load(open(sys.argv[1], encoding="utf-8"))
inbox = json.load(open(sys.argv[3], encoding="utf-8"))
raw = open(sys.argv[1], encoding="utf-8").read() + open(sys.argv[2], encoding="utf-8").read()
for forbidden in ("PRIVATE GOAL", "SECRET FALLBACK CONTENT", "PRIVATE-VERIFY-EXIT",
                  "PRIVATE LIFECYCLE CONTENT", "PRIVATE APPROVAL SUMMARY",
                  "PRIVATE PATCH PATH", "change.patch", sys.argv[4]):
    assert forbidden not in raw, forbidden
assert report["schema"] == 1 and report["kind"] == "oms-dashboard", report
assert report["collection"] == {"ok": True, "sources": {"state": "ok", "artifacts": "ok", "attempts": "ok"}}
assert report["goal"] == {"text": "operator fixture", "source": "plan"}, report["goal"]
assert report["plan"]["by_state"] == {"ready": 1, "review": 1}, report["plan"]
attempt = report["attempts"]["recent"][0]
assert (attempt["attempt_id"], attempt["state"], attempt["tokens"], attempt["cost_microusd"]) == (
    "att_one", "cancelled", 12, 7), attempt
ops = {row["event_id"]: row for row in report["operations"]["recent"]}
call, subject, review = ops["evt_call"], ops["evt_subject"], ops["evt_review"]
assert (call["selected_model"], call["served_model"], call["route_class"]) == (
    "gpt-test", "gpt-served", "explicit"), call
assert (call["model_attribution"], call["fallback_reason"], call["tokens"], call["cost_usd"]) == (
    "transport", "other", 120, 0.25), call
assert call["verify_exit"] is None and subject["verify_exit"] == 0, (call, subject)
# Unknown model, tokens and cost stay unknown rather than turning into zero.
for key in ("selected_model", "served_model", "route_class", "tokens", "cost_usd"):
    assert subject[key] is None, (key, subject)
assert subject["model_attribution"] == "unknown", subject
assert (review["exit"], review["status"], review["tokens"]) == (1, "unresolved", None), review
assert report["reviews"] == {"outcomes": 0, "passed": 0, "failed": 0, "unknown": 0, "seat_answers": 1}, report["reviews"]
acceptance = report["acceptance"]
assert (acceptance["total"], acceptance["counts"], acceptance["complete"]) == (2, {"missing": 2}, False), acceptance
assert [item["code"] for item in report["attention"]["items"]] == [
    item["code"] for item in inbox["items"]][:8], (report["attention"], inbox)
assert len(report["delegations"]) == 2 and not report["delegations"][0]["live"], report["delegations"]
PY

# Untrusted labels must not drive the terminal: escapes, BEL, C1 controls and
# bidi overrides are neutralized in both views.
hostile="$TMP/dash-hostile"
cp -R "$repo" "$hostile"
python3 - "$hostile" <<'PY'
import json, sys
from pathlib import Path
repo = Path(sys.argv[1])
plan_path = repo / ".oms/plan/tasks.json"
plan = json.loads(plan_path.read_text(encoding="utf-8"))
plan["goal"] = "evil\x1b[2J\x1b]0;title\x07 ‮goal​ end"
plan_path.write_text(json.dumps(plan), encoding="utf-8")
index = repo / ".oms/artifacts/index.jsonl"
rows = [json.loads(line) for line in index.read_text(encoding="utf-8").splitlines()]
rows[0]["provider"], rows[0]["selected_model"] = "co\x1bdex", "gpt\x1b[31mred\x9b"
index.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
PY
dash --repo "$hostile" > "$TMP/dash-hostile.txt" || fail "hostile labels broke the dashboard"
dash --repo "$hostile" --json > "$TMP/dash-hostile.json" || fail "hostile labels broke dashboard JSON"
python3 - "$TMP/dash-hostile.txt" "$TMP/dash-hostile.json" <<'PY' || fail "dashboard passed terminal control text through"
import json, sys
text = open(sys.argv[1], "rb").read()
for raw in (text, open(sys.argv[2], "rb").read()):
    for needle in (b"\x1b", b"\x07", b"\\u001b", "‮".encode(), "\x9b".encode(), b"\\u202e"):
        assert needle not in raw, needle
assert b"goal: evil?[2J?]0;title? ?goal? end (plan)" in text, text
assert json.load(open(sys.argv[2], encoding="utf-8"))["operations"]["recent"][-1]["provider"] == "co?dex"
PY

# A source that cannot be read degrades the view loudly instead of showing an
# empty healthy one.
corrupt="$TMP/dash-corrupt"
cp -R "$repo" "$corrupt"
printf '{"schema":1,"event_id":"levt_torn' >> "$corrupt/.oms/lifecycle/events.jsonl"
rc=0
dash --repo "$corrupt" > "$TMP/dash-corrupt.txt" || rc=$?
[ "$rc" = 1 ] || fail "corrupt lifecycle should degrade the dashboard with exit 1, got $rc"
grep -Fq 'COLLECTION FAILED attempts: exit 2' "$TMP/dash-corrupt.txt" ||
  fail "dashboard hid the failed lifecycle collection: $(cat "$TMP/dash-corrupt.txt")"
grep -Fq 'attempts: UNAVAILABLE (lifecycle stream invalid)' "$TMP/dash-corrupt.txt" ||
  fail "dashboard showed attempts from an invalid stream"
! grep -Fq "$corrupt" "$TMP/dash-corrupt.txt" || fail "dashboard leaked the repo path"

# Missing state and oversized listings, through the pure projection.
snap="$TMP/dash-snap"
mkdir -p "$snap"
printf '1\n' > "$snap/state.rc"
printf 'error: PRIVATE COLLECTOR CONTENT\n' > "$snap/state.err"
printf '0\n' > "$snap/artifacts.rc"
printf '0\n' > "$snap/attempts.rc"
printf '[]\n' > "$snap/attempts.json"
python3 - "$snap/artifacts.json" <<'PY'
import json, sys
rows = [{"event_id": "evt_%d" % n, "ts": "x" * 5000, "kind": "call", "provider": "codex", "exit": 0}
        for n in range(30)]
rows[-1]["cost_usd"] = float("nan")
rows[-2]["cost_usd"] = float("inf")
rows[-3].update(kind="review-outcome", exit="unrecorded")
json.dump({"schema": 1, "action": "list", "rows": rows}, open(sys.argv[1], "w"))
PY
rc=0
PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/scripts/lib/dashboard_projection.py" "$snap" --json \
  > "$TMP/dash-snap.json" || rc=$?
[ "$rc" = 1 ] || fail "missing state must exit 1, got $rc"
PYTHONDONTWRITEBYTECODE=1 python3 "$ROOT/scripts/lib/dashboard_projection.py" "$snap" \
  > "$TMP/dash-snap.txt" || true
grep -Fq 'state: UNAVAILABLE' "$TMP/dash-snap.txt" || fail "missing state rendered as a healthy view"
python3 - "$TMP/dash-snap.json" "$TMP/dash-snap.txt" <<'PY' || fail "unavailable/oversized projection failed"
import json, sys
report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["collection"]["sources"]["state"] == "exit 1", report["collection"]
assert "PRIVATE COLLECTOR CONTENT" not in json.dumps(report)
assert all(row["cost_usd"] is None for row in report["operations"]["recent"])
assert report["reviews"]["unknown"] == 1 and report["reviews"]["failed"] == 0, report["reviews"]
assert report["operations"]["recent"][2]["status"] == "unknown"
json.dumps(report, allow_nan=False)
assert report["task"] is None and report["attention"] is None and report["acceptance"] is None, report
assert report["attempts"]["available"] is False, report["attempts"]
assert len(report["operations"]["recent"]) == 8 and report["operations"]["omitted"] == 22, report["operations"]
assert all(len(row["ts"]) <= 40 for row in report["operations"]["recent"]), report["operations"]
assert max(len(line) for line in open(sys.argv[2], encoding="utf-8").read().splitlines()) <= 120
PY

# A live view must fit a normal terminal without scrolling its goal away.
# Keep route and acceptance information visible when old details are omitted.
PYTHONDONTWRITEBYTECODE=1 python3 - "$ROOT/scripts/lib/dashboard_projection.py" "$TMP/dash.json" "$snap" <<'PY' || fail "dashboard bounds/contract regression"
import json, runpy, subprocess, sys
from pathlib import Path
module = runpy.run_path(sys.argv[1])
report = json.load(open(sys.argv[2], encoding="utf-8"))
report["operations"]["recent"] *= 4
report["attempts"]["recent"] *= 4
text = module["render"](report, 100, interval=5, height=24)
assert len(text.splitlines()) <= 24, text
assert "model=gpt-test route=explicit" in text, text
assert "acceptance:" in text and "attention:" in text and "detail line(s) hidden" in text, text
assert all(module["display_width"](line) <= 100 for line in text.splitlines())
assert module["display_width"](module["fit"]("한글" * 30, 40)) <= 40
snap = Path(sys.argv[3])
original_artifacts = (snap / "artifacts.json").read_bytes()
(snap / "artifacts.json").write_text('{"schema":1,"rows":{}}')
bad = module["build"](snap, "fixture")
assert bad["collection"]["sources"]["artifacts"] == "unsupported artifact-index contract"
(snap / "artifacts.json").write_bytes(b" " * (module["MAX_SOURCE_BYTES"] + 1))
assert "display limit" in module["load_source"](snap, "artifacts")[1]
(snap / "artifacts.json").write_bytes(original_artifacts)
(snap / "state.json").write_bytes((Path(sys.argv[2]).parent / "shared-state.json").read_bytes())
(snap / "state.rc").write_text("0\n")
cli = subprocess.run([sys.executable, "-B", sys.argv[1], str(snap), "--width", "80",
                      "--height", "24", "--interval", "5"], capture_output=True, text=True)
assert cli.returncode == 0, cli.stderr
assert len(cli.stdout.splitlines()) <= 23, cli.stdout
assert "acceptance:" in cli.stdout and "attention:" in cli.stdout
PY

# Arguments: watch-only options, a sensible interval floor, and no JSON stream.
for bad in '--interval 5' '--count 1' '--json --watch' '--watch --interval 1' \
  '--watch --interval abc' '--watch --count 0'; do
  rc=0
  # shellcheck disable=SC2086 # deliberate word splitting of the case
  dash --repo "$repo" $bad >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "dashboard $bad should be a usage error, got $rc"
done
"$ROOT/scripts/oms" dashboard --help | grep -Fq 'Usage: dashboard.sh' || fail "oms dashboard --help failed"
# A non-terminal stream is appended to, never cleared.
dash --repo "$repo" --watch --interval 2 --count 2 > "$TMP/dash-watch.txt" || fail "dashboard watch failed"
[ "$(grep -c '^OMS dashboard .* refresh=2s$' "$TMP/dash-watch.txt")" = 2 ] || fail "watch did not refresh twice"
[ "$(grep -c '^----$' "$TMP/dash-watch.txt")" = 1 ] || fail "watch snapshots lack a separator"
! grep -q "$(printf '\033')" "$TMP/dash-watch.txt" || fail "watch erased a non-terminal stream"

# --- Content-free OTLP JSONL ----------------------------------------------

otel="$TMP/traces.jsonl"
python3 - "$ROOT/scripts/lib/otel-export.py" "$TMP/reader.jsonl" <<'PY' || fail "bounded telemetry reader lost source identities"
import importlib.util
import io
import json
import sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, str(Path(sys.argv[1]).parent))
spec = importlib.util.spec_from_file_location("otel_export", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
path = Path(sys.argv[2])
with path.open('w', encoding='utf-8') as handle:
    for number in range(1000):
        handle.write(json.dumps({'n': number}) + '\n')
    handle.write('x' * (2 * 1024 * 1024) + '\n')
    handle.write('invalid\n[]\n\n{"n":1000}')
assert module.read_rows(path, 2) == [(1000, {'n': 999}), (1005, {'n': 1000})]
assert module.read_rows(path, 1) == [(1005, {'n': 1000})]
assert module.read_rows(path.with_suffix('.missing'), 2) == []
class BoundedReader(io.StringIO):
    def __iter__(self):
        raise AssertionError('unbounded line iteration')
    def readline(self, size=-1):
        assert 0 < size <= 1024 * 1024 + 1, size
        return super().readline(size)
with patch.object(Path, 'open', return_value=BoundedReader('x' * (3 * 1024 * 1024) + '\n{"n":1}')):
    assert module.read_rows(path, 1) == [(2, {'n': 1})]
PY
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

# --- Terminal panel and bidirectional orchestration ------------------------
# Reuse this operator suite for the new front end. Providers are protocol
# fixtures; the real consult/delegate/review and artifact writers run below.
PYTHONDONTWRITEBYTECODE=1 python3 - "$ROOT" "$TMP" <<'PY' || fail "terminal panel regression failed"
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

root, temporary = map(Path, sys.argv[1:])
sys.path.insert(0, str(root / "scripts/lib"))
import terminal_panel as panel

project = temporary / "panel repo 'quoted'; dollar$"
project.mkdir()
home = temporary / "panel-home"
home.mkdir()
binary = temporary / "panel-bin"
binary.mkdir()
subprocess.run(["git", "init", "-q", str(project)], check=True)
for key, value in (("user.email", "test@example.com"), ("user.name", "test")):
    subprocess.run(["git", "-C", str(project), "config", key, value], check=True)
(project / "value.txt").write_text("old\n")
subprocess.run(["git", "-C", str(project), "add", "value.txt"], check=True)
subprocess.run(["git", "-C", str(project), "commit", "-qm", "init"], check=True)
brief = temporary / "panel-brief.md"
brief.write_text("Task: append new to value.txt. No primary edits or recursive workers.\n")
log = temporary / "panel-native-log.jsonl"
for provider in panel.PROVIDERS:
    executable = binary / provider
    executable.write_text('''#!/usr/bin/env bash
set -eu
case "${1:-}:${2:-}" in
  --version:|--help:|exec:--help) printf 'fixture CLI 1.0\\n'; exit 0 ;;
esac
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  cat >/dev/null
  if [ "${PANEL_TEST_MODE:-}" = write ]; then
    printf 'new\\n' >> value.txt
    if bash "$OMS_PANEL_ENTRYPOINT" panel --launch codex >/dev/null 2>&1; then
      echo 'nested owner launch should have failed' >&2
      exit 1
    fi
  fi
  printf 'Answer: inspected the bounded repository task.\\nGATE: pass\\n'
else
  python3 - "$@" <<'INNER'
import json, os, sys
with open(os.environ["PANEL_TEST_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "agent": os.environ["OMS_AGENT"],
                        "repo": os.environ["OMS_PANEL_REPO"], "cwd": os.getcwd(),
                        "entry": os.environ["OMS_PANEL_ENTRYPOINT"]}) + "\\n")
print("NATIVE_FIXTURE_READY")
INNER
fi
''')
    executable.chmod(0o755)

environment = dict(os.environ, HOME=str(home), NVM_DIR=str(home / ".nvm"),
                   PATH=str(binary) + os.pathsep + os.environ["PATH"],
                   XDG_STATE_HOME=str(temporary / "panel-state"),
                   OMS_PANEL_ENTRYPOINT=str(panel.ENTRY), PANEL_TEST_LOG=str(log),
                   OMS_PEER_TIMEOUT="30", OMS_ROLE_ROUTING="0")
for key in ("OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH", "OMS_PANEL_SESSION", "TMUX",
            "OH_MY_SETTING_CALL_DRY_RUN", "OH_MY_SETTING_REVIEW_DRY_RUN"):
    environment.pop(key, None)

def call(command, env=None):
    result = subprocess.run(command, cwd=str(project), env=env or environment,
                            capture_output=True, text=True, timeout=90)
    assert result.returncode == 0, (command, result.returncode, result.stdout[-6000:], result.stderr[-6000:])
    return result.stdout

baseline = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--json"]))
assert report["kind"] == "oms-panel" and report["schema"] == 1
assert all(report["providers"][p]["installed"] for p in panel.PROVIDERS)
assert report["dashboard"]["kind"] == "oms-dashboard"
assert not (project / ".oms").exists(), "panel query created a state store"
assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == baseline
assert shlex.split(panel.shell_command(["bash", str(panel.ENTRY), str(project)]))[1:] == ["bash", str(panel.ENTRY), str(project)]

for owner in panel.PROVIDERS:
    env = dict(environment, OMS_AGENT=owner)
    peer = "claude" if owner == "codex" else "codex"
    command = panel.peer_command("ask", owner, project, prompt="Inspect value.txt without edits.")
    answer = call(command, env)
    assert "inspected the bounded repository task" in answer
    assert (project / "value.txt").read_text() == "old\n"
    write_env = dict(env, PANEL_TEST_MODE="write")
    command = panel.peer_command("delegate", owner, project, brief=brief,
                                 verify="test \"$(tail -1 value.txt)\" = new")
    answer = call(command, write_env)
    patch = re.search(r"^patch: (.+)$", answer, re.M)
    assert patch and "+new" in Path(patch.group(1)).read_text(), answer
    assert (project / "value.txt").read_text() == "old\n", "worker edited primary"
    answer = call(panel.peer_command("review", owner, project, prompt="Review value.txt.",
                                     verify="test -f value.txt"), env)
    assert "pass" in answer
    turns = json.loads(call(["bash", str(panel.ENTRY), "thread", "--repo", str(project), "show", "--json"], env))["turns"]
    assert any(t.get("provider") == peer and t["role"] == "answer" for t in turns)

rows = json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo", str(project),
                        "--json", "list", "30"]))["rows"]
assert {r["provider"] for r in rows if r["kind"] == "delegate"} == set(panel.PROVIDERS)
assert {r["provider"] for r in rows if r["kind"] == "review"} == set(panel.PROVIDERS)

for owner in panel.PROVIDERS:
    plan = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                           "--launch", owner, "--resume", "exact-session-id", "--dry-run"]))
    assert plan["executes"] is False and "exact-session-id" in plan["argv"]
    assert "--last" not in plan["argv"] and "--continue" not in plan["argv"]
    assert "--dangerously" not in " ".join(plan["argv"])

for arguments in (["--resume", "--last"], ["--launch", "codex", "--resume", "../bad"],
                  ["--launch", "codex", "--resume=--last", "--dry-run"],
                  ["--launch", "codex", "--json"], ["--model", "x"],
                  ["--json", "--dry-run"], []):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel"] + arguments,
                            cwd=str(project), env=environment, capture_output=True, text=True)
    assert result.returncode == 2, (arguments, result)
for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--launch", "codex"],
                            cwd=str(project), env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result

if os.name == "posix":
    import fcntl
    import pty
    import select
    import struct
    import termios
    import time

    def terminal(command, inputs, expected):
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe(command[0], command, dict(environment, TERM="xterm", OMS_AGENT="codex"))
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        pending = inputs.splitlines(keepends=True)
        prompts = 0
        output = b""
        deadline = time.monotonic() + 20
        finished = False
        try:
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .2)[0]:
                    try:
                        output += os.read(fd, 65536)
                    except OSError:
                        pass
                seen = output.count(b"oms> ")
                if pending and seen > prompts:
                    os.write(fd, pending.pop(0))
                    prompts = seen
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == 0, output
                    break
            if not finished:
                waited, status = os.waitpid(pid, os.WNOHANG)
                finished = bool(waited)
                assert finished, output
                assert os.waitstatus_to_exitcode(status) == 0, output
            assert expected in output, output
        finally:
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)

    terminal(["bash", str(panel.ENTRY), "panel", "--layout", "inline"], b"9\nq\n", b"OMS control panel")
    for owner in panel.PROVIDERS:
        terminal(["bash", str(panel.ENTRY), "panel", "--launch", owner, "--resume", "exact-session-id"],
                 b"", b"NATIVE_FIXTURE_READY")
    launched = [json.loads(line) for line in log.read_text().splitlines()]
    assert {r["agent"] for r in launched} == set(panel.PROVIDERS)
    for row in launched:
        assert row["cwd"] == row["repo"] == str(project.resolve())
        assert row["entry"] == str(panel.ENTRY)
        assert "exact-session-id" in row["argv"]
        assert "peer-delegate" in " ".join(row["argv"])
    actual_tmux = shutil.which("tmux")
    if actual_tmux:
        socket = "oms-panel-fixture-" + str(os.getpid())
        wrapper = binary / "tmux"
        wrapper.write_text("#!/usr/bin/env bash\nexec %s -L %s \"$@\"\n" %
                           (shlex.quote(actual_tmux), shlex.quote(socket)))
        wrapper.chmod(0o755)
        tmux = [actual_tmux, "-L", socket]
        env = dict(environment, TERM="xterm")
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe("bash", ["bash", str(panel.ENTRY)], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        finished = False
        client_output = bytearray()
        try:
            def tmux_wait(arguments, expected, timeout=15):
                deadline = time.monotonic() + timeout
                text = ""
                while time.monotonic() < deadline:
                    if select.select([fd], [], [], .1)[0]:
                        try:
                            client_output.extend(os.read(fd, 65536))
                        except OSError:
                            pass
                    result = subprocess.run(tmux + arguments, capture_output=True, text=True, env=env)
                    text = result.stdout
                    if result.returncode == 0 and expected in text:
                        return text
                raise AssertionError((arguments, expected, text, bytes(client_output[-4000:])))

            sessions = tmux_wait(["list-sessions", "-F", "#{session_name}"], "oms-panel-")
            session = sessions.strip().splitlines()[0]
            panes = tmux_wait(["list-panes", "-t", session, "-F", "#{pane_id}"], "%").splitlines()
            control = panes[0]
            tmux_wait(["capture-pane", "-p", "-t", control], "oms>")
            for owner, key in (("codex", "1"), ("claude", "2")):
                subprocess.run(tmux + ["send-keys", "-t", control, key, "Enter"], env=env, check=True)
                tmux_wait(["list-windows", "-t", session, "-F", "#{window_name}"], owner)
                listing = tmux_wait(["list-panes", "-t", session + ":" + owner,
                                     "-F", "#{pane_id}"], "%").splitlines()
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], "NATIVE_FIXTURE_READY")
                tmux_wait(["capture-pane", "-p", "-t", listing[1]], "OMS dashboard")
            records = [json.loads(line) for line in log.read_text().splitlines()]
            assert {r["agent"] for r in records[-2:]} == set(panel.PROVIDERS)
            assert all(r["cwd"] == str(project.resolve()) for r in records[-2:])
            subprocess.run(tmux + ["send-keys", "-t", control, "q", "Enter"], env=env, check=True)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .1)[0]:
                    try:
                        os.read(fd, 65536)
                    except OSError:
                        pass
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
            assert finished, "detaching hung the launcher"
            assert subprocess.run(tmux + ["has-session", "-t", session], env=env).returncode == 0
        finally:
            subprocess.run(tmux + ["kill-server"], env=env, capture_output=True)
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)
print("terminal-panel: both directions, isolated patches, reviews, guards and PTY passed")
PY

echo 'operator-tools-smoke: ok'
