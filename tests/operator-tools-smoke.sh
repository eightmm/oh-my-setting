#!/usr/bin/env bash
set -euo pipefail
unset HERDR_ENV HERDR_SOCKET_PATH HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID \
  OMS_PANEL_HOST OMS_PANEL_VIEW OMS_PANEL_POSITION OMS_PANEL_COLOR
# The suite runs as a top-level operator even inside a panel window or a write worker, whose
# child/room bindings would otherwise turn its room fixtures into refusals.
unset TMUX TMUX_PANE OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH
for oms_inherited in $(compgen -e | grep -E '^OMS_(PANEL|ROOM)_' || true); do unset "$oms_inherited"; done

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
assert report["plan"]["idle_days"] == 0, report["plan"]
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
assert len(report["delegations"]) == 2 and report["delegations"][0]["live"], report["delegations"]
PY

# An old active attempt must survive a page full of newer terminal outcomes.
cp -R "$repo" "$TMP/panel-active"
python3 - "$TMP/panel-active" <<'PY'
import json, sys
from pathlib import Path
ledger = Path(sys.argv[1]) / ".oms/lifecycle/events.jsonl"
rows = [json.loads(line) for line in ledger.read_text().splitlines()]
created, cancelled = rows[0], rows[-1]
rows.append(dict(created, attempt_id="att_still_active", event_id="levt_active", task_id="panel-ui"))
for index in range(10):
    identity = "att_new_%s" % index
    rows.extend([dict(created, attempt_id=identity, event_id="levt_new_%s" % index,
                      ts="2026-09-01T00:00:%02dZ" % (index * 2)),
                 dict(cancelled, attempt_id=identity, event_id="levt_end_%s" % index,
                      seq=2, ts="2026-09-01T00:00:%02dZ" % (index * 2 + 1))])
ledger.write_text("".join(json.dumps(row) + "\n" for row in rows))
marker = Path(sys.argv[1]) / ".oms/delegations/live.json"
row = json.loads(marker.read_text())
row.update(task_id="panel-ui", model="model-for-live-worker", reasoning_effort="high")
marker.write_text(json.dumps(row))
PY
before="$(tree_sums "$TMP/panel-active/.oms")"
dash --repo "$TMP/panel-active" --json > "$TMP/panel-active.json"
[ "$before" = "$(tree_sums "$TMP/panel-active/.oms")" ] || fail "active panel mutated records"
python3 - "$TMP/panel-active.json" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
assert all(row["attempt_id"] != "att_still_active" for row in report["attempts"]["recent"])
assert report["attempts"]["active_recent"][0]["attempt_id"] == "att_still_active"
marker = report["delegations"][0]
assert (marker["task_id"], marker["requested_model"], marker["reasoning_effort"]) == (
    "panel-ui", "model-for-live-worker", "high"), marker
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
rows[-4]["model_attribution"] = ["bad"]
rows[-5]["fallback_reason"] = {"bad": 1}
rows[-6]["model_class"] = []
rows[-7]["status"] = {}
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
assert report["operations"]["recent"][3]["model_attribution"] == "unknown"
assert report["operations"]["recent"][4]["fallback_reason"] == "other"
assert report["operations"]["recent"][5]["route_class"] is None
assert report["operations"]["recent"][6]["status"] == "success"
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
# A panel board restarts after a crash instead of closing its pane; outside a
# panel session the same failure still exits.
board_bin="$TMP/board-bin"
mkdir -p "$board_bin"
real_python="$(command -v python3)"
cat > "$board_bin/python3" <<BOARD
#!/usr/bin/env bash
case "\$1" in *terminal_panel.py) ;; *) exec "$real_python" "\$@" ;; esac
count=\$(( \$(cat "$TMP/board-runs" 2>/dev/null || echo 0) + 1 ))
echo "\$count" > "$TMP/board-runs"
[ "\$count" -ge 2 ]
BOARD
chmod +x "$board_bin/python3"
TMUX=fixture OMS_PANEL_SESSION=oms-fixture PATH="$board_bin:$PATH" bash "$ROOT/scripts/panel.sh" --repo "$repo" --watch 2>"$TMP/board-err" ||
  fail "a crashed panel board must restart, not exit: $(cat "$TMP/board-err")"
[ "$(cat "$TMP/board-runs")" = 2 ] && grep -q 'OMS board stopped (exit 1); restarting' "$TMP/board-err" ||
  fail "the board should say it restarts and run again: $(cat "$TMP/board-err")"
rm -f "$TMP/board-runs"
if TMUX='' OMS_PANEL_SESSION=oms-fixture PATH="$board_bin:$PATH" bash "$ROOT/scripts/panel.sh" --repo "$repo" --watch 2>/dev/null; then
  fail "outside tmux a failed board must exit with its failure"
fi
[ "$(cat "$TMP/board-runs")" = 1 ] || fail "outside tmux the board must not loop"

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
# A run started inside an OMS panel must not inherit that panel's tmux or room binding.
for name in [n for n in os.environ if n in ("TMUX", "TMUX_PANE") or n.startswith(("OMS_PANEL_", "OMS_ROOM_"))]:
    del os.environ[name]
sys.path.insert(0, str(root / "scripts/lib"))
import terminal_panel as panel
from panel_routing import allocate
from dashboard_projection import display_width
from panel_view import render
from panel_view import wrapped

view = json.loads((temporary / "panel-active.json").read_text())
view["goal"] = {"text": "패널 개선 / show workers and verify results"}
original = json.dumps(view, sort_keys=True)
wide = render(view, "claude", 100, 32, previous="ready")
assert "MAIN / Claude Code" in wide and "[queued] Codex" in wide, wide
assert "panel-ui" in wide and "requested: model-for-live-worker" in wide, wide
assert "not accepted" in wide and "ACCEPT" in wide and "native subagents unobserved" not in wide, wide
assert wide.index("MAIN") < wide.index("OMS activity") < wide.index("[queued]"), wide
assert json.dumps(view, sort_keys=True) == original, "render changed its input records"
for width, height in ((100, 28), (38, 28), (24, 18), (12, 10), (2, 8)):
    for menu in (False, True):
        plain = render(view, "codex", width, height, menu=menu)
        assert "\033" not in plain and len(plain.splitlines()) <= height - int(menu), plain
        assert all(display_width(line) <= width for line in plain.splitlines()), plain
assert "[1] Codex" in render(view, "codex", 38, 28, menu=True)
hostile = json.loads(json.dumps(view))
hostile["delegations"][0]["requested_model"] = "bad\033[2J\u202eforged"
assert "\033" not in render(hostile, "claude", 100, 32)
assert "\u202e" not in render(hostile, "claude", 100, 32)
assert "\033[" in render(view, "claude", 100, 32, color=True)
# A model from a different attempt/provider is never copied into a worker card.
view["delegations"] = []
view["operations"]["recent"] = [{"attempt_id": "unrelated", "kind": "ask", "selected_model": "DO_NOT_ATTRIBUTE"}]
attributed_view = render(view, "codex", 100, 32)
assert "DO_NOT_ATTRIBUTE" in attributed_view.split("OMS activity", 1)[0]
assert "DO_NOT_ATTRIBUTE" not in attributed_view.split("OMS activity", 1)[1]
assert "model: unrecorded" in render(view, "codex", 100, 32)
view["attempts"]["recent"] = [{"state": "timed_out", "provider": "claude", "tool": "review"}]
assert "[timed_out]" in render(view, "codex", 100, 32)
assert "Worker state unavailable" in render({}, "codex", 100, 32)
assert "Collection degraded" in render({}, "codex", 100, 32)
# Titles occupy their own bright lines; metadata never competes for that space.
title = "작업 내용을 여러 줄로 읽기 쉽게 보여 주고 모델별 호출 흐름을 확인합니다"
parts = wrapped(title, 28, 4)
assert len(parts) > 1 and all(display_width(line) <= 28 for line in parts), parts
assert "..." not in " ".join(parts), parts
from copy import deepcopy
live = deepcopy(view)
live["attempts"]["recent"] = []
live["attempts"]["active_recent"] = [{"attempt_id": "att_display_live", "parent_attempt_id": "att_main",
    "state": "working", "provider": "claude", "tool": "peer-delegate", "task_id": "readable-title",
    "panel": {"role": "worker", "model": "claude-sonnet-5-5", "label": title,
              "access": "write", "location": "worktree:example/wt", "effort": "medium"}}]
first = render(live, "codex", 42, 34, frame=0)
second = render(live, "codex", 42, 34, frame=1)
assert first != second and "작업 내용을 여러 줄로" in first and "worktree example/wt" in first, first
# Display hierarchy follows explicit ownership; generic parent links are not
# native-main relationships. Compact and attention views never alter records.
from panel_view import hierarchy, worker_rows, render_results
tree = deepcopy(live)
tree["attempts"]["active_recent"] = [
    {"attempt_id": "att_main", "state": "working", "provider": "codex", "tool": "panel-main",
     "panel": {"role": "main", "model": "gpt-6-sol", "label": "Sol main task"}},
    {"attempt_id": "att_opus", "state": "working", "provider": "claude", "tool": "panel-main",
     "panel": {"role": "main", "model": "claude-opus-5-5", "label": "Opus main task"}},
    *tree["attempts"]["active_recent"],
    {"attempt_id": "att_adv", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "panel": {"role": "advisor", "model": "gpt-6-astra", "label": "Architecture advice"}},
    {"attempt_id": "att_rev", "parent_attempt_id": "att_main", "state": "review", "provider": "claude",
     "panel": {"role": "reviewer", "model": "claude-fable-5-1", "label": "Pending review"}},
    {"attempt_id": "att_other", "parent_attempt_id": "att_opus", "state": "blocked", "provider": "codex",
     "panel": {"role": "worker", "model": "gpt-6-luna", "label": "Opus child blocked"}},
    {"attempt_id": "att_legacy", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "tool": "legacy-unlinked"}]
saved_tree = json.dumps(tree, sort_keys=True)
forest = hierarchy(tree, worker_rows(tree))
assert {r["title"] for r in forest} == {"MAIN / Sol 6", "MAIN / Opus 5.5", "No known main"}, forest
full_tree = render(tree, "codex", 100, 60, view="detail")
assert full_tree.index("MAIN / Sol 6 /") < full_tree.index("ADVISORS (1)") < full_tree.index("Architecture advice"), full_tree
assert "REVIEWERS (1)" in full_tree and "Opus child blocked" in full_tree and "No known main" in full_tree
assert re.search(r"^│.*\[working\] Astra.*│$", full_tree, re.M), full_tree
# A running council groups its seats as one visible debate, not loose advisors.
debate = deepcopy(tree)
seat = {"parent_attempt_id": "att_main", "provider": "claude", "tool": "ask", "task_id": "decide",
        "panel": {"role": "advisor", "model": "claude-fable-5-1", "label": "Pick next work"}}
debate["attempts"]["active_recent"] = debate["attempts"]["active_recent"][:2] + [
    {"attempt_id": "att_council", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "tool": "panel-council", "task_id": "decide", "panel": {"role": "advisor", "label": "Pick next work"}},
    dict(seat, attempt_id="att_seat", state="working")]
debate["attempts"]["recent"] = [dict(seat, attempt_id="att_seat_done", state="done")]
debate_tree = render(debate, "codex", 100, 60, view="detail")
assert "COUNCIL (2)" in debate_tree and "Debate: Pick next work" in debate_tree, debate_tree
assert "1 seat(s) answering" in debate_tree and "ADVISORS" not in debate_tree, debate_tree
# The debate reader: seats with stances, agreement only from synthesis headings, the owner's decision, the full-text path.
import panel_debate
reader_report = {"collection": {"ok": True}, "room": {"id": "r", "title": "T", "participants": [
    {"participant": "m1", "role": "main", "joined": True, "model": "gpt-6-sol", "seq": 1, "provider": "codex"}]},
    "attempts": {"active_recent": [{"attempt_id": "a1", "state": "working",
                                    "panel": {"room_id": "r", "room_participant": "m1", "role": "main"}}],
                 "recent": [{"attempt_id": "c1", "tool": "panel-council", "task_id": "decide", "state": "done",
                             "updated_at": "2026-10-07T10:00:00Z", "parent_attempt_id": "a1",
                             "panel": {"role": "advisor", "label": "Pick next work", "room_id": "r", "room_participant": "m1"}}]}}
assert "d Debates" in render(reader_report, "codex", 110, 24, view="graph", navigation={"dismissed": True})
assert "d Debates" not in render(tree, "codex", 110, 24, view="graph", navigation={"dismissed": True})
reader_nav = {}
listed = render(reader_report, "codex", 100, 24, view="debate", navigation=reader_nav)
assert "Pick next work - m1 - done" in listed and "Debates" in listed, listed
ask = ".oms/artifacts/ask/"
reader_nav["debate_view"]["open"] = ("decide", "m1")
reader_nav["debate_view"]["report"] = {"rows": [{"title": "Pick next work", "summary": "Ship X", "outcome": "accepted", "calls": [
    {"kind": "ask", "selected_model": "gpt-6-sol", "artifact": ask + "codex-gpt-6-sol-s-1.md", "answer": "Answer: Ship X first. More.\nRisks: none"},
    {"kind": "ask", "selected_model": "gpt-6-sol", "artifact": ask + "codex-gpt-6-sol-s-1-r2.md", "answer": "VERDICT: ship Y\x1b[31m\nAnswer: Y."},
    {"kind": "ask", "selected_model": "claude-fable-5-1", "artifact": ask + "claude-claude-fable-5-1-s-1.md", "answer": "plain answer"}]}],
    "synthesis": {"path": ask + "_synthesis-s-1.md",
                  "text": "## Prompt\n### Agreement\nasked text\n## Output\n### Agreement\nTests first.\n### Dissent\nOrder differs.\n"}}
opened = render(reader_report, "codex", 100, 40, view="debate", navigation=reader_nav)
for needle in ("Sol 6 (Codex)", "Fable 5.1 (Claude Code)", "ship Y", "Changed after round 1: was Ship X first.", "plain answer",
               "Agreement\n  Tests first.", "Disagreement\n  Order differs.", "Ship X (outcome: accepted)",
               "Full text: " + ask + "_synthesis-s-1.md"):
    assert needle in opened, (needle, opened)
assert "asked text" in opened and "Agreement\n  asked" not in opened and "\x1b" not in opened, opened
reader_nav["debate_view"]["report"]["synthesis"]["text"] = "## Output\nplain prose"
reader_nav["debate_view"]["report"]["rows"][0].update(summary=None, outcome=None)
bare = render(reader_report, "codex", 100, 40, view="debate", navigation=reader_nav)
assert "Agreement" not in bare and "Disagreement" not in bare and "No decision recorded yet" in bare, bare
reader_nav = {}
render(reader_report, "codex", 100, 12, view="debate", navigation=reader_nav)
assert panel_debate.handle(("click", 5, reader_nav["hits"][0]["y"]), reader_nav) and reader_nav["debate_view"]["open"] == ("decide", "m1")
reader_nav["debate_view"]["report"] = {"rows": [{"calls": [{"kind": "ask", "selected_model": "gpt-6-sol", "answer": "x\n" * 30}]}]}
render(reader_report, "codex", 100, 12, view="debate", navigation=reader_nav)
assert panel_debate.handle(("pagedown",), reader_nav) and reader_nav["offset"] > 0
# The messages reader: names, local times, unread marks, filters, in-place full text, card clicks; reading never writes.
import panel_messages
msg_members = [{"participant": "m1", "role": "main", "joined": True, "model": "claude-opus-5-5", "seq": 1, "provider": "claude", "attempt": "a1"},
               {"participant": "m2", "role": "main", "joined": True, "model": "gpt-6.1-sol", "seq": 2, "provider": "codex", "attempt": "a2"},
               {"participant": "m3", "role": "main", "joined": True, "model": "gpt-6-luna", "seq": 3, "provider": "codex", "attempt": "a3"}]
msg_log = [{"id": "n%d" % i, "sender": "m1" if i % 3 else "m3", "recipient": "m2" if i % 3 else "m1", "targets": ["m2" if i % 3 else "m1"],
            "message_kind": "question" if i % 5 == 0 else "note", "ts": "2026-10-0%dT10:%02d:00Z" % (6 if i < 5 else 7, i),
            "pending_for": ["m2"] if i == 29 else [], "text": "line %d\nsecond \x1b[31mred" % i} for i in range(30)]
msg_log[28]["text"] = "long " + "word " * 80 + "END\n\nparagraph two"
msg_report = {"collection": {"ok": True}, "attempts": {"active_recent": [
    {"attempt_id": "a%d" % n, "state": "working", "panel": {"room_id": "r", "room_participant": "m%d" % n, "role": "main"}} for n in (1, 2, 3)]},
              "main_windows": {"m1": 1, "m2": 2, "m3": 3},
              "room": {"id": "r", "title": "T", "participants": msg_members, "message_count": 30, "messages": msg_log[-12:]}}
msg_nav = {}
before = deepcopy(msg_report)
shown = render(msg_report, "codex", 110, 8, view="messages", navigation=msg_nav, main_attempt="a2")
assert "OMS / Messages / T" in shown and "line 29" in shown and "line 20" not in shown, shown
assert "#1 Opus 5.5 → #2 Sol 6.1" in shown and "· unread" in shown and "\x1b" not in shown, shown
assert msg_report == before, "reading changed the report"
msg_nav["messages_view"]["log"] = msg_log
render(msg_report, "codex", 110, 20, view="messages", navigation=msg_nav, main_attempt="a2")
assert panel_messages.handle(("up",), msg_nav) and panel_messages.handle(("enter",), msg_nav)
opened_message = render(msg_report, "codex", 60, 30, view="messages", navigation=msg_nav, main_attempt="a2")
assert "END" in opened_message and "paragraph two" in opened_message and opened_message.count("word") > 70, opened_message
assert panel_messages.handle(("enter",), msg_nav) and msg_nav["messages_view"]["open"] is None
msg_nav["selected"] = ("chat", "m3")
panel_messages.handle(("right",), msg_nav)
narrowed = render(msg_report, "codex", 110, 40, view="messages", navigation=msg_nav, main_attempt="a2")
assert "Filter: to/from #3 Luna 6" in narrowed and "#2 Sol 6.1 main" not in narrowed and "line 27" in narrowed and "line 29" not in narrowed, narrowed
panel_messages.handle(("right",), msg_nav)
mine = render(msg_report, "codex", 110, 40, view="messages", navigation=msg_nav, main_attempt="a3")
assert "to/from me" in mine and "#3 Luna 6 → #1" in mine and "line 29" not in mine, mine
ascii_messages = render(msg_report, "codex", 110, 20, view="messages", navigation=msg_nav, main_attempt="a2", unicode=False)
assert ascii_messages.isascii(), ascii_messages
card_nav = {"dismissed": True}
board = render(msg_report, "codex", 110, 40, view="graph", navigation=card_nav)
assert "RECENT MESSAGES" in board and "m Messages" in board, board
card_hit = next(h for h in card_nav["hits"] if h["action"][0] == "message")
assert board.split("\n")[card_hit["y"] - 1].lstrip().startswith(("│", "|")) and "line" in board.split("\n")[card_hit["y"] - 1], board
assert panel_messages.clicked(("click", 5, card_hit["y"]), card_nav) == card_hit["action"][1]
panel_messages.open_message(card_nav, "n29")
assert card_nav["messages_view"]["open"] == "n29"
empty_calls = deepcopy(tree)
empty_calls["attempts"]["active_recent"] = empty_calls["attempts"]["active_recent"][:2]
idle_mains = render(empty_calls, "codex", 100, 40)
assert "MAIN / Sol 6 / working" in idle_mains and "MAIN / Opus 5.5 / working" in idle_mains
assert "No active child calls" in idle_mains
own_tree = render(tree, "codex", 100, 60, main_attempt="att_main", view="detail")
assert "Sol main task" in own_tree and "Opus child blocked" not in own_tree and "legacy-unlinked" not in own_tree
compact_tree = render(tree, "codex", 100, 60, view="compact")
assert "Architecture advice" in compact_tree and "worktree example/wt" not in compact_tree
assert len(compact_tree.splitlines()) < len(full_tree.splitlines())
attention_tree = render(tree, "codex", 100, 60, attention_only=True)
assert "Pending review" in attention_tree and "Opus child blocked" in attention_tree
assert "Architecture advice" not in attention_tree and "filtered" in attention_tree
assert json.dumps(tree, sort_keys=True) == saved_tree
# Closed root cards replace deep indentation; role counts and attention titles
# stay readable in the short menu, with stable roots as child states change.
short_tree = render(tree, "codex", 100, 28)
assert "Opus child blocked" in short_tree and "REVIEWERS (1)" in short_tree, short_tree
assert "WORKERS (1)" in short_tree and "No known main" in short_tree, short_tree
assert "2 advisor" not in short_tree
changed_tree = deepcopy(tree)
changed_tree["attempts"]["active_recent"][4]["state"] = "blocked"
changed_text = render(changed_tree, "codex", 100, 60, view="detail")
assert full_tree.index("MAIN / Sol 6 /") < full_tree.index("MAIN / Opus") < full_tree.index("No known main")
assert changed_text.index("MAIN / Sol 6 /") < changed_text.index("MAIN / Opus") < changed_text.index("No known main")
missing_model = deepcopy(tree)
missing_model["attempts"]["active_recent"][0]["panel"].pop("model")
missing_text = render(missing_model, "codex", 42, 34, main_attempt="att_main")
assert "model unrecorded" in missing_text and "Sol 6 / high / working" not in missing_text, missing_text

# Usage is the recent artifact window, never an account total or an overlapping
# lifecycle sum. Mixed routes and selected-only identities stay separate.
from dashboard_projection import project_acceptance
malformed_acceptance = project_acceptance({"healthy": True, "criteria": [
    {"id": "malformed", "status": ["bad"]}, {"id": "valid", "status": "failed"}]})
assert [c["id"] for c in malformed_acceptance["criteria"]] == ["valid", "malformed"]
from panel_view import model_usage, usage_labels
usage_view = deepcopy(tree)
usage_view["operations"] = {"available": True, "recent": [
    {"event_id": "use1", "kind": "ask", "provider": "codex", "selected_model": "gpt-6-astra",
     "served_model": "gpt-6-sol", "tokens": 120, "cost_usd": .25},
    {"event_id": "use2", "kind": "ask", "provider": "codex", "served_model": "gpt-6-sol"},
    {"event_id": "use3", "kind": "ask", "provider": "codex", "selected_model": "gpt-6-sol", "tokens": 0},
    {"event_id": "use4", "kind": "review", "provider": "claude", "selected_model": "claude-fable-5-1",
     "tokens": 7, "cost_usd": .5, "model_attribution": "ambiguous"},
    {"event_id": "use5", "kind": "delegate", "provider": "claude", "selected_model": "claude-opus-5-5",
     "tokens": 9, "cost_usd": .1, "fallback_used": True},
    {"event_id": "owner", "kind": "owner-finalize", "tokens": 99999, "cost_usd": 100},
    {"event_id": "gate", "kind": "review-outcome", "tokens": 99999, "cost_usd": 100}]}
usage_view["operations"]["recent"].append(deepcopy(usage_view["operations"]["recent"][0]))
usage_view["attempts"]["recent"] = [{"attempt_id": "use1", "tokens": 50000, "state": "done"}]
groups = model_usage(usage_view)
assert len(groups) == 3 and sum(g["records"] for g in groups) == 5, groups
served, selected, mixed = groups
assert (served["name"], served["tokens"], served["token_reports"], served["records"]) == ("Sol 6", 120, 1, 2), groups
assert selected["attribution"] == "selected" and selected["tokens"] == 0 and selected["token_reports"] == 1
assert mixed["name"] == "Mixed models" and mixed["tokens"] == 16 and abs(mixed["cost"] - .6) < .000001
labels = " | ".join(usage_labels(usage_view, costs=True))
assert "120+? tok" in labels and "$0.25+?" in labels and "Sol 6(sel) 0 tok" in labels, labels
assert "Astra" not in labels and "Opus" not in labels and "$?" in labels, labels
assert "USAGE" in render(usage_view, "codex", 100, 28)
assert "USAGE" in render(usage_view, "claude", 42, 34, main_attempt="att_main")
assert model_usage(usage_view) == model_usage({**usage_view, "attempts": {}})
bad_usage = {"operations": {"recent": [{"kind": "ask", "tokens": True, "cost_usd": float("nan")}]}}
assert "? tok" in usage_labels(bad_usage, costs=True)[0] and "$?" in usage_labels(bad_usage, costs=True)[0]
assert model_usage(bad_usage)[0]["token_reports"] == 0
assert "Usage unavailable" in render({"operations": {"available": False}}, "codex", 42, 34)
crowded_scope = render(tree, "codex", 32, 24, main_attempt="att_main", view="detail", attention_only=True,
                       previous="ready")
overflow_row = next(line for line in crowded_scope.splitlines() if "Details" in line)
assert "f2" in overflow_row and "out2" in overflow_row and "..." not in overflow_row, crowded_scope

# Wide compact rows use one line per call. Tall detail retains the same titles
# and metadata; narrow cards preserve title space without tree indentation.
many_calls = deepcopy(tree)
many_calls["attempts"]["active_recent"] = [many_calls["attempts"]["active_recent"][0]]
for n in range(8):
    call_row = deepcopy(tree["attempts"]["active_recent"][2])
    call_row.update(attempt_id="many_%s" % n, task_id="many_%s" % n)
    call_row["panel"]["label"] = "Distinct task %s" % n
    many_calls["attempts"]["active_recent"].append(call_row)
many_text = render(many_calls, "codex", 100, 28)
assert all("Distinct task %s" % n in many_text for n in range(8)), many_text
assert "more" not in many_text, many_text

def closed_cards(text, width, unicode=True):
    opened = False
    for line in text.splitlines():
        if unicode and line.startswith("╭"):
            assert not opened and line.endswith("╮") and display_width(line) == width, text
            opened = True
        elif unicode and line.startswith("╰"):
            assert opened and line.endswith("╯") and display_width(line) == width, text
            opened = False
        elif unicode and line.startswith(("│", "├")):
            assert opened and line.endswith("│" if line.startswith("│") else "┤"), text
            assert display_width(line) == width, text
        elif not unicode and line.startswith("+") and line.endswith("+"):
            assert display_width(line) == width, text
            if set(line[1:-1]) == {"-"}:
                assert opened, text
                opened = False
            elif not opened:
                opened = True
        elif not unicode and line.startswith("|"):
            assert opened and line.endswith("|") and display_width(line) == width, text
    assert not opened, text

for width, height in ((100, 28), (42, 34), (24, 18), (12, 10), (2, 8)):
    for density in ("auto", "compact", "detail"):
        for filtered in (False, True):
            for glyphs in (False, True):
                text = render(tree, "codex", width, height, view=density, attention_only=filtered, unicode=glyphs, menu=True)
                assert len(text.splitlines()) <= height - 1 and all(display_width(line) <= width for line in text.splitlines()), text
                closed_cards(text, width, glyphs)
for width, height in ((80, 28), (90, 28), (30, 20), (42, 24), (100, 24)):
    for density in ("auto", "compact", "detail"):
        for glyphs in (False, True):
            text = render(usage_view, "codex", width, height, view=density, unicode=glyphs,
                          menu=True, managed=True, previous="ready")
            closed_cards(text, width, glyphs)
idle = deepcopy(live)
idle["attempts"]["active_recent"][0]["state"] = "blocked"
assert render(idle, "codex", 42, 34, frame=0) == render(idle, "codex", 42, 34, frame=1)
for width, height in ((100, 32), (42, 34), (24, 18), (12, 10), (2, 8)):
    for frame in (None, 0, 1):
        text = render(live, "codex", width, height, frame=frame)
        assert len(text.splitlines()) <= height and all(display_width(line) <= width for line in text.splitlines()), text
# Frames use one cached snapshot. --count still counts collections, and resize
# triggers one complete redraw rather than leaving stale lines in the pane.
import io
from unittest.mock import patch
with patch.dict(os.environ, {"OMS_CODEX_NOTIFY": "0", "OMS_CLAUDE_NOTIFY": "0"}, clear=True):
    assert "OMS_CLAUDE_NOTIFY=0" in panel.panel_environment()
for mode in ("animated", "static", "environment", "interrupt"):
    clock = [0.0]
    output = io.StringIO()
    def sleep(seconds):
        if mode == "interrupt":
            raise KeyboardInterrupt
        clock[0] += seconds
    with patch.object(panel, "snapshot", return_value=(live, 0)) as collect, \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", output), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((42, 34))), \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=sleep), \
            patch.dict(os.environ, {"OMS_PANEL_NO_ANIMATION": "1" if mode == "environment" else "0"}):
        try:
            assert panel.watch(Path("."), "codex", count=2, no_animation=mode == "static") == 0
            assert mode != "interrupt", "watch swallowed the interrupt"
        except KeyboardInterrupt:
            assert mode == "interrupt"
    assert collect.call_count == (1 if mode == "interrupt" else 2)
    assert clock[0] == (0 if mode == "interrupt" else 5.0), clock
    assert output.getvalue().count("\033[2J") == 1 and "\033[?25h" in output.getvalue(), output.getvalue()
    assert ("\033[2K" in output.getvalue()) == (mode == "animated")
# A blocked state read must not block view keys or quitting the terminal.
import threading
import time
for pressed, shown in ("t", "tree"), ("d", "debate"), ("m", "messages"):
  entered, released, finished_read = threading.Event(), threading.Event(), threading.Event()
  class ResponsiveInput:
      fd = 0
      def __init__(self, *args, **kwargs):
          self.events = iter([[(pressed,)], [("q",)]])
      def __enter__(self):
          return self
      def __exit__(self, *args):
          pass
      def discard_clicks(self):
          pass
      def wait(self, seconds):
          assert entered.wait(1), "background read never started"
          assert not released.is_set(), "read completed before input was handled"
          return next(self.events)
  def blocked_snapshot(*args, **kwargs):
      entered.set()
      try:
          assert released.wait(3), "fixture failed to release the read"
          return live, 0
      finally:
          finished_read.set()
  with patch.object(panel, "snapshot", side_effect=blocked_snapshot), \
          patch.object(panel, "managed_session", return_value=False), \
          patch.object(panel, "terminal_style", return_value=(True, False, True)), \
          patch.object(panel.sys, "stdout", io.StringIO()), \
          patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
          patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
          patch("panel_input.TerminalInput", ResponsiveInput):
      started = time.monotonic()
      try:
          assert panel.watch(Path("."), "codex", no_animation=True) == 0
          assert time.monotonic() - started < .5, "blocked collector delayed input"
          assert display.call_args.kwargs["view"] == shown
      finally:
          released.set()
          assert finished_read.wait(1)

# A completed refresh must preserve standalone view choices made with keys.
class ImmediateRead:
    pending = False
    def start(self, reader):
        self.result, self.pending = reader(), True
    def take(self):
        ready, self.pending = self.pending, False
        return ready, self.result if ready else None
clock = [0.0]
def change_view(seconds):
    clock[0] += 5
    return [("g",)]
with patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "read_panel", return_value=(live, 0, "codex", None, "summary", False, "")), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
        patch.object(ResponsiveInput, "wait", side_effect=change_view), \
        patch("panel_input.TerminalInput", ResponsiveInput):
    assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert display.call_args.kwargs["view"] == "graph", "refresh reset the key-selected view"

# Window controls can change between snapshots. Explicit CLI choices persist;
# rendered text must never overwrite the density option on later refreshes.
for requested, forced in ((None, False), ("compact", True)):
    clock = [0.0]
    def pane_value(name):
        return {"@oms_panel_owner": "codex", "@oms_panel_main_attempt": "att_main",
                "@oms_panel_view": "compact" if clock[0] < 5 else "detail",
                "@oms_panel_attention": "0" if clock[0] < 5 else "1"}.get(name, "")
    with patch.object(panel, "snapshot", return_value=(tree, 0)), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "pane_option", side_effect=pane_value), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds)):
        assert panel.watch(Path("."), "codex", count=2, view=requested, attention_only=forced) == 0
    assert [c.kwargs["view"] for c in display.call_args_list] == (["compact", "compact"] if requested else ["compact", "detail"])
    assert [c.kwargs["attention_only"] for c in display.call_args_list] == ([True, True] if forced else [False, True])
# A flag-set attention filter (--attention-only, as the Herdr watcher uses) still yields to the b key.
clock = [0.0]
def press_b(seconds):
    clock[0] += 5
    return [("b",)] if clock[0] == 5 else []
with patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "snapshot", return_value=(live, 0)), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
        patch.object(ResponsiveInput, "wait", side_effect=press_b), \
        patch("panel_input.TerminalInput", ResponsiveInput):
    assert panel.watch(Path("."), "codex", count=3, no_animation=True, attention_only=True) == 0
    assert [c.kwargs["attention_only"] for c in display.call_args_list][0] is True
    assert [c.kwargs["attention_only"] for c in display.call_args_list][-1] is False, display.call_args_list
with patch.object(panel.sys, "stdout", io.StringIO()) as output:
    last = panel.draw_frame("first\nsecond", (42, 34), True)
    panel.draw_frame("first\nchanged", (42, 34), True, last)
    panel.draw_frame("small", (24, 18), True, last)
    assert output.getvalue().count("\033[2J") == 2 and "\033[2;1H\033[2Kchanged" in output.getvalue()
from unittest.mock import patch
with patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired("tmux", 5)):
    assert panel.pane_option("@oms_panel_view") == ""
# A failed collection remains visible and the same watcher recovers on its next
# refresh. Invalid window selection must not display another room as selected.
import thread_live
import room
for failure in (([], 0), subprocess.TimeoutExpired("dashboard", 5)):
    clock = [0.0]
    responses = [failure, (live, 0)]
    with patch.object(panel, "dashboard", side_effect=responses), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds)), \
            patch.dict(os.environ, {}, clear=True), patch.object(thread_live, "current_thread", return_value=None):
        assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert displayed.call_args_list[0].args[2]["collection"]["ok"] is False
    assert displayed.call_args_list[1].args[2] == live
with patch.object(panel, "snapshot", return_value=(live, 0)), \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "pane_option", side_effect=lambda name: "invalid room" if name == "@oms_panel_room" else ""), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed:
    assert panel.watch(Path("."), "codex", count=1, no_animation=True) == 1
assert "invalid window room selection" in displayed.call_args.args[2]["room"]["error"]
clock = [0.0]
with patch.object(panel, "dashboard", side_effect=[(deepcopy(live), 0), (deepcopy(live), 0)]), \
        patch.object(room, "status", side_effect=lambda repo, ident: {"id": ident}), \
        patch.object(thread_live, "current_thread", return_value="unrelated-current") as current, \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "pane_option", side_effect=lambda name: "selected-room" if name == "@oms_panel_room" and clock[0] < 5 else ""), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel.time, "sleep", side_effect=lambda s: clock.__setitem__(0, clock[0]+s)), \
        patch.dict(os.environ, {}, clear=True):
    assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert displayed.call_args_list[0].args[2]["room"]["id"] == "selected-room"
    assert "room" not in displayed.call_args_list[1].args[2]
    assert current.call_count == 0
# Choose an actual result by number. Only its task ID is used for the detail
# query; returning to the list does not call or retry a model or app receiver.
sample_rows = [{"task_id": "one", "title": "First result", "summary": "FIRST DETAIL"},
               {"task_id": "two", "title": "Second result", "summary": "SECOND DETAIL"}]
def result_query(repo, task_id=None):
    return {"rows": [r for r in sample_rows if not task_id or r["task_id"] == task_id]}
with patch.object(panel, "results", side_effect=result_query) as query, \
        patch.object(panel, "read_field", side_effect=["0", "2", "", ""]), \
        patch.object(panel.sys, "stdout", io.StringIO()) as output:
    panel.browse_results(Path("."))
    assert query.call_args_list[2].args == (Path("."), "two"), query.call_args_list
    assert "SECOND DETAIL" in output.getvalue() and "FIRST DETAIL" not in output.getvalue()
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "show") as display, \
        patch.object(panel, "read_field", side_effect=["v", "b", "v", "v", "q"]):
    assert panel.interactive(Path(".")) == 0
    assert [c.kwargs["view"] for c in display.call_args_list] == ["auto", "compact", "compact", "detail", "auto"]
    assert [c.kwargs["attention_only"] for c in display.call_args_list] == [False, False, True, True, True]
for setting in ({"TERM": "dumb"}, {"TERM": "xterm", "NO_COLOR": ""}):
    saved = dict(os.environ)
    try:
        os.environ.update(setting)
        with patch.object(panel.sys.stdout, "isatty", return_value=True):
            assert panel.terminal_style()[1] is False
    finally:
        os.environ.clear()
        os.environ.update(saved)
for setting, expected in (({"TERM": "xterm", "NO_COLOR": "1", "OMS_PANEL_COLOR": "always"}, True),
                          ({"TERM": "xterm", "OMS_PANEL_COLOR": "never"}, False),
                          ({"TERM": "dumb", "OMS_PANEL_COLOR": "always"}, False)):
    with patch.dict(os.environ, setting, clear=True), patch.object(panel.sys.stdout, "isatty", return_value=True):
        assert panel.terminal_style()[1] is expected
        assert "OMS_PANEL_COLOR=" + setting["OMS_PANEL_COLOR"] in panel.panel_environment()
with patch.dict(os.environ, {"TERM": "xterm", "OMS_PANEL_COLOR": "always"}, clear=True), \
        patch.object(panel.sys.stdout, "isatty", return_value=False):
    assert panel.terminal_style()[1] is False, "redirected output must stay plain"

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
worker_log = temporary / "panel-worker-log.jsonl"
for provider in (*panel.PROVIDERS, "grok", "agy", "vibe", "oms-agent-adapter-muse"):
    executable = binary / provider
    executable.write_text('''#!/usr/bin/env bash
set -eu
case "${1:-}:${2:-}" in
  --version:|--help:|exec:--help|--no-auto-update:version|--no-auto-update:--help) printf 'fixture CLI 1.0 --effort low medium high --config\\n'; exit 0 ;;
esac
if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  python3 - "$@" <<'WORKER'
import json, os, sys
with open(os.environ["PANEL_TEST_WORKER_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(),
                        "role": os.environ.get("OMS_PANEL_ROLE"),
                        "parent": os.environ.get("OMS_PANEL_MAIN_ATTEMPT"),
                        "room": os.environ.get("OMS_ROOM_ID"),
                        "participant": os.environ.get("OMS_ROOM_PARTICIPANT")}) + "\\n")
WORKER
  cat >/dev/null
  if [ "${PANEL_TEST_MODE:-}" = write ]; then
    printf 'new\\n' >> value.txt
    if bash "$OMS_PANEL_ENTRYPOINT" panel --launch codex >/dev/null 2>&1; then
      echo 'nested owner launch should have failed' >&2
      exit 1
    fi
  fi
  if [ "${OMS_PANEL_PURPOSE:-}" = advise ]; then
    printf 'Answer: inspected source.\nAlternatives: separate panels.\nEvidence: value.txt exists.\nCounterargument: retained history is bounded.\nRisks: delivery can fail.\nRecommendation: owner records evidence.\nVerification: inspect source.\nChanged from previous round: none.\nRemaining disagreements: none.\n'
    exit 0
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
  if [ "${PANEL_TEST_NATIVE_HOLD:-0}" = 1 ]; then read -r reply || :; printf 'NATIVE_CHAT_REPLY:%s\\n' "$reply"; fi
fi
''')
    executable.chmod(0o755)

environment = dict(os.environ, HOME=str(home), NVM_DIR=str(home / ".nvm"),
                   PATH=str(binary) + os.pathsep + os.environ["PATH"],
                   XDG_STATE_HOME=str(temporary / "panel-state"),
                   OMS_PANEL_ENTRYPOINT=str(panel.ENTRY), PANEL_TEST_LOG=str(log),
                   PANEL_TEST_WORKER_LOG=str(worker_log), OMS_CAPABILITY_SKIP_MODELS="1",
                   OMS_PEER_TIMEOUT="30", OMS_ROLE_ROUTING="0")
for key in ("OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH", "OMS_PANEL_SESSION", "TMUX",
            "OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_DISPATCH", "OMS_ATTEMPT_ID",
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
# Discovery is registry-driven and does not execute even marked adapters.
inventory = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--providers", "--json"]))
registered = {row["provider"]: row for row in inventory["providers"]}
assert inventory["executes"] is False and inventory["kind"] == "oms-panel-providers"
assert not log.exists() and not worker_log.exists() and not (project / ".oms").exists()
for name in ("grok", "antigravity", "muse"):
    assert registered[name]["installed"] and not registered[name]["native_launch"], registered[name]
    assert registered[name]["session_messaging"] == "room_polling"
    assert registered[name]["native_hook_wiring"] == "unverified"
assert registered["muse"]["access"] == ["read"]
assert registered["codex"]["native_launch"] and registered["claude"]["native_launch"]
assert shlex.split(panel.shell_command(["bash", str(panel.ENTRY), str(project)]))[1:] == ["bash", str(panel.ENTRY), str(project)]
assert allocate("claude", workload="light")["model"] == "gpt-6-luna"
assert allocate("codex", workload="routine")["model"] == "claude-sonnet-5-5"
assert allocate("codex", workload="main")["model"] == "gpt-6.1-sol"
assert allocate("claude", workload="main")["model"] == "claude-opus-5-5"
assert allocate("claude", role="advisor")["model"] == "gpt-6-astra"
assert allocate("codex", role="advisor")["model"] == "claude-fable-5-1"
assert allocate("codex", role="advisor", seat="astra")["model"] == "gpt-6-astra"
assert allocate("claude", role="advisor", seat="fable")["model"] == "claude-fable-5-1"
for owner in panel.PROVIDERS:
    arguments = panel.native_command(owner, project)
    expected = "gpt-6.1-sol" if owner == "codex" else "claude-opus-5-5"
    assert arguments[arguments.index("--model") + 1] == expected, arguments
    assert "--dispatch worker" in panel.bootstrap(owner, project)
    assert "--dispatch advisor" in panel.bootstrap(owner, project)
    assert "control tower: by default delegate" in panel.bootstrap(owner, project)
option_text = "--dangerously-skip-permissions"
assert panel.native_command("claude", project, task=option_text)[-2:] == ["--", option_text]
assert option_text not in panel.native_command("codex", project, task=option_text)[:-1]

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
    patch_match = re.search(r"^patch: (.+)$", answer, re.M)
    assert patch_match and "+new" in Path(patch_match.group(1)).read_text(), answer
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

# Exercise exact role routes through the real provider runner, with one
# recorded main per caller and read/write authority tested independently.
main_attempts = {}
for owner in panel.PROVIDERS:
    main_attempts[owner] = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                               "start", "--provider", owner, "--tool", "panel-main", "--ref", "panel_role=main",
                               "--then", "starting", "--then", "working"]).strip()
cases = [("claude", "worker", "light", "read", "explain", "gpt-6-luna"),
         ("codex", "worker", "routine", "read", "explain", "claude-sonnet-5-5"),
         ("codex", "worker", "main", "read", "investigate", "gpt-6.1-sol"),
         ("claude", "worker", "main", "write", "implement", "claude-opus-5-5"),
         ("claude", "advisor", "routine", "read", "advise", "gpt-6-astra"),
         ("codex", "advisor", "routine", "read", "advise", "claude-fable-5-1"),
         ("claude", "reviewer", "routine", "read", "review", "gpt-6-astra")]
for index, (owner, role, workload, access, purpose, expected) in enumerate(cases):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", role,
               "--owner", owner, "--workload", workload, "--access", access, "--purpose", purpose,
               "--task-id", "panel-route-%s" % index, "--label", "Inspect fixture source %s" % index]
    if access == "write":
        request += ["--brief-file", str(brief), "--verify", "test \"$(tail -1 value.txt)\" = new"]
    else:
        request += ["--prompt", "Explain value.txt; judge only the supplied source evidence."]
        if role == "reviewer":
            request += ["--verify", "test -f value.txt"]
    before = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
    route = json.loads(call(request + ["--dry-run"]))
    assert route["route"]["model"] == expected and route["executes"] is False, route
    env = dict(environment, OMS_AGENT=owner, OMS_PANEL_MAIN_ATTEMPT=main_attempts[owner])
    if access == "write":
        env["PANEL_TEST_MODE"] = "write"
    answer = call(request, env)
    if access == "write":
        assert "patch:" in answer, answer
        write_patch = re.search(r"^patch: (.+)$", answer, re.M).group(1)
    assert (project / "value.txt").read_text() == "old\n", "role task edited primary"
    assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == before
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(row for row in attempts if row.get("task_id") == "panel-route-%s" % index)
    refs = attempt["refs"]
    assert refs["panel_role"] == role and refs["panel_model"] == expected, attempt
    assert refs["panel_effort"] == route["route"]["effort"], refs
    assert refs["panel_access"] == access and refs["panel_label"] == "Inspect fixture source %s" % index, refs
    assert attempt["parent_attempt_id"] == main_attempts[owner], attempt
    assert attempt["state"] == ("review" if access == "write" else "done"), attempt
    assert refs["panel_location"].startswith("worktree:") if role == "worker" else refs["panel_location"] == "repository", refs
    assert str(home) not in json.dumps(refs) and str(project) not in json.dumps(refs), refs
    if role == "advisor":
        retained = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                                    "--task-id", "panel-route-%s" % index, "--json"]))["rows"][0]
        assert any(c.get("kind") == "call" and "inspected source" in c.get("answer", "") for c in retained["calls"]), retained
        assert retained["acceptance"] is False
    observed = json.loads(worker_log.read_text().splitlines()[-1])
    assert expected in observed["argv"], observed
    if route["route"]["provider"] == "claude":
        assert observed["argv"][observed["argv"].index("--effort") + 1] == route["route"]["effort"], observed
    else:
        assert any("model_reasoning_effort" in arg and route["route"]["effort"] in arg
                   for arg in observed["argv"]), observed
    assert observed["parent"] == main_attempts[owner], observed
    if role == "worker":
        assert observed["cwd"] != str(project), observed
    else:
        assert observed["cwd"] == str(project), observed

# Registered transports join the same task tree without a native launcher or
# changes to preset allocation. Muse is a synthetic custom-adapter fixture.
external_base = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                 "--owner", "codex", "--purpose", "investigate", "--prompt", "Inspect value.txt without edits."]
for target, expected in (("grok", "grok"), ("agy", "antigravity"), ("muse", "muse")):
    request = external_base + ["--to", target, "--model", target + "-fixture",
                               "--task-id", "external-" + target, "--label", "Inspect with " + target]
    planned = json.loads(call(request + ["--dry-run"]))
    assert planned["route"]["provider"] == expected and planned["route"]["effort"] is None
    assert "--reasoning-effort" not in planned["argv"] and planned["executes"] is False
    before = (project / "value.txt").read_bytes()
    env = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])
    answer = call(request, env)
    artifact = re.search(r"^artifact: (.+)$", answer, re.M)
    assert artifact and "inspected the bounded repository task" in Path(artifact.group(1)).read_text(), (target, answer)
    assert (project / "value.txt").read_bytes() == before
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(row for row in attempts if row.get("task_id") == "external-" + target)
    assert attempt["provider"] == expected and attempt["parent_attempt_id"] == main_attempts["codex"]
    assert attempt["refs"]["panel_model"] == target + "-fixture" and attempt["state"] == "done"
    execution = json.loads(worker_log.read_text().splitlines()[-1])
    assert execution["cwd"] != str(project) and execution["parent"] == main_attempts["codex"]
    if target == "muse":
        assert execution["argv"][0] == "run" and "--access" in execution["argv"]
        assert execution["argv"][execution["argv"].index("--model") + 1] == "muse-fixture"

for options in (["--to", "muse"], ["--to", "muse", "--model", "provider-default"],
                ["--to", "muse,grok", "--model", "fixture"], ["--to", "unregistered", "--model", "fixture"],
                ["--to", "deepseek", "--model", "fixture"], ["--to", "codex", "--model", "gpt-5.6-sol"],
                ["--to", "grok", "--model", "fixture", "--seat", "astra"],
                ["--model", "fixture"], ["--reasoning-effort", "extreme"],
                ["--to", "muse", "--model", "fixture", "--access", "write", "--purpose", "implement",
                 "--brief-file", str(brief), "--verify", "test -f value.txt"]):
    before = worker_log.read_bytes()
    failure = subprocess.run(external_base + options + ["--dry-run"], cwd=project,
                             env=environment, capture_output=True, text=True)
    assert failure.returncode != 0, (options, failure.stdout)
    if "--access" in options:
        assert "OMS_PROVIDER_WRITE_ADAPTERS" in failure.stderr, failure.stderr
    assert worker_log.read_bytes() == before
# A role route keeps its tier's model and takes a per-dispatch effort.
harder = json.loads(call(external_base + ["--workload", "light", "--reasoning-effort", "high", "--dry-run"]))
assert harder["route"]["model"] == "gpt-6-luna" and harder["route"]["effort"] == "high", harder
assert harder["argv"][harder["argv"].index("--reasoning-effort") + 1] == "high", harder["argv"]
profile_route = json.loads(call(external_base + ["--to", "vibe", "--dry-run"]))
assert profile_route["route"]["model"] == "provider-default" and "--model" not in profile_route["argv"]
answer = call(external_base + ["--to", "vibe", "--task-id", "external-profile", "--label", "Inspect native profile"],
              dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
assert "artifact:" in answer
for role, target in (("advisor", "grok"), ("reviewer", "muse")):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", role,
               "--owner", "codex", "--to", target, "--model", target + "-fixture", "--prompt", "Inspect source.",
               "--task-id", "external-" + role, "--label", "Inspect with " + role]
    if role == "reviewer":
        request += ["--verify", "test -f value.txt"]
    planned = json.loads(call(request + ["--dry-run"]))
    assert planned["argv"][2] == ("advise" if role == "advisor" else "peer-review")
    assert "--reasoning-effort" not in planned["argv"]
    call(request, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
opted_in = dict(environment, OMS_PROVIDER_WRITE_ADAPTERS="muse")
write_request = [a for a in external_base[:external_base.index("--purpose")]] + [
    "--to", "muse", "--model", "muse-fixture", "--access", "write", "--purpose", "implement",
    "--brief-file", str(brief), "--verify", 'test "$(tail -1 value.txt)" = new', "--task-id", "external-muse-write",
    "--label", "Implement with adapter"]
assert json.loads(call(write_request + ["--dry-run"], opted_in))["route"]["access"] == "write"
write_env = dict(opted_in, PANEL_TEST_MODE="write", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])
assert "patch:" in call(write_request, write_env)
assert (project / "value.txt").read_text() == "old\n"
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show"), \
        patch.object(panel, "provider_catalog", return_value=list(registered.values())), \
        patch.object(panel, "dispatch", return_value=0) as routed, \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "read_field", side_effect=["p", "muse", "5", "muse-fixture", "", "Inspect source", "", "q"]):
    assert panel.interactive(project) == 0
    assert routed.call_args.kwargs == {"target": "muse", "model": "muse-fixture", "effort": None, "main": None}
snapshot = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--json"]))["dashboard"]
panel_rows = [row for row in snapshot["attempts"]["recent"] if row.get("panel")]
assert {row["panel"]["role"] for row in panel_rows} >= {"worker", "advisor", "reviewer"}, panel_rows
assert all(row.get("parent_attempt_id") in main_attempts.values() for row in panel_rows
           if row["panel"]["role"] != "main")
assert all(row["panel"]["label"] and row["panel"]["access"] for row in panel_rows
           if row["panel"]["role"] != "main")
# An exact native main filters both live children and recent outcomes. Its
# supplied title must never be replaced by an unrelated repository goal.
from copy import deepcopy
from panel_view import render
scoped = deepcopy(snapshot)
scoped["goal"] = {"text": "Unrelated repository goal"}
main_id = main_attempts["claude"]
for row in scoped["attempts"]["active_recent"] + scoped["attempts"]["recent"]:
    if row["attempt_id"] == main_id:
        row["panel"].update(model="claude-opus-5-5", label="Refine panel routing")
own_text = render(scoped, "claude", width=150, height=45, main_attempt=main_id)
assert "Refine panel routing" in own_text and "Unrelated repository goal" not in own_text, own_text
assert "selected: claude-fable-5-1" not in own_text and "selected: claude-sonnet-5-5" not in own_text, own_text
for title in ("".join(("api", "_key", "=", "fixture-value")), "/private/absolute", "../escaping"):
    try:
        panel.safe_label(title)
        raise AssertionError("unsafe task label accepted")
    except ValueError:
        pass
assert panel.safe_label("설명 \033[31m source") == "설명 ?[31m source"

# The council reuses the peer front door: four exact seats, explicit rounds,
# bounded repairs, and the owning main's child links rather than a new task DB.
council_args = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--council",
                "--owner", "codex", "--task-id", "panel-council-test", "--label", "Decide readable results",
                "--prompt", "Inspect value.txt and compare the result presentation."]
before_index = (project / ".oms/artifacts/index.jsonl").read_bytes()
for rounds in (1, 2):
    planned = json.loads(call(council_args + ["--rounds", str(rounds), "--dry-run"]))
    assert planned["primary_calls"] == 4 * (rounds + 1) and planned["max_calls"] == 8 * (rounds + 1)
    assert planned["families"] == 2 and planned["executes"] is False
    assert planned["argv"][planned["argv"].index("--providers") + 1] == \
        "codex:model=gpt-6.1-sol,claude:model=claude-opus-5-5,codex:model=gpt-6-astra,claude:model=claude-fable-5-1"
assert (project / ".oms/artifacts/index.jsonl").read_bytes() == before_index
call(council_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
council_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
council_attempts = [a for a in council_attempts if a.get("task_id") == "panel-council-test"]
assert any(a["tool"] == "panel-council" and a["state"] == "done" for a in council_attempts)
assert {a["refs"].get("panel_model") for a in council_attempts if a["tool"] != "panel-council"} == \
    {"gpt-6.1-sol", "claude-opus-5-5", "gpt-6-astra", "claude-fable-5-1"}, council_attempts
assert all(a.get("parent_attempt_id") == main_attempts["codex"] for a in council_attempts)

import panel_results as saved
from panel_view import render_results
summary = temporary / "panel-result-summary.txt"
summary.write_text("결과 화면을 읽기 쉽게 정리했습니다.\n검증 근거는 value.txt에서 확인합니다.\n", encoding="utf-8")
final_args = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--finalize", "--owner", "codex",
              "--task-id", "panel-council-test", "--summary-file", str(summary), "--outcome", "accepted",
              "--verify", "test -f value.txt", "--evidence", "value.txt"]
first = json.loads(call(final_args))
second = json.loads(call(final_args))
assert first["revision"] == second["revision"] and first["verification"]["status"] == "passed"
reopened = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                            "--task-id", "panel-council-test", "--json"]))
row = reopened["rows"][0]
assert row["acceptance"] and row["summary"].startswith("결과 화면") and row["delivery"] == "not_requested"
assert any(c.get("answer", "").startswith("Answer:") for c in row["calls"]), row
assert all("--prompt" not in c.get("answer", "") for c in row["calls"])
for width in (24, 42, 100):
    readable = render_results(reopened, width)
    assert "\033" not in readable and all(display_width(line) <= width for line in readable.splitlines())
    assert "Outcome:" in readable and "결과 화면" in readable
failure = subprocess.run(final_args[:-4] + ["--verify", "false", "--evidence", "value.txt"],
                         cwd=project, env=environment, capture_output=True, text=True)
assert failure.returncode != 0 and "verifier failed" in failure.stderr
write_finalize = list(final_args)
write_finalize[write_finalize.index("--task-id") + 1] = "panel-route-3"
failure = subprocess.run(write_finalize, cwd=project, env=environment, capture_output=True, text=True)
assert failure.returncode != 0 and "matching landed patch" in failure.stderr
whole_finalize = list(final_args)
whole_finalize[whole_finalize.index("--task-id") + 1] = "whole-main-result"
whole_finalize[whole_finalize.index("--owner") + 1] = "claude"
owner_env = dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"])
failure = subprocess.run(whole_finalize, cwd=project, env=owner_env, capture_output=True, text=True)
assert failure.returncode != 0 and "matching landed patch" in failure.stderr, failure
call(["bash", str(panel.ENTRY), "patch-land", "--repo", str(project), "--patch", write_patch,
      "--verify", "test \"$(tail -1 value.txt)\" = new"])
write_finalize[write_finalize.index("--verify") + 1] = "test \"$(tail -1 value.txt)\" = new"
whole_finalize[whole_finalize.index("--verify") + 1] = "test \"$(tail -1 value.txt)\" = new"
assert json.loads(call(write_finalize))["outcome"] == "accepted"
whole = json.loads(call(whole_finalize, owner_env))
assert whole["main_attempt_id"] == main_attempts["claude"] and whole["admission_event_ids"]
joined = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                         "--task-id", "whole-main-result", "--json"]))["rows"][0]
assert any(c.get("kind") == "delegate" for c in joined["calls"]), joined
# A task with its own attempts never adopts its main's other children.
call(final_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
own = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                       "--task-id", "panel-council-test", "--json"]))["rows"][0]
assert own["calls"] and all(c.get("kind") != "delegate" for c in own["calls"]), own
subprocess.run(["git", "-C", str(project), "restore", "value.txt"], check=True)
patch_file = project / ".oms/artifacts/test-result.patch"
patch_file.write_text("fixture patch bytes\n")
digest = saved._relative(project, ".oms/artifacts/test-result.patch")["sha256"]
delegated = {"kind": "delegate", "task_id": "test-land", "attempt_id": "att_worker",
             "patch": ".oms/artifacts/test-result.patch", "patch_sha256": digest, "plan_id": "fixture-plan", "exit": 0}
landed = {"kind": "patch-land", "task_id": "test-land", "patch_sha256": digest,
          "plan_id": "fixture-plan", "exit": 0}
assert saved._admission(project, [delegated, landed], "test-land", {"attempt_id": "att_worker"}) == landed
assert saved._admission(project, [delegated, dict(landed, plan_id="other")], "test-land", {"attempt_id": "att_worker"}) is None
patch_file.write_text("changed patch bytes\n")
assert saved._admission(project, [delegated, landed], "test-land", {"attempt_id": "att_worker"}) is None

# Test retry fencing and durable reopen using the transport boundary alone.
# The production artifact/lifecycle writers still run; no app socket is used.
with patch.dict(os.environ, environment, clear=True):
    unlanded = {"attempt_id": "att_hidden_write", "task_id": "hidden-write", "state": "review",
                "refs": {"panel_role": "worker", "panel_access": "write"}}
    recent = [{"attempt_id": "att_other_%s" % n, "task_id": "other-%s" % n} for n in range(301)]
    def windowed_query(repo, verb, *args):
        if verb == "artifact-index":
            return {"rows": []}
        values = [unlanded] + recent
        return values[-int(args[args.index("--limit") + 1]):] if "--limit" in args else values
    with patch.object(saved, "_query", side_effect=windowed_query):
        try:
            saved.finalize(project, "codex", "hidden-write", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("old unlanded write worker escaped acceptance")
        except ValueError as error:
            assert "matching landed patch" in str(error)
    history = ([{"task_id": "history-%s" % n, "ts": "2026-01-%02d" % (n + 1)} for n in range(21)] +
               [{"task_id": "history-0", "ts": "2026-02-01"}])
    with patch.object(saved, "_records", return_value=([], history)):
        assert saved.results(project)["rows"][0]["task_id"] == "history-0"
    mine = [{"attempt_id": "att_old", "task_id": "old-worker", "updated_at": "2026-01-01",
             "refs": {"panel_room_participant": "chat-old"}}]
    noise = [{"attempt_id": "att_new_%s" % n, "task_id": "new-%02d" % n, "updated_at": "2026-02-%02d" % (n + 1),
              "refs": {"panel_room_participant": "chat-new"}} for n in range(25)]
    clash = [{"attempt_id": "att_clash", "task_id": "old-worker", "updated_at": "2026-03-01",
              "refs": {"panel_room_participant": "chat-new"}}]
    old_call = {"kind": "call", "task_id": "old-worker", "attempt_id": "att_old", "exit": 0}
    clash_call = dict(old_call, attempt_id="att_clash", exit=1)
    with patch.object(saved, "_records", return_value=(mine + noise, [old_call])):
        old = saved.results(project, room_participant="chat-old")
        assert [r["task_id"] for r in old["rows"]] == ["old-worker"] and not old["truncated"], old
        assert [call["attempt_id"] for call in old["rows"][0]["calls"]] == ["att_old"]
        recent = saved.results(project, room_participant="chat-new")
        assert [r["task_id"] for r in recent["rows"]] == ["new-%02d" % n for n in range(24, 4, -1)] and recent["truncated"]
        assert saved.results(project, "missing-task")["truncated"]
    with patch.object(saved, "_records", return_value=(mine + noise + clash, [old_call, clash_call])):
        selected = saved.results(project, room_participant="chat-old")
        assert [r["task_id"] for r in selected["rows"]] == ["old-worker"]
        assert [call["attempt_id"] for call in selected["rows"][0]["calls"]] == ["att_old"]
        assert not saved.results(project, "old-worker", "chat-other")["rows"][0]["calls"]
    twenty = [dict(a, parent_attempt_id="mine-main") for a in noise[:20]]
    foreign_owner = {"kind": "panel-result", "task_id": "foreign-owner", "ts": "2025-01-01"}
    with patch.object(saved, "_records", return_value=(twenty, [foreign_owner])), \
            patch.object(saved, "_load_result", return_value={"main_attempt_id": "foreign-main", "revision": "unused"}):
        twenty_results = saved.results(project, room_participant="chat-new")
        assert len(twenty_results["rows"]) == 20 and not twenty_results["truncated"], twenty_results
    uncertain = {"status": "timeout", "persisted": False, "delivery_unknown": True}
    with patch.object(saved.codex_app_notify, "deliver", return_value=uncertain) as transport:
        delivered = saved.finalize(project, "codex", "result-retry", str(summary), "completed", notify=True)
        saved.finalize(project, "codex", "result-retry", str(summary), "completed", notify=True)
        assert transport.call_count == 1
    summary.unlink()
    with patch.object(saved.codex_app_notify, "deliver", return_value={"status": "persisted", "persisted": True}) as transport:
        assert saved.retry_delivery(project, "result-retry")["receipt"]["persisted"]
        assert saved.retry_delivery(project, "result-retry")["receipt"]["persisted"]
        assert transport.call_count == 1
    summary.write_text("A completed result.\n")
    with patch.object(saved.codex_app_notify, "deliver", side_effect=RuntimeError("lost after submission")):
        try:
            saved.finalize(project, "codex", "result-crash", str(summary), "completed", notify=True)
            raise AssertionError("crash fixture did not fire")
        except RuntimeError:
            pass
    with patch.object(saved.codex_app_notify, "deliver") as transport:
        crash = saved.finalize(project, "codex", "result-crash", str(summary), "completed", notify=True)
        assert crash["delivery"]["receipt"]["status"] == "pending" and not transport.called
    for unsafe in ("/private/file", "../value.txt"):
        try:
            saved.finalize(project, "codex", "bad-result", str(summary), "completed", evidence=[unsafe])
            raise AssertionError("unsafe evidence accepted")
        except ValueError:
            pass
    for text in ("/private/absolute", "".join(("api", "_key", "=", "fixture-value"))):
        summary.write_text(text)
        try:
            saved.finalize(project, "codex", "bad-result", str(summary), "completed")
            raise AssertionError("unsafe summary accepted")
        except ValueError:
            pass
    summary.write_text("Safe result.\n")
    link = temporary / "panel-result-linked.txt"
    try:
        link.symlink_to(summary)
        try:
            saved.finalize(project, "codex", "bad-result", str(link), "completed")
            raise AssertionError("linked summary accepted")
        except ValueError:
            pass
    except (OSError, NotImplementedError):
        if os.name != "nt":
            raise
    linked_result = project / saved.results(project, "panel-council-test")["rows"][0]["artifact"]
    linked_result.write_text("changed\n")
    assert saved.results(project, "panel-council-test")["rows"][0]["missing_result"]

for owner, attempt in main_attempts.items():
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition",
          "--attempt", attempt, "--state", "cancelled"])
closed_parent = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                               "--dispatch", "worker", "--owner", "codex", "--prompt", "Inspect source"],
                              env=dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]),
                              capture_output=True, text=True)
assert closed_parent.returncode != 0 and "identity" in closed_parent.stderr

# Task bodies keep native admission while only their shared display title is withheld.
for task in ("/review", "../scripts inspect", "Explain " + "/" + "home" + "/fixture/private.py", "x" * 160):
    with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show"), \
            patch.object(panel, "read_field", side_effect=["t", task, "q"]), \
            patch.object(panel, "ensure_room", return_value="fixture-room") as chosen_room, \
            patch.object(panel, "open_native", return_value=0) as task_open, \
            patch.dict(os.environ, environment, clear=True):
        assert panel.interactive(project) == 0
        assert task_open.call_args.kwargs["task"] == task
        expected_title = "x" * 160 if task == "x" * 160 else "Shared work"
        assert chosen_room.call_args.kwargs["title"] == expected_title

# More actions opens ordinary scrollable help, even when the viewport is short.
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show"), \
        patch.object(panel, "read_field", side_effect=["?", "", "q"]), \
        patch.object(panel, "open_native") as help_launch, patch.object(panel, "dispatch") as help_dispatch, \
        patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((32, 12))), \
        patch.object(panel, "terminal_style", return_value=(False, False, False)), \
        patch.dict(os.environ, environment, clear=True), patch("sys.stdout", new=io.StringIO()) as help_text:
    assert panel.interactive(project) == 0
    assert "all shortcuts" in help_text.getvalue() and "Advisor" in help_text.getvalue()
    assert "Finalize" in help_text.getvalue() and "Resume" in help_text.getvalue()
    help_launch.assert_not_called()
    help_dispatch.assert_not_called()

# A lifecycle writer failure cannot kill a running native main or mask its exit.
from unittest.mock import Mock
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show"), \
        patch.object(panel, "read_field", return_value="1"), patch.object(panel, "ensure_room", return_value="fixture-room"), \
        patch.object(panel, "open_native", side_effect=panel.NativeShutdown(15)), patch.dict(os.environ, environment, clear=True):
    try:
        panel.interactive(project)
    except panel.NativeShutdown as error:
        assert error.exit_code == 143
    else:
        raise AssertionError("interactive panel swallowed native termination")
uncertain_child = Mock()
uncertain_child.wait.side_effect = KeyboardInterrupt
uncertain_child.poll.return_value = None
uncertain_child.terminate.side_effect = OSError("fixture shutdown unavailable")
with patch.object(panel, "events", return_value="att_fixture") as event_log, \
        patch.object(panel, "safe_label", return_value="Inspect source"), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "ensure_room", return_value="fixture-room"), patch.object(panel.room, "join"), \
        patch.object(panel.subprocess, "Popen", return_value=uncertain_child), patch.dict(os.environ, environment, clear=True):
    try:
        panel.run_native("codex", project)
    except KeyboardInterrupt:
        pass
    assert any("native_shutdown_unconfirmed" in row.args and "blocked" in row.args for row in event_log.call_args_list)
    assert not any("cancelled" in row.args or "done" in row.args for row in event_log.call_args_list)
for action in ("native", "council"):
    process = Mock()
    process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 60), 0]
    process.poll.return_value = 0
    def broken_events(repo, operation, *args, **kwargs):
        if operation == "start":
            return "att_fixture"
        raise ValueError("fixture ledger is unavailable")
    with patch.object(panel, "events", side_effect=broken_events), \
            patch.object(panel, "safe_label", return_value="Inspect source"), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "ensure_room", return_value="fixture-room"), \
            patch.object(panel.room, "join"), \
            patch.object(panel.subprocess, "Popen", return_value=process), \
            patch.dict(os.environ, environment, clear=True):
        status = panel.run_native("codex", project) if action == "native" else \
            panel.council(project, "codex", "heartbeat-council", "Inspect source", "Compare options")
        assert status == 0 and not process.terminate.called and not process.kill.called

for owner in panel.PROVIDERS:
    plan = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                           "--launch", owner, "--resume", "exact-session-id", "--dry-run"]))
    assert plan["executes"] is False and "exact-session-id" in plan["argv"]
    assert "--last" not in plan["argv"] and "--continue" not in plan["argv"]
    assert "--dangerously" not in " ".join(plan["argv"])
# With tmux installed, the fixed panel is the default home for bare and --launch runs, also inside
# another tmux session; inline stays an explicit choice.
if shutil.which("tmux") and os.name != "nt":
    for extra, env_extra, layout in (([], {}, "split"), (["--launch", "codex"], {}, "split"),
                                     ([], {"TMUX": "/tmp/other,1,0"}, "split"),
                                     (["--host", "inline", "--launch", "codex"], {}, "inline"),
                                     (["--layout", "inline"], {}, "inline")):
        plan = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project)] + extra + ["--dry-run"],
                               dict(environment, **env_extra)))
        assert plan["layout"] == layout, (extra, env_extra, plan["layout"])

for arguments in (["--resume", "--last"], ["--launch", "codex", "--resume", "../bad"],
                  ["--launch", "codex", "--resume=--last", "--dry-run"],
                  ["--launch", "codex", "--json"], ["--model", "x"],
                  ["--watch", "--launch", "codex"], ["--watch", "--json"],
                  ["--owner", "claude"], ["--count", "1"], ["--watch", "--count", "0"],
                  ["--no-animation"],
                  ["--json", "--view", "compact"], ["--results", "--attention-only"],
                  ["--launch", "codex", "--view", "detail"],
                  ["--finalize", "--dry-run"], ["--retry-delivery"], ["--results", "--notify"],
                  ["--council", "--rounds", "3"],
                  ["--dispatch", "advisor", "--access", "write", "--prompt", "change source"],
                  ["--dispatch", "worker", "--access", "write", "--purpose", "implement", "--prompt", "change source"],
                  ["--dispatch", "worker", "--seat", "fable", "--prompt", "explain"],
                  ["--dispatch", "advisor", "--purpose", "explain", "--prompt", "explain"],
                  ["--json", "--dry-run"], []):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel"] + arguments,
                            cwd=str(project), env=environment, capture_output=True, text=True)
    assert result.returncode == 2, (arguments, result)
for bad_reopen in (["--reopen", "--json"], ["--reopen", "--dry-run"], ["--reopen", "--host", "inline"],
                   ["--reopen", "--launch", "codex"]):
    rejected = subprocess.run(["bash", str(panel.ENTRY), "panel", *bad_reopen], cwd=str(project),
                              env=environment, capture_output=True, text=True)
    assert rejected.returncode == 2, rejected
for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--launch", "codex"],
                            cwd=str(project), env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--reopen"], cwd=str(project),
                            env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--dispatch", "worker", "--prompt", "explain"],
                            cwd=str(project), env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
snapshot = call(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "1", "--owner", "claude"],
                dict(environment, OMS_HARNESS_CHILD="1", NO_COLOR=""))
assert "MAIN / Claude Code" in snapshot and "\033" not in snapshot, snapshot
filtered = call(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "1", "--view", "compact", "--attention-only"],
                dict(environment, OMS_HARNESS_CHILD="1", NO_COLOR=""))
assert "compact" in filtered and "\033" not in filtered, filtered

if os.name == "posix":
    import fcntl
    import pty
    import select
    import struct
    import termios
    import time

    def terminal(command, inputs, expected, trigger=None, signal_after=None, expected_exit=0, timeout=20):
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe(command[0], command, dict(environment, TERM="xterm", OMS_AGENT="codex"))
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        initial_tty = termios.tcgetattr(fd) if signal_after is not None else None
        pending = inputs.splitlines(keepends=True)
        prompts = 0
        output = b""
        deadline = time.monotonic() + timeout
        finished = False
        try:
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .2)[0]:
                    try:
                        output += os.read(fd, 65536)
                    except OSError:
                        pass
                seen = output.count(b"oms> ")
                if pending and trigger is not None and trigger in output and not prompts:
                    os.write(fd, b"".join(pending))
                    pending = []
                    prompts = 1
                elif pending and seen > prompts:
                    os.write(fd, pending.pop(0))
                    prompts = seen
                if signal_after is not None and trigger in output and not prompts:
                    os.kill(pid, signal_after)
                    prompts = 1
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == expected_exit, output
                    break
            if not finished:
                waited, status = os.waitpid(pid, os.WNOHANG)
                finished = bool(waited)
                assert finished, output
                assert os.waitstatus_to_exitcode(status) == expected_exit, output
            for unused in range(4):
                if not select.select([fd], [], [], 0)[0]:
                    break
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
            assert expected in output, output
            if initial_tty is not None:
                assert termios.tcgetattr(fd) == initial_tty, "watch shutdown did not restore the tty"
        finally:
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)

    # Raw single keys: no Enter, and no "oms> " prompt to wait for.
    terminal(["bash", str(panel.ENTRY), "panel", "--layout", "inline"], b"9q", b"OMS control panel", trigger=b"Needs you")
    motion_attempt = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                           "--provider", "claude", "--tool", "panel-motion-fixture",
                           "--ref", "panel_role=worker", "--ref", "panel_model=claude-sonnet-5-5",
                           "--ref", "panel_label=Explain the active fixture task", "--then", "starting",
                           "--then", "working"]).strip()
    ledger = project / ".oms/lifecycle/events.jsonl"
    before_motion = ledger.read_bytes()
    # Two collections sit five seconds apart and each can take seconds under a parallel gate.
    terminal(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "2"], b"", b"\033[2K", timeout=60)
    assert ledger.read_bytes() == before_motion, "animation wrote lifecycle state"
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition",
          "--attempt", motion_attempt, "--state", "cancelled"])
    for owner in panel.PROVIDERS:
        terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", owner, "--resume", "exact-session-id",
                  "--task", "Explain fixture parser"],
                 b"", b"NATIVE_FIXTURE_READY")
    length_room = room.create(project, "native-label-length", "Native title length")
    for length in (120, 121, 160):
        terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", "codex", "--room", length_room,
                  "--resume", "exact-session-id", "--task", "x" * length], b"", b"NATIVE_FIXTURE_READY")
    assert all(len(p["label"]) <= 120 for p in room.status(project, length_room)["participants"])
    private_task = "Explain " + "/" + "home" + "/fixture-private/input.py"
    terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", "codex", "--resume", "exact-session-id",
              "--task", private_task], b"", b"NATIVE_FIXTURE_READY")
    launched = [json.loads(line) for line in log.read_text().splitlines()]
    assert {r["agent"] for r in launched} == set(panel.PROVIDERS)
    for length in (120, 121, 160):
        assert any(("x" * length) in " ".join(row["argv"]) for row in launched)
    for row in launched:
        assert row["cwd"] == row["repo"] == str(project.resolve())
        assert row["entry"] == str(panel.ENTRY)
        assert "exact-session-id" in row["argv"]
        assert "peer-delegate" in " ".join(row["argv"])
    native_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    assert all(row["state"] == "done" for row in native_attempts
               if row["tool"] == "panel-main" and row["attempt_id"] not in main_attempts.values()), native_attempts
    titles = [row["refs"]["panel_label"] for row in native_attempts
              if row["tool"] == "panel-main" and row["attempt_id"] not in main_attempts.values()]
    assert titles.count("Explain fixture parser") == 2 and "Native task not recorded" in titles, titles
    assert private_task not in json.dumps(native_attempts), "private native task leaked into lifecycle metadata"
    assert private_task in " ".join(launched[-1]["argv"]), "native task was lost instead of hiding its title"
    actual_tmux = shutil.which("tmux")
    if actual_tmux:
        socket = "oms-panel-fixture-" + str(os.getpid())
        wrapper = binary / "tmux"
        wrapper.write_text("#!/usr/bin/env bash\nexec %s -L %s \"$@\"\n" %
                           (shlex.quote(actual_tmux), shlex.quote(socket)))
        wrapper.chmod(0o755)
        tmux = [actual_tmux, "-L", socket]
        subprocess.run(tmux + ["new-session", "-d", "-s", "fixture-existing", "sleep 60"],
                       env=dict(environment, TERM="xterm"), check=True)
        # Mains are opened one by one from the menu here; automatic opening is checked separately.
        env = dict(environment, TERM="xterm", OMS_PANEL_NO_ANIMATION="1", NO_COLOR="", OMS_PANEL_POSITION="side",
                   OMS_PANEL_MAINS="")
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
            session = next(s for s in sessions.splitlines() if s.startswith("oms-panel-"))
            # F6/F7 switch windows only in OMS panel sessions; other sessions receive the key itself.
            tmux_wait(["list-keys", "-T", "root", "F7"], "#{m:oms-panel-*,#{session_name}}")
            tmux_wait(["list-keys", "-T", "root", "F9"], "select-pane")
            tmux_wait(["list-keys", "-T", "root", "F9"], "#{m:oms-panel-*,#{session_name}}")
            tmux_wait(["list-keys", "-T", "root", "F5"], "select-window -t :=control")
            tmux_wait(["list-keys", "-T", "root", "F12"], "OMS panel keys")
            subprocess.run(tmux + ["set-environment", "-t", session, "PANEL_TEST_NATIVE_HOLD", "1"], env=env, check=True)
            panes = tmux_wait(["list-panes", "-t", session, "-F", "#{pane_id}"], "%").splitlines()
            control = panes[0]
            tmux_wait(["capture-pane", "-p", "-t", control], "Press 1 (Codex) or 2 (Claude)")
            subprocess.run(tmux + ["send-keys", "-t", control, "v"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_view"], "compact")
            subprocess.run(tmux + ["send-keys", "-t", control, "b"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_attention"], "1")
            for owner, key in (("codex", "1"), ("claude", "2")):
                subprocess.run(tmux + ["send-keys", "-t", control, key], env=env, check=True)
                tmux_wait(["list-windows", "-t", session, "-F", "#{window_name}"], owner)
                listing = tmux_wait(["list-panes", "-t", session + ":" + owner,
                                     "-F", "#{pane_id}"], "\n%").splitlines()
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], "NATIVE_FIXTURE_READY")
                sidebar = tmux_wait(["capture-pane", "-p", "-t", listing[1]],
                                    "MAIN / " + ("Codex" if owner == "codex" else "Claude Code"))
                assert "MAIN / " + ("Codex" if owner == "codex" else "Claude Code") in sidebar, sidebar
                assert "OMS activity" in sidebar, sidebar
                assert "OMS activity / attention" in sidebar, sidebar
                assert subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[1],
                    "@oms_panel_view"], env=env, text=True).strip() == "compact"
                pane_cmd = subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[1],
                                                           "#{pane_start_command}"], env=env, text=True)
                assert "OMS_PANEL_NO_ANIMATION=1" in pane_cmd and "NO_COLOR=" in pane_cmd, pane_cmd
                captured = subprocess.check_output(tmux + ["capture-pane", "-e", "-p", "-t", listing[1]],
                                                   env=env, text=True)
                assert "\033" not in captured, captured
                # Select tree in the watcher and click its own current main.
                # The existing native pane is focused, with no second launch.
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "t"], env=env, check=True)
                tree_sidebar = tmux_wait(["capture-pane", "-p", "-t", listing[1]], "this window · ")
                clicked_row = next(n + 1 for n, line in enumerate(tree_sidebar.splitlines()) if "this window · " in line)
                subprocess.run(tmux + ["select-pane", "-t", listing[1]], env=env, check=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "\033[<0;8;%sM" % clicked_row], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", session + ":" + owner, "#{pane_id}"], listing[0])
                native_pid = subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0],
                    "#{pane_pid}"], env=env, text=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "v"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "1")
                enlarged = tmux_wait(["capture-pane", "-p", "-t", listing[1]], "MAIN / ")
                main_row, main_text = next((n + 1, line) for n, line in enumerate(enlarged.splitlines()) if "MAIN / " in line)
                # Cards sit in window order, the F6/F7 order: codex opened first, so claude's card is second.
                main_col = main_text.index("MAIN / ", main_text.index("MAIN / ") + 1 if owner == "claude" else 0) + 2
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "\033[<0;%s;%sM" % (main_col, main_row)], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "0")
                tmux_wait(["display-message", "-p", "-t", session + ":" + owner, "#{pane_id}"], listing[0])
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "v"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "1")
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "Escape"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "0")
                tmux_wait(["show-option", "-w", "-v", "-t", listing[1], "@oms_panel_view"], "summary")
                assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0],
                    "#{pane_pid}"], env=env, text=True) == native_pid
                assert subprocess.check_output(tmux + ["show-option", "-g", "-v", "mouse"], env=env, text=True).strip() == "off"
                assert subprocess.check_output(tmux + ["show-option", "-t", session, "-v", "mouse"], env=env, text=True).strip() == "on"
                subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
            control_sidebar = tmux_wait(["list-panes", "-t", session + ":control", "-F", "#{pane_id}"], "\n%").splitlines()[1]
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "claude")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Claude | W")
            subprocess.run(tmux + ["send-keys", "-t", control, "4"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", control], "Main provider (codex/claude)")
            subprocess.run(tmux + ["send-keys", "-t", control, "codex", "Enter"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "codex")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Codex | W")
            records = [json.loads(line) for line in log.read_text().splitlines()]
            assert {r["agent"] for r in records[-2:]} == set(panel.PROVIDERS)
            assert all(r["cwd"] == str(project.resolve()) for r in records[-2:])
            # Select the destination with a real attached client on another session.
            # Provenance rejection is covered by the catalog fixtures below.
            other = subprocess.check_output(tmux + ["new-session", "-d", "-s", "fixture-navigation",
                "-P", "-F", "#{session_id}\t#{window_id}\t#{pane_id}", "cat"], env=env, text=True).strip().split("\t")
            target = {"session": other[0], "window": other[1], "pane": other[2]}
            with patch.dict(os.environ, dict(env, TMUX_PANE=control), clear=True), \
                    patch("panel_chats.catalog", return_value={"rows": [{"participant": "fixture-destination",
                        "provider": "codex", "terminal": target, "uri": None, "status": "native"}]}):
                import panel_chats
                assert panel_chats.open_chat(project, "fixture-room", "fixture-destination")["status"] == "navigation_requested"
            assert subprocess.check_output(tmux + ["list-clients", "-F", "#{session_name}"], env=env, text=True).strip() == "fixture-navigation"
            subprocess.run(tmux + ["switch-client", "-t", session], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", control, "q"], env=env, check=True)
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
            # One command opens a native chat below a full-width graph. The
            # watcher can zoom/collapse without replacing the native process.
            # The checkout keeps one panel session; a later launch adds a native window to it.
            assert session == panel.panel_session(project), session
            assert subprocess.check_output(tmux + ["show-options", "-v", "-t", session, "@oms_panel_repo"],
                                           env=env, text=True).strip() == str(project.resolve())
            known_sessions = set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env, text=True).splitlines())
            known_windows = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                        env=env, text=True).splitlines())
            subprocess.run(tmux + ["set-environment", "-g", "PANEL_TEST_NATIVE_HOLD", "1"], env=env, check=True)
            os.close(fd)
            pid, fd = pty.fork()
            if pid == 0:
                os.chdir(project)
                os.execvpe("bash", ["bash", str(panel.ENTRY), "panel", "--host", "tmux", "--position", "top",
                                    "--launch", "codex", "--task", "Native conversation fixture"],
                           dict(env, PANEL_TEST_NATIVE_HOLD="1"))
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 110, 0, 0))
            finished = False
            deadline = time.monotonic() + 20
            top_window = None
            while time.monotonic() < deadline:
                windows = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                      env=env, text=True).splitlines()) - known_windows
                if windows:
                    top_window = next(iter(windows))
                    break
                time.sleep(.1)
            assert top_window, "native chat board did not start"
            assert set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env,
                                               text=True).splitlines()) == known_sessions, "a second panel session opened"
            # Menu-opened and externally launched mains share the panel's recorded room.
            menu_room = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", session + ":codex",
                                                        "@oms_panel_room"], env=env, text=True).strip()
            # Window options are bound right after the window appears; wait for them instead of racing.
            assert menu_room, "menu-opened main recorded no room"
            tmux_wait(["show-option", "-w", "-v", "-t", top_window, "@oms_panel_room"], menu_room)
            pane_rows = tmux_wait(["list-panes", "-t", top_window, "-F",
                                   "#{pane_id}\t#{pane_top}\t#{pane_width}\t#{pane_start_command}"], "--watch").splitlines()
            chat_row = next(row.split("\t", 3) for row in pane_rows if "--launch" in row)
            graph_row = next(row.split("\t", 3) for row in pane_rows if "--watch" in row)
            chat_pane, graph_pane = chat_row[0], graph_row[0]
            assert int(graph_row[1]) < int(chat_row[1]) and graph_row[2] == chat_row[2] == "110", pane_rows
            tmux_wait(["capture-pane", "-p", "-t", chat_pane], "NATIVE_FIXTURE_READY")
            tmux_wait(["capture-pane", "-p", "-t", graph_pane], "Sol 6.1 ─")
            native_pid = subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", top_window, "#{pane_id}"], env=env, text=True).strip() == chat_pane
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "v"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "1")
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "0")
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            # After the person unzooms by hand, Esc must not zoom the board again.
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "v"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "1")
            subprocess.run(tmux + ["resize-pane", "-Z", "-t", graph_pane], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "0")
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            time.sleep(.8)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"],
                                           env=env, text=True).strip() == "0", "Esc re-zoomed an unzoomed board"
            # F9 (the attached client's xterm F9) swaps focus between chat and board and keeps both alive;
            # q on a managed board does nothing. If a pane is explicitly killed, restore only the board
            # and keep the CLI PID.
            subprocess.run(tmux + ["select-pane", "-t", graph_pane], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "q"], env=env, check=True)
            time.sleep(.5)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", top_window, "#{pane_id}"],
                                           env=env, text=True).strip() == graph_pane, "q moved focus"
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], chat_pane)
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], graph_pane)
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], chat_pane)
            subprocess.run(tmux + ["kill-pane", "-t", graph_pane], env=env, check=True)
            reopen_env = dict(env, TMUX="fixture", TMUX_PANE=chat_pane, OMS_PANEL_SESSION=session)
            restore = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                       "--position", "top", "--reopen"], reopen_env))
            assert restore["status"] == "reopened" and restore["native_restarted"] is False, restore
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            restored = tmux_wait(["list-panes", "-t", top_window, "-F", "#{pane_id}\t#{pane_start_command}"], "--watch")
            graph_pane = next(row.split("\t", 1)[0] for row in restored.splitlines() if "--watch" in row)
            tmux_wait(["capture-pane", "-p", "-t", graph_pane], "Sol 6.1 ─")
            with patch.dict(os.environ, reopen_env, clear=True), patch.object(panel.sys.stdin, "isatty", return_value=True), \
                    patch.object(panel.sys.stdout, "isatty", return_value=True), \
                    patch.object(panel, "reopen_panel", wraps=panel.reopen_panel) as reopen_again:
                assert panel.main(["--repo", str(project)]) == 0
                assert reopen_again.called
            assert len(subprocess.check_output(tmux + ["list-panes", "-t", top_window], env=env, text=True).splitlines()) == 2
            # An agent starts another main without a terminal: a detached window in the same panel and room,
            # no new session, no focus change, recorded as started by the calling main.
            starter_id = next(p["participant"] for p in room.status(project, menu_room)["participants"]
                              if p["role"] == "main" and p["provider"] == "codex" and p["joined"])
            windows_before = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                         env=env, text=True).splitlines())
            current_before = subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                                     env=env, text=True).strip()
            spawn_env = dict(env, OMS_ROOM_ID=menu_room, OMS_ROOM_PARTICIPANT=starter_id, OMS_PANEL_SESSION="")
            spawned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude",
                                       "--task", "Spawned fixture", "--json"], spawn_env))
            assert spawned["provider"] == "claude" and spawned["room"] == menu_room and isinstance(spawned["window"], int), spawned
            new_window = next(iter(set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                               env=env, text=True).splitlines()) - windows_before))
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                           env=env, text=True).strip() == current_before, "spawning moved focus"
            assert set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env,
                                               text=True).splitlines()) == known_sessions, "spawning opened a session"
            tmux_wait(["show-option", "-w", "-v", "-t", new_window, "@oms_panel_started_by"], starter_id)
            tmux_wait(["show-option", "-w", "-v", "-t", new_window, "@oms_panel_room"], menu_room)
            tmux_wait(["list-panes", "-t", new_window, "-F", "#{pane_id}"], "\n%")
            spawned_attempts = [row for row in json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                "list", "--json"])) if row.get("refs", {}).get("panel_started_by") == starter_id]
            assert len(spawned_attempts) == 1 and spawned_attempts[0]["refs"]["panel_room_id"] == menu_room, spawned_attempts
            watched_env = dict(env, TMUX="fixture", OMS_PANEL_SESSION=session, OMS_ROOM_ID=menu_room,
                               COLUMNS="140", LINES="44", OMS_PANEL_POSITION="auto")
            board_text = ""
            for unused in range(50):
                board_text = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", menu_room,
                                   "--watch", "--count", "1", "--view", "graph"], watched_env)
                if "↳" in board_text or "^" in board_text:
                    break
                time.sleep(.2)
            # The start bar sits right above the pinned key hints.
            assert "[+ Codex main]  [+ Claude main]" in board_text.splitlines()[-2], board_text
            assert "↳" in board_text or "^" in board_text, board_text
            # A main may only request a close; the person's second x closes the proven window and the room shows it left.
            target_id = tmux_wait(["show-option", "-w", "-v", "-t", new_window, "@oms_panel_room_participant"], "att_").strip()
            request_command = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--request-close", target_id]
            requested = json.loads(call(request_command + ["--reason", "done\x1b[2J line", "--json"], spawn_env))
            assert requested == {"requested": target_id, "by": starter_id, "room": menu_room}, requested
            asked = [m for m in room.status(project, menu_room)["messages"] if m["text"].startswith("Close requested: ")]
            assert len(asked) == 1 and asked[0]["message_kind"] == "question" and asked[0]["recipient"] == target_id, asked
            assert "\x1b" not in asked[0]["text"], asked
            from panel_view import close_requests
            other_room = room.create(project, title="Other close room")
            for refusal_env, refusal_args, expected in (
                    (spawn_env, ["--request-close", "nobody-here"], "not a joined main"),
                    (dict(spawn_env, OMS_ROOM_ID=other_room), ["--request-close", target_id], "joined main"),
                    (dict(spawn_env, OMS_HARNESS_CHILD="1"), ["--request-close", target_id], "worker cannot open owner sessions")):
                refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project)] + refusal_args,
                                         cwd=str(project), env=refusal_env, capture_output=True, text=True, timeout=60,
                                         stdin=subprocess.DEVNULL)
                assert refused.returncode != 0 and expected in refused.stderr, (refusal_args, refused.stdout, refused.stderr)
            assert len([m for m in room.status(project, menu_room)["messages"] if m["text"].startswith("Close requested: ")]) == 1
            with patch.dict(os.environ, dict(env, TMUX="fixture", OMS_PANEL_SESSION=session, OMS_ROOM_ID=menu_room), clear=True):
                close_state, unused = panel.snapshot(project, menu_room)
                assert [r["participant"] for r in close_requests(close_state)] == [target_id], close_state.get("room")
                close_nav = {"selected": ("close", target_id), "room_id": menu_room}
                assert panel.close_selected(project, close_nav, close_state, True) == "close not confirmed"
                assert "press x again to confirm, Esc to cancel" in close_nav["notice"], close_nav
                assert new_window in subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                             env=env, text=True), "asking closed the window"
                assert panel.close_selected(project, close_nav, close_state, True) == "closed " + close_nav["closed_note"].split(" · ", 1)[1]
                assert close_nav["closed_note"].startswith("Closed ") and "by the person" in close_nav["closed_note"], close_nav
            assert new_window not in subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                             env=env, text=True), "the confirmed close left its window"
            assert not any(p["participant"] == target_id and p["joined"] for p in room.status(project, menu_room)["participants"])
            assert starter_id in [p["participant"] for p in room.status(project, menu_room)["participants"] if p["joined"]]
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "-l", "hello native chat"], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "Enter"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", chat_pane], "NATIVE_CHAT_REPLY:hello native chat")
            subprocess.run(tmux + ["detach-client", "-s", session], env=env, check=True)
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
            assert finished, "native chat detach hung"
        finally:
            subprocess.run(tmux + ["kill-server"], env=env, capture_output=True)
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)
# The canonical operator suite also covers the room front door and native open contract.
import room
import codex_app_notify
shared = room.create(project, "operator-room", "Shared parser task")
room.join(project, shared, main_attempts["codex"], "codex", model="gpt-6-sol", label="Sol main")
room.join(project, shared, main_attempts["claude"], "claude", model="claude-opus-5-5", label="Opus main")
room_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_PARTICIPANT=main_attempts["claude"],
                OMS_ROOM_REPO=str(project), OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"])
# Parent identities were intentionally closed by earlier negative cases; use a fresh owner here.
room_owner = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                   "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                   "--ref", "panel_model=claude-opus-5-5", "--then", "starting", "--then", "working"]).strip()
room.join(project, shared, room_owner, "claude", model="claude-opus-5-5", label="Active Opus")
room_env.update(OMS_PANEL_MAIN_ATTEMPT=room_owner, OMS_ROOM_PARTICIPANT=room_owner)
answer = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
               "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate",
               "--workload", "light", "--prompt", "Inspect value.txt without edits.",
               "--task-id", "room-inspection", "--label", "Inspect the shared parser"], room_env)
assert "artifact:" in answer, answer
state = room.status(project, shared)
child = next(p for p in state["participants"] if p["role"] == "worker")
assert child["parent"] == room_owner and child["model"] == "gpt-6-luna", child
handoff = next(m for m in state["messages"] if m["sender"] == child["participant"])
assert handoff["recipient"] == room_owner and "parent acceptance pending" in handoff["text"], handoff
assert "inspected the bounded repository task" in handoff["text"], handoff
assert (project / "value.txt").read_text() == "old\n", "room cannot widen worker write access"
# A status message is a declaration: no mail, no unread, latest per sender kept past the 12-message window;
# unread counts ignore participants who left.
declared = room.create(project, title="Declared status")
room.join(project, declared, "decl-main", "claude", model="claude-opus-5-5")
room.join(project, declared, "decl-peer", "codex", role="worker", parent="decl-main")
room.join(project, declared, "decl-other", "codex", role="worker", parent="decl-main")
room.send(project, declared, "decl-main", "all", "Parsing the config", kind="status")
room.send(project, declared, "decl-peer", "decl-main", "Call exit=3; parent acceptance pending.\nignored", kind="handoff",
          message_id="result-decl-peer")
# Another call cannot settle a sibling by sending a result under its name, and a plain note is no result.
try:
    room.send(project, declared, "decl-peer", "decl-main", "Call exit=0;", kind="handoff", message_id="result-decl-other")
except ValueError as error:
    assert "reserved" in str(error), error
else:
    raise AssertionError("a call sent a result under a sibling's name")
room.join(project, declared, "decl-elsewhere", "claude", model="claude-opus-5-5")
try:
    room.send(project, declared, "decl-other", "decl-elsewhere", "Call exit=0;", kind="handoff", message_id="result-decl-other")
except ValueError as error:
    assert "to its parent" in str(error), error
else:
    raise AssertionError("a call's result reached a main that is not its parent")
room.append(project, declared, {"kind": "leave", "participant": "decl-elsewhere"}, "Left")
room.acknowledge(project, declared, "decl-main", ["result-decl-peer"])
for number in range(13):
    room.send(project, declared, "decl-peer", "decl-main", "chatter %s" % number)
    room.acknowledge(project, declared, "decl-main", [room.status(project, declared)["messages"][-1]["id"]])
declared_state = room.status(project, declared)
assert declared_state["pending_count"] == 0 and declared_state["statuses"]["decl-main"]["text"] == "Parsing the config", declared_state
assert all(not m["targets"] for m in declared_state["messages"] if m["message_kind"] == "status") and declared_state["statuses"]["decl-main"]["seq"], declared_state
assert not any(t.get("room_event", {}).get("message_kind") == "status" for t in room.updates(project, declared, "decl-peer")["turns"])
declared_results = room.status(project, declared)["call_results"]
assert declared_results["decl-peer"]["exit"] == 3 and set(declared_results["decl-peer"]) == {"exit", "ts", "seq", "read"}, declared_results
assert "decl-other" not in declared_results, declared_results
room.append(project, declared, {"kind": "leave", "participant": "decl-other"}, "Left")
assert not any(m["id"] == "result-decl-peer" for m in room.status(project, declared)["messages"]), "result left the 12-message window"
room.send(project, declared, "decl-main", "all", "mail for the worker")
assert room.status(project, declared)["pending_count"] == 1
room.append(project, declared, {"kind": "leave", "participant": "decl-peer"}, "Left")
assert room.status(project, declared)["pending_count"] == 0
# Control-window dispatch resolves the chosen provider's active main, rather
# than the first main ever joined, and refuses zero or ambiguous ownership.
control_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project))
with patch.dict(os.environ, control_env, clear=True), patch.object(panel.subprocess, "call", return_value=0):
    assert panel.dispatch(project, "claude", "worker", "light", "auto", "read", "investigate",
                          prompt="Inspect the bounded source", task_id="control-owner", label="Control dispatch") == 0
    newest = room.status(project, shared)["participants"][-1]
    assert newest["parent"] == room_owner
    for owner in ("codex",):
        try:
            panel.dispatch(project, owner, "worker", "light", "auto", "read", "investigate", prompt="Inspect source")
        except ValueError as error:
            assert "unique active main" in str(error)
        else:
            raise AssertionError("control dispatch chose an exited owner")
    second_owner = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                                "--ref", "panel_room_id=" + shared, "--then", "starting", "--then", "working", output=True)
    room.join(project, shared, second_owner, "claude")
    try:
        panel.dispatch(project, "claude", "worker", "light", "auto", "read", "investigate", prompt="Inspect source")
    except ValueError as error:
        assert "unique active main" in str(error)
    else:
        raise AssertionError("control dispatch silently chose between two mains")
    # An explicit main resolves the ambiguity but cannot name a non-live caller.
    assert panel.dispatch(project, "claude", "advisor", "routine", "auto", "read", "advise",
                          prompt="Advise the second main", main=second_owner) == 0
    assert room.status(project, shared)["participants"][-1]["parent"] == second_owner
    try:
        panel.dispatch(project, "claude", "advisor", "routine", "auto", "read", "advise",
                       prompt="Advise nobody", main="outside-main")
    except ValueError as error:
        assert "unique active main" in str(error)
    else:
        raise AssertionError("explicit main bypassed live-main proof")
    # An inherited caller cannot be redirected to another main by --main.
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
        try:
            panel.dispatch(project, "claude", "advisor", "routine", "auto", "read", "advise",
                           prompt="Misattributed advice", main=second_owner)
        except ValueError as error:
            assert "different main" in str(error)
        else:
            raise AssertionError("--main silently kept the inherited caller")
    # Calls have their own cap: a room whose member slots are full still admits calls.
    cap_rows = room.records(project, shared)
    members_now = sum(not p.get("parent") for p in room.status(project, shared)["participants"])
    calls_now = sum(bool(p.get("parent")) for p in room.status(project, shared)["participants"])
    call_join = {"kind": "join", "participant": "call-capacity", "provider": "codex", "role": "advisor",
                 "model": "gpt-6-astra", "label": "Capacity", "owns": [], "parent": second_owner}
    with patch.object(room, "MAX_PARTICIPANTS", members_now):
        room.validate_event(cap_rows, call_join, "Joined: Capacity")
        try:
            room.validate_event(cap_rows, dict(call_join, participant="main-capacity", role="main", parent=None),
                                "Joined: Capacity")
        except ValueError as error:
            assert "participant limit" in str(error)
        else:
            raise AssertionError("room member cap was not enforced")
    with patch.object(room, "MAX_CALLS", calls_now):
        try:
            room.validate_event(cap_rows, call_join, "Joined: Capacity")
        except ValueError as error:
            assert "call limit" in str(error)
        else:
            raise AssertionError("room call cap was not enforced")
    panel.events(project, "transition", "--attempt", second_owner, "--state", "cancelled")
    room.append(project, shared, {"kind": "leave", "participant": second_owner}, "Fixture owner stopped")
recorded = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--limit", "300", "--json"]))
room_attempt = next(a for a in recorded if a.get("refs", {}).get("panel_room_participant") == child["participant"])
assert room_attempt["refs"]["panel_room_id"] == shared
observed = json.loads(worker_log.read_text().splitlines()[-1])
assert observed["room"] == shared and observed["participant"] == child["participant"], observed
# Same task reuse cannot return another participant's artifact as the result.
assert not panel.results(project, "room-inspection", "unrelated-participant")["rows"][0]["calls"]
participant_results = panel.results(project, room_participant=child["participant"])
assert any("Answer:" in c.get("answer", "") for r in participant_results["rows"] for c in r["calls"]), participant_results
room_view = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--json"], room_env))["dashboard"]
assert room_view["room"]["id"] == shared
room_view["attempts"]["active_recent"].append({"attempt_id": "graph-example-child", "state": "working",
    "task_id": "graph-sample", "panel": {"role": "worker", "room_id": shared,
    "room_participant": child["participant"], "model": "gpt-6-luna", "label": "긴 작업 설명을 분리해서 표시",
    "location": "worktree:example/wt"}})
immutable = json.dumps(room_view, sort_keys=True)
graph = render(room_view, "claude", 100, 44, view="graph", frame=0)
assert "unread" in graph and "MAIN /" in graph and "Luna" in graph, graph
assert "No joined worker" not in graph and "No joined judge" not in graph, graph
degraded = deepcopy(room_view)
degraded["collection"] = {"ok": False}
degraded["room"]["error"] = "Room evidence unavailable"
for width, height in ((100, 44), (76, 36), (35, 24), (35, 23), (35, 12)):
    for density in ("graph", "compact"):
        for menu in (False, True):
            drawn = render(degraded, "claude", width, height, view=density, menu=menu)
            assert "Collection degraded" in drawn and "Room evidence unavailable" in drawn, drawn
            assert len(drawn.splitlines()) <= height - int(menu)
            assert all(display_width(line) <= width for line in drawn.splitlines())
assert graph != render(room_view, "claude", 100, 44, view="graph", frame=1)
assert json.dumps(room_view, sort_keys=True) == immutable
fallback = render(room_view, "claude", 35, 23, view="graph")
assert "Joined mains" in fallback and "Graph needs 76x16" in fallback and "Messages:" in fallback, fallback
for source_view, dimensions in ((room_view, ((80, 11), (80, 12), (60, 14), (100, 16))),
                                (degraded, ((100, 30),))):
    for columns, rows in dimensions:
        nav = {}
        screen = render(source_view, "claude", columns, rows, view="graph", navigation=nav)
        assert any(item[0] == "chat" for item in nav["items"]), (columns, rows, nav, screen)
        assert any(item[0] == "result" for item in nav["items"]), (columns, rows, nav, screen)
        assert any(hit["action"][0] == "chat" for hit in nav["hits"]), (columns, rows, nav, screen)
        assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines())
        if source_view is degraded:
            assert "Collection degraded" in screen and "Room evidence unavailable" in screen
for columns, rows in ((100, 44), (76, 36), (60, 36), (36, 28), (2, 8)):
    for ascii_only in (False, True):
        drawn = render(room_view, "claude", columns, rows, view="graph", unicode=not ascii_only, menu=True)
        assert len(drawn.splitlines()) <= rows - 1 and all(display_width(line) <= columns for line in drawn.splitlines()), drawn
        assert "\033" not in drawn
        if ascii_only:
            assert "╭" not in drawn and "─" not in drawn
# The tree exposes every joined main, maps cells from the drawn frame, and
# never manufactures a parent or changes room data while folding/scrolling.
from panel_input import TerminalInput, choose
from panel_metrics import cached, collect, pane_reading, percent
tree_room = deepcopy(room_view)
tree_room["room"]["participants"].extend([
    {"participant": "extra-main", "provider": "codex", "model": "gpt-6-sol", "role": "main",
     "label": "Third main", "joined": True, "seq": 100},
    {"participant": "unlinked-worker", "provider": "codex", "model": "gpt-6-luna", "role": "worker",
     "label": "연결 근거 없는 작업", "joined": True, "seq": 101, "parent": "outside-room"}])
before_tree = json.dumps(tree_room, sort_keys=True)

# The visual flow selects one exact parent, uses real role cards, and pages
# other mains/calls through the same frame-local chat/result targets.
flow_members = [{"participant": "flow-main", "role": "main", "joined": True, "provider": "codex",
                 "model": "gpt-6-sol", "label": "Main native chat"},
                {"participant": "flow-other", "role": "main", "joined": True, "provider": "claude",
                 "model": "claude-opus-5-5", "label": "Other native chat"}]
for ident, role, model in (("advisor", "advisor", "gpt-6-astra"), ("reviewer", "reviewer", "claude-fable-5-1"),
                          ("researcher", "worker", "gpt-6-luna"), ("explorer", "worker", "gpt-6-luna"),
                          ("builder", "worker", "claude-sonnet-5-5")):
    flow_members.append({"participant": ident, "role": role, "model": model, "joined": True,
                         "provider": "codex", "parent": "flow-attempt", "label": "한글 작업 제목 / " + ident})
flow_members += [{"participant": "foreign-child", "role": "worker", "model": "gpt-6-luna", "joined": True,
                  "parent": "flow-other", "label": "ONLY OTHER OWNER"},
                 {"participant": "unlinked-child", "role": "worker", "joined": True,
                  "parent": "missing-owner", "label": "NO PARENT PROOF"}]
flow = {"collection": {"ok": True}, "room": {"id": "flow-room", "title": "Visual team", "participants": flow_members},
        "attempts": {"active_recent": [{"attempt_id": "flow-attempt" if m["participant"] == "flow-main" else m["participant"],
            "state": "working", "panel": {"room_id": "flow-room", "room_participant": m["participant"], "role": m["role"]}}
            for m in flow_members]}, "operations": {"recent": [{"kind": "call", "event_id": model,
                "served_model": model, "tokens": 10} for model in ("gpt-6-sol", "gpt-6-astra", "gpt-6-luna")]}}
flow_before = json.dumps(flow, sort_keys=True)
# Long headings leave the true midpoint free for a wire in either glyph set.
geometry_flow = deepcopy(flow)
for attempt in geometry_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "waiting_approval"
for columns, rows, menu in ((76, 16, False), (76, 16, True), (76, 24, False), (100, 24, False), (157, 66, False)):
    for glyphs in (False, True):
        geometry_nav = {"dismissed": True, "selected": ("result", "builder")}
        geometry_picture = render(geometry_flow, "codex", columns, rows, view="graph", unicode=glyphs,
                                  main_attempt="flow-attempt", navigation=geometry_nav, menu=menu)
        geometry_lines = geometry_picture.splitlines()
        assert len(geometry_lines) <= rows - int(menu) and all(display_width(line) <= columns for line in geometry_lines)
        assert "Sonnet 5.5" in geometry_picture and "approval" in geometry_picture, geometry_picture
        if geometry_nav.get("surface") != "graph":
            assert any(hit["action"] == ("result", "builder") for hit in geometry_nav["hits"]), geometry_nav
            continue
        for ident in ("researcher", "explorer", "builder"):
            edges = [hit for hit in geometry_nav["hits"] if hit["action"] == ("result", ident)]
            edge = min(edges, key=lambda hit: hit["y"])
            middle = edge["x1"] - 1 + (edge["x2"] - edge["x1"] + 1) // 2
            assert geometry_lines[edge["y"] - 1][middle] == ("┴" if glyphs else "+"), geometry_picture
            if ident == "builder":
                card_text = "\n".join(geometry_lines[hit["y"] - 1][edge["x1"] - 1:edge["x2"]] for hit in edges)
                assert "Sonnet 5.5" in card_text and "needs approval" in card_text, card_text
                assert "한글 작업 제목" in card_text and "builder" in card_text, card_text
            geometry_click = dict(geometry_nav, selected=None, preview={})
            assert choose(("click", middle + 1, edge["y"] + 1), geometry_click) is None
            assert geometry_click["selected"] == ("result", ident)
            assert geometry_click["preview"]["target"] == ("result", ident)
import panel_chats
fast_room = deepcopy(flow["room"])
fast_room["participants"][0].update(consumer="enrolled-main-hash", provider="codex")
fast_room["participants"][1].update(consumer="other-main-hash", provider="claude")
target = {"session": "$1", "window": "@1", "pane": "%1"}
with patch.object(panel_chats.room, "status", return_value=fast_room), \
        patch.object(panel_chats, "windows", return_value={"flow-main": [target]}), \
        patch.object(panel_chats, "native_index", side_effect=AssertionError("unneeded transcript scan")), \
        patch.object(panel_chats, "terminal_commands", return_value=[["tmux", "select-pane", "-t", "%1"]]):
    assert panel_chats.plan(Path("."), "flow-room", "flow-main")["method"] == "existing-terminal"
for columns, rows in ((110, 44), (100, 24), (76, 16), (76, 20)):
    for glyphs in (False, True):
        for menu in (False, True):
            nav = {}
            picture = render(flow, "codex", columns, rows, view="graph", unicode=glyphs, menu=menu,
                             main_attempt="flow-attempt", navigation=nav)
            assert "MAIN / Sol 6" in picture and "ADVISOR / Astra" in picture and "WORKER / Luna" in picture, picture
            assert picture.index("ADVISOR / Astra") < picture.index("MAIN / Sol 6") < picture.index("WORKER / Luna"), picture
            assert "ONLY OTHER OWNER" not in picture and "NO PARENT PROOF" not in picture, picture
            assert len(picture.splitlines()) <= rows - int(menu), picture
            assert all(display_width(line) <= columns for line in picture.splitlines()), picture
            target = next(hit for hit in nav["hits"] if hit["action"] == ("result", "builder"))
            # A call's content shows in the board's detail area; clicking again opens no other screen.
            assert choose(("click", target["x1"], target["y"]), nav) is None
            assert nav["preview"] == {"target": ("result", "builder")}
            assert choose(("click", target["x1"], target["y"]), nav) is None
            nav["surface"] = "graph"
            assert choose(("enter",), nav) is None and nav["preview"]["target"] == ("result", "builder")
            nav.pop("preview")
            choose(("end",), nav)
            other = render(flow, "claude", columns, rows, view="graph", unicode=glyphs, menu=menu,
                           main_attempt="flow-attempt", navigation=nav)
            assert "ONLY OTHER OWNER" in other and "MAIN / Opus" in other, other
            assert any(hit["action"] == nav["selected"] for hit in nav["hits"]), nav
            if not glyphs:
                assert "╭" not in picture and "─" not in picture
assert render(flow, "codex", 100, 24, view="graph", frame=0) != render(flow, "codex", 100, 24, view="graph", frame=1)
# Tabs keep same-model mains distinct; a click on another main's tab only
# re-roots the graph, while the shown main's tab opens its chat.
nav = {}
picture = render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=nav)
tabs = next(line for line in picture.splitlines() if "[+ All]" in line)
assert "Sol 6·Main native chat" in tabs and "Opus 5.5·Other" in tabs and "W3 A2" in tabs, tabs
assert nav["primary"] == ("chat", "flow-main") and nav["surface"] == "graph"
other_tab = next(h for h in nav["hits"] if h["action"] == ("chat", "flow-other") and h.get("select"))
assert choose(("click", other_tab["x1"], other_tab["y"]), nav) is None and nav["selected"] == ("chat", "flow-other")
render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=nav)
assert nav["primary"] == ("chat", "flow-other")
assert choose(("click", other_tab["x1"], other_tab["y"]), nav) == ("chat", "flow-other")
choose(("right",), nav)
assert nav["selected"] == ("chat", "flow-main")
choose(("left",), nav)
assert nav["selected"] == ("chat", "flow-other")
# Mains follow tmux window order, the order F6/F7 step through, in tabs and tree alike.
windowed = dict(flow, main_windows={"flow-other": 1, "flow-main": 2})
tabs = next(line for line in render(windowed, "codex", 110, 30, view="graph", main_attempt="flow-attempt",
                                    navigation={}).splitlines() if "All]" in line)
assert tabs.index("Opus 5.5·Other") < tabs.index("Sol 6·Main native chat"), tabs
tree = render(windowed, "codex", 110, 40, view="tree", main_attempt="flow-attempt")
# The tree keeps this window's main first, then follows window order for the other mains.
assert tree.index("Main native chat") < tree.index("Other native chat"), tree
# Every main with a known window leads with its window number; without one, a shared model gets the id tail.
twin = deepcopy(windowed)
twin["room"]["participants"].append({"participant": "twin-main-9f3c", "role": "main", "joined": True, "provider": "claude",
                                     "model": "claude-opus-5-5", "label": "Twin native chat"})
twin["attempts"]["active_recent"].append({"attempt_id": "twin-main-9f3c", "state": "working", "panel": {
    "room_id": "flow-room", "room_participant": "twin-main-9f3c", "role": "main"}})
for text in (render(twin, "codex", 157, 40, view="graph", navigation={"overview": True}),
             render(twin, "codex", 157, 40, view="tree")):
    assert "#1 Opus 5.5" in text and "#2 Sol" in text and "Opus 5.5 #9f3c" in text, text
twin_detail = render(twin, "codex", 157, 40, view="graph", navigation={"selected": ("result", "foreign-child"),
    "preview": {"target": ("result", "foreign-child"), "report": {}}, "overview": True})
assert "for #1 Opus 5.5" in twin_detail, twin_detail
third_order = deepcopy(windowed)
third_order["room"]["participants"].append({"participant": "ordered-third", "role": "main", "joined": True,
    "provider": "claude", "model": "claude-opus-5-5", "label": "Third ordered main"})
third_order["main_windows"] = {"flow-main": 3, "flow-other": 2, "ordered-third": 1}
tree = render(third_order, "codex", 110, 60, view="tree", main_attempt="flow-attempt")
assert tree.index("Main native chat") < tree.index("Third ordered main") < tree.index("Other native chat"), tree
# Wheel input scrolls only the band under the pointer.
crowded = deepcopy(flow)
crowded["room"]["participants"] += [{"participant": "extra-%s" % n, "role": "worker", "model": "gpt-6-luna",
    "joined": True, "parent": "flow-attempt", "label": "EXTRA WORKER %s" % n} for n in range(3)]
crowded["attempts"]["active_recent"] += [{"attempt_id": "extra-%s" % n, "state": "working", "panel": {
    "room_id": "flow-room", "room_participant": "extra-%s" % n, "role": "worker"}} for n in range(3)]
nav = {}
render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=nav)
lower = next(b for b in nav["bands"] if b["name"] == "worker")
choose(("scroll", 3, 5, lower["y1"]), nav)
assert nav["band_offsets"]["worker"] == 1 and nav["band_offsets"]["judge"] == 0, nav["band_offsets"]
assert "EXTRA WORKER 0" in render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=nav)
# Pinned mains render as side-by-side lanes; their calls preview in place.
lanes = {"pinned": ["flow-main", "flow-other"], "preview": {"target": ("result", "builder"),
         "report": {"rows": [{"calls": [{"answer": "PREVIEW ANSWER TEXT"}]}]}}}
for glyphs in (False, True):
    picture = render(flow, "codex", 110, 30, view="graph", unicode=glyphs, main_attempt="flow-attempt", navigation=lanes)
    assert "MAIN / Opus" in picture and "ONLY OTHER OWNER" in picture and "PREVIEW ANSWER TEXT" in picture, picture
    assert "NO PARENT PROOF" not in picture and len(picture.splitlines()) <= 30, picture
    assert all(display_width(line) <= 110 for line in picture.splitlines())
    if not glyphs:
        assert "╭" not in picture and "─" not in picture and "·" not in picture, picture
row = next(h for h in lanes["hits"] if h["action"] == ("result", "foreign-child"))
assert row.get("preview") and any(b["name"] == "lane:flow-other" for b in lanes["bands"])
assert choose((" ",), dict(lanes, selected=("chat", "flow-main"))) == ("pin", "flow-main")
pin = next(h for h in lanes["hits"] if h["action"] == ("pin", "*"))
assert choose(("click", pin["x1"], pin["y"]), lanes) == ("pin", "*")
# The overview keeps one-line cards while the selected block's content fills the lower detail area.
mailed = deepcopy(flow)
mailed["room"]["messages"] = [{"sender": "advisor", "targets": ["flow-main"], "text": "MAIL FOR MAIN"}]
mail_before = json.dumps(mailed, sort_keys=True)
detail_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
picture = render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation=detail_nav)
assert "DETAIL / MAIN / Sol" in picture and "Team: 5 active" in picture and "MAIL FOR MAIN" in picture, picture
assert "ADVISOR / Astra" in picture and len(picture.splitlines()) <= 30, picture
assert json.dumps(mailed, sort_keys=True) == mail_before, "reading main detail changed room mail"
detail_band = next(b for b in detail_nav["bands"] if b["name"] == "detail")
choose(("scroll", 3, 5, detail_band["y1"] + 1), detail_nav)
assert detail_nav["band_offsets"]["detail"] == 3, detail_nav["band_offsets"]
tab = next(h for h in detail_nav["hits"] if h["action"] == ("chat", "flow-other") and h.get("select"))
assert choose(("click", tab["x1"], tab["y"]), detail_nav) is None
assert detail_nav["preview"] == {"target": ("chat", "flow-other")}
# An ordinary report keeps its body; only a deliberation (Answer/Findings) drops text before its sections.
import panel_results
report_artifact = (b"# Peer call\n## Output\n**What changed:** BODY TEXT\n**Why / Evidence:** FINDING TEXT\n"
                   b"**Verification:** passed\n## Exit\n0\n")
with patch.object(panel_results, "_records", return_value=(
        [{"attempt_id": "a1", "task_id": "report-body", "state": "done", "refs": {}}],
        [{"kind": "ask", "task_id": "report-body", "attempt_id": "a1", "artifact": ".oms/artifacts/ask/r.md", "ts": "1"}])), \
        patch.object(panel_results, "_indexed", return_value=report_artifact):
    kept_answer = panel_results.results(project, task_id="report-body")["rows"][0]["calls"][0]["answer"]
assert "BODY TEXT" in kept_answer and "FINDING TEXT" in kept_answer and "passed" in kept_answer, kept_answer
# A worker's detail lists the changed files of its recorded patch, only while the digest matches.
import hashlib
import panel_results
patch_dir = project / ".oms/artifacts/delegate"
patch_dir.mkdir(parents=True, exist_ok=True)
patch_bytes = b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1,2 @@\n-old\n+new\n+more\n"
(patch_dir / "fixture-diffstat.patch").write_bytes(patch_bytes)
patch_row = {"patch": ".oms/artifacts/delegate/fixture-diffstat.patch", "patch_sha256": hashlib.sha256(patch_bytes).hexdigest()}
stat = panel_results._diffstat(project, patch_row)
assert stat == {"files": [["value.txt", 2, 1]], "count": 1, "added": 2, "removed": 1}, stat
# Content lines that look like headers still count once a hunk has started.
tricky = (b"diff --git a/x b/dir b/y.txt\n--- a/dir b/y.txt\n+++ b/dir b/y.txt\n@@ -1,2 +1,2 @@\n"
          b"--- removed dashes\n+++ added pluses\n")
(patch_dir / "fixture-tricky.patch").write_bytes(tricky)
tricky_stat = panel_results._diffstat(project, {"patch": ".oms/artifacts/delegate/fixture-tricky.patch",
                                                "patch_sha256": hashlib.sha256(tricky).hexdigest()})
assert tricky_stat["files"] == [["dir b/y.txt", 1, 1]], tricky_stat
two_files = (b"diff --git a/one.txt b/one.txt\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1,2 @@\n-a\n+b\n+c\n"
             b"diff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-x\n+y\n")
(patch_dir / "fixture-two.patch").write_bytes(two_files)
assert panel_results._diffstat(project, {"patch": ".oms/artifacts/delegate/fixture-two.patch",
    "patch_sha256": hashlib.sha256(two_files).hexdigest()})["files"] == [["one.txt", 2, 1], ["two.txt", 1, 1]]
for bad in (dict(patch_row, patch_sha256="0" * 64), dict(patch_row, patch="../outside.patch")):
    try:
        panel_results._diffstat(project, bad)
    except ValueError:
        pass
    else:
        raise AssertionError("unverified patch evidence was summarized")
changed_nav = {"preview": {"target": ("result", "builder"), "report": {"rows": [{"calls": [
    {"answer": "Patched the parser.", "changes": stat}]}]}}}
picture = render(flow, "codex", 100, 34, view="graph", main_attempt="flow-attempt", navigation=changed_nav)
assert "Changed files: 1 (+2 -1)" in picture and "value.txt +2 -1" in picture, picture
# Board placement follows the window shape (cells are about twice as tall as wide), with a
# hysteresis band so a resize near the boundary does not flap; explicit positions are kept.
assert panel.shape(220, 50) == "left" and panel.shape(157, 132) == "top"
assert panel.shape(200, 100, "left") == "left" and panel.shape(200, 100, "top") == "top"
with patch.dict(os.environ, {"OMS_PANEL_POSITION": "auto", "TMUX_PANE": "%7"}), \
        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "240 60\n", "")):
    assert panel.board_position() == "left"
with patch.dict(os.environ, {"OMS_PANEL_POSITION": "side"}):
    assert panel.board_position() == "side" and panel.board_view("auto") == "summary"
with patch.dict(os.environ, {"OMS_PANEL_POSITION": "auto", "TMUX_PANE": "%7"}), patch.object(panel.subprocess, "run", side_effect=[
        subprocess.CompletedProcess([], 0, "%7 157 157\n%8 157 157\n", ""),
        subprocess.CompletedProcess([], 0, "240 60\n", ""),
        subprocess.CompletedProcess([], 0, "", ""), subprocess.CompletedProcess([], 0, "", "")]) as moved:
    assert panel.relayout_board() == "left"
    assert moved.call_args_list[2].args[0][1:3] == ["join-pane", "-h"], moved.call_args_list
# A watcher restart keeps the person's selection: watch() restores what the reload saved.
resumed_nav = {}
class StopWatch(Exception):
    pass
def capture_frame(*args, **kwargs):
    resumed_nav.update(kwargs["navigation"])
    raise StopWatch()
with patch.dict(os.environ, {"OMS_PANEL_RESUME": json.dumps({"room_id": "r1", "selected": ["chat", "m2"], "dismissed": True,
                                                            "preview": ["debate", ["t1", "m2"]]})}), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel, "read_panel", return_value=({"room": {"id": "r1"}}, 0, "codex", None, "graph", False, "r1")), \
        patch.object(panel, "frame_view", side_effect=capture_frame):
    try:
        panel.watch(Path("."), "codex", count=1)
    except StopWatch:
        pass
assert resumed_nav["selected"] == ("chat", "m2") and resumed_nav["dismissed"] is True, resumed_nav
assert resumed_nav["preview"]["target"] == ("debate", ("t1", "m2")), resumed_nav
# A watcher restarts only for changed sources that hold still, compile and import in a fresh interpreter.
stamps_now = panel.source_stamps()
assert panel.reload_ready(stamps_now) is False
clock = [100.0]
panel.REJECTED_RELOAD.clear()
with patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]):
    assert panel.reload_ready({}) is False      # first sighting only starts the clock
    clock[0] += .5
    assert panel.reload_ready({}) is False      # not yet one second apart
    clock[0] += .6
    assert panel.reload_ready({}) is True
    with patch.object(panel, "compile", side_effect=SyntaxError("broken"), create=True):
        assert panel.reload_ready({}) is False
    # A tree that compiles but cannot be imported (a half-copied install) keeps the old watcher running.
    broken_tree = temporary / "broken-reload"
    shutil.copytree(root / "scripts" / "lib", broken_tree / "scripts" / "lib", ignore=shutil.ignore_patterns("__pycache__"))
    with patch.object(panel, "ROOT", broken_tree):
        (broken_tree / "scripts" / "lib" / "room_view.py").write_text("from panel_view import no_such_name\n", encoding="utf-8")
        panel.REJECTED_RELOAD.clear()
        assert panel.reload_ready({}) is False
        clock[0] += 1.1
        assert panel.reload_ready({}) is False and panel.REJECTED_RELOAD.pop("notice") is True
        clock[0] += 1.1
        assert panel.reload_ready({}) is False and "notice" not in panel.REJECTED_RELOAD
    panel.REJECTED_RELOAD.clear()
    # Sources still changing between two reads are not reloaded yet.
    unstable = root / "scripts" / "lib" / "panel_view.py"
    for pair in ([{unstable: 1}, {unstable: 2}, {unstable: 2}, {unstable: 2}],):
        with patch.object(panel, "source_stamps", side_effect=pair), patch.object(panel, "compile", create=True):
            assert panel.reload_ready({}) is False
            clock[0] += 1.1
            assert panel.reload_ready({}) is False   # changed again since the first sighting
panel.REJECTED_RELOAD.clear()
# A finished worker's kept worktree opens only when its label names exactly one git worktree.
kept_parent = temporary / "oh-my-setting-delegate.fixture"
kept_parent.mkdir()
subprocess.run(["git", "-C", str(project), "worktree", "add", "--detach", str(kept_parent / "wt")],
               check=True, capture_output=True)
kept = {"participant": "call-kept", "role": "worker", "location": "worktree:oh-my-setting-delegate.fixture/wt"}
assert panel.worker_worktree(project, dict(kept, state="done")) == (kept_parent / "wt").resolve()
for member, reason in ((dict(kept, state="working"), "still running"),
                       (dict(kept, state="done", location="worktree:gone.parent/wt"), "cleaned up"),
                       (dict(kept, state="done", location="repository"), "recorded no worktree")):
    try:
        panel.worker_worktree(project, member)
    except ValueError as error:
        assert reason in str(error), error
    else:
        raise AssertionError("worktree shell opened for " + repr(member))
assert TerminalInput().decode(b"w") == [("w",)]
with patch("room_view.nodes", return_value=[dict(kept, state="done")]), \
        patch.dict(os.environ, {"TMUX": ""}):
    assert panel.open_worktree(project, {}, {"selected": ("result", "call-kept")}) == "Worktree: " + str((kept_parent / "wt").resolve())
subprocess.run(["git", "-C", str(project), "worktree", "remove", "--force", str(kept_parent / "wt")], check=True)
# The board's advisor action opens a question prompt on the control path; it never calls a model itself.
assert TerminalInput().decode(b"a") == [("a",)]
with patch.dict(os.environ, {"TMUX": ""}):
    hint = panel.ask_advisor(Path("/tmp/project-a"), mailed, detail_nav)
assert "--main flow-other" in hint and "--dispatch advisor" in hint, hint
# Inside a user's own tmux session (no OMS panel session) the board opens no popup or window.
with patch.dict(os.environ, {"TMUX": "fixture", "TMUX_PANE": "%9", "OMS_PANEL_SESSION": ""}), \
        patch.object(panel.subprocess, "Popen") as foreign_popup, patch.object(panel.subprocess, "run") as foreign_run:
    assert panel.ask_advisor(Path("/tmp/project-a"), mailed, detail_nav).startswith("Ask from a terminal: ")
    foreign_popup.assert_not_called()
with patch.dict(os.environ, {"TMUX": "fixture", "TMUX_PANE": "%9", "OMS_PANEL_SESSION": "oms-panel-abcdef012345"}), \
        patch.object(panel.subprocess, "Popen") as popup:
    # Without a selected main, the shown main is the target.
    assert "flow-main" in panel.ask_advisor(Path("/tmp/project-a"), mailed, dict(detail_nav, selected=None))
    argv = popup.call_args.args[0]
    assert argv[:2] == ["tmux", "display-popup"] and argv[argv.index("-t") + 1] == "%9", argv
    assert "-u OMS_ROOM_PARTICIPANT" in argv[-1] and "--main flow-main" in argv[-1], argv
    assert "Advisor call failed" in argv[-1] and "read -r _" in argv[-1], "a failed call closed its popup"
    assert argv[-1].startswith("sh -c "), "popup script depends on the user's default shell"
try:
    panel.ask_advisor(Path("/tmp/project-a"), mailed, {"room_id": "flow-room"})
except ValueError as error:
    assert "select a joined main" in str(error)
else:
    raise AssertionError("advisor action ran without a selected main")
with patch.object(panel.sys.stdin, "isatty", return_value=False):
    assert panel.main(["--repo", str(root), "--dispatch", "advisor", "--owner", "claude", "--main", "flow-main"]) == 1
with patch.object(panel, "session_owner", return_value="/elsewhere"):
    try:
        panel.split_panel(Path("/tmp/project-a"))
    except ValueError as error:
        assert "not this checkout" in str(error)
    else:
        raise AssertionError("a foreign panel session was reused")
assert panel.panel_session(Path("/tmp/project-a")) == panel.panel_session(Path("/tmp/project-a/."))
# A launch that loses the creation race joins the winner without treating its own new room as a choice.
race_repo = Path("/tmp/project-a")
original_split = panel.split_panel
seen = {}
with patch.dict(os.environ, {"OMS_ROOM_ID": ""}), patch.object(panel, "session_owner", side_effect=[None, str(race_repo), str(race_repo)]), \
        patch.object(panel, "legacy_session", return_value=None), patch.object(panel, "ensure_room", return_value="room-lost"), \
        patch.object(panel.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, ["tmux"])), \
        patch.object(panel, "split_panel", side_effect=lambda *a, **k: seen.update(room=os.environ.get("OMS_ROOM_ID"), retry=k.get("retry")) or 0):
    os.environ.pop("OMS_ROOM_ID")
    assert original_split(race_repo, launch="codex") == 0
assert seen == {"room": None, "retry": False}, seen
# A new panel opens every installed main in its own window, sharing the room recorded before the owner marker.
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-both"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.shutil, "which", return_value="/fixture/bin"), \
        patch.object(panel.subprocess, "check_output", side_effect=["%1\n", "%2\n", "%3\n"]) as created_panes, \
        patch.object(panel.subprocess, "run") as tmux_calls, patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
    os.environ.pop("OMS_ROOM_ID")
    os.environ.pop("OMS_PANEL_MAINS", None)
    assert panel.split_panel(race_repo) == 0
    windows = [c.args[0] for c in created_panes.call_args_list]
    assert windows[0][1] == "new-session" and [w[w.index("-n") + 1] for w in windows[1:]] == ["codex", "claude"], windows
    order = [c.args[0][1:5] for c in tmux_calls.call_args_list]
    assert ["set-environment", "-t", "=" + panel.panel_session(race_repo), "OMS_ROOM_ID"] in order
    assert any(c.args[0][1:3] == ["select-window", "-t"] and c.args[0][3] == "%2" for c in tmux_calls.call_args_list)
with patch.dict(os.environ, {"OMS_PANEL_MAINS": ""}):
    assert panel.panel_mains() == []
with patch.dict(os.environ, {"OMS_PANEL_MAINS": "claude,claude,unknown"}), patch.object(panel.shutil, "which", return_value="/x"):
    assert panel.panel_mains() == ["claude"]
# A session whose owner marker is not written yet is waited for, not refused as foreign.
with patch.object(panel, "session_owner", side_effect=["", str(race_repo)]), patch.object(panel.time, "sleep"), \
        patch.object(panel.subprocess, "call", return_value=0):
    assert panel.split_panel(race_repo) == 0
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-first"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.subprocess, "check_output", return_value="%1\n"), \
        patch.object(panel.subprocess, "run") as tmux_calls, patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
    os.environ.pop("OMS_ROOM_ID")
    assert panel.split_panel(race_repo, launch="codex") == 0
    order = [c.args[0][1:4] for c in tmux_calls.call_args_list]
    assert order.index(["set-environment", "-t", "=" + panel.panel_session(race_repo)]) < \
        next(i for i, a in enumerate(order) if a[0] == "set-option"), order
# One proven legacy random-named panel is adopted; several are never merged.
legacy_rows = [{"session": "oms-panel-aaaaaaaaaaaa"}, {"session": "oms-panel-aaaaaaaaaaaa"}]
with patch.object(panel, "panel_panes", return_value=legacy_rows):
    assert panel.legacy_session(Path("/tmp/project-a")) == "oms-panel-aaaaaaaaaaaa"
with patch.object(panel, "panel_panes", return_value=legacy_rows + [{"session": "oms-panel-bbbbbbbbbbbb"}]):
    assert panel.legacy_session(Path("/tmp/project-a")) is None
assert re.fullmatch(r"oms-panel-[a-f0-9]{12}", panel.panel_session(Path("/tmp/project-a")))
# A control window (no own main) overviews every main as lanes until pins are chosen.
control_nav = {}
overview = render(flow, "codex", 110, 30, view="graph", navigation=control_nav)
assert "MAIN / Opus" in overview and "ONLY OTHER OWNER" in overview and "[x All]" in overview, overview
assert control_nav["auto_pins"] == ["flow-main", "flow-other"]
panel.navigate(Path("."), ("pin", "*"), {}, control_nav, 110)
assert control_nav["pinned"] == []
focused = render(flow, "codex", 110, 30, view="graph", navigation=control_nav)
assert "ONLY OTHER OWNER" not in focused and "[+ All]" in focused, focused
assert "ONLY OTHER OWNER" not in render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
assert "ONLY OTHER OWNER" in render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt",
                                   navigation={"expanded": True}), "an expanded native board overviews every main"
assert "ONLY OTHER OWNER" in render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt",
                                   navigation={"overview": True}), "the live board overviews every main"
# Mains live in another bounded room of the same panel are named, never merged into this graph.
elsewhere = deepcopy(flow)
elsewhere["room"]["other_rooms"] = [{"id": "room-next", "mains": 1, "pending": 2}]
picture = render(elsewhere, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
assert "OTHER ROOMS / room-next 1 live main / 2 pending" in picture, picture
with patch.object(panel.room, "discover", return_value=([
        {"id": "room-a", "closed": False, "pending_count": 0, "participants": [
            {"participant": "att-live", "joined": True, "role": "main"},
            {"participant": "att-gone", "joined": True, "role": "main"}]},
        {"id": "room-b", "closed": False, "pending_count": 0, "participants": [
            {"participant": "att-gone-too", "joined": True, "role": "main"}]}], False)):
    panel.ROOM_SUMMARY.clear()
    assert panel.other_rooms(Path("/tmp/rooms"), "room-current", {"att-live"})[0] == [
        {"id": "room-a", "title": None, "pending": 0, "mains": 1}]
with patch.object(panel.room, "discover", return_value=([{"id": "room-p", "closed": False, "messages": [
        {"pending_for": ["att-live", "x"]}], "participants": [{"participant": "att-live", "joined": True, "role": "main"}]}], True)):
    panel.ROOM_SUMMARY.clear()
    others, incomplete = panel.other_rooms(Path("/tmp/rooms"), "room-current", {"att-live"})
    assert others[0]["pending"] == 2 and incomplete is True
panel.ROOM_SUMMARY.clear()
partial = deepcopy(flow)
partial["room"]["other_rooms_incomplete"] = True
assert "scan incomplete" in render(partial, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
partial["room"]["other_rooms"] = [{"id": "room-next", "mains": 1, "pending": 0}]
partial["room"]["other_rooms"] = [{"id": "room-%s-long-identifier" % n, "mains": 1, "pending": 3} for n in range(4)]
assert "OTHER ROOMS (scan incomplete)" in render(partial, "codex", 110, 30, view="graph",
                                                 main_attempt="flow-attempt", navigation={})
# Page keys move graph bands (or the shown detail) rather than an offset the bands ignore.
page_nav = {}
render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
choose(("pagedown",), page_nav)
assert page_nav["band_offsets"]["detail"] == 6, "a shown detail takes the page keys"
page_nav = {"dismissed": True}
render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
choose(("pagedown",), page_nav)
assert page_nav["band_offsets"]["worker"] == 3, page_nav["band_offsets"]
# A results screen opened from the graph scrolls itself, not the graph's stale bands.
from panel_tree import render_detail
page_nav["detail"] = {"title": "RECORDED RESULTS", "text": "\n".join("line %s" % n for n in range(300))}
render_detail(flow, page_nav["detail"], 110, 30, page_nav)
choose(("pagedown",), page_nav)
choose(("scroll", 3, 5, 10), page_nav)
assert page_nav["offset"] > 0 and page_nav["bands"] == [], page_nav["offset"]
assert "EXTRA WORKER 2" in render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
stale_nav = {"pinned": ["room-old-main"]}
assert "ONLY OTHER OWNER" in render(flow, "codex", 110, 30, view="graph", navigation=stale_nav) and stale_nav["pinned"] is None
try:
    panel.main(["--repo", str(root), "--dispatch", "advisor", "--owner", "claude", "--main", "a b", "--dry-run", "--prompt", "x"])
except SystemExit as error:
    assert error.code == 2
else:
    raise AssertionError("an invalid --main was accepted")
# Each snapshot refreshes an open preview, with or without saved pins; another room resets the view.
kept_nav = {"room_id": "flow-room", "preview": {"target": ("result", "builder"), "report": {}}}
assert panel.refreshed_navigation(kept_nav, flow, "")["preview"]["refresh"] is True
assert panel.refreshed_navigation(dict(kept_nav, preview={"target": 1}), flow, "a,b,-x")["pinned"] == ["a", "b", "-x"]
assert "preview" not in panel.refreshed_navigation(dict(kept_nav, room_id="other"), flow, None)
# A full or closed selected room makes the next main start a new room instead of reusing it.
with patch.dict(os.environ, {"OMS_ROOM_ID": "room-full"}), patch.object(panel, "room_headroom", return_value=False), \
        patch.object(panel.room, "create", return_value="room-fresh") as fresh, \
        patch.object(panel, "managed_session", return_value=False):
    assert panel.panel_room(Path("/tmp/project-a")) == "room-fresh" and fresh.called
# --main without a room cannot fall through to an unowned call.
with patch.dict(os.environ, {"OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": "", "OMS_PANEL_MAIN_ATTEMPT": ""}), \
        patch.object(panel, "child_environment", return_value={"PATH": os.environ.get("PATH", "")}), \
        patch.object(panel.shutil, "which", return_value="/bin/true"), \
        patch.object(panel.subprocess, "call", side_effect=AssertionError("model call without owner")):
    try:
        panel.dispatch(Path("/tmp/project-a"), "claude", "advisor", "routine", "auto", "read", "advise",
                       prompt="Who owns this?", main="unknown-main")
    except ValueError as error:
        assert "--main needs a room" in str(error)
    else:
        raise AssertionError("--main without a room was ignored")
# UX contract from the critique debate: the quit key survives narrow footers, arrows move the
# detail with the main, hidden tabs keep their attention, and attention rows name their state.
# Footer hints follow the selection (at most five), keep the chat/board toggle and "?" in every width,
# name only keys that work, and "?" swaps in the full key list.
for view_name in ("graph", "tree"):
    for columns in (40, 80, 100, 157):
        for managed_board in (True, False):
            drawn = render(flow, "codex", columns, 24, view=view_name, main_attempt="flow-attempt",
                           navigation={}, managed=managed_board).splitlines()
            # The panel-wide F-keys are fixed at the end of the hint line; board keys fill what is left.
            footer_line = next(line for line in reversed(drawn) if "Keys" in line)
            # The key hints are pinned to the board's last row, however short the content is.
            assert "Keys" in drawn[-1] and len(drawn) == 24, (view_name, columns, drawn[-3:])
            assert display_width(footer_line) <= columns, (view_name, columns, footer_line)
            if columns >= 80:
                assert footer_line.endswith("F12 Keys" if managed_board else "? Keys"), (view_name, columns, footer_line)
            assert ("F9" in footer_line) == managed_board and ("q Quit" in footer_line) == (not managed_board), footer_line
            assert len(re.split(r"  +", footer_line)) <= 7 and "Esc Back" not in footer_line and "q Chat" not in footer_line, footer_line
    open_nav = {"preview": {"target": ("chat", "flow-main")}}
    assert "Esc Close" in render(flow, "codex", 157, 24, view=view_name, main_attempt="flow-attempt",
                                 navigation=open_nav, managed=True).splitlines()[-1]
    for expanded_board, label in ((False, "v Expand"), (True, "v Collapse")):
        helped = render(flow, "codex", 100, 24, view=view_name, main_attempt="flow-attempt", managed=True,
                        navigation={"keys_help": True, "expanded": expanded_board}).splitlines()
        assert label in "\n".join(helped) and "? Hide" in "\n".join(helped), helped
        assert all(display_width(line) <= 100 for line in helped), helped
arrow_nav = {}
render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=arrow_nav)
choose(("right",), arrow_nav)
assert arrow_nav["preview"] == {"target": ("chat", "flow-other")} and arrow_nav["band_offsets"]["detail"] == 0
alarmed = deepcopy(flow)
alarmed["room"]["participants"].append({"participant": "third-main", "role": "main", "joined": True,
                                        "provider": "claude", "model": "claude-opus-5-5", "label": "Third"})
alarmed["room"]["participants"].append({"participant": "third-fail", "role": "worker", "joined": True,
                                        "model": "gpt-6-luna", "parent": "third-main", "label": "Broken"})
alarmed["attempts"]["active_recent"] += [
    {"attempt_id": "third-main", "state": "working", "panel": {"room_id": "flow-room", "room_participant": "third-main", "role": "main"}},
    {"attempt_id": "third-fail", "state": "failed", "panel": {"room_id": "flow-room", "room_participant": "third-fail", "role": "worker"}}]
hidden_nav = {}
narrow = render(alarmed, "codex", 80, 24, view="graph", main_attempt="flow-attempt", navigation=hidden_nav)
assert "+1 ▸ !1" in narrow, narrow
for columns in range(76, 90):
    sweep_nav = {}
    swept = render(alarmed, "codex", columns, 24, view="graph", main_attempt="flow-attempt", navigation=sweep_nav)
    assert all(display_width(line) <= columns for line in swept.splitlines()), (columns, swept)
    assert all(hit["x2"] <= columns for hit in sweep_nav["hits"]), columns
marker = next(h for h in hidden_nav["hits"] if h["action"] == ("chat", "third-main") and h.get("select"))
assert choose(("click", marker["x1"], marker["y"]), hidden_nav) is None and hidden_nav["selected"] == ("chat", "third-main")
lane_picture = render(alarmed, "codex", 120, 30, view="graph", navigation={})
assert "Worker Luna 6 ! failed" in lane_picture, lane_picture
# Finished calls leave the board; their count remains as "N done", with results in the main's detail.
finished_flow = deepcopy(flow)
for attempt in finished_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
finished_picture = render(finished_flow, "codex", 120, 34, view="graph", navigation={"dismissed": True})
assert "Worker Sonnet" not in finished_picture and "1 done" in finished_picture, finished_picture
lines_of = finished_picture.splitlines()
assert next(i for i, l in enumerate(lines_of) if "Advisor Astra" in l) < next(i for i, l in enumerate(lines_of) if "MAIN / Sol 6" in l) \
    < next(i for i, l in enumerate(lines_of) if "Worker Luna" in l), finished_picture
ordered = render(flow, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation={"preview": {
    "target": ("result", "builder"), "report": {"rows": [{"outcome": "accepted", "calls": [{"answer": "DONE TEXT"}]}]}}})
assert ordered.index("Accepted by its main") < ordered.index("DONE TEXT"), ordered
auto_nav = {}
assert "DETAIL / MAIN / Sol 6 / auto" in render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation=auto_nav)
roomy = render(flow, "codex", 100, 60, view="graph", main_attempt="flow-attempt", navigation={})
roomy_lines = roomy.splitlines()
detail_top = next(i for i, line in enumerate(roomy_lines) if "DETAIL /" in line)
detail_end = next(i for i in range(detail_top + 1, len(roomy_lines)) if roomy_lines[i].startswith("╰"))
# The board fills the pane (key hints pinned to the last row), but a short detail keeps a short box.
assert detail_end - detail_top < 20 and len(roomy_lines) == 60, "a short detail stretched over the board"
assert "DETAIL /" not in render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
# People read the board: call exits become words, machine status lines and markdown marks are dropped.
import room_view as graph_text
assert graph_text.readable("Call exit=0; parent acceptance pending.\nstop-reason: provider=codex is_error=0\n**Verification:** `ok`", " ") \
    == "Finished. Verification: ok"
assert graph_text.readable("Call exit=2; parent acceptance pending.", " ") == "Failed (exit 2)."
assert graph_text.readable("## Decision\n\nFirst paragraph\n\n\n- next step", paragraphs=True) == "Decision\n\nFirst paragraph\n\n- next step"
talking = deepcopy(flow)
talking["room"]["pairs"] = [{"sender": "flow-main", "recipient": "flow-other", "sent": 2, "pending": 1}]
picture = render(talking, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
assert "Between mains: 1 pair · 1 unread" in picture, picture
tall = render(talking, "codex", 157, 65, view="graph", main_attempt="flow-attempt", navigation={})
assert "Between mains\n  Sol 6 → Opus 5.5 · 2 messages · 1 unread\n" in tall, tall
assert "Model use, last calls" not in tall and "W0 A0" not in tall and "Click any block" not in tall, tall
assert "..." not in tall and sum("week" in line for line in tall.splitlines()) <= 1, tall
ascii_tall = render(talking, "codex", 157, 65, view="graph", unicode=False, main_attempt="flow-attempt", navigation={})
assert not any(g in ascii_tall for g in "…│·→⚑") and "Between mains\n  Sol 6 > Opus 5.5 / 2 messages / 1 unread\n" in ascii_tall, ascii_tall
from dashboard_projection import fit, use_unicode
use_unicode(True)
assert fit("abcdefghij", 6) == "abcde…"
use_unicode(False)
assert fit("abcdefghij", 6) == "abc..."
use_unicode(True)
from panel_view import activity
assert activity("waiting_approval") == "⚑" and activity("waiting_approval", unicode=False) == "A" and activity("waiting_input") == "?"
crowded_tabs = deepcopy(flow)
crowded_tabs["room"]["participants"][0]["label"] = "Shared prefix for every main alpha-tail"
crowded_tabs["room"]["participants"][1]["label"] = "Shared prefix for every main omega-tail"
narrow = next(line for line in render(crowded_tabs, "codex", 90, 30, view="graph", main_attempt="flow-attempt",
                                      navigation={}).splitlines() if "[+ All]" in line)
assert narrow.count("-tail") == 2, narrow
# A failed call marks its own main's window with "! "; a handled failure or another main's call does not.
alarm = deepcopy(flow)
assert not panel.main_needs_attention(alarm, "flow-attempt")
next(a for a in alarm["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = "failed"
assert panel.main_needs_attention(alarm, "flow-attempt") and not panel.main_needs_attention(alarm, "flow-other")
alarm["room"]["messages"] = [{"id": "result-builder", "sender": "builder", "recipient": "flow-attempt", "message_kind": "handoff", "pending_for": []}]
assert not panel.main_needs_attention(alarm, "flow-attempt")
renames = []
class TmuxStub:
    def __init__(self, name):
        self.name = name
    def __call__(self, command, **kwargs):
        if "rename-window" in command:
            renames.append(command[-1])
            self.name = command[-1]
        return subprocess.CompletedProcess(command, 0, self.name + "\n" if "display-message" in command else "")
os.environ.update(TMUX="x", TMUX_PANE="%7", OMS_PANEL_SESSION="oms-panel-0123456789ab")
for original, marks in (("codex", ["! codex", "codex"]), ("my-shell", [])):
    with patch.object(panel.subprocess, "run", TmuxStub(original)):
        panel.mark_window(True)
        panel.mark_window(True)
        panel.mark_window(False)
    assert renames == marks, (original, renames)
    renames.clear()
for name in ("TMUX", "TMUX_PANE", "OMS_PANEL_SESSION"):
    del os.environ[name]
# A short board shows a notice instead of dropping it.
assert "Opening chat" in render(flow, "codex", 100, 17, view="graph", main_attempt="flow-attempt",
                                navigation={"notice": "Opening chat..."})
# Messages and details name each participant with its role, so a Sol worker never reads like the Sol main.
same_model = deepcopy(flow)
same_model["room"]["participants"].append({"participant": "sol-worker", "role": "worker", "model": "gpt-6-sol",
                                           "joined": True, "parent": "flow-attempt", "label": "Same model worker"})
same_model["room"]["messages"] = [{"sender": "sol-worker", "targets": ["flow-main"], "text": "Call exit=1; parent acceptance pending."}]
picture = render(same_model, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                 navigation={"preview": {"target": ("chat", "flow-main"), "report": {}}})
assert "From Sol 6 worker: Failed (exit 1)." in picture and "Messages to Sol 6 main" in picture, picture
assert "Sol 6 worker → Sol 6 main: Failed (exit 1)." in render(same_model, "codex", 110, 40, view="graph",
                                                          main_attempt="flow-attempt", navigation={"dismissed": True})
# A finished call stays reachable from its main's detail: its team row selects it.
done_flow = deepcopy(flow)
for attempt in done_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
team_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt", navigation=team_nav)
row = next(h for h in team_nav["hits"] if h["action"] == ("result", "builder"))
assert choose(("click", row["x1"], row["y"]), team_nav) is None and team_nav["preview"] == {"target": ("result", "builder")}
assert "DETAIL / WORKER / Sonnet 5.5" in render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                                                  navigation=team_nav)
# Expanded detail puts decisions before raw answers and keeps every grouped team row actionable.
people = graph_text.nodes(done_flow)
main = next(m for m in people if m["participant"] == "flow-main")
team = [m for m in people if m["participant"] in {"advisor", "researcher", "builder"}]
team[0]["state"] = "waiting_approval"
team[1]["state"] = "working"
team[2]["state"] = "done"
for columns in (28, 76, 110):
    lines, targets = graph_text.detail(main, {}, columns, people, {}, everyone={"flow-main": team})
    assert lines.index("  Finished") < lines.index("  Needs attention") < lines.index("  Active"), lines
    assert set(targets.values()) == {("result", m["participant"]) for m in team}, targets
    assert next(i for i,line in enumerate(lines) if "Messages to" in line) < lines.index("  Finished"), lines
    for row, target in targets.items():
        assert 0 <= row < len(lines) and target[1] in {m["participant"] for m in team}
    assert any("Team:" in line for line in lines) and any("Messages to" in line for line in lines)
unknown_call = dict(team[2], state="presence unknown")
lines, targets = graph_text.detail(main, {}, 76, people, {}, everyone={"flow-main": [unknown_call]})
assert "Team: 1 no record" in lines and "  Active" not in lines and "  Finished" not in lines
assert any("Messages to" in line for line in lines[:8]) and ("result", "builder") in targets.values()
worker = next(m for m in people if m["participant"] == "builder")
recorded = {"rows": [{"outcome": "accepted", "verification": {"status": "passed"},
    "summary": "OWNER DECISION", "calls": [{"answer": "RAW WORKER ANSWER"}]}]}
kept = json.dumps(recorded, sort_keys=True)
lines, targets = graph_text.detail(worker, {"report": recorded}, 76, people)
assert lines.index("Main's summary:") < lines.index("Worker's answer:"), lines
assert "  OWNER DECISION" in lines and "  RAW WORKER ANSWER" in lines
assert "Accepted by its main · checks passed" in lines
assert any("Permissions unrecorded" in line for line in lines) and not any("Read-only" in line for line in lines)
long_recorded = deepcopy(recorded)
long_recorded["rows"][0]["summary"] = "\n".join("decision line %s" % i for i in range(1000))
long_recorded["rows"][0]["calls"][0]["answer"] = "answer " * 20000
lines, _ = graph_text.detail(worker, {"report": long_recorded}, 76, people)
assert len(lines) < 60 and sum("More in the full recorded result" in line for line in lines) == 2, lines
assert json.dumps(recorded, sort_keys=True) == kept
# f opens the full retained reader through the existing revalidated result path; Enter stays a preview.
assert TerminalInput().decode(b"f") == [("f",)]
full_recorded = deepcopy(recorded)
full_recorded["rows"][0]["calls"][0]["answer"] = "\n".join("full line %s" % i for i in range(80)) + "\nFINAL RETAINED LINE"
full_nav = {"selected": ("result", "builder"), "preview": {"target": ("result", "builder"), "report": full_recorded}}
preview_screen = render(done_flow, "codex", 110, 40, view="graph", navigation=full_nav)
assert "FINAL RETAINED LINE" not in preview_screen and "f Full result" in preview_screen, preview_screen
full_action = choose(("f",), full_nav)
assert full_action == ("result", "builder"), full_nav
assert choose(("enter",), full_nav) is None
with patch.object(panel.room, "status", return_value=done_flow["room"]), \
        patch.object(panel, "call_results", return_value=deepcopy(full_recorded)) as fetch:
    # Opening is asynchronous: navigate only asks, the watcher's background read completes it.
    panel.navigate(project, full_action, done_flow, full_nav, 110)
    assert full_nav["opening"] == ("result", "builder") and not fetch.called, full_nav
    arrived = panel.read_evidence(project, full_nav["opening"])
    fetch.assert_called_once_with(project, ("result", "builder"))
    late = dict(full_nav, opening=None)
    assert not panel.finish_opening(late, arrived) and "detail" not in late, "a stale read reopened a closed detail"
    assert panel.finish_opening(full_nav, arrived) and "opening" not in full_nav
assert "FINAL RETAINED LINE" in full_nav["detail"]["report"]["rows"][0]["calls"][0]["answer"]
for unused in range(12):
    full_screen = render_detail(done_flow, full_nav["detail"], 110, 24, full_nav)
    if "FINAL RETAINED LINE" in full_screen:
        break
    choose(("pagedown",), full_nav)
else:
    raise AssertionError("full reader could not reach the retained last line")
# A one-row preview scrolls the same body index used by its title and click mapping.
short_detail_cases = []
for columns in (76, 100):
    for rows in range(16, 28):
        nav = {"preview": {"target": ("result", "builder"), "report": full_recorded}, "band_offsets": {"detail": 5}}
        screen = render(done_flow, "codex", columns, rows, view="graph", navigation=nav)
        band = next((b for b in nav.get("bands", []) if b["name"] == "detail"), None)
        if band and band["y2"] - band["y1"] == 2:
            body, _ = graph_text.detail(worker, {"report": full_recorded}, columns - 4, people)
            first = nav["band_offsets"]["detail"]
            assert body[first].strip() in screen.splitlines()[band["y1"]], (columns, rows, first, screen)
            hit = next(h for h in nav["hits"] if h["y"] == band["y1"] + 1)
            assert hit["action"] == ("result", "builder"), hit
            short_detail_cases.append((columns, rows))
assert short_detail_cases, "one-row preview fixture was not exercised"
assert choose(("f",), {"surface": "graph", "items": [("result", "builder")], "preview": {"target": ("chat", "flow-main")},
    "full_result": ("result", "builder")}) is None, "a stale preview opened a result"
for columns, rows in ((76, 24), (100, 30), (160, 42)):
    for ascii_only in (False, True):
        nav = {"preview": {"target": ("result", "builder"), "report": long_recorded}}
        screen = render(done_flow, "codex", columns, rows, view="graph", unicode=not ascii_only, navigation=nav)
        assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines())
        band = next(b for b in nav["bands"] if b["name"] == "detail")
        assert choose(("scroll", 3, 5, band["y1"] + 1), nav) is None
        assert nav["band_offsets"]["detail"] > 0
        render(done_flow, "codex", columns, rows, view="graph", unicode=not ascii_only, navigation=nav)
        assert all(1 <= h["y"] <= rows and 1 <= h["x1"] <= h["x2"] <= columns for h in nav["hits"])

# The header counts unread mail to mains only, shows the time, and a main without a task title shows its
# latest message; failed calls whose result their main already read fold into the finished count.
counted = deepcopy(flow)
# A result kept from before the write-time rule counts only when it went to the call's own parent.
kept_room = {"participants": [{"participant": "kept-call", "role": "worker", "parent": "kept-main"}]}
assert graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff",
                                             "recipient": "kept-main"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff",
                                                 "recipient": "other-main"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "intruder", "message_kind": "handoff"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff"})
# A broadcast stays pending for calls that ended but remain joined; only calls that can still read count.
counted["room"]["pairs"] = [{"sender": "flow-main", "recipient": "flow-other", "sent": 1, "pending": 1},
                            {"sender": "flow-main", "recipient": "done-call", "sent": 2, "pending": 2},
                            {"sender": "flow-main", "recipient": "explorer", "sent": 3, "pending": 3}]
counted["room"]["participants"][1]["label"] = "Native task not recorded"
counted["room"]["messages"] = [{"sender": "flow-other", "targets": ["flow-main"], "text": "Call exit=0; parent acceptance pending.\nRebasing the parser"},
                               {"id": "result-explorer", "sender": "explorer", "recipient": "flow-attempt", "message_kind": "handoff", "targets": ["flow-main"], "pending_for": [], "text": "Call exit=1;"}]
for attempt in counted["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "explorer":
        attempt["state"] = "failed"
picture = render(counted, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"overview": True, "dismissed": True})
first_line = picture.splitlines()[0]
assert "1 unread for live participants" in first_line and re.search(r"\d\d:\d\d$", first_line), first_line
assert "Latest sent: Finished. Rebasing the parser" in picture and "explorer" not in picture and "1 done" in picture, picture
declared_board = deepcopy(counted)
declared_board["room"]["statuses"] = {"flow-other": {"text": "Fixing the parser", "ts": "2026-10-07T09:05:00Z", "seq": 3}}
declared_picture = render(declared_board, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"overview": True, "dismissed": True})
assert "Now: Fixing the parser" in declared_picture and "Latest sent:" not in declared_picture, declared_picture
declared_picture = render(declared_board, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                 navigation={"preview": {"target": ("chat", "flow-other"), "report": {}}})
assert re.search(r"Now: Fixing the parser \(\d\d:\d\d\)", declared_picture), declared_picture
# Advisors and reviewers sit in their own box above each main, which shows its work over several rows.
assert "ADVISORS / REVIEWS / none active" in picture, picture
lane_rows = picture.splitlines()
main_top = next(i for i, l in enumerate(lane_rows) if "MAIN / " in l)
assert next(i for i, l in enumerate(lane_rows) if "ADVISORS / REVIEWS" in l) < main_top, picture
assert next(i for i, l in enumerate(lane_rows) if i > main_top and l.startswith("╰─┬")) - main_top > 3, picture
idle_other = deepcopy(flow)
idle_other["room"]["participants"] = [p for p in idle_other["room"]["participants"] if p["participant"] != "foreign-child"]
empty_lane = render(idle_other, "codex", 160, 34, view="graph", navigation={"dismissed": True})
assert "└ no workers" in empty_lane, empty_lane
lonely = deepcopy(flow)
lonely["room"]["participants"] = [p for p in lonely["room"]["participants"] if p["participant"] == "flow-main"]
alone = render(lonely, "codex", 120, 30, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
assert "Advisors: none active" in alone and "Workers: none running" in alone, alone
navigate_state = {"mains": ["flow-main", "flow-other"], "pinned": ["flow-main"]}
panel.navigate(Path("."), ("pin", "*"), {}, navigate_state, 80)
# Selecting a main that is not pinned still gives it a lane, so the tab marker never points at nothing.
trio = deepcopy(flow)
trio["room"]["participants"].append({"participant": "flow-third", "role": "main", "joined": True, "provider": "claude",
                                     "model": "claude-sonnet-5-5", "label": "Third native chat"})
trio["attempts"]["active_recent"].append({"attempt_id": "flow-third", "state": "working", "panel": {
    "room_id": "flow-room", "room_participant": "flow-third", "role": "main"}})
partial = render(trio, "codex", 160, 40, view="graph", navigation={"pinned": ["flow-main", "flow-third"],
                                                                  "selected": ("chat", "flow-other")})
assert "▸ MAIN / Opus 5.5" in partial, partial
# Four to six mains fill two rows (2+2, 3+2, 3+3) in window order; a short board keeps one row.
grid_models = ("gpt-6-sol", "claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "gpt-6-luna", "claude-haiku-4-5-20251001")
for grid_count, grid_shape in ((4, (2, 2)), (5, (3, 2)), (6, (3, 3))):
    many = deepcopy(flow)
    many["room"]["participants"] = [p for p in many["room"]["participants"] if p["role"] != "main" or p["participant"] == "flow-main"]
    many["room"]["participants"][0]["model"] = grid_models[0]
    many["attempts"]["active_recent"] = [a for a in many["attempts"]["active_recent"] if a["panel"]["role"] != "main"]
    order = ["flow-main"] + ["grid-%s" % n for n in range(1, grid_count)]
    for n, ident in enumerate(order):
        if n:
            many["room"]["participants"].append({"participant": ident, "role": "main", "joined": True,
                "provider": "claude" if "claude" in grid_models[n] else "codex", "model": grid_models[n], "label": "Grid chat %s" % n})
        many["attempts"]["active_recent"].append({"attempt_id": "flow-attempt" if n == 0 else ident, "state": "working",
            "panel": {"room_id": "flow-room", "room_participant": ident, "role": "main"}})
    shown_order = list(reversed(order))  # F6/F7 window order, not join order
    many["main_windows"] = {ident: n + 1 for n, ident in enumerate(shown_order)}
    grid_nav = {"dismissed": True}
    grid_lines = render(many, "codex", 157, 60, view="graph", navigation=grid_nav).splitlines()
    title_rows = [l for l in grid_lines if "MAIN / " in l]
    assert [l.count("MAIN / ") for l in title_rows] == list(grid_shape), (grid_shape, "\n".join(grid_lines))
    lane_bands = [b for b in grid_nav["bands"] if b["name"].startswith("lane:")]
    assert [b["name"] for b in sorted(lane_bands, key=lambda b: (b["y1"], b["x1"]))] == ["lane:" + i for i in shown_order], lane_bands
    assert all(b["y1"] <= b["y2"] <= len(grid_lines) for b in lane_bands), (lane_bands, len(grid_lines))
    row_two = [b for b in lane_bands if b["y1"] > min(x["y1"] for x in lane_bands)]
    assert len(row_two) == grid_shape[1] and all(b["y1"] > max(x["y2"] for x in lane_bands if x not in row_two) for b in row_two), lane_bands
    assert ("chat", shown_order[-1]) in [h["action"] for h in grid_nav["hits"] if h["y"] >= row_two[0]["y1"]], grid_nav["hits"]
    one_row = render(many, "codex", 157, 24, view="graph", navigation={"dismissed": True}).splitlines()
    # Too short for two rows: today's single row (or, past five mains, the focused graph), never a grid.
    assert sum("MAIN / " in l for l in one_row) <= 1, "\n".join(one_row)
    assert max(l.count("MAIN / ") for l in one_row) == (grid_count if grid_count < 6 else 1), "\n".join(one_row)
# All is automatic, not a frozen list: a main that joins later is shown as well.
assert navigate_state["pinned"] is None
assert panel.refreshed_navigation({"room_id": "flow-room"}, flow, "*")["pinned"] is None
navigate_state["auto_pins"] = ["flow-main", "flow-other"]
panel.navigate(Path("."), ("pin", "flow-main"), {}, navigate_state, 80)
assert navigate_state["pinned"] == ["flow-other"]
assert json.dumps(flow, sort_keys=True) == flow_before
wide_flow = deepcopy(flow)
wide_flow["room"]["messages"] = [
    {"sender": "advisor", "targets": ["flow-main"], "pending_for": ["flow-main"], "text": "CHECK FIXTURE FIRST"},
    {"sender": "advisor", "targets": ["flow-other"], "pending_for": ["flow-other"], "text": "PRIVATE OTHER MAIL"}]
wide_flow["room"]["pairs"] = [{"recipient": "flow-main", "pending": 1}]
wide_flow["room"]["participants"][0]["label"] = "Native task not recorded"
for columns in (139, 140, 141, 200):
    nav = {}
    drawn = render(wide_flow, "codex", columns, 44, view="graph", main_attempt="flow-attempt", navigation=nav)
    assert all(display_width(line) <= columns for line in drawn.splitlines()) and len(drawn.splitlines()) <= 44
    if columns >= 140:
        assert "WORK STATUS" in drawn and "MAIL / to main" in drawn and "CHECK FIXTURE FIRST" in drawn
        assert "no task title yet" in drawn and "Native task not recorded" not in drawn
        row = next(line for line in drawn.splitlines() if "MAIN / Sol 6" in line)
        assert "WORK STATUS" in row and "MAIL / to main" in row, row
        for hit in nav["hits"]:
            # Card hits stay inside the card; the full-width detail area is a separate target.
            if hit["action"] == ("chat", "flow-main") and hit["y"] > 6 and not hit.get("preview"):
                assert hit["x1"] > 1 and hit["x2"] < columns, hit
        # Other mains' mail must not be copied into the selected main's card.
        mailbox = "\n".join(line.split("MAIL / to main", 1)[-1] if "MAIL / to main" in line else
                            line[(columns + 58) // 2 + 2:] for line in drawn.splitlines()[:20])
        assert "PRIVATE OTHER MAIL" not in mailbox, mailbox
    else:
        assert "WORK STATUS" not in drawn
from panel_view import PALETTE
colored = render(wide_flow, "codex", 200, 44, view="graph", color=True)
# Each provider keeps its own hue, and a selection is a coloured block, never a bare white inversion.
assert PALETTE["worker"] in colored and PALETTE["codex"] in colored, colored
assert colored.count("\033[7m") == colored.count(PALETTE["main"] + "\033[7m") + colored.count(PALETTE["codex"] + "\033[7m") + sum(
    colored.count(PALETTE[k] + "\033[7m") for k in ("worker", "review", "alert", "bad", "dim")), colored
# Light terminal themes get darker shades of the same hues.
for theme_env, shade in (({"OMS_PANEL_THEME": "light"}, "166"), ({"COLORFGBG": "0;15"}, "166"),
                         ({"COLORFGBG": "15;0"}, "209"), ({"OMS_PANEL_THEME": "dark", "COLORFGBG": "0;15"}, "209")):
    picked = subprocess.run([sys.executable, "-c", "import panel_view; print(repr(panel_view.PALETTE['main']))"],
                            env=dict(os.environ, PYTHONPATH=str(Path(panel.__file__).parent), **theme_env),
                            capture_output=True, text=True, check=True).stdout
    assert "38;5;" + shade + "m" in picked, (theme_env, picked)
flow_publication = deepcopy(flow)
flow_publication["room"]["publications"] = {"claude": {"status": "submitted", "delivery_unknown": True}}
for columns, rows in ((76, 16), (100, 22), (100, 48)):
    for menu in (False, True):
        nav = {}
        picture = render(flow_publication, "codex", columns, rows, view="graph", menu=menu, navigation=nav)
        assert "APP DELIVERY" in picture and "submitted" in picture and "unconfirmed" in picture, picture
        assert len(picture.splitlines()) <= rows - int(menu) and all(display_width(line) <= columns for line in picture.splitlines())
navigation = {}
connected = render(tree_room, "codex", 91, 99, view="tree", navigation=navigation)
assert connected.count("Claude |") == connected.count("Codex |") == 1, connected
assert "Third main" in connected and "연결 근거 없는 작업" in connected and "Calls with no known main" in connected
assert "No joined worker" not in connected and "participants outside graph" not in connected
assert len([i for i in navigation["items"] if i[0] == "chat"]) == 4, navigation
target = next(hit for hit in navigation["hits"] if hit["action"][0] == "result")
assert choose(("click", 8, target["y"]), navigation) == target["action"]
assert choose(("click", 92, target["y"]), navigation) is None
assert choose(("click", 1, 100), navigation) is None
navigation["selected"] = ("result", "gone")
assert choose(("enter",), navigation) is None
owner_hit = next(hit for hit in navigation["hits"] if hit.get("fold"))
assert choose(("click", 1, owner_hit["y"]), navigation) == ("fold", owner_hit["action"][1])
navigation["collapsed"] = {owner_hit["action"][1]}
folded = render(tree_room, "codex", 91, 99, view="tree", navigation=navigation)
assert "calls" in folded and owner_hit["action"] in navigation["items"]
for columns, rows in ((91, 99), (42, 24), (28, 16), (28, 12), (2, 8), (1, 1)):
    for ascii_only in (False, True):
        for menu in (False, True):
            nav = {}
            drawn = render(tree_room, "codex", columns, rows, view="tree", unicode=not ascii_only,
                           menu=menu, navigation=nav)
            assert len(drawn.splitlines()) <= rows - int(menu), drawn
            assert all(display_width(line) <= columns for line in drawn.splitlines()), drawn
            assert all(1 <= hit["y"] <= len(drawn.splitlines()) for hit in nav["hits"])
            if ascii_only:
                assert "─" not in drawn and "│" not in drawn and "▾" not in drawn
            choose(("end",), nav)
            resized = render(tree_room, "codex", columns, rows, view="tree", navigation=nav)
            if nav["viewport"] and nav["selected"]:
                assert any(h["action"] == nav["selected"] for h in nav["hits"]), (columns, rows, menu, nav, resized)
assert json.dumps(tree_room, sort_keys=True) == before_tree
# ASCII mode never emits a byte above 0x7F in the tree, the list or the menu (data labels aside).
plain_room = deepcopy(tree_room)
for member in plain_room["room"]["participants"]:
    member["label"] = "plain label"
for row in plain_room["attempts"]["active_recent"] + plain_room["attempts"]["recent"]:
    row.get("panel", {})["label"] = "plain label"
for columns, rows in ((120, 40), (100, 30), (80, 24), (54, 24), (42, 16)):
    for kind in ("tree", "list", "summary"):
        for menu in (False, True):
            drawn = render(plain_room, "codex", columns, rows, view=kind, unicode=False, menu=menu, navigation={})
            assert all(ord(ch) < 128 for ch in drawn), (columns, rows, kind, menu, [ch for ch in drawn if ord(ch) > 127], drawn)
# The tree speaks in words: usage card, unread scope, an old plan, and calls without lifecycle evidence.
words = deepcopy(tree_room)
words["acceptance"] = {"available": True, "total": 6, "counts": {}, "complete": False}
words["plan"] = {"idle_days": 44}
words["operations"] = {"recent": [{"kind": "call", "event_id": "w1", "selected_model": "gpt-6-sol", "tokens": 1100000}]}
speech = render(words, "codex", 120, 60, view="tree", navigation={})
assert "Old plan · idle 44 days · 0 of 6 tasks verified" in speech and "Usage limits" in speech, speech
assert "Model use, last 8 calls" in speech and "Sol 6 1.1m tokens ×1" in speech, speech
assert "unread for live participants" in speech, speech
for stale in ("PROVIDER LIMITS", "(sel)", " tok / ", "Plan:"):
    assert stale not in speech, (stale, speech)
assert "no record" in speech and "no lifecycle record" not in speech, speech
# Calls whose attempts left the projection take their end state from the result messages: finished above, running below.
def minutes_ago(minutes):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - minutes * 60))
ended = deepcopy(flow)
ended["room"]["call_results"] = {}
for n in range(14):
    ident = "old-%02d" % n
    ended["room"]["participants"].append({"participant": ident, "role": "worker", "model": "gpt-6-luna", "joined": True,
        "provider": "codex", "parent": "flow-attempt", "label": "Old job %02d" % n, "seq": 10 + n,
        "joined_at": minutes_ago(300 - n * 10)})
    ended["room"]["call_results"][ident] = {"exit": 1 if n == 0 else 0, "ts": minutes_ago(290 - n * 10), "seq": 100 + n}
ended["room"]["call_results"]["old-03"]["exit"] = None
ended_text = render(ended, "codex", 120, 120, view="tree", navigation={})
assert "status unknown" not in ended_text and "failed (exit 1)" in ended_text, ended_text
assert "Old job 03" in ended_text and ended_text.count("finished") == 12, ended_text
rows = [line for line in ended_text.splitlines() if ("Luna" in line or "Sonnet" in line) and ("finished" in line or "failed" in line or "running" in line or "no record" in line)]
order = [0 if "finished" in r else 2 if "running" in r else 1 for r in rows]
assert order == sorted(order) and order[0] == 0 and order[-1] == 2 and 1 in order, rows
# A call outside the projection takes its real state from room_attempts, ahead of any exit code.
gone = deepcopy(ended)
for ident in ("gone-review", "gone-blocked"):
    gone["room"]["participants"].append({"participant": ident, "role": "worker", "model": "gpt-6-luna", "joined": True,
        "provider": "codex", "parent": "flow-attempt", "label": "Gone " + ident, "seq": 40})
gone["room"]["call_results"]["gone-review"] = {"exit": 0, "ts": minutes_ago(5), "seq": 200}
gone["room_attempts"] = {"gone-review": {"state": "review", "attempt_id": "att-r", "updated_at": minutes_ago(4), "task_id": "task-r"},
                         "gone-blocked": {"state": "blocked", "attempt_id": "att-b", "updated_at": minutes_ago(4), "task_id": "task-b"}}
seen = {m["participant"]: m for m in graph_text.nodes(gone)}
assert (seen["gone-review"]["state"], seen["gone-review"]["attempt"], seen["gone-review"]["task_id"]) == ("review", "att-r", "task-r"), seen["gone-review"]
assert seen["gone-blocked"]["state"] == "blocked" and seen["old-01"]["state"] == "done", seen
# snapshot() links full-projection rows only on the room, participant and role identity, newest row winning.
from unittest.mock import patch
import room as room_module
def event(attempt, state, at, room_id="flow-room", who="gone-review", role="worker"):
    # The real ledger row shape: panel identity lives in refs, not in a "panel" mapping.
    return {"attempt_id": attempt, "state": state, "updated_at": at, "task_id": "t-" + attempt,
            "refs": {"panel_room_id": room_id, "panel_room_participant": who, "panel_role": role}}
listed = [event("a-old", "working", "2026-01-01T00:00:00Z"), event("a-new", "review", "2026-01-02T00:00:00Z"),
          event("a-room", "failed", "2026-01-03T00:00:00Z", room_id="other-room"),
          event("a-who", "failed", "2026-01-03T00:00:00Z", who="someone-else"),
          event("a-role", "failed", "2026-01-03T00:00:00Z", role="advisor")]
members = [{"participant": "gone-review", "role": "worker", "joined": True},
           {"participant": "flow-main", "role": "main", "joined": True}]
def fake_run(command, **kwargs):
    assert command[2:3] == [str(panel.ENTRY)] or "agent-events" in command, command
    return subprocess.CompletedProcess(command, 0, json.dumps(listed), "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.subprocess, "run", side_effect=fake_run):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert got["room_attempts"] == {"gone-review": {"state": "review", "attempt_id": "a-new",
    "updated_at": "2026-01-02T00:00:00Z", "task_id": "t-a-new"}}, got.get("room_attempts")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired("agent-events", 5)):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert "room_attempts" not in got and got["room"]["participants"] == members, got
ended_nav = {}
render(ended, "codex", 120, 120, view="tree", navigation=ended_nav)
shown = list(dict.fromkeys(h["action"][1] for h in sorted(ended_nav["hits"], key=lambda h: h["y"]) if h["action"][0] == "result"))
assert shown == [i[1] for i in ended_nav["items"] if i[0] == "result"] and shown.index("old-13") < shown.index("builder"), shown
team_lines, _ = graph_text.detail(next(m for m in graph_text.nodes(ended) if m["participant"] == "flow-main"), {}, 120,
    graph_text.nodes(ended), ended["room"], everyone={"flow-main": [m for m in graph_text.nodes(ended)
                                                                  if m.get("parent") == "flow-attempt"]})
team_rows = [line for line in team_lines if "Old job" in line or "한글 작업 제목 / builder" in line]
assert "finished" in team_rows[0] and "running" in team_rows[-1] and "failed (exit 1)" in team_rows[-3], team_rows
assert not any("status unknown" in line for line in team_lines) and "failed (exit 1)" in "\n".join(team_lines), team_lines
# Attempt evidence beats the result message, and the header clock carries date and weekday.
beaten = deepcopy(ended)
beaten["attempts"]["active_recent"].append({"attempt_id": "old-00", "state": "working",
    "panel": {"room_id": "flow-room", "room_participant": "old-00", "role": "worker"}})
assert next(m for m in graph_text.nodes(beaten) if m["participant"] == "old-00")["state"] == "working"
assert re.search(r"\d\d-\d\d (Mon|Tue|Wed|Thu|Fri|Sat|Sun) \d\d:\d\d$", graph_text.render_graph(flow, 120, 40).splitlines()[0])
board_text = graph_text.render_graph(words, 120, 40)
assert "Plan:" not in board_text, board_text
words["plan"] = {"idle_days": 3}
assert "Plan: 0 of 6 tasks verified" in graph_text.render_graph(words, 120, 40)
assert "Old plan" not in render(words, "codex", 120, 60, view="tree", navigation={})
decoder = TerminalInput()
assert decoder.decode(b"\033[<0;8;") == []
assert decoder.decode(b"7M\033[<0;8;7m\033[<65;8;7M\033[A\r") == [
    ("click", 8, 7), ("scroll", 3, 8, 7), ("up",), ("enter",)]
assert decoder.decode(b"\033[<32;1;1M") == []
assert percent(True) is None and percent(float("nan")) is None and percent(10**1000) is None
assert percent(0) == 0 and percent(100) == 100
hud = temporary / "panel-hud"
hud.mkdir()
consumer = "a" * 32
cache = hud / ("ctx-" + consumer[:24] + ".json")
cache.write_text(json.dumps({"schema": 1, "used_percentage": 25, "ts": 1000,
                            "seven_day": {"used_percentage": 0, "resets_at": 2000}}))
with patch.dict(os.environ, {"OMS_HUD_CACHE_DIR": str(hud)}):
    assert cached(consumer, 1000)["context_left"] == 75
    assert cached(consumer, 1000)["weekly_used"] == 0
    assert cached(consumer, 1601) == {"source": "cache stale"}
    assert cached(consumer, 999) == {}
    cache.write_text(json.dumps({"used_percentage": True, "ts": 1000,
        "seven_day": {"used_percentage": 50, "resets_at": 900}}))
    assert cached(consumer, 1000)["context_left"] is None and cached(consumer, 1000)["weekly_used"] is None
    cache.unlink()
    if os.name != "nt":
        cache.symlink_to(project / "value.txt")
        assert cached(consumer, 1000) == {}
with patch("panel_chats.windows", return_value={}), patch("panel_chats.native_index", side_effect=AssertionError("render scan")):
    reading = collect(tree_room)
    assert reading["codex"]["source"] == "main ambiguous", reading
    reading = collect(tree_room, main_attempt=main_attempts["codex"])
    assert reading["codex"]["participant"] == main_attempts["codex"], reading
with patch("panel_metrics.subprocess.run", side_effect=[
        subprocess.CompletedProcess([], 0, "99\t0\n"),
        subprocess.CompletedProcess([], 0, "GPT-6-Sol · Context 83% left · weekly 61% left\n")]) as pane_probe:
    assert pane_reading({"pane": "%42"}, "codex") == {"context_left": 83, "weekly_used": 39, "source": "TUI"}
    assert pane_probe.call_args.args[0][-4:] == ["-S", "91", "-E", "98"]
# The smaller task panel preserves frame-local navigation and never upgrades
# native process exit to owner acceptance.
summary_room = deepcopy(tree_room)
for attempt in summary_room["attempts"]["active_recent"] + summary_room["attempts"]["recent"]:
    if attempt.get("panel", {}).get("role") == "main":
        attempt["state"] = "done"
for columns, rows in ((32, 28), (80, 24), (2, 8)):
    nav = {}
    compact_panel = render(summary_room, "codex", columns, rows, view="summary", navigation=nav)
    assert len(compact_panel.splitlines()) <= rows
    assert all(display_width(line) <= columns for line in compact_panel.splitlines())
    assert all(hit["y"] <= len(compact_panel.splitlines()) for hit in nav["hits"])
assert "exited" in render(summary_room, "codex", 120, 99, view="summary")
usage_board = deepcopy(summary_room)
usage_board["operations"] = {"recent": [{"kind": "call", "provider": "codex", "event_id": model,
    "served_model": model, "tokens": 10} for model in ("gpt-6-sol", "gpt-6-astra", "gpt-6-luna")]}
small_board = render(usage_board, "codex", 32, 28, view="summary")
expanded_board = render(usage_board, "codex", 100, 28, view="tree")
assert "+1 models" in small_board and "Luna 6 10 tok" in expanded_board

# The screenshot's room/main/plan screen has three closed sections, no duplicate
# usage or empty scopes, and titles wrap without losing their navigation target.
boxed_room = deepcopy(usage_board)
boxed_room["room"]["repo_tasks"] = [{"id": "layout", "state": "ready",
    "title": "Keep a long task title readable when the terminal has fewer columns than its description"}]
for columns, rows in ((108, 36), (100, 28), (42, 24), (32, 28)):
    nav = {}
    screen = render(boxed_room, "codex", columns, rows, view="tree", navigation=nav)
    assert screen.count("╭") == screen.count("╰") == 2, screen
    assert screen.count("Model use, last 8 calls") == 1 and "USAGE recent8" not in screen, screen
    assert "DECLARED SCOPES" not in screen, screen
    assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines()), screen
    choose(("end",), nav)
    scrolled = render(boxed_room, "codex", columns, rows, view="tree", navigation=nav)
    assert scrolled.count("╭") == scrolled.count("╰") == 2, scrolled
    assert any(hit["action"] == ("task", "layout") for hit in nav["hits"]), nav
assert "W 100% / C 0%" in render(dict(usage_board, provider_status={"claude": {
    "weekly_used": 100, "context_left": 0}}), "claude", 32, 28, view="summary")
empty_room = dict(usage_board, room={"id": "empty", "participants": []})
# The control menu is an inbox: empty rooms onboard, failures and waits list under "Needs you", one key acts.
onboarding = render(empty_room, "codex", 100, 28, view="tree", menu=True)
assert "Press 1 (Codex) or 2 (Claude) to start a main" in onboarding and "Nothing needs you right now" in onboarding
assert "[q] Detach" in render(empty_room, "codex", 100, 28, view="tree", menu=True, managed=True)
inbox_room = {"repo": {"name": "r"}, "collection": {"ok": True}, "finalized": {}, "awaiting_admission": ["w3"],
    "room": {"id": "inbox-room", "title": "Shared", "participants": [
        {"participant": "m1", "role": "main", "joined": True, "provider": "claude", "model": "claude-opus-5-5", "seq": 1},
        {"participant": "w1", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 2, "parent": "m1"},
        {"participant": "w2", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 3, "parent": "m1"},
        {"participant": "w3", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 4, "parent": "m1"}],
        "messages": [{"id": "n1", "sender": "m1", "recipient": "all", "message_kind": "question", "text": "Which branch?\x1b[2J",
                      "pending_for": ["w1"], "answered": False, "ts": "2026-10-07T00:00:00Z"}]},
    "attempts": {"available": True, "recent": [], "active_recent": [
        {"attempt_id": "a1", "state": "working", "panel": {"role": "main", "room_participant": "m1", "room_id": "inbox-room"}},
        {"attempt_id": "a2", "state": "failed", "updated_at": "2026-10-07T00:01:00Z",
         "panel": {"role": "worker", "room_participant": "w1", "room_id": "inbox-room", "label": "Fix parser"}},
        {"attempt_id": "a3", "state": "waiting_approval", "updated_at": "2026-10-07T00:02:00Z",
         "panel": {"role": "worker", "room_participant": "w2", "room_id": "inbox-room", "label": "Run migration"}},
        {"attempt_id": "a4", "state": "done", "updated_at": "2026-10-07T00:00:30Z",
         "panel": {"role": "worker", "room_participant": "w3", "room_id": "inbox-room", "label": "Write patch"}}]}}
nav = {}
inbox = render(inbox_room, "codex", 100, 28, menu=True, navigation=nav)
assert "Needs you" in inbox and "Nothing needs you" not in inbox and "Press 1 (Codex)" not in inbox, inbox
assert "Sol 6.1 worker for Opus 5.5 · failed: Fix parser" in inbox, inbox
assert "waiting for approval: Run migration" in inbox and "patch awaits admission: Write patch" in inbox, inbox
assert "Opus 5.5 main · to all: Which branch?" in inbox and "\033" not in inbox, inbox
assert [h["action"] for h in nav["hits"]] == [("result", "w2"), ("result", "w1"), ("result", "w3"), ("chat", "m1")], nav
assert nav["items"][0] == ("result", "w2") and nav["selected"] == ("result", "w2")
choose(("down",), nav)
assert nav["selected"] == nav["items"][1]
for width, height in ((100, 28), (42, 24), (24, 18), (12, 10)):
    for glyphs in (True, False):
        drawn = render(inbox_room, "codex", width, height, unicode=glyphs, menu=True, navigation={})
        assert len(drawn.splitlines()) <= height - 1 and all(display_width(line) <= width for line in drawn.splitlines()), drawn
        closed_cards(drawn, width, glyphs)
assert "key + Enter" in render(inbox_room, "codex", 100, 28, menu=True, navigation={"line_input": True})
# A main's close request is a row only the person acts on; ASCII mode stays ASCII and the placeholder replaces it.
close_room = dict(inbox_room, main_windows={"m1": 3, "m0": 1})
close_room["room"] = dict(inbox_room["room"], participants=inbox_room["room"]["participants"] + [
    {"participant": "m0", "role": "main", "joined": True, "provider": "claude", "model": "claude-opus-5-5", "seq": 0}],
    messages=inbox_room["room"]["messages"] + [{"id": "c1", "sender": "m0", "recipient": "m1", "message_kind": "question",
    "text": "Close requested: done\x1b[2J here", "pending_for": ["m1"], "answered": False, "ts": "2026-10-07T05:20:00Z"}])
for glyphs in (True, False):
    nav = {"closed_note": "Closed 14:20 by the person"}
    drawn = render(close_room, "codex", 120, 30, unicode=glyphs, menu=True, navigation=nav)
    sep = " · " if glyphs else " / "
    assert ("Close #3 Opus 5.5" + sep + "requested by #1 Opus 5.5" + sep + "done?[2J here" + sep + "running calls 0" + sep + "unread 0") in drawn, drawn
    assert ("Closed 14:20 by the person" in drawn and "\033" not in drawn and (glyphs or drawn.isascii())), drawn
    assert ("close", "m1") in nav["items"], nav
assert panel.menu_input().decode(b"1\033[B2q ") == [("1",), ("down",), ("2",), ("q",), (" ",)]
assert TerminalInput().decode(b"v\033[A") == [("v",), ("up",)]
# Keys the board does not bind are ignored, never read as Esc; Home/End work in every encoding tmux sends.
for sequence, expected in ((b"\033[1~", "home"), (b"\033[4~", "end"), (b"\033[7~", "home"), (b"\033[8~", "end"),
                           (b"\033OH", "home"), (b"\033OF", "end")):
    assert TerminalInput().decode(sequence) == [(expected,)], sequence
for sequence in (b"\033[Z", b"\033[3~", b"\033[15~", b"\033[1;5C", b"\033OP"):
    ignored = TerminalInput()
    assert ignored.decode(sequence) == [] and ignored.pending == b"", sequence
assert TerminalInput().decode(b"\033[3~q") == [("q",)]
for partial, expected in ((b"\033", [("escape",)]), (b"\033[1;5", []), (b"\033O", [])):
    timed = TerminalInput()
    timed.fd = 0
    assert timed.decode(partial) == [] and timed.escape_since is not None
    timed.escape_since -= 1
    with patch("panel_input.select.select", return_value=([], [], [])):
        assert timed.wait(0) == expected and timed.pending == b"" and timed.escape_since is None, partial
assert TerminalInput().decode(b"?") == [("?",)]
# A transient snapshot failure keeps the board's folds, selection and view; only another room resets them.
held = {"room_id": "flow-room", "selected": ("chat", "m1"), "collapsed": {"m1"}, "offset": 4, "view": "graph", "attention": True}
transient = {"repo": {"name": "x"}, "collection": {"ok": False}}
assert panel.refreshed_navigation(dict(held), transient, None) == held
assert panel.refreshed_navigation(dict(held), {"room": {"id": "flow-room"}}, None) == held
assert panel.refreshed_navigation(dict(held), {"room": {"id": "other-room"}}, None) == {"collapsed": set(), "offset": 0}
# An open preview re-reads evidence only when the room moved.
previewed_nav = {"room_id": "flow-room", "preview": {"target": ("result", "c1"), "report": {}}}
moved = {"room": {"id": "flow-room", "message_count": 1}}
assert panel.refreshed_navigation(previewed_nav, moved, None)["preview"].pop("refresh") is True
assert "refresh" not in panel.refreshed_navigation(previewed_nav, moved, None)["preview"]
assert panel.refreshed_navigation(previewed_nav, {"room": {"id": "flow-room", "message_count": 2}}, None)["preview"]["refresh"]
# A failed evidence read still names its target, so the error cannot land on another preview.
with patch.object(panel, "call_results", side_effect=subprocess.TimeoutExpired("agent-events", 30)):
    assert panel.read_evidence(project, ("result", "c1"))[0] == ("result", "c1")
    assert panel.read_evidence(project, ("result", "c1"))[2]
# Notices age out after ten seconds.
aging = {"notice": "Opening chat..."}
assert panel.expire_notice(aging, 100.0) is False and panel.expire_notice(aging, 109.0) is False
assert panel.expire_notice(aging, 110.0) is True and "notice" not in aging
aging["notice"] = "again"
assert panel.expire_notice(aging, 111.0) is False and aging["notice"] == "again"
from panel_tree import render_detail
long_answer = "\n".join("Answer line %s" % n for n in range(40)) + "\n" + "x" * 900 + " END_OF_ANSWER"
long_result = {"rows": [{"task_id": "long-result", "summary": "Owner summary",
    "calls": [{"kind": "call", "exit": 0, "answer": long_answer,
               "artifact": ".oms/artifacts/long-answer.md"}]}]}
assert "END_OF_ANSWER" in render_results(long_result, 100)
detail = {"title": "RECORDED RESULTS", "report": long_result}
for width in (100, 28, 60):
    screen = render_detail(tree_room, detail, width, 200, {})
    assert "Answer line 39" in screen and "END_OF_ANSWER" in screen, screen
    assert all(display_width(line) <= width for line in screen.splitlines()), screen
limited_result = deepcopy(long_result)
limited_result["rows"][0]["calls"][0]["answer_truncated"] = True
assert "retained preview limit" in render_results(limited_result, 100)
answer_attempt = {"attempt_id": "att_long_answer", "task_id": "retained-long-answer", "state": "done",
                  "refs": {"panel_room_participant": "long-reader"}}
answer_record = {"kind": "call", "task_id": "retained-long-answer", "attempt_id": "att_long_answer", "exit": 0,
                 "artifact": ".oms/artifacts/long-answer.md"}
sample_auth = "gh" + "p_" + "a" * 25
for payload, truncated in ((long_answer + "\n" + sample_auth + "\n" + "z" * 9000 + " API_ANSWER_TAIL", False),
                           ("x" * 40000 + " API_ANSWER_TAIL", True)):
    with patch.object(saved, "_records", return_value=([answer_attempt], [answer_record])), \
            patch.object(saved, "_indexed", return_value=("## Output\n\n" + payload +
                "\n\n## Verify\nPRIVATE_VERIFIER_SENTINEL\n\n## Exit\n\n0\n").encode()):
        normalized = saved.results(project, "retained-long-answer")["rows"][0]["calls"][0]
    assert normalized["answer_truncated"] == truncated, normalized
    assert len(normalized["answer"].encode()) <= saved.MAX_RESULT
    assert sample_auth not in normalized["answer"]
    assert "PRIVATE_VERIFIER_SENTINEL" not in normalized["answer"]
    if not truncated:
        assert "Answer line 39" in normalized["answer"] and "API_ANSWER_TAIL" in normalized["answer"]
binding = panel.room_instructions("current-room", "stable-main")
assert "room show --id current-room" in binding and "--room current-room" in binding
assert "room updates --id current-room --participant stable-main" in binding
assert "--id current-room --participant stable-main --message MESSAGE_ID" in binding
standalone_env = {key: value for key, value in environment.items()
                  if key not in {"OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_PANEL_MAIN_ATTEMPT"}}
with patch.dict(os.environ, standalone_env, clear=True), patch.object(panel.subprocess, "call", return_value=0), \
        patch("sys.stderr", new=io.StringIO()) as standalone_notice:
    assert panel.dispatch(project, "codex", "worker", "light", "auto", "read", "investigate",
                          prompt="Inspect source", task_id="standalone-fixture") == 0
    assert "room and main relationship are unrecorded" in standalone_notice.getvalue()
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "inline", "--dry-run"]))["host"] == "inline"
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "herdr", "--dry-run"]))["host"] == "herdr"

# Exercise the real bounded JSON socket client without installing a host or
# running agents; lifecycle and room admission use the canonical OMS tools.
if os.name == "posix":
    import panel_host
    import panel_chats
    import socket
    import threading
    socket_path = temporary / "herdr.sock"
    host_pane = {"pane_id": "w1:p1", "terminal_id": "term_fixture", "workspace_id": "w1",
                 "tab_id": "w1:t1", "cwd": str(project.resolve()), "agent": "codex"}
    host_panes = {"w1:p1": host_pane, "w1:p2": dict(host_pane, pane_id="w1:p2", terminal_id="term_other")}
    pane_errors = set()
    host_calls, host_fault = [], []
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(socket_path))
    server.listen()
    server.settimeout(.1)
    stopped = threading.Event()
    def serve_host():
        while not stopped.is_set():
            try:
                connection, _ = server.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            with connection:
                connection.settimeout(2)
                data = bytearray()
                while b"\n" not in data:
                    data.extend(connection.recv(65536))
                message = json.loads(bytes(data).split(b"\n", 1)[0])
                host_calls.append(message)
                result = ({"pane": dict(host_panes[message["params"]["pane_id"]])} if message["method"] == "pane.get" else
                          {"read": {"pane_id": "w1:p1", "text": "Context 83% left / weekly 61% left"}}
                          if message["method"] == "pane.read" else {})
                reply = {"id": "wrong" if host_fault else message["id"], "result": result}
                if message["method"] == "pane.get" and message["params"]["pane_id"] in pane_errors:
                    reply = {"id": message["id"], "error": {"message": "closed pane"}}
                connection.sendall(json.dumps(reply).encode() + b"\n")
    host_thread = threading.Thread(target=serve_host, daemon=True)
    host_thread.start()
    try:
        host_env = dict(environment, HERDR_ENV="1", HERDR_SOCKET_PATH=str(socket_path),
                        HERDR_PANE_ID="w1:p1", OMS_PANEL_HOST="herdr")
        host_room = room.create(project, "herdr-room", "Host fixture")
        host_env.update(OMS_ROOM_ID=host_room, OMS_ROOM_REPO=str(project))
        with patch.dict(os.environ, host_env, clear=True):
            assert panel_host.current(project)["terminal_id"] == "term_fixture"
            host_ref = panel_host.refs(project)
            host_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-main",
                *[arg for key, value in host_ref.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            room.join(project, host_room, "herdr-main", "codex", label="Herdr main")
            host_state = room.status(project, host_room)
            destination = panel_host.targets(project, host_room, host_state)["herdr-main"][0]
            assert destination["host"] == "herdr" and destination["attempt"] == host_attempt
            assert pane_reading(destination, "codex")["weekly_used"] == 39
            with patch("panel_chats.native_index", return_value={}):
                before_focus = len(host_calls)
                planned = panel_chats.open_chat(project, host_room, "herdr-main", dry_run=True)
                assert planned["host"] == "herdr" and not any(c["method"] == "pane.focus" for c in host_calls[before_focus:])
                assert panel_chats.open_chat(project, host_room, "herdr-main")["status"] == "navigation_requested"
            panel.open_native("codex", project)
            layout = next(c["params"] for c in reversed(host_calls) if c["method"] == "layout.apply")
            assert "tab_id" not in layout and layout["root"]["ratio"] == .5
            assert layout["root"]["direction"] == "down" and "--watch" in layout["root"]["first"]["command"]
            main_command = layout["root"]["second"]["command"]
            assert str(panel.ENTRY.parent / "panel.sh") in main_command and "--launch" in main_command
            assert layout["root"]["first"]["env"]["OMS_ROOM_ID"] == host_room
            assert layout["root"]["first"]["env"]["OMS_PANEL_VIEW"] == "graph"
            with patch.dict(os.environ, {"OMS_PANEL_POSITION": "side"}):
                panel.open_native("codex", project)
                side_layout = next(c["params"] for c in reversed(host_calls) if c["method"] == "layout.apply")
                assert side_layout["root"]["direction"] == "right" and side_layout["root"]["ratio"] == .74
                assert "--launch" in side_layout["root"]["first"]["command"]
            assert not any(c["method"] in {"pane.send_text", "pane.send_input", "server.stop"} for c in host_calls)
            other_refs = dict(host_ref, panel_pane_id="w1:p2", panel_terminal_id="term_other")
            other_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-other",
                *[arg for key, value in other_refs.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            room.join(project, host_room, "herdr-other", "codex")
            host_state = room.status(project, host_room)
            assert set(panel_host.targets(project, host_room, host_state)) == {"herdr-main", "herdr-other"}
            pane_errors.add("w1:p2")
            assert set(panel_host.targets(project, host_room, host_state)) == {"herdr-main"}
            panel_host.verify(destination)
            duplicate_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-main",
                *[arg for key, value in other_refs.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            assert panel_host.targets(project, host_room, host_state) == {}, "failed duplicate established unique ownership"
            panel.events(project, "transition", "--attempt", duplicate_attempt, "--state", "cancelled")
            panel.events(project, "transition", "--attempt", other_attempt, "--state", "cancelled")
            pane_errors.clear()
            host_pane["terminal_id"] = "term_replaced"
            assert panel_host.targets(project, host_room, host_state) == {}
            try:
                panel_host.focus(destination)
            except ValueError:
                pass
            else:
                raise AssertionError("reused pane focused another terminal")
            host_pane["terminal_id"] = "term_fixture"
            host_pane["agent_session"] = {"agent": "claude", "value": "foreign-session"}
            assert panel_host.targets(project, host_room, host_state) == {}
            host_pane.pop("agent_session")
            panel.events(project, "transition", "--attempt", host_attempt, "--state", "cancelled")
            assert panel_host.targets(project, host_room, host_state) == {}
            host_fault.append(True)
            try:
                panel_host.current(project)
            except ValueError:
                pass
            else:
                raise AssertionError("wrong response id was accepted")
    finally:
        stopped.set()
        server.close()
        host_thread.join(timeout=2)
        socket_path.unlink()
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(room, "status", return_value=tree_room["room"]), \
        patch("panel_chats.open_chat", return_value={"method": "existing-terminal"}) as opened:
    nav = {"room_id": shared}
    panel.navigate(project, ("chat", main_attempts["codex"]), tree_room, nav, 91)
    assert opened.call_args.kwargs == {"allowed_methods": {"existing-terminal", "app-uri", "windows-uri"}}, opened.call_args
    assert nav["notice"] == "Chat navigation requested in its window"
    left = deepcopy(tree_room["room"])
    for member in left["participants"]:
        member["joined"] = False
    with patch.object(room, "status", return_value=left):
        try:
            panel.navigate(project, ("chat", main_attempts["codex"]), tree_room, nav, 91)
        except ValueError:
            pass
        else:
            raise AssertionError("left participant was navigated")
    assert opened.call_count == 1
with patch("panel_chats.plan", return_value={"method": "claude-desktop"}), \
        patch("panel_chats.subprocess.run") as launched:
    import panel_chats
    try:
        panel_chats.open_chat(project, shared, main_attempts["claude"], allowed_methods={"existing-terminal", "app-uri", "windows-uri"})
    except ValueError:
        pass
    else:
        raise AssertionError("tree navigation started a desktop CLI")
    launched.assert_not_called()
if os.name != "nt":
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    try:
        with os.fdopen(os.dup(slave), "r") as input_tty, os.fdopen(os.dup(slave), "w") as output_tty, \
                patch.object(sys, "stdin", input_tty), patch.object(sys, "stdout", output_tty), \
                patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-panel-abcdef012345"}), \
                patch("panel_input.subprocess.run") as mouse_setting:
            with TerminalInput(active=False) as passive:
                assert passive.fd is None and termios.tcgetattr(slave) == original
            mouse_setting.assert_not_called()
            # A failed tmux mouse setting is best effort: the watcher keeps its raw keyboard input.
            mouse_setting.side_effect = subprocess.TimeoutExpired("tmux", 2)
            with TerminalInput(managed=True) as degraded:
                assert degraded.fd is not None and not termios.tcgetattr(slave)[3] & termios.ICANON
            assert termios.tcgetattr(slave) == original
            mouse_setting.side_effect = None
            mouse_setting.reset_mock()
            try:
                with TerminalInput(managed=True) as listener:
                    assert listener.fd is not None
                    changed = termios.tcgetattr(slave)
                    assert not changed[3] & termios.ICANON and not changed[3] & termios.ECHO
                    assert changed[3] & termios.ISIG == original[3] & termios.ISIG
                    os.write(master, b"\033[<0;8;7M")
                    assert listener.wait(.2) == [("click", 8, 7)]
                    raise KeyboardInterrupt
            except KeyboardInterrupt:
                pass
            assert termios.tcgetattr(slave) == original
            argv = mouse_setting.call_args.args[0]
            assert argv == ["tmux", "set-option", "-t", "oms-panel-abcdef012345", "mouse", "on"]
        output = b""
        deadline = time.monotonic() + 1
        while b"\033[?1000l" not in output and time.monotonic() < deadline:
            if select.select([master], [], [], max(0, deadline - time.monotonic()))[0]:
                output += os.read(master, 4096)
        assert b"\033[?1000h" in output and b"\033[?1000l" in output, output
    finally:
        os.close(master)
        os.close(slave)
    actual = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--json"], room_env))["dashboard"]
    nav = {}
    render(actual, "codex", 110, 28, view="tree", navigation=nav)
    hit = next(h for h in nav["hits"] if h["action"][0] == "result")
    # The result opens from a background read; the watcher exits on its own after three snapshots, so a slow background read still lands.
    input_bytes = ("\033[<0;8;%sM" % hit["y"]).encode()
    before_click = ledger.read_bytes()
    terminal(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
              "--watch", "--view", "tree", "--count", "3", "--no-animation"],
             input_bytes, b"RECORDED RESULTS", trigger=b"Activity", timeout=60)
    assert ledger.read_bytes() == before_click, "clicking a result wrote lifecycle state"
    import signal
    for signum in (signal.SIGHUP, signal.SIGTERM):
        terminal(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                  "--watch", "--view", "tree", "--count", "2", "--no-animation"], b"", b"\033[?1000l",
                 trigger=b"Activity", signal_after=signum, expected_exit=128 + signum)
# Publishing uses the saved receiver contract and persists intent before the call.
with patch.object(codex_app_notify, "deliver", return_value={"status": "persisted", "turn_id": "fixture-app-turn",
        "delivery_unknown": False}) as delivered:
    output = room.publish(project, shared)
    assert output["receipt"]["status"] == "persisted"
    assert room.publish(project, shared)["deduplicated"] and delivered.call_count == 1
room.send(project, shared, room_owner, main_attempts["codex"], "New bounded result")
with patch.object(codex_app_notify, "deliver", return_value={"status": "timeout", "delivery_unknown": True}) as delivered:
    room.publish(project, shared)
    assert room.publish(project, shared)["deduplicated"] and delivered.call_count == 1
    room.publish(project, shared, retry=True)
    assert delivered.call_count == 2
assert room.status(project, shared)["publication"]["status"] == "timeout"
# Native Claude app/terminal delivery has its own receiver and durable intent.
import hashlib
import claude_app_notify
room.join(project, shared, "opus-app", "claude", native_session="fixture-claude-app", label="Opus app")
before = room.records(project, shared)
with patch.object(claude_app_notify, "deliver", side_effect=AssertionError("wakeup was not authorized")):
    blocked = room.publish(project, shared, app="claude", recipient="opus-app")
    assert blocked["receipt"]["status"] == "wakeup_required"
    assert room.records(project, shared) == before
blocked = json.loads(call(["bash", str(panel.ENTRY), "room", "publish", "--repo", str(project),
                          "--id", shared, "--app", "claude", "--to", "opus-app"]))
assert blocked["receipt"]["status"] == "wakeup_required"
def claude_post(*args):
    pending = room.status(project, shared)["publications"]["claude.opus-app"]
    assert pending["status"] == "pending" and pending["delivery_unknown"], pending
    assert args[2] == hashlib.sha256(b"fixture-claude-app").hexdigest()[:32] and args[3] is True
    return {"status": "submitted", "submitted": True, "delivery_unknown": True}
with patch.object(claude_app_notify, "deliver", side_effect=claude_post) as delivered:
    sent = room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)
    assert sent["target"] == "claude.opus-app" and sent["receipt"]["status"] == "submitted"
    room.send(project, shared, room_owner, "opus-app", "New result must not silently resend an uncertain app post")
    assert room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)["deduplicated"]
    assert delivered.call_count == 1
    room.publish(project, shared, retry=True, app="claude", recipient="opus-app", allow_wakeup=True)
    assert delivered.call_count == 2
with patch.object(codex_app_notify, "deliver", side_effect=AssertionError("Codex uncertainty must remain independent")):
    assert room.publish(project, shared)["deduplicated"]
for recipient in (main_attempts["claude"], main_attempts["codex"], "missing"):
    with patch.object(claude_app_notify, "deliver", side_effect=AssertionError("unbound receiver")):
        try:
            room.publish(project, shared, app="claude", recipient=recipient, allow_wakeup=True)
        except ValueError:
            pass
        else:
            raise AssertionError("unbound or foreign app receiver accepted")
with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
    try:
        room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)
    except ValueError:
        pass
    else:
        raise AssertionError("worker could publish")
# The current shortcut stores a native session hash, never the exported token/socket.
with patch.object(claude_app_notify, "current_session", return_value="fixture-current-app"):
    bound = room.join(project, shared, "current-app", "claude", native_session="current")
assert bound["consumer"] == hashlib.sha256(b"fixture-current-app").hexdigest()[:32]
assert "socket" not in bound and "token" not in bound
room_view["room"] = room.status(project, shared)
for view in ("graph", "compact"):
    drawn = render(room_view, "claude", 100, 48, view=view)
    assert "APP DELIVERY" in drawn and "submitted" in drawn, drawn
    assert all(display_width(line) <= 100 for line in drawn.splitlines()) and len(drawn.splitlines()) <= 48
# Old target-less receipts continue to protect the original Codex receiver.
legacy_app = room.create(project, "legacy-app-room", "Legacy delivery")
intent = {"kind": "publication", "id": "old-app-intent", "digest": "a" * 64,
          "status": "pending", "delivery_unknown": True}
room.append(project, legacy_app, intent, "Old app intent")
room.append(project, legacy_app, dict(intent, status="timeout"), "Old app receipt")
assert room.status(project, legacy_app)["publications"]["codex"]["status"] == "timeout"
with patch.object(codex_app_notify, "deliver", side_effect=AssertionError("old logs must not resend")):
    assert room.publish(project, legacy_app)["deduplicated"]
# Native chat lookup is explicit, private and read-only. A malformed enrolled
# record must not hide a different valid chat in the same directory.
import panel_chats
chat_codex = "11111111-1111-4111-8111-111111111111"
bad_codex = "22222222-2222-4222-8222-222222222222"
chat_claude = "33333333-3333-4333-8333-333333333333"
for who, provider, native in (("chat-sol", "codex", chat_codex), ("bad-sol", "codex", bad_codex),
                               ("chat-opus", "claude", chat_claude)):
    room.join(project, shared, who, provider)
    room.bind(project, shared, who, native)
codex_home = home / "chat-codex"
codex_day = codex_home / "sessions/2026/10/05"
codex_day.mkdir(parents=True)
(codex_day / ("rollout-x-" + bad_codex + ".jsonl")).write_text("bad metadata\n")
chat_file = codex_day / ("rollout-x-" + chat_codex + ".jsonl")
chat_file.write_text(json.dumps({"type": "session_meta", "payload": {"id": chat_codex, "cwd": str(project)}})
                     + "\nPRIVATE_TRANSCRIPT_BODY_MUST_NOT_BE_READ_OR_SHARED\n")
claude_home = home / "chat-claude"
claude_project = claude_home / "projects/local-project"
claude_project.mkdir(parents=True)
(claude_project / (chat_claude + ".jsonl")).write_text(json.dumps({"type": "user", "sessionId": chat_claude}) + "\n")
chat_env = dict(environment, CODEX_HOME=str(codex_home), CLAUDE_CONFIG_DIR=str(claude_home),
                OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project), TMUX="fixture-tmux", TMUX_PANE="%1")
room_before_chats = room.records(project, shared)
navigation_calls = []
chat_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + shared,
    "--ref", "panel_room_participant=chat-sol", "--then", "starting", "--then", "working", output=True)
navigation_run = subprocess.run
current_session = ["$1"]
client_listing = ["fixture-client\t$2\n"]
def tmux_navigation(command, **kwargs):
    if command[0] == "bash":
        return navigation_run(command, **kwargs)
    if command[1] == "list-windows":
        return subprocess.CompletedProcess(command, 0, "@42\t" + chat_attempt + "\t" + shared + "\t%42\tchat-sol\t" + str(project.resolve()) + "\t$1\n"
            + "@99\tchat-opus\tforeign-room\t%99\tchat-opus\t" + str(project.resolve()) + "\t$2\n", "")
    if command[1] == "list-panes":
        return subprocess.CompletedProcess(command, 0, "%42\t@42\n%99\t@99\n", "")
    if command[1] == "display-message":
        return subprocess.CompletedProcess(command, 0, current_session[0] + "\n", "")
    if command[1] == "list-clients":
        return subprocess.CompletedProcess(command, 0, client_listing[0], "")
    navigation_calls.append(command)
    return subprocess.CompletedProcess(command, 0, "", "")
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/launcher"), \
        patch.object(panel_chats.subprocess, "run", side_effect=tmux_navigation):
    chats = panel_chats.catalog(project, shared)
    assert "PRIVATE_TRANSCRIPT_BODY" not in json.dumps(chats)
    native = next(r for r in chats["rows"] if r["participant"] == "chat-sol")
    assert native["uri"] == "codex://threads/" + chat_codex and native["terminal"]["pane"] == "%42", native
    assert next(r for r in chats["rows"] if r["participant"] == "bad-sol")["status"] == "unresolved"
    assert next(r for r in chats["rows"] if r["participant"] == "chat-opus")["session_id"] == chat_claude
    assert next(r for r in chats["rows"] if r["participant"] == child["participant"])["status"] == "artifacts"
    dry = panel_chats.open_chat(project, shared, "chat-sol", dry_run=True)
    assert dry["method"] == "existing-terminal" and not navigation_calls
    opened = panel_chats.open_chat(project, shared, "chat-sol")
    assert opened["status"] == "navigation_requested" and not opened["ui_observed"]
    assert navigation_calls == [["tmux", "select-window", "-t", "@42"], ["tmux", "select-pane", "-t", "%42"]]
    current_session[0] = "$2"
    navigation_calls.clear()
    assert panel_chats.open_chat(project, shared, "chat-sol")["status"] == "navigation_requested"
    assert navigation_calls == [["tmux", "select-window", "-t", "@42"], ["tmux", "select-pane", "-t", "%42"],
                                ["tmux", "switch-client", "-c", "fixture-client", "-t", "$1"]]
    client_listing[0] += "second-client\t$2\n"
    before_ambiguous = list(navigation_calls)
    try:
        panel_chats.open_chat(project, shared, "chat-sol")
    except ValueError as error:
        assert "ambiguous" in str(error)
    else:
        raise AssertionError("cross-session navigation chose between current clients")
    assert navigation_calls == before_ambiguous
    current_session[0] = "$1"
    navigation_calls[:] = navigation_calls[:2]
    with patch.object(panel_chats.sys, "platform", "linux"), \
            patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 0, "codex.desktop\n", "")) as handler_probe:
        app = panel_chats.open_chat(project, shared, "chat-sol", "app", dry_run=True)
    assert app["uri"] == native["uri"]
    assert len(navigation_calls) == 2
    if os.name != "nt":
        assert app["commands"] == [["xdg-open", native["uri"]]]
        assert handler_probe.call_args.args[0] == ["xdg-mime", "query", "default", "x-scheme-handler/codex"]
    else:
        assert app["method"] == "windows-uri" and not handler_probe.called
    with patch.object(panel_chats.sys, "platform", "linux"):
        try:
            panel_chats.open_chat(project, shared, "chat-opus", "app", dry_run=True)
        except ValueError:
            pass
        else:
            raise AssertionError("invented a Claude Code app deep link")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
        try:
            panel_chats.catalog(project, shared)
        except ValueError:
            pass
        else:
            raise AssertionError("worker navigated owner chats")
    with patch.object(panel, "read_field", side_effect=["1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat", return_value=opened) as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()):
        panel.browse_chats(project)
    assert selected.call_args.args[2] == "chat-sol"
    assert selected.call_args.kwargs == {"surface": "auto"}
    with patch.object(panel, "read_field", side_effect=["a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat", return_value=app) as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()):
        panel.browse_chats(project)
    assert selected.call_args.kwargs == {"surface": "app"}
    assert selected.call_args.args[2] == "chat-sol"
    with patch.object(panel, "read_field", side_effect=["", "a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat") as selected:
        panel.browse_chats(project)
    selected.assert_not_called()
    claude_row = next(r for r in chats["rows"] if r["participant"] == "chat-opus")
    with patch.object(panel, "read_field", side_effect=["a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [claude_row]}), \
            patch.object(panel_chats, "open_chat") as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()) as output:
        panel.browse_chats(project)
    selected.assert_not_called()
    assert "requires an enrolled native Codex main" in output.getvalue()
    for row, hint in ((native, "Codex app"), (claude_row, "Claude app sidebar")):
        with patch.object(panel, "read_field", side_effect=["1", "", ""]), \
                patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [row]}), \
                patch.object(panel_chats, "open_chat", side_effect=ValueError("navigation unavailable")), \
                patch.object(panel.sys, "stdout", io.StringIO()) as output:
            panel.browse_chats(project)
        assert hint in output.getvalue(), output.getvalue()
# A main's native model can change after launch; the view must follow its bound session.
import sqlite3
model_member = dict(next(m for m in room.status(project, shared)["participants"] if m["participant"] == "chat-sol"), state="working")
model_database = codex_home / "state_5.sqlite"
with sqlite3.connect(str(model_database)) as database:
    database.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, model TEXT, reasoning_effort TEXT, cwd TEXT)")
    database.execute("INSERT INTO threads VALUES (?,?,?,?)", (chat_codex, "gpt-6.1-sol", "xhigh", str(project.resolve())))
model_report = {"room": {"id": shared, "participants": [model_member]}, "attempts": {"active_recent": [
    {"attempt_id": chat_attempt, "state": "working", "panel": {"room_id": shared, "room_participant": "chat-sol",
     "role": "main", "model": "gpt-6-sol", "effort": "high"}}]}}
model_record_before = json.dumps(model_report, sort_keys=True)
with patch.dict(os.environ, chat_env, clear=True):
    current = panel_chats.current_models(project, [model_member])
    assert current["chat-sol"]["model"] == "gpt-6.1-sol" and current["chat-sol"]["effort"] == "xhigh"
    assert not panel_chats.current_models(project, [dict(model_member, role="worker")])
    assert not panel_chats.current_models(project, [dict(model_member, state="done")])
    assert not panel_chats.current_models(project, [dict(model_member, consumer="f" * 32)])
    status = collect(model_report, chat_attempt, repo=project)
    assert status["codex"]["model"] == "gpt-6.1-sol", status
    from room_view import nodes
    refreshed = dict(model_report, provider_status=status)
    member = nodes(refreshed)[0]
    assert member["model"] == "gpt-6.1-sol" and member["effort"] == "xhigh", member
    assert "Sol 6.1" in render(refreshed, "codex", 100, 26, view="graph"), refreshed
    from panel_view import hierarchy
    assert hierarchy(refreshed, [], chat_attempt)[0]["title"] == "MAIN / Sol 6.1"
    for density in ("tree", "compact", "detail"):
        assert "Sol 6.1" in render(refreshed, "codex", 100, 40, view=density), density
    assert json.dumps(model_report, sort_keys=True) == model_record_before
    for phase in ("blocked", "orphaned", "waiting_input", "review"):
        paused = deepcopy(model_report)
        paused["attempts"]["active_recent"][0]["state"] = phase
        paused_status = collect(paused, chat_attempt, repo=project)
        assert paused_status["codex"]["model"] == "gpt-6.1-sol", (phase, paused_status)
        paused_status["codex"]["native_models"] = {}
        paused_status["codex"]["model"] = None
        missing = nodes(dict(paused, provider_status=paused_status))[0]
        assert missing["model"] is None and missing["effort"] is None and missing["model_at_launch"] == "gpt-6-sol", missing
    with patch.object(panel_chats, "current_models", return_value={}):
        paused_status = collect(paused, chat_attempt, repo=project)
        assert paused_status["codex"]["model"] is None, paused_status
    wrong = deepcopy(status)
    wrong["codex"]["native_models"]["chat-sol"]["consumer"] = "f" * 32
    assert nodes(dict(model_report, provider_status=wrong))[0]["model"] is None
    assert hierarchy(dict(model_report, provider_status=wrong), [], chat_attempt)[0]["title"] != "MAIN / Sol 6"
    with sqlite3.connect(str(model_database)) as database:
        database.execute("UPDATE threads SET cwd=?", (str(temporary / "another-project"),))
    assert not panel_chats.current_models(project, [model_member]), "a foreign project supplied the model"
    with sqlite3.connect(str(model_database)) as database:
        database.execute("UPDATE threads SET cwd=?", (str(project.resolve()),))
    link = codex_home / "state_99.sqlite"
    os.link(model_database, link)
    try:
        assert not panel_chats.current_models(project, [model_member]), "linked native database was trusted"
    finally:
        link.unlink()
    old = codex_home / "state_4.sqlite"
    old.write_bytes(model_database.read_bytes())
    saved = model_database.with_suffix(".saved")
    model_database.rename(saved)
    model_database.write_text("unreadable newer database")
    try:
        assert not panel_chats.current_models(project, [model_member]), "an older model setting replaced a failed current read"
    finally:
        model_database.unlink()
        saved.rename(model_database)
    # The current setting can live only in WAL; immutable main-file reads would be stale.
    with sqlite3.connect(str(model_database)) as writer:
        writer.execute("PRAGMA journal_mode=WAL")
        writer.execute("UPDATE threads SET model=?, reasoning_effort=?", ("gpt-6.1-sol", "ultra"))
        writer.commit()
        wal = Path(str(model_database) + "-wal")
        before_native = [model_database.read_bytes(), wal.read_bytes()]
        assert panel_chats.current_models(project, [model_member])["chat-sol"]["effort"] == "ultra"
        assert [model_database.read_bytes(), wal.read_bytes()] == before_native, "reader changed native model rows"
        isolated = codex_home / "state_6.sqlite"
        isolated.write_bytes(model_database.read_bytes())
        isolated_wal = Path(str(isolated) + "-wal")
        isolated_wal.write_bytes(wal.read_bytes())
        native_files = sorted(p.name for p in codex_home.iterdir())
        assert not panel_chats.current_models(project, [model_member]), "missing WAL reader files must stay unavailable"
        assert sorted(p.name for p in codex_home.iterdir()) == native_files, "reader created missing native sidecars"
        isolated.unlink()
        isolated_wal.unlink()
    assert room.records(project, shared) == room_before_chats
    assert "PRIVATE_TRANSCRIPT_BODY" in chat_file.read_text(), "native history was changed"
# Navigation needs this repo's active main provenance; copied identifiers are insufficient.
valid_window = "@42\t" + chat_attempt + "\t" + shared + "\t%42\tchat-sol\t" + str(project.resolve()) + "\t$1\n"
for bad_window in (valid_window.replace(str(project.resolve()), str(temporary / "foreign-repo")),
                   valid_window.replace(chat_attempt, "att_" + "f" * 32),
                   "@42\tchat-sol\t" + shared + "\t%42\n"):
    def invalid_navigation(command, **kwargs):
        if command[0] == "bash":
            return navigation_run(command, **kwargs)
        return subprocess.CompletedProcess(command, 0,
            bad_window if command[1] == "list-windows" else "%42\t@42\n", "")
    with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
            patch.object(panel_chats.subprocess, "run", side_effect=invalid_navigation):
        assert panel_chats.windows(project, shared) == {}
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=tmux_navigation):
    mismatched = deepcopy(room.status(project, shared))
    next(p for p in mismatched["participants"] if p["participant"] == "chat-sol")["provider"] = "claude"
    assert panel_chats.windows(project, shared, mismatched) == {}
    wrong_room = deepcopy(mismatched)
    wrong_room["participants"] = [dict(wrong_room["participants"][0], participant="chat-sol", provider="codex", role="worker")]
    assert panel_chats.windows(project, shared, wrong_room) == {}
# A first launch has no native binding or stable-participant reference yet.
initial_room = room.create(project, "first-native-navigation", "First native navigation")
initial_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + initial_room,
    "--then", "starting", "--then", "working", output=True)
room.join(project, initial_room, initial_attempt, "codex")
def initial_navigation(command, **kwargs):
    if command[0] == "bash":
        return navigation_run(command, **kwargs)
    text = ("@42\t" + initial_attempt + "\t" + initial_room + "\t%42\t" + initial_attempt + "\t" + str(project.resolve()) + "\t$1\n"
            if command[1] == "list-windows" else "$1\n" if command[1] == "display-message" else "%42\t@42\n")
    return subprocess.CompletedProcess(command, 0, text, "")
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=initial_navigation):
    first_chat = panel_chats.catalog(project, initial_room)["rows"][0]
    assert first_chat["status"] == "native" and first_chat["session_id"] is None and first_chat["terminal"]["pane"] == "%42"
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition", "--attempt", initial_attempt, "--state", "cancelled"])
    assert panel_chats.windows(project, initial_room) == {}
# Explicit worker bindings retain routing, but are not owner app-navigation grants.
legacy_room = room.create(project, "legacy-native-worker", "Legacy role binding")
legacy_sid = "44444444-4444-4444-8444-444444444444"
legacy_hash = hashlib.sha256(legacy_sid.encode()).hexdigest()[:32]
legacy_rows = room.records(project, legacy_room)
legacy_row = dict(room.records(project, shared)[-1], thread=legacy_room, seq=legacy_rows[-1]["seq"] + 1,
                  text="Legacy fixture", room_event={"kind": "join", "participant": "legacy-worker",
                  "provider": "codex", "role": "worker", "label": "Legacy worker", "consumer": legacy_hash, "owns": []})
with (project / ".oms/threads" / (legacy_room + ".jsonl")).open("a") as output:
    output.write(json.dumps(legacy_row) + "\n")
(codex_day / ("rollout-x-" + legacy_sid + ".jsonl")).write_text(
    json.dumps({"type": "session_meta", "payload": {"id": legacy_sid}}) + "\n")
assert room.selected(project, legacy_hash) == (legacy_room, "legacy-worker")
assert room.selected(project, legacy_hash, legacy_room, "legacy-worker") == (legacy_room, "legacy-worker")
assert room.selected(project, legacy_hash, shared, child["participant"]) == (shared, child["participant"])
with patch.dict(os.environ, chat_env, clear=True):
    legacy_chats = panel_chats.catalog(project, legacy_room)
    assert legacy_chats["rows"][0]["status"] == "artifacts" and legacy_chats["rows"][0]["uri"] is None
    assert panel_chats.open_chat(project, legacy_room, "legacy-worker", "app", dry_run=True)["method"] == "artifacts"
room.join(project, legacy_room, "explicit-worker", "codex", role="worker", native_session="explicit-worker-session")
explicit_hash = hashlib.sha256(b"explicit-worker-session").hexdigest()[:32]
assert room.selected(project, explicit_hash) == (legacy_room, "explicit-worker")
reason_report = deepcopy(chats)
reason_report["rows"][0].update(joined=False, reason="ambiguous native identity")
assert " / left" in panel_chats.text(reason_report) and "ambiguous native identity" in panel_chats.text(reason_report)

# Desktop navigation uses the exact session and only an advertised native
# flag. Inspecting a plan must not run a model or force-close a live session.
if os.name != "nt":
  with patch.dict(os.environ, chat_env, clear=True), \
          patch.object(panel_chats.sys, "platform", "linux"), \
          patch.object(panel_chats, "catalog", return_value=chats):
    mime_command = ["xdg-mime", "query", "default", "x-scheme-handler/codex"]
    app_command = ["xdg-open", "codex://threads/" + chat_codex]
    for missing in ("xdg-open", "xdg-mime"):
        with patch.object(panel_chats.shutil, "which", side_effect=lambda tool: None if tool == missing else "/fixture/" + tool), \
                patch.object(panel_chats.subprocess, "run") as launched:
            try:
                panel_chats.open_chat(project, shared, "chat-sol", "app")
            except ValueError:
                pass
            else:
                raise AssertionError("missing Linux app tool was accepted")
            launched.assert_not_called()
    failures = (subprocess.CompletedProcess(mime_command, 0, "", ""),
                subprocess.CompletedProcess(mime_command, 1, "codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "../codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\nextra.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "x" * 257, ""),
                subprocess.TimeoutExpired(mime_command, 3))
    for failure in failures:
        with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
                patch.object(panel_chats.subprocess, "run", side_effect=(failure if isinstance(failure, Exception) else None),
                             return_value=(None if isinstance(failure, Exception) else failure)) as launched:
            try:
                panel_chats.open_chat(project, shared, "chat-sol", "app")
            except ValueError as error:
                assert "handler" in str(error)
            else:
                raise AssertionError("unverified Linux handler was accepted")
            assert launched.call_count == 1 and launched.call_args.args[0] == mime_command
    with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", side_effect=[
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\n", ""),
                subprocess.CompletedProcess(app_command, 0, "", "")]) as launched:
        requested = panel_chats.open_chat(project, shared, "chat-sol", "app")
        assert requested["status"] == "navigation_requested" and not requested["ui_observed"]
        assert requested["commands"] == [app_command]
        assert [call.args[0] for call in launched.call_args_list] == [mime_command, app_command]
    with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", side_effect=[
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "", "")]) as launched:
        assert panel_chats.open_chat(project, shared, "chat-sol", "app", dry_run=True)["method"] == "app-uri"
        try:
            panel_chats.open_chat(project, shared, "chat-sol", "app")
        except ValueError:
            pass
        else:
            raise AssertionError("a stale app plan launched without a current handler")
        assert [call.args[0] for call in launched.call_args_list] == [mime_command, mime_command]
    with patch.object(panel_chats, "terminal_commands", return_value=[]), \
            patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                mime_command, 0, "codex.desktop\n", "")) as launched:
        preferred = panel_chats.open_chat(project, shared, "chat-sol", dry_run=True)
        assert preferred["method"] == "existing-terminal"
        launched.assert_not_called()
for platform in ("darwin", "win32"):
    for supported in (True, False):
        with patch.dict(os.environ, chat_env, clear=True), \
                patch.object(panel_chats.sys, "platform", platform), \
                patch.object(panel_chats, "catalog", return_value=chats), \
                patch.object(panel_chats.shutil, "which", return_value="/fixture/claude"), \
                patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                    [], 0, "--desktop" if supported else "--resume", "")) as probe:
            try:
                destination = panel_chats.open_chat(project, shared, "chat-opus", "app", dry_run=True)
                assert supported and destination["commands"] == [["/fixture/claude", "--desktop", "--resume", chat_claude]]
                assert not destination["ui_observed"]
            except ValueError:
                assert not supported
            assert probe.call_count == 1 and probe.call_args.args[0] == ["/fixture/claude", "--help"]
with patch.dict(os.environ, chat_env, clear=True), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/launcher"), \
        patch.object(panel_chats.subprocess, "run", side_effect=lambda command, **kwargs:
            subprocess.CompletedProcess(command, 0, "@42\tchat-sol\t" + shared + "\t%42\n" if command[1] == "list-windows" else "", "")):
    assert not next(r for r in panel_chats.catalog(project, shared)["rows"] if r["participant"] == "chat-sol")["terminal"]
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats, "catalog", return_value=chats), \
        patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess([], 1)):
    try:
        panel_chats.open_chat(project, shared, "chat-sol")
    except ValueError as error:
        assert "session was preserved" in str(error)
    else:
        raise AssertionError("failed navigation was reported as successful")
assert room.records(project, shared) == room_before_chats, "chat navigation cannot mutate shared history"
chat_cli_env = dict(chat_env)
chat_cli_env.pop("TMUX")
mime_fixture = binary / "xdg-mime"
mime_fixture.write_text("#!/usr/bin/env bash\n" +
                        "test \"$1 $2 $3\" = 'query default x-scheme-handler/codex' || exit 1\n" +
                        "printf 'codex.desktop\\n'\n")
mime_fixture.chmod(0o755)
opener_fixture = binary / "xdg-open"
opener_fixture.write_text("#!/usr/bin/env bash\nexit 0\n")
opener_fixture.chmod(0o755)
listed = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                        "--chats", "--json"], chat_cli_env))
assert listed["kind"] == "oms-panel-chats" and any(r["uri"] for r in listed["rows"])
planned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                         "--open-chat", "chat-sol", "--chat-surface", "app", "--dry-run", "--json"], chat_cli_env))
assert planned["uri"] == "codex://threads/" + chat_codex and not planned["ui_observed"]
unselected_env = dict(chat_cli_env)
unselected_env.pop("OMS_ROOM_ID")
unselected = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--chats", "--json"],
                            env=unselected_env, capture_output=True, text=True)
assert unselected.returncode != 0 and "select an existing room first" in unselected.stderr
# Symlinked metadata must not become an app navigation target.
chat_file.unlink()
chat_file.symlink_to(codex_day / ("rollout-x-" + bad_codex + ".jsonl"))
with patch.dict(os.environ, chat_cli_env, clear=True):
    assert next(r for r in panel_chats.catalog(project, shared)["rows"] if r["participant"] == "chat-sol")["status"] == "unresolved"
# A new adapter opts into native open without changing a provider-specific panel branch.
open_log = temporary / "native-open.json"
open_context = temporary / "native-context.txt"
native_adapter = binary / "oms-agent-adapter-roomtest"
native_adapter.write_text("#!/usr/bin/env python3\n" +
    "import json, os, pathlib, signal, sys, time\na=sys.argv[1:]\nassert a[0]=='open'\n" +
    "p=pathlib.Path(a[a.index('--context-file')+1])\n" +
    "pathlib.Path(os.environ['OMS_OPEN_CONTEXT']).write_text(p.read_text())\n" +
    "pathlib.Path(os.environ['OMS_OPEN_LOG']).write_text(json.dumps({'argv':a,'room':os.environ['OMS_ROOM_ID'],"
    "'participant':os.environ['OMS_ROOM_PARTICIPANT'],'attempt':os.environ['OMS_PANEL_MAIN_ATTEMPT'],'context':str(p)}))\n" +
    "if os.environ.get('OMS_OPEN_APPEND_LOG'):\n" +
    " with open(os.environ['OMS_OPEN_APPEND_LOG'],'a') as f: f.write(str(os.getpid())+'\\n')\n" +
    "if os.environ.get('OMS_OPEN_HOLD'): time.sleep(float(os.environ['OMS_OPEN_HOLD']))\n" +
    "if os.environ.get('OMS_OPEN_INT_LOG'):\n" +
    " signal.signal(signal.SIGINT, lambda n,f: pathlib.Path(os.environ['OMS_OPEN_INT_LOG']).write_text('interrupted turn'))\n" +
    " os.killpg(os.getpgrp(),signal.SIGINT)\n time.sleep(1)\n" +
    "if os.environ.get('OMS_OPEN_SIGNAL'):\n" +
    " os.kill(os.getppid(), getattr(signal, os.environ['OMS_OPEN_SIGNAL']))\n time.sleep(300)\n")
native_adapter.chmod(0o755)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                                OMS_OPEN_CONTEXT=str(open_context), OMS_PROVIDER_NATIVE_ADAPTERS="roomtest"), clear=True):
    inventory = panel.provider_catalog("roomtest")[0]
    assert inventory["native_launch"] and not open_log.exists(), "discovery must not execute open"
    assert panel.run_native("roomtest", project, task="Inspect source; no unrelated work") == 0
opened = json.loads(open_log.read_text())
assert opened["room"] == shared and opened["participant"]
assert not Path(opened["context"]).exists(), "native context lifetime ends with its process"
assert "OMS room" in open_context.read_text() and "Inspect source; no unrelated work" in open_context.read_text()
assert "--dangerously" not in " ".join(opened["argv"])
recovery_env = dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                    OMS_OPEN_CONTEXT=str(open_context), OMS_PROVIDER_NATIVE_ADAPTERS="roomtest")
with patch.dict(os.environ, recovery_env, clear=True):
    assert panel.run_native("roomtest", project, resume="fixture-stable-session") == 0
    original_main = json.loads(open_log.read_text())
    member = original_main["participant"]
    room.send(project, shared, room_owner, member, "Mail while the native main was offline", message_id="offline-mail")
    before_resume = room.records(project, shared)
    first_delta = room.updates(project, shared, member)
    delivered = list(first_delta["turns"])
    for _ in range(len(before_resume)):
        if not first_delta["has_more"]:
            break
        first_delta = room.updates(project, shared, member, first_delta["cursor"])
        delivered += first_delta["turns"]
    assert not first_delta["has_more"]
    assert any(row["room_event"]["id"] == "offline-mail" for row in delivered)
    with patch.object(room, "MAX_PARTICIPANTS", len(room.status(project, shared)["participants"])):
        assert panel.run_native("roomtest", project, resume="fixture-stable-session", model="fixture-model") == 0
    resumed = json.loads(open_log.read_text())
    assert resumed["participant"] == member and resumed["attempt"] != original_main["attempt"]
    assert room.records(project, shared) == before_resume, "resume cannot reset the subscription or consume a participant slot"
    assert room.status(project, shared)["pending_count"] > 0, "read does not acknowledge offline mail"
    assert not room.updates(project, shared, member, first_delta["cursor"])["turns"], "resume preserves cursor anchors"
    # An existing active successor blocks another resume even though the
    # original participant-named attempt was terminal.
    active = panel.events(project, "start", "--provider", "roomtest", "--tool", "panel-main", "--ref", "panel_role=main",
                          "--ref", "panel_room_id=" + shared, "--ref", "panel_room_participant=" + member,
                          "--then", "starting", "--then", "working", output=True)
    with patch.object(panel, "native_command", side_effect=AssertionError("unknown owner must not launch")):
        try:
            panel.run_native("roomtest", project, resume="fixture-stable-session")
        except ValueError as error:
            assert "terminal owner" in str(error)
        else:
            raise AssertionError("active successor was ignored")
    panel.events(project, "transition", "--attempt", active, "--state", "cancelled")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
        try:
            panel.run_native("roomtest", project, resume="fixture-stable-session")
        except ValueError:
            pass
        else:
            raise AssertionError("worker opened a native main")
    for bad_tool, bad_provider in (("consult", "roomtest"), ("panel-main", "codex")):
        bad_room = room.create(project, "bad-owner-" + bad_tool, "Ownership evidence fixture")
        impostor = panel.events(project, "start", "--provider", bad_provider, "--tool", bad_tool, "--ref", "panel_role=main",
                                  "--ref", "panel_room_id=" + bad_room,
                                  "--then", "starting", "--then", "working", output=True)
        panel.events(project, "transition", "--attempt", impostor, "--state", "cancelled")
        room.join(project, bad_room, impostor, "roomtest", native_session="fixture-bad-owner")
        try:
            with patch.dict(os.environ, {"OMS_ROOM_ID": bad_room}), \
                    patch.object(panel, "native_command", side_effect=AssertionError("bad provenance must not launch")):
                panel.run_native("roomtest", project, resume="fixture-bad-owner")
        except ValueError as error:
            assert "terminal owner" in str(error)
        else:
            raise AssertionError("resume accepted unrelated ownership evidence")
# Two independent processes requesting the same resumed address must not both
# launch. The first fake native stays active beyond the existing lock wait.
race_log = temporary / "native-resume-race.txt"
race_env = dict(recovery_env, OMS_OPEN_HOLD="14", OMS_OPEN_APPEND_LOG=str(race_log))
race_script = ("import sys\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
               "try: p.run_native('roomtest',Path(sys.argv[2]),resume='fixture-stable-session')\n"
               "except ValueError as e: print(str(e));sys.exit(75)\n")
other_race_room = room.create(project, "parallel-resume-room", "Concurrent resume fixture")
racers = [subprocess.Popen([sys.executable, "-c", race_script, str(root / "scripts/lib"), str(project)],
                           env=dict(race_env, OMS_ROOM_ID=ident), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
          for ident in (shared, other_race_room)]
try:
    outcomes = [r.communicate(timeout=35) for r in racers]
    assert sorted(r.returncode for r in racers) == [0, 75], outcomes
    assert len(race_log.read_text().splitlines()) == 1, "concurrent resumes launched two native owners"
finally:
    for racer in racers:
        if racer.poll() is None:
            racer.kill(); racer.wait(timeout=5)
# Native references link the stable participant even when attempt ids differ,
# and an unrelated room's same spelling cannot override it.
import room_view as graph_view
native_report = {"room": room.status(project, shared), "attempts": {"active_recent": [
    {"attempt_id": "unqualified-attempt", "state": "failed", "panel": {"role": "main", "room_participant": member}},
    {"attempt_id": "fresh-attempt", "state": "working", "panel": {"role": "main", "room_id": shared,
     "room_participant": member, "model": "fixture-model"}},
    {"attempt_id": "foreign-attempt", "state": "failed", "panel": {"role": "main", "room_id": "foreign-room",
     "room_participant": member}}], "recent": []}}
linked = next(row for row in graph_view.nodes(native_report) if row["participant"] == member)
assert linked["state"] == "working" and linked["model"] == "fixture-model"
# A council's seats inherit the main's participant; the graph shows the debate
# beside the main instead of relabeling the main card as a seat.
council_meta = {"role": "advisor", "label": "Pick next work", "room_id": shared, "room_participant": member}
native_report["attempts"]["active_recent"][:0] = [
    {"attempt_id": "seat-attempt", "parent_attempt_id": "fresh-attempt", "state": "working", "tool": "ask",
     "task_id": "decide", "panel": dict(council_meta, model="claude-fable-5-1")},
    {"attempt_id": "council-attempt", "parent_attempt_id": "fresh-attempt", "state": "working",
     "tool": "panel-council", "task_id": "decide", "panel": council_meta}]
linked = next(row for row in graph_view.nodes(native_report) if row["participant"] == member)
assert linked["attempt"] == "fresh-attempt" and linked["model"] == "fixture-model", linked
debate_graph = graph_view.render_graph(native_report, 110, 40)
assert "COUNCIL / 1 seat(s) answering" in debate_graph and "Debate: Pick next work" in debate_graph, debate_graph
debate_navigation = {}
debate_connected = render(native_report, "claude", 91, 40, view="tree", main_attempt="fresh-attempt",
                          navigation=debate_navigation)
assert "COUNCIL (1)" in debate_connected and "Debate: Pick next work" in debate_connected, debate_connected
assert "1 seat(s) answering" in debate_connected and "this window" in debate_connected, debate_connected
assert ("debate", ("decide", member)) in debate_navigation["items"]
# Claude Code's built-in advisor shows above its main only while unanswered, read from the
# main's own transcript tail; finalized failed calls leave the board.
from datetime import datetime, timezone
advisor_home = temporary / "advisor-claude"
(advisor_home / "projects" / "slug").mkdir(parents=True)
(advisor_home / "settings.json").write_text('{"advisorModel": "fable"}')
advisor_sid = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d"
advisor_consumer = hashlib.sha256(advisor_sid.encode()).hexdigest()[:32]
advisor_log = advisor_home / "projects" / "slug" / (advisor_sid + ".jsonl")
advisor_use = {"timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z"), "message": {"content": [
    {"type": "server_tool_use", "name": "advisor", "id": "srvtoolu_1"}]}}
advisor_log.write_text("partial{\n" + json.dumps(advisor_use) + "\nnot json advisor\n")
claude_main = {"participant": "adv-main", "provider": "claude", "role": "main", "joined": True, "label": "Claude",
               "consumer": advisor_consumer, "seq": 1}
with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(advisor_home)}):
    open_call = panel_chats.native_advisors([claude_main])
    assert open_call["adv-main"]["model"] == "Fable 5.1" and len(open_call["adv-main"]["started"]) == 5, open_call
    assert open_call["adv-main"]["running"] is True and "finished" not in open_call["adv-main"], open_call
    assert panel_chats.native_advisors([dict(claude_main, provider="codex")]) == {}
    advisor_log.write_text(advisor_log.read_text() + json.dumps({"timestamp": advisor_use["timestamp"], "message": {"content": [
        {"type": "advisor_tool_result", "tool_use_id": "srvtoolu_1", "content": "ciphertext"}]}}) + "\n")
    answered_call = panel_chats.native_advisors([claude_main])
    assert answered_call["adv-main"]["running"] is False and len(answered_call["adv-main"]["finished"]) == 5, answered_call
    stale_stamp = "2020-01-01T00:00:00.000Z"
    advisor_log.write_text(json.dumps(dict(advisor_use, timestamp=stale_stamp)) + "\n" + json.dumps({
        "timestamp": stale_stamp, "message": {"content": [{"type": "advisor_tool_result", "tool_use_id": "srvtoolu_1"}]}}) + "\n")
    assert panel_chats.native_advisors([claude_main]) == {}
board = {"room": {"id": "adv-room", "participants": [claude_main]}, "attempts": {"active_recent": [], "recent": []}}
plain_board = graph_view.render_graph(board, 100, 40)
assert "Fable 5.1" not in plain_board and "Built-in advisor" not in plain_board, plain_board
advisor_board = graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40)
assert "Fable 5.1" in advisor_board and "Built-in advisor" in advisor_board, advisor_board
nav = {"selected": ("result", "native-advisor-adv-main"), "preview": {"target": ("result", "native-advisor-adv-main"), "report": {}}}
assert "answer stays inside the main" in graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40, navigation=nav)
# An answered call draws no card; it counts as finished and the main's detail says when it answered.
pair_board = dict(board, room={"id": "adv-room", "participants": [claude_main, dict(claude_main, participant="adv-other",
                                                                                    provider="codex", seq=2)]})
finished_board = graph_view.render_graph(dict(pair_board, native_advisors=answered_call), 120, 34,
                                         navigation={"overview": True, "dismissed": True})
assert "Fable 5.1" not in finished_board and "1 done" in finished_board, finished_board
finished_nav = {"selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
finished_detail = graph_view.render_graph(dict(board, native_advisors=answered_call), 100, 40, navigation=finished_nav)
assert "Built-in advisor: last answered " + answered_call["adv-main"]["finished"] in finished_detail, finished_detail
running_nav = {"selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
assert "Built-in advisor: asked " in graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40, navigation=running_nav)
# The board's last row starts a main: clickable only in a managed panel, never on a static render, and
# a main started by another main is marked on its tab and in its detail.
starter_board = dict(board, room={"id": "adv-room", "participants": [dict(claude_main, model="claude-opus-5-5"), dict(claude_main, participant="adv-sub",
                     provider="codex", seq=2, consumer=None, model="gpt-6-sol")]},
                     main_starters={"adv-sub": "adv-main"})
assert "[+ " not in graph_view.render_graph(starter_board, 100, 40, navigation={})
for bar_width, bar_height in ((80, 30), (157, 40)):
    for bar_unicode in (True, False):
        bar_nav = {"installed": ["codex", "claude"], "spawn_managed": True, "selected": ("chat", "adv-sub"),
                   "preview": {"target": ("chat", "adv-sub"), "report": {}}}
        for bar_draw in (lambda: graph_view.render_graph(starter_board, bar_width, bar_height, unicode=bar_unicode, navigation=bar_nav),
                         lambda: render(dict(starter_board, collection={"ok": True}), "claude", bar_width, bar_height,
                                        unicode=bar_unicode, view="tree", navigation=bar_nav)):
            bar_text = bar_draw()
            bar_rows = bar_text.splitlines()
            spots = {h["action"][1]: h for h in bar_nav["hits"] if h["action"][0] == "spawn"}
            # The start bar sits right above the pinned key hints, and its hits are on its own row.
            bar_row = next(i for i, row in enumerate(bar_rows) if "[+ Codex main]" in row)
            assert "Keys" in bar_rows[-1] and bar_row < len(bar_rows) - 1, bar_rows[-3:]
            assert set(spots) == {"codex", "claude"} and all(h["y"] == bar_row + 1 for h in spots.values()), bar_nav["hits"]
            assert bar_rows[bar_row][spots["codex"]["x1"] - 1:spots["codex"]["x2"]] == "[+ Codex main]", bar_rows[bar_row]
            assert bar_rows[bar_row][spots["claude"]["x1"] - 1:spots["claude"]["x2"]] == "[+ Claude main]", bar_rows[bar_row]
        graph_text = graph_view.render_graph(starter_board, bar_width, bar_height, unicode=bar_unicode, navigation=bar_nav)
        assert ("↳" if bar_unicode else "^") in graph_text and "Started by Opus 5.5" in graph_text, graph_text
disabled_nav = {"installed": ["codex"], "spawn_managed": False}
assert "needs the tmux panel" in graph_view.render_graph(starter_board, 100, 40, navigation=disabled_nav).splitlines()[-2]
assert not [h for h in disabled_nav["hits"] if h["action"][0] == "spawn"]
armed_nav = {"installed": ["codex"], "spawn_managed": True, "spawn_busy": True}
assert graph_view.render_graph(starter_board, 100, 40, navigation=armed_nav).splitlines()[-2].startswith("Starting a main")
# --spawn-main needs no terminal, refuses workers, an absent panel and a full room, and never attaches.
for spawn_env, expected in (({"OMS_HARNESS_CHILD": "1"}, "worker cannot open owner sessions"),
                            ({"OMS_HARNESS_DELEGATE_DEPTH": "1"}, "worker cannot open owner sessions"),
                            # Without tmux or the CLI (CI hosts) the tool check refuses first.
                            ({}, "no OMS panel is open for this checkout" if shutil.which("tmux") and shutil.which("claude")
                             else "starting a main needs tmux")):
    refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude", "--json"],
                             cwd=str(project), env=dict(environment, OMS_PANEL_SESSION="", **spawn_env),
                             capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    assert refused.returncode != 0 and expected in refused.stderr, (spawn_env, refused.stdout, refused.stderr)
# The cap is checked after the tool check, so hosts without tmux or a CLI (CI) still reach it.
with patch.object(panel, "session_owner", return_value=str(project)), patch.object(panel, "panel_room", return_value="r1"), \
        patch.object(panel.shutil, "which", return_value="/usr/bin/true"), \
        patch.object(panel, "live_mains", return_value=panel.MAX_LIVE_MAINS), patch.object(panel, "add_main_window") as opened_window:
    try:
        panel.spawn_main(project, "claude")
        raise AssertionError("a fifth live main was started")
    except ValueError as error:
        assert "already has 6 live mains" in str(error)
    assert not opened_window.called
    with patch.object(panel, "tmux_command", return_value=["true"]), patch.object(panel, "bind_main_keys"):
        try:
            panel.split_panel(project, launch="claude")
            raise AssertionError("--launch added a seventh live main")
        except ValueError as error:
            assert "already has 6 live mains" in str(error)
    assert not opened_window.called
failed_worker = {"participant": "adv-worker", "provider": "codex", "role": "worker", "joined": True, "label": "Worker",
                 "parent": "adv-main", "seq": 2}
failed_board = dict(board, room={"id": "adv-room", "participants": [claude_main, failed_worker]}, attempts={"active_recent": [
    {"attempt_id": "adv-attempt", "state": "failed", "task_id": "adv-task", "panel": {"role": "worker", "room_id": "adv-room",
     "room_participant": "adv-worker", "label": "Broken patch"}}], "recent": []})
assert "Broken patch" in graph_view.render_graph(failed_board, 100, 40)
assert "Broken patch" in graph_view.render_graph(dict(failed_board, finalized={"other": "accepted"}), 100, 40)
assert "Broken patch" not in graph_view.render_graph(dict(failed_board, finalized={"adv-task": "accepted"}), 100, 40)
# A call missing from a complete active list for ten minutes has ended; a just-joined one stays,
# and a truncated active list (the projection keeps 8 rows) proves nothing.
untracked = dict(board, attempts={"available": True, "active": 0, "active_recent": [], "recent": []},
                 room={"id": "adv-room", "participants": [claude_main, dict(
    failed_worker, label="Old patch", joined_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 900)))]})
assert "Old patch" not in graph_view.render_graph(untracked, 100, 40)
assert "Old patch" in graph_view.render_graph(dict(untracked, attempts=dict(untracked["attempts"], active=9)), 100, 40)
untracked["room"]["participants"][1]["joined_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
assert "Old patch" in graph_view.render_graph(untracked, 100, 40)
resumed_lifecycle = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                      "show", "--attempt", resumed["attempt"], "--json"]))
resumed_lifecycle.update(terminal=False, state="blocked")
def resumed_navigation(command, **kwargs):
    data = (json.dumps([resumed_lifecycle]) if command[0] == "bash" else
            "@42\t" + resumed["attempt"] + "\t" + shared + "\t%42\t" + member + "\t" + str(project.resolve()) + "\t$1\n"
            if command[1] == "list-windows" else "$1\n" if command[1] == "display-message" else "%42\t@42\n")
    return subprocess.CompletedProcess(command, 0, data, "")
with patch.dict(os.environ, dict(recovery_env, TMUX="fixture-tmux"), clear=True), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=resumed_navigation):
    assert panel_chats.windows(project, shared)[member][0]["pane"] == "%42"
    resumed_lifecycle["terminal"] = True
    assert panel_chats.windows(project, shared) == {}
if os.name != "nt":
    import signal
    int_log = temporary / "native-turn-interrupt.txt"
    int_env = dict(recovery_env, OMS_OPEN_INT_LOG=str(int_log))
    int_script = ("import sys,signal\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
                  "before=signal.getsignal(signal.SIGINT)\nassert p.run_native('roomtest',Path(sys.argv[2]))==0\n"
                  "assert signal.getsignal(signal.SIGINT)==before\n")
    interrupted = subprocess.run([sys.executable, "-c", int_script, str(root / "scripts/lib"), str(project)],
                                 env=int_env, capture_output=True, text=True, start_new_session=True, timeout=20)
    assert interrupted.returncode == 0 and int_log.read_text() == "interrupted turn", interrupted.stderr
    for name in ("SIGTERM", "SIGHUP"):
        if not hasattr(signal, name):
            continue
        signal_env = dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                          OMS_OPEN_CONTEXT=str(open_context), OMS_OPEN_SIGNAL=name,
                          OMS_PROVIDER_NATIVE_ADAPTERS="roomtest")
        script = ("import sys, signal\nfrom pathlib import Path\n"
                  "sys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
                  "before={n:signal.getsignal(getattr(signal,n)) for n in ('SIGTERM','SIGHUP')}\n"
                  "try: p.run_native('roomtest',Path(sys.argv[2]))\n"
                  "except KeyboardInterrupt: pass\n"
                  "else: raise AssertionError('native signal was not handled')\n"
                  "assert all(signal.getsignal(getattr(signal,n))==v for n,v in before.items())\n")
        finished = subprocess.run([sys.executable, "-c", script, str(root / "scripts/lib"), str(project)],
                                  env=signal_env, capture_output=True, text=True, timeout=25)
        assert finished.returncode == 0, finished.stderr
        signalled = json.loads(open_log.read_text())
        status = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                 "show", "--attempt", signalled["participant"], "--json"]))
        assert status["terminal"] and status["state"] == "cancelled", status
        assert not Path(signalled["context"]).exists()
if os.name != "nt":
    import time, signal
    stop_log = temporary / "panel-stop-watch.log"
    with stop_log.open("w") as output:
        watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                    "--room", shared, "--watch", "--no-animation"],
                                   env=recovery_env, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while "Messages:" not in stop_log.read_text() and time.monotonic() < deadline:
                assert watcher.poll() is None
                time.sleep(0.1)
            assert "Messages:" in stop_log.read_text(), "watcher did not become ready"
            watcher.send_signal(signal.SIGINT)
            assert watcher.wait(timeout=10) == 130, "public panel did not receive SIGINT"
        finally:
            if watcher.poll() is None:
                watcher.kill()
                watcher.wait(timeout=5)
soak_seconds = int(os.environ.get("OMS_PANEL_SOAK_SECONDS", "0"))
if soak_seconds:
    assert os.name != "nt" and 30 <= soak_seconds <= 3600
    import time, signal
    watch_log = temporary / "panel-soak-watch.log"
    log_path = project / ".oms/threads" / (shared + ".jsonl")
    displaced = log_path.with_suffix(".soak-backup")
    started = time.monotonic()
    cycles = 0
    cursor = first_delta["cursor"]
    with watch_log.open("w") as output:
        watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                    "--room", shared, "--watch", "--no-animation"],
                                   env=recovery_env, stdout=output, stderr=subprocess.STDOUT)
        try:
            while time.monotonic() - started < soak_seconds:
                assert watcher.poll() is None, "watcher exited during sustained polling"
                mid = "soak-%s" % cycles
                with patch.dict(os.environ, recovery_env, clear=True):
                    room.send(project, shared, room_owner, member, "Controlled soak delivery", message_id=mid)
                    received = []
                    for _ in range(len(room.records(project, shared))):
                        delta = room.updates(project, shared, member, cursor)
                        cursor = delta["cursor"]
                        received.extend(row["room_event"]["id"] for row in delta["turns"])
                        if not delta["has_more"]:
                            break
                    assert mid in received
                    room.acknowledge(project, shared, member, [mid])
                    if cycles % 25 == 0:
                        try:
                            room.updates(project, shared, member, "invalid-cursor")
                        except ValueError:
                            pass
                        else:
                            raise AssertionError("invalid cursor was accepted")
                    if cycles % 50 == 0:
                        log_path.rename(displaced)
                        try:
                            time.sleep(7)
                            assert watcher.poll() is None
                        finally:
                            displaced.rename(log_path)
                cycles += 1
                time.sleep(5)
        finally:
            watcher.send_signal(signal.SIGINT)
            try:
                watcher.wait(timeout=45)
            except subprocess.TimeoutExpired:
                watcher.kill(); watcher.wait(timeout=5)
    assert watcher.returncode == 130
    observed = watch_log.read_text()
    assert "room evidence unavailable" in observed and "Messages:" in observed
    print("panel-soak: %s cycles / %.1fs / addressed delivery, explicit ack, invalid cursors and room-loss recovery" %
          (cycles, time.monotonic() - started), flush=True)
print("room: addressed handoff, graph bounds, saved receiver intents and native adapter passed")
print("terminal-panel: both directions, isolated patches, reviews, guards and PTY passed")
PY

echo 'operator-tools-smoke: ok'
