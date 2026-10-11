#!/usr/bin/env bash
set -euo pipefail
unset HERDR_ENV HERDR_SOCKET_PATH HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID \
  OMS_PANEL_HOST OMS_PANEL_VIEW OMS_PANEL_COLOR
# The suite runs as a top-level operator even inside a panel window or a write worker, whose
# child/room bindings would otherwise turn its room fixtures into refusals.
unset TMUX TMUX_PANE OMS_HARNESS_CHILD OMS_HARNESS_DELEGATE_DEPTH OMS_STATE_REPO OMS_PLAN_LEASE_ID
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
panel_refs = {"panel_role": "researcher", "panel_harness": "codex", "panel_room_id": "research-room",
              "panel_room_participant": "research-call"}
rows.append(dict(created, attempt_id="att_still_active", event_id="levt_active", task_id="panel-ui", refs=panel_refs))
for index in range(10):
    identity = "att_new_%s" % index
    rows.extend([dict(created, attempt_id=identity, event_id="levt_new_%s" % index,
                      ts="2026-09-01T00:00:%02dZ" % (index * 2)),
                 dict(cancelled, attempt_id=identity, event_id="levt_end_%s" % index,
                      seq=2, refs=panel_refs, ts="2026-09-01T00:00:%02dZ" % (index * 2 + 1))])
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
for attempt in (report["attempts"]["active_recent"][0], report["attempts"]["recent"][0]):
    assert {key: attempt["panel"][key] for key in ("role", "harness", "room_id", "room_participant")} == {
        "role": "researcher", "harness": "codex", "room_id": "research-room", "room_participant": "research-call"}, attempt
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
assert module["panel_metadata"]({"refs": {"panel_role": "researcher",
    "panel_room_participant": "research-call"}, "parent_attempt_id": "main-call"})["panel"]["room_participant"] == "research-call"
assert module["panel_metadata"]({"refs": {"panel_role": "unrecognized"}}) == {}
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
PYTHONDONTWRITEBYTECODE=1 python3 - "$ROOT/scripts/lib" "$TMP" <<'PY' || fail "shared panel cache regression"
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import time
from unittest.mock import patch

sys.path.insert(0, sys.argv[1])
import panel_cache as cache
import panel_results
import terminal_panel as panel

project = Path(sys.argv[2]) / 'shared-panel'
(project / '.oms/hooks').mkdir(parents=True)
(project / '.oms/artifacts').mkdir()
(project / '.oms/plan').mkdir()
plan = project / '.oms/plan/tasks.json'
plan.write_text('first', encoding='utf-8')
counter = project / 'computations'
worker = '''
import json, sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, sys.argv[1])
import panel_cache as cache
repo, counter = Path(sys.argv[2]), Path(sys.argv[3])
def compute():
    number = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(number))
    return {'number': number}
if len(sys.argv) > 4:
    cache.time.time = lambda: float(sys.argv[4])
try:
    print(json.dumps(cache.read_shared(repo, 'dashboard', compute)))
except ValueError:
    print(json.dumps({'pending': True}))
'''
def board(now=None):
    command = [sys.executable, '-c', worker, sys.argv[1], str(project), str(counter)]
    if now is not None:
        command.append(str(now))
    return json.loads(subprocess.check_output(command, text=True, timeout=5))

assert board() == board() == {'number': 1}, counter.read_text()
path = project / cache.DIRECTORY / 'dashboard.json'
created = json.loads(path.read_text())['created']
with panel_results._lock(project, 'panel-cache-dashboard', timeout=0):
    start = time.monotonic()
    assert board(created + cache.INTERVAL + 1) == {'number': 1}
    assert time.monotonic() - start < 2, 'contending board blocked'
    plan.write_text('second generation', encoding='utf-8')
    # A board never replays another input generation; with the writer busy it reads for itself.
    assert board() == {'number': 2}, 'replayed a different input generation'
assert counter.read_text() == '2'
assert board() == {'number': 3}
assert board(json.loads(path.read_text())['created'] + cache.INTERVAL + 1) == {'number': 4}
path.write_text('{"schema":1,', encoding='utf-8')
assert board() == {'number': 5}, 'corrupt cache was reused'
path.write_text('{"schema":1}', encoding='utf-8')
assert board() == {'number': 6}, 'partial cache was reused'
entry = json.loads(path.read_text())
entry['value'] = {'number': 999}
path.write_text(json.dumps(entry), encoding='utf-8')
assert board() == {'number': 7}, 'damaged payload was reused'
entry = json.loads(path.read_text())
entry['created'] = 10 ** 400
path.write_text(json.dumps(entry), encoding='utf-8')
assert board() == {'number': 8}, 'overflowing cache timestamp did not trigger recollection'

# Plan reads outlive the dashboard interval, including across board processes.
plan.write_text('{"tasks": {}}', encoding='utf-8')
plan_worker = worker.replace("cache.read_shared(repo, 'dashboard', compute)",
                             "cache.read_plan_list(repo, lambda: {'tasks': [compute()]})")
def plan_board(now):
    return json.loads(subprocess.check_output(
        [sys.executable, '-c', plan_worker, sys.argv[1], str(project), str(counter), str(now)],
        text=True, timeout=5))
now = int(time.time())
assert plan_board(now) == plan_board(now + 5) == plan_board(now + 10) == {'tasks': [{'number': 9}]}
replacement = plan.with_suffix('.tmp')
replacement.write_text(plan.read_text(), encoding='utf-8')
replacement.replace(plan)
assert plan_board(now + 15) == {'tasks': [{'number': 10}]}, 'same-size replacement was missed'
os.utime(plan, ns=(plan.stat().st_atime_ns, plan.stat().st_mtime_ns + 1_000_000))
assert plan_board(now + 20) == {'tasks': [{'number': 11}]}, 'mtime change was missed'

# Measure real agent-plan invocations on fixture state only, using simulated
# five-second tick clocks so this regression needs no sleep or live repository.
plan.write_text(json.dumps({'schema': 3, 'tasks': {
    'cache-task': {'id': 'cache-task', 'state': 'ready', 'depends': [], 'title': 'fixture'}}}), encoding='utf-8')
plan_cache = project / cache.DIRECTORY / 'plan-list.json'
plan_cache.unlink()
real_run = subprocess.run
calls = []
def run_plan(command, **kwargs):
    if 'agent-plan' in command:
        started = time.perf_counter()
        result = real_run(command, **kwargs)
        calls.append((time.perf_counter() - started) * 1000)
        return result
    if command[0] == 'git' or command[:2] == ['bash', '-c']:
        return real_run(command, **kwargs)
    return subprocess.CompletedProcess(command, 0, '{}', '')
def measure(reader, repo=project, expected='cache-task', detailed=False):
    calls.clear()
    ticks, counts = [], []
    started = time.perf_counter()
    with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
            patch.object(panel.room, 'status', side_effect=lambda repo, ident: {'id': ident}), \
            patch.object(panel_results, 'completed_tasks', return_value={}), \
            patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
            patch.object(cache, 'read_plan_list', side_effect=reader), \
            patch.object(panel.subprocess, 'run', side_effect=run_plan):
        for tick in range(3):
            tick_start, tick_calls = time.perf_counter(), len(calls)
            with patch.object(cache.time, 'time', return_value=now + tick * 5):
                state, status = panel.snapshot(repo, 'board-%d' % tick)
                assert status == 0 and state['room']['repo_tasks'][0]['id'] == expected, state
            ticks.append(round((time.perf_counter() - tick_start) * 1000, 1))
            counts.append(len(calls) - tick_calls)
    if detailed:
        return {'tick_ms': ticks, 'tick_calls': counts}
    return len(calls), (time.perf_counter() - started) * 1000
baseline = measure(lambda repo, collect: collect())
cached = measure(cache.read_plan_list)
assert baseline[0] == 3 and cached[0] == 1, (baseline, cached)
os.utime(plan, ns=(plan.stat().st_atime_ns, plan.stat().st_mtime_ns + 1_000_000))
touched = measure(cache.read_plan_list)
assert touched[0] == 1, touched
print('plan-list fixture: before=%d calls %.1f ms; after=%d calls %.1f ms; touched=%d calls %.1f ms'
      % (baseline + cached + touched))

with patch.object(cache, 'fingerprint', side_effect=AssertionError('plan cache walked the repository')):
    assert cache.read_plan_list(project, lambda: None)['tasks'][0]['id'] == 'cache-task'
def raced_plan():
    plan.write_text(plan.read_text() + ' ', encoding='utf-8')
    return {'tasks': []}
plan_cache.unlink()
assert cache.read_plan_list(project, raced_plan) == {'tasks': []}
assert not plan_cache.exists(), 'plan changed during collection was published'
completions = project / '.oms/plan/completions'
completions.mkdir()
assert cache.read_plan_list(project, lambda: {'tasks': []}) == {'tasks': []}
assert plan_cache.exists(), 'empty completion namespace prevented reuse'
completions.rmdir()

# Refresh ages locally; at the exact TTL boundary consult the engine again.
plan_cache.unlink(missing_ok=True)
anchor = int(time.time()) - 20
claim = {'id': 'claim', 'state': 'claimed', 'depends': [], 'title': 'fixture',
         'claimed_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(anchor)), 'ttl': '10',
         'claim_expired': False, 'claim_age_s': 0}
stored_claim = dict(claim)
stored_claim.pop('claim_expired')
stored_claim.pop('claim_age_s')
plan.write_text(json.dumps({'schema': 3, 'tasks': {'claim': stored_claim}}), encoding='utf-8')
with patch.object(cache.time, 'time', return_value=anchor):
    assert cache.read_plan_list(project, lambda: {'tasks': [claim]})['tasks'][0]['claim_expired'] is False
with patch.object(cache.time, 'time', return_value=anchor + 5):
    assert cache.read_plan_list(project, lambda: None)['tasks'][0]['claim_age_s'] == 5
with patch.object(cache.time, 'time', return_value=anchor + 10):
    with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
            patch.object(panel.room, 'status', return_value={'id': 'ttl'}), \
            patch.object(panel_results, 'completed_tasks', return_value={}), \
            patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
            patch.object(panel.subprocess, 'run', side_effect=run_plan) as command:
        state, status = panel.snapshot(project, 'ttl')
        assert status == 0 and state['room']['repo_tasks'][0]['claim_expired'] is True
        assert command.call_count == 2, 'expired claim did not reach agent-plan'

# Failed reads leave the room drawn and retry on the following tick.
plan_cache.unlink()
with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
        patch.object(panel.room, 'status', return_value={'id': 'retry'}), \
        patch.object(panel_results, 'completed_tasks', return_value={}), \
        patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
        patch.object(panel.subprocess, 'run', side_effect=[
            subprocess.CompletedProcess([], 0, '{}', ''), subprocess.CompletedProcess([], 1, '', ''),
            subprocess.CompletedProcess([], 0, '{}', ''), subprocess.CompletedProcess([], 0, '{"tasks": []}', '')]) as command:
    state, status = panel.snapshot(project, 'retry')
    assert status == 0 and state['room']['repo_tasks_unavailable'] is True
    assert not plan_cache.exists(), 'failed read was cached'
    state, status = panel.snapshot(project, 'retry')
    assert status == 0 and state['room']['repo_tasks'] == [] and command.call_count == 4

# Scale the same real validation path to a mixed closed plan. Synthetic receipt
# digests/proofs use the canonical codec; every measured collection runs the
# actual agent-plan validator rather than mocking completion acceptance.
closed = Path(sys.argv[2]) / 'closed-panel'
(closed / '.oms/hooks').mkdir(parents=True)
(closed / '.oms/artifacts').mkdir()
(closed / '.oms/plan/completions').mkdir(parents=True)
root = Path(sys.argv[1]).parents[1]
fixture_env = {key: value for key, value in os.environ.items() if not key.startswith('OMS_')}
fixture_env['OMS_LOCK_DIR'] = str(closed / '.oms/fixture-locks')
real_run(['git', '-C', str(closed), 'init', '-q'], check=True, env=fixture_env)
def fixture_call(script, *args):
    result = real_run(['bash', str(root / 'scripts' / script), '--repo', str(closed), *args],
                      capture_output=True, text=True, check=True, timeout=20, env=fixture_env)
    return result.stdout.strip()
room_id = json.loads(fixture_call('room.sh', 'new', '--topic', 'Closed plan fixture', '--json'))['id']
main = fixture_call('agent-events.sh', 'start', '--provider', 'codex', '--tool', 'panel-main',
                    '--ref', 'panel_room_id=' + room_id, '--ref', 'panel_role=main',
                    '--then', 'starting', '--then', 'working')
fixture_call('room.sh', 'join', '--id', room_id, '--participant', main, '--provider', 'codex', '--role', 'main')
core = runpy.run_path(str(root / 'scripts/lib/plan-completion.py'))
receipt_codec = core['RECEIPT']
encoded, sha = core['encoded'], core['sha']
provenance = core['capture_prefixes'](str(closed), room_id)
namespace = core['namespace'](str(closed))
(closed / 'verify.sh').write_text(':\n', encoding='utf-8')
(closed / 'decision.md').write_text('Reviewed cancellation.\n', encoding='utf-8')
plan_id = 'plan_' + '1' * 32
closed_tasks, index_rows = {}, []
stamp = '2026-10-11T00:00:00Z'
def closed_ref(rel):
    return {'path': rel, 'sha256': sha((closed / rel).read_bytes())}
verifier_file = dict(closed_ref('verify.sh'), state='file', mode='100644')
def original_task(tid):
    return {'id': tid, 'title': 'Inspect source evidence', 'state': 'review', 'updated': stamp,
            'created': stamp, 'depends': [], 'provider': 'codex', 'role': 'analysis-worker',
            'claimed_by_participant': main, 'lease_epoch': 1, 'lease_id': 'lease_' + '2' * 32,
            'repair_count': 0, 'repair_artifact': '', 'executor_id': '', 'executor_soul_sha256': '',
            'autopilot_owner_id': '', 'verify': 'bash verify.sh', 'artifact': '.oms/artifacts/' + tid + '.md'}
def add_completion(number):
    tid = 'closed-%03d' % number
    original = original_task(tid)
    (closed / original['artifact']).write_text('Source evidence for ' + tid + '\n', encoding='utf-8')
    artifact = closed_ref(original['artifact'])
    row = dict(event_id='evt-' + tid, kind='call-import', exit=0, artifact=artifact['path'],
               artifact_sha256=artifact['sha256'], task_id=tid, plan_id=plan_id)
    index_rows.append(row)
    bundle = {'schema': 1, 'kind': 'main-research-accepted', 'plan_id': plan_id, 'task_id': tid,
              'task_sha256': receipt_codec['digest'](original), 'plan_sha256': '3' * 64,
              'state': original['state'], 'lease_id': original['lease_id'],
              'owner': {'provider': 'codex', 'participant': main, 'parent_provider': 'codex'},
              'original_contract': receipt_codec['projection'](original), 'review_summary': 'Reviewed source evidence',
              'reviewed_obligations': ['inspect'], 'requirements': [{
                  'id': 'inspect', 'obligation': original['title'], 'deliverable': 'Source findings',
                  'source_paths': ['verify.sh'], 'evidence': [artifact]}],
              'base_commit': '4' * 40, 'source_envelope': ['verify.sh'], 'source_manifest': [verifier_file],
              'verifier': {'command': 'bash verify.sh', 'files': [verifier_file],
                           'adopted': False, 'review_summary': 'Reviewed frozen verifier'},
              'reviewer': {'kind': 'native-panel-main'},
              'main_research': {'classification': 'research-only', 'provenance': 'main-reviewed-external',
                  'artifact': artifact, 'index_event_id': row['event_id'], 'original_deliverable': original['title'],
                  'read_only_contract': 'original-report-only', 'original_producer_status':
                  {key: row.get(key) for key in ('kind', 'exit', 'attempt_id', 'task_id', 'plan_id', 'base_sha')}}}
    bundle_sha = sha(encoded(bundle))
    reviewer = {'schema': 1, 'kind': 'native-panel-main', 'participant': main, 'parent_provider': 'codex',
                'room_id': room_id, 'attempt_id': main, 'bundle_sha256': bundle_sha}
    evidence = {'index': row, 'artifact': artifact, 'provenance': 'main-reviewed-external',
                'child_provenance_asserted': False, 'reviewer': reviewer}
    snapshot = {'head': '4' * 40, 'base_commit': '4' * 40,
                'manifest_sha256': sha(encoded(bundle['source_manifest'])), 'source_identities': {},
                'verifier_command_sha256': sha(b'bash verify.sh'), 'repository_sha256': namespace,
                'evidence': evidence, 'evidence_identities': {}, 'provenance': provenance}
    verification = {'exit': 0, 'command_sha256': snapshot['verifier_command_sha256'],
                    'output_sha256': sha(b''), 'output_bytes': 0}
    verification['proof_sha256'] = sha(encoded(dict(verification, snapshot_sha256=sha(encoded(snapshot)))))
    receipt = {'schema': 1, 'kind': 'oms-plan-completion', 'completion_kind': 'main-research-accepted',
               'bundle_sha256': bundle_sha, 'bundle': bundle, 'bundle_raw': encoded(bundle).decode(),
               'original_task': original, 'original_plan_sha256': bundle['plan_sha256'],
               'snapshot': snapshot, 'verification': verification, 'ts': stamp}
    rel = '.oms/plan/completions/' + sha(encoded(receipt)) + '.json'
    (closed / rel).write_bytes(encoded(receipt))
    closed_tasks[tid] = core['completion_task'](receipt, closed_ref(rel))
    return closed / rel
first_receipt = add_completion(0)
for number in range(1, 125):
    add_completion(number)
for number in range(125, 250):
    tid = 'closed-%03d' % number
    original = dict(original_task(tid), state='ready')
    cancellation = {'schema': 1, 'kind': 'operator-cancellation', 'ts': stamp, 'operator_consent': 'cancel',
                    'reason': 'Reviewed cancellation', 'state': 'ready', 'original_updated': stamp,
                    'lease_id_sha256': sha(original['lease_id'].encode()), 'plan_sha256': '3' * 64,
                    'task_sha256': sha(json.dumps(original, sort_keys=True, ensure_ascii=False,
                                                separators=(',', ':'), allow_nan=False).encode()),
                    'decision_artifact': 'decision.md', 'decision_artifact_sha256': closed_ref('decision.md')['sha256']}
    closed_tasks[tid] = dict(original, state='cancelled', cancellation=cancellation)
closed_plan = closed / '.oms/plan/tasks.json'
def save_closed():
    closed_plan.write_text(json.dumps({'schema': 3, 'plan_id': plan_id, 'tasks': closed_tasks}), encoding='utf-8')
    (closed / '.oms/artifacts/index.jsonl').write_bytes(b''.join(encoded(row) for row in index_rows))
save_closed()
readers = [('round3', cache.read_plan_list)]
if len(sys.argv) > 3:
    # Comparative timing is an explicit fixture benchmark, not a repeated CI
    # baseline. Ordinary regression runs require no historical Git objects.
    previous = {'__file__': str(Path(sys.argv[1]) / 'panel_cache.py')}
    exec(Path(sys.argv[3]).read_bytes(), previous)
    readers = [('round2', previous['read_plan_list']), ('round3', cache.read_plan_list)]
for label, reader in readers:
    (closed / cache.DIRECTORY / 'plan-list.json').unlink(missing_ok=True)
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan unchanged %s: %s' % (label, json.dumps(result)))
add_completion(250)
save_closed()
for label, reader in readers:
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan added %s: %s' % (label, json.dumps(result)))
os.utime(first_receipt, ns=(first_receipt.stat().st_atime_ns, first_receipt.stat().st_mtime_ns + 1_000_000))
for label, reader in readers:
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan receipt-touched %s: %s' % (label, json.dumps(result)))
with patch.object(cache, '_plan_inputs', side_effect=AssertionError('dependencies reread on a cache hit')), \
        patch.object(Path, 'iterdir', side_effect=AssertionError('namespace walked on a cache hit')):
    assert len(cache.read_plan_list(closed, lambda: None)['tasks']) == 251
# External HOME/config aliases are normal on macOS and Fedora Atomic. Keep
# their resolved targets and detect retargeting without accepting repo symlinks.
outside = Path(sys.argv[2]) / 'external-home'
outside.mkdir()
(outside / '.gitconfig').write_text('', encoding='utf-8')
alias = Path(sys.argv[2]) / 'home-alias'
alias.symlink_to(outside, target_is_directory=True)
with patch.dict(os.environ, HOME=str(alias), GIT_CONFIG_GLOBAL=str(alias / '.gitconfig')):
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], result
    envelope = {'inputs': cache._plan_inputs(closed)}
    os.utime(outside / '.gitconfig', None)
    assert not cache._plan_inputs_current(envelope), 'external config touch was missed'
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], result
    envelope = {'inputs': cache._plan_inputs(closed)}
    replacement_home = Path(sys.argv[2]) / 'replacement-home'
    replacement_home.mkdir()
    (replacement_home / '.gitconfig').write_text('', encoding='utf-8')
    alias.unlink()
    alias.symlink_to(replacement_home, target_is_directory=True)
    assert not cache._plan_inputs_current(envelope), 'external alias retarget was missed'
repo_alias = closed / 'unsafe-alias'
repo_alias.symlink_to(outside, target_is_directory=True)
try:
    cache._plan_input_stamp(repo_alias)
except ValueError:
    pass
else:
    raise AssertionError('repository symlink became cacheable')
repo_alias.unlink()
# Check parser parity with the engine definition without running its CLI body.
import ast
engine = ast.parse((root / 'scripts/lib/agent_plan_core.py').read_text())
parser = next(node for node in engine.body if isinstance(node, ast.FunctionDef) and node.name == 'claim_ttl_for')
for raw_default in ('', '0', '0010', '3600', '+10', ' 10', '١٠'):
    default = int(raw_default) if raw_default and all('0' <= digit <= '9' for digit in raw_default) else 3600
    namespace = {'CLAIM_TTL': default}
    exec(compile(ast.Module(body=[parser], type_ignores=[]), '<engine TTL>', 'exec'), namespace)
    with patch.dict(os.environ, OMS_PLAN_CLAIM_TTL=raw_default):
        for value in ('', '0', '0010', '١٠', '+10', ' 10', 10, None):
            task = {'ttl': value}
            assert cache._plan_claim_ttl(task) == namespace['claim_ttl_for'](task), (raw_default, value)
# Discovery pays the commit namespace resolver once and guards only the pushed
# ref chain (plus replacement refs), not unrelated refs or the POSIX index.
receipt_paths = [first_receipt, next(path for path in (closed / '.oms/plan/completions').iterdir() if path != first_receipt)]
original_receipts = [path.read_bytes() for path in receipt_paths]
land_directory = Path(sys.argv[2]) / 'land-receipts'
land_directory.mkdir()
commit = '5' * 40
(land_directory / (commit + '-fixture.json')).write_text(json.dumps({'remote': 'origin', 'target': 'main'}))
remote_ref = closed / '.git/refs/remotes/origin/main'
remote_ref.parent.mkdir(parents=True)
resolved_ref = remote_ref.with_name('accepted')
remote_ref.write_text('ref: refs/remotes/origin/accepted\n')
resolved_ref.write_text(commit + '\n')
try:
    for path, raw in zip(receipt_paths, original_receipts):
        receipt = json.loads(raw)
        receipt['completion_kind'] = 'satisfied-by'
        receipt['bundle']['successor'] = {'product_kind': 'commit', 'commit_sha': commit}
        path.write_bytes(encoded(receipt))
    from unittest.mock import Mock
    resolver = Mock(return_value=str(land_directory))
    with patch.object(cache.runpy, 'run_path', return_value={'canonical_land_directory': resolver}) as loader:
        dependencies = cache._plan_inputs(closed)
    assert loader.call_count == resolver.call_count == 1, 'commit namespace resolved per receipt'
    paths = {item[0] for item in dependencies}
    assert {str(remote_ref), str(resolved_ref), str(closed / '.git/packed-refs')}.issubset(paths)
    envelope = {'inputs': dependencies}
    os.utime(resolved_ref, ns=(resolved_ref.stat().st_atime_ns, resolved_ref.stat().st_mtime_ns + 1_000_000))
    assert not cache._plan_inputs_current(envelope), 'resolved Git ref touch was missed'
    assert str(closed / '.git/refs/heads') not in paths, 'unrelated refs were stamped'
    assert (str(closed / '.git/index') in paths) == (os.name == 'nt')
finally:
    for path, raw in zip(receipt_paths, original_receipts):
        path.write_bytes(raw)
    remote_ref.unlink()
    resolved_ref.unlink()

for changed in (closed / 'decision.md', closed / '.oms/artifacts/closed-000.md',
                closed / provenance['capture']['path'], closed / provenance['room']['path'],
                closed / 'verify.sh'):
    os.utime(changed, ns=(changed.stat().st_atime_ns, changed.stat().st_mtime_ns + 1_000_000))
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], (changed, result)
# A changed dependency reaches validation and its rejection does not reuse the
# previous successful board value or publish a failed collection.
(closed / 'decision.md').write_text('Changed decision bytes!\n', encoding='utf-8')
def collect_closed():
    result = real_run(['bash', str(root / 'scripts/oms'), 'agent-plan', '--repo', str(closed), 'list', '--json'],
                      capture_output=True, text=True, timeout=15, env=fixture_env)
    if result.returncode:
        raise ValueError(result.stderr)
    return json.loads(result.stdout)
for tick in range(2):
    try:
        cache.read_plan_list(closed, collect_closed)
    except ValueError as error:
        assert 'cancellation decision artifact digest changed' in str(error), error
    else:
        raise AssertionError('changed decision bypassed actual validation')

# The managed patch reader's private certificate/anchor are validation inputs,
# even though their bytes live outside the repository. Probe dependency discovery
# directly; this altered receipt is never presented as an accepted completion.
import shutil
capture = core['CAPTURE']
source_patch = closed / 'fixture.patch'
source_patch.write_bytes(b'fixture patch bytes\n')
private_patch = capture['allocate'](str(closed), str(source_patch), 'land-cache-fixture', sha(source_patch.read_bytes()))
private_dir = Path(private_patch).parent
receipt_bytes = first_receipt.read_bytes()
try:
    publication = closed / '.oms/landing-patches/land-cache-fixture.patch'
    publication.parent.mkdir()
    capture['publish'](str(closed), private_patch, str(publication))
    altered = json.loads(receipt_bytes)
    altered['bundle']['requirements'][0]['evidence'].append(closed_ref('.oms/landing-patches/land-cache-fixture.patch'))
    first_receipt.write_bytes(encoded(altered))
    dependencies = cache._plan_inputs(closed)
    paths = {item[0] for item in dependencies}
    assert str(private_dir / 'owner.json') in paths and str(private_dir / 'publication.anchor') in paths, paths
    assert str(closed / '.oms/landings.jsonl') in paths
    for changed in (private_dir / 'owner.json', private_dir / 'publication.anchor'):
        envelope = {'inputs': cache._plan_inputs(closed)}
        os.utime(changed, ns=(changed.stat().st_atime_ns, changed.stat().st_mtime_ns + 1_000_000))
        assert not cache._plan_inputs_current(envelope), 'private patch generation input was missed'
finally:
    first_receipt.write_bytes(receipt_bytes)
    shutil.rmtree(private_dir)

# The board still selects its own room, and result queries share the same
# generation without retaining scoped answer text or skipping digest checks.
path.unlink()
with patch.object(panel, 'dashboard', return_value=({'repo': {'name': 'fixture'}}, 0)) as dashboard, \
        patch.object(panel.room, 'status', side_effect=lambda repo, ident: {'id': ident}), \
        patch.object(panel.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{}', '')):
    assert panel.snapshot(project, 'one')[0]['room']['id'] == 'one'
    assert panel.snapshot(project, 'two')[0]['room']['id'] == 'two'
    assert dashboard.call_count == 1
    with panel_results._lock(project, 'panel-cache-dashboard', timeout=0):
        plan.write_text('third generation', encoding='utf-8')
        # Inputs that change while another board collects never turn this board into "collection degraded".
        state, status = panel.snapshot(project, 'two')
        assert status == 0 and state['room']['id'] == 'two', state
    assert panel.snapshot(project, 'two')[0]['room']['id'] == 'two'
    assert dashboard.call_count == 3
with patch.object(panel_results, '_records', return_value=([], [])) as queries:
    assert panel_results.results(project, _shared=True)['rows'] == []
    assert panel_results.results(project, _shared=True)['rows'] == []
    assert queries.call_count == 1
    plan.write_text('fourth generation', encoding='utf-8')
    panel_results.results(project, _shared=True)
    assert queries.call_count == 2

# One board collection shares the full projection with D/E/F/G and the H tmux path,
# even when another writer invalidates every persistent cache during the tick.
from copy import deepcopy
import panel_chats
import panel_host
import panel_metrics

task_ids = ['tick-write', 'tick-wait']
plan.write_text(json.dumps({'plan_id': 'tick-plan', 'tasks': [{'id': t} for t in task_ids]}))
tick_room = {'id': 'tick-room', 'participants': [
    {'participant': 'tick-main', 'role': 'main', 'provider': 'codex', 'joined': True},
    {'participant': 'tick-worker', 'role': 'worker', 'provider': 'codex', 'joined': True, 'parent': 'tick-main'}]}
main = {'attempt_id': 'tick-main', 'terminal': False, 'state': 'working', 'tool': 'panel-main', 'provider': 'codex',
        'refs': {'panel_room_id': 'tick-room', 'panel_room_participant': 'tick-main', 'panel_role': 'main',
                 'panel_host': 'herdr', 'panel_socket_digest': 'fixture',
                 'panel_pane_id': 'tab:p1', 'panel_terminal_id': 'terminal'}}
write = {'attempt_id': 'tick-worker', 'task_id': 'tick-write', 'terminal': True, 'state': 'done',
         'refs': {'panel_room_id': 'tick-room', 'panel_room_participant': 'tick-worker', 'panel_role': 'worker'}}
listed = [main] + [{'attempt_id': 'old-%s' % i, 'terminal': True} for i in range(300)] + [write]
artifacts = [{'kind': 'delegate', 'task_id': 'tick-write', 'attempt_id': 'tick-worker', 'patch': 'change.patch'},
             {'kind': 'panel-result', 'task_id': 'tick-wait'}]
tick_state = {'repo': {'name': 'fixture'}, 'plan': {'present': True}, 'attempts': {'recent': [
    dict(write, panel={'room_id': 'tick-room', 'room_participant': 'tick-worker', 'role': 'worker', 'access': 'write'}),
    {'task_id': 'tick-wait', 'state': 'failed'}]}}
counts = {'events': 0, 'index': 0}
failure = [False]
def tick_run(command, **kwargs):
    value = {}
    if 'agent-events' in command:
        counts['events'] += 1
        assert command[-2:] == ['list', '--json'], command
        if failure[0]:
            raise subprocess.TimeoutExpired(command, 5)
        value = listed
        plan.write_text(plan.read_text() + '\n')
    elif 'artifact-index' in command:
        counts['index'] += 1
        value = {'rows': artifacts}
        plan.write_text(plan.read_text() + '\n')
    elif 'agent-plan' in command:
        value = {'tasks': [{'id': t} for t in task_ids]}
    elif command[:2] == ['tmux', 'list-windows']:
        return subprocess.CompletedProcess(command, 0, f'@1\ttick-main\ttick-room\t%1\ttick-main\t{project.resolve()}\t$1\n', '')
    elif command[:2] == ['tmux', 'list-panes']:
        return subprocess.CompletedProcess(command, 0, '%1\t@1\n', '')
    return subprocess.CompletedProcess(command, 0, json.dumps(value), '')
def tick_result(repo, row):
    return {'outcome': 'accepted', 'revision': 'a' * 24, 'evidence': [],
            'plan_binding': panel_results._plan_bindings(repo, task_ids)['tick-wait'],
            'verification': {'status': 'passed', 'exit': 0}}
with patch.object(panel, 'dashboard', side_effect=lambda *a, **k: (deepcopy(tick_state), 0)), \
        patch.object(panel.room, 'status', side_effect=lambda *a: deepcopy(tick_room)), \
        patch.object(panel, 'managed_session', return_value=False), \
        patch.object(panel, 'other_rooms', return_value=([], False)), \
        patch.object(panel.subprocess, 'run', side_effect=tick_run), \
        patch.object(panel_chats.shutil, 'which', return_value='/fixture/tmux'), \
        patch.object(panel_chats, 'current_models', return_value={}), \
        patch.object(panel_results, '_load_result', side_effect=tick_result), \
        patch.object(panel_results, 'completed_tasks', wraps=panel_results.completed_tasks) as completed, \
        patch.object(panel, 'outcomes', wraps=panel.outcomes) as outcomes, \
        patch.object(panel_results, 'pending_admissions', wraps=panel_results.pending_admissions) as pending, \
        patch.object(panel_chats, 'windows', wraps=panel_chats.windows) as windows, \
        patch.dict(os.environ, {'OMS_ROOM_ID': 'tick-room', 'TMUX': 'fixture'}):
    for read in (lambda: panel.snapshot(project, 'tick-room')[0],
                 lambda: panel.read_panel(project, 'codex', None, False)[0]):
        counts.update(events=0, index=0)
        report = read()
        assert counts == {'events': 1, 'index': 1}, counts
        if 'dead_mains' in report:
            assert [row['attempt'] for row in report['dead_mains']['unverifiable']] == ['tick-main']
        assert report['completed_tasks'].keys() == {'tick-wait'}, report
        assert report['finalized'] == {'tick-wait': 'accepted'}, report
        assert report['awaiting_admission'] == ['tick-worker'], report
        assert report['room_attempts']['tick-worker']['state'] == 'done', report
    assert completed.call_count == outcomes.call_count == pending.call_count == 2
    tick = windows.call_args.kwargs['_tick']
    assert windows(project, 'tick-room', tick_room, _tick=tick) == {
        'tick-main': [{'window': '@1', 'pane': '%1', 'session': '$1'}]}
    assert tick.records()[0] == listed[-300:], 'results lost their latest-300 coverage'
    # The Herdr H reader uses the same full rows, including the old active main.
    with patch.object(panel_host, 'herdr_active', return_value=True), \
            patch.object(panel_host, 'socket_digest', return_value='fixture'), \
            patch.object(panel_host, 'pane', return_value={
                'pane_id': 'tab:p1', 'terminal_id': 'terminal', 'workspace_id': 'workspace',
                'tab_id': 'tab', 'cwd': str(project.resolve())}):
        assert panel_host.targets(project, 'tick-room', tick_room, _tick=tick).get('tick-main')
    assert counts['events'] == 1
    # Action-time navigation has no tick: it still probes --active for itself.
    def action_run(command, **kwargs):
        if 'agent-events' in command:
            return subprocess.CompletedProcess(command, 0, json.dumps([main]), '')
        return tick_run(command, **kwargs)
    with patch.object(panel_chats.subprocess, 'run', side_effect=action_run) as action:
        assert panel_chats.windows(project, 'tick-room', tick_room).get('tick-main')
        assert any('--active' in c.args[0] for c in action.call_args_list)
    # A failed common read stays unknown for every consumer and is retried next tick.
    failure[0] = True
    for i in range(2):
        plan.write_text(plan.read_text() + '\n')
        counts.update(events=0, index=0)
        report = panel.read_panel(project, 'codex', None, False)[0]
        assert counts['events'] == 1 and counts['index'] == 0, counts
        assert report['completed_tasks'] == {} and 'room_attempts' not in report, report
        assert report['awaiting_admission'] == [], report

# An owner decision older than results()' twenty-task window still reaches the board through outcomes().
many = [{'kind': 'panel-result', 'task_id': 'task-%02d' % n} for n in range(30)]
with patch.object(panel_results, '_records', return_value=([], many)), \
        patch.object(panel_results, '_load_result', side_effect=lambda repo, row: {'outcome': 'completed'}):
    assert panel_results.outcomes(project, {'task-00', 'task-29', 'missing', '../bad'}) == {'task-00': 'completed', 'task-29': 'completed'}
    assert panel_results.outcomes(project, set()) == {}

def changing():
    plan.write_text('changed while collecting', encoding='utf-8')
    return {'number': 42}
# A read that overlapped a write is this board's answer, but it is not published for the others.
assert cache.read_shared(project, 'race', changing) == {'number': 42}
assert not (project / cache.DIRECTORY / 'race.json').exists()
# Mains' activity heartbeats change all the time and are read directly; they do not invalidate shared reads.
before = cache.fingerprint(project)
(project / '.oms/hooks/panel-activity').mkdir(parents=True, exist_ok=True)
(project / '.oms/hooks/panel-activity/main.json').write_text('{}', encoding='utf-8')
assert cache.fingerprint(project) == before
# Watch loop and control window keep the room board when a later read fails.
good = ({'repo': {'name': 'fixture'}, 'room': {'id': 'one', 'participants': []}}, 0)
bad = ({'repo': {'name': 'fixture'}, 'collection': {'ok': False}}, 1)
held = {}
assert panel.hold_state(held, *good, 'one') == (good[0], 0, None)
held['at'] -= 12
state, status, notice = panel.hold_state(held, *bad, 'one')
assert state is good[0] and status == 0 and notice.startswith('State refresh failed; showing data from 1') and notice.endswith('s ago'), notice
panel.hold_state(held, *bad, 'one')
assert 'keeps failing (3 in a row)' in panel.hold_state(held, *bad, 'one')[2]
assert panel.hold_state(held, *bad, 'two') == (bad[0], 1, None), 'another room drew the held board'
assert panel.hold_state({}, *bad, 'one') == (bad[0], 1, None), 'a board that never read kept today fallback'
# A failed first read or room switch says the room is unknown, never an empty dashboard.
from panel_view import render as draw
from dashboard_projection import display_width
healthy = {'repo': {'name': 'fixture'}, 'collection': {'ok': True}, 'room': {'id': 'one', 'participants': []}}
for view in ('auto', 'graph', 'tree'):
    for menu in (False, True):
        for width, height in ((100, 28), (38, 40), (24, 18), (12, 10), (2, 8)):
            stale = {'selected': ('chat', 'old'), 'primary': ('chat', 'old'), 'preview': {'target': ('result', 'r')},
                     'surface': 'graph', 'hits': [{'y': 1, 'action': ('chat', 'old')}], 'items': [('chat', 'old')],
                     'selected_row': 3, 'row_ids': ['old'], 'detail': 'old', 'room_id': 'one'}
            drawn = draw(bad[0], 'codex', width, height, menu=menu, view=view, navigation=stale)
            assert len(drawn.splitlines()) <= height - int(menu) and all(display_width(line) <= width for line in drawn.splitlines()), drawn
            assert any(word in drawn for word in ('unavailable', 'unknown', 'Unknown')) or width < 12 or height - int(menu) < 2, (view, menu, width, height, drawn)
            assert not any(word in drawn for word in ('Collection degraded', 'REPO ACCEPT', 'No active', 'Goal:', 'Joined mains', 'Messages:', 'Nothing needs you')), drawn
            assert not any(stale.get(key) for key in ('selected', 'primary', 'preview', 'surface', 'hits', 'items', 'selected_row', 'row_ids', 'detail')), stale
            assert stale['room_id'] == 'one'
            assert menu is False or width < 28 or height < 20 or '[1] Claude' in drawn and '[9] Refresh' in drawn, drawn
assert 'Press 9 to read again' in draw(bad[0], 'codex', 100, 28, menu=True)
assert '\033' in draw(bad[0], 'codex', 100, 28, color=True)
assert 'unavailable' not in draw(healthy, 'codex', 100, 28, menu=True).split('Actions')[0]
assert 'Collection degraded' in draw(dict(bad[0], room={'id': 'one', 'participants': [], 'error': 'x'}), 'codex', 100, 28)
assert bad[0] == {'repo': {'name': 'fixture'}, 'collection': {'ok': False}}
# The full reader must yield to an unknown-room screen on first failure.
with patch.object(panel, 'terminal_style', return_value=(False, False, False)), patch.object(panel, 'managed_session', return_value=False), patch.object(panel.os, 'get_terminal_size', return_value=os.terminal_size((100, 28))):
    reader_nav = {'detail': 'old-result', 'selected_row': 'old-row', 'row_ids': ['old-row']}
    unknown_text, unused, capable = panel.frame_view(project, 'codex', bad[0], navigation=reader_nav, view='tree')
    assert 'Room state unavailable' in unknown_text and not reader_nav.get('detail'), (unknown_text, reader_nav)

assert 'Room state unavailable' not in draw(good[0], 'codex', 100, 28, navigation={'selected': ('chat', 'old')})
board, held_nav = {}, {}
panel.hold_state(board, *good, 'one')
shown, unused, hint = panel.hold_state(board, *bad, 'two')
assert shown is bad[0] and hint is None and 'Room state unavailable' in draw(shown, 'codex', 100, 28, navigation=held_nav)
assert panel.hold_state(board, *good, 'two')[0] is good[0] and 'Room state unavailable' not in draw(good[0], 'codex', 100, 28, navigation=held_nav)
navigation = {}
panel.stale_notice(navigation, notice)
assert navigation['notice'] == notice
panel.stale_notice(navigation, None)
assert 'notice' not in navigation
with patch.object(panel, 'snapshot', side_effect=[good, bad]), patch.object(panel, 'frame_view', return_value=('', os.terminal_size((80, 24)), False)) as frame:
    shown, navigation = {}, {}
    for unused in range(2):
        shown.pop('state', None)
        panel.show(project, 'codex', navigation=navigation, cache=shown)
    assert frame.call_args[0][2] is good[0] and navigation['notice'].startswith('State refresh failed'), 'control window dropped the room'
for relative in ('.oms/lifecycle/events.jsonl', '.oms/hooks/events.jsonl', '.oms/threads/room.jsonl'):
    source = project / relative
    source.parent.mkdir(exist_ok=True)
    before = cache.fingerprint(project)
    source.write_text('record', encoding='utf-8')
    assert cache.fingerprint(project) != before, relative
git_dir = project / 'worktree-git'
git_dir.mkdir()
(project / '.git').write_text('gitdir: worktree-git\n', encoding='utf-8')
before = cache.fingerprint(project)
(git_dir / 'HEAD').write_text('ref: refs/heads/fixture\n', encoding='utf-8')
assert cache.fingerprint(project) != before, 'worktree HEAD was omitted'
PY
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
import hashlib
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
os.chdir(temporary)
# A run started inside an OMS panel must not inherit that panel's tmux or room binding.
for name in [n for n in os.environ if n in ("TMUX", "TMUX_PANE") or n.startswith(("OMS_PANEL_", "OMS_ROOM_"))]:
    del os.environ[name]
sys.path.insert(0, str(root / "scripts/lib"))
import terminal_panel as panel
from panel_routing import allocate
from dashboard_projection import display_width
from panel_view import render
from panel_input import passive_click
from room_view import render_graph, plan_rows as render_plan_rows
from panel_view import wrapped
import panel_results

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
assert "[1] Claude" in render(view, "codex", 38, 28, menu=True)
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
# One bottom box with tabs (Detail, Plan, Debate, Messages) replaces the d and m views and the messages card.
import panel_debate
import panel_messages
import panel_chats
import room_view
from panel_input import TerminalInput, choose
from terminal_panel import tab_event
assert panel_debate._sentence("I'm keeping (a), the `!` marker is cheaper. More.") == "I'm keeping (a), the `!` marker is cheaper."
assert panel_debate._sentence('He said "stop! now" ok. More') == 'He said "stop! now" ok.'
assert [panel_debate.choice(t) for t in ("저는 (a)를 고릅니다.", "B: safest", "A good idea", "Yes, ship", "revise", "none")] == \
    ["(a)", "(b)", None, "yes", "revise", None]
assert TerminalInput().decode(b"0\t") == [("0",), ("tab",)] and TerminalInput().decode(b"dm") == []
msg_members = [{"participant": "m1", "role": "main", "joined": True, "model": "claude-opus-5-5", "seq": 1, "provider": "claude", "attempt": "a1"},
               {"participant": "m2", "role": "main", "joined": True, "model": "gpt-6.1-sol", "seq": 2, "provider": "codex", "attempt": "a2"},
               {"participant": "m3", "role": "main", "joined": True, "model": "gpt-6-luna", "seq": 3, "provider": "codex", "attempt": "a3"}]
msg_log = [{"id": "n%d" % i, "sender": "m1" if i % 3 else "m3", "recipient": "m2" if i % 3 else "m1", "targets": ["m2" if i % 3 else "m1"],
            "message_kind": "question" if i % 5 == 0 else "note", "ts": "2026-10-0%dT10:%02d:00Z" % (6 if i < 5 else 7, i),
            "pending_for": ["m2"] if i == 29 else [], "text": "line %d\nsecond \x1b[31mred" % i} for i in range(30)]
msg_log[28]["text"] = "long " + "word " * 80 + "END\n\nparagraph two"
ask = ".oms/artifacts/ask/"
msg_report = {"collection": {"ok": True}, "attempts": {"active_recent": [
    {"attempt_id": "a%d" % n, "state": "working", "panel": {"room_id": "r", "room_participant": "m%d" % n, "role": "main"}} for n in (1, 2, 3)],
    "recent": [{"attempt_id": "c1", "tool": "panel-council", "task_id": "decide", "state": "done", "updated_at": "2026-10-07T10:00:00Z",
                "parent_attempt_id": "a2", "panel": {"role": "advisor", "label": "Panel attention signal", "room_id": "r", "room_participant": "m2"}}]},
              "goal": {"text": "Ship the tabbed panel", "source": "plan"},
              "room": {"id": "r", "title": "T", "participants": msg_members, "message_count": 30, "messages": msg_log[-12:],
                       "pairs": [{"recipient": "m2", "pending": 2}],
                       "repo_tasks": [{"id": "t-blk", "title": "Blocked one", "state": "blocked", "provider": "codex", "verify": "make a"},
                                      {"id": "t-rdy", "title": "Ready one", "state": "ready", "provider": "", "verify": "make b"},
                                      {"id": "t-run", "title": "Running one", "state": "running", "provider": "claude",
                                       "claimed_by_participant": "m1", "verify": "make c", "claim_expired": True, "claim_age_s": 32428},
                                      {"id": "t-ver", "title": "Verified one", "state": "done", "provider": "codex", "verify": "make d"},
                                      {"id": "t-rev", "title": "Review one", "state": "review", "provider": "claude", "verify": "make e"}]}}
seat_calls = [{"kind": "ask", "selected_model": model, "artifact": ask + "%s-%s-s-1%s.md" % (family, model, suffix), "answer": text}
              for suffix, texts in (("", ("Answer: 저는 (a)를 고릅니다. 이유는 단순합니다.", "Answer: I pick (c) first. Reasons follow.",
                                          "Answer: (b) is safest. More.", "VERDICT: proceed")),
                                    ("-r2", ("Answer: **저는 (a)를 고릅니다.** 그대로 유지합니다.\nFULLTEXT-SOL second line",
                                             "Answer: I keep my main point. I'm keeping (a), the `!` marker is cheaper. More.",
                                             "Answer: (b) stays safest. Still.", "VERDICT: revise\nAnswer: x")))
              for (family, model), text in zip((("codex", "gpt-6-sol"), ("claude", "claude-opus-5-5"), ("claude", "claude-fable-5-1"),
                                                ("codex", "gpt-6-astra")), texts)]
debate_rows = {"rows": [{"title": "Panel attention signal", "summary": "Ship (a)", "outcome": "accepted", "calls": seat_calls}],
               "synthesis": {"path": ask + "_synthesis-s-1.md", "text": "## Prompt\n```\n--- begin conversation context (prior turns) ---\nold turn\n--- end conversation context ---\n```\nQuestion: Should the panel flag attention with a color or a glyph? Pick one. Third.\n"
                             "## Output\n### Agreement\nTests first.\n### Dissent\n- **With Astra:** Order differs.\n- **With Sol:** Scope differs.\n"}}
before = deepcopy(msg_report)
BOX_CHARS = "╭╮│─╰╯•→—▸▾·"


def board(tab, width=80, height=60, **kwargs):
    nav = kwargs.pop("nav", None) or {"selected": ("chat", "m2"), "tab": tab}
    nav["tab"] = tab
    return nav, render(msg_report, "codex", width, height, view="graph", navigation=nav, main_attempt="a2", **kwargs)


# The strip: selected tab bracketed, unread count and new-debate dot, hits equal drawing at 80 and 157 columns and in ASCII.
for width in (80, 157):
    for glyphs in (True, False):
        nav, text = board("detail", width, 40, unicode=glyphs)
        rows = text.split("\n")
        assert len(rows) == 40 and "RECENT MESSAGES" not in text and "d Debates" not in text and "m Messages" not in text, text
        assert "Quit" in rows[-1] and "Tab Next tab" in rows[-1] or width == 80, rows[-1]
        strip_y = max(h["y"] for h in nav["hits"] if h["action"] == ("tab", "detail"))
        tabs = {h["action"][1]: h for h in nav["hits"] if h["action"][0] == "tab" and h["y"] == strip_y}
        assert list(tabs) == ["detail", "plan", "debate", "messages"], tabs
        for key, name in (("detail", "Detail"), ("plan", "Plan"), ("debate", "Debate"), ("messages", "Messages")):
            hit = tabs[key]
            seg = rows[hit["y"] - 1][hit["x1"] - 1:hit["x2"]]
            assert seg.strip("[] ").startswith(name) and seg.count("[") == (key == "detail"), (width, glyphs, key, seg)
        strip = rows[tabs["detail"]["y"] - 1]
        assert "[ Detail ]  Plan  Debate " + ("•" if glyphs else "*") + "  Messages 2 unread" in strip, strip
        assert strip.startswith("╭─ " if glyphs else "+- ") and (glyphs or text.isascii()), strip
# Tab cycles Detail > Plan > Debate > Messages and never moves by itself; clicks on a tab name select it.
nav, text = board("detail")
seen = []
for unused in range(4):
    assert tab_event(("tab",), nav)
    seen.append(nav["tab"])
assert seen == ["plan", "debate", "messages", "detail"], seen
nav, text = board("plan")
render(msg_report, "codex", 80, 60, view="graph", navigation=nav, main_attempt="a2")
assert nav["tab"] == "plan"
hit = next(h for h in nav["hits"] if h["action"] == ("tab", "messages"))
assert tab_event(("click", hit["x1"], hit["y"]), nav) and nav["tab"] == "messages"
assert choose(("click", hit["x1"], hit["y"]), nav) == ("tab", "messages") and nav["selected"] == ("chat", "m2")
assert nav["box"]["y1"] == hit["y"] and msg_report == before
# Plan: goal first, then completed, review, running, ready, blocked, with the claimant by board name or provider.
nav, plan = board("plan")
plan_rows = [line for line in plan.split("\n") if "Goal:" in line or " t-" in line]
assert "Goal: Ship the tabbed panel" in plan_rows[0], plan
assert [row.split(" · ")[0].split()[-1] for row in plan_rows[1:]] == ["t-ver", "t-rev", "t-run", "t-rdy", "t-blk"], plan_rows
assert "t-ver · done · Verified one" in plan and "Verified one · unclaimed" not in plan and "make d" not in plan and "t-run · running 9h stale · Running one · Opus 5.5 · make c" in plan, plan
assert "t-rev · review · Review one · unclaimed ·" in plan and "codex ·" not in plan and "claude ·" not in plan, plan
task_hits = [h["action"] for h in nav["hits"] if h["action"][0] == "task"]
assert task_hits == [("task", t) for t in ("t-ver", "t-rev", "t-run", "t-rdy", "t-blk")], task_hits
# Recorded completion metadata is visible only for terminal rows; unknown/malformed kinds stay plain done.
completion_report = deepcopy(msg_report)
completion_report["room"]["repo_tasks"] = [
    {"id": "legacy", "title": "Legacy", "state": "done"},
    {"id": "research", "title": "Research", "state": "done", "completion_kind": "research-accepted"},
    {"id": "main-read", "title": "Main READ", "state": "done", "completion_kind": "main-research-accepted"},
    {"id": "satisfied", "title": "Satisfied", "state": "done", "completion_kind": "satisfied-by"},
    {"id": "running-kind", "title": "Running", "state": "running", "completion_kind": "research-accepted"},
    {"id": "unknown-kind", "title": "Unknown", "state": "done", "completion_kind": "operator-proof"},
    {"id": "bad-kind", "title": "Bad", "state": "done", "completion_kind": {"type": "research-accepted"}},
]
completion_plan = "\n".join(row for row, _ in render_plan_rows(completion_report, {}, 120, True))
assert "legacy · done · Legacy" in completion_plan, completion_plan
assert "research · done (research) · Research" in completion_plan, completion_plan
assert "main-read · done (main research) · Main READ" in completion_plan, completion_plan
assert "satisfied · done (satisfied by) · Satisfied" in completion_plan, completion_plan
assert "running-kind · running · Running" in completion_plan, completion_plan
assert "unknown-kind · done · Unknown" in completion_plan and "bad-kind · done · Bad" in completion_plan, completion_plan
completion_hits = [action for _, action in render_plan_rows(completion_report, {}, 120, True) if action]
assert completion_hits == [("task", task_id) for task_id in ("legacy", "research", "main-read", "satisfied", "unknown-kind", "bad-kind", "running-kind")], completion_hits
assert next(row for row, action in render_plan_rows(completion_report, {}, 120, False) if action).startswith("+ ")
assert next(row for row, action in render_plan_rows(completion_report, {}, 120, True) if action).startswith("✓ ")
foot = plan.split("\n")[-1]
assert "wheel Scroll" in foot and "Tab Next tab" in foot and "Enter Chat" not in foot and "Ask advisor" not in foot, foot
assert "✓ t-ver" in plan and "○ t-rdy" in plan and "✗ t-blk" in plan, plan
assert board("plan", unicode=False)[1].isascii()
# Long plans count off-screen rows and reach the last actionable task through the existing wheel band.
long_report = deepcopy(msg_report)
long_report["room"]["repo_tasks"] = [{"id": "long-%02d" % i, "title": "Plan task %s" % i,
                                       "state": "ready" if i == 44 else "done"} for i in range(45)]
for glyphs in (True, False):
    long_nav = {"selected": ("chat", "m2"), "tab": "plan"}
    picture = render(long_report, "codex", 100, 36, view="graph", navigation=long_nav, unicode=glyphs)
    assert "hidden" in picture and "oms agent-plan list" in picture, picture
    band = next(b for b in long_nav["bands"] if b["name"] == "detail")
    for unused in range(15):
        choose(("scroll", 3, 5, band["y1"] + 1), long_nav)
    picture = render(long_report, "codex", 100, 36, view="graph", navigation=long_nav, unicode=glyphs)
    assert "long-44" in picture and "hidden" in picture and "oms agent-plan list" in picture, picture
    assert len(picture.splitlines()) == 36 and all(display_width(line) <= 100 for line in picture.splitlines())
# Debate: opener and time by board name and local clock, question, one line per seat, stance cut, choice changes only.
nav, text = board("debate")
assert "No debate opened by Opus 5.5 · a main opens one with oms panel --council" in board("debate", nav={"selected": ("chat", "m1")})[1]
empty_foot = board("debate", nav={"selected": ("chat", "m1")})[1].split("\n")[-1]
assert "Tab Next tab" in empty_foot and "Seat" not in empty_foot and "Target" not in empty_foot, empty_foot
render(msg_report, "codex", 80, 60, view="graph", navigation=nav, main_attempt="a2")
nav["debate_view"]["report"] = deepcopy(debate_rows)
nav, text = board("debate", nav=nav)
stamp = room_view.clock_stamp("2026-10-07T10:00:00Z")
flat = " ".join(re.sub("[│|]", " ", line) for line in text.split("\n")).split()
flat = " ".join(flat)
for needle in ("Panel attention signal · opened by Sol 6.1 · " + stamp + " · 4 of 4 seats answered · round 2 of 2",
               "Question: Should the panel flag attention with a color or a glyph? Pick one. Third.",
               "Sol 6 — 저는 (a)를 고릅니다.", "Opus 5.5 — I'm keeping (a), the `!` marker is cheaper.", "Fable 5.1 — (b) stays safest.",
               "Astra 6 — revise", "(changed in round 2: (c) → (a))", "(changed in round 2: proceed → revise)",
               "Agreement: Tests first.", "Disagreement: With Astra: Order differs.", "With Sol: Scope differs.", "Decision: Ship (a) (outcome: accepted)"):
    assert needle in flat, (needle, flat)
assert text.count("changed in round") == 2 and "Changed after" not in text and "opened by m2" not in text and "2026-10-07T" not in text, text
assert "FULLTEXT-SOL" not in text and "\x1b" not in text and "Debate •" not in text and "[ Debate ]" in text, text
nav["debate_view"]["report"]["synthesis"]["text"] = "## Output\nplain prose"
nav["debate_view"]["report"]["rows"][0].update(summary=None, outcome=None)
bare = board("debate", nav=nav)[1]
assert "Agreement" not in bare and "Disagreement" not in bare and "Decision: not finalized" in " ".join(bare.replace("│", " ").split()) and "oms panel --finalize" in bare, bare
assert "**" not in text and "begin conversation context" not in text, text
assert "Question: Panel attention signal" in bare
# Enter on a seat shows its full answer in the tab, Esc returns to the summary.
assert panel_debate.handle(("down",), nav) and panel_debate.handle(("up",), nav) and panel_debate.handle(("enter",), nav)
assert "round 2 answer" in board("debate", nav=nav)[1] and "FULLTEXT-SOL second line" in board("debate", nav=nav)[1]
# The full-seat view carries the selected round's truncation flag and source path.
full_nav = deepcopy(nav)
full_nav["debate_view"].update(seat=0, scroll=0)
base_call = dict(seat_calls[0], artifact=ask + "codex-gpt-6-sol-s-1-r1.md", answer="round 1 answer", answer_truncated=True)
round_two = dict(base_call, artifact=ask + "codex-gpt-6-sol-s-1-r2.md", answer="round 2 answer", answer_truncated=False)
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "source artifact:" not in board("debate", 80, 40, nav=full_nav)[1]
round_two["answer_truncated"] = True
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
notice = board("debate", 80, 40, nav=full_nav)[1]
assert "Answer was truncated" in notice and base_call["artifact"] not in notice and round_two["artifact"] in notice, notice
round_two["artifact"] = ask + ("x" * 80 + "/") * 2 + "x" * 120 + "-r2.md"
for glyphs in (True, False):
    notice = board("debate", 76, 40, nav=full_nav, unicode=glyphs)[1]
    assert round_two["artifact"] in "".join(line.strip(" │|") for line in notice.splitlines()), notice
    assert len(notice.splitlines()) == 40 and all(display_width(line) <= 76 for line in notice.splitlines())
round_two["answer_truncated"] = False
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "Answer was truncated" not in board("debate", 80, 40, nav=full_nav)[1]
round_two.pop("answer_truncated")
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "Answer was truncated" not in board("debate", 80, 40, nav=full_nav)[1]
same_round_replacement = dict(base_call, answer="replacement answer", answer_truncated=False)
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, same_round_replacement]
replacement_view = board("debate", 80, 40, nav=full_nav)[1]
assert "replacement answer" in replacement_view and "Answer was truncated" not in replacement_view, replacement_view
# Full answers remain scrollable through the retained source byte bound, including
# the tail of both a multiline answer and one long paragraph.
for answer_tail, answer_text in (("LINE-0499", "x\n" * 499 + "LINE-0499"),
                                 ("PARAGRAPH-END", "word " * 1800 + "PARAGRAPH-END"),
                                 ("LONG-PARAGRAPH-END", "x" * 12000 + "LONG-PARAGRAPH-END")):
    assert len(answer_text.encode("utf-8")) <= panel_results.MAX_RESULT
    full_nav = deepcopy(nav)
    full_nav["debate_view"].update(seat=0, scroll=0)
    full_nav["debate_view"]["report"]["rows"][0]["calls"] = [
        dict(seat_calls[0], artifact=ask + "codex-gpt-6-sol-s-1.md", answer=answer_text, answer_truncated=False)]
    visible = board("debate", 80, 40, nav=full_nav)[1]
    assert answer_tail not in visible, visible
    assert tab_event(("scroll", panel_results.MAX_RESULT), full_nav)
    visible = board("debate", 80, 40, nav=full_nav)[1]
    assert answer_tail in "".join(line.strip(" │") for line in visible.splitlines()), (answer_tail, visible)
    assert len(visible.splitlines()) == 40 and all(display_width(line) <= 80 for line in visible.splitlines())
assert tab_event(("escape",), nav) and nav["debate_view"]["seat"] is None and "FULLTEXT-SOL" not in board("debate", nav=nav)[1]
assert not tab_event(("escape",), nav)
nav["tab"] = "debate"
hit = next(h for h in nav["hits"] if h["action"] == ("seat", 1))
assert tab_event(("click", 5, hit["y"]), nav) and nav["debate_view"]["seat"] == 1
nav["debate_view"]["seat"] = None
ascii_debate = board("debate", nav=nav, unicode=False)[1]
assert not any(ch in ascii_debate for ch in BOX_CHARS), ascii_debate
assert "(changed in round 2: (c) -> (a))" in " ".join(ascii_debate.replace("|", " ").split()), ascii_debate
# Messages: names, local times, unread marks, filters, in-place full text; reading never writes; the dot clears once the Debate tab was seen.
nav, shown = board("messages", 110, 24)
assert "Filter: all" in shown and "line 29" in shown and "Opus 5.5 main → Sol 6.1 main" in shown and "· unread" in shown and "\x1b" not in shown, shown
assert "loading earlier" not in shown, shown
nav["messages_view"]["reading"] = True
assert "loading earlier messages" in board("messages", 110, 24, nav=nav)[1]
nav["messages_view"]["reading"] = False
assert "Debate •" in board("detail", nav=nav)[1]
viewed = board("debate")[0]
assert "Debate •" not in board("detail", nav=viewed)[1] and "  Debate  " in board("detail", nav=viewed)[1]
nav["messages_view"]["log"] = msg_log
board("messages", 110, 40, nav=nav)
assert tab_event(("up",), nav) and tab_event(("enter",), nav)
opened_message = board("messages", 80, 60, nav=nav)[1]
assert "END" in opened_message and "paragraph two" in opened_message and opened_message.count("word") > 70, opened_message
assert tab_event(("enter",), nav) and nav["messages_view"]["open"] is None
# Esc closes an open message and nothing else; with none open it is left to the board.
assert tab_event(("enter",), nav) and nav["messages_view"]["open"] is not None
assert tab_event(("escape",), nav) and nav["messages_view"]["open"] is None and not tab_event(("escape",), nav)
nav["selected"] = ("chat", "m3")
assert tab_event(("right",), nav)
narrowed = board("messages", 110, 40, nav=nav)[1]
assert "Filter: to/from Luna 6 main" in narrowed and "Sol 6.1 main" not in narrowed and "line 27" in narrowed and "line 29" not in narrowed, narrowed
assert board("messages", unicode=False)[1].isascii() and msg_report == before, "reading changed the report"
nav, listed = board("messages", 110, 40)
hit = next(h for h in nav["hits"] if h["action"][0] == "message")
assert listed.split("\n")[hit["y"] - 1].lstrip().startswith("│") and "line" in listed.split("\n")[hit["y"] - 1], listed
assert tab_event(("click", 5, hit["y"]), nav) and nav["messages_view"]["open"] == hit["action"][1]
# Supported room messages keep their tail beyond both former wrapping limits; page keys and the wheel reach it.
for message_text in ("x\n" * 999 + "TAIL", "word " * 790 + "TAIL", "한글 " * 570 + "TAIL"):
    assert len(message_text.encode("utf-8")) <= 4000
    for glyphs in (True, False):
        nav, unused = board("messages", 80, 40, unicode=glyphs)
        nav["messages_view"]["log"] = [dict(msg_log[-1], text=message_text)]
        board("messages", 80, 40, nav=nav, unicode=glyphs)
        assert tab_event(("enter",), nav)
        opened = board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert "TAIL" not in opened, opened
        assert tab_event(("scroll", panel_messages.MAX_TEXT), nav)
        tail = board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert "TAIL" in tail, (len(message_text), glyphs, tail)
        assert len(tail.splitlines()) == 40 and all(display_width(line) <= 80 for line in tail.splitlines())
        assert "\x1b" not in tail and (glyphs or not any(ch in tail for ch in BOX_CHARS)), tail
        last_scroll = nav["messages_view"]["scroll"]
        assert tab_event(("pageup",), nav)
        assert "TAIL" not in board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert tab_event(("pagedown",), nav)
        assert "TAIL" in board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert nav["messages_view"]["scroll"] == last_scroll
        assert tab_event(("escape",), nav) and nav["messages_view"]["open"] is None
assert len(panel_messages.readable_text("x" * (panel_messages.MAX_TEXT + 1))) == panel_messages.MAX_TEXT
assert msg_report == before and msg_log[-1]["text"].startswith("line 29"), "reading changed the report or log"
# The footer lists keys that work: Messages/Debate show their own keys; the tree has no tabs, so no Tab hint or badge line.
for name, hint in (("messages", "Message"), ("debate", "Seat")):
    nav, text = board(name, 110, 40, managed=True)
    foot = text.split("\n")[-1]
    assert hint in foot and "Enter Open" in foot and "Ask advisor" not in foot and "Space Pin" not in foot, foot
    assert tab_event(("tab",), nav) and nav["tab"] != name
nav, text = board("detail", 110, 40, managed=True)
assert "Tab Next tab" in text.split("\n")[-1]
from panel_tree import render_tree
tree_nav = {"selected": ("chat", "m2")}
tree_text = render_tree(msg_report, 110, 40, navigation=tree_nav)
assert "Tab Next tab" not in tree_text, tree_text
# A selection from the inbox is valid only if the current tree still exposes it; visible tree selections remain intact.
stale_inbox_nav = {"selected": ("result", "w1"), "selected_row": 2, "row_ids": ["w1"], "surface": "inbox"}
render_tree(msg_report, 110, 40, navigation=stale_inbox_nav)
assert stale_inbox_nav["selected"] is None and "selected_row" not in stale_inbox_nav and "row_ids" not in stale_inbox_nav, stale_inbox_nav
assert tree_nav["selected"] == ("chat", "m2") and tree_nav["selected"] in tree_nav["items"], tree_nav
# The watch loop turns an unconsumed Tab into Down, so on the tree it is only a selection move.
assert not tab_event(("tab",), tree_nav) and not tab_event(("tab",), dict(tree_nav, surface="graph", detail={"target": ("chat", "m2")}))
helped = render_tree(msg_report, 110, 40, navigation=dict(tree_nav, keys_help=True))
assert "Tab Next tab" not in helped and "Tab badges" not in helped and "n New main" not in render_tree(msg_report, 110, 40, navigation={"keys_help": True}), helped
# On a short tree every recorded main stays on screen: earlier mains other than the selected one drop to their status line, which carries ? and context badges.
short_tree = deepcopy(msg_report)
short_tree["room"]["open_questions"] = [{"id": "q1", "sender": "m1", "recipient": "m3", "ts": "2026-10-07T10:00:00Z"}]
short_tree["room"]["participants"] = msg_members + [
    {"participant": "w%d" % n, "role": "worker", "joined": True, "model": "claude-opus-5-5", "seq": 9 + n, "provider": "claude",
     "parent": "m1" if n == 5 else "m3", "state": "working"} for n in range(6)]
every_main = {("chat", m) for m in ("m1", "m2", "m3")}
for ascii_only in (False, True):
    short_nav = {"selected": ("chat", "m1")}
    short_text = render_tree(short_tree, 90, 18, navigation=short_nav, unicode=not ascii_only)
    assert "? 1 open" in short_text and (not ascii_only or short_text.isascii()), short_text
    assert every_main <= {h["action"] for h in short_nav["hits"]}, short_text
    plain_nav = {"selected": ("chat", "m1"), "_auto_fold": True}
    render_tree(short_tree, 90, 18, navigation=plain_nav, unicode=not ascii_only)
    assert not every_main <= {h["action"] for h in plain_nav["hits"]}, "the fold is what brings every main on screen"
# Detail keeps its content after Esc dismissed the preview: the box stays so every tab remains reachable.
nav, dismissed = board("plan", 110, 40, nav={"dismissed": True, "tab": "plan"})
assert "t-ver" in dismissed and "[ Plan ]" in dismissed and "RECENT MESSAGES" not in dismissed, dismissed
nav, empty = board("detail", 110, 40, nav={"dismissed": True})
assert "No call selected" in empty and "[ Detail ]" in empty, empty
# The bottom box is reserved before the board is sized, from the terminal size alone: call counts, tabs, an empty
# or dismissed box, a selected call, its task text and the drawing set never move it; unused overview rows sit above it.


def crowd(report, advisors, workers, label=None, parent="a2", prefix="x"):
    crowded = deepcopy(report)
    for role, count, model in (("advisor", advisors, "gpt-6-astra"), ("worker", workers, "gpt-6-luna")):
        for n in range(count):
            ident = "%s-%s-%d" % (prefix, role, n)
            crowded["room"]["participants"].append({"participant": ident, "role": role, "joined": True, "model": model,
                                                    "parent": parent, "label": ident})
            crowded["attempts"]["active_recent"].append({"attempt_id": ident, "state": "working", "panel": {
                "room_id": "r", "room_participant": ident, "role": role}})
    if label:
        for member in crowded["room"]["participants"]:
            if member["role"] in ("advisor", "worker"):
                member["model"], member["label"] = "claude-opus-5-5", label
    return crowded


long_label = "A normal recorded task with sufficient words to wrap to a second line"
for width, height in ((80, 40), (157, 60), (126, 90)):
    baseline = set()
    for advisors in (0, 1, 3):
        for workers in (0, 1, 3):
            for label in (None, long_label):
                report = crowd(msg_report, advisors, workers, label)
                for tab, tab_name in room_view.TABS:
                    for selected in (("chat", "m2"), ("result", "x-advisor-0"), ("result", "x-worker-0")):
                        if selected[1] == "x-advisor-0" and not advisors or selected[1] == "x-worker-0" and not workers:
                            continue
                        for closed in (True, False):
                            for glyphs in (True, False) if label else (True,):
                                nav = {"selected": selected, "tab": tab, **({"dismissed": True} if closed else {})}
                                text = render(report, "codex", width, height, view="graph", navigation=nav, main_attempt="a2", unicode=glyphs)
                                rows = text.split("\n")
                                where = (width, height, advisors, workers, label, tab, selected, closed, glyphs)
                                y1, y2 = nav["box"]["y1"], nav["box"]["y2"]
                                assert len(rows) == height and y2 == height - 1 and max(map(display_width, rows)) <= width, (where, text)
                                assert rows[y1 - 1].startswith("╭─ " if glyphs else "+- ") and "[ %s" % tab_name in rows[y1 - 1], (where, text)
                                assert rows[y2 - 1].startswith("╰" if glyphs else "+") and "Quit" in rows[-1], (where, text)
                                assert glyphs or text.isascii(), (where, text)
                                detail_band = next(b for b in nav["bands"] if b["name"] == "detail")
                                assert (detail_band["y1"], detail_band["y2"]) == (y1, y2), (where, detail_band, nav["box"])
                                strip = {h["action"][1]: h["y"] for h in nav["hits"] if h["action"][0] == "tab"}
                                assert strip == {key: y1 for key, _ in room_view.TABS}, (where, strip)
                                baseline.add((y1, y2))
    # One box for every ordinary count, selection, task text, tab, drawing set and open state; it holds a first page.
    assert len(baseline) == 1 and next(iter(baseline))[1] - next(iter(baseline))[0] + 1 >= 11, (width, height, baseline)
# A main with an unanswered question shows "? N open" on its card; the person sees it in "Needs you" from 20 minutes.
import panel_view as question_view
import time as question_time
asked_report = deepcopy(msg_report)
asked_report["room"]["open_questions"] = [{"id": "q1", "sender": "m1", "recipient": "m2", "ts": question_time.strftime(
    "%Y-%m-%dT%H:%M:%SZ", question_time.gmtime(question_time.time() - 25 * 60))}]
for ascii_only in (False, True):
    asked_board = render(asked_report, "codex", 140, 40, view="graph", navigation={"dismissed": True}, unicode=not ascii_only,
                         main_attempt="a2")
    assert "? 1 open" in asked_board, asked_board
    assert not ascii_only or asked_board.isascii(), asked_board
assert "? 1 open" not in render(msg_report, "codex", 140, 40, view="graph", navigation={"dismissed": True}, main_attempt="a2")
waiting = [i for i in question_view.inbox_items(asked_report) if i["glyph"] == "?"]
assert [(i["who"], i["what"], i["action"]) for i in waiting] == [("Question from Opus 5.5", "to Sol 6.1", ("chat", "m2"))], waiting
# Two recorded mains on one model stay apart in the control window, which reuses the board's names.
twin = deepcopy(asked_report)
twin["room"]["participants"][1]["model"] = twin["room"]["participants"][0]["model"]
twin["room"]["messages"] = [{"id": "bc", "sender": "m1", "recipient": "all", "text": "heads up", "pending_for": ["m2"], "ts": "2026-10-07T10:00:00Z"}]
twin_who = [i["who"] for i in question_view.inbox_items(twin) if i["what"].startswith("to all")]
assert twin_who == ["Opus 5.5 #m1 main"], twin_who
asked_report["room"]["open_questions"][0]["ts"] = question_time.strftime("%Y-%m-%dT%H:%M:%SZ", question_time.gmtime(question_time.time() - 19 * 60))
assert not [i for i in question_view.inbox_items(asked_report) if i["glyph"] == "?"], "under twenty minutes the person is not pulled in"
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
    # Keep real collector subprocesses/threads out of the simulated refresh clock.
    with patch.object(panel, "read_panel", return_value=(live, 0, "codex", None, "auto", False, "")) as collect, \
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
for pressed, shown in [("t", "tree")]:
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

# Keys run against the board that was drawn: Esc closes ? help alone, and a click after a redraw in the same read is dropped.
class BatchInput:
    fd = 0
    def __init__(self, batches):
        self.batches = list(batches)
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def discard_clicks(self): pass
    def wait(self, seconds): return self.batches.pop(0) if self.batches else [("q",)]
def drive(batches, view="graph"):
    seen, kept = [], []
    def record(*args, **kwargs):
        seen.append(kwargs["navigation"])
        return panel_frame_view(*args, **kwargs)
    with patch.object(panel, "snapshot", return_value=(live, 0)), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
            patch.object(panel, "frame_view", side_effect=record), \
            patch("panel_input.choose", side_effect=lambda event, nav: kept.append(event) or choose(event, nav)), \
            patch("panel_input.TerminalInput", lambda *a, **k: BatchInput(batches)):
        assert panel.watch(Path("."), "codex", view=view, no_animation=True) == 0
    return seen[-1], kept
panel_frame_view = panel.frame_view
helped, unused = drive([[("?",)], [("escape",)]])
assert not helped.get("keys_help") and not helped.get("dismissed"), helped
healthy, routed = drive([[("k",)], [("a",), ("n",), ("repaint",), ("u",)], [("escape",)]])
assert healthy.get("health_view") is False and not healthy.get("dismissed"), healthy
assert not any(event[0] in {"k", "u", "repaint", "a", "n"} for event in routed), routed
unused, kept = drive([[("t",), ("click", 99, 39)]])
assert not any(event[0] == "click" for event in kept), kept
unused, kept = drive([[("click", 99, 39)]])
assert [event[0] for event in kept] == ["click"], kept

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
import json, os, sys, subprocess
with open(os.environ["PANEL_TEST_WORKER_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(),
                        "git_common": subprocess.check_output(["git", "rev-parse", "--git-common-dir"], text=True).strip(),
                        "role": os.environ.get("OMS_PANEL_ROLE"),
                        "purpose": os.environ.get("OMS_PANEL_PURPOSE"),
                        "task_id": os.environ.get("OMS_TASK_ID"),
                        "parent": os.environ.get("OMS_PANEL_MAIN_ATTEMPT"),
                        "room": os.environ.get("OMS_ROOM_ID"),
                        "room_repo": os.environ.get("OMS_ROOM_REPO"),
                        "timeout": os.environ.get("OMS_PEER_TIMEOUT"),
                        "state_repo": os.environ.get("OMS_STATE_REPO"),
                        "participant": os.environ.get("OMS_ROOM_PARTICIPANT")}) + "\\n")
WORKER
  if [ -n "${LEASE_FIXTURE_ENTERED:-}" ]; then
    python3 - "$@" <<'LEASEWAIT'
import json, os, sys, subprocess, time
from pathlib import Path
assert os.environ["OMS_AGENT"] == os.environ["LEASE_FIXTURE_PROVIDER"]
assert os.environ["LEASE_FIXTURE_MODEL"] in sys.argv[1:], sys.argv
if os.environ["OMS_AGENT"] == "claude":
    assert sys.argv[sys.argv.index("--effort") + 1] == os.environ["LEASE_FIXTURE_EFFORT"], sys.argv
else:
    assert any("model_reasoning_effort" in a and os.environ["LEASE_FIXTURE_EFFORT"] in a for a in sys.argv), sys.argv
for name in os.environ:
    assert name not in ("OMS_PLAN_LEASE_ID", "OMS_PANEL_EXECUTION_ROOT", "OBSERVED_BINDING", "OMS_INDEX_BASE_REPO", "DELEGATE_SOURCE_REPO") and not name.startswith(("OMS_DL_", "OMS_OBSERVER_", "OMS_OBSERVE_PLAN_")), name
entry = os.environ["OMS_PANEL_ENTRYPOINT"]
for args in (("agent-plan", "--repo", os.environ["OMS_ROOM_REPO"], "add", "--id", "child-denied", "--title", "Denied"),
             ("agent-plan", "--repo", os.environ["OMS_ROOM_REPO"], "touch", "--id", os.environ["OMS_TASK_ID"]),
             ("peer-delegate", "--to", "codex", "--prompt", "nested")):
    result = subprocess.run(["bash", entry, *args], capture_output=True, text=True)
    assert result.returncode != 0, (args, result.stdout, result.stderr)
Path(os.environ["LEASE_FIXTURE_ENTERED"]).write_text(json.dumps({"cwd": os.getcwd(),
    "head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "value": Path("value.txt").read_text(), "participant": os.environ["OMS_ROOM_PARTICIPANT"],
    "parent": os.environ.get("OMS_PANEL_MAIN_ATTEMPT"), "role": os.environ.get("OMS_PANEL_ROLE"),
    "purpose": os.environ.get("OMS_PANEL_PURPOSE")}))
end = time.monotonic() + 60
while not Path(os.environ["LEASE_FIXTURE_RELEASE"]).exists():
    assert time.monotonic() < end, "worker fixture barrier exceeded"
    time.sleep(.05)
LEASEWAIT
  fi
  cat > "${PANEL_TEST_PROMPT:-/dev/null}"
  if [ -n "${PANEL_TEST_SESSION:-}" ]; then
    printf '{"type":"thread.started","thread_id":"%s"}\\n{"type":"item.completed","item":{"type":"agent_message","text":"Answer: inspected the bounded repository task."}}\\n{"type":"turn.completed","usage":{}}\\n' "$PANEL_TEST_SESSION"
    exit 0
  fi
  if [ "${PANEL_TEST_MODE:-}" = write ]; then
    printf 'new\\n' >> value.txt
    if bash "$OMS_PANEL_ENTRYPOINT" panel --launch codex >/dev/null 2>&1; then
      echo 'nested owner launch should have failed' >&2
      exit 1
    fi
  fi
  if [ "${PANEL_TEST_MODE:-}" = primary-write ]; then
    printf 'outside worker\\n' >> "$PANEL_TEST_PRIMARY"
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
                   CODEX_HOME=str(home / ".codex"), CLAUDE_CONFIG_DIR=str(home / ".claude"),
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
# Role preferences split execution and judgment; independence and availability take precedence.
both = {"claude", "codex"}
assert allocate("codex", tier="light", installed=both)["model"] == "gpt-6-luna"
assert allocate("claude", tier="light", installed={"codex"})["model"] == "gpt-6-luna"
assert allocate("codex", installed={"claude"})["model"] == "claude-sonnet-5-5"
for role in ("worker", "builder", "refactorer", "documenter", "performance-engineer", "integrator", "release-engineer"):
    for owner in both:
        assert allocate(owner, role=role, installed=both)["provider"] == "codex"
assert allocate("claude", role="advisor", installed=both)["model"] == "claude-fable-5-1"
assert allocate("claude", role="advisor", installed={"codex"})["model"] == "gpt-6-astra"
assert allocate("claude", role="verifier", installed={"claude"})["model"] == "claude-fable-5-1"
assert allocate("claude", role="architect", installed=both)["model"] == "claude-fable-5-1"
for role in ("reviewer", "verifier", "security-auditor", "critic"):
    assert allocate("claude", role=role, installed=both)["provider"] == "codex"
    assert allocate("codex", role=role, installed=both)["provider"] == "claude"
assert allocate("codex", role="researcher", installed=both)["model"] == "claude-haiku-5-5"
for used, expected in ((89, "codex"), (90, "claude"), (100, "claude"),
                       (None, "codex"), (True, "codex"), ("90", "codex"), (101, "codex")):
    readings = {"codex": {"weekly_used": used}}
    assert allocate("codex", installed=both, provider_status=readings)["provider"] == expected
assert allocate("claude", role="reviewer", installed=both,
                provider_status={"codex": {"weekly_used": 90}})["provider"] == "claude"
assert allocate("codex", installed={"codex"}, provider_status={"codex": {"weekly_used": 100}})["provider"] == "codex"
try:
    allocate("codex", installed=both, provider_status={p: {"weekly_used": 90} for p in both})
    raise AssertionError("all saturated candidates were accepted")
except ValueError as error:
    assert ">=90% weekly usage" in str(error), error
routes = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--routes"]))
assert set(routes["tiers"]) == set(panel.TIERS) and set(routes["roles"]) == set(routes["kinds"]) == set(panel.KINDS)
assert routes["writers"] == sorted(panel.WRITERS) and report["routing"]["roles"] == routes["roles"]
assert set(routes["chosen"]) == set(panel.KINDS)
from panel_view import PURPOSE_NAMES
assert set(routes["roles"]) <= set(PURPOSE_NAMES), "every role strategy needs a board label"
for role, rule in routes["roles"].items():
    expected = allocate(routes["owner"], role, installed=both)
    assert routes["chosen"][role] == {k: expected[k] for k in ("provider", "model", "effort")}
    assert rule["tier"] == expected["workload"]
from panel_routing import policy
configured = policy()
assert configured["pool"] == {"max_rework_rounds": 2}
without_pool = dict(configured)
del without_pool["pool"]
with patch("panel_routing.Path.read_text", return_value=json.dumps(without_pool)):
    assert policy()["pool"] == configured["pool"]
for invalid_pool in (None, [], {"max_rework_rounds": 0}, {"max_rework_rounds": True},
                     {"max_rework_rounds": "2"}, {"max_running": 3}, {"unexpected": 1}):
    with patch("panel_routing.Path.read_text", return_value=json.dumps(dict(configured, pool=invalid_pool))):
        try:
            policy()
        except ValueError as error:
            assert "positive integers" in str(error)
        else:
            raise AssertionError("invalid pool policy accepted")
for invalid in ("routine", {"tier": "missing"}, {"tier": "routine", "prefer": "grok"},
                {"tier": []}, {"tier": "routine", "prefer": None}, {"tier": "routine", "extra": True}):
    malformed = deepcopy(policy())
    malformed["roles"]["builder"] = invalid
    with patch("panel_routing.Path.read_text", return_value=json.dumps(malformed)):
        try:
            policy()
            raise AssertionError("invalid role policy accepted: %r" % invalid)
        except ValueError:
            pass
for markers, expected in (({}, "codex"), ({"CODEX_SANDBOX": "1"}, "codex"),
                          ({"CLAUDECODE": "1", "CODEX_SANDBOX": "1"}, "claude"),
                          ({"CLAUDE_CODE_ENTRYPOINT": "cli"}, "claude"),
                          ({"OMS_AGENT": "codex", "CLAUDECODE": "1"}, "codex"),
                          ({"OMS_AGENT": "claude", "CODEX_SANDBOX": "1"}, "claude")):
    with patch.dict(os.environ, markers, clear=True):
        assert panel.caller_owner() == expected
    detected_env = {k: v for k, v in environment.items()
                    if k not in {"OMS_AGENT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CODEX_SANDBOX"}}
    detected_env.update(markers)
    planned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "reviewer",
                              "--prompt", "Inspect source.", "--verify", "true", "--dry-run"], detected_env))
    assert planned["route"]["owner"] == expected
    assert planned["route"]["provider"] != expected
    with patch.dict(os.environ, detected_env, clear=True), patch.object(panel, "council", return_value=0) as detected:
        assert panel.main(["--repo", str(project), "--council", "--task-id", "detected-council",
                           "--label", "Inspect source", "--prompt", "Inspect source.", "--dry-run"]) == 0
        assert detected.call_args.args[1] == expected
for target, model, effort in (("codex", "gpt-6-luna", "medium"), ("claude", "claude-sonnet-5-5", "medium")):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
               "--to", target, "--prompt", "Inspect source.", "--dry-run"]
    planned = json.loads(call(request))
    assert (planned["route"]["provider"], planned["route"]["model"], planned["route"]["effort"]) == (target, model, effort)
    assert planned["argv"][planned["argv"].index("--model") + 1] == model
    overridden = json.loads(call(request + ["--model", model, "--reasoning-effort", "high"]))
    assert overridden["route"]["model"] == model and overridden["route"]["effort"] == "high"
for target in ("grok", "muse"):
    rejected = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                               "--to", target, "--prompt", "Inspect source.", "--dry-run"],
                              env=environment, capture_output=True, text=True)
    assert rejected.returncode != 0 and "requires an explicit valid --model" in rejected.stderr
for role, kind, tier in (("builder", "worker", "routine"), ("integrator", "worker", "deep"),
                         ("release-engineer", "worker", "routine"), ("critic", "advisor", "frontier"),
                         ("verifier", "reviewer", "frontier"), ("reviewer", "reviewer", "frontier"),
                         ("researcher", "researcher", "light")):
    allocated = allocate("claude", role=role, installed=both)
    assert (allocated["kind"], allocated["workload"]) == (kind, tier), allocated
for bad in ({"role": "verifier", "access": "write"}, {"role": "release-engineer", "access": "write"},
            {"role": "builder", "purpose": "explain"}, {"tier": "main"}, {"installed": set()},
            {"role": "researcher", "tier": "deep"}):
    try:
        allocate("claude", installed=bad.pop("installed", both), **bad)
        raise AssertionError("invalid allocation accepted: %r" % bad)
    except ValueError as error:
        assert "install one" in str(error) or bad, error
for owner in panel.PROVIDERS:
    arguments = panel.native_command(owner, project)
    if owner == "claude":
        import uuid
        native_id = arguments[arguments.index("--session-id") + 1]
        assert str(uuid.UUID(native_id, version=4)) == native_id and uuid.UUID(native_id).version == 4
        resumed_argv = panel.native_command(owner, project, resume=native_id)
        assert "--session-id" not in resumed_argv and resumed_argv[resumed_argv.index("--resume") + 1] == native_id
    else:
        assert "--session-id" not in arguments
    expected = "gpt-6.1-sol" if owner == "codex" else "claude-opus-5-5"
    assert arguments[arguments.index("--model") + 1] == expected, arguments
    assert "--dispatch ROLE" in panel.bootstrap(owner, project) and "--workload light|routine|deep|frontier" in panel.bootstrap(owner, project)
    assert all(trigger in panel.bootstrap(owner, project) for trigger in (
        "security-auditor for risky changes", "verifier before declaring completion", "integrator when merging",
        "performance-engineer when performance needs measurement", "documenter at completion",
        "release-engineer when deploying", "not all by default", "a context apart"))
    assert "--seat" not in panel.bootstrap(owner, project) and "Astra" not in panel.bootstrap(owner, project)
    assert "control tower: by default delegate" in panel.bootstrap(owner, project)
    assert "exactly one live main" in panel.bootstrap(owner, project) and "trust-boundary" in panel.bootstrap(owner, project)
    assert "The sole main lands its work" in panel.bootstrap(owner, project)
    assert "oms scratch-worktree add --repo . starts detached" in panel.bootstrap(owner, project)
    assert "only when publication is authorized" in panel.bootstrap(owner, project)
    assert "git checkout --no-track -b oms/<task>" in panel.bootstrap(owner, project)
    assert "No upstream is needed" in panel.bootstrap(owner, project)
    assert "pushes the verified SHA to its target" in panel.bootstrap(owner, project)
    assert "concurrent land exits 75" in panel.bootstrap(owner, project) and "retry after" in panel.bootstrap(owner, project)
    assert "worker exit or review handoff is an intermediate step" in panel.bootstrap(owner, project)
    assert "OMS_ROOM_REPO" in panel.bootstrap(owner, project)
    assert "oms room scope" in panel.bootstrap(owner, project) and "--scope PATH" in panel.bootstrap(owner, project)
option_text = "--dangerously-skip-permissions"
assert panel.native_command("claude", project, task=option_text)[-2:] == ["--", option_text]
assert option_text not in panel.native_command("codex", project, task=option_text)[:-1]
pi_argv = panel.native_command("pi", project, task=option_text, room_id="pi-room", member="pi-main")
assert pi_argv[:4] == ["bash", str(panel.ENTRY), "harness", "pi"], pi_argv
assert pi_argv[pi_argv.index("--model") + 1] == "gpt-6.1-sol"
assert "--append-system-prompt" in pi_argv and "--owner codex" in pi_argv[pi_argv.index("--append-system-prompt") + 1]
assert json.dumps(option_text) in pi_argv[-1] and pi_argv[-2] == "--", pi_argv
assert "pi-room" in pi_argv[pi_argv.index("--append-system-prompt") + 1]
try:
    panel.native_command("pi", project, resume="exact-session-id")
    raise AssertionError("Pi history was treated as a Codex native session")
except ValueError as error:
    assert "verified Pi session binding" in str(error)
assert "--owner codex" in panel.watch_shell(project, "pi") and "--owner pi" not in panel.watch_shell(project, "pi")
assert "Codex CLI" in panel.main_missing("pi") and "Pi CLI" in panel.main_missing("pi")
for harness in panel.MAIN_HARNESSES:
    with patch.object(panel, "main_installed", return_value=False):
        try:
            panel._run_native(harness, project)
            raise AssertionError("missing harness started")
        except ValueError as error:
            assert str(error) == panel.main_missing(harness)
with patch.dict(os.environ, environment, clear=True), patch.object(panel.shutil, "which", return_value="/x"), \
        patch.object(panel, "ensure_room", return_value="pi-room"), patch.object(panel, "events", return_value="pi-attempt") as pi_events, \
        patch.object(panel.room, "join") as pi_join, patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.panel_host, "herdr_active", return_value=False), patch.object(panel, "update_activity"), \
        patch.object(panel.subprocess, "Popen") as pi_process:
    pi_process.return_value.wait.return_value = pi_process.return_value.poll.return_value = 0
    assert panel._run_native("pi", project, task="fixture") == 0
    assert pi_join.call_args.args[3] == "codex" and pi_join.call_args.args[7] is None
    assert "panel_harness=pi" in pi_events.call_args.args
    pi_env = pi_process.call_args.kwargs["env"]
    assert pi_env["OMS_PANEL_OWNER"] == pi_env["OMS_AGENT"] == "codex" and pi_env["OMS_PANEL_HARNESS"] == "pi"
    assert "OMS_HOOK_AGENT" not in pi_env

# Scratch repository selection shares state without moving native execution.
import room
import room_repository as repo_paths
shared_root = temporary / "shared root 'resolution'"
subprocess.run(["git", "clone", "--quiet", "--no-local", str(project), str(shared_root)], check=True)
registry = temporary / "resolution-registry"
scratch_parent = registry / "oh-my-setting-scratch.resolution"
scratch_parent.mkdir(parents=True)
working_root = scratch_parent / "wt"
subprocess.run(["git", "-C", str(shared_root), "worktree", "add", "--quiet", "--detach", str(working_root)], check=True)
working_child = working_root / "nested"
working_child.mkdir()
proof = scratch_parent / ".oh-my-setting-tmp"
proof_fields = {"kind": "oh-my-setting-temp", "temporary": "1", "pid": "99999999",
                "repo": str(shared_root), "worktree": str(working_root)}
proof_bytes = "".join(key + "=" + value + "\n" for key, value in proof_fields.items()).encode()
proof.write_bytes(proof_bytes)
resolver_env = dict(environment, OMS_DELEGATE_WORKTREE_ROOT=str(registry))
shared_id = json.loads(call(["bash", str(panel.ENTRY), "room", "new", "--repo", str(shared_root), "--json"], resolver_env))["id"]
call(["bash", str(panel.ENTRY), "room", "join", "--repo", str(shared_root), "--id", shared_id,
      "--participant", "resolution-owner", "--native-session", "resolution-native-consumer", "--json"], resolver_env)
selected_env = dict(resolver_env, OMS_ROOM_ID=shared_id, OMS_ROOM_REPO=str(shared_root),
                    OMS_ROOM_PARTICIPANT="resolution-owner")
consumer_before = room.participant(room.status(shared_root, shared_id), "resolution-owner")
for requested in (working_root, working_child):
    shown = json.loads(call(["bash", str(panel.ENTRY), "room", "show", "--repo", str(requested), "--json"], selected_env))
    assert shown["id"] == shared_id, shown
    board = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(requested), "--json"], selected_env))
    assert board["dashboard"]["room"]["id"] == shared_id, board
    with patch.dict(os.environ, resolver_env, clear=True):
        assert panel.repository(requested) == shared_root
        assert repo_paths.work_repository(requested) == working_root
for front in ("room", "panel"):
    default_command = ["bash", str(panel.ENTRY), front] + (["show"] if front == "room" else []) + ["--json"]
    default_view = subprocess.run(default_command, cwd=working_child, env=selected_env,
                                  capture_output=True, text=True, timeout=30)
    assert default_view.returncode == 0, default_view
    default_data = json.loads(default_view.stdout)
    assert (default_data if front == "room" else default_data["dashboard"]["room"])["id"] == shared_id

ordinary_root = registry / "ordinary"
subprocess.run(["git", "-C", str(shared_root), "worktree", "add", "--quiet", "--detach", str(ordinary_root)], check=True)
plain_directory = temporary / "resolution-non-git"
plain_directory.mkdir()
panel_front = ["bash", str(panel.ENTRY), "panel"]
missing_repo_message = "oms panel needs a git repository: run it inside one or pass --repo PATH"
for options in ([], ["--watch", "--count", "1"], ["--launch", "codex", "--dry-run"],
                ["--spawn-main", "codex", "--json"], ["--reopen"]):
    refused = subprocess.run(panel_front + options, cwd=plain_directory, env=resolver_env,
                             capture_output=True, text=True, timeout=30)
    assert refused.returncode == 2 and missing_repo_message in refused.stderr, refused
assert not list(plain_directory.iterdir())
for options in (["--providers", "--json"], ["--routes"], ["--help"]):
    informational = subprocess.run(panel_front + options, cwd=plain_directory, env=selected_env,
                                   capture_output=True, text=True, timeout=30)
    assert informational.returncode == 0, informational
explicit = subprocess.run(panel_front + ["--repo", str(plain_directory), "--dry-run"],
                          cwd=plain_directory, env=resolver_env, capture_output=True, text=True, timeout=30)
assert explicit.returncode == 0 and json.loads(explicit.stdout)["repo"] == str(plain_directory), explicit
ordinary_child = ordinary_root / "nested"
ordinary_child.mkdir()
ordinary_alias = temporary / "resolution-ordinary-link"
ordinary_alias.symlink_to(ordinary_child, target_is_directory=True)
for directory in (ordinary_child, ordinary_alias):
    implicit = subprocess.run(panel_front + ["--dry-run"], cwd=directory, env=resolver_env,
                              capture_output=True, text=True, timeout=30)
    assert implicit.returncode == 0 and json.loads(implicit.stdout)["repo"] == str(ordinary_root), implicit
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
        [], 0, str(ordinary_alias / "..") + "\r\n", "")), \
        patch.object(panel, "state_repository", return_value=ordinary_root) as state_resolution:
    assert panel.repository(None) == ordinary_root
    assert state_resolution.call_args.args == (ordinary_root,)
with patch.dict(os.environ, resolver_env, clear=True):
    assert panel.repository(ordinary_root) == ordinary_root
    assert panel.repository(plain_directory) == plain_directory
    non_git_scratch = registry / "oh-my-setting-scratch.non-git" / "wt"
    non_git_scratch.mkdir(parents=True)
    (non_git_scratch.parent / ".oh-my-setting-tmp").write_bytes(proof_bytes)
    assert panel.repository(non_git_scratch) == non_git_scratch
    for delegation in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "2"}):
        with patch.dict(os.environ, delegation):
            assert panel.repository(working_child) == working_root
    # The dead original PID never invalidates a registered parent scratch.
    assert panel.repository(working_root) == shared_root
    # A copied Git pointer with the same common directory is not registration.
    orphan_parent = registry / "oh-my-setting-scratch.unregistered"
    orphan_worktree = orphan_parent / "wt"
    orphan_worktree.mkdir(parents=True)
    (orphan_worktree / ".git").write_bytes((working_root / ".git").read_bytes())
    orphan_proof = dict(proof_fields, worktree=str(orphan_worktree))
    (orphan_parent / ".oh-my-setting-tmp").write_text("".join(key + "=" + value + "\n" for key, value in orphan_proof.items()))
    assert panel.repository(orphan_worktree) == orphan_worktree
    for field, invalid_value in (("repo", str(project)), ("worktree", str(ordinary_root)),
                                 ("worktree", str(registry / "missing" / "wt")), ("temporary", "0"),
                                 ("kind", "foreign-temp")):
        invalid = dict(proof_fields, **{field: invalid_value})
        proof.write_text("".join(key + "=" + value + "\n" for key, value in invalid.items()))
        assert panel.repository(working_root) == working_root, (field, invalid_value)
    for invalid in (proof_bytes + b"repo=conflict\n", b"x" * (repo_paths.MARKER_LIMIT + 1), b"kind=oh-my-setting-temp"):
        proof.write_bytes(invalid)
        assert panel.repository(working_root) == working_root
    proof.write_bytes(proof_bytes.replace(b"\n", b"\r\n"))
    assert panel.repository(working_root) == shared_root
    proof.unlink()
    proof_source = scratch_parent / "proof-source"
    proof_source.write_bytes(proof_bytes)
    proof.symlink_to(proof_source)
    assert panel.repository(working_root) == working_root
    proof.unlink()
    os.link(proof_source, proof)
    assert panel.repository(working_root) == working_root
    proof.unlink()
    if hasattr(os, "mkfifo"):
        os.mkfifo(proof)
        assert panel.repository(working_root) == working_root
        proof.unlink()
    proof.write_bytes(proof_bytes)
    alias_registry = temporary / "resolution-registry-link"
    alias_registry.symlink_to(registry, target_is_directory=True)
    with patch.dict(os.environ, {"OMS_DELEGATE_WORKTREE_ROOT": str(alias_registry)}):
        assert panel.repository(working_root) == working_root

for front in ("room", "panel"):
    conflict = ["bash", str(panel.ENTRY), front] + (["show"] if front == "room" else []) + ["--repo", str(project), "--json"]
    refused = subprocess.run(conflict, env=selected_env, capture_output=True, text=True, timeout=30)
    assert refused.returncode and "conflicts" in refused.stderr, refused
for restricted in (["updates", "--participant", "not-enrolled"],
                   ["join", "--participant", "resolution-orphan", "--role", "worker", "--parent", "not-enrolled"]):
    refused = subprocess.run(["bash", str(panel.ENTRY), "room"] + restricted +
                             ["--repo", str(working_root), "--id", shared_id, "--json"], env=resolver_env,
                             capture_output=True, text=True, timeout=30)
    assert refused.returncode, refused

execution_log = temporary / "resolution-native.jsonl"
with patch.dict(os.environ, dict(selected_env, PANEL_TEST_LOG=str(execution_log)), clear=True), \
        patch.object(panel.sys.stdin, "isatty", return_value=True), \
        patch.object(panel.sys.stdout, "isatty", return_value=True):
    assert panel.main(["--repo", str(working_child), "--host", "inline", "--layout", "inline", "--launch", "codex"]) == 0
native_observed = json.loads(execution_log.read_text().splitlines()[-1])
assert native_observed["cwd"] == str(working_root) and native_observed["repo"] == str(shared_root), native_observed
assert room.participant(room.status(shared_root, shared_id), "resolution-owner") == consumer_before

assert not (working_root / ".oms").exists()
with patch.dict(os.environ, dict(selected_env, **{repo_paths.EXECUTION_ENV: str(working_root)}), clear=True):
    # Carry execution context into a canonical --repo child without changing
    # tmux's existing state ownership and command identity checks.
    assert repo_paths.EXECUTION_ENV + "=" + str(working_root) in shlex.split(panel.launch_command(shared_root, "oms-resolution", "codex"))
    with patch.object(panel.subprocess, "check_output", return_value="%resolution\n") as created, \
            patch.object(panel, "bind_window"), patch.object(panel, "board_split"):
        panel.add_main_window(shared_root, "oms-resolution", "codex", "auto", False, os.environ.copy())
        command = created.call_args.args[0]
        assert command[command.index("-c") + 1] == str(working_root), command
    with patch.object(panel, "native_binary", return_value="resolution-adapter"):
        adapter = panel.native_command("muse", shared_root)
        assert adapter[adapter.index("--workdir") + 1] == str(working_root), adapter
    with patch.object(panel.panel_host, "herdr_active", return_value=True), \
            patch.object(panel.panel_host, "open_native") as host_launch:
        panel.open_native("codex", shared_root)
        directory, provider, native_args, watcher_args = host_launch.call_args.args
        assert directory == working_root and provider == "codex"
        assert all(args[args.index("--repo") + 1] == str(working_root) for args in (native_args, watcher_args))
    with patch.dict(os.environ, {repo_paths.EXECUTION_ENV: str(ordinary_root)}):
        try:
            repo_paths.native_repository(shared_root)
        except ValueError:
            pass
        else:
            raise AssertionError("ordinary linked worktree inherited canonical execution authority")
room.command(shared_root, "close", "--id", shared_id)
for closed_command in (["panel", "--room", shared_id],
                       ["room", "join", "--participant", "resolution-owner"]):
    refused = subprocess.run(["bash", str(panel.ENTRY)] + closed_command + ["--repo", str(working_root), "--json"],
                             env=selected_env, capture_output=True, text=True, timeout=30)
    assert refused.returncode and "closed" in refused.stderr, refused
closed_read = json.loads(call(["bash", str(panel.ENTRY), "room", "updates", "--participant", "resolution-owner",
                               "--repo", str(working_root), "--json"], selected_env))
assert not closed_read["turns"], "closed-room reads remain available"
print("PASS: native scratch-state resolver and execution directory boundaries")


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

# Same-repository dispatch retains its per-role execution locations, with one
# recorded main per caller and read/write authority tested independently.
main_attempts = {}
for owner in panel.PROVIDERS:
    main_attempts[owner] = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                               "start", "--provider", owner, "--tool", "panel-main", "--ref", "panel_role=main",
                               "--then", "starting", "--then", "working"]).strip()
cases = [("codex", "worker", "light", "read", "explain", "gpt-6-luna"),
         ("codex", "worker", None, "read", "explain", "gpt-6-luna"),
         ("codex", "worker", "deep", "read", "investigate", "gpt-6.1-sol"),
         ("claude", "builder", None, "write", None, "gpt-6-luna"),
         ("claude", "advisor", None, "read", None, "claude-fable-5-1"),
         ("codex", "advisor", "deep", "read", None, "claude-opus-5-5"),
         ("claude", "reviewer", None, "read", None, "gpt-6-astra"),
         ("claude", "security-auditor", None, "read", None, "gpt-6-astra"),
         ("codex", "researcher", None, "read", None, "claude-haiku-5-5"),
         ("claude", "researcher", None, "read", None, "claude-haiku-5-5")]
for index, (owner, role, workload, access, purpose, expected) in enumerate(cases):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", role,
               "--owner", owner, "--access", access, "--task-id", "panel-route-%s" % index,
               "--label", "Inspect fixture source %s" % index]
    request += (["--workload", workload] if workload else []) + (["--purpose", purpose] if purpose else [])
    if access == "write":
        request += ["--brief-file", str(brief), "--verify", "test \"$(tail -1 value.txt)\" = new"]
    else:
        request += ["--prompt", "Explain value.txt; judge only the supplied source evidence."]
        if role == "reviewer":
            request += ["--verify", "test -f value.txt"]
    before = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
    route = json.loads(call(request + ["--dry-run"]))
    assert route["route"]["model"] == expected and route["executes"] is False, route
    if role not in {"advisor", "reviewer"}:
        assert route["argv"][route["argv"].index("--role") + 1] == (
            role if role != "worker" else "analysis-worker"), route["argv"]
    env = dict(environment, OMS_AGENT=owner, OMS_PANEL_MAIN_ATTEMPT=main_attempts[owner])
    if access == "write":
        env["PANEL_TEST_MODE"] = "write"
        call(["git", "config", "diff.mnemonicPrefix", "true"])
    answer = call(request, env)
    if access == "write":
        assert "patch:" in answer, answer
        write_patch = re.search(r"^patch: (.+)$", answer, re.M).group(1)
        from panel_results import _patch_paths, _diffstat
        captured = Path(write_patch).read_bytes()
        assert _patch_paths(captured) == ["value.txt"]
        stat = _diffstat(project, {"patch": Path(write_patch).relative_to(project).as_posix(),
                                  "patch_sha256": hashlib.sha256(captured).hexdigest()})
        assert stat["files"] == [["value.txt", 1, 0]], stat
        call(["git", "config", "--unset", "diff.mnemonicPrefix"])
    assert (project / "value.txt").read_text() == "old\n", "role task edited primary"
    assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == before
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(row for row in attempts if row.get("task_id") == "panel-route-%s" % index)
    refs = attempt["refs"]
    assert refs["panel_role"] == panel.KINDS[role] and refs["panel_model"] == expected, attempt
    assert refs["panel_purpose"] == route["route"]["purpose"] and (role == "worker" or refs["panel_purpose"] in (
        role, "advise", "review", "research")), refs
    assert refs["panel_effort"] == route["route"]["effort"], refs
    assert refs["panel_access"] == access and refs["panel_label"] == "Inspect fixture source %s" % index, refs
    assert attempt["parent_attempt_id"] == main_attempts[owner], attempt
    assert attempt["state"] == ("review" if access == "write" else "done"), attempt
    assert refs["panel_location"].startswith("worktree:") if role not in {"advisor", "reviewer"} else refs["panel_location"] == "repository", refs
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
    if role not in {"advisor", "reviewer"}:
        assert observed["cwd"] != str(project), observed
    else:
        assert observed["cwd"] == str(project), observed

# A researcher keeps the cheap family, read-only strategy and source evidence.
from panel_routing import newest_route_model
assert newest_route_model("codex", "gpt-6-luna", ["gpt-6.2-luna", "gpt-6.10-luna", "gpt-7-astra"]) == "gpt-6.10-luna"
assert newest_route_model("claude", "claude-haiku-5-5", ["claude-haiku-5-6", "claude-opus-9-9"]) == "claude-haiku-5-6"
codex_only = {"provider": "codex", "binary": "codex", "installed": True, "models": ["gpt-6-sol"],
              "catalog_models": ["gpt-6-sol", "gpt-6.1-sol", "gpt-6.2-sol", "gpt-7-astra"]}
with patch.object(panel, "provider_catalog", return_value=[codex_only]):
    assert panel.selected_route("codex", "worker", "deep", "read", "investigate")["model"] == "gpt-6.2-sol"
    assert panel.selected_route("codex", "worker", "light", "read", "explain")["model"] == "gpt-6-luna"
    assert panel.selected_route("codex", "reviewer", None, "read", None)["provider"] == "codex"
    explicit = panel.selected_route("claude", "worker", None, "read", None, "codex")
    assert explicit["provider"] == "codex" and explicit["model"] == "gpt-6-luna" and explicit["effort"] == "medium"
recorded = {"codex": {"weekly_used": 90}, "claude": {"weekly_used": None}}
claude_candidate = {"provider": "claude", "binary": "claude", "installed": True, "models": []}
with patch.object(panel, "provider_catalog", return_value=[codex_only, claude_candidate]), \
        patch.object(panel, "snapshot", return_value=({"room": {"id": "quota-room"}}, 0)), \
        patch("panel_metrics.collect", return_value=recorded) as metrics:
    selected = panel.selected_route("codex", "worker", None, "read", None,
                                    repo=project, room_id="quota-room", main="quota-main")
    assert selected["provider"] == "claude" and selected["model"] == "claude-sonnet-5-5"
    metrics.assert_called_once_with({"room": {"id": "quota-room"}}, "quota-main", repo=project)
with patch.object(panel, "provider_catalog", return_value=[codex_only]), \
        patch("panel_metrics.collect", side_effect=AssertionError("explicit provider queried usage")):
    assert panel.selected_route("claude", "worker", None, "read", None, "codex", repo=project,
                                room_id="quota-room")["provider"] == "codex"
with patch.object(panel, "provider_catalog", return_value=[codex_only, claude_candidate]), \
        patch.object(panel, "snapshot", return_value=({}, 1)), \
        patch("panel_metrics.collect", side_effect=ValueError("unavailable reading")):
    assert panel.selected_route("codex", "worker", None, "read", None,
                                repo=project, room_id="quota-room")["provider"] == "codex"
with patch.object(panel, "provider_catalog", return_value=[dict(codex_only, installed=False)]):
    try:
        panel.selected_route("codex", "worker", None, "read", None)
        raise AssertionError("a route was chosen with no installed provider")
    except ValueError as error:
        assert "no installed provider serves the routine tier" in str(error), error
with patch.object(panel, "provider_catalog", return_value=[dict(codex_only, models=["gpt-6-luna"],
        catalog_models=["gpt-6-luna"] * 20 + ["gpt-6.2-luna"])]):
    assert panel.selected_route("codex", "researcher", None, "read", None)["model"] == "gpt-6.2-luna"
try:
    panel.room.declared_scopes("researcher", ["src"])
    raise AssertionError("researcher write scope was accepted")
except ValueError:
    pass
for target in ("codex", "claude"):
    preset = panel.selected_route("claude", "researcher", None, "read", None, target)
    command = panel.routed_command(panel.ENTRY, project, preset, prompt="Find cited evidence.")
    assert "--read-only" in command and command[command.index("--role") + 1] == "researcher"
    assert preset["provider"] == target and preset["effort"] == "low" and preset["workload"] == "light"
    for options in ((None, "read", "implement", None, None, None),
                    (None, "write", None, None, None, None),
                    ("deep", "read", None, None, None, None),
                    (None, "read", None, None, None, "high"),
                    (None, "read", None, "codex", "gpt-6-astra", None),
                    (None, "read", None, "claude", "provider-default", None),
                    (None, "read", None, "grok", None, None),
                    ("light", "read", None, "codex", None, None)):
        workload, access, purpose, to, model, effort = options
        try:
            panel.selected_route("claude", "researcher", workload, access, purpose, to, model, effort)
            raise AssertionError("researcher authority/model override was accepted")
        except ValueError:
            pass
    for forbidden in ({"verify": "true"}, {"resume": "fixture-session"}):
        try:
            panel.routed_command(panel.ENTRY, project, preset, prompt="Lookup only.", **forbidden)
            raise AssertionError("researcher gate/continuation was accepted")
        except ValueError:
            pass
# Only write-capable roles continue a session; judges keep a context apart from the implementer.
for role in ("reviewer", "verifier", "security-auditor", "critic", "advisor", "release-engineer"):
    judge = panel.selected_route("claude", role, None, "read", None)
    try:
        panel.routed_command(panel.ENTRY, project, judge, brief=str(brief), verify="true", resume="fixture-session")
        raise AssertionError("%s resumed a session" % role)
    except ValueError as error:
        assert "only write-capable roles continue" in str(error), error
refactor = panel.selected_route("claude", "refactorer", None, "write", None)
assert "--resume-session" in panel.routed_command(panel.ENTRY, project, refactor, brief=str(brief), verify="true",
                                                  resume="fixture-session")

# Lifecycle admission rejects a forged researcher label on a write delegate before the native CLI runs.
for access, purpose, model, effort in (("write", "research", "gpt-6-luna", "low"),
                                     ("read", "implement", "gpt-6-luna", "low"),
                                     ("read", "research", "gpt-6-astra", "low"),
                                     ("read", "research", "gpt-6-luna", "high")):
    forged = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"],
                  OMS_PANEL_DISPATCH="1", OMS_PANEL_ROLE="researcher", OMS_PANEL_PURPOSE=purpose)
    request = ["bash", str(panel.ENTRY), "peer-delegate", "--repo", str(project), "--to", "codex",
               "--model", model, "--reasoning-effort", effort, "--brief-file", str(brief), "--verify", "test -f value.txt"]
    if access == "read":
        request.append("--read-only")
    before = worker_log.read_bytes()
    denied = subprocess.run(request, env=forged, capture_output=True, text=True)
    assert denied.returncode != 0 and worker_log.read_bytes() == before, denied.stdout + denied.stderr

# Omitted worker purpose follows access, and one effective ID binds the plan,
# argv, execution metadata and operator-visible launch line.
read_default = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                "--owner", "codex", "--prompt", "Explain the fixture source."]
write_default = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                 "--owner", "codex", "--access", "write", "--brief-file", str(brief),
                 "--verify", 'test "$(tail -1 value.txt)" = new', "--label", "Default implementation"]
worker_before = worker_log.read_bytes()
events_before = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"])
read_plan = json.loads(call(read_default + ["--dry-run"]))
write_plan = json.loads(call(write_default + ["--dry-run"]))
for planned, purpose in ((read_plan, "explain"), (write_plan, "implement")):
    ident = planned["task_id"]
    assert planned["route"]["purpose"] == purpose, planned
    assert re.fullmatch(r"panel-%s-[a-f0-9]{8}" % purpose, ident), ident
    assert planned["argv"][planned["argv"].index("--task-id") + 1] == ident, planned
    assert planned["executes"] is False
assert worker_log.read_bytes() == worker_before, "dry-run invoked a native fixture"
assert call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]) == events_before

write_output = call(write_default, dict(environment, OMS_AGENT="codex",
                                        OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"], PANEL_TEST_MODE="write"))
visible = re.findall(r"\btask_id=(panel-implement-[a-f0-9]{8})\b", write_output)
assert len(visible) == 1, write_output
generated_id = visible[0]
execution = json.loads(worker_log.read_text().splitlines()[-1])
assert execution["purpose"] == "implement" and execution["task_id"] == generated_id, execution
attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
attempt = next(row for row in attempts if row.get("task_id") == generated_id)
assert attempt["refs"]["panel_purpose"] == "implement", attempt

# Explicit purpose and task IDs remain exact.
explicit_id = "explicit-task-ID.7"
explicit = read_default + ["--purpose", "investigate", "--task-id", explicit_id]
explicit_plan = json.loads(call(explicit + ["--dry-run"]))
assert explicit_plan["task_id"] == explicit_id and explicit_plan["route"]["purpose"] == "investigate"
assert explicit_plan["argv"][explicit_plan["argv"].index("--task-id") + 1] == explicit_id

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
                ["--to", "grok", "--model", "fixture", "--workload", "deep"],
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
    assert routed.call_args.kwargs == {"target": "muse", "model": "muse-fixture", "effort": None}
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
        "codex:model=gpt-6.1-sol,claude:model=claude-opus-5-5,claude:model=claude-fable-5-1,codex:model=gpt-6-astra"
assert (project / ".oms/artifacts/index.jsonl").read_bytes() == before_index
call(council_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
council_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
council_attempts = [a for a in council_attempts if a.get("task_id") == "panel-council-test"]
assert any(a["tool"] == "panel-council" and a["state"] == "done" for a in council_attempts)
assert {a["refs"].get("panel_model") for a in council_attempts if a["tool"] != "panel-council"} == \
    {"gpt-6.1-sol", "claude-opus-5-5", "gpt-6-astra", "claude-fable-5-1"}, council_attempts
assert all(a.get("parent_attempt_id") == main_attempts["codex"] for a in council_attempts)
# A control-menu council has no inherited main: it binds the room's one live main, or refuses, so Debate can show it.
class CouncilBound(Exception):
    pass
def council_events(repo, *args, **kwargs):
    if args[0] == "show":
        return json.dumps({"tool": "panel-main", "provider": "codex", "terminal": False})
    raise CouncilBound(list(args))
menu_env = {"OMS_ROOM_ID": "council-room", "OMS_ROOM_PARTICIPANT": ""}
with patch.dict(os.environ, menu_env), patch.object(panel.room, "records", return_value=[]), \
        patch.object(panel.room, "project", return_value={}), patch.object(panel, "events", council_events), \
        patch.object(panel, "active_mains", return_value={"m1": {"attempt_id": "att_m1"}}):
    try:
        panel.council(project, "codex", "menu-council", "Menu council", "Compare options.")
    except CouncilBound as bound:
        assert "--parent-attempt-id" in bound.args[0] and "att_m1" in bound.args[0], bound.args
        assert "panel_room_id=council-room" in bound.args[0], bound.args
    else:
        raise AssertionError("council did not start")
for none in ({}, {"m1": {"attempt_id": "a1"}, "m2": {"attempt_id": "a2"}}):
    with patch.dict(os.environ, menu_env), patch.object(panel.room, "records", return_value=[]), \
            patch.object(panel.room, "project", return_value={}), patch.object(panel, "events", council_events), \
            patch.object(panel, "active_mains", return_value=none):
        try:
            panel.council(project, "codex", "menu-council", "Menu council", "Compare options.")
        except ValueError as refused:
            assert "unique active main" in str(refused)
        else:
            raise AssertionError("a council without a unique main started")

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
# A result recorded without panel identity still reaches the shared Plan,
# while an old generation, reopened task or changed proof cannot settle it.
plan_path = project / ".oms/plan/tasks.json"
plan_path.parent.mkdir(parents=True, exist_ok=True)
before_plan = plan_path.read_bytes() if plan_path.exists() else None
external_plan = {"plan_id": "plan_elsewhere", "goal": "Elsewhere work", "tasks": {
    "elsewhere-task": {"id": "elsewhere-task", "title": "Completed outside the panel", "state": "review",
                       "verify": "test -f value.txt", "updated": "2026-10-10T00:00:00Z"}}}
def write_external_plan():
    plan_path.write_text(json.dumps(external_plan), encoding="utf-8")
write_external_plan()
outside_args = list(final_args)
outside_args[outside_args.index("--task-id") + 1] = "elsewhere-task"
outside_args[outside_args.index("--outcome") + 1] = "completed"
other_directory = temporary / "outside-panel-caller"
other_directory.mkdir()
external_call = subprocess.run(outside_args, cwd=other_directory, env=environment,
                               capture_output=True, text=True, check=True)
outside = json.loads(external_call.stdout)
assert outside["plan_binding"]["plan_id"] == "plan_elsewhere" and outside["main_attempt_id"] is None, outside
complete = saved.completed_tasks(project, ["elsewhere-task"])
assert complete == {"elsewhere-task": "completed"}, complete
from room_view import plan_rows, goal_box
plan_report = {"plan": {"present": True, "goal": "Elsewhere work", "task_count": 1, "by_state": {"review": 1}},
               "room": {"repo_tasks": list(external_plan["tasks"].values())}, "completed_tasks": complete}
assert any("completed locally" in row[0] for row in plan_rows(plan_report, {}, 120, True))
assert "1 locally completed" in "\n".join(row[0] for row in goal_box(plan_report, 120, True, False))
assert external_plan["tasks"]["elsewhere-task"]["state"] == "review", "display granted acceptance"
task_navigation = {"room_id": "elsewhere-room"}
with patch.object(panel.room, "status", return_value={"id": "elsewhere-room", "participants": []}):
    panel.navigate(project, ("task", "elsewhere-task"), plan_report, task_navigation, 120)
assert "Work: completed locally" in task_navigation["detail"]["text"], task_navigation
assert "Plan acceptance: review" in task_navigation["detail"]["text"], task_navigation
external_plan["tasks"]["elsewhere-task"]["state"] = "running"
write_external_plan()
assert not saved.completed_tasks(project, ["elsewhere-task"]), "reopened task inherited completion"
external_plan["tasks"]["elsewhere-task"]["state"] = "review"
external_plan["plan_id"] = "plan_replaced"
write_external_plan()
assert not saved.completed_tasks(project, ["elsewhere-task"]), "replacement plan inherited completion"
external_plan["plan_id"] = "plan_elsewhere"
write_external_plan()
proof = (project / "value.txt").read_bytes()
(project / "value.txt").write_bytes(b"changed evidence\n")
assert not saved.completed_tasks(project, ["elsewhere-task"]), "changed evidence stayed completed"
(project / "value.txt").write_bytes(proof)
unverified_args = outside_args[:outside_args.index("--verify")]
call(unverified_args)
assert not saved.completed_tasks(project, ["elsewhere-task"]), "unverified latest result stayed completed"
with patch.object(saved, "_plan_bindings", side_effect=[
        {"elsewhere-task": outside["plan_binding"]}, {"elsewhere-task": {"plan_id": "changed"}}]):
    try:
        saved.finalize(project, "codex", "elsewhere-task", str(summary), "completed", "true")
    except ValueError as error:
        assert "plan task changed" in str(error), error
    else:
        raise AssertionError("task changed during verification but result was persisted")
if before_plan is None:
    plan_path.unlink()
else:
    plan_path.write_bytes(before_plan)
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
# BEGIN patch-scope finalize regression (also runnable as an isolated case).
scope_patch = project / ".oms/artifacts/scope-result.patch"
scope_worker = {"attempt_id": "att_scope_write", "task_id": "scope-result", "parent_attempt_id": "scope-main",
                "refs": {"panel_role": "worker", "panel_access": "write", "panel_room_id": "scope-room",
                         "panel_room_participant": "scope-worker"}}

def scope_evidence(body):
    scope_patch.write_bytes(body)
    digest = hashlib.sha256(body).hexdigest()
    return [{"kind": "delegate", "task_id": "scope-result", "attempt_id": "att_scope_write",
             "patch": ".oms/artifacts/scope-result.patch", "patch_sha256": digest, "exit": 0},
            {"kind": "patch-land", "task_id": "scope-result", "patch_sha256": digest, "exit": 0}]

extra_patch = (b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-old\n+new\n"
               b"diff --git a/other.txt b/other.txt\n--- /dev/null\n+++ b/other.txt\n@@ -0,0 +1 @@\n+extra\n")
scope_member = {"participants": [{"participant": "scope-worker", "owns": ["value.txt"]}]}
with patch.dict(os.environ, environment, clear=True), \
        patch.object(saved, "_plan_bindings", return_value={}), \
        patch.object(saved, "_register"), patch.object(saved, "_plan_hint"), \
        patch.object(saved.room, "status", return_value=scope_member):
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(extra_patch))):
        changes = saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]
        assert changes["out_of_scope"] == {"count": 1, "paths": ["other.txt"]}, changes
        for task, main in (("scope-result", None), ("whole-scope-main", "scope-main")):
            attempts = [scope_worker] + ([{"attempt_id": main, "tool": "panel-main", "provider": "codex"}] if main else [])
            with patch.object(saved, "_records", return_value=(attempts, scope_evidence(extra_patch))), \
                    patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": main} if main else {}):
                try:
                    saved.finalize(project, "codex", task, str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("out-of-scope write patch accepted")
                except ValueError as error:
                    assert "1 path(s) outside its declared scope" in str(error) and "other.txt" in str(error), error
                assert saved.finalize(project, "codex", task, str(summary), "completed")["outcome"] == "completed"
        for owns in ([], None):
            scope_member["participants"][0]["owns"] = owns
            assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"]["count"] == 0
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
    scope_member["participants"][0]["owns"] = ["docs/*.md"]
    glob_patch = b"diff --git a/docs/readme.md b/docs/readme.md\n--- /dev/null\n+++ b/docs/readme.md\n@@ -0,0 +1 @@\n+doc\n"
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(glob_patch))):
        assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"]["count"] == 0
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
    rename_patch = b"diff --git a/docs/readme.md b/outside.md\nsimilarity index 100%\nrename from docs/readme.md\nrename to outside.md\n"
    for owns, outside in ((["docs/*.md"], "outside.md"), (["outside.md"], "docs/readme.md")):
        scope_member["participants"][0]["owns"] = owns
        with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(rename_patch))):
            assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"] == {"count": 1, "paths": [outside]}
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("out-of-scope rename accepted")
            except ValueError as error:
                assert "outside its declared scope" in str(error) and outside in str(error), error
    many_patch = b"".join(("diff --git a/file%d b/file%d\nGIT binary patch\nliteral 0\n\n" % (n, n)).encode() for n in range(25))
    assert saved._patch_paths(many_patch) == ["file%d" % n for n in range(25)]
    scope_patch.write_bytes(many_patch)
    assert saved._patch_paths(scope_patch) == saved._patch_paths(many_patch)
    assert saved._scope_drift(saved._patch_paths(many_patch), ["file0"])[-1] == "file24"
    long_name = "docs/" + "x" * 180 + ".md"
    assert saved._patch_paths(("diff --git a/%s b/%s\n" % (long_name, long_name)).encode()) == [long_name]
    quoted_patch = b'diff --git "a/docs/caf\\303\\251.md" "b/docs/caf\\303\\251.md"\nGIT binary patch\n'
    assert saved._patch_paths(quoted_patch) == ["docs/café.md"]
    spaced = "docs/a b/read me.md"
    assert saved._patch_paths(("diff --git a/%s b/%s\nGIT binary patch\n" % (spaced, spaced)).encode()) == [spaced]
    plain = b"--- a/first.txt\n+++ b/first.txt\n@@ -1 +1 @@\n--- content\n+++ content\n--- a/second.txt\n+++ b/second.txt\n@@ -0,0 +1 @@\n+new\n"
    assert saved._patch_paths(plain) == ["first.txt", "second.txt"]
    with patch.object(saved, "verified_patch", return_value=plain):
        stats = saved._diffstat(project, {})
        assert stats == {"files": [["first.txt", 1, 1], ["second.txt", 1, 0]], "count": 2, "added": 2, "removed": 1}, stats
    with patch.object(saved, "verified_patch", return_value=many_patch):
        stats = saved._diffstat(project, {})
        assert stats["count"] == 25 and len(stats["files"]) == 20, stats
    scope_member["participants"][0]["owns"] = ["file0"]
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(many_patch))):
        drift = saved.results(project)["rows"][0]["calls"][0]["changes"]["out_of_scope"]
        assert drift == {"count": 24, "paths": ["file%d" % n for n in range(1, 9)]}, drift
        try:
            saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("drift after display file cap accepted")
        except ValueError as error:
            assert "24 path(s) outside its declared scope" in str(error) and "file8" in str(error) and "file9" not in str(error), error
    assert saved._patch_paths(b"--- a/deleted.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n") == ["deleted.txt"]
# END patch-scope finalize regression.

whole = json.loads(call(whole_finalize, owner_env))
assert whole["main_attempt_id"] == main_attempts["claude"] and whole["admission_event_ids"]
joined = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                         "--task-id", "whole-main-result", "--json"]))["rows"][0]
assert any(c.get("kind") == "delegate" for c in joined["calls"]), joined
assert joined["title"] == whole["summary"].split("\n")[0]
assert all(joined[key] is None for key in ("model", "role", "location", "process_state"))
parent_task = {"task_id": "parent-read", "main_attempt_id": "main-parent", "revision": "a" * 24,
               "outcome": "completed", "verification": {"status": "passed"},
               "summary": "Owner summary for the parent read.\nMore detail."}
unrelated_child = {"task_id": "failed-child", "attempt_id": "att_failed_child", "parent_attempt_id": "main-parent",
                   "state": "failed", "refs": {"panel_label": "Unrelated failed write", "panel_model": "gpt-test",
                   "panel_role": "worker", "panel_location": "remote"}}
failed_call = {"kind": "delegate", "task_id": "failed-child", "attempt_id": "att_failed_child", "exit": 1}
parent_result_row = {"kind": "panel-result", "task_id": "parent-read"}
with patch.object(saved, "_records", return_value=([unrelated_child], [parent_result_row, failed_call])), \
        patch.object(saved, "_load_result", return_value=parent_task):
    parent = saved.results(project, "parent-read")["rows"][0]
    assert parent["title"] == "Owner summary for the parent read."
    assert parent["issue"] is None and parent["outcome"] == "completed"
    assert parent["verification"]["status"] == "passed"
    assert parent["model"] is None and parent["role"] is None and parent["location"] is None
    assert parent["process_state"] is None and parent["calls"] == [{"kind": "delegate", "exit": 1,
                                                                    "attempt_id": "att_failed_child"}]
own_task_result = dict(parent_task, task_id="own-task", summary="Own task summary.")
own_attempt = {"task_id": "own-task", "attempt_id": "att_own_task", "parent_attempt_id": "main-parent",
               "state": "done", "refs": {"panel_label": "Own task title", "panel_model": "own-model",
               "panel_role": "reviewer", "panel_location": "local"}}
own_call = {"kind": "call", "task_id": "own-task", "attempt_id": "att_own_task", "exit": 0}
with patch.object(saved, "_records", return_value=([own_attempt, unrelated_child],
        [{"kind": "panel-result", "task_id": "own-task"}, own_call, failed_call])), \
        patch.object(saved, "_load_result", return_value=own_task_result):
    own_metadata = saved.results(project, "own-task")["rows"][0]
    assert (own_metadata["title"], own_metadata["model"], own_metadata["role"],
            own_metadata["location"], own_metadata["process_state"]) == (
                "Own task title", "own-model", "reviewer", "local", "done")
    assert [call["attempt_id"] for call in own_metadata["calls"]] == ["att_own_task"]
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
        patch.object(panel, "check_legacy_main"), \
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
            patch.object(panel, "check_legacy_main"), \
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
                  ["--health", "--launch", "codex"], ["--health", "--dry-run"], ["--health", "--view", "graph"],
                  ["--json", "--view", "compact"], ["--results", "--attention-only"],
                  ["--launch", "codex", "--view", "detail"],
                  ["--finalize", "--dry-run"], ["--retry-delivery"], ["--results", "--notify"], ["--participant", "w1"],
                  ["--position", "top"],
                  ["--council", "--rounds", "3"],
                  ["--dispatch", "advisor", "--access", "write", "--prompt", "change source"],
                  ["--dispatch", "worker", "--access", "write", "--purpose", "implement", "--prompt", "change source"],
                  ["--dispatch", "worker", "--workload", "main", "--prompt", "explain"],
                  ["--dispatch", "verifier", "--access", "write", "--brief-file", "b.md", "--verify", "true"],
                  ["--dispatch", "reviewer", "--continue", "t1", "--brief-file", "b.md"],
                  ["--dispatch", "builder", "--purpose", "implement", "--prompt", "explain"],
                  ["--dispatch", "advisor", "--purpose", "explain", "--prompt", "explain"],
                  ["--dispatch", "worker", "--continue", "t1", "--prompt", "explain"],
                  ["--dispatch", "worker", "--continue", "t1", "--brief-file", "b.md", "--task-id", "t2"],
                  ["--dispatch", "advisor", "--continue", "t1", "--brief-file", "b.md"],
                  ["--continue", "t1", "--brief-file", "b.md"],
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
health_state = {"collection": {"ok": True}, "room": {"participants": [], "message_count": 0}}
health_out = io.StringIO()
with patch.object(panel, "read_panel", return_value=(health_state, 0, "codex", None, "auto", False, "")), \
        patch.object(panel, "run_native", side_effect=AssertionError("health launched a native session")), \
        patch.object(panel, "run_dispatch", side_effect=AssertionError("health dispatched a model")), \
        patch.object(panel.sys, "stdout", health_out):
    assert panel.main(["--repo", str(project), "--health", "--json"]) == 0
assert json.loads(health_out.getvalue())["schema"] == 1 and "items" in json.loads(health_out.getvalue())
health_out = io.StringIO()
with patch.dict(os.environ, {}, clear=False), patch.object(panel.room, "status", return_value={"closed": True}), \
        patch.object(panel, "snapshot", return_value=(dict(health_state, room={"closed": True, "participants": [], "message_count": 0}), 0)) as selected, \
        patch.object(panel, "read_panel", side_effect=AssertionError("explicit health room ignored")), \
        patch("panel_metrics.collect", return_value={}), patch.object(panel, "private_capacity", return_value={}), \
        patch.object(panel.sys, "stdout", health_out):
    assert panel.main(["--repo", str(project), "--health", "--room", "health-room", "--json"]) == 0
    assert selected.call_args.kwargs["room_id"] == "health-room"
assert any(item["label"] == "Room closed" and item["status"] == "full" for item in json.loads(health_out.getvalue())["items"])
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

    def terminal(command, inputs, expected, trigger=None, signal_after=None, expected_exit=0, timeout=20, then=None):
        # then=(marker, bytes): sent once marker is drawn after the first inputs, so a later click meets its hit map.
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe(command[0], command, dict(environment, TERM="xterm", OMS_AGENT="codex"))
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        initial_tty = termios.tcgetattr(fd) if signal_after is not None or expected_exit in (129, 143) else None
        pending = inputs.splitlines(keepends=True)
        prompts = sent_at = 0
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
                    sent_at = len(output)
                elif then and prompts and then[0] in output[sent_at:]:
                    os.write(fd, then[1])
                    then = None
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
                  "--resume", "label-session-id", "--task", "x" * length], b"", b"NATIVE_FIXTURE_READY")
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
        assert any(session in row["argv"] for session in ("exact-session-id", "label-session-id"))
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
        env = dict(environment, TERM="xterm", OMS_PANEL_NO_ANIMATION="1", NO_COLOR="",
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
            # Panel keys act only in OMS panel sessions; other sessions receive the key itself.
            tmux_wait(["list-keys", "-T", "root", "F9"], "select-pane")
            tmux_wait(["list-keys", "-T", "root", "F9"], "#{@oms_panel_repo}")
            tmux_wait(["list-keys", "-T", "root", "F5"], "select-window -t :=control")
            tmux_wait(["list-keys", "-T", "root", "F12"], "OMS panel keys")
            subprocess.run(tmux + ["set-environment", "-t", session, "PANEL_TEST_NATIVE_HOLD", "1"], env=env, check=True)
            panes = tmux_wait(["list-panes", "-t", session, "-F", "#{pane_id}\t#{pane_start_command}"], "--watch").splitlines()
            control = next(row.split("\t")[0] for row in panes if "--watch" not in row)
            tmux_wait(["capture-pane", "-p", "-t", control], "Press 1 (Claude) or 2 (Pi)")
            subprocess.run(tmux + ["send-keys", "-t", control, "v"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_view"], "compact")
            subprocess.run(tmux + ["send-keys", "-t", control, "b"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_attention"], "1")
            for owner, key in (("codex", "0"), ("claude", "1")):
                subprocess.run(tmux + ["send-keys", "-t", control, key], env=env, check=True)
                tmux_wait(["list-windows", "-t", session, "-F", "#{window_name}"], owner)
                # [native chat, board]: the board is the window's first (left) pane.
                listing = tmux_wait(["list-panes", "-t", session + ":" + owner,
                                     "-F", "#{pane_id}"], "\n%").splitlines()[::-1]
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
                main_col = main_text.index("MAIN / ") + 2
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "\033[<0;%s;%sM" % (main_col, main_row)], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "0")
                tmux_wait(["display-message", "-p", "-t", session + ":" + owner, "#{pane_id}"], listing[0])
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "v"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "1")
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "Escape"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "0")
                tmux_wait(["show-option", "-w", "-v", "-t", listing[1], "@oms_panel_view"], "graph")
                assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0],
                    "#{pane_pid}"], env=env, text=True) == native_pid
                assert subprocess.check_output(tmux + ["show-option", "-g", "-v", "mouse"], env=env, text=True).strip() == "off"
                assert subprocess.check_output(tmux + ["show-option", "-t", session, "-v", "mouse"], env=env, text=True).strip() == "on"
                subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], owner + " session exited (0)")
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], "r resume/restart here")
                if owner == "claude":
                    before_windows = subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                             env=env, text=True)
                    first_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[0],
                                                                   "@oms_panel_main_attempt"], env=env, text=True).strip()
                    first_main = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                                  "show", "--attempt", first_attempt, "--json"], env))
                    assert first_main["terminal"] and first_main["reason_code"] == "native_exit", first_main
                    native_id = first_main["refs"]["native_session"]
                    assert uuid.UUID(native_id).version == 4
                    first_launch = json.loads(log.read_text().splitlines()[-1])
                    assert first_launch["argv"][first_launch["argv"].index("--session-id") + 1] == native_id
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "-l", "r"], env=env, check=True)
                    deadline = time.monotonic() + 15
                    while True:
                        next_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[0],
                                                                      "@oms_panel_main_attempt"], env=env, text=True).strip()
                        if next_attempt != first_attempt:
                            break
                        assert time.monotonic() < deadline, "restart did not replace the attempt"
                        time.sleep(.1)
                    tmux_wait(["display-message", "-p", "-t", listing[0], "#{@oms_panel_native_pane}"], listing[0])
                    next_main = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                                 "show", "--attempt", next_attempt, "--json"], env))
                    assert next_main["refs"]["native_session"] == native_id
                    assert next_main["refs"]["panel_room_participant"] == first_attempt
                    assert subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                   env=env, text=True) == before_windows, "restart added a window"
                    assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0], "#{pane_pid}"],
                                                   env=env, text=True) == native_pid, "restart replaced the pane"
                    deadline = time.monotonic() + 15
                    while True:
                        next_launch = json.loads(log.read_text().splitlines()[-1])
                        if "--resume" in next_launch["argv"]:
                            break
                        assert time.monotonic() < deadline, "restart did not resume the native session"
                        time.sleep(.1)
                    assert next_launch["argv"][next_launch["argv"].index("--resume") + 1] == native_id
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
                    tmux_wait(["display-message", "-p", "-t", listing[0], "[#{@oms_panel_native_pane}]"], "[]")
            # One panel-wide board geometry in cells: both mains end with the same board size, a drag in the active
            # window is recorded and the other main follows; a zoomed board is left alone.
            shared = {}
            for owner in ("codex", "claude"):
                rows = subprocess.check_output(tmux + ["list-panes", "-t", session + ":" + owner, "-F",
                                                       "#{pane_id}\t#{pane_start_command}"], env=env, text=True)
                shared[owner] = next(row.split("\t")[0] for row in rows.splitlines() if "--watch" in row)
            subprocess.run(tmux + ["select-window", "-t", session + ":codex"], env=env, check=True)
            time.sleep(2.5)  # the codex watcher must see its window active before the drag
            geometry = subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["codex"],
                "#{window_width} #{window_height} #{pane_width}"], env=env, text=True).split()
            window, extent = int(geometry[0]), "#{pane_width}"
            assert int(geometry[2]) < window, geometry
            tmux_wait(["display-message", "-p", "-t", shared["claude"], extent],
                      subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["codex"], extent],
                                              env=env, text=True).strip(), timeout=20)
            dragged = panel.split_cells(("left", window * 3 // 10, False), window)
            subprocess.run(tmux + ["resize-pane", "-t", shared["codex"], "-x", str(dragged)], env=env, check=True)
            tmux_wait(["show-options", "-v", "-t", session, "@oms_panel_split"], panel.split_text(dragged))
            tmux_wait(["display-message", "-p", "-t", shared["claude"], extent], str(dragged), timeout=20)
            tmux_wait(["display-message", "-p", "-t", shared["codex"], extent], str(dragged), timeout=20)
            subprocess.run(tmux + ["resize-pane", "-t", shared["claude"], "-Z"], env=env, check=True)
            wider = panel.split_cells(("left", window * 7 // 10, False), window)
            subprocess.run(tmux + ["set-option", "-t", session, "@oms_panel_split", panel.split_text(wider)],
                           env=env, check=True)
            time.sleep(3)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["claude"],
                "#{window_zoomed_flag}"], env=env, text=True).strip() == "1", "a zoomed board was resized"
            subprocess.run(tmux + ["resize-pane", "-t", shared["claude"], "-Z"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", shared["claude"], extent], str(wider), timeout=20)
            control_sidebar = next(row.split("\t")[0] for row in panes if "--watch" in row)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "claude")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Claude --")
            subprocess.run(tmux + ["send-keys", "-t", control, "4"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", control], "Main harness (claude/pi/codex)")
            subprocess.run(tmux + ["send-keys", "-t", control, "codex", "Enter"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "codex")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Codex --")
            records = [json.loads(line) for line in log.read_text().splitlines()]
            assert [r["agent"] for r in records[-3:]] == ["codex", "claude", "claude"]
            assert all(r["cwd"] == str(project.resolve()) for r in records[-3:])
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
                os.execvpe("bash", ["bash", str(panel.ENTRY), "panel", "--host", "tmux",
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
                                   "#{pane_id}\t#{pane_left}\t#{pane_height}\t#{pane_start_command}"], "--watch").splitlines()
            chat_row = next(row.split("\t", 3) for row in pane_rows if "--launch" in row)
            graph_row = next(row.split("\t", 3) for row in pane_rows if "--watch" in row)
            chat_pane, graph_pane = chat_row[0], graph_row[0]
            assert int(graph_row[1]) < int(chat_row[1]) and graph_row[2] == chat_row[2], pane_rows
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
                                       "--reopen"], reopen_env))
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
            # A call opens in a reader pane right of the board, above the chat (the board's click/Enter choice is unit
            # tested with panel_reader.picked); another call reuses that reader, --reopen ignores it, Esc on the board
            # closes only it, and q in the reader closes it and returns focus to the board. The native chat process
            # is never touched.
            def window_rows():
                return [row.split("\t") for row in subprocess.check_output(tmux + ["list-panes", "-t", top_window, "-F",
                        "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{@oms_panel_reader}"], env=env, text=True).splitlines()]
            def wait_panes(count, timeout=15):
                deadline = time.monotonic() + timeout
                while len(window_rows()) != count:
                    assert time.monotonic() < deadline, window_rows()
                    time.sleep(.1)
                return window_rows()
            with patch.dict(os.environ, reopen_env, clear=True):
                opened = panel.panel_reader.open_reader(graph_pane, panel.reader_shell(
                    project, ("result", "fixture-worker"), graph_pane), project)
            rows = wait_panes(3)
            reader = next(row[0] for row in rows if row[3] == graph_pane)
            assert opened == reader, (opened, rows)
            chat_at = next(row for row in rows if row[0] == chat_pane)
            reader_at = next(row for row in rows if row[0] == reader)
            assert reader_at[1] == chat_at[1] and int(reader_at[2]) < int(chat_at[2]), rows
            assert next(row for row in rows if row[0] == graph_pane)[1] == "0", rows
            tmux_wait(["capture-pane", "-p", "-t", reader], "q/Esc close")
            assert "OMS / Results" in subprocess.check_output(tmux + ["capture-pane", "-p", "-t", reader], env=env, text=True)
            with patch.dict(os.environ, reopen_env, clear=True):
                assert panel.panel_reader.open_reader(graph_pane, panel.reader_shell(
                    project, ("debate", ("fixture-task", "fixture-seat")), graph_pane), project) == reader
            assert len(wait_panes(3)) == 3
            assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--reopen"],
                                   reopen_env))["status"] == "already_open"
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            assert reader not in [row[0] for row in wait_panes(2)]
            with patch.dict(os.environ, reopen_env, clear=True):
                reader = panel.panel_reader.open_reader(graph_pane, panel.reader_shell(project, ("result", "fixture-worker"),
                                                                                 graph_pane), project)
            tmux_wait(["capture-pane", "-p", "-t", reader], "q/Esc close")
            subprocess.run(tmux + ["select-pane", "-t", reader], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", reader, "-l", "q"], env=env, check=True)
            assert reader not in [row[0] for row in wait_panes(2)]
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], graph_pane)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            # After the first main ends, a replacement starts without a terminal in the same panel and room,
            # no new session, no focus change.
            starter_id = next(p["participant"] for p in room.status(project, menu_room)["participants"]
                              if p["role"] == "main" and p["provider"] == "codex" and p["joined"])
            windows_before = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                         env=env, text=True).splitlines())
            current_before = subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                                     env=env, text=True).strip()
            spawn_env = dict(env, OMS_ROOM_ID=menu_room, OMS_ROOM_PARTICIPANT=starter_id, OMS_PANEL_SESSION="")
            refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude", "--json"],
                                     env=spawn_env, capture_output=True, text=True, timeout=15)
            assert refused.returncode != 0 and "already has a live main" in refused.stderr, refused.stderr
            assert set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                               env=env, text=True).splitlines()) == windows_before
            # Direct inline launch cannot bypass the process-lifetime lease held by the native fixture.
            bypass = subprocess.run([sys.executable, "-c",
                "import sys; from pathlib import Path; sys.path.insert(0, sys.argv[1]); import terminal_panel as p; p.run_native('claude', Path(sys.argv[2]))",
                str(root / "scripts/lib"), str(project)], env=spawn_env, capture_output=True, text=True, timeout=15)
            assert bypass.returncode != 0 and "panel operation is busy" in bypass.stderr, bypass.stderr
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "-l", "hello native chat"], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "Enter"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-J", "-t", chat_pane], "NATIVE_CHAT_REPLY:hello native chat")
            tmux_wait(["capture-pane", "-p", "-t", chat_pane], "codex session exited (0)")
            spawned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude",
                                       "--task", "Spawned fixture", "--json"], spawn_env))
            assert spawned["provider"] == "claude" and spawned["room"] == menu_room and isinstance(spawned["window"], int), spawned
            new_window = next(iter(set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                               env=env, text=True).splitlines()) - windows_before))
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                           env=env, text=True).strip() == current_before, "spawning moved focus"
            assert set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env,
                                               text=True).splitlines()) == known_sessions, "spawning opened a session"
            tmux_wait(["show-option", "-w", "-v", "-t", new_window, "@oms_panel_room"], menu_room)
            tmux_wait(["list-panes", "-t", new_window, "-F", "#{pane_id}"], "\n%")
            watched_env = dict(env, TMUX="fixture", OMS_PANEL_SESSION=session, OMS_ROOM_ID=menu_room,
                               COLUMNS="140", LINES="44")
            board_text = ""
            for unused in range(50):
                board_text = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", menu_room,
                                   "--watch", "--count", "1", "--view", "graph"], watched_env)
                if "One main active or unverified" in board_text:
                    break
                time.sleep(.2)
            # The start bar sits right above the pinned key hints.
            assert "One main active or unverified" in board_text.splitlines()[-2], board_text
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "-l", "r"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-J", "-t", chat_pane], "another main is live or unverified")
            # Enter closes the retired main's entire window and its board watcher.
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "Enter"], env=env, check=True)
            deadline = time.monotonic() + 15
            while top_window in subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                        env=env, text=True).splitlines():
                assert time.monotonic() < deadline, "Enter left the old board window running"
                time.sleep(.1)
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
# Typed close is authorized by enrollment or creation and retains unread mail.
closed_room = room.create(project, "typed-close", "Close contract", creator="creator")
room.join(project, closed_room, "close-writer", "codex")
room.join(project, closed_room, "close-reader", "claude")
room.send(project, closed_room, "close-writer", "close-reader", "Preserve until read", message_id="close-mail")
for who in ("outsider",):
    try:
        room.close(project, closed_room, who)
    except ValueError as error:
        assert "not joined" in str(error)
    else:
        raise AssertionError("nonmember closed the room")
room.close(project, closed_room, "creator")
assert room.status(project, closed_room)["closed"]
assert room.records(project, closed_room)[-1]["room_event"] == {"kind": "closed", "participant": "creator"}
closed_rows = room.records(project, closed_room)
room.close(project, closed_room, "creator")
assert room.records(project, closed_room) == closed_rows, "close is idempotent"
for operation in (lambda: room.join(project, closed_room, "late", "codex"),
                  lambda: room.send(project, closed_room, "close-writer", "close-reader", "Late mail")):
    try:
        operation()
    except ValueError as error:
        assert "room is closed" in str(error)
    else:
        raise AssertionError("closed room accepted new enrollment/mail")
assert room.updates(project, closed_room, "close-reader")["turns"][0]["room_event"]["id"] == "close-mail"
room.acknowledge(project, closed_room, "close-reader", ["close-mail"])
assert not room.status(project, closed_room)["messages"][0]["pending_for"]
listed_rooms = json.loads(call(["bash", str(room.ENTRY), "room", "list", "--repo", str(project), "--json"], environment))
assert next(r for r in listed_rooms["rooms"] if r["id"] == closed_room)["closed"]
member_close = room.create(project, "member-close", "Member close")
room.join(project, member_close, "joined-closer", "codex")
closed_cli = json.loads(call(["bash", str(room.ENTRY), "room", "close", "--repo", str(project),
                              "--id", member_close, "--participant", "joined-closer", "--json"], environment))
assert closed_cli["status"] == "closed"
assert room.status(project, member_close)["closed"]
for env in ({"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": member_close, "OMS_ROOM_PARTICIPANT": "joined-closer"},
            {"OMS_ROOM_ID": member_close, "OMS_ROOM_PARTICIPANT": "foreign-caller"}):
    with patch.dict(os.environ, env):
        try:
            room.close(project, member_close, "joined-closer")
        except ValueError:
            pass
        else:
            raise AssertionError("child or foreign binding closed the room")
# A new participant starts after enrollment, even when history exceeds its first byte budget.
mail_room = room.create(project, "enrollment-mail", "Enrollment mailbox")
room.join(project, mail_room, "mail-writer", "codex")
room.send(project, mail_room, "mail-writer", "all", "Pre-enrollment mail", message_id="old-mail")
legacy_cursor = room.thread_live.updates(project, mail_room, limit=1)["cursor"]
room.join(project, mail_room, "mail-reader", "claude")
room.send(project, mail_room, "mail-writer", "mail-reader", "Assignment", message_id="first-mail")
initial_mail = room.updates(project, mail_room, "mail-reader", budget=1)
assert [r["room_event"]["id"] for r in initial_mail["turns"]] == ["first-mail"], "initial mail traversed pre-enrollment history"
assert not room.updates(project, mail_room, "mail-reader", initial_mail["cursor"])["turns"]
assert [r["room_event"]["id"] for r in room.updates(project, mail_room, "mail-reader", legacy_cursor)["turns"]] == ["first-mail"], "old cursors remain readable"
resume_room = room.create(project, "resume-mail", "Resume cutoff")
room.join(project, resume_room, "resume-writer", "codex")
room.join(project, resume_room, "resume-reader", "claude")
room.send(project, resume_room, "resume-writer", "resume-reader", "Before resume", message_id="before-resume")
room.append(project, resume_room, {"kind": "leave", "participant": "resume-reader"}, "Participant left")
room.join(project, resume_room, "resume-reader", "claude")
room.send(project, resume_room, "resume-writer", "resume-reader", "After resume", message_id="after-resume")
assert [r["room_event"]["id"] for r in room.updates(project, resume_room, "resume-reader")["turns"]] == [
    "after-resume"], "resume replayed mail from before the participant cutoff"
# Default reads are addressed unread mail; explicit cursors remain raw stream deltas.
unread_room = room.create(project, "unread-mail", "Unread mailbox")
room.join(project, unread_room, "unread-a", "codex")
room.join(project, unread_room, "unread-b", "claude")
room.join(project, unread_room, "unread-c", "codex")
start_cursor = room.updates(project, unread_room, "unread-b")["cursor"]
for number in range(60):
    message_id = "old-%s" % number
    room.send(project, unread_room, "unread-a", "unread-b", "Already consumed", message_id=message_id)
    room.acknowledge(project, unread_room, "unread-b", [message_id])
room.send(project, unread_room, "unread-c", "unread-b", "Direct from second sender", message_id="new-direct")
room.send(project, unread_room, "unread-a", "all", "Broadcast", message_id="new-broadcast")
room.send(project, unread_room, "unread-a", "unread-c", "Other recipient only", message_id="other-only")
before_read = room.records(project, unread_room)
page = room.updates(project, unread_room, "unread-b", budget=1, limit=1)
assert [row["room_event"]["id"] for row in page["turns"]] == ["new-direct"]
assert page["has_more"], "unread pagination ignored a later addressed message"
default_mail = room.updates(project, unread_room, "unread-b")
assert [row["room_event"]["id"] for row in default_mail["turns"]] == ["new-direct", "new-broadcast"]
assert not default_mail["has_more"] and room.records(project, unread_room) == before_read, "reading changed room state"
assert [row["room_event"]["id"] for row in room.updates(project, unread_room, "unread-b")["turns"]] == [
    "new-direct", "new-broadcast"], "a repeated read hid unacknowledged mail"
raw_delta = room.updates(project, unread_room, "unread-b", start_cursor)
raw_rows = list(raw_delta["turns"])
for _ in range(len(before_read)):
    if not raw_delta["has_more"]:
        break
    raw_delta = room.updates(project, unread_room, "unread-b", raw_delta["cursor"])
    raw_rows.extend(raw_delta["turns"])
assert {row["room_event"].get("id") for row in raw_rows} >= {"old-0", "old-59", "new-direct", "new-broadcast"}
other_mail = {row["room_event"]["id"] for row in room.updates(project, unread_room, "unread-c")["turns"]}
assert other_mail == {"new-broadcast", "other-only"}, "direct mail crossed recipients or broadcast was hidden"
room.acknowledge(project, unread_room, "unread-b", ["new-direct", "new-broadcast"])
assert room.updates(project, unread_room, "unread-b")["turns"] == [], "acknowledged mail remained in the default inbox"
try:
    room.updates(project, unread_room, "unread-b", "invalid-cursor")
except ValueError:
    pass
else:
    raise AssertionError("invalid room cursor was accepted")
# A cached hook projection must not move its cursor past mail appended before the scan.
race_room = room.create(project, "first-mail-race", "First delivery race")
room.join(project, race_room, "race-writer", "codex")
room.join(project, race_room, "race-reader", "claude")
room.join(project, race_room, "race-other", "codex")
room.send(project, race_room, "race-writer", "race-reader", "Consumed", message_id="race-old")
room.acknowledge(project, race_room, "race-reader", ["race-old"])
cached_room = room.project(room.records(project, race_room))
room.send(project, race_room, "race-writer", "race-reader", "New direct", message_id="race-direct")
room.send(project, race_room, "race-writer", "all", "New broadcast", message_id="race-broadcast")
room.send(project, race_room, "race-writer", "race-other", "Other reader", message_id="race-other-only")
race_page = room.updates(project, race_room, "race-reader", budget=1, limit=1, state=cached_room)
assert not race_page["turns"] and race_page["has_more"], "first read classified mail outside its projection"
race_next = room.updates(project, race_room, "race-reader", race_page["cursor"], budget=1, limit=1)
assert [r["room_event"]["id"] for r in race_next["turns"]] == ["race-direct"] and race_next["has_more"], "cursor skipped concurrent mail"
race_tail = room.updates(project, race_room, "race-reader", race_next["cursor"])
assert [r["room_event"]["id"] for r in race_tail["turns"]] == ["race-broadcast"], "cursor skipped broadcast or crossed recipients"
assert {r["room_event"]["id"] for r in room.updates(project, race_room, "race-reader")["turns"]} == {
    "race-direct", "race-broadcast"}, "delivery consumed mail or replayed acknowledged mail"
room.append(project, race_room, {"kind": "leave", "participant": "race-reader"}, "Reader left")
room.send(project, race_room, "race-writer", "all", "While absent", message_id="race-absent")
room.join(project, race_room, "race-reader", "claude")
stale_membership = room.updates(project, race_room, "race-reader", state=cached_room)
assert not stale_membership["turns"] and stale_membership["has_more"]
assert not room.updates(project, race_room, "race-reader", stale_membership["cursor"])["turns"], "cached enrollment leaked broadcast to a later join"
assert not room.updates(project, race_room, "race-reader")["turns"], "rejoining replayed mail from the previous enrollment"
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
               "--task-id", "room-inspection", "--label", "Inspect the shared parser"],
              {name: value for name, value in room_env.items() if name != "OMS_ROOM_REPO"})
assert "artifact:" in answer, answer
state = room.status(project, shared)
child = next(p for p in state["participants"] if p["role"] == "worker")
assert child["parent"] == room_owner and child["model"] == "gpt-6-luna", child
handoff = next(m for m in state["messages"] if m["sender"] == child["participant"])
assert handoff["recipient"] == room_owner and "parent acceptance pending" in handoff["text"], handoff
assert "inspected the bounded repository task" in handoff["text"], handoff
assert (project / "value.txt").read_text() == "old\n", "room cannot widen worker write access"
# A screened summary must still settle the room call, under the same result message id.
sensitive_summary = "pass" + "word=fixture-value"
room.join(project, shared, "screen-call", "codex", "worker", parent=room_owner)
with patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": sensitive_summary}]}]}):
    panel.deliver_result(project, shared, "screen-call", room_owner, "screen-task", 143)
screened = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-screen-call")
assert "Call exit=143" in screened["text"] and "--results --task-id screen-task" in screened["text"]
assert sensitive_summary not in screened["text"]
import io
from contextlib import redirect_stderr
conflict = io.StringIO()
with redirect_stderr(conflict):
    panel.deliver_result(project, shared, "screen-call", room_owner, "screen-task", 1)
assert "message-id conflict: result-screen-call" in conflict.getvalue()
def exercise_dispatch_exit_evidence(root, temporary, project, shared, room_owner, room_env):
    import io
    import signal
    from contextlib import redirect_stderr
    from unittest.mock import Mock, patch
    from panel_routing import allocate
    if os.name != "posix":
        return
    import panel_terminals
    # Exercise the real child, handlers and ledger without a provider call: leave an active attempt on TERM.
    interrupted_file = temporary / "interrupted-attempt"
    cleaned_file = temporary / "interrupted-cleaned"
    interrupted_child = temporary / "interrupted-child.py"
    interrupted_child.write_text('''import os, signal, sys, time
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import terminal_panel as panel
import room
def stop(number, frame):
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    time.sleep(0.1)
    Path(sys.argv[3]).write_text("cleaned")
    sys.exit(143)
signal.signal(signal.SIGTERM, stop)
repo, ident, member = Path(os.environ["OMS_ROOM_REPO"]), os.environ["OMS_ROOM_ID"], os.environ["OMS_ROOM_PARTICIPANT"]
room.scope(repo, ident, member, ["value.txt"])
attempt = panel.events(repo, "start", "--provider", "codex", "--tool", "peer-delegate", "--task-id", "interrupted-task",
                       "--parent-attempt-id", os.environ["OMS_PANEL_MAIN_ATTEMPT"],
                       "--ref", "panel_role=worker", "--ref", "panel_room_id=" + ident,
                       "--ref", "panel_room_participant=" + member, "--then", "starting", "--then", "working", output=True)
Path(sys.argv[2]).write_text(attempt)
os.kill(os.getppid(), signal.SIGTERM)
time.sleep(60)
''')
    saved = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    with patch.dict(os.environ, room_env, clear=True), \
            patch.object(panel.shutil, "which", return_value=sys.executable), \
            patch.object(panel, "routed_command", return_value=[sys.executable, str(interrupted_child),
                         str(root / "scripts/lib"), str(interrupted_file), str(cleaned_file), "--repo", str(project)]):
        interrupted_route = allocate("claude", "worker", "light", "read", "investigate", {"codex"})
        def menu_dispatch(*args, **kwargs):
            return panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                      "interrupted-task", False, "Interrupted call", interrupted_route,
                                      None, None, None)
        with patch.object(panel, "dispatch", side_effect=menu_dispatch), \
                patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel, "panel_mains", return_value=["claude"]), \
                patch.object(panel, "raw_keys", return_value=False), \
                patch.object(panel, "read_choice", return_value="5") as menu_choice, \
                patch.object(panel, "read_field", side_effect=["Inspect source", "light"]) as menu_fields:
            try:
                panel.interactive(project)
            except panel.NativeShutdown as error:
                assert error.exit_code == 143
            else:
                raise AssertionError("dispatch menu swallowed TERM after settlement")
            assert menu_choice.call_count == 1 and menu_fields.call_count == 2, "menu paused after TERM"
    assert all(signal.getsignal(number) == handler for number, handler in saved.items())
    interrupted = json.loads(panel.events(project, "show", "--attempt", interrupted_file.read_text(), "--json", output=True))
    assert interrupted["state"] == "cancelled" and interrupted["reason_code"] == "panel_call_interrupted", interrupted
    member = interrupted["refs"]["panel_room_participant"]
    settled = room.status(project, shared)
    assert cleaned_file.read_text() == "cleaned" and settled["call_results"][member]["exit"] == 143
    assert room.participant(settled, member)["joined"] and panel.scope_overlaps(project, shared, settled, room_owner, ["value.txt"]) == []
    with patch.object(panel, "main_history", return_value=[dict(interrupted, state="review", terminal=False)]):
        assert panel.scope_overlaps(project, shared, settled, room_owner, ["value.txt"]) == [], "verified exit receipt did not release scope"
    # HUP follows TERM out of run_dispatch; the turn-interrupt paths retain exit 130.
    for interruption in (panel.NativeShutdown(signal.SIGHUP), panel.NativeShutdown(signal.SIGINT), KeyboardInterrupt()):
        finished = Mock(pid=2147483647, poll=lambda: 0)
        finished.wait.side_effect = interruption
        with patch.dict(os.environ, {}, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel, "plan_link", return_value=None), \
                patch.object(panel.subprocess, "Popen", return_value=finished), \
                patch.object(panel_terminals, "process", return_value=None), \
                patch.object(panel, "_dispatch_group_gone", return_value=True):
            try:
                code = panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                          "interrupt-contract", False, None, interrupted_route, None, None, None)
            except panel.NativeShutdown as error:
                assert interruption is error and error.exit_code == 129
            else:
                assert code == 130
    # Observe a real provider in a new session, with its parent reaping it during fixture cleanup.
    subtree_file = temporary / "provider-subtree.pid"
    subtree_child = temporary / "provider-subtree.py"
    subtree_child.write_text('''import signal, subprocess, sys, time
from pathlib import Path
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
def stop(number, frame):
    child.terminate()
    child.wait(timeout=5)
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
Path(sys.argv[1]).write_text(str(child.pid))
time.sleep(60)
''')
    subtree = subprocess.Popen([sys.executable, str(subtree_child), str(subtree_file)], start_new_session=True)
    try:
        deadline = time.monotonic() + 5
        while not subtree_file.exists() and subtree.poll() is None and time.monotonic() < deadline:
            time.sleep(.02)
        detached = int(subtree_file.read_text())
        snapshot = panel._dispatch_descendants(subtree.pid)
        assert snapshot and any(row["pid"] == row["group"] == detached for row in snapshot), snapshot
        assert not panel._dispatch_descendants_gone(snapshot)
    finally:
        subtree.terminate()
        subtree.wait(timeout=5)
    assert panel._dispatch_descendants_gone(snapshot)
    # Force-stop follows the grace; signaling alone is not evidence that a process group exited.
    for identity_matches, descendants in ((True, []), (False, []), (True, None)):
        stopped = Mock(pid=123, poll=lambda: None)
        stopped.wait.side_effect = [subprocess.TimeoutExpired("fixture", 20), 0]
        with patch.object(panel, "_dispatch_descendants", return_value=descendants), \
                patch.object(panel_terminals, "process", return_value={"group": 123}), \
                patch.object(panel_terminals, "matches", return_value=identity_matches), \
                patch.object(panel_terminals, "process_gone", return_value=False), \
                patch.object(panel, "_dispatch_group_gone", return_value=True), \
                patch.object(panel.os, "killpg") as force, redirect_stderr(io.StringIO()):
            assert panel._stop_dispatch(stopped, "original-identity") is (identity_matches and descendants is not None)
        assert stopped.terminate.call_count == 1 and stopped.wait.call_args_list[0].kwargs == {"timeout": 20}
        assert force.call_count == int(identity_matches)
    disappeared = Mock(pid=123, poll=lambda: None)
    disappeared.terminate.side_effect = ProcessLookupError
    with patch.object(panel_terminals, "process_gone", return_value=True), \
            patch.object(panel, "_dispatch_group_gone", return_value=True):
        assert panel._stop_dispatch(disappeared, "original-identity") is True
    disappeared.wait.assert_not_called()
    real_popen = subprocess.Popen
    for scenario in ("unknown-identity", "kill-unconfirmed", "provider-alive", "leader-only"):
        pending = Mock(pid=2147483647, poll=lambda: 0 if scenario == "leader-only" else None)
        pending.wait.side_effect = ([0] if scenario == "leader-only" else
                                   [panel.NativeShutdown(signal.SIGTERM), subprocess.TimeoutExpired("fixture", 20)] +
                                   ([subprocess.TimeoutExpired("fixture", 5)] if scenario == "kill-unconfirmed" else
                                    [-signal.SIGKILL] if scenario == "provider-alive" else []))
        pending_attempt = []
        def pending_child(argv, *args, **kwargs):
            if argv[:3] != [sys.executable, "-c", "pass"]:
                return real_popen(argv, *args, **kwargs)
            env = kwargs["env"]
            member = env["OMS_ROOM_PARTICIPANT"]
            with patch.dict(os.environ, env, clear=True):
                room.scope(project, shared, member, ["value.txt"])
                pending_attempt.append(panel.events(project, "start", "--provider", "codex", "--tool", "peer-delegate",
                    "--task-id", scenario, "--parent-attempt-id", room_owner,
                    "--ref", "panel_role=worker", "--ref", "panel_room_id=" + shared,
                    "--ref", "panel_room_participant=" + member, "--then", "starting", "--then", "working", output=True))
            return pending
        observation = ({"identity": "original-identity", "group": pending.pid, "state": "S"}
                       if scenario != "unknown-identity" else None)
        provider = {"pid": 2147483646, "identity": "provider-identity", "group": 2147483646}
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel.subprocess, "Popen", side_effect=pending_child), \
                patch.object(panel_terminals, "process", return_value=observation), \
                patch.object(panel_terminals, "process_gone", side_effect=lambda pid, identity: scenario == "provider-alive" and pid == pending.pid), \
                patch.object(panel_terminals, "matches", return_value=True), \
                patch.object(panel, "_dispatch_descendants", return_value=[provider] if scenario == "provider-alive" else []), \
                patch.object(panel, "_dispatch_group_gone", side_effect=lambda pid: scenario == "provider-alive" and pid == pending.pid), \
                patch.object(panel.os, "killpg") as force, redirect_stderr(io.StringIO()) as notices:
            try:
                status = panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                            scenario, False, "Pending child", interrupted_route, None, None, None)
            except panel.NativeShutdown as error:
                assert scenario != "leader-only" and error.exit_code == 143
                status = error.exit_code
            else:
                assert scenario == "leader-only", "dispatch swallowed shutdown"
            row = json.loads(panel.events(project, "show", "--attempt", pending_attempt[0], "--json", output=True))
            member = row["refs"]["panel_room_participant"]
            state = room.status(project, shared)
            assert row["state"] == "working" and status == (1 if scenario == "leader-only" else 143), (scenario, row)
            assert member not in state["call_results"] and "settlement pending" in notices.getvalue()
            assert not panel._read(project, panel._dispatch_call_path(member, True), missing=True)
            record = json.loads(panel._read(project, panel._dispatch_call_path(member)))
            assert record["pid"] == pending.pid and record["identity"] == (observation or {}).get("identity")
            assert any(o["participant"] == member for o in panel.scope_overlaps(project, shared, state, room_owner, ["value.txt"]))
            assert force.call_count == int(scenario in {"kill-unconfirmed", "provider-alive"})
            deferred = panel._dispatch_pending(project, record)
            assert deferred["forced"] == (scenario in {"kill-unconfirmed", "provider-alive"})
            if scenario == "provider-alive":
                assert deferred["descendants"] == [provider]
            # A stale or premature final handoff cannot release an active child's scope.
            if scenario != "provider-alive":
                room.send(project, shared, member, room_owner, "Call exit=143; parent acceptance pending.",
                          kind="handoff", message_id="result-" + member)
            assert any(o["participant"] == member for o in panel.scope_overlaps(
                project, shared, room.status(project, shared), room_owner, ["value.txt"]))
        if scenario == "provider-alive":
            # No board read settles the pending call; once its detached provider exits, the next dispatch
            # settles the old attempt before launching. Concurrent calls are not capped.
            with patch.dict(os.environ, room_env, clear=True), \
                    patch.object(panel.shutil, "which", return_value=sys.executable), \
                    patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]):
                with patch.object(panel_terminals, "process_gone", return_value=True), \
                        patch.object(panel, "_dispatch_group_gone", return_value=True):
                    assert panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                              "after-pending", False, "Next call", interrupted_route, None, None, None) == 0
                reconciled = json.loads(panel.events(project, "show", "--attempt", pending_attempt[0], "--json", output=True))
                after = room.status(project, shared)
                assert reconciled["state"] == "cancelled" and reconciled["reason_code"] == "panel_call_interrupted"
                assert after["call_results"][member]["exit"] == 143
                assert not any(o["participant"] == member for o in panel.scope_overlaps(project, shared, after, room_owner, ["value.txt"]))
        else:
            # Explicit fixture cleanup keeps unresolved children out of unrelated pool/scope cases.
            panel.events(project, "transition", "--attempt", pending_attempt[0], "--state", "cancelled",
                         "--reason-code", "fixture_complete")
    print("dispatch exit evidence: menu TERM, unknown identity, unconfirmed kill, surviving groups and action-time reconciliation passed")

exercise_dispatch_exit_evidence(root, temporary, project, shared, room_owner, room_env)
# A room in A keeps its parent, call and results when the worker executes in B,
# whether the caller selects the room explicitly or inherits it.
other_project = temporary / "dispatch work repo"
subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(project), str(other_project)], check=True)
(other_project / "dispatch-brief.md").write_text("Task: append new to value.txt.\n")
cross_brief = other_project / "dispatch-brief.md"
verify_log = temporary / "cross-repo-verify"
for task_id, explicit_room, writing, continuing in (
        ("cross-repo-explicit", True, False, False),
        ("cross-repo-inherited", False, False, False),
        ("cross-repo-write", True, True, False),
        ("cross-repo-write", False, True, True)):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(other_project),
               "--dispatch", "worker", "--owner", "claude", "--workload", "light"]
    if writing:
        request += ["--purpose", "implement", "--access", "write", "--brief-file", cross_brief.name,
                    "--verify", 'git rev-parse --git-common-dir > "$PANEL_TEST_VERIFY_LOG"',
                    "--continue" if continuing else "--task-id", task_id]
    else:
        request += ["--purpose", "investigate", "--prompt", "Inspect value.txt without edits.", "--task-id", task_id]
    if explicit_room:
        request += ["--room", shared]
    # Write calls run without an explicit wall here so the panel's own default is observed.
    call_env = {key: value for key, value in room_env.items() if not (writing and key == "OMS_PEER_TIMEOUT")}
    answer = call(request, dict(call_env, PANEL_TEST_MODE="write" if writing else "",
                               PANEL_TEST_VERIFY_LOG=str(verify_log)))
    assert "artifact:" in answer, answer
    observed = json.loads(worker_log.read_text().splitlines()[-1])
    assert observed["task_id"] == task_id and observed["parent"] == room_owner, observed
    assert observed["room"] == shared and observed["room_repo"] == str(project), observed
    assert Path(observed["cwd"]) != other_project and Path(observed["cwd"]) != project, observed
    assert observed["git_common"] == str(other_project / ".git"), observed
    assert not Path(observed["cwd"]).exists(), "cross-repository worker cleanup failed"
    assert observed["timeout"] == ("40m" if writing else room_env.get("OMS_PEER_TIMEOUT")), observed
    if writing:
        assert verify_log.read_text().strip() == str(other_project / ".git"), "verification used the state repository"
    state = room.status(project, shared)
    enrolled = room.participant(state, observed["participant"])
    assert enrolled["parent"] == room_owner and enrolled["role"] == "worker", enrolled
    messages = state["messages"]
    assert any(m["recipient"] == observed["participant"] and m["text"].startswith("Assigned:") for m in messages), messages
    assert any(m["sender"] == observed["participant"] and m["recipient"] == room_owner
               and "inspected the bounded repository task" in m["text"] for m in messages), messages
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = [a for a in attempts if a.get("task_id") == task_id][-1]
    assert attempt["parent_attempt_id"] == room_owner and attempt["refs"]["panel_room_id"] == shared, attempt
    assert attempt["refs"]["panel_room_participant"] == observed["participant"], attempt
    report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                             "--task-id", task_id, "--json"], room_env))
    assert report["rows"] and any(c.get("answer") for row in report["rows"] for c in row.get("calls", [])), report
    assert not json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(other_project),
                               "list", "--json"])), "call attempt split into work repository"
    assert not (other_project / ".oms/artifacts/index.jsonl").exists(), "call artifact split into work repository"
    assert (other_project / "value.txt").read_text() == "old\n", "worker edited the source repository"
    assert (project / "value.txt").read_text() == "old\n", "worker edited the state repository"
cross_brief.unlink()
board = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results", "--json"], room_env))
assert {row["task_id"] for row in board["rows"]} >= {
    "cross-repo-explicit", "cross-repo-inherited", "cross-repo-write"}, board
for changed in ({"OMS_ROOM_PARTICIPANT": main_attempts["codex"]},
                {"OMS_ROOM_REPO": str(temporary / "missing state repo")},
                {"OMS_PANEL_MAIN_ATTEMPT": main_attempts["codex"]}):
    before = worker_log.read_bytes()
    refused = subprocess.run(request, env=dict(room_env, **changed), capture_output=True, text=True, timeout=30)
    assert refused.returncode != 0 and worker_log.read_bytes() == before, (changed, refused)
tamper = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(other_project), "--room", shared,
    "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate", "--workload", "light",
    "--prompt", "Inspect value.txt without edits.", "--task-id", "cross-repo-guard"],
    env=dict(room_env, PANEL_TEST_MODE="primary-write", PANEL_TEST_PRIMARY=str(other_project / "value.txt"),
             OMS_WORKER_GUARD_STRICT="1"), capture_output=True, text=True, timeout=90)
assert tamper.returncode != 0 and "changed protected state outside its worktree: tracked" in tamper.stderr, tamper
assert "source repository protected surfaces changed: tracked" in tamper.stderr, tamper.stderr
assert (other_project / "value.txt").read_text() == "old\noutside worker\n", "fixture did not exercise source protection"
assert (project / "value.txt").read_text() == "old\n", "source mutation reached the state repository"
(other_project / "value.txt").write_text("old\n")
def finish_fixture_reviews():
    # The fixture main accepts finished calls between independent phases, releasing its running pool slots.
    for row in json.loads(panel.events(project, "list", "--json", output=True)):
        if row.get("parent_attempt_id") == room_owner and row.get("state") == "review":
            panel.events(project, "transition", "--attempt", row["attempt_id"], "--state", "done",
                         "--reason-code", "fixture_complete")
finish_fixture_reviews()
# A dispatch with a shared-plan task ID moves that task for the calling main only: dry-run leaves it ready, the
# claim records the main, and every further worker of the same main reuses the running lease.
plan_cli = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(project)]
call(plan_cli + ["init", "--goal", "dispatch moves the plan"])
call(plan_cli + ["add", "--id", "plan-link", "--title", "Plan-linked dispatch"])
plan_dispatch = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                 "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate", "--workload", "light",
                 "--prompt", "Inspect value.txt without edits.", "--task-id", "plan-link", "--label", "Plan linked"]
plan_task = lambda: {k: v for k, v in json.loads(call(plan_cli + ["show", "--id", "plan-link"])).items() if k != "claim_age_s"}
call(plan_dispatch + ["--dry-run"], room_env)
assert plan_task()["state"] == "ready", plan_task()
linked = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert linked.returncode == 0, linked
held = plan_task()
assert held["state"] == "running" and held["provider"] == "claude" and held["claimed_by_participant"] == room_owner, held
assert "plan plan-link running (lease %s)" % held["lease_id"] in linked.stderr, linked.stderr
assert "agent-plan review --id plan-link --lease-id %s" % held["lease_id"] in linked.stderr, linked.stderr
assert "agent-plan finish --id plan-link --landed-commit SHA" in linked.stderr, linked.stderr
again = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert again.returncode == 0 and plan_task() == held and "lease %s" % held["lease_id"] in again.stderr, again
call(plan_cli + ["add", "--id", "plan-held", "--title", "Held by codex"])
call(plan_cli + ["claim", "--id", "plan-held", "--provider", "codex"],
     dict(environment, OMS_ROOM_PARTICIPANT=main_attempts["codex"]))
other = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-held", "--label", "Held elsewhere"], env=room_env,
                       capture_output=True, text=True)
assert other.returncode == 0 and "another claimant" in other.stderr, other
assert json.loads(call(plan_cli + ["show", "--id", "plan-held"]))["state"] == "claimed"
call(plan_cli + ["add", "--id", "plan-blocked", "--title", "Blocked before write"])
call(plan_cli + ["block", "--id", "plan-blocked", "--reason", "waits on a decision"])
call(plan_cli + ["add", "--id", "plan-ownerless", "--title", "Ownerless claim"])
call(plan_cli + ["claim", "--id", "plan-ownerless", "--provider", "codex"])
(project / "write-brief.md").write_text("Change nothing.\n")
write_plan_dispatch = (plan_dispatch[:plan_dispatch.index("--prompt")] +
                       ["--purpose", "implement", "--access", "write", "--workload", "light",
                        "--brief-file", str(project / "write-brief.md"), "--verify", "true"])
thread = project / ".oms/threads" / (shared + ".jsonl")
for refused_id in ("plan-blocked", "plan-held", "plan-ownerless"):
    before_thread, before_provider = thread.read_bytes(), worker_log.read_bytes()
    refused = subprocess.run(write_plan_dispatch + ["--task-id", refused_id, "--label", "Refused write"],
                             env=room_env, capture_output=True, text=True)
    assert refused.returncode != 0 and "reopen or claim it before dispatching a write worker" in refused.stderr, refused
    assert thread.read_bytes() == before_thread, "a refused write dispatch must not join the room"
    assert worker_log.read_bytes() == before_provider, "a refused write dispatch must not call a provider"
# Known unclaimable tasks keep informational read dispatch compatibility.
read_blocked = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-blocked", "--label", "Read blocked task"],
                              env=room_env, capture_output=True, text=True)
assert read_blocked.returncode == 0 and "plan plan-blocked is blocked" in read_blocked.stderr, read_blocked
# Standalone known plan tasks have no main whose exact binding can be observed.
call(plan_cli + ["add", "--id", "plan-standalone", "--title", "Standalone write"])
standalone_before = worker_log.read_bytes()
standalone_env = dict(environment)
standalone_env.pop("CLAUDE_CODE_SESSION_ID", None)
standalone_argv = list(write_plan_dispatch)
room_arg = standalone_argv.index("--room")
del standalone_argv[room_arg:room_arg + 2]
standalone = subprocess.run(standalone_argv + ["--task-id", "plan-standalone", "--label", "Refused standalone"],
                            env=standalone_env, capture_output=True, text=True)
assert standalone.returncode != 0 and "no owned main relationship" in standalone.stderr, standalone
assert worker_log.read_bytes() == standalone_before
assert json.loads(call(plan_cli + ["show", "--id", "plan-standalone"]))["state"] == "ready"
# Missing tasks retain the legacy unprotected write path.
missing_before = worker_log.read_bytes()
legacy = subprocess.run(write_plan_dispatch + ["--task-id", "plan-not-recorded", "--label", "Legacy write"],
                        env=room_env, capture_output=True, text=True)
assert legacy.returncode == 0 and worker_log.read_bytes() != missing_before, legacy
finish_fixture_reviews()
call(plan_cli + ["review", "--id", "plan-link", "--lease-id", held["lease_id"]])
closed = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert closed.returncode == 0 and plan_task()["state"] == "review" and "plan plan-link is review" in closed.stderr, closed
before_thread, before_provider = thread.read_bytes(), worker_log.read_bytes()
review_write = subprocess.run(write_plan_dispatch + ["--task-id", "plan-link", "--label", "Refused review write"],
                              env=room_env, capture_output=True, text=True)
assert review_write.returncode != 0 and "reopen or claim it before dispatching a write worker" in review_write.stderr, review_write
assert thread.read_bytes() == before_thread and worker_log.read_bytes() == before_provider, review_write
(project / "write-brief.md").unlink()  # The later blocking reviewer must see only its source fixture.
hinted = subprocess.run(final_args[:final_args.index("--task-id") + 1] + ["plan-link"] + final_args[final_args.index("--task-id") + 2:],
                        cwd=project, env=environment, capture_output=True, text=True)
assert hinted.returncode == 0 and "agent-plan finish --id plan-link --landed-commit SHA" in hinted.stderr, hinted
def exercise_observed_worker_leases():
    # Every success marker comes from the real peer dispatcher while the fake CLI waits.
    import time
    plan_api = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(project)]
    # The observer tuple belongs on the peer dispatcher argv; the native CLI
    # receives ordinary model flags and cannot expose this frontend argument.
    argv_binding = panel.PlanBinding("plan_" + "1" * 32, "argv-observer", "lease_" + "2" * 32,
                                     "codex", "claude", "argv-main", "att_" + "3" * 32, "argv-room")
    captured_dispatch = {}
    route = panel.selected_route("claude", "worker", "light", "write", "implement")
    dispatch_args = (project, "claude", "worker", "write",
                     None, brief, "--task-id", "argv-observer", False, None, route, None, None, None)
    # The fake provider CLIs live in the fixture PATH, but this in-process
    # observer check inherits the host PATH. Keep route availability explicit
    # so CI hosts without Codex or Claude still exercise the real dispatcher.
    with patch.object(panel.shutil, "which", return_value=None), \
            patch.object(panel.subprocess, "Popen") as unavailable_call:
        try:
            panel.run_dispatch(*dispatch_args)
        except ValueError as error:
            assert "%s is not installed" % route["provider"] in str(error), error
        else:
            raise AssertionError("unavailable worker route was accepted")
        unavailable_call.assert_not_called()
    with patch.object(panel.shutil, "which", return_value=str(binary / route.get("binary", route["provider"]))), \
            patch.object(panel, "moved_main", return_value=None), \
            patch.object(panel, "child_environment", return_value={}), \
            patch.object(panel, "native_repository", return_value=project), \
            patch.object(panel, "plan_link", return_value=argv_binding), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as preflight, \
            patch.object(panel.subprocess, "Popen", side_effect=lambda argv, **kwargs:
                         captured_dispatch.update(argv=argv) or Mock(pid=0, wait=lambda: 0, poll=lambda: 0)):
        assert panel.run_dispatch(*dispatch_args) == 0
    preview_argv = preflight.call_args.args[0]
    assert preview_argv[-1] == "--dry-run" and "--observe-plan-binding" not in preview_argv
    assert preview_argv[preview_argv.index("--verify") + 1] == "--task-id"
    assert preview_argv[-3] == "--task-id" and preview_argv[-2].startswith("preflight-")
    dispatch_argv = captured_dispatch["argv"]
    assert "--observe-plan-binding" in dispatch_argv, dispatch_argv
    assert json.loads(dispatch_argv[dispatch_argv.index("--observe-plan-binding") + 1]) == argv_binding._asdict()
    # Capacity refusals leave enrollment and plan claims untouched; failed assignment rolls enrollment back.
    import io
    from contextlib import redirect_stderr
    capacity_route = panel.selected_route("claude", "worker", "light", "read", "investigate")
    capacity_env = {"OMS_ROOM_ID": argv_binding.room_id,
                    "OMS_ROOM_PARTICIPANT": argv_binding.owner_participant,
                    "OMS_PANEL_MAIN_ATTEMPT": argv_binding.owner_attempt}
    capacity_args = (project, "claude", "worker", "read",
                     "Capacity fixture", None, None, argv_binding.task_id, False,
                     "Capacity call", capacity_route, None, None, None)
    for scenario in ("normal", "full", "full-write", "calls-full", "closed", "foreign-main",
                     "lost-caller", "lost-child", "send-error"):
        writing = scenario == "full-write"
        scopes = ["value.txt"] if writing else None
        this_args = capacity_args if not writing else (
            project, "claude", "worker", "write", None,
            brief, "git diff --check", argv_binding.task_id, False, "Capacity call",
            panel.selected_route("claude", "worker", "light", "write", "implement"), None, None, scopes)
        state = {"messages": [{}] * (room.MAX_MESSAGES - 1 if scenario in {"full", "full-write"} else 1),
                 "closed": scenario == "closed", "participants": [{
                     "participant": argv_binding.owner_participant, "role": "main",
                     "provider": "claude", "joined": True}]}
        if scenario == "calls-full":
            state["participants"] += [{"participant": "full-%d" % i, "parent": argv_binding.owner_participant,
                                       "joined": False} for i in range(room.MAX_CALLS)]
        participants_before = len(state["participants"])
        main_record = {"tool": "panel-main", "provider": "claude", "terminal": False,
                       "refs": {"panel_room_participant": argv_binding.owner_participant,
                                "panel_room_id": "foreign-room" if scenario == "foreign-main" else argv_binding.room_id}}
        notices, children, leaves = [], [], []
        def rollback(repo, room_id, event, text):
            leaves.append(event)
            for participant in state["participants"]:
                if participant["participant"] == event["participant"]:
                    participant["joined"] = False
        live_rows = [{"parent_attempt_id": argv_binding.owner_attempt, "terminal": False,
                      "refs": {"panel_role": "worker"}} for _ in range(5)] if scenario == "normal" else []
        # Live calls of this main or of others never block a dispatch: a main has no call cap.
        live_rows += [{"parent_attempt_id": "other-main", "terminal": False, "refs": {"panel_role": "worker"}},
                      {"parent_attempt_id": argv_binding.owner_attempt, "terminal": True,
                       "refs": {"panel_role": "worker"}}]
        def enroll(repo, room_id, member, provider, role, model, label, owns, parent):
            children.append((room_id, member, provider, role, model, owns, parent))
            state["participants"].append({"participant": member, "provider": provider,
                                           "role": role, "joined": True})
            if scenario in {"lost-caller", "lost-child"}:
                state["participants"].pop(0 if scenario == "lost-caller" else -1)
        def notice(repo, room_id, sender, recipient, text, kind, **kwargs):
            if scenario == "send-error" and text.startswith("Assigned:"):
                raise ValueError("unrelated room send failure")
            if len(state["messages"]) >= 1024:
                raise ValueError("room message limit reached; start a new bounded room")
            notices.append((sender, recipient, text))
        errors = io.StringIO()
        with patch.object(panel.shutil, "which", return_value="fixture-codex"), \
                patch.object(panel, "moved_main", return_value=None), \
                patch.object(panel, "child_environment", side_effect=lambda *args: dict(capacity_env)), \
                patch.object(panel, "native_repository", return_value=project), \
                patch.object(panel, "events", side_effect=lambda repo, operation, *args, **kwargs:
                             json.dumps(main_record if operation == "show" else live_rows)), \
                patch.object(panel, "plan_link", return_value=argv_binding) as observed_plan, \
                patch.object(room, "records", return_value=[]), \
                patch.object(room, "project", return_value=state), \
                patch.object(room, "join", side_effect=enroll), \
                patch.object(room, "send", side_effect=notice), \
                patch.object(room, "append", side_effect=rollback), \
                patch.object(panel, "results", return_value={"rows": []}), \
                patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")), \
                patch("panel_terminals.process", return_value=None), \
                patch.object(panel, "_dispatch_group_gone", return_value=True), \
                patch.object(panel.subprocess, "Popen", return_value=Mock(pid=123, wait=lambda: 0, poll=lambda: 0)) \
                    as provider_call, redirect_stderr(errors):
            if scenario != "normal":
                try:
                    panel.run_dispatch(*this_args)
                except ValueError as error:
                    expected = {"closed": "room is closed", "foreign-main": "identities disagree",
                                "full": "room %s is full" % argv_binding.room_id,
                                "full-write": "room %s is full" % argv_binding.room_id,
                                "calls-full": "room %s is full" % argv_binding.room_id,
                                "lost-caller": "participant has not joined this room",
                                "lost-child": "participant has not joined this room",
                                "send-error": "unrelated room send failure"}[scenario]
                    assert expected in str(error), (scenario, error)
                else:
                    raise AssertionError("invalid capacity dispatch was accepted: " + scenario)
                provider_call.assert_not_called()
                if scenario in {"full", "full-write", "calls-full", "closed"}:
                    assert len(state["participants"]) == participants_before and not children
                    observed_plan.assert_not_called()
                if scenario == "send-error":
                    assert leaves == [{"kind": "leave", "participant": children[0][1]}]
                    assert not next(p for p in state["participants"] if p["participant"] == children[0][1])["joined"]
                continue
            assert panel.run_dispatch(*this_args) == 0, scenario
            provider_call.assert_called_once()
            assert provider_call.call_args.kwargs["start_new_session"] == (os.name == "posix")
            invoked, child_env = provider_call.call_args.args[0], provider_call.call_args.kwargs["env"]
            assert json.loads(invoked[invoked.index("--observe-plan-binding") + 1]) == argv_binding._asdict()
            assert capacity_route["model"] in " ".join(invoked), invoked
            assert child_env["OMS_ROOM_ID"] == argv_binding.room_id
            assert child_env["OMS_PANEL_MAIN_ATTEMPT"] == argv_binding.owner_attempt
            assert child_env["OMS_ROOM_PARTICIPANT"] == children[0][1]
            assert children[0][0] == argv_binding.room_id and children[0][-1] == argv_binding.owner_participant
            assert children[0][2:6] == (capacity_route["provider"], "worker", capacity_route["model"], scopes)
            assert child_env["OMS_PANEL_PURPOSE"] == ("implement" if writing else "investigate")
            assert notices[0][2] == "Assigned: Capacity call" and not errors.getvalue()
    test_room = room.create(project, "observed-worker-room", "Worker lease producer")
    mains = {}
    for provider in panel.PROVIDERS:
        participant = "observed-main-" + provider
        attempt = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                        "--provider", provider, "--tool", "panel-main", "--ref", "panel_role=main",
                        "--ref", "panel_room_id=" + test_room, "--ref", "panel_room_participant=" + participant,
                        "--then", "starting", "--then", "working"]).strip()
        room.join(project, test_room, participant, provider, "main")
        mains[provider] = participant, attempt
    source = Path(call(["bash", str(panel.ENTRY), "scratch-worktree", "add", "--repo", str(project),
                        "--owner-pid", str(os.getpid())]).strip())
    (source / "value.txt").write_text("lease fixture scratch source\n")
    subprocess.run(["git", "-C", str(source), "add", "value.txt"], check=True)
    subprocess.run(["git", "-C", str(source), "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture scratch"], check=True)
    source_sha = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    primary_sha = subprocess.check_output(["git", "-C", str(project), "rev-parse", "HEAD"], text=True).strip()
    assert source_sha != primary_sha
    print("scratch source HEAD:", source_sha, "canonical HEAD:", primary_sha)
    scenarios = [("codex", "routine", "codex", "gpt-6-luna", "medium", "write", True),
                 ("claude", "light", "codex", "gpt-6-luna", "low", "write", True),
                 ("codex", "routine", "codex", "gpt-6-luna", "medium", "write", False),
                 ("claude", "light", "codex", "gpt-6-luna", "low", "read", True),
                 ("codex", None, "codex", "gpt-6-luna", "low", "read", True, "researcher")]
    for number, scenario in enumerate(scenarios):
        owner, workload, worker, model, effort, access, reviewed = scenario[:7]
        role = scenario[7] if len(scenario) > 7 else "worker"
        purpose = "research" if role == "researcher" else "implement" if access == "write" else "investigate"
        task_id = "observed-worker-" + str(number)
        main_member, main_attempt = mains[owner]
        assignment = dict(provider=worker, model=model, reasoning_effort=effort) if reviewed else {}
        call(plan_api + ["init", "--goal", "Observed worker fixture"])
        call(plan_api + ["add", "--id", task_id, "--title", "Exact observed worker", "--assignment", json.dumps(assignment)])
        entered, release = temporary / (task_id + "-entered"), temporary / (task_id + "-release")
        env = dict(environment, OMS_ROOM_ID=test_room, OMS_ROOM_REPO=str(project), OMS_ROOM_PARTICIPANT=main_member,
                   OMS_PANEL_MAIN_ATTEMPT=main_attempt, OMS_LOCK_DIR=str(temporary / "observed-locks"),
                   OMS_PLAN_LEASE_ID="ambient-not-authority", OMS_OBSERVER_INJECTED="untrusted", OMS_DL_OBSERVATION="untrusted", OMS_INDEX_BASE_REPO="untrusted", DELEGATE_SOURCE_REPO="untrusted",
                   LEASE_FIXTURE_ENTERED=str(entered), LEASE_FIXTURE_RELEASE=str(release),
                   LEASE_FIXTURE_MODEL=model, LEASE_FIXTURE_EFFORT=effort, LEASE_FIXTURE_PROVIDER=worker,
                   OMS_PEER_TIMEOUT="90")
        argv = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
                "--dispatch", role, "--owner", owner, "--access", access]
        argv += ["--workload", workload] if workload else ["--to", worker]
        if role == "researcher":
            argv += ["--prompt", "Inspect the native source read-only."]
        else:
            argv += ["--purpose", purpose, "--brief-file", str(brief)]
        if access == "write":
            argv += ["--verify", ":"]
        argv += ["--task-id", task_id]
        child_process = subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 30
            while not entered.exists() and child_process.poll() is None and time.monotonic() < deadline:
                time.sleep(.05)
            if not entered.exists():
                output, errors = child_process.communicate(timeout=10)
                raise AssertionError((argv, child_process.returncode, output, errors))
            observed = json.loads(entered.read_text())
            row = json.loads(call(plan_api + ["evidence-snapshot", "--id", task_id], env))
            marker = next(json.loads(p.read_text()) for p in (project / ".oms/delegations").glob("*.json")
                          if json.loads(p.read_text())["task_id"] == task_id)
            assert marker["schema"] == 4 and marker["lease_id"] == row["lease_id"], marker
            assert (marker["provider"], marker["model"], marker["reasoning_effort"]) == (worker, model, effort), marker
            assert marker["native_pid"] > 0 and marker["native_pid_source"] and marker["state"] == "running", marker
            os.kill(marker["native_pid"], 0)
            assert observed["head"] == source_sha and observed["value"] == "lease fixture scratch source\n", observed
            assert observed["cwd"] == marker["worktree"] and observed["cwd"] != str(source), observed
            if role == "researcher":
                assert observed["role"] == "researcher" and observed["purpose"] == "research", observed
                native_argv = json.loads(worker_log.read_text().splitlines()[-1])["argv"]
                assert "--sandbox" in native_argv and native_argv[native_argv.index("--sandbox") + 1] == "read-only", native_argv
                assert "--verify" not in native_argv and "--read-only" not in native_argv, native_argv
            else:
                assert observed["role"] == "worker" and observed["purpose"] == purpose, observed
            assert not (source / ".oms/delegations").exists(), "state split into scratch"
            assert row["claimed_by_participant"] == main_member and row["assignment"] == assignment, row
            assert row["provider"] == (worker if reviewed else owner), row
            observed_binding = dict(plan_id=row["plan_id"], task_id=task_id, lease_id=row["lease_id"],
                                    claim_provider=row["provider"], owner_provider=owner,
                                    owner_participant=main_member, owner_attempt=main_attempt,
                                    room_id=test_room)
            if reviewed and access == "write" and worker != owner:
                assert observed_binding["claim_provider"] != observed_binding["owner_provider"], observed_binding
            enrolled = room.participant(room.project(room.records(project, test_room)), observed["participant"])
            assert enrolled["parent"] == main_member and observed["parent"] == main_attempt, enrolled
            assert enrolled["role"] == role and enrolled["owns"] == [], enrolled
            route = panel.selected_route(owner, role, workload, access, purpose if role == "worker" else None,
                                         None if workload else worker)
            validation_route = dict(provider=route["provider"], model=route["model"],
                                    reasoning_effort=route["effort"], fallback_model="")
            binding = panel.PlanBinding(row["plan_id"], task_id, row["lease_id"], row["provider"], owner,
                                        main_member, main_attempt, test_room)
            assert binding._asdict() == observed_binding, binding
            child_env = dict(env, OMS_AGENT=owner, OMS_TASK_ID=task_id,
                             OMS_ROOM_PARTICIPANT=observed["participant"],
                             OMS_ROOM_ADMITTED_PARTICIPANT=observed["participant"])
            if role == "researcher":
                child_env["OMS_PANEL_ROLE"] = role
                binding.validate(project, child_env, validation_route)
                room_log = project / ".oms/threads" / (test_room + ".jsonl")
                room_before = room_log.read_bytes()
                for bad_state in ({"joined": False}, {"joined": True, "role": "worker", "parent": main_member,
                                   "provider": route["provider"], "model": route["model"]},
                                  {"joined": True, "role": "researcher", "parent": main_member,
                                   "provider": route["provider"], "model": "wrong-model"}):
                    with patch.object(panel.room, "participant", return_value=dict(enrolled, **bad_state)):
                        try:
                            binding.validate(project, child_env, validation_route)
                        except (ValueError, KeyError):
                            pass
                        else:
                            raise AssertionError("plan observer accepted invalid researcher membership: " + repr(bad_state))
                wrong_controller_role = dict(child_env, OMS_PANEL_ROLE="worker")
                try:
                    binding.validate(project, wrong_controller_role, validation_route)
                except ValueError:
                    pass
                else:
                    raise AssertionError("researcher observation accepted a worker controller role")
                assert room_log.read_bytes() == room_before, "observer validation changed room events"
            else:
                # Older worker observers have no controller role marker; the validator defaults to worker.
                binding.validate(project, child_env, validation_route)
            for lease_state in ("claimed", "running"):
                # Clock/state edits are adversarial private-fixture data, serialized by the production plan lock.
                mutate = ('import json,sys; from pathlib import Path; p=Path(sys.argv[1]); d=json.loads(p.read_text()); '
                          'd["tasks"][sys.argv[2]].update(state=sys.argv[3], claimed_at="2000-01-01T00:00:00Z",ttl="1"); '
                          'p.write_text(json.dumps(d))')
                call(["bash", "-c", '. "$1"; oms_with_file_lock "$2" python3 -c "$3" "$2" "$4" "$5"',
                      "fixture-expiry", str(panel.ROOT / "scripts/lib/file-lock.sh"),
                      str(project / ".oms/plan/tasks.json"), mutate, task_id, lease_state], env)
                plan_bytes = (project / ".oms/plan/tasks.json").read_bytes()
                shown = json.loads(call(plan_api + ["show", "--id", task_id], env))
                assert shown["claim_expired"] is False and shown["lease_id"] == marker["lease_id"], shown
                assert task_id not in call(plan_api + ["ready"], env).splitlines()
                next_claim = subprocess.run(plan_api + ["next", "--claim", "--provider", owner], env=env,
                                            capture_output=True, text=True, timeout=30)
                assert next_claim.returncode == 3 and "no actionable task" in next_claim.stderr, next_claim
                call(plan_api + ["reclaim", "--include-running", "--ttl", "1"], env)
                assert (project / ".oms/plan/tasks.json").read_bytes() == plan_bytes, "live exact lease changed"
            release.touch()
            output, errors = child_process.communicate(timeout=30)
            assert child_process.returncode == 0, (output, errors)
            delegate_rows = [r for r in json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo",
                             str(project), "list", "1000", "--json"]))["rows"]
                             if r["kind"] == "delegate" and r.get("task_id") == task_id]
            assert delegate_rows and all(r["exit"] == 0 for r in delegate_rows), delegate_rows
            for indexed in delegate_rows:
                # The index uses Git's abbreviation; compare it to the actual full source object.
                short = indexed["base_sha"]
                assert len(short) >= 7 and source_sha.startswith(short), indexed
                assert subprocess.check_output(["git", "-C", str(source), "rev-parse", short], text=True).strip() == source_sha
                print("delegate source base:", task_id, short, source_sha)
            assert not list((project / ".oms/delegations").glob("*.json"))
            call(plan_api + ["reclaim", "--include-running", "--ttl", "1"], env)
            assert json.loads(call(plan_api + ["show", "--id", task_id], env))["state"] == "ready"
        finally:
            release.touch()
            if child_process.poll() is None:
                child_process.terminate()
            child_process.communicate(timeout=15)
    # One cross-repository read regression: dirty state A and a different scratch source B.
    untracked_state = project / "state-only-untracked.txt"
    untracked_state.write_text("owner state stays here\n")
    source_value = (source / "value.txt").read_text()
    (source / "value.txt").write_text(source_value + "pending scratch review\n")
    with (project / ".git/info/exclude").open("a") as exclude:
        exclude.write("\n__pycache__/\n")
    state_status = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
    source_status = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"])
    source_diff = subprocess.check_output(["git", "-C", str(source), "diff", "HEAD"])
    read_prompt = temporary / "scratch-read-prompt"
    read_verify = temporary / "scratch-read-verify"
    main_member, main_attempt = mains["claude"]
    canonical_env = dict(environment, OMS_ROOM_ID=test_room, OMS_ROOM_REPO=str(project),
                         OMS_ROOM_PARTICIPANT=main_member, OMS_PANEL_MAIN_ATTEMPT=main_attempt,
                         OMS_LOCK_DIR=str(temporary / "observed-locks"))
    for role in ("advisor", "reviewer"):
        task_id = "scratch-" + role
        request = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
                   "--dispatch", role, "--owner", "claude",
                   "--prompt", "Inspect value.txt; judge only the supplied source evidence.", "--task-id", task_id]
        if role == "reviewer":
            request += ["--verify", "test -f value.txt"]
        preview = json.loads(call(request + ["--dry-run"], canonical_env))
        assert preview["argv"][preview["argv"].index("--repo") + 1] == str(source), preview
        if role == "reviewer":
            request[request.index("--verify") + 1] = ('mkdir -p __pycache__ verify-temp-dir; '
                'printf cache > __pycache__/x.pyc; printf output > verify-temp-dir/output; '
                'pwd -P > "$PANEL_TEST_VERIFY_LOG"; git rev-parse HEAD >> "$PANEL_TEST_VERIFY_LOG"')
        read_env = dict(canonical_env, PANEL_TEST_PROMPT=str(read_prompt),
                        PANEL_TEST_VERIFY_LOG=str(read_verify))
        answer = call(request, read_env)
        assert ("artifact:" if role == "advisor" else "artifacts:") in answer, answer
        observed = json.loads(worker_log.read_text().splitlines()[-1])
        assert observed["cwd"] == str(source) and observed["parent"] == main_attempt and observed["room"] == test_room, observed
        assert observed["room_repo"] == observed["state_repo"] == str(project), observed
        if role == "reviewer":
            assert read_verify.read_text().splitlines() == [str(source), source_sha], "verify did not execute in B"
            assert "+pending scratch review" in read_prompt.read_text(), "review diff came from A"
            assert (source / "__pycache__/x.pyc").read_text() == "cache"
            assert (source / "verify-temp-dir/output").read_text() == "output"
            subprocess.run(["git", "-C", str(source), "check-ignore", "-q", "__pycache__/x.pyc"], check=True)
            shutil.rmtree(source / "verify-temp-dir")
        enrolled = room.participant(room.project(room.records(project, test_room)), observed["participant"])
        assert enrolled["role"] == role and enrolled["parent"] == main_member, enrolled
        report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                                 "--task-id", task_id, "--json"], canonical_env))
        assert report["rows"] and any(c.get("answer") for r in report["rows"] for c in r.get("calls", [])), report
        indexed = [r for r in json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo", str(project),
                    "list", "1000", "--json"]))["rows"] if r.get("task_id") == task_id]
        assert indexed and any(r.get("artifact") and (project / r["artifact"]).is_file() for r in indexed), indexed
        assert all(source_sha.startswith(r["base_sha"]) for r in indexed), indexed
        if role == "reviewer":
            scope_path = re.search(r"^review-scope: (.+)$", answer, re.M).group(1)
            scope = json.loads(Path(scope_path).read_text())
            assert scope["head"] == source_sha and scope["base"] == source_sha and scope["paths"] == ["value.txt"], scope
            import hashlib
            assert scope["diff_sha256"] == hashlib.sha256(source_diff).hexdigest(), scope
        attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
        attempt = next(a for a in attempts if a.get("task_id") == task_id)
        assert attempt["parent_attempt_id"] == main_attempt and attempt["refs"]["panel_location"].startswith("worktree:"), attempt
        assert not (source / ".oms").exists(), "read records split into B"
        assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == state_status
        assert subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"]) == source_status
        assert (source / "value.txt").read_text() == source_value + "pending scratch review\n"
        assert untracked_state.read_text() == "owner state stays here\n"
        assert (project / "value.txt").read_text() == "old\n", "read pass edited A source"
        mutations = [(project / "value.txt", False), (project / "value.txt", True),
                     (source / "value.txt", False)]
        if role == "reviewer":
            mutations.append((source / "__pycache__/x.pyc", False))
        for target, strict in mutations:
            original = target.read_bytes()
            result = subprocess.run(request, env=dict(read_env, PANEL_TEST_MODE="primary-write",
                PANEL_TEST_PRIMARY=str(target), OMS_WORKER_GUARD_STRICT="1" if strict else "0"),
                capture_output=True, text=True, timeout=90)
            if target.parent == project and not strict:
                assert result.returncode == 0 and "warning: read pass observed state repository changes: tracked" in result.stderr, result
            else:
                assert result.returncode != 0 and "protected" in result.stderr, result
            assert target.read_bytes() != original, "read guard mutation fixture did not execute"
            target.write_bytes(original)
            shutil.rmtree(source / "verify-temp-dir", ignore_errors=True)
        if role == "reviewer":
            input_untracked = source / "review-input-untracked.txt"
            input_untracked.write_text("not verifier output\n")
            worker_before = worker_log.read_bytes()
            result = subprocess.run(request, env=read_env, capture_output=True, text=True, timeout=90)
            assert result.returncode != 0 and "cannot review untracked file content" in result.stderr, result
            assert worker_log.read_bytes() == worker_before, "untracked input reached a reviewer"
            input_untracked.unlink()
    shutil.rmtree(source / "__pycache__")
    # Cross-repository council pointers bind A; isolated seats still receive private copies.
    isolated_context = temporary / "scratch-read-isolated-context"
    isolated_context.mkdir()
    context = call(["bash", "-c", '''set -eu
    . "$1/scripts/lib/peer-common.sh"
    REPO="$2"; MA_READ_SOURCE_REPO="$3"; prompt_file="$4"
    ma_prepare_council_context
    ma_stage_council_context "$5"
    ma_export_child_env codex test "$REPO" "" read
    printf '%s\\n' "$MA_COUNCIL_CONTEXT_REL" "$OMS_STATE_REPO"
    ma_cleanup_council_context
    ''', "context", str(root), str(project), str(source), str(read_prompt), str(isolated_context)],
        dict(canonical_env, OMS_PANEL_DISPATCH="1"))
    pointer, state_root = context.strip().splitlines()
    assert pointer.startswith("$OMS_STATE_REPO/.oms/delegations/") and state_root == str(project), context
    relative_context = pointer.removeprefix("$OMS_STATE_REPO/")
    assert (isolated_context / relative_context / "request.md").is_file()
    assert (isolated_context / ".oms/artifacts" / Path(relative_context).name / "request.md").is_file()
    assert not (source / ".oms").exists()
    untracked_state.unlink()
    (source / "value.txt").write_text(source_value)
    # Snapshot failure is a frontend refusal, even before an observer binding can be selected.
    task_id = "frontend-owned"
    call(plan_api + ["init", "--goal", "Frontend snapshot refusal"])
    call(plan_api + ["add", "--id", task_id, "--title", "Owned snapshot"])
    call(plan_api + ["claim", "--id", task_id, "--provider", "claude"], canonical_env)
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
               "--dispatch", "worker", "--owner", "claude", "--workload", "light", "--purpose", "investigate",
               "--prompt", "Inspect source without edits.", "--task-id", task_id]
    plan_path = project / ".oms/plan/tasks.json"
    original = plan_path.read_bytes()
    def frontend_refused(arguments, denied_env=canonical_env):
        started_before = worker_log.read_bytes()
        refused = subprocess.run(arguments, env=denied_env, capture_output=True, text=True, timeout=30)
        assert refused.returncode != 0 and refused.stderr.startswith("error:") and "plan protection is absent" not in refused.stderr, refused
        assert worker_log.read_bytes() == started_before, ("provider started after failed snapshot", refused)
    for hostile in (b"{", b"", b"[]", b'{"tasks":{},"tasks":{}}', b'{"tasks":[]}', b" " * (4 * 1024 * 1024 + 1)):
        plan_path.write_bytes(hostile)
        frontend_refused(request)
    plan_path.write_bytes(original)
    for linked in ("symbolic", "hard", "directory"):
        saved = plan_path.with_name("frontend-saved.json")
        plan_path.rename(saved)
        if linked == "symbolic":
            plan_path.symlink_to(saved)
        elif linked == "hard":
            os.link(saved, plan_path)
        else:
            plan_path.mkdir()
        try:
            frontend_refused(request)
        finally:
            plan_path.rmdir() if linked == "directory" else plan_path.unlink()
            saved.rename(plan_path)
    plan_path.chmod(0)
    try:
        frontend_refused(request)
    finally:
        plan_path.chmod(0o600)
    # Inject failures only at the frontend's authoritative snapshot boundary; the provider remains real/fake.
    harness = """import sys, subprocess
sys.path.insert(0, sys.argv[1])
import terminal_panel as p
real, mode = p.subprocess.run, sys.argv[2]
def query(args, **kwargs):
    if 'agent-plan' in args and 'evidence-snapshot' in args:
        if mode == 'timeout':
            raise subprocess.TimeoutExpired(args, 1)
        if mode == 'io':
            raise OSError('snapshot unreadable')
        if mode == 'missing':
            return subprocess.CompletedProcess(args, 2, '',
                'error: no such task: ' + args[args.index('--id') + 1] + '\\n'
                'hint: run `oms agent-plan --help` for usage\\n')
        return subprocess.CompletedProcess(args, 2 if mode == 'error' else 0,
            '' if mode == 'error' else ('{}' if mode == 'json' else '{'),
            'error: current-owner snapshot failed' if mode == 'error' else '')
    return real(args, **kwargs)
p.subprocess.run = query
sys.exit(p.main(sys.argv[3:]))
"""
    for mode in ("error", "missing", "json", "malformed", "timeout", "io"):
        frontend_refused(["python3", "-c", harness, str(panel.ROOT / "scripts/lib"), mode, *request[3:]])
    # Proven task absence and no-plan compatibility still execute, explicitly without protection.
    for no_plan in (False, True):
        saved = plan_path.with_name("frontend-no-plan.json")
        if no_plan:
            plan_path.rename(saved)
        try:
            absent = subprocess.run(request[:-1] + ["frontend-absent"], env=canonical_env,
                                    capture_output=True, text=True, timeout=30)
            assert absent.returncode == 0 and "plan protection is absent" in absent.stderr, absent
        finally:
            if no_plan:
                saved.rename(plan_path)
    assert plan_path.read_bytes() == original, "frontend compatibility changed the owned row"
    call(["bash", str(panel.ENTRY), "scratch-worktree", "remove", "--repo", str(project), str(source)])
    print("observed worker leases: real markers, source-index lineage, scratch non-worker results, frontend refusal and recovery passed")

exercise_observed_worker_leases()

import shutil
shutil.rmtree(project / ".oms/plan")  # later board fixtures expect a project with no plan
# A continued worker resumes its native session in a new run and carries a delta preamble; without a stored
# session it falls back to a fresh worker, and another main never reaches it.
session = "0199aaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
stored = home / ".codex/sessions/2026/10/07"
stored.mkdir(parents=True)
(stored / ("rollout-2026-10-07T00-00-00-" + session + ".jsonl")).write_text("{}\n")
worker = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch", "worker",
          "--owner", "claude", "--purpose", "investigate", "--to", "codex", "--model", "gpt-6-luna"]
call(worker + ["--prompt", "Inspect value.txt without edits.", "--task-id", "room-continue", "--label", "Inspect"],
     dict(room_env, PANEL_TEST_SESSION=session))
listed = lambda: json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
first = next(a for a in listed() if a["task_id"] == "room-continue" and a["refs"].get("panel_role") == "worker")
assert first["refs"]["native_session"] == session, first
brief.write_text("Task: look at value.txt again.\n")
prompt_log = temporary / "continue-prompt.md"
again = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue", "--label", "Look again"],
                       env=dict(room_env, PANEL_TEST_SESSION=session, PANEL_TEST_PROMPT=str(prompt_log)),
                       capture_output=True, text=True)
assert again.returncode == 0 and "artifact:" in again.stdout and "native session" in again.stderr, again
resumed = [json.loads(line) for line in worker_log.read_text().splitlines()][-1]
assert resumed["task_id"] == "room-continue", resumed
assert len(re.findall(r"\btask_id=room-continue\b", again.stdout)) == 1, again.stdout
assert resumed["argv"][-4:] == ["resume", "--all", session, "-"], (resumed, again)
text = prompt_log.read_text()
assert "Your earlier work was on" in text and "New instructions follow." in text and "Task: look at value.txt again." in text, text
rounds = [a for a in listed() if a["task_id"] == "room-continue" and a["refs"].get("panel_role") == "worker"]
assert len(rounds) == 2 and rounds[-1]["refs"]["native_session"] == session, rounds
# One session moves between write-capable roles (worker -> refactorer); a reviewer never resumes it.
refactor = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch",
    "refactorer", "--owner", "claude", "--to", "codex", "--model", "gpt-6-luna", "--access", "write",
    "--brief-file", str(brief), "--verify", "true", "--continue", "room-continue", "--dry-run"],
    env=room_env, capture_output=True, text=True)
assert refactor.returncode == 0 and "native session" in refactor.stderr, refactor
planned = json.loads(refactor.stdout)["argv"]
assert planned[planned.index("--role") + 1] == "refactorer" and planned[planned.index("--resume-session") + 1] == session, planned
fresh = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-inspection"], env=dict(
    room_env, PANEL_TEST_PROMPT=str(prompt_log)), capture_output=True, text=True)
assert fresh.returncode == 0 and "no resumable session" in fresh.stderr, fresh
assert "A fresh worker continues earlier work" in prompt_log.read_text(), prompt_log.read_text()
foreign = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"],
                         env=dict(room_env, OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"]),
                         capture_output=True, text=True)
assert foreign.returncode != 0 and "no worker" in foreign.stderr, foreign
third = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"], env=room_env,
                       capture_output=True, text=True)
assert third.returncode == 0, third
escalated = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"], env=room_env,
                          capture_output=True, text=True)
assert escalated.returncode != 0 and "task room-continue reached 2 rework rounds; escalate:" in escalated.stderr
# Continuation survives retained results that omit the task and never inlines patch bytes it cannot verify.
import hashlib
last_round = [a for a in listed() if a["task_id"] == "room-inspection" and a["refs"].get("panel_role") == "worker"][-1]
patch_dir = project / ".oms/artifacts"
patch_dir.mkdir(parents=True, exist_ok=True)
(patch_dir / "cont.patch").write_text("diff --git a/value.txt b/value.txt\n+tampered-marker\n")
(patch_dir / "link.patch").symlink_to(project / "value.txt")
good = hashlib.sha256((patch_dir / "cont.patch").read_bytes()).hexdigest()
real_results = panel.results
def continued(rows):
    panel.results = lambda repo, task: {"rows": rows, "truncated": True}
    try:
        return panel.continuation(project, "claude", None, "room-inspection", {"provider": "codex"}, brief, room_env)
    finally:
        panel.results = real_results
def prompt_of(rows):
    out = continued(rows)
    return out[0].read_text()
assert "A fresh worker continues" in prompt_of([]), "empty results must still fall back to a fresh worker"
def with_patch(path, digest):
    return [{"calls": [{"attempt_id": last_round["attempt_id"], "patch": path, "patch_sha256": digest,
                        "exit": 0, "answer": "SAFE-CONTINUATION-SUMMARY",
                        "changes": {"issue": "patch evidence unavailable"}}]}]
real_verified_patch = panel.verified_patch
verified_calls = []
def tracked_verified_patch(repo, call):
    verified_calls.append(call)
    return real_verified_patch(repo, call)
panel.verified_patch = tracked_verified_patch
try:
    successful_text = prompt_of(with_patch(".oms/artifacts/cont.patch", good))
    assert "tampered-marker" in successful_text
    assert "SAFE-CONTINUATION-SUMMARY" in successful_text
    assert len(verified_calls) == 1, verified_calls
    verified_calls[:] = []
    rejected = [
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": 1},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": None},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": "0"},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": True},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"attempt_id": "other-attempt"},
    ]
    missing_exit = with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0]
    del missing_exit["exit"]
    rejected.append(missing_exit)
    for previous_call in rejected:
        text = prompt_of([{"calls": [previous_call]}])
        assert "tampered-marker" not in text and "cont.patch" not in text, text
        assert "SAFE-CONTINUATION-SUMMARY" not in text, text
        assert verified_calls == [], verified_calls
    older = with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0]
    latest = dict(older, attempt_id=last_round["attempt_id"], exit=1)
    text = prompt_of([{"calls": [older, latest]}])
    assert "tampered-marker" not in text and "cont.patch" not in text, text
    assert "SAFE-CONTINUATION-SUMMARY" not in text, text
    assert verified_calls == [], verified_calls
finally:
    panel.verified_patch = real_verified_patch
for path, digest in ((".oms/artifacts/cont.patch", "0" * 64), (".oms/artifacts/link.patch", good)):
    text = prompt_of(with_patch(path, digest))
    assert "tampered-marker" not in text, text
    assert "could not be verified" in text, text
# A status message is a declaration: no mail, no unread, latest per sender kept past the 12-message window;
# unread counts ignore participants who left.
declared = room.create(project, title="Declared status")
room.join(project, declared, "decl-main", "claude", model="claude-opus-5-5", owns=["value.txt"])
room.join(project, declared, "decl-peer", "codex", role="worker", parent="decl-main")
room.join(project, declared, "decl-other", "codex", role="worker", parent="decl-main")
room.send(project, declared, "decl-main", "all", "Parsing the config", kind="status")
for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
    with patch.dict(os.environ, dict(guard, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-peer")):
        for message_id in ("result-decl-peer", "result-decl-other"):
            before_child_result = len(room.records(project, declared))
            try:
                room.send(project, declared, "decl-peer", "decl-main", "Call exit=0;", kind="handoff",
                          message_id=message_id)
            except ValueError as error:
                assert "result handoffs are written by the panel, not by a worker" in str(error), error
            else:
                raise AssertionError("a worker wrote a panel result handoff")
            assert len(room.records(project, declared)) == before_child_result
        child_note = room.send(project, declared, "decl-peer", "decl-main", "Scoped worker observation")
    room.acknowledge(project, declared, "decl-main", [child_note["id"]])
room.join(project, declared, "decl-unparented", "codex", role="worker")
with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": declared,
                             "OMS_ROOM_PARTICIPANT": "decl-unparented"}):
    try:
        room.send(project, declared, "decl-unparented", "decl-main", "Missing parent")
    except ValueError as error:
        assert "a worker can only message its parent <missing>" in str(error), error
    else:
        raise AssertionError("a worker without an admitted parent sent a message")
room.append(project, declared, {"kind": "leave", "participant": "decl-unparented"}, "Left")
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
room.send(project, declared, "decl-peer", "decl-main", "unread before title change", message_id="before-title")
# Updating a live work title does not rejoin, consume mail or change native identity.
room.bind(project, declared, "decl-main", "declared-title-native")
before_description = room.status(project, declared)
main_before = deepcopy(room.participant(before_description, "decl-main"))
assert "label_override" not in main_before  # New code still reads logs written before describe existed.
mail_before = room.records(project, declared)
cursor_before = room.updates(project, declared, "decl-main", budget=65536, limit=200)["cursor"]
unbound_room_env = dict(environment, OMS_ROOM_ID="", OMS_ROOM_PARTICIPANT="")
with patch.dict(os.environ, unbound_room_env, clear=True):
    room.describe(project, declared, "decl-main", "Improve result navigation")
    after_description = room.status(project, declared)
    main_after = room.participant(after_description, "decl-main")
    assert {key: value for key, value in main_after.items() if key not in {"label", "label_override"}} == {
        key: value for key, value in main_before.items() if key != "label"}
    assert main_after["label"] == main_after["label_override"] == "Improve result navigation"
    assert main_after["seq"] == main_before["seq"] and main_after["joined_at"] == main_before["joined_at"]
    assert main_after["initial_seq"] == main_before["initial_seq"]
    assert after_description["pending_count"] == before_description["pending_count"] == 1
    assert after_description["statuses"] == before_description["statuses"]
    assert after_description["messages"] == before_description["messages"]
    assert after_description["statuses"]["decl-main"]["text"] == "Parsing the config"
    assert [room.visible(r, "decl-main", main_before["seq"]) for r in mail_before] == [
        room.visible(r, "decl-main", main_after["seq"]) for r in mail_before]
    delta_after_description = room.updates(project, declared, "decl-main", cursor_before,
                                           budget=65536, limit=200)
    assert not delta_after_description["turns"] and delta_after_description["cursor"] != cursor_before
    written_description = room.records(project, declared)[-1]["room_event"]
    assert written_description == {"kind": "describe", "participant": "decl-main",
                                   "label": "Improve result navigation"}
    repeated_rows = len(room.records(project, declared))
    room.describe(project, declared, "decl-main", "Improve result navigation")
    assert len(room.records(project, declared)) == repeated_rows
    title_args = ["bash", str(panel.ENTRY), "room", "describe", "--repo", str(project), "--id", declared,
                  "--participant", "decl-main", "--label", "Review result navigation"]
    bound_main_env = dict(environment, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-main")
    call(title_args, bound_main_env)
    assert room.participant(room.status(project, declared), "decl-main")["label"] == "Review result navigation"
    cli_rows = len(room.records(project, declared))
    call(title_args, bound_main_env)
    assert len(room.records(project, declared)) == cli_rows
    for extra in (["--owns", "scripts/lib"], ["--native-session", "native-id"],
                  ["--model", "gpt-6-astra"], ["--parent", "other-main"]):
        rejected = subprocess.run(title_args + extra, env=unbound_room_env, capture_output=True, text=True)
        assert rejected.returncode != 0, (extra, rejected.stdout, rejected.stderr)
    for wrong_binding in (dict(environment, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-peer"),
                          dict(environment, OMS_ROOM_ID="another-room", OMS_ROOM_PARTICIPANT="decl-main"),
                          dict(environment, OMS_ROOM_ID=declared),
                          dict(environment, OMS_ROOM_PARTICIPANT="decl-main")):
        rejected = subprocess.run(title_args, env=wrong_binding, capture_output=True, text=True)
        assert rejected.returncode != 0, (wrong_binding, rejected.stdout, rejected.stderr)
    rows_for_description = room.records(project, declared)
    description = {"kind": "describe", "participant": "decl-main", "label": "New title"}
    sensitive_label = "pass" + "word=fixture-value"
    for altered in (dict(description, label=""), dict(description, label="x" * 121),
                    dict(description, label="bad\nlabel"), dict(description, label=None),
                    dict(description, consumer="0" * 32), dict(description, owns=[]),
                    dict(description, model="gpt-6-astra"), dict(description, parent="other-main"),
                    dict(description, initial_seq="1"), dict(description, label=sensitive_label),
                    dict(description, participant="decl-peer"), dict(description, participant="missing")):
        try:
            room.validate_event(rows_for_description, altered, "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("invalid description accepted: %r" % altered)
    for malformed in (dict(description, label=None), dict(description, owns=[]),
                       dict(description, label="x" * 121),
                       {"kind": "describe", "participant": "decl-main"}):
        try:
            room.project(rows_for_description + [{"seq": rows_for_description[-1]["seq"] + 1, "room_event": malformed}])
        except ValueError:
            pass
        else:
            raise AssertionError("malformed persisted description accepted")
    left_rows = deepcopy(rows_for_description)
    left_rows.append({"seq": left_rows[-1]["seq"] + 1, "role": "note",
                      "room_event": {"kind": "leave", "participant": "decl-main"}})
    closed_rows = deepcopy(rows_for_description)
    closed_rows.append({"seq": closed_rows[-1]["seq"] + 1, "role": "closed", "text": "closed"})
    for invalid_rows in (left_rows, closed_rows):
        try:
            room.validate_event(invalid_rows, description, "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("describe accepted a left main or closed room")
    for actor in ({"OMS_HARNESS_CHILD": "1", "OMS_HARNESS_DELEGATE_DEPTH": "0",
                   "OMS_ROOM_PARTICIPANT": "decl-peer", "OMS_ROOM_ID": declared},
                  {"OMS_HARNESS_DELEGATE_DEPTH": "1", "OMS_ROOM_PARTICIPANT": "decl-peer",
                   "OMS_ROOM_ID": declared},
                  {"OMS_ROOM_PARTICIPANT": "decl-peer", "OMS_ROOM_ID": declared},
                  {"OMS_ROOM_PARTICIPANT": "decl-main", "OMS_ROOM_ID": "another-room"},
                  {"OMS_ROOM_PARTICIPANT": "decl-main"}, {"OMS_ROOM_ID": declared}):
        with patch.dict(os.environ, actor):
            try:
                room.validate_event(rows_for_description, description, "Work description updated")
            except ValueError:
                pass
            else:
                raise AssertionError("description widened actor authority")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_PARTICIPANT": "decl-peer",
                                 "OMS_ROOM_ID": declared}):
        try:
            room.validate_event(rows_for_description, dict(description, participant="decl-peer"),
                                "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("worker described itself")
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": "decl-main", "OMS_ROOM_ID": declared}):
        assert room.validate_event(rows_for_description, description, "Work description updated") is True
    before_bad_append = len(room.records(project, declared))
    try:
        room.append(project, declared, dict(description, consumer="0" * 32), "Work description updated")
    except ValueError:
        pass
    else:
        raise AssertionError("locked append accepted identity injection")
    assert len(room.records(project, declared)) == before_bad_append
    closed_room = room.create(project, "describe-closed", "Closed title fixture")
    room.join(project, closed_room, "closed-main", "codex")
    room.command(project, "close", "--id", closed_room)
    try:
        room.describe(project, closed_room, "closed-main", "Closed update")
    except ValueError as error:
        assert "closed" in str(error)
    else:
        raise AssertionError("describe appended to a closed room")
    title_state = {"room": after_description, "attempts": {"recent": [{"attempt_id": "decl-attempt", "state": "working",
        "panel": {"role": "main", "room_id": declared, "room_participant": "decl-main",
                  "label": "Stale launch label", "model": "claude-opus-5-5"}}]}}
    from room_view import nodes
    shown_main = next(m for m in nodes(title_state) if m["participant"] == "decl-main")
    assert shown_main["title"] == "Improve result navigation" and shown_main["consumer"] == main_before["consumer"]
    assert shown_main["model"] == shown_main["model_at_launch"] == "claude-opus-5-5"
    legacy_title = deepcopy(title_state)
    legacy_member = room.participant(legacy_title["room"], "decl-main")
    legacy_member.pop("label_override")
    legacy_member["owns"] = ["scope-only-update"]
    assert next(m for m in nodes(legacy_title) if m["participant"] == "decl-main")["title"] == "Stale launch label"
    for title_view in ("tree", "graph"):
        titled = render(title_state, "claude", 120, 40, view=title_view, navigation={"dismissed": True})
        assert "Improve result navigation" in titled and "Now: Parsing the config" in titled and "Opus" in titled, titled
        long_title = deepcopy(title_state)
        room.participant(long_title["room"], "decl-main")["label_override"] = "Now: " + "work title " * 10
        for columns, rows in ((120, 40), (76, 40), (28, 16)):
            bounded = render(long_title, "claude", columns, rows, view=title_view, navigation={"dismissed": True})
            assert all(display_width(line) <= columns for line in bounded.splitlines()) and len(bounded.splitlines()) <= rows
            if columns >= 76:
                assert "Now: Parsing the config" in bounded, bounded
# Finish the title fixture's extra unread mail before the original leave/count checks.
room.acknowledge(project, declared, "decl-main", ["before-title"])
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
control_launch = {}
real_popen = panel.subprocess.Popen
def observe_control_launch(argv, *args, **kwargs):
    if argv[:3] == [sys.executable, "-c", "pass"]:
        control_launch.update(env=kwargs["env"])
    return real_popen(argv, *args, **kwargs)
with patch.dict(os.environ, control_env, clear=True), \
        patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
        patch.object(panel.subprocess, "Popen", side_effect=observe_control_launch):
    assert panel.dispatch(project, "claude", "worker", "light", "read", "investigate",
                          prompt="Inspect the bounded source", task_id="control-owner", label="Control dispatch") == 0
    newest = room.status(project, shared)["participants"][-1]
    assert newest["parent"] == room_owner
    for owner in ("codex",):
        try:
            panel.dispatch(project, owner, "worker", "light", "read", "investigate", prompt="Inspect source")
        except ValueError as error:
            assert "unique active main" in str(error)
        else:
            raise AssertionError("control dispatch chose an exited owner")
    second_owner = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                                "--ref", "panel_room_id=" + shared, "--then", "starting", "--then", "working", output=True)
    room.join(project, shared, second_owner, "claude")
    try:
        panel.dispatch(project, "claude", "worker", "light", "read", "investigate", prompt="Inspect source")
    except ValueError as error:
        assert "unique active main" in str(error)
    else:
        raise AssertionError("control dispatch silently chose between two mains")
    # An explicit main resolves the ambiguity but cannot name a non-live caller.
    assert panel.dispatch(project, "claude", "advisor", None, "read", None,
                          prompt="Advise the second main", main=second_owner) == 0
    assert room.status(project, shared)["participants"][-1]["parent"] == second_owner
    try:
        panel.dispatch(project, "claude", "advisor", None, "read", None,
                       prompt="Advise nobody", main="outside-main")
    except ValueError as error:
        assert "unique active main" in str(error)
    else:
        raise AssertionError("explicit main bypassed live-main proof")
    # An inherited caller cannot be redirected to another main by --main.
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None,
                           prompt="Misattributed advice", main=second_owner)
        except ValueError as error:
            assert "different main" in str(error)
        else:
            raise AssertionError("--main silently kept the inherited caller")
    # A main moved to Claude Code's background service keeps only its session id;
    # its room binding names the caller, with or without a selected room.
    moved_sid = "3f0c2a4e-9b1d-4c6e-8a7f-1d2e3c4b5a69"
    room.bind(project, shared, second_owner, moved_sid)
    assert not room.discover(project)[1]
    for selected in (shared, None):
        with patch.dict(os.environ, {"CLAUDE_CODE_SESSION_ID": moved_sid}):
            if not selected:
                os.environ.pop("OMS_ROOM_ID")
            assert panel.dispatch(project, "claude", "advisor", None, "read", None,
                                  prompt="Advise the moved main") == 0
        assert room.status(project, shared)["participants"][-1]["parent"] == second_owner
        assert control_launch["env"]["OMS_PANEL_MAIN_ATTEMPT"] == second_owner
    for refused in ({"CLAUDE_CODE_SESSION_ID": "0d9e8f7a-6b5c-4d3e-2f1a-0b9c8d7e6f5a"},
                    {"CLAUDE_CODE_SESSION_ID": moved_sid, "OMS_HARNESS_CHILD": "1"}):
        with patch.dict(os.environ, refused):
            try:
                panel.dispatch(project, "claude", "advisor", None, "read", None, prompt="Advise nobody")
            except ValueError as error:
                assert "unique active main" in str(error)
            else:
                raise AssertionError("dispatch resolved an unbound or worker session to a main")
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
observed = next(row for row in map(json.loads, reversed(worker_log.read_text().splitlines()))
                if row["participant"] == child["participant"])
assert observed["room"] == shared, observed
# Declared scopes: only the participant replaces or clears its own; overlap is a path-component prefix.
assert room.overlaps("scripts/lib", "scripts/lib/room.py") and room.overlaps("scripts/lib/room.py", "scripts/lib")
assert room.overlaps("docs", "docs") and room.overlaps(".", "docs") and not room.overlaps("scripts/li", "scripts/lib")
other_main = main_attempts["codex"]
room.join(project, shared, "scope-advisor", "codex", role="advisor", parent=room_owner)
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, ["docs", "scripts/lib"])
    assert room.participant(room.status(project, shared), room_owner)["owns"] == ["docs", "scripts/lib"]
    room.scope(project, shared, room_owner, [])
    assert room.participant(room.status(project, shared), room_owner)["owns"] == []
for who, scopes, reason in ((room_owner, ["s%d" % n for n in range(17)], "sixteen"), (room_owner, ["./docs"], "normalized"),
                            ("scope-advisor", ["docs"], "advisors and reviewers"), (other_main, ["docs"], "its own scope")):
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner if reason == "its own scope" else who}):
        try:
            room.scope(project, shared, who, scopes)
        except ValueError as error:
            assert reason in str(error), (reason, error)
        else:
            raise AssertionError("scope was not refused: " + reason)
scope_cli = subprocess.run(["bash", str(panel.ENTRY), "room", "scope", "--repo", str(project), "--id", shared,
                            "--participant", other_main, "--owns", "scripts/lib/room.py"],
                           env=dict(environment, OMS_ROOM_PARTICIPANT=other_main), capture_output=True, text=True, timeout=30)
assert scope_cli.returncode == 0 and json.loads(scope_cli.stdout)["owns"] == ["scripts/lib/room.py"], scope_cli
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, ["scripts/lib"])
room.join(project, shared, "scope-live", "codex", role="worker", parent=room_owner, owns=["scripts"])
room.join(project, shared, "scope-done", "codex", role="worker", parent=room_owner, owns=["scripts/lib/panel_view.py"])
room.send(project, shared, "scope-done", room_owner, "Call exit=0; parent acceptance pending.", kind="handoff",
          message_id="result-scope-done")
panel.events(project, "start", "--provider", "codex", "--tool", "peer-delegate",
             "--ref", "panel_role=worker", "--ref", "panel_room_id=" + shared,
             "--ref", "panel_room_participant=scope-done", "--then", "starting", "--then", "working",
             "--then", "verifying", "--then", "review", "--then", "done")
scope_before_preview = room.records(project, shared)
scoped_dispatch = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                  "--dispatch", "worker", "--owner", "claude", "--access", "write", "--purpose", "implement",
                                  "--brief-file", str(brief), "--verify", "test -f value.txt", "--scope", "./scripts/lib/",
                                  "--scope", "docs", "--dry-run"], env=room_env, capture_output=True, text=True, timeout=60)
assert scoped_dispatch.returncode == 0, scoped_dispatch
scoped_plan = json.loads(scoped_dispatch.stdout)
assert sorted((o["participant"], o["scope"]) for o in scoped_plan["overlaps"]) == sorted(
    [(other_main, "scripts/lib/room.py"), ("scope-live", "scripts")]), scoped_plan
assert scoped_dispatch.stderr.count("Scope overlap") == 1 and other_main in scoped_dispatch.stderr, scoped_dispatch.stderr
assert room.records(project, shared) == scope_before_preview, "scope preview changed the room"
unscoped = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch", "worker",
                            "--owner", "claude", "--access", "write", "--purpose", "implement", "--brief-file", str(brief),
                            "--verify", "test -f value.txt", "--dry-run"], room_env))
assert "overlaps" not in unscoped, unscoped
fake_bin = temporary / "dispatch-bin"
fake_bin.mkdir()
provider_marker = temporary / "provider-invoked"
fake_provider = fake_bin / "claude"
fake_provider.write_text("#!/bin/sh\nprintf called > \"$OMS_FAKE_PROVIDER_MARKER\"\n")
fake_provider.chmod(0o700)
dry_env = dict(room_env, PATH=str(fake_bin) + os.pathsep + os.environ.get("PATH", ""),
               OMS_FAKE_PROVIDER_MARKER=str(provider_marker))
clean_prompt = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                               "--dispatch", "worker", "--owner", "claude", "--access", "read",
                               "--purpose", "investigate", "--prompt", "Inspect value.txt.", "--dry-run"],
                              env=dry_env, capture_output=True, text=True, timeout=60)
assert clean_prompt.returncode == 0 and json.loads(clean_prompt.stdout)["executes"] is False, clean_prompt
room_before_scan = room.status(project, shared)["participants"]
sensitive_prompt = "Inspect source.\n" + sensitive_summary
blocked_prompt = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                 "--dispatch", "worker", "--owner", "claude", "--access", "read",
                                 "--purpose", "investigate", "--prompt", sensitive_prompt, "--dry-run"],
                                env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_prompt.returncode != 0 and not blocked_prompt.stdout, blocked_prompt
assert "secret-tier match in your prompt at line 2" in blocked_prompt.stderr, blocked_prompt.stderr
assert "fixture-value" not in blocked_prompt.stderr and room.status(project, shared)["participants"] == room_before_scan
sensitive_brief = temporary / "sensitive-brief.md"
sensitive_brief.write_text("Inspect source.\npass" + "word=fixture-value")
blocked_brief = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                "--dispatch", "worker", "--owner", "claude", "--access", "write",
                                "--purpose", "implement", "--brief-file", str(sensitive_brief),
                                "--verify", "test -f value.txt", "--dry-run"],
                               env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_brief.returncode != 0 and not blocked_brief.stdout, blocked_brief
assert "secret-tier match in your prompt at line 2" in blocked_brief.stderr, blocked_brief.stderr
assert "fixture-value" not in blocked_brief.stderr and room.status(project, shared)["participants"] == room_before_scan
blocked_scoped = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                 "--dispatch", "worker", "--owner", "claude", "--access", "write",
                                 "--purpose", "implement", "--brief-file", str(sensitive_brief),
                                 "--verify", "test -f value.txt", "--scope", "docs", "--dry-run"],
                                env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_scoped.returncode != 0 and not blocked_scoped.stdout, blocked_scoped
assert "secret-tier match in your prompt at line 2" in blocked_scoped.stderr, blocked_scoped.stderr
assert "fixture-value" not in blocked_scoped.stderr and room.status(project, shared)["participants"] == room_before_scan
for name, prepare in (
        ("symlink", lambda path: path.symlink_to(sensitive_brief)),
        ("missing", lambda path: None),
        ("unreadable", lambda path: (path.write_text("bounded"), path.chmod(0))),
        ("oversized", lambda path: path.write_bytes(b"x" * (panel.MAX_PROMPT + 1)))):
    invalid_brief = temporary / (name + "-brief.md")
    prepare(invalid_brief)
    refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                             "--dispatch", "worker", "--owner", "claude", "--access", "write",
                             "--purpose", "implement", "--brief-file", str(invalid_brief),
                             "--verify", "test -f value.txt", "--dry-run"],
                            env=dry_env, capture_output=True, text=True, timeout=60)
    assert refused.returncode != 0 and not refused.stdout, (name, refused)
    assert "brief must be a readable regular file" in refused.stderr, (name, refused.stderr)
    assert str(invalid_brief) not in refused.stderr
    assert room.status(project, shared)["participants"] == room_before_scan
    if name == "unreadable":
        invalid_brief.chmod(0o600)
assert not provider_marker.exists(), "dry-run invoked the fake provider"
call(plan_cli + ["add", "--id", "preflight-refusal", "--title", "Refused before worker start"])
refusal_task = call(plan_cli + ["show", "--id", "preflight-refusal"])
workers_before_refusal = worker_log.read_text()
refused_write = subprocess.run(blocked_brief.args[:-1] + ["--task-id", "preflight-refusal"],
                               env=room_env, capture_output=True, text=True, timeout=90)
assert refused_write.returncode != 0 and "preflight refused before task claim" in refused_write.stderr, refused_write
assert call(plan_cli + ["show", "--id", "preflight-refusal"]) == refusal_task
assert room.status(project, shared)["participants"] == room_before_scan
assert worker_log.read_text() == workers_before_refusal, "preflight refusal invoked a provider"
shutil.rmtree(project / ".oms/plan")  # later board fixtures expect a project with no plan
scoped_run = subprocess.run(scoped_dispatch.args[:-1], env=dict(room_env, PANEL_TEST_MODE="write"),
                            capture_output=True, text=True, timeout=90)
assert scoped_run.returncode == 0 and "Scope overlap" in scoped_run.stderr, scoped_run
scoped_call = [p for p in room.status(project, shared)["participants"] if p["participant"].startswith("call-")][-1]
assert scoped_call["owns"] == ["scripts/lib", "docs"] and scoped_call["parent"] == room_owner, scoped_call
room.append(project, shared, {"kind": "leave", "participant": scoped_call["participant"]}, "Left")
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, [])
for who in ("scope-advisor", "scope-live", "scope-done"):
    room.append(project, shared, {"kind": "leave", "participant": who}, "Left")
import hook_state
with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": room_owner, "OMS_PANEL_SESSION": "oms-fixture"}):
    tower = hook_state.panel_main_hint({"hook_event_name": "UserPromptSubmit"})
    assert "oms room scope" in tower
    for trigger in ("security-auditor for risky changes (permissions, secrets, destructive operations, scripts, hooks, install)",
                    "verifier before declaring completion (original request against final result)",
                    "integrator when merging or resolving conflicts", "performance-engineer when performance needs measurement",
                    "documenter at completion", "release-engineer when deploying",
                    "Advisor (which direction?), architect (how to structure?) and critic (when does it fail?) are separate judgments; call one when needed, not all by default",
                    "A role is a mandate, not an instance", "coordination yourself as the sole panel main",
                    "Never commit in the shared checkout another session uses"):
        assert trigger in tower, (trigger, tower)
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
for columns, rows in ((110, 44), (100, 24), (76, 18), (76, 21)):
    for alert in (None, "! Capacity full", "! Collection degraded - some state unknown"):
        graph_report = deepcopy(flow)
        if alert == "! Capacity full":
            graph_report["capacity_alerts"] = [alert]
        elif alert:
            graph_report["collection"]["ok"] = False
        for glyphs in (False, True):
            for menu in (False, True):
                nav = {}
                picture = render(graph_report, "codex", columns, rows, view="graph", unicode=glyphs, menu=menu,
                                 main_attempt="flow-attempt", navigation=nav)
                assert nav["surface"] == "graph", (alert, nav)
                if alert:
                    assert alert in picture, picture
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
                # Keys reach only this board's main and its calls, never an earlier main's.
                assert all(item[1] not in ("flow-other", "foreign-child") for item in nav["items"]), nav["items"]
                choose(("end",), nav)
                render(graph_report, "claude", columns, rows, view="graph", unicode=glyphs, menu=menu,
                       main_attempt="flow-attempt", navigation=nav)
                assert any(hit["action"] == nav["selected"] for hit in nav["hits"]), nav
                if not glyphs:
                    assert "╭" not in picture and "─" not in picture
assert render(flow, "codex", 100, 24, view="graph", frame=0) != render(flow, "codex", 100, 24, view="graph", frame=1)
# The tree keeps this window's main first; earlier mains of the room follow.
tree = render(flow, "codex", 110, 40, view="tree", main_attempt="flow-attempt")
assert tree.index("Main native chat") < tree.index("Other native chat"), tree
# A main on an earlier main's model is told apart by its id tail; the first listed (live, then newest) keeps the name.
twin = deepcopy(flow)
twin["room"]["participants"].append({"participant": "twin-main-9f3c", "role": "main", "joined": True, "provider": "claude",
                                     "model": "claude-opus-5-5", "label": "Twin native chat"})
twin["attempts"]["active_recent"].append({"attempt_id": "twin-main-9f3c", "state": "working", "panel": {
    "room_id": "flow-room", "room_participant": "twin-main-9f3c", "role": "main"}})
twin_tree = render(twin, "codex", 157, 40, view="tree")
assert "Opus 5.5 #9f3c" in twin_tree and "Opus 5.5 #ther" not in twin_tree, twin_tree
twin_detail = render(twin, "codex", 157, 40, view="graph", navigation={"selected": ("result", "foreign-child"),
    "preview": {"target": ("result", "foreign-child"), "report": {}}})
assert "for Opus 5.5 main" in twin_detail, twin_detail
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
# The overview keeps one-line cards while the selected block's content fills the lower detail area.
mailed = deepcopy(flow)
mailed["room"]["messages"] = [{"sender": "advisor", "targets": ["flow-main"], "text": "MAIL FOR MAIN"}]
mail_before = json.dumps(mailed, sort_keys=True)
detail_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
picture = render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation=detail_nav)
assert "[ Detail ]" in picture and "MAIN / Sol 6 / 1-9 of" in picture and "Team: 5 active" in picture and "MAIL FOR MAIN" in picture, picture
assert "ADVISOR / Astra" in picture and len(picture.splitlines()) <= 30, picture
assert json.dumps(mailed, sort_keys=True) == mail_before, "reading main detail changed room mail"
detail_band = next(b for b in detail_nav["bands"] if b["name"] == "detail")
choose(("scroll", 3, 5, detail_band["y1"] + 1), detail_nav)
assert detail_nav["band_offsets"]["detail"] == 3, detail_nav["band_offsets"]
# Plain Detail text is passive: repeated clicks neither navigate nor reset; a border row still activates the card.
body = next(h for h in detail_nav["hits"] if h.get("passive"))
for unused in range(2):
    assert choose(("click", body["x1"], body["y"]), detail_nav) is None
assert detail_nav["preview"]["target"] == ("chat", "flow-main") and "report" in detail_nav["preview"], detail_nav["preview"]
assert not any(h.get("passive") for h in detail_nav["hits"] if h["y"] in (detail_band["y1"], detail_band["y2"]))
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
# The board is the full-height left pane. One panel-wide width in absolute cells (the older percent form is read once
# and re-recorded as cells; an older session's top record is replaced): every main window's board, active or not,
# resizes itself to it; a border drag in the active window is recorded and the others follow by at least two cells;
# our own resize, a window resize, a zoomed board or an older top board never overwrite it; the board and the chat
# each keep 20 cells.
assert panel.parse_split("left:15c") == ("left", 15, False) and panel.parse_split("left:40") == ("left", 40, True)
assert panel.parse_split("left:3") is None and panel.parse_split("x") is None and panel.parse_split("left:0c") is None
assert panel.parse_split("top:25c") is None and panel.split_text(30) == "left:30c"
assert panel.split_cells(("left", 40, True), 220) == 88 and panel.split_cells(("left", 25, False), 50) == 25
assert panel.split_cells(("left", 5, False), 100) == 20 and panel.split_cells(("left", 95, False), 100) == 79
assert panel.split_cells(("left", 30, False), 30) == 15
assert panel.split_resize(("left", 25, False), 25, 50) is None and panel.split_resize(("left", 25, False), 24, 50) is None
assert panel.split_resize(("left", 25, False), 22, 50) == 25 and panel.split_resize(("left", 20, False), 25, 50) == 20
assert panel.split_resize(("left", 40, True), 25, 50) == 20
def board_calls(display, terminal=None, percent=""):
    def answer(command, **kwargs):
        return subprocess.CompletedProcess(command, 0, display if "display-message" in command else "", "")
    with patch.dict(os.environ, {"OMS_PANEL_BOARD_PERCENT": percent}), \
            patch.object(panel, "terminal_window", return_value=terminal), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        panel.board_split("%1", Path("/repo"), "board")
    return [call.args[0][1:] for call in tmux_run.call_args_list if "display-message" not in call.args[0]]
def left_split(width):
    return ["split-window", "-f", "-h", "-b", "-l", width, "-t", "%1", "-c", "/repo", "board"]
record_left = ["set-option", "-t", "$1", "@oms_panel_split", "left:56c"]
legacy_left = ["set-option", "-t", "$1", "@oms_panel_split", "left:80c"]
assert board_calls("200 50 1 $1\tcodex\t") == [record_left, left_split("56")]
assert board_calls("200 50 1 $1\tclaude\tleft:70c") == [left_split("70")]
assert board_calls("200 50 1 $1\tcodex\tleft:40") == [legacy_left, left_split("80")]
assert board_calls("200 50 1 $1\tcodex\ttop:12c") == [record_left, left_split("56")], "an older top record was kept"
assert board_calls("200 50 1 $1\tcontrol\t") == [left_split("56")]
# A detached session still reports tmux's 80x24: the attaching terminal sizes the board and the record.
assert board_calls("80 24 0 $1\tcodex\t", terminal=(240, 59)) == [
    ["set-option", "-t", "$1", "@oms_panel_split", "left:67c"], left_split("28%")]
assert board_calls("80 24 0 $1\tcodex\t") == [left_split("28%")]
for configured, cells in (("25", 50), ("35", 70), ("1", 50), ("99", 70), ("-1", 50),
                          ("garbage", 56), ("28.5", 56), ("2_8", 56)):
    assert board_calls("200 50 1 $1\tcodex\t", percent=configured) == [
        ["set-option", "-t", "$1", "@oms_panel_split", "left:%dc" % cells], left_split(str(cells))]
    assert board_calls("200 50 1 $1\tclaude\tleft:90c", percent=configured) == [left_split("90")]
assert board_calls("50 24 1 $1\tcodex\t", percent="25") == [
    ["set-option", "-t", "$1", "@oms_panel_split", "left:20c"], left_split("20")]
def sync_run(sync, *reads):
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run") as tmux_run:
        done = [sync.tick(read) for read in reads]
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
def reading(active, window, pane, text="", zoomed=False, panes=2):
    return (active, zoomed, panes, window, pane, text)
# A border drag in the active window is recorded as cells (clamped) and an inactive window resizes to the record.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (100, 50), (22, 50), "left:25c"))
assert done == [None, "record"] and calls == [["set-option", "-t", "s", "@oms_panel_split", "left:22c"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (100, 50), (5, 50), "left:25c"))
assert done == [None, "record"] and calls[-1][-1] == "left:20c", (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (25, 50), "left:22c"), reading(False, (100, 50), (22, 50), "left:22c"))
assert done == ["follow", None] and calls == [["resize-pane", "-t", "%1", "-x", "22"]], (done, calls)
# A reader pane splitting the chat column (three panes) leaves the board synced.
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (130, 50), "left:80c", panes=3))
assert done == ["follow"] and calls == [["resize-pane", "-t", "%1", "-x", "80"]], (done, calls)
# The active window enforces the record too, and its own resize is not read back as a drag.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (30, 50), "left:22c"), reading(True, (100, 50), (21, 50), "left:22c"))
assert done == ["follow", None] and calls == [["resize-pane", "-t", "%1", "-x", "22"]], (done, calls)
# A window resize moves the board back to the recorded cells and never records the new size.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (120, 50), (30, 50), "left:25c"))
assert done == [None, "follow"] and calls == [["resize-pane", "-t", "%1", "-x", "25"]], (done, calls)
# Nothing recorded yet: the active window records its size, an inactive one waits; the older percent form is converted.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50)), reading(False, (100, 50), (25, 50)))
assert done == ["record", None] and calls == [["set-option", "-t", "s", "@oms_panel_split", "left:25c"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (25, 50), "left:30"))
assert done == ["follow"] and calls == [["set-option", "-t", "s", "@oms_panel_split", "left:30c"],
                                        ["resize-pane", "-t", "%1", "-x", "30"]], (done, calls)
# A zoomed board and an older session's full-width top board are left alone.
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (100, 50), "left:30c", zoomed=True),
                       reading(False, (200, 50), (200, 25), "left:15c"))
assert done == [None, None] and calls == [], (done, calls)
# A call opens in a reader pane right of the board: the window's recorded native chat pane is split with the reader
# on top and the reader is marked with its board; a second call respawns that reader, close kills only it, and a
# short chat or a missing/foreign chat record keeps the detail in the board. Recorded identifiers are revalidated
# and every argument of the pane command is quoted.
import panel_reader
assert panel_reader.target_args(("result", "worker-1")) == ["--results", "--participant", "worker-1"]
assert panel_reader.target_args(("debate", ("task.1", "seat-a"))) == [
    "--results", "--participant", "seat-a", "--task-id", "task.1"]
for bad in (("result", "-x"), ("result", "a b"), ("result", "$(touch x)"), ("result", None),
            ("debate", ("-t", "seat")), ("debate", ("t;x", "seat")), ("chat", "main-1")):
    try:
        panel_reader.target_args(bad)
    except ValueError:
        pass
    else:
        raise AssertionError(bad)
words = shlex.split(panel.reader_shell(Path("/repo x"), ("result", "worker-1"), "%1"))
assert words[:2] == ["exec", "env"] and "OMS_PANEL_READER_BOARD=%1" in words, words
assert words[-5:] == ["--repo", "/repo x", "--results", "--participant", "worker-1"], words
def reader_calls(listing, native="%2", fail=()):
    def answer(command, **kwargs):
        out = {"list-panes": listing, "show-option": native + "\n", "split-window": "%9\n"}.get(command[1], "")
        return subprocess.CompletedProcess(command, 1 if command[1] in fail else 0, out, "")
    with patch.object(panel_reader.subprocess, "run", side_effect=answer) as tmux_run:
        opened = panel_reader.open_reader("%1", "CMD", Path("/repo"))
    return opened, [call.args[0][1:] for call in tmux_run.call_args_list]
opened, calls = reader_calls("%1\t40\t\n%2\t40\t\n")
assert opened == "%9" and calls[-2:] == [
    ["split-window", "-d", "-v", "-b", "-l", "24", "-t", "%2", "-c", "/repo", "-P", "-F", "#{pane_id}", "CMD"],
    ["set-option", "-p", "-t", "%9", "@oms_panel_reader", "%1"]], calls
opened, calls = reader_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n")
assert opened == "%9" and calls[-1] == ["respawn-pane", "-k", "-t", "%9", "-c", "/repo", "CMD"], calls
assert not any(c[0] in ("split-window", "kill-pane") or "%2" in c for c in calls), calls
opened, calls = reader_calls("%1\t40\t\n%8\t24\t%7\n%2\t15\t\n")
assert opened == "%9" and calls[-2][:7] == ["split-window", "-d", "-v", "-b", "-l", "9", "-t"], "another board's reader was reused"
for listing, native in (("%1\t20\t\n%2\t12\t\n", "%2"), ("%1\t40\t\n%2\t40\t\n", ""), ("%1\t40\t\n%2\t40\t\n", "%1"),
                        ("%1\t40\t\n%2\t40\t\n", "%5"), ("%1\t40\t\n%2\t40\t\n", "%2;kill")):
    opened, calls = reader_calls(listing, native)
    assert opened is None and not any(c[0] == "split-window" for c in calls), (listing, native, calls)
opened, calls = reader_calls("%1\t40\t\n%2\t40\t\n", fail=("set-option",))
assert opened is None and calls[-1] == ["kill-pane", "-t", "%9"], "an unmarked reader was left behind"
for failure in (subprocess.TimeoutExpired("tmux", 3), OSError("tmux unavailable")):
    for cleanup_failure in (None, subprocess.TimeoutExpired("tmux", 3), OSError("tmux gone")):
        def interrupted_reader(command, **kwargs):
            assert kwargs["timeout"] == 3 and kwargs["stdin"] == subprocess.DEVNULL
            if command[1] == "set-option":
                raise failure
            if command[1] == "kill-pane" and cleanup_failure:
                raise cleanup_failure
            out = {"list-panes": "%1\t40\t\n%2\t40\t\n", "show-option": "%2\n", "split-window": "%9\n"}.get(command[1], "")
            return subprocess.CompletedProcess(command, 0, out, "")
        with patch.object(panel_reader.subprocess, "run", side_effect=interrupted_reader) as tmux_run:
            try:
                panel_reader.open_reader("%1", "CMD", Path("/repo"))
            except (OSError, subprocess.TimeoutExpired) as error:
                assert error is failure, "cleanup replaced the original failure"
            else:
                raise AssertionError("reader marking failure was swallowed")
        assert tmux_run.call_args.args[0] == ["tmux", "kill-pane", "-t", "%9"], "an unmarked reader was left behind"
assert reader_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n", fail=("respawn-pane",))[0] is None
assert panel_reader.split_rows(13) is None and panel_reader.split_rows(14) == 8 and panel_reader.split_rows(40) == 24
def close_calls(listing, board="%1"):
    with patch.object(panel_reader.subprocess, "run", side_effect=lambda command, **kwargs: subprocess.CompletedProcess(
            command, 0, listing if command[1] == "list-panes" else "", "")) as tmux_run:
        closed = panel_reader.close_reader(board)
    return closed, [call.args[0][1:] for call in tmux_run.call_args_list]
assert close_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n") == (True, [
    ["list-panes", "-t", "%1", "-F", "#{pane_id}\t#{pane_height}\t#{@oms_panel_reader}"], ["kill-pane", "-t", "%9"]])
assert close_calls("%1\t40\t\n%2\t40\t\n")[0] is False and close_calls("x", board="1; kill")[1] == []
# Completion at the same terminal size and line count is repainted without losing the person's scroll.
def read_page(render, batches):
    clock, waits = [0.0], []
    class PageInput:
        def __enter__(self):
            return self
        def __exit__(self, *args):
            pass
        def wait(self, seconds):
            assert 0 < seconds <= 1
            waits.append(seconds)
            clock[0] += seconds
            return next(batches)
    with patch.object(panel_reader, "TerminalInput", return_value=PageInput()), \
            patch.object(panel_reader.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel_reader.shutil, "get_terminal_size", return_value=os.terminal_size((80, 4))), \
            patch.object(panel_reader, "tmux", return_value=subprocess.CompletedProcess([], 0, "%1\n", "")) as tmux_run, \
            patch.object(panel_reader.sys, "stdout", io.StringIO()) as output:
        assert panel_reader.page(render, "%1") == 0
    assert tmux_run.call_args.args == ("select-pane", "-t", "%1")
    return output.getvalue(), waits
with patch.object(panel, "render_results", side_effect=["\n".join("running %d" % i for i in range(8)),
                                                       "\n".join("done %d" % i for i in range(8))]) as rendered:
    output, waits = read_page(rendered, iter([[("pagedown",)], [], [("q",)]]))
assert rendered.call_count == 2 and waits == [1, 1, 1], "reader evidence polling was unbounded or stale"
assert "done 3\ndone 4\ndone 5" in output and output.count("4-6/8") == 2, output
with patch.object(panel, "render_results", side_effect=["\n".join(str(i) for i in range(8)), "short"]) as rendered:
    output, unused = read_page(rendered, iter([[("end",)], [], [("escape",)]]))
assert "short\n\n\nq/Esc" in output and "1-1/1" in output, "shrinking evidence left the reader out of range"
failure = "bad\033]52;c;payload\a\x9b31m\u200b\r\n" + "x" * 250
with patch.object(panel, "render_results", side_effect=ValueError(failure)) as rendered:
    output, unused = read_page(rendered, iter([[("q",)]]))
assert "Result evidence unavailable: " + panel.clean(failure, 200) in output, output
assert "\033]52" not in output and not any(c in output for c in ("\a", "\x9b", "\u200b", "\r")), output
# A slow result query runs off the input thread: q closes the reader at once, showing the pending notice.
slow_started = time.time()
output, unused = read_page(lambda width: (time.sleep(3), "late")[1], iter([[("q",)]]))
assert time.time() - slow_started < 2 and "Reading the recorded result..." in output, output
# A room transition closes the reader through the watcher's close path; refreshes of the same room do not.
class RoomFrameReceived(Exception):
    pass
for current_room in ("old-room", "new-room"):
    def room_frame(*args, **kwargs):
        assert closed.call_count == int(current_room != "old-room"), "room refresh left the reader open or closed it unnecessarily"
        if current_room != "old-room":
            closed.assert_called_once_with("%1")
            assert "preview" not in kwargs["navigation"]
        raise RoomFrameReceived()
    with patch.dict(os.environ, {"TMUX": "", "TMUX_PANE": "%1", "OMS_PANEL_RESUME": json.dumps(
            {"room_id": "old-room", "preview": ["result", "w1"]})}), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "session_option", return_value=""), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel, "read_panel", return_value=({"room": {"id": current_room}}, 0, "codex", None,
                                                           "graph", False, current_room)), \
            patch.object(panel, "frame_view", side_effect=room_frame), \
            patch.object(panel_reader, "close_reader", return_value=True) as closed:
        try:
            panel.watch(Path("."), "codex", count=1)
        except RoomFrameReceived:
            pass
# A click or Enter that previews a call (or selects a main) on the board is what opens (or closes) the reader;
# arrow keys and clicks elsewhere leave it alone.
chosen_nav = {"preview": {"target": ("result", "w1")}, "selected": ("result", "w1"),
              "hits": [{"y": 3, "x1": 1, "x2": 9, "action": ("result", "w1")}, {"y": 4, "x1": 1, "x2": 9, "action": ("group", "g")}]}
assert panel_reader.picked(("enter",), chosen_nav) == ("result", "w1") == panel_reader.picked(("click", 2, 3), chosen_nav)
assert panel_reader.picked(("click", 2, 4), chosen_nav) is None and panel_reader.picked(("down",), chosen_nav) is None
assert panel_reader.picked(("enter",), dict(chosen_nav, preview={"target": ("chat", "m1")}, selected=("chat", "m1"))) == ("chat", "m1")
# A reader pane is never discovered as a board or an input pane, so --reopen ignores it.
reader_row = "oms-fixture\t@1\t%%3\t0\t%%1\texec bash %s panel --repo /repo --results --participant w1" % panel.ENTRY
board_row = "oms-fixture\t@1\t%%1\t0\t\texec bash %s panel --repo /repo --watch" % panel.ENTRY
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, reader_row + "\n" + board_row + "\n", "")):
    assert [row["pane"] for row in panel.panel_panes(Path("/repo"))] == ["%1"]
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
assert "--main flow-main" in hint and "--dispatch advisor" in hint, hint
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
with patch.object(panel.sys.stdin, "isatty", return_value=False), patch.dict(os.environ, {}, clear=False):
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
race_repo = project
original_split = panel.split_panel
seen = {}
with patch.dict(os.environ, {"OMS_ROOM_ID": ""}), patch.object(panel, "session_owner", side_effect=[None, None, str(race_repo), str(race_repo)]), \
        patch.object(panel, "legacy_session", return_value=None), patch.object(panel, "check_legacy_main"), \
        patch.object(panel, "ensure_room", return_value="room-lost"), \
        patch.object(panel.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, ["tmux"])), \
        patch.object(panel, "split_panel", side_effect=lambda *a, **k: seen.update(room=os.environ.get("OMS_ROOM_ID"), retry=k.get("retry")) or 0):
    os.environ.pop("OMS_ROOM_ID")
    assert original_split(race_repo, launch="codex") == 0
assert seen == {"room": None, "retry": False}, seen
# A new panel opens only Claude, sharing the room recorded before the owner marker.
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-both"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.shutil, "which", return_value="/fixture/bin"), \
        patch.object(panel.subprocess, "check_output", side_effect=["%1\n", "%2\n", "%3\n"]) as created_panes, \
        patch.object(panel, "check_legacy_main"), \
        patch.object(panel.subprocess, "run") as tmux_calls, patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
    os.environ.pop("OMS_ROOM_ID")
    os.environ.pop("OMS_PANEL_MAINS", None)
    assert panel.split_panel(race_repo) == 0
    windows = [c.args[0] for c in created_panes.call_args_list]
    assert windows[0][1] == "new-session" and [w[w.index("-n") + 1] for w in windows[1:]] == ["claude"], windows
    order = [c.args[0][1:5] for c in tmux_calls.call_args_list]
    assert ["set-environment", "-t", "=" + panel.panel_session(race_repo), "OMS_ROOM_ID"] in order
    assert any(c.args[0][1:3] == ["select-window", "-t"] and c.args[0][3] == "%2" for c in tmux_calls.call_args_list)
with patch.dict(os.environ, {"OMS_PANEL_MAINS": ""}), patch.object(panel.sys, "stderr", new_callable=io.StringIO) as warnings:
    assert panel.panel_mains() == []
    assert warnings.getvalue() == ""
for invalid in ("claude,pi", "unknown"):
    with patch.dict(os.environ, {"OMS_PANEL_MAINS": invalid}), patch.object(panel.shutil, "which", return_value="/x"), \
            patch.object(panel.sys, "stderr", new_callable=io.StringIO) as warnings:
        assert panel.panel_mains() == []
        assert warnings.getvalue().splitlines() == [
            "Warning: OMS_PANEL_MAINS accepts a single value: claude, pi, codex (or empty to open no main); no main opened."]
for configured, available, expected in ((None, {"claude", "pi", "codex"}, ["claude"]),
                                      (None, {"pi", "codex"}, ["pi"]),
                                      (None, {"pi"}, []),
                                      (None, {"codex"}, ["codex"]),
                                      ("pi", {"claude", "pi", "codex"}, ["pi"]),
                                      ("claude", {"pi", "codex"}, [])):
    with patch.dict(os.environ, {}, clear=False), \
            patch.object(panel.shutil, "which", side_effect=lambda name: "/x/" + name if name in available else None):
        if configured is None:
            os.environ.pop("OMS_PANEL_MAINS", None)
        else:
            os.environ["OMS_PANEL_MAINS"] = configured
        assert panel.panel_mains() == expected, (configured, available, panel.panel_mains())
# A session whose owner marker is not written yet is waited for, not refused as foreign.
with patch.object(panel, "session_owner", side_effect=["", str(race_repo), str(race_repo)]), patch.object(panel.time, "sleep"), \
        patch.object(panel.subprocess, "call", return_value=0):
    assert panel.split_panel(race_repo) == 0
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-first"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.subprocess, "check_output", return_value="%1\n"), \
        patch.object(panel, "check_legacy_main"), \
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
assert panel.panel_session(Path("/tmp/project-a")) == "oms-project-a"
# The slug is lowercase [a-z0-9-]; a taken name falls to the hashed one, never over another checkout or a non-OMS session.
import tempfile
with tempfile.TemporaryDirectory() as slug_root:
    odd = Path(slug_root) / "  My.Project_X v2  "
    odd.mkdir()
    plain, hashed = panel.session_candidates(odd)
    assert plain == "oms-my-project-x-v2" and re.fullmatch(r"oms-my-project-x-v2-[a-f0-9]{6}", hashed), (plain, hashed)
    long_name = Path(slug_root) / ("A" * 40)
    long_name.mkdir()
    assert panel.session_candidates(long_name)[0] == "oms-" + "a" * 32
    bare = Path(slug_root) / "..."
    bare.mkdir()
    assert panel.session_candidates(bare)[0] == "oms-project"
    assert re.fullmatch(panel.PANEL_SESSION, hashed) and re.fullmatch(panel.PANEL_SESSION, "oms-panel-abcdef012345")
    for held, expected in ((None, plain), (str(odd.resolve()), plain), ("/elsewhere", hashed), ("", hashed)):
        with patch.object(panel, "session_owner", side_effect=lambda n, h=held: h if n == plain else None):
            assert panel.resolve_session(odd, wait=False) == (expected, held if expected == plain else None), held
    with patch.object(panel, "session_owner", return_value="/elsewhere"):
        assert panel.resolve_session(odd, wait=False) == (hashed, "/elsewhere")
    # A live legacy panel proven for this checkout is adopted while the new name is free.
    with patch.object(panel, "session_owner", side_effect=lambda n: None if n == plain else str(odd)), \
            patch.object(panel, "legacy_session", return_value="oms-panel-aaaaaaaaaaaa"), \
            patch.object(panel, "bind_main_keys"), patch.object(panel.subprocess, "run") as adopted, \
            patch.object(panel.subprocess, "call", return_value=0):
        assert panel.split_panel(odd) == 0
        assert ["tmux", "set-option", "-t", "oms-panel-aaaaaaaaaaaa", "@oms_panel_repo", str(odd)] in \
            [c.args[0] for c in adopted.call_args_list]
# The context left rides on the shown main's card in three bands; only the proven main carries it.
claude_main = next(m for m in flow["room"]["participants"] if m.get("role") == "main" and m.get("provider") == "claude")["participant"]
for left, words in ((42, "ctx 42%"), (22, "ctx 22%"), (12, "ctx 12% · compact soon")):
    metered = dict(flow, provider_status={"claude": {"participant": claude_main, "context_left": left}})
    picture = render(metered, "codex", 110, 30, view="graph", navigation={"selected": ("chat", claude_main)})
    # The usage row already says "ctx N%"; the card is the second place.
    assert picture.count(words) == 1 + (left > 15), (left, picture)
    plain = render(metered, "codex", 110, 30, view="graph", unicode=False, navigation={"selected": ("chat", claude_main)})
    assert plain.count(words.replace("·", "/")) == 1 + (left > 15) and not any(g in plain for g in "─│·"), (left, plain)
assert render(dict(flow, provider_status={"claude": {"participant": "someone-else", "context_left": 42}}), "codex", 110, 30,
              view="graph", navigation={"selected": ("chat", claude_main)}).count("ctx 42%") == 1
# The board shows only its own main's calls, expanded or not.
for board_nav in ({}, {"expanded": True}):
    assert "ONLY OTHER OWNER" not in render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=board_nav)
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
page_nav["surface"] = "inbox"  # A reader opened from the inbox must still receive page and wheel events.
page_nav["detail"] = {"title": "RECORDED RESULTS", "text": "\n".join("line %s" % n for n in range(300))}
render_detail(flow, page_nav["detail"], 110, 30, page_nav)
bands_before = dict(page_nav["band_offsets"])
choose(("down",), page_nav)
assert page_nav["offset"] == 1 and page_nav["band_offsets"] == bands_before
choose(("end",), page_nav)
assert "line 299" in render_detail(flow, page_nav["detail"], 110, 30, page_nav)
choose(("down",), page_nav)
assert "line 299" in render_detail(flow, page_nav["detail"], 110, 30, page_nav)
choose(("home",), page_nav)
choose(("up",), page_nav)
assert page_nav["offset"] == 0
page_nav["detail"]["text"] = "\n".join("long result line %s with retained information" % n for n in range(300))
render_detail(flow, page_nav["detail"], 110, 30, page_nav)
wide_rows = page_nav["body_rows"]
render_detail(flow, page_nav["detail"], 24, 20, page_nav)
assert page_nav["body_rows"] > wide_rows
choose(("end",), page_nav)
assert "information" in render_detail(flow, page_nav["detail"], 24, 20, page_nav)
choose(("home",), page_nav)
choose(("pagedown",), page_nav)
choose(("scroll", 3, 5, 10), page_nav)
assert page_nav["offset"] > 0 and page_nav["bands"] == [], page_nav["offset"]
assert "EXTRA WORKER 2" in render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
try:
    panel.main(["--repo", str(root), "--dispatch", "advisor", "--owner", "claude", "--main", "a b", "--dry-run", "--prompt", "x"])
except SystemExit as error:
    assert error.code == 2
else:
    raise AssertionError("an invalid --main was accepted")
# Each snapshot refreshes an open preview; another room resets the view.
kept_nav = {"room_id": "flow-room", "preview": {"target": ("result", "builder"), "report": {}}}
assert panel.refreshed_navigation(kept_nav, flow)["preview"]["refresh"] is True
assert "preview" not in panel.refreshed_navigation(dict(kept_nav, room_id="other"), flow)
# The bottom box is one panel-session state: a tab, target or scroll changed on one board reaches the others.
session_bottom = {}
def fake_bottom_tmux(command, **kwargs):
    if "@oms_panel_bottom" in command:
        session_bottom["raw"] = command[command.index("@oms_panel_bottom") + 1]
    return subprocess.CompletedProcess(command, 0, "", "")
fresh_bottom = {"tab": "detail", "target": None, "offset": 0, "dismissed": False}
board_a = {"room_id": "flow-room", "tab": "plan", "preview": {"target": ("debate", ("t1", "flow-other"))},
           "band_offsets": {"detail": 3}}
with patch.object(panel, "managed_session", return_value=True), patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-panel-x"}), \
        patch.object(panel.subprocess, "run", side_effect=fake_bottom_tmux) as bottom_writes:
    panel.publish_bottom(board_a, panel.bottom_state(board_a))
    assert bottom_writes.call_count == 0, "an unchanged bottom box was written"
    panel.publish_bottom(board_a, fresh_bottom)
    assert bottom_writes.call_count == 1 and len(session_bottom["raw"]) <= 512, session_bottom
    assert panel.read_bottom(session_bottom["raw"]) == panel.bottom_state(board_a) == {
        "tab": "plan", "target": ("debate", ("t1", "flow-other")), "offset": 3, "dismissed": False}, session_bottom
board_b = {"room_id": "flow-room", "tab": "messages", "preview": {"target": ("chat", "flow-main")}}
panel.refreshed_navigation(board_b, flow, session_bottom["raw"])
assert board_b["tab"] == "plan" and board_b["preview"]["target"] == ("debate", ("t1", "flow-other")), board_b
assert board_b["band_offsets"]["detail"] == 3 and "selected" not in board_b, board_b
board_b["tab"] = "debate"  # B's own later change is not undone while the shared value is unchanged
assert panel.refreshed_navigation(board_b, flow, session_bottom["raw"])["tab"] == "debate"
shared_tab = lambda target: json.dumps({"tab": "messages", "target": target, "offset": 0})
assert "preview" not in panel.refreshed_navigation(board_b, flow, shared_tab(None))
# Esc in one window empties the box in every window; a later pick clears it again.
emptied = panel.refreshed_navigation({"room_id": "flow-room"}, flow,
                                     json.dumps({"tab": "detail", "target": None, "offset": 0, "dismissed": True}))
assert emptied.get("dismissed") is True and panel.bottom_state(emptied)["dismissed"] is True, emptied
assert "dismissed" not in panel.refreshed_navigation(emptied, flow, shared_tab(["chat", "flow-main"]))
# A target naming nobody in this room is ignored; the tab still follows.
ghost_nav = panel.refreshed_navigation({"room_id": "flow-room", "preview": {"target": ("chat", "flow-main")}},
                                       flow, shared_tab(["chat", "ghost"]))
assert ghost_nav["tab"] == "messages" and ghost_nav["preview"]["target"] == ("chat", "flow-main"), ghost_nav
for invalid in ("", "{", "[]", '{"tab":"nope","target":null}', '{"tab":"plan","target":["x","y"]}',
                '{"tab":"plan","target":["chat","a b"]}', '{"tab":"plan","target":["debate","t1"]}',
                '{"tab":"plan","offset":-1}', '{"tab":"plan","offset":true}', '{"tab":"plan","extra":1}',
                '{"tab":"plan","dismissed":1}', '{"tab":"between","target":null}', shared_tab(["pair", ["flow-main", "flow-other"]]),
                shared_tab(["chat", "x" * 600]), shared_tab(["chat", "x" * 129])):
    assert panel.read_bottom(invalid) is None, invalid
    assert panel.refreshed_navigation({"room_id": "flow-room", "tab": "plan"}, flow, invalid) == {"room_id": "flow-room", "tab": "plan"}
# Arriving at a window never clears the selection.
assert "was_active" not in Path(panel.__file__).read_text(encoding="utf-8")
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
        panel.dispatch(Path("/tmp/project-a"), "claude", "advisor", None, "read", None,
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
    # A --launch panel has no control window: its boards do not offer F5.
    for control, offered in ((True, True), (False, False)):
        launched = render(flow, "codex", 120, 24, view="graph", main_attempt="flow-attempt",
                          navigation={"control_window": control}, managed=True).splitlines()[-1]
        assert ("F5 Control" in launched) == offered and launched.endswith("F12 Keys"), launched
    open_nav = {"preview": {"target": ("chat", "flow-main")}}
    assert "Esc Close" in render(flow, "codex", 157, 24, view=view_name, main_attempt="flow-attempt",
                                 navigation=open_nav, managed=True).splitlines()[-1]
    for expanded_board, label in ((False, "v Expand"), (True, "v Collapse")):
        helped = render(flow, "codex", 100, 24, view=view_name, main_attempt="flow-attempt", managed=True,
                        navigation={"keys_help": True, "expanded": expanded_board}).splitlines()
        assert label in "\n".join(helped) and "? Hide" in "\n".join(helped), helped
        assert all(display_width(line) <= 100 for line in helped), helped
alarmed = deepcopy(flow)
alarmed["room"]["participants"].append({"participant": "third-main", "role": "main", "joined": True,
                                        "provider": "claude", "model": "claude-opus-5-5", "label": "Third"})
alarmed["room"]["participants"].append({"participant": "third-fail", "role": "worker", "joined": True,
                                        "model": "gpt-6-luna", "parent": "third-main", "label": "Broken"})
alarmed["attempts"]["active_recent"] += [
    {"attempt_id": "third-main", "state": "working", "panel": {"room_id": "flow-room", "room_participant": "third-main", "role": "main"}},
    {"attempt_id": "third-fail", "state": "failed", "panel": {"room_id": "flow-room", "room_participant": "third-fail", "role": "worker"}}]
for columns in range(76, 90):
    sweep_nav = {}
    swept = render(alarmed, "codex", columns, 24, view="graph", main_attempt="flow-attempt", navigation=sweep_nav)
    assert all(display_width(line) <= columns for line in swept.splitlines()), (columns, swept)
    assert all(hit["x2"] <= columns for hit in sweep_nav["hits"]), columns
# Finished calls leave the live drawing; their past-work count and inspection remain.
finished_flow = deepcopy(flow)
for attempt in finished_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
finished_picture = render(finished_flow, "codex", 120, 34, view="graph", navigation={"dismissed": True})
assert "WORKER / Sonnet" not in finished_picture and "Past work (1)" in finished_picture, finished_picture
lines_of = finished_picture.splitlines()
assert next(i for i, l in enumerate(lines_of) if "ADVISOR / Astra" in l) < next(i for i, l in enumerate(lines_of) if "MAIN / Sol 6" in l) \
    < next(i for i, l in enumerate(lines_of) if "WORKER / Luna" in l), finished_picture
ordered = render(flow, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation={"preview": {
    "target": ("result", "builder"), "report": {"rows": [{"outcome": "accepted", "calls": [{"answer": "DONE TEXT"}]}]}}})
assert ordered.index("Accepted by its main") < ordered.index("DONE TEXT"), ordered
auto_nav = {}
auto_board = render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation=auto_nav)
assert "[ Detail ]" in auto_board and "MAIN / Sol 6 / auto" in auto_board, auto_board
roomy = render(flow, "codex", 100, 60, view="graph", main_attempt="flow-attempt", navigation={})
roomy_lines = roomy.splitlines()
detail_top = next(i for i, line in enumerate(roomy_lines) if "[ Detail ]" in line)
detail_end = next(i for i in range(detail_top + 1, len(roomy_lines)) if roomy_lines[i].startswith("╰"))
# The board fills the pane (key hints pinned to the last row) and the tab box takes the rows the cards leave,
# so switching tabs never resizes it and no blank band sits under it.
assert detail_end - detail_top >= 20 and len(roomy_lines) == 60, roomy
assert not any(line.strip() for line in roomy_lines[detail_end + 1:-3]) or roomy_lines[detail_end + 1].strip(), roomy
closed_board = render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
assert "MAIN / Sol 6 / auto" not in closed_board and "No call selected" in closed_board, closed_board
# A board too short for the full overview still keeps a feasible box, auto or dismissed, and its warning row above it.
for rows_high in (26, 28, 30, 34):
    kept = {}
    for label, nav in (("auto", {}), ("closed", {"dismissed": True})):
        shown = render(flow, "codex", 100, rows_high, view="graph", main_attempt="flow-attempt", navigation=nav)
        kept[label] = (nav["box"]["y1"], nav["box"]["y2"])
        lines_shown = shown.splitlines()
        assert nav["box"]["y2"] - nav["box"]["y1"] + 1 >= 5 and nav["box"]["y2"] == rows_high - 1, (rows_high, nav["box"], shown)
        assert ("MAIN / Sol 6 / auto" in shown) == (label == "auto") and ("No call selected" in shown) == (label == "closed"), shown
        assert "no known main" in lines_shown[nav["box"]["y1"] - 2], shown
    assert kept["auto"] == kept["closed"], (rows_high, kept)
# A room with four recorded mains and one worker: advisors 0 to 3, other rooms and an unlinked call
# change neither the box nor the visibility of their own warning.
four = deepcopy(msg_report)
four["room"]["participants"].append({"participant": "m4", "role": "main", "joined": True, "model": "gpt-6-luna", "seq": 4,
                                     "provider": "codex", "attempt": "a4"})
four["attempts"]["active_recent"].append({"attempt_id": "a4", "state": "working", "panel": {
    "room_id": "r", "room_participant": "m4", "role": "main"}})
four["room"]["messages"] = [
    {"id": "p%d" % i, "sender": "m1", "recipient": "m2", "targets": ["m2"], "message_kind": "note",
     "ts": "2026-10-07T10:%02d:00Z" % i, "text": "pair %d" % i} for i in range(6)]
four["room"]["pairs"] = [{"sender": "m1", "recipient": "m2", "sent": 6, "pending": 0}]
for variant in ("plain", "other", "unlinked"):
    kept = set()
    for advisors in (0, 1, 3):
        report = crowd(four, advisors, 1, parent="a1", prefix=variant)
        if variant == "other":
            report["room"]["other_rooms"] = [{"id": "room-next", "mains": 1, "pending": 2}]
        if variant == "unlinked":
            report["room"]["participants"].append({"participant": "orphan", "role": "worker", "joined": True, "parent": "gone",
                                                   "model": "gpt-6-luna", "label": "orphan"})
            report["attempts"]["active_recent"].append({"attempt_id": "orphan", "state": "working", "panel": {
                "room_id": "r", "room_participant": "orphan", "role": "worker"}})
        nav = {"selected": ("chat", "m1"), "dismissed": True}
        text = render(report, "codex", 157, 32, view="graph", navigation=nav, main_attempt="a1")
        assert len(text.splitlines()) == 32, text
        assert ("OTHER ROOMS" in text) == (variant == "other") and ("no known main" in text) == (variant == "unlinked"), text
        kept.add((nav["box"]["y1"], nav["box"]["y2"]))
    assert len(kept) == 1, (variant, kept)
# Where a readable box cannot fit, the one-row tab strip stays reachable while the cards fit around it (19 rows;
# at 16 the compact fallback draws none); neither keeps the previous box or its open-in-full target.
for rows_high in (19, 16):
    nav = {"preview": {"target": ("result", "builder"), "report": {}}, "box": {"y1": 1, "y2": 2}, "full_result": ("result", "builder")}
    shown = render(flow, "codex", 100, rows_high, view="graph", main_attempt="flow-attempt", navigation=nav)
    assert nav["surface"] == "graph" and "box" not in nav and "full_result" not in nav and len(shown.splitlines()) == rows_high, nav
    assert rows_high == 16 or shown.splitlines()[-2].startswith("╭─ [ Detail ]  Plan"), shown
# Tabs and body actions follow the drawn dock through resize, including help and the tree fallback.
for managed in (False, True):
    for keys_help in (False, True):
        for initial_tab in ("detail", "plan", "debate", "messages"):
            nav = {"tab": initial_tab, "keys_help": keys_help}
            for columns, rows_high in ((76, 16), (76, 19), (100, 30), (76, 16), (50, 10)):
                shown = render(flow, "codex", columns, rows_high, view="graph", main_attempt="flow-attempt",
                               navigation=nav, managed=managed)
                strip = [h for h in nav["hits"] if h["action"] == ("tab", "detail")]
                box = nav.get("box")
                assert len(shown.splitlines()) <= rows_high and all(display_width(line) <= columns for line in shown.splitlines()), shown
                assert all(1 <= h["y"] <= len(shown.splitlines()) and 1 <= h["x1"] <= h["x2"] <= columns
                           for h in nav["hits"]), nav
                if not keys_help:
                    assert bool(strip) == (rows_high in (19, 30)), (columns, rows_high, nav, shown)
                    assert bool(box) == (rows_high == 30), (columns, rows_high, nav, shown)
                footer = "\n".join(shown.splitlines()[-5:] if keys_help else shown.splitlines()[-1:])
                if not box:
                    assert not any(hint in footer for hint in
                                   ("wheel Scroll", "↑↓ Message", "↑↓ Seat", "←→ Filter", "←→ Target")), shown
                    assert not tab_event(("scroll", 1, 1, 1), nav), nav
                if not strip:
                    assert "Tab Next tab" not in footer, shown
                    before_tab = nav["tab"]
                    assert not tab_event(("tab",), nav) and nav["tab"] == before_tab, nav
                else:
                    assert tab_event(("click", strip[0]["x1"], strip[0]["y"]), nav) and nav["tab"] == "detail", nav
                    assert tab_event(("tab",), nav) and nav["tab"] == "plan", nav
                    # Keep each tab's body hints when a full box fits.
                    if box and not keys_help:
                        nav["tab"] = initial_tab
                        full = render(flow, "codex", columns, rows_high, view="graph", main_attempt="flow-attempt",
                                      navigation=nav, managed=managed)
                        expected = {"plan": "wheel Scroll", "messages": "↑↓ Message"}.get(initial_tab)
                        assert not expected or expected in full.splitlines()[-1], full
                nav["tab"] = initial_tab
# People read the board: call exits become words, machine status lines and markdown marks are dropped.
import room_view as graph_text
assert graph_text.readable("Call exit=0; parent acceptance pending.\nstop-reason: provider=codex is_error=0\n**Verification:** `ok`", " ") \
    == "Finished. Verification: ok"
assert graph_text.readable("Call exit=2; parent acceptance pending.", " ") == "Failed (exit 2)."
assert graph_text.readable("## Decision\n\nFirst paragraph\n\n\n- next step", paragraphs=True) == "Decision\n\nFirst paragraph\n\n- next step"
talking = deepcopy(flow)
talking["room"]["pairs"] = [{"sender": "flow-main", "recipient": "flow-other", "sent": 2, "pending": 1}]
picture = render(talking, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
assert "Between mains" not in picture and "BETWEEN MAINS" not in picture, picture
tall = render(talking, "codex", 157, 65, view="graph", main_attempt="flow-attempt", navigation={})
assert "BETWEEN MAINS" not in tall and "Between mains" not in tall, tall
assert "Model use, last calls" not in tall and "Click any block" not in tall, tall
assert "..." not in tall and sum("week" in line for line in tall.splitlines()) <= 1, tall
import time as pair_clock
stamp = pair_clock.strftime("%Y-%m-%dT%H:%M:%SZ", pair_clock.gmtime())
# Board truth: stale status shows its age, a guard stop reads as one, calls in review are grouped.
def stamp_ago(hours):
    return pair_clock.strftime("%Y-%m-%dT%H:%M:%SZ", pair_clock.gmtime(pair_clock.time() - hours * 3600))
truth = deepcopy(flow)
truth["room"]["participants"][0]["label"] = "Native task not recorded"
truth["room"]["statuses"] = {"flow-main": {"text": "Now: 방 범위 선언", "ts": stamp_ago(6), "seq": 1},
                             "flow-other": {"text": "fresh work", "ts": stamp_ago(0.1), "seq": 2}}
truth["room"]["messages"] = [{"id": "result-builder", "sender": "builder", "recipient": "flow-attempt", "message_kind": "handoff",
    "pending_for": ["flow-attempt"], "ts": stamp,
    "text": "Call exit=1; parent acceptance pending.\n## Output\n\nThe worker guard stopped this run: protected state changed (gitmeta)."}]
truth["room"]["call_results"] = {"builder": {"exit": 1, "ts": stamp, "seq": 5, "read": False}}
for attempt in truth["attempts"]["active_recent"]:
    attempt["state"] = {"builder": "failed", "researcher": "review", "explorer": "review"}.get(attempt["attempt_id"], attempt["state"])
for glyphs, dot, ending in ((True, "·", "→"), (False, "/", "->")):
    board = render(truth, "codex", 157, 40, view="graph", unicode=glyphs, main_attempt="flow-attempt", navigation={})
    assert "Now: 방 범위 선언 %s 6h ago" % dot in board and "Now: Now:" not in board, board
    assert "6h ago (stale)" in board and board.count("(stale)") == 1 and "fresh work" not in board, board
    assert "guard stop" in board and "failed (exit 1)" not in board, board
    assert "Needs review (2)" in board, board
nonguard = deepcopy(truth)
nonguard["room"]["messages"][0]["text"] = "Call exit=1; parent acceptance pending."
assert "failed (exit 1)" in render(nonguard, "codex", 157, 40, view="graph", main_attempt="flow-attempt", navigation={})
# The tree never doubles "Now:" and a main with no readings gets one usage line instead of a box.
tree_board = render(truth, "codex", 157, 40, view="tree", navigation={})
assert "Now: 방 범위 선언" in tree_board and "Now: Now:" not in tree_board, tree_board
assert "Model use, last 8 calls" in tree_board, tree_board
bare = deepcopy(truth)
bare.pop("operations")
bare.pop("provider_status", None)
bare_board = render(bare, "codex", 157, 40, view="tree", navigation={})
assert bare_board.splitlines()[1] == "Claude -- │ Codex --" and "USAGE" not in bare_board, bare_board
from dashboard_projection import fit, use_unicode
use_unicode(True)
assert fit("abcdefghij", 6) == "abcde…"
use_unicode(False)
assert fit("abcdefghij", 6) == "abc..."
use_unicode(True)
from panel_view import activity
assert activity("waiting_approval") == "⚑" and activity("waiting_approval", unicode=False) == "A" and activity("waiting_input") == "?"
# A failed call marks its own main's window with "! "; a handled failure or another main's call does not.
alarm = deepcopy(flow)
assert not panel.main_needs_attention(alarm, "flow-attempt")
next(a for a in alarm["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = "failed"
assert panel.main_needs_attention(alarm, "flow-attempt") and not panel.main_needs_attention(alarm, "flow-other")
alarm["room"]["messages"] = [{"id": "result-builder", "sender": "builder", "recipient": "flow-attempt", "message_kind": "handoff", "pending_for": []}]
assert not panel.main_needs_attention(alarm, "flow-attempt")
# A handled failure stays handled once later mail evicts its handoff from the message window.
from panel_view import inbox_items
assert not any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
alarm["room"]["messages"] = []
assert panel.main_needs_attention(alarm, "flow-attempt")
assert any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
alarm["room"]["call_results"] = {"builder": {"exit": 1, "read": True}}
assert not panel.main_needs_attention(alarm, "flow-attempt")
assert not any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
for state in ("failed", "timed_out", "abandoned", "orphaned", "review", "blocked", "waiting_approval"):
    failure = deepcopy(alarm)
    next(a for a in failure["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = state
    assert any(i["action"] == ("result", "builder") for i in inbox_items(failure)) == (state in {"review", "waiting_approval"}), state
    member = next(m for m in graph_text.nodes(failure) if m["participant"] == "builder")
    assert (graph_text.call_classifier(failure)(member) == "past") == (state not in {"review", "blocked", "waiting_approval"}), state
# Root key cleanup removes only the retired OMS window commands, even when inspection fails.
legacy_f6 = "bind-key -T root F6 if-shell -F '#{@oms_panel_repo}' previous-window 'send-keys F6'\n"
legacy_f7 = "bind-key -T root F7 if-shell -F '#{@oms_panel_repo}' " + shlex.quote(
    "next-window" + " ; if-shell -F '#{==:#{@oms_panel_native_pane},}' next-window" * 2) + " 'send-keys F7'\n"
for f6_binding, f7_binding, inspect_error, removed in (
        (legacy_f6, legacy_f7, None, {"F6", "F7"}),
        ("bind-key -T root F6 next-window\n", legacy_f7, None, {"F7"}),
        (legacy_f6.replace("previous-window", "run-shell 'echo previous-window'"), "", None, set()),
        (legacy_f6.replace("previous-window", "'previous-window ; display-message user'"), "", None, set()),
        (legacy_f6.replace("send-keys F6", "display-message user"), "", None, set()),
        ("bind-key -T root F6 if-shell -F 'unterminated", "", None, set()),
        (legacy_f6, legacy_f7, 1, {"F7"}),
        (legacy_f6, legacy_f7, OSError("unavailable"), {"F7"}),
        (legacy_f6, legacy_f7, subprocess.TimeoutExpired("tmux", 3), {"F7"})):
    removed_keys = set()
    def key_run(argv, **kwargs):
        if argv[1] == "list-keys":
            key = argv[-1]
            if key == "F6" and inspect_error is not None:
                if isinstance(inspect_error, Exception):
                    raise inspect_error
                return subprocess.CompletedProcess(argv, inspect_error, f6_binding, "")
            binding = {"F6": f6_binding, "F7": f7_binding}.get(key, "")
            return subprocess.CompletedProcess(argv, 0 if binding else 1, binding, "")
        if argv[1] == "unbind-key":
            assert argv[2:4] == ["-T", "root"], argv
            removed_keys.add(argv[-1])
        return subprocess.CompletedProcess(argv, 0, "", "")
    with patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel.subprocess, "run", side_effect=key_run):
        panel.bind_main_keys()
    assert removed_keys == removed, (f6_binding, inspect_error, removed_keys, removed)
# An attention-marked window name keeps the geometry fields and the split text apart.
with patch.object(panel, "tmux_command", side_effect=lambda *a: ["x"]), patch.object(panel.subprocess, "run",
        return_value=subprocess.CompletedProcess([], 0, "1 0 2 100 50 100 25\t! codex\ttop:30\n", "")):
    assert panel.panel_geometry() == (True, False, 2, (100, 50), (100, 25), "top:30")
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
with patch.object(panel.subprocess, "run", TmuxStub("codex")):
    for flags in ((False, True), (True, True), (False, False)):
        panel.mark_window(*flags)
assert renames == ["? codex", "! codex", "codex"], renames
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
sent_board = render(same_model, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                    navigation={"dismissed": True, "tab": "messages"})
assert "Sol 6 worker → Sol 6 main  · Failed (exit 1)." in sent_board, sent_board
# A finished call stays reachable from its main's detail: its team row selects it.
done_flow = deepcopy(flow)
for attempt in done_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
team_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt", navigation=team_nav)
row = next(h for h in team_nav["hits"] if h["action"] == ("result", "builder"))
assert choose(("click", row["x1"], row["y"]), team_nav) is None and team_nav["preview"] == {"target": ("result", "builder")}
assert "WORKER / Sonnet 5.5" in render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
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
assert not any(line.startswith("Scope:") for line in lines)
scoped_main = graph_text.detail(dict(main, owns=["scripts/lib", "docs"]), {}, 76, people, {}, everyone={"flow-main": team})[0]
assert "Scope: scripts/lib, docs" in scoped_main, scoped_main
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
assert choose(("enter",), full_nav) is None and full_nav["preview"]["report"] is full_recorded
same_hit = {"y": 1, "x1": 1, "x2": 9, "action": ("result", "builder"), "preview": True}
same_nav = dict(full_nav, hits=[same_hit])
assert choose(("click", 2, 1), same_nav) is None and same_nav["preview"]["report"] is full_recorded
assert choose(("click", 2, 1), dict(same_nav, hits=[dict(same_hit, action=("result", "other"))],
                                    items=[("result", "other")])) is None
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
counted["room"]["participants"][0]["label"] = "Native task not recorded"
counted["room"]["messages"] = [{"sender": "flow-other", "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 180)),
                                "targets": ["flow-main"], "text": "Call exit=0; parent acceptance pending.\nRebasing the parser"},
                               {"id": "result-explorer", "sender": "explorer", "recipient": "flow-attempt", "message_kind": "handoff", "targets": ["flow-main"], "pending_for": [], "text": "Call exit=1;"}]
for attempt in counted["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "explorer":
        attempt["state"] = "failed"
picture = render(counted, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"dismissed": True})
first_line = picture.splitlines()[0]  # row 1 is the header, row 2 the usage line
assert "1 unread for live participants" in first_line and re.search(r"\d\d:\d\d$", first_line), first_line
# Calls in review or blocked have ended; broadcasts still pending for them are not unread anyone can act on.
ended_members = [{"participant": "um", "role": "main"}, {"participant": "ur", "role": "worker", "parent": "um", "state": "review"},
                 {"participant": "ub", "role": "worker", "parent": "um", "state": "blocked"},
                 {"participant": "uw", "role": "worker", "parent": "um", "state": "working"}]
ended_pairs = {"room": {"pairs": [{"recipient": who, "pending": n} for who, n in (("ur", 40), ("ub", 7), ("uw", 2), ("um", 1))]}}
assert graph_text.live_unread(ended_pairs, ended_members, lambda m: False) == 3
assert "no status yet" in picture and "Latest sent" not in picture, picture
assert "explorer" not in picture and "Past work (1)" in picture, picture
pic_rows = picture.splitlines()
goal_at = next(i for i, l in enumerate(pic_rows) if l.startswith("[ GOAL · none · oms agent-plan init --goal TEXT"))
main_row = next(i for i, l in enumerate(pic_rows) if "MAIN / " in l)
assert pic_rows[0].startswith("OMS · ") and ("week" in pic_rows[1] or "Claude --" in pic_rows[1]) and 2 == goal_at < main_row, picture
assert all(l.strip() for l in pic_rows[goal_at:main_row]), picture
assert "LANDING" not in picture, picture
no_plan_nav = {"dismissed": True}
render(counted, "codex", 157, 34, view="graph", main_attempt="flow-attempt", navigation=no_plan_nav)
assert {"y": 3, "x1": 1, "x2": 157, "action": ("tab", "plan")} in no_plan_nav["hits"], no_plan_nav["hits"][:4]
ascii_none = render(counted, "codex", 100, 34, view="graph", main_attempt="flow-attempt", unicode=False,
                    navigation={"dismissed": True}).splitlines()[2]
assert ascii_none.startswith("[ GOAL / none / oms agent-plan init --goal TEXT") and all(ord(c) < 128 for c in ascii_none), ascii_none
for land in ({"active": True, "sha": "abcdef0123", "step": "ci", "minutes": 7},):
    landing = deepcopy(counted)
    landing["land"] = land
    shown = render(landing, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                   navigation={"dismissed": True})
    assert any(l.startswith("LANDING abcdef0 · ci · 7m") for l in shown.splitlines()[3:8]), shown
    landing["land"]["active"] = False
    assert "LANDING" not in render(landing, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                                   navigation={"dismissed": True})
declared_board = deepcopy(counted)
declared_board["room"]["statuses"] = {"flow-main": {"text": "Fixing the parser", "ts": "2026-10-07T09:05:00Z", "seq": 3}}
declared_picture = render(declared_board, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"dismissed": True})
assert "Now: Fixing the parser" in declared_picture and "Latest sent:" not in declared_picture, declared_picture
declared_picture = render(declared_board, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                 navigation={"preview": {"target": ("chat", "flow-main"), "report": {}}})
assert re.search(r"Now: Fixing the parser \(\d\d:\d\d\)", declared_picture), declared_picture
# A board too small for the box keeps the one-row tab strip: its names switch tabs, and no hint names a closed box.
for glyphs in (True, False):
    strips = 0
    for rows_high in range(16, 33):
        small_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
        small = render(counted, "codex", 80, rows_high, view="graph", main_attempt="flow-attempt", navigation=small_nav, unicode=glyphs)
        tab_hits = [h for h in small_nav["hits"] if h["action"][0] == "tab"]
        tab_hits = [h for h in tab_hits if h["y"] == max(t["y"] for t in tab_hits)]
        if len(tab_hits) < 4:
            continue
        small_rows = small.split("\n")
        strip_y = tab_hits[0]["y"]
        assert len(small_rows) == rows_high and {h["y"] for h in tab_hits} == {strip_y}, (rows_high, len(small_rows), tab_hits, small)
        assert [h["action"][1] for h in tab_hits] == ["detail", "plan", "debate", "messages"], (rows_high, tab_hits)
        for h in tab_hits:
            assert small_rows[strip_y - 1][h["x1"] - 1:h["x2"]].strip("[] ").startswith(
                {"detail": "Detail", "plan": "Plan", "debate": "Debate", "messages": "Messages"}[h["action"][1]]), small
        if not small_rows[strip_y].startswith(("│", "|")):
            strips += 1
            assert "Esc Close" not in small_rows[-1] and "f Full" not in small_rows[-1] and (glyphs or not any(c in small for c in "╭╮╰╯│─")), small
    assert strips, "no height showed the bare tab strip"
# An answered advisor or reviewer stays above its main for half an hour, then counts as Past work.
import time as linger_clock
for age, kept in ((60, True), (3600, False)):
    answered = deepcopy(flow)
    answered["room"]["participants"] = [p for p in answered["room"]["participants"] if p.get("role") == "main"
                                        or p["participant"] == "reviewer"]
    for attempt in answered["attempts"]["active_recent"]:
        if attempt["attempt_id"] == "reviewer":
            attempt["state"] = "done"
    answered["room"]["call_results"] = {"reviewer": {"exit": 0, "read": True, "seq": 9, "ts": linger_clock.strftime(
        "%Y-%m-%dT%H:%M:%SZ", linger_clock.gmtime(linger_clock.time() - age))}}
    answered_lanes = render(answered, "codex", 160, 50, view="graph", navigation={"dismissed": True})
    assert ("REVIEWER / " in answered_lanes and "Past work" not in answered_lanes) is kept, answered_lanes
    assert ("Past work (1)" in answered_lanes) is not kept, answered_lanes
    # Attention-only views still hide settled calls, lingering or not.
    assert "REVIEWER / " not in render(answered, "codex", 160, 50, view="graph", attention_only=True,
                                    navigation={"dismissed": True}), answered_lanes
lonely = deepcopy(flow)
lonely["room"]["participants"] = [p for p in lonely["room"]["participants"] if p["participant"] == "flow-main"]
alone = render(lonely, "codex", 120, 30, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
assert "Advisors: none active" in alone and "Workers: none running" in alone, alone
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
    # The selected main's mail is read in the bottom Detail tab; another main's mail never shows.
    assert "CHECK FIXTURE FIRST" in drawn and "PRIVATE OTHER MAIL" not in drawn, drawn
    assert "no task title yet" in drawn and "Native task not recorded" not in drawn
    for hit in nav["hits"]:
        # Card hits stay inside the card; the full-width detail area is a separate target.
        if hit["action"] == ("chat", "flow-main") and hit["y"] > 6 and not hit.get("preview"):
            assert hit["x1"] > 1 and hit["x2"] < columns, hit
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
usage_row = connected.splitlines()[1]
assert "Claude" in usage_row and "Codex" in usage_row and connected.count("Codex --") + connected.count("Codex week") == 1, connected
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
assert "Old plan · idle 44 days · 0 of 6 tasks verified" in speech and "Model use, last 8 calls" in speech, speech
assert "Model use, last 8 calls" in speech and "Sol 6 1.1m tokens ×1" in speech, speech
assert "unread for live participants" in speech, speech
for stale in ("PROVIDER LIMITS", "(sel)", " tok / ", "Plan:"):
    assert stale not in speech, (stale, speech)
assert "status unknown" in speech and "no lifecycle record" not in speech, speech
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
ended_text = render(ended, "codex", 120, 120, view="tree",
                    navigation={"expanded_groups": {("past", "flow-main")}})
assert "status unknown" in ended_text and "failed (exit 1)" in ended_text, ended_text
assert "Old job 03" in ended_text and ended_text.count("finished") == 12, ended_text
# A worker that hit its wall clock says how to continue; later rounds of one task are numbered.
timed = deepcopy(ended)
timed["room"]["call_results"]["old-00"]["exit"] = 124
timed_text = render(timed, "codex", 120, 120, view="tree", navigation={})
assert "timed out · continue with --continue" in timed_text and "c continue" not in timed_text, timed_text
from room_view import nodes as room_nodes
rounded = deepcopy(ended)
rounded["attempts"] = {"active_recent": [], "recent": [
    {"attempt_id": "a-%s" % ident, "state": "failed", "task_id": "same-task",
     "panel": {"role": "worker", "room_participant": ident, "room_id": rounded["room"].get("id")}}
    for ident in ("old-00", "old-01")]}
numbered = {m["participant"]: m for m in room_nodes(rounded)}
assert numbered["old-00"]["round"] == 1 and numbered["old-01"]["round"] == 2 and numbered["old-01"]["continues"] == "old-00", numbered
rows = [line for line in ended_text.splitlines() if ("Luna" in line or "Sonnet" in line) and ("finished" in line or "failed" in line or "running" in line or "no record" in line)]
order = [0 if "finished" in r else 2 if "running" in r else 1 for r in rows]
first_finished = order.index(0)
assert order[first_finished:first_finished + 12] == [0] * 12 and 1 in order[:first_finished] and 2 in order[:first_finished], rows
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
    if command[2:3] == ["land"]:
        return subprocess.CompletedProcess(command, 0, json.dumps({"active": True, "sha": "abc1234", "step": "gate", "minutes": 2}), "")
    assert command[2:3] == [str(panel.ENTRY)] or "agent-events" in command, command
    return subprocess.CompletedProcess(command, 0, json.dumps(listed), "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "results", return_value={"rows": []}), \
        patch.object(panel, "outcomes", return_value={}), \
        patch.object(panel.subprocess, "run", side_effect=fake_run):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert got["land"] == {"active": True, "sha": "abc1234", "step": "gate", "minutes": 2}, got.get("land")
assert got["room_attempts"] == {"gone-review": {"state": "review", "attempt_id": "a-new",
    "updated_at": "2026-01-02T00:00:00Z", "task_id": "t-a-new"}}, got.get("room_attempts")
# An older review call found only in room_attempts still gets the owner's recorded decision.
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "outcomes", side_effect=lambda repo, ids, _shared=False, _tick=None: {"t-a-new": "accepted"} if "t-a-new" in ids else {}), \
        patch.object(panel.subprocess, "run", side_effect=fake_run):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert got.get("finalized") == {"t-a-new": "accepted"}, got.get("finalized")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired("agent-events", 5)):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert "room_attempts" not in got and got["room"]["participants"] == members, got
# Admission belongs to the shared snapshot, including watch reads, not only the control menu.
admission_room = {"id": "admission-room", "participants": [
    {"participant": "admission-main", "role": "main", "joined": True},
    {"participant": "admission-worker", "role": "worker", "parent": "admission-main", "joined": True}]}
admission_state = {"repo": {"name": "x"}, "attempts": {"recent": [{
    "attempt_id": "admission-worker", "task_id": "admission-task", "state": "done",
    "panel": {"room_id": "admission-room", "room_participant": "admission-worker", "role": "worker", "access": "write"}}]}}
for pending_tasks in ({"admission-task"}, set()):
    with patch.object(panel, "dashboard", side_effect=lambda *a, **k: (deepcopy(admission_state), 0)), \
            patch.object(room_module, "status", side_effect=lambda *a: deepcopy(admission_room)), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel_results, "pending_admissions", return_value=pending_tasks) as pending, \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "[]", "")), \
            patch("panel_metrics.collect", return_value={}), \
            patch.object(panel, "other_rooms", return_value=([], False)), \
            patch.dict(os.environ, {"OMS_ROOM_ID": "admission-room"}):
        watched = panel.read_panel(Path("."), "codex", "graph", False)[0]
        collected, _ = panel.snapshot(Path("."), "admission-room")
    expected = ["admission-worker"] if pending_tasks else []
    assert pending.call_count == 2 and all(call.args[1] == {"admission-task"} for call in pending.call_args_list)
    for report in (watched, collected):
        assert report.get("awaiting_admission") == expected, report
        worker = next(m for m in graph_text.nodes(report) if m["participant"] == "admission-worker")
        assert graph_text.call_classifier(report)(worker) == ("review" if expected else "past")
# Record-only admission reads every retained call, without loading patch or answer bytes.
admission_records = [
    {"kind": "delegate", "task_id": ident, "patch": ".oms/artifacts/change.patch", "patch_sha256": "a" * 64}
    for ident in ("pending", "admitted", "landed", "many-calls")]
admission_records += [{"kind": "patch-admit", "task_id": "admitted"},
                      {"kind": "patch-land", "task_id": "landed"}]
admission_records += [{"kind": "call", "task_id": "many-calls"} for _ in range(35)]
admission_records += [{"kind": "call", "task_id": "no-patch"}]
with patch.object(panel_results, "_records", return_value=([], admission_records)), \
        patch.object(panel_results, "_read", side_effect=AssertionError("admission read artifact bytes")), \
        patch("panel_cache.read_shared", side_effect=lambda repo, key, collect: collect()):
    assert panel_results.pending_admissions(Path("."), {"pending", "admitted", "landed", "many-calls", "no-patch", "missing"}) == {"pending", "many-calls"}
    assert panel_results.pending_admissions(Path("."), {"../bad"}) == set()
    # Only tasks without any retained rows are unknown at the coverage boundary.
    with patch.object(panel_results, "_records", return_value=([{}] * 300, admission_records)):
        assert panel_results.pending_admissions(Path("."), {"pending", "missing", "admitted", "landed", "no-patch"}) == {"pending", "missing"}
    with patch.object(panel_results, "_records", return_value=([], admission_records + [{}] * (1000 - len(admission_records)))):
        assert panel_results.pending_admissions(Path("."), {"pending", "missing", "admitted", "landed", "no-patch"}) == {"pending", "missing"}
# Gather the whole linked scope before applying the inbox cap; keep member order.
admission_members = [{"participant": "w-%s" % n, "role": "worker", "access": "write",
                      "state": "done", "task_id": "t-%s" % n} for n in range(panel.INBOX_LIMIT + 2)]
with patch("room_view.nodes", return_value=admission_members), \
        patch.object(panel_results, "pending_admissions", return_value={m["task_id"] for m in admission_members}) as pending:
    assert panel.awaiting_admission(Path("."), {}) == [m["participant"] for m in admission_members[:panel.INBOX_LIMIT]]
    assert pending.call_count == 1 and pending.call_args.args[1] == {m["task_id"] for m in admission_members}
# Both plan collectors retain tasks beyond their former 20/40 caps for pagination.
long_tasks = [{"id": "long-%02d" % i, "title": "Plan task %s" % i,
               "state": "ready" if i == 44 else "done"} for i in range(45)]
long_plan_repo = Path(sys.argv[2]) / "long-plan"
(long_plan_repo / ".oms/plan").mkdir(parents=True)
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": long_tasks}), encoding="utf-8")
from dashboard_projection import plan_tasks
assert len(plan_tasks(str(long_plan_repo))) == 45
projected = plan_tasks(str(long_plan_repo))
assert projected[0] == {"id": "long-00", "title": "Plan task 0", "state": "done", "claimed_by": None}, projected[0]
completion_tasks = [dict(row) for row in long_tasks]
completion_tasks[0]["completion_kind"] = "research-accepted"
completion_tasks[1]["completion_kind"] = "satisfied-by"
completion_tasks[2]["completion_kind"] = {"kind": "research-accepted"}
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": completion_tasks}), encoding="utf-8")
projected = plan_tasks(str(long_plan_repo))
assert projected[0]["completion_kind"] == "research-accepted" and projected[1]["completion_kind"] == "satisfied-by", projected[:2]
assert "completion_kind" not in projected[2] and "completion_kind" not in projected[3], projected[2:4]
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": long_tasks}), encoding="utf-8")
def long_plan_run(command, **kwargs):
    data = {"tasks": long_tasks} if "agent-plan" in command else []
    return subprocess.CompletedProcess(command, 0, json.dumps(data), "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}, "plan": {"present": True}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "long-plan", "participants": []}), \
        patch.object(panel.subprocess, "run", side_effect=long_plan_run):
    long_report, _ = panel.snapshot(long_plan_repo, "long-plan")
assert long_report["room"]["repo_tasks"] == long_tasks
def slow_plan_run(command, **kwargs):
    if "agent-plan" in command:
        raise subprocess.TimeoutExpired(command, kwargs.get("timeout"))
    return subprocess.CompletedProcess(command, 0, "[]", "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}, "plan": {"present": True}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "long-plan", "participants": [{"participant": "m1"}]}), \
        patch.object(panel.subprocess, "run", side_effect=slow_plan_run):
    slow_report, _ = panel.snapshot(long_plan_repo, "long-plan")
assert slow_report["room"].get("participants") and slow_report["room"].get("repo_tasks_unavailable"), slow_report["room"]
assert "error" not in slow_report["room"], "a slow plan list must not hide the room and its main"
ended_nav = {"expanded_groups": {("past", "flow-main")}}
render(ended, "codex", 120, 120, view="tree", navigation=ended_nav)
shown = list(dict.fromkeys(h["action"][1] for h in sorted(ended_nav["hits"], key=lambda h: h["y"]) if h["action"][0] == "result"))
assert shown == [i[1] for i in ended_nav["items"] if i[0] == "result"] and shown.index("old-13") > shown.index("builder"), shown
team_lines, _ = graph_text.detail(next(m for m in graph_text.nodes(ended) if m["participant"] == "flow-main"), {}, 120,
    graph_text.nodes(ended), ended["room"], everyone={"flow-main": [m for m in graph_text.nodes(ended)
                                                                  if m.get("parent") == "flow-attempt"]})
team_rows = [line for line in team_lines if "Old job" in line or "한글 작업 제목 / builder" in line]
assert "finished" in team_rows[0] and "running" in team_rows[-1] and "failed (exit 1)" in team_rows[-3], team_rows
assert any("status unknown" in line for line in team_lines) and "failed (exit 1)" in "\n".join(team_lines), team_lines
# Attempt evidence beats the result message, and the header clock carries date and weekday.
beaten = deepcopy(ended)
beaten["attempts"]["active_recent"].append({"attempt_id": "old-00", "state": "working",
    "panel": {"room_id": "flow-room", "room_participant": "old-00", "role": "worker"}})
assert next(m for m in graph_text.nodes(beaten) if m["participant"] == "old-00")["state"] == "working"
assert re.search(r"\d\d-\d\d (Mon|Tue|Wed|Thu|Fri|Sat|Sun) \d\d:\d\d$", graph_text.render_graph(flow, 120, 40).splitlines()[0])
board_text = graph_text.render_graph(words, 120, 40)
assert "Plan:" not in board_text, board_text
# An active plan puts its goal and progress below the header and usage on both single-main boards and the held task on the main's card.
shown_main = next(m["participant"] for m in graph_text.nodes(words) if m.get("role") == "main")
words["provider_status"] = {"claude": {"weekly_used": 12, "context_left": 80}, "codex": {"weekly_used": 30}}
words["plan"] = {"present": True, "goal": "Ship the shared plan board", "task_count": 9, "idle_days": 3,
                 "by_state": {"done": 6, "review": 1, "running": 2},
                 "tasks": [{"id": "t7", "title": "Wire", "state": "running", "claimed_by": shown_main}]}
banner_nav = {}
active = graph_text.render_graph(words, 120, 40, navigation=banner_nav)
active_rows = active.splitlines()
top_at = next(i for i, l in enumerate(active_rows) if l.startswith("╭─ GOAL"))
top = active_rows[top_at]
assert "OMS · " in active_rows[0] and top_at == 2 and "week" in active_rows[1], active
assert top.endswith("▕██████░░░░▏ 6/9 verified · 1 review · 2 claimed ─╮") and display_width(top) == 120, top
assert active_rows[top_at + 1].startswith("│ Ship the shared plan board ") and active_rows[top_at + 2].startswith("╰"), active
assert "Plan:" not in active and "Task: t7 · running" in active, active
for row in range(top_at + 1, top_at + 4):
    assert {"y": row, "x1": 1, "x2": 120, "action": ("tab", "plan")} in banner_nav["hits"], banner_nav["hits"][:5]
# Header, usage and the goal box are the first rows of the graph, the tree and the control window at every size.
for order_width, order_height in ((100, 30), (100, 40), (160, 60)):
    order_rows = graph_text.render_graph(words, order_width, order_height).splitlines()
    first_main = next(i for i, l in enumerate(order_rows) if "MAIN / " in l)
    assert order_rows[0].startswith("OMS · ") and "Claude" in order_rows[1] and "Codex" in order_rows[1] \
        and order_rows[2].startswith("╭─ GOAL") and 4 < first_main and "BETWEEN MAINS" not in "\n".join(order_rows), order_rows
    tree_order = render(words, "codex", order_width, order_height, view="tree", navigation={}).splitlines()
    assert tree_order[0].startswith("OMS / ") and "Claude" in tree_order[1] and tree_order[2].startswith("╭─ GOAL"), tree_order[:4]
    inbox_order = render(words, "codex", order_width, order_height, menu=True, view="tree", navigation={}).splitlines()
    assert "OMS control panel" in inbox_order[0] and "Codex" in inbox_order[1] and inbox_order[2].startswith("╭─ GOAL"), inbox_order[:4]
    for provider in ("claude", "codex"):
        # Both providers stay on the row whichever main the window belongs to; one without a reading reads "--".
        one = deepcopy(words)
        one["provider_status"] = {provider: {"weekly_used": 41, "context_left": 70}}
        usage_row = graph_text.render_graph(one, order_width, order_height).splitlines()[1]
        other = "Codex" if provider == "claude" else "Claude"
        assert "%s week 41%% · ctx 70%%" % provider.title() in usage_row and other + " --" in usage_row, usage_row
        assert render(one, provider, order_width, order_height, view="tree", navigation={}).splitlines()[1] == usage_row
# Short boards keep all three pinned rows: the box shrinks to one row before anything is dropped.
for short in (12, 14, 18):
    short_rows = graph_text.render_graph(words, 100, short).splitlines()
    assert short_rows[0].startswith("OMS · ") and "week" in short_rows[1] and short_rows[2].startswith("[ GOAL  Ship") \
        and short_rows[2].endswith(" 6/9 verified · 1 review · 2 claimed ]"), short_rows[:4]
long_goal = deepcopy(words)
long_goal["plan"]["goal"] = "Ship the shared plan board with every main claiming work from it " * 2
wrapped_top = graph_text.render_graph(long_goal, 80, 40).splitlines()
wrap_at = next(i for i, l in enumerate(wrapped_top) if l.startswith("╭─ GOAL"))
assert "OMS · " in wrapped_top[0] and wrapped_top[wrap_at].endswith("6/9 verified · 1 review · 2 claimed ─╮") \
    and wrapped_top[wrap_at + 1].startswith("│ Ship the shared") and wrapped_top[wrap_at + 2].startswith("│ ") \
    and wrapped_top[wrap_at + 3].startswith("╰"), wrapped_top[:wrap_at + 4]
# On a narrow board the claimed count goes first; work waiting for review stays visible next to the bar.
narrow_top = next(l for l in graph_text.render_graph(words, 60, 40).splitlines() if l.startswith("╭─ GOAL"))
assert narrow_top.endswith("6/9 verified · 1 review ─╮") and "claimed" not in narrow_top, narrow_top
plain_board = graph_text.render_graph(words, 100, 40, unicode=False)
plain_goal = next(l for l in plain_board.splitlines() if l.startswith("+- GOAL"))
assert "[######....] 6/9 verified" in plain_goal and all(ord(ch) < 128 for ch in "".join(plain_board.splitlines()[:6])), plain_goal
tree_active = render(words, "codex", 120, 60, view="tree", navigation={})
tree_rows = tree_active.splitlines()
tree_goal = next(i for i, l in enumerate(tree_rows) if l.startswith("╭─ GOAL"))
assert tree_goal == 2 and tree_rows[0].startswith("OMS / ") and "USAGE" in "\n".join(tree_rows[tree_goal:]) and "Plan:" not in tree_active, tree_active
assert "Old plan" not in tree_active
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
    # Earlier mains stay on record; the reading belongs to the live, then newest, one.
    reading = collect(tree_room)
    assert reading["codex"]["participant"] == next(m["participant"] for m in graph_text.nodes(tree_room)
                                                   if m.get("role") == "main" and m.get("provider") == "codex"), reading
    reading = collect(tree_room, main_attempt=main_attempts["codex"])
    assert reading["codex"]["participant"] == main_attempts["codex"], reading
pi_report = {"room": {"id": "pi-room", "participants": [{"participant": "pi-main", "provider": "codex",
    "role": "main", "joined": True, "model": "gpt-6.1-sol"}]}, "attempts": {"active_recent": [{
    "attempt_id": "pi-main", "state": "working", "panel": {"role": "main", "harness": "pi", "room_id": "pi-room"}}]}}
pi_fallback = dict(pi_report, room_attempts={"pi-main": pi_report["attempts"]["active_recent"][0]},
                   attempts={"active_recent": [], "recent": []})
from room_view import model_name
for candidate in (pi_report, pi_fallback):
    with patch("panel_chats.windows", return_value={"pi-main": [{"pane": "%42"}]}), \
            patch("panel_chats.current_models", return_value={}), \
            patch("panel_metrics.pane_reading", side_effect=AssertionError("Pi was parsed as native Codex")):
        reading = collect(candidate, repo=project)
        assert reading["codex"]["context_left"] is None and reading["codex"]["weekly_used"] is None
    assert model_name(nodes(candidate)[0]).startswith("Pi / ")
with patch("panel_metrics.subprocess.run", side_effect=[
        subprocess.CompletedProcess([], 0, "99\t0\n"),
        subprocess.CompletedProcess([], 0, "GPT-6-Sol · Context 83% left · weekly 61% left\n")]) as pane_probe:
    assert pane_reading({"pane": "%42"}, "codex") == {"context_left": 83, "weekly_used": 39, "source": "TUI"}
    assert pane_probe.call_args.args[0][-4:] == ["-S", "91", "-E", "98"]
# The smaller task panel preserves frame-local navigation and never upgrades
# native process exit to owner acceptance.
summary_room = deepcopy(tree_room)
# Earlier calls can evict terminal mains from the bounded dashboard history.
summary_main = next(p for p in summary_room["room"]["participants"] if p["role"] == "main" and p["joined"])
summary_room["attempts"]["recent"].append({"attempt_id": "summary-exited-main", "state": "done",
    "panel": {"role": "main", "room_id": shared, "room_participant": summary_main["participant"]}})
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
assert "Claude W 100% · C 0%" in render(dict(usage_board, provider_status={"claude": {
    "weekly_used": 100, "context_left": 0}}), "claude", 32, 28, view="summary")
empty_room = dict(usage_board, room={"id": "empty", "participants": []})
# The control menu is an inbox: empty rooms onboard, failures and waits list under "Needs you", one key acts.
onboarding = render(empty_room, "codex", 100, 28, view="tree", menu=True)
assert "Press 1 (Claude) or 2 (Pi) to start the sole main" in onboarding and "Nothing needs you right now" in onboarding
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
assert "Needs you" in inbox and "Nothing needs you" not in inbox and "Press 1 (Claude)" not in inbox, inbox
assert "Sol 6.1 worker for Opus 5.5 · failed: Fix parser" in inbox, inbox
assert "waiting for approval: Run migration" in inbox and "patch awaits admission: Write patch" in inbox, inbox
assert "Opus 5.5 main · to all: Which branch?" in inbox and "\033" not in inbox, inbox
assert [h["action"] for h in nav["hits"]] == [("result", "w2"), ("result", "w1"), ("result", "w3"), ("chat", "m1")], nav
assert nav["items"][0] == ("result", "w2") and nav["selected"] == ("result", "w2")
choose(("down",), nav)
assert nav["selected"] == nav["items"][1]
# Two newer broadcasts to the same main remain separate rows and keyboard choices.
duplicate_room = deepcopy(inbox_room)
duplicate_room["room"]["participants"] = duplicate_room["room"]["participants"][:2]
duplicate_room["attempts"]["active_recent"] = duplicate_room["attempts"]["active_recent"][:2]
duplicate_room["room"]["messages"] = [dict(inbox_room["room"]["messages"][0], id="n%s" % number,
    text="Message %s" % number, ts="2026-10-07T00:0%s:00Z" % (number + 2)) for number in (1, 2)]
for columns, rows in ((100, 28), (24, 18)):
    duplicate_nav = {}
    render(duplicate_room, "codex", columns, rows, menu=True, navigation=duplicate_nav)
    assert duplicate_nav["items"] == [("chat", "m1"), ("chat", "m1"), ("result", "w1")]
    first, second = duplicate_nav["row_ids"][:2]
    assert first != second and duplicate_nav["selected_row"] == first
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected_row"] == second and duplicate_nav["selected"] == ("chat", "m1")
    assert choose(("enter",), duplicate_nav) == ("chat", "m1")
    assert render(duplicate_room, "codex", columns, rows, menu=True, navigation=duplicate_nav)
    assert duplicate_nav["selected_row"] == second
    second_hit = next(hit for hit in duplicate_nav["hits"] if hit["row_id"] == second)
    assert choose(("click", second_hit["x1"], second_hit["y"]), duplicate_nav) == ("chat", "m1")
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected"] == ("result", "w1")
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected_row"] == first
    choose(("scroll", 2, 1, 1), duplicate_nav)
    assert duplicate_nav["selected"] == ("result", "w1")
# The single-main board's control list has no folds: Space, Left and Right change nothing there.
nav["selected"] = ("chat", "m1")
for key in (" ", "left", "right"):
    assert choose((key,), nav) is None and nav["selected"] == ("chat", "m1") and "collapsed" not in nav, key
small_nav = {}
small_inbox = render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav)
assert choose(("scroll", 1, 5, 5), small_nav) is None
assert small_nav["selected"] == small_nav["items"][1]
choose(("pagedown",), small_nav)
assert small_nav["selected"] == small_nav["items"][-1]
scrolled_inbox = render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav)
assert scrolled_inbox != small_inbox and small_nav["selected"] in [hit["action"] for hit in small_nav["hits"]]
assert render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav) == scrolled_inbox
choose(("pagedown",), small_nav)
choose(("scroll", 3, 5, 5), small_nav)
assert small_nav["selected"] == small_nav["items"][-1]
choose(("pageup",), small_nav)
assert small_nav["selected"] == small_nav["items"][1]
choose(("scroll", -20, 5, 5), small_nav)
choose(("pageup",), small_nav)
assert small_nav["selected"] == small_nav["items"][0]
render(inbox_room, "codex", 100, 28, menu=True, navigation=small_nav)
assert small_nav["selected"] == small_nav["items"][0]
empty_nav = {}
render(empty_room, "codex", 100, 8, menu=True, navigation=empty_nav)
for event in (("scroll", 3, 5, 5), ("pageup",), ("pagedown",)):
    assert choose(event, empty_nav) is None and empty_nav["selected"] is None and empty_nav["offset"] == 0
for width, height in ((100, 28), (42, 24), (24, 18), (12, 10)):
    for glyphs in (True, False):
        drawn = render(inbox_room, "codex", width, height, unicode=glyphs, menu=True, navigation={})
        assert len(drawn.splitlines()) <= height - 1 and all(display_width(line) <= width for line in drawn.splitlines()), drawn
        closed_cards(drawn, width, glyphs)
assert "key + Enter" in render(inbox_room, "codex", 100, 28, menu=True, navigation={"line_input": True})
assert panel.menu_input().decode(b"1\033[B2q ") == [("1",), ("down",), ("2",), ("q",), (" ",)]
# Enter on a result row in the control window opens its recorded result; no watch loop is there to read it.
class OpenInput:
    fd, batches = 0, [[("enter",)], [("q",)]]
    def __enter__(self): return self
    def __exit__(self, *exc): return False
    def wait(self, seconds): return self.batches.pop(0)
open_nav = {"items": [("result", "w1")], "selected": ("result", "w1")}
def request_open(repo, action, state, navigation, width):
    navigation["opening"] = action
shown_detail = []
with patch.object(panel, "menu_input", return_value=OpenInput()), \
        patch.object(panel, "show", side_effect=lambda *a, **k: shown_detail.append((open_nav.get("detail") or {}).get("title"))), \
        patch.object(panel, "navigate", side_effect=request_open), \
        patch.object(panel, "read_evidence", side_effect=lambda repo, target: (target, {"rows": [{"calls": [1]}]}, None)):
    panel.read_choice(Path("."), "codex", "ready", "auto", False, open_nav, {})
assert shown_detail[-1] == "RECORDED RESULTS" and "opening" not in open_nav, (shown_detail, open_nav)
# A full reader ignores clicks and Enter without repainting; a wheel step still redraws.
class ReaderInput:
    fd, batches = 0, [[("click", 3, 3), ("enter",)], [("scroll", 3)], [("q",)]]
    def __enter__(self): return self
    def __exit__(self, *exc): return False
    def wait(self, seconds): return self.batches.pop(0)
reader_nav = {"detail": {"target": ("result", "builder")}, "offset": 2}
with patch.object(panel, "menu_input", return_value=ReaderInput()), patch.object(panel, "show") as painted, \
        patch("panel_input.choose", wraps=choose) as chosen:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, reader_nav, {}) == "9"
assert painted.call_count == 2 and reader_nav["offset"] == 0 and not any(
    c.args[0][0] in ("click", "enter") for c in chosen.call_args_list), (painted.call_count, reader_nav)
# A rendered graph preview: a click on plain Detail text repaints nothing; a card click and the wheel still do.
graph_nav = {"preview": {"target": ("chat", "flow-main"), "report": {}}}
render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation=graph_nav)
text_hit = next(h for h in graph_nav["hits"] if h.get("passive"))
card_hit = next(h for h in graph_nav["hits"] if h["action"] == ("chat", "flow-main") and not h.get("passive") and h.get("preview"))
box = next(b for b in graph_nav["bands"] if b["name"] == "detail")
class GraphInput(ReaderInput):
    batches = [[("click", text_hit["x1"], text_hit["y"])], [("click", card_hit["x1"], card_hit["y"])],
               [("scroll", 3, 5, box["y1"] + 1)], [("q",)]]
with patch.object(panel, "menu_input", return_value=GraphInput()), patch.object(panel, "show") as painted, \
        patch.object(panel, "navigate") as moved:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, graph_nav, {}) == "q"
# The first frame, the card click and the wheel paint; the plain-text click does not.
assert painted.call_count == 3 and graph_nav["preview"].get("report") == {} and moved.call_count == 1, (
    painted.call_count, moved.call_args_list)
# Empty and blank Detail body rows have no hit, but still must not trigger a redraw.
empty_graph = {"room": {"id": "empty-graph", "participants": []}}
empty_graph_nav = {"dismissed": True}
empty_graph_frame = render_graph(empty_graph, 100, 30, navigation=empty_graph_nav)
hint_y = next(i for i, line in enumerate(empty_graph_frame.splitlines(), 1)
              if "No call selected" in line)
blank_y = next(i for i, line in enumerate(empty_graph_frame.splitlines(), 1)
               if empty_graph_nav["box"]["y1"] < i < empty_graph_nav["box"]["y2"]
               and line.startswith("│") and line.endswith("│") and not line.strip("│ "))
assert passive_click(("click", 50, hint_y), empty_graph_nav)
assert passive_click(("click", 50, blank_y), empty_graph_nav)
tab_hit = next(h for h in empty_graph_nav["hits"] if h["action"] == ("tab", "plan"))
assert not passive_click(("click", tab_hit["x1"], tab_hit["y"]), empty_graph_nav)
assert choose(("click", tab_hit["x1"], tab_hit["y"]), dict(empty_graph_nav)) == ("tab", "plan")
for default_tab in (None, "bogus"):
    assert passive_click(("click", 50, hint_y), dict(empty_graph_nav, tab=default_tab)), default_tab
for bad_nav, event in (
        (dict(empty_graph_nav, tab="plan"), ("click", 50, hint_y)),
        (dict(empty_graph_nav, surface="tree"), ("click", 50, hint_y)),
        (dict(empty_graph_nav, box={"y1": 2, "y2": 2}), ("click", 50, hint_y)),
        (empty_graph_nav, ("click", 1, hint_y)),
        (empty_graph_nav, ("click", 100, hint_y)),
        (empty_graph_nav, ("click", 50, empty_graph_nav["box"]["y1"]))):
    assert not passive_click(event, bad_nav), (event, bad_nav)
class EmptyGraphInput(ReaderInput):
    batches = [[("click", 50, hint_y)], [("click", 50, blank_y)], [("q",)]]
def draw_empty_graph(*args, **kwargs):
    render_graph(empty_graph, 100, 30, navigation=empty_graph_nav)
with patch.object(panel, "menu_input", return_value=EmptyGraphInput()), \
        patch.object(panel, "show", side_effect=draw_empty_graph) as painted, \
        patch.object(panel, "navigate") as moved:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, empty_graph_nav, {}) == "q"
assert painted.call_count == 1 and moved.call_count == 0, (painted.call_count, moved.call_count)
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
                           ("x" * 40000 + " API_ANSWER_TAIL", True),
                           ("😀" * 9000 + " TAIL", True),
                           ("😀" * 8191 + "ABCD", False),
                           ("😀" * 8191 + "ABCDE", True),
                           ("to" + "ken=" + "s" * 40000 + " RETAINED_TAIL", False)):
    with patch.object(saved, "_records", return_value=([answer_attempt], [answer_record])), \
            patch.object(saved, "_indexed", return_value=("## Output\n\n" + payload +
                "\n\n## Verify\nPRIVATE_VERIFIER_SENTINEL\n\n## Exit\n\n0\n").encode()):
        normalized = saved.results(project, "retained-long-answer")["rows"][0]["calls"][0]
    assert normalized["answer_truncated"] == truncated, normalized
    assert len(normalized["answer"].encode()) <= saved.MAX_RESULT
    assert sample_auth not in normalized["answer"]
    assert "PRIVATE_VERIFIER_SENTINEL" not in normalized["answer"]
    if not truncated:
        if "Answer line 39" in payload:
            assert "Answer line 39" in normalized["answer"] and "API_ANSWER_TAIL" in normalized["answer"]
        if "RETAINED_TAIL" in payload:
            assert "RETAINED_TAIL" in normalized["answer"] and "s" * 40000 not in normalized["answer"]
    elif "😀" in payload:
        assert "TAIL" not in normalized["answer"]
binding = panel.room_instructions("current-room", "stable-main")
assert "room show --id current-room" in binding and "--room current-room" in binding
assert "room updates --id current-room --participant stable-main" in binding
assert "unacknowledged addressed messages" in binding and "--after CURSOR" in binding
assert "--id current-room --participant stable-main --message MESSAGE_ID" in binding
standalone_env = {key: value for key, value in environment.items()
                  if key not in {"OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_PANEL_MAIN_ATTEMPT"}}
with patch.dict(os.environ, standalone_env, clear=True), \
        patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
        patch("sys.stderr", new=io.StringIO()) as standalone_notice:
    assert panel.dispatch(project, "codex", "worker", "light", "read", "investigate",
                          prompt="Inspect source", task_id="standalone-fixture") == 0
    assert "room and main relationship are unrecorded" in standalone_notice.getvalue()
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "inline", "--dry-run"]))["host"] == "inline"
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "herdr", "--dry-run"]))["host"] == "herdr"

# End simulated owners before each real native-start phase; no fixture CLI remains running here.
def end_fixture_mains():
    for row in json.loads(panel.events(project, "list", "--json", output=True)):
        if row.get("tool") == "panel-main" and row.get("terminal") is not True:
            panel.events(project, "transition", "--attempt", row["attempt_id"], "--state", "cancelled")
end_fixture_mains()

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
            try:
                panel.open_native("codex", project)
                raise AssertionError("a second legacy Herdr main was opened")
            except ValueError as error:
                assert "already has a live main" in str(error)
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
            panel.open_native("codex", project)
            layout = next(c["params"] for c in reversed(host_calls) if c["method"] == "layout.apply")
            # Herdr keeps the board as the left pane too.
            assert "tab_id" not in layout and layout["root"]["ratio"] == .28
            for configured, ratio in (("30", .30), ("1", .25), ("99", .35), ("-1", .25),
                                      ("+35", .35), ("garbage", .28), ("28.5", .28), ("2_8", .28)):
                with patch.dict(os.environ, {"OMS_PANEL_BOARD_PERCENT": configured}):
                    panel_host.open_native(project, "codex", layout["root"]["second"]["command"],
                                           layout["root"]["first"]["command"])
                assert host_calls[-1]["params"]["root"]["ratio"] == ratio
            assert layout["root"]["direction"] == "right" and "--watch" in layout["root"]["first"]["command"]
            main_command = layout["root"]["second"]["command"]
            assert str(panel.ENTRY.parent / "panel.sh") in main_command and "--launch" in main_command
            assert layout["root"]["first"]["env"]["OMS_ROOM_ID"] == host_room
            assert layout["root"]["first"]["env"]["OMS_PANEL_VIEW"] == "graph"
            assert not any(c["method"] in {"pane.send_text", "pane.send_input", "server.stop"} for c in host_calls)
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
    # The recorded result sits in the collapsed unlinked "Past work" group: open it first, then click the result.
    group_hit = next(h for h in nav["hits"] if h["action"] == ("group", ("past", "unlinked")))
    nav["expanded_groups"] = {("past", "unlinked")}
    render(actual, "codex", 110, 28, view="tree", navigation=nav)
    hit = next(h for h in nav["hits"] if h["action"][0] == "result")
    # The result opens from a background read; the watcher exits on its own after three snapshots, so a slow background read still lands.
    group_click, result_click = [("\033[<0;8;%sM" % y).encode() for y in (group_hit["y"], hit["y"])]
    before_click = ledger.read_bytes()
    terminal(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
              "--watch", "--view", "tree", "--count", "3", "--no-animation"],
             group_click, b"RECORDED RESULTS", trigger=b"Activity", timeout=60, then=("▾ Past work".encode(), result_click))
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
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.Path, "home", return_value=home), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/launcher"), \
        patch.object(panel_chats.subprocess, "run", side_effect=tmux_navigation):
    chats = panel_chats.catalog(project, shared)
    assert "PRIVATE_TRANSCRIPT_BODY" not in json.dumps(chats)
    native = next(r for r in chats["rows"] if r["participant"] == "chat-sol")
    assert native["uri"] == "codex://threads/" + chat_codex and native["terminal"]["pane"] == "%42", native
    assert next(r for r in chats["rows"] if r["participant"] == "bad-sol")["status"] == "unresolved"
    assert next(r for r in chats["rows"] if r["participant"] == "chat-opus")["session_id"] == chat_claude
    chat_text = panel_chats.text(chats)
    assert "native session: " + chat_claude + " / Claude app sidebar" in chat_text
    assert "codex://threads/" + chat_codex in chat_text
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
end_fixture_mains()
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
# A foreign-room racer must not take the main lease away from the legitimate
# resume. The legitimate native stays active beyond the existing lock wait.
race_log = temporary / "native-resume-race.txt"
race_env = dict(recovery_env, OMS_OPEN_HOLD="14", OMS_OPEN_APPEND_LOG=str(race_log))
race_script = ("import sys\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
               "try: p.run_native('roomtest',Path(sys.argv[2]),resume='fixture-stable-session')\n"
               "except ValueError as e: print(str(e));sys.exit(75)\n")
other_race_room = room.create(project, "parallel-resume-room", "Concurrent resume fixture")
racers = [subprocess.Popen([sys.executable, "-c", race_script, str(root / "scripts/lib"), str(project)],
                           env=dict(race_env, OMS_ROOM_ID=ident), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
          for ident in (other_race_room, shared)]
try:
    outcomes = [r.communicate(timeout=35) for r in racers]
    assert [r.returncode for r in racers] == [75, 0], outcomes
    assert "native session is bound to another room" in outcomes[0][0], outcomes
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
# Main turn presence changes only presentation, including the control graph's main rows.
activity_main = {"participant": "activity-main", "role": "main", "joined": True,
                 "provider": "claude", "model": "fixture-model", "seq": 1}
activity_worker = {"participant": "activity-worker", "role": "worker", "joined": True,
                   "provider": "codex", "model": "gpt-6-sol", "parent": "activity-attempt", "seq": 2}
activity_report = {"collection": {"ok": True}, "room": {"id": "activity-room", "participants": [activity_main, activity_worker]},
                   "attempts": {"recent": [], "active_recent": [
                       {"attempt_id": "activity-attempt", "state": "working", "panel": {"role": "main",
                        "room_id": "activity-room", "room_participant": "activity-main"}},
                       {"attempt_id": "activity-child", "state": "working", "panel": {"role": "worker",
                        "room_id": "activity-room", "room_participant": "activity-worker"}}]}}
activity_row = {"schema": 1, "participant": "activity-main", "room": "activity-room",
                "attempt": "activity-attempt", "state": "busy", "since": 100, "updated_at": 100}
activity_repo = temporary / "main-activity"
activity_path = activity_repo / ".oms/hooks/panel-activity/activity-main.json"
activity_path.parent.mkdir(parents=True)
activity_path.write_text(json.dumps(activity_row))
activity_windows = [["activity-main", "%777", str(activity_repo), "activity-attempt"]]
panel.MAIN_OUTPUT.clear()
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"native prompt", b"")) as capture:
    with patch.object(panel.time, "time", return_value=1000):
        assert "activity-main" in panel.main_activity(activity_repo, [activity_worker] * 129 + [activity_main])
        activity_before = activity_path.read_bytes()
        first = panel.main_activity(activity_repo, [activity_main, activity_worker], activity_windows)
        assert first["activity-main"]["state"] == "busy"
        assert first["activity-main"]["since"] == activity_row["since"]
        assert activity_path.read_bytes() == activity_before, "reading activity must not rewrite hook evidence"
    with patch.object(panel.time, "time", return_value=1005):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows) == first
    with patch.object(panel.time, "time", return_value=100 + 7 * 24 * 60 * 60):
        silent = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert silent["activity-main"]["state"] == "quiet" and silent["activity-main"]["since"] == 1000, silent
        for kind in ("approval", "input"):
            activity_path.write_text(json.dumps(dict(activity_row, state="waiting", kind=kind)))
            before_capture = capture.call_count
            waiting = panel.main_activity(activity_repo, [activity_main], activity_windows)
            assert waiting["activity-main"]["state"] == "waiting" and waiting["activity-main"]["kind"] == kind, waiting
            assert capture.call_count == before_capture, "waiting is explicit hook evidence, not pane silence"
    activity_path.write_text(json.dumps(dict(activity_row, updated_at=1090)))
    with patch.object(panel.time, "time", return_value=1092):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy", "a fresh heartbeat prevents silence fallback"
    activity_path.write_text(json.dumps(activity_row))
    assert "native prompt" not in repr(panel.MAIN_OUTPUT)
    assert capture.call_args.args[0][-5:] == ["-p", "-t", "%777", "-S", "0"], capture.call_args
    capture.return_value = subprocess.CompletedProcess([], 0, b"new native output", b"")
    with patch.object(panel.time, "time", return_value=1102):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy"
    capture.return_value = subprocess.CompletedProcess([], 1, b"", b"")
    with patch.object(panel.time, "time", return_value=1200):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy", "a failed capture cannot prove silence"
    assert not panel.MAIN_OUTPUT
    activity_path.write_text(json.dumps(dict(activity_row, attempt="fresh", since=1201, updated_at=1201)))
    activity_windows[0][-1] = "fresh"
    with patch.object(panel.time, "time", return_value=1202):
        restarted = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert restarted["activity-main"]["state"] == "busy" and restarted["activity-main"]["attempt"] == "fresh"
        assert restarted["activity-main"]["since"] == 1201, restarted
    for bad_row in (None, "{", dict(activity_row, participant="other"),
                    dict(activity_row, state="waiting"), dict(activity_row, state="waiting", kind="unknown")):
        if bad_row is None:
            activity_path.unlink()
        elif isinstance(bad_row, str):
            activity_path.write_text(bad_row)
        else:
            activity_path.write_text(json.dumps(bad_row))
        with patch.object(panel.time, "time", return_value=1300):
            assert not panel.main_activity(activity_repo, [activity_main], activity_windows), bad_row
    activity_path.write_text(json.dumps(dict(activity_row, attempt="old")))
    with patch.object(panel.time, "time", return_value=1300):
        mismatched = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert graph_view.main_state(dict(activity_main, state="working", attempt="fresh",
                                          panel_activity=mismatched["activity-main"])) == "live marker"
    assert graph_view.main_state(dict(activity_main, state="exited", attempt="fresh",
                                      panel_activity=dict(activity_row, attempt="fresh", state="busy"))) == "exited", \
        "terminal attempt state takes precedence over a matching busy activity row"
for reading, word in (({}, "live"), ({"activity-main": dict(activity_row, since=280)}, "working · 12m"),
                      ({"activity-main": dict(activity_row, state="quiet", since=280)}, "quiet · 12m"),
                      ({"activity-main": dict(activity_row, state="idle", since=280)}, "idle · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="approval", since=280)}, "needs approval · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="input", since=280)}, "needs input · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="approval", attempt="old")}, "live"),
                      ({"activity-main": dict(activity_row, state="idle", since=280, attempt="old")}, "live")):
    report = dict(activity_report, main_activity=reading)
    with patch.object(panel.time, "time", return_value=1000):
        projected_main = next(m for m in graph_view.nodes(report) if m["role"] == "main")
        assert projected_main["state"] == "working", "presentation must preserve attempt lifecycle"
        expected_state = ("waiting_approval" if word.startswith("needs approval") else "waiting_input" if word.startswith("needs input")
                          else "idle" if word.startswith("idle") else "quiet" if word.startswith("quiet")
                          else "working" if word.startswith("working") else "live marker")
        assert graph_view.main_state(projected_main) == expected_state
        assert graph_view.main_words(projected_main) == word
        for ascii_only in (False, True):
            expected = word.replace(" · ", " / ") if ascii_only else word
            for activity_view, menu in (("graph", False), ("tree", False), ("graph", True)):
                navigation = {}
                screen = render(report, "claude", 120, 44, view=activity_view, menu=menu,
                                frame=2, unicode=not ascii_only, navigation=navigation)
                assert expected in screen and "running" in screen, (activity_view, screen)
                if ascii_only:
                    screen.encode("ascii")
                if word.startswith("idle") and activity_view == "graph":
                    assert ("o " if ascii_only else "○ ") + expected in screen, screen
        for ascii_only in (False, True):
            legacy = render(report, "claude", 120, 44, view="compact", main_attempt="activity-attempt",
                            frame=2, unicode=not ascii_only)
            assert (word.replace(" · ", " / ") if ascii_only else word) in legacy, legacy
            if ascii_only:
                legacy.encode("ascii")
        from panel_view import inbox_items
        waiting = word.startswith("needs")
        assert panel.main_needs_attention(report, "activity-attempt") == waiting
        assert panel.main_needs_attention(report, "activity-main") == waiting
        assert not panel.main_needs_attention(report, "unrelated-main")
        if waiting:
            items = inbox_items(report)
            assert len(items) == 1 and items[0]["what"] == "waiting for " + ("approval" if "approval" in word else "input"), items
            assert items[0]["action"] == ("chat", "activity-main") and items[0]["style"] == "alert", items
            ended = dict(report, attempts={"recent": [], "active_recent": [
                dict(activity_report["attempts"]["active_recent"][0], state="done")]})
            assert not panel.main_needs_attention(ended, "activity-attempt") and not inbox_items(ended)
        else:
            assert inbox_items(report) == inbox_items(activity_report)
with patch.object(panel.time, "time", return_value=1000):
    activity_path.write_text(json.dumps(dict(activity_row, updated_at=1001)))
    assert not panel.main_activity(activity_repo, [activity_main]), "future timestamps are untrusted"
    activity_path.write_text("x" * 4097)
    assert not panel.main_activity(activity_repo, [activity_main]), "activity reads must be bounded"
    activity_path.unlink()
    activity_path.symlink_to(activity_repo / "missing-target")
    assert not panel.main_activity(activity_repo, [activity_main]), "never follow activity symlinks"
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
# An answered call keeps its card above the main instead of counting as finished; the detail says when it answered.
finished_board = graph_view.render_graph(dict(board, native_advisors=answered_call), 120, 34,
                                         navigation={"dismissed": True})
assert "Fable 5.1" in finished_board and "Past work" not in finished_board, finished_board
finished_nav = {"selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
finished_detail = graph_view.render_graph(dict(board, native_advisors=answered_call), 100, 40, navigation=finished_nav)
assert "Built-in advisor: last answered " + answered_call["adv-main"]["finished"] in finished_detail, finished_detail
running_nav = {"selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
assert "Built-in advisor: asked " in graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40, navigation=running_nav)
# The board's last row starts a main: clickable only in a managed panel, never on a static render.
starter_board = dict(board, room={"id": "adv-room", "participants": [dict(claude_main, model="claude-opus-5-5"), dict(claude_main, participant="adv-sub",
                     provider="codex", seq=2, consumer=None, model="gpt-6-sol")]})
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
disabled_nav = {"installed": ["codex"], "spawn_managed": False}
assert "needs the tmux panel" in graph_view.render_graph(starter_board, 100, 40, navigation=disabled_nav).splitlines()[-2]
assert not [h for h in disabled_nav["hits"] if h["action"][0] == "spawn"]
armed_nav = {"installed": ["codex"], "spawn_managed": True, "spawn_busy": True}
assert graph_view.render_graph(starter_board, 100, 40, navigation=armed_nav).splitlines()[-2].startswith("Starting a main")
# --spawn-main needs no terminal, refuses workers, an absent panel and a full room, and never attaches.
for spawn_env, expected in (({"OMS_HARNESS_CHILD": "1"}, "worker cannot open owner sessions"),
                            ({"OMS_HARNESS_DELEGATE_DEPTH": "1"}, "worker cannot open owner sessions"),
                            # Without tmux or the CLI on the child's PATH (CI hosts) the tool check refuses first.
                            ({}, "no OMS panel is open for this checkout" if shutil.which("tmux", path=environment.get("PATH"))
                             and shutil.which("claude", path=environment.get("PATH")) else "starting a main needs tmux")):
    refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude", "--json"],
                             cwd=str(project), env=dict(environment, OMS_PANEL_SESSION="", **spawn_env),
                             capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    assert refused.returncode != 0 and expected in refused.stderr, (spawn_env, refused.stdout, refused.stderr)
# The cap is checked after the tool check, so hosts without tmux or a CLI (CI) still reach it.
with patch.object(panel, "session_owner", return_value=str(project)), patch.object(panel, "panel_room", return_value="r1"), \
        patch.object(panel.shutil, "which", return_value="/usr/bin/true"), \
        patch.object(panel, "live_mains", return_value=panel.MAX_LIVE_MAINS), \
        patch.object(panel, "events", return_value="[]"), patch.object(panel, "add_main_window") as opened_window:
    try:
        panel.spawn_main(project, "claude")
        raise AssertionError("a second live main was started")
    except ValueError as error:
        assert str(error).startswith("this panel already has a live main")
        assert "capacity screen k" in str(error) and "unverifiable" in str(error)
        assert "--release-main" not in str(error)
    assert not opened_window.called
    with patch.object(panel, "tmux_command", return_value=["true"]), patch.object(panel, "bind_main_keys"):
        try:
            panel.split_panel(project, launch="claude")
            raise AssertionError("--launch added a second live main")
        except ValueError as error:
            assert str(error).startswith("this panel already has a live main")
            assert "capacity screen k" in str(error) and "unverifiable" in str(error)
            assert "--release-main" not in str(error)
    assert not opened_window.called
    with patch.object(panel, "recorded_room", return_value="r1"):
        try:
            panel.check_legacy_main(project)
            raise AssertionError("an unverified main window allowed replacement")
        except ValueError as error:
            assert str(error).startswith("this panel already has a live main")
            assert "capacity screen k" in str(error) and "unverifiable" in str(error)
            assert "--release-main" not in str(error)
# The cap counts the whole panel, not the selected room; a close needs exactly one proven window and records the leave only after the kill.
def fake_status(repo, ident):
    return {"participants": [{"participant": "%s-m%s" % (ident, n), "joined": True, "role": "main"} for n in range(4)]}
def fake_history(repo, ident, member=None):
    return [{"attempt_id": "%s-m%s" % (ident, n), "tool": "panel-main", "state": "working",
             "refs": {"panel_room_participant": "%s-m%s" % (ident, n)}} for n in range(4)]
two_rooms = "\n".join("claude\t%s\t%s-m%s\tclaude\t@%s" % (ident, ident, n, n) for ident in ("ra", "rb") for n in range(3)) + "\nclaude\trb\t\tclaude\t@9"
with patch.object(panel, "tmux_command", return_value=["true"]), patch.object(panel.room, "status", fake_status), \
        patch.object(panel, "main_history", fake_history), \
        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, two_rooms, "")):
    assert panel.live_mains(project, "s", "ra") == 9 and panel.live_mains(project, "s", "rb") == 9, "cap counted one room"
with patch.object(panel.room, "status", return_value={"participants": []}), \
        patch.object(panel, "main_history", return_value=[]), patch.object(panel.subprocess, "run") as window_query:
    window_query.return_value = subprocess.CompletedProcess([], 0, "codex\tr1\t\tcontrol\t@1\ncodex\tr1\t\tpi\t@2\n", "")
    assert panel.live_mains(project, "s", "r1") == 1, "control counted as a main or pending Pi was lost"
    window_query.return_value = subprocess.CompletedProcess([], 0, "codex\tr1\tmissing\tpi\t@2\n", "")
    assert panel.live_mains(project, "s", "r1") == 1, "missing evidence allowed replacement"
    window_query.return_value = subprocess.CompletedProcess([], 1, "", "")
    try:
        panel.live_mains(project, "s", "r1")
        raise AssertionError("failed window lookup allowed replacement")
    except ValueError as error:
        assert "status unavailable" in str(error)
# Legacy inline owners have no new lease or terminal-host binding; unknown lifecycle still blocks replacement.
with patch.object(panel, "events", return_value=json.dumps([{"tool": "panel-main", "state": "working", "terminal": False}])), \
        patch.object(panel.shutil, "which", return_value=None), patch.object(panel, "run_single_native") as inline_start:
    try:
        panel.run_native("claude", project)
        raise AssertionError("a legacy inline main was replaced")
    except ValueError as error:
        assert "already has a live main" in str(error)
    assert not inline_start.called
# Explicit recovery requires recorded host ownership and positive native termination evidence.
import panel_release
import panel_health
import panel_host
import panel_terminals
import codex_harness
import io
release_repo = temporary / "release-main"
release_repo.mkdir()
subprocess.run(["git", "init", "-q", str(release_repo)], check=True)
release_bin = temporary / "release-bin"
release_bin.mkdir()
release_socket = str(temporary / "release.sock")
release_server = panel_release.process_refs(os.getpid(), "tmux_server")
release_server["panel_socket_digest"] = hashlib.sha256(os.path.realpath(release_socket).encode()).hexdigest()
(release_bin / "tmux").write_text('#!/bin/sh\nif [ "$OMS_RELEASE_ERROR" = 1 ]; then echo "server error" >&2; exit 1; fi\n'
    'if [ "$OMS_RELEASE_NO_SERVER" = 1 ]; then echo "no server running" >&2; exit 1; fi\n'
    'if [ "$1" = display-message ]; then printf "%s\\t%s\\r\\n" "$OMS_RELEASE_SERVER" "$OMS_RELEASE_SOCKET"; '
    'elif [ "$1" = list-windows ]; then printf "%s\\r\\n" "$OMS_RELEASE_WINDOW"; '
    'else printf "%s\\r\\n" "$OMS_RELEASE_PANE"; fi\n')
(release_bin / "tmux").chmod(0o700)
release_now = time.time() + 600
release_env = dict(environment, PATH=str(release_bin) + os.pathsep + environment["PATH"],
                   OMS_RELEASE_ERROR="0", OMS_RELEASE_NO_SERVER="0", OMS_RELEASE_WINDOW="", OMS_RELEASE_PANE="",
                   OMS_RELEASE_SERVER=str(os.getpid()), OMS_RELEASE_SOCKET=release_socket, OMS_RELEASE_NOW=str(release_now))
release_cli = [sys.executable, "-c", "import os,sys; sys.path.insert(0,sys.argv[1]); import terminal_panel as p; "
               "import panel_release as r; r.time.time=lambda:float(os.environ['OMS_RELEASE_NOW']); "
               "sys.exit(p.main(sys.argv[2:]))", str(root / "scripts/lib"), "--repo", str(release_repo), "--release-main"]
live_release_process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(90)"])
dead_release_process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(90)"])
try:
    live_refs = panel_release.process_refs(live_release_process.pid, "native")
    dead_refs = panel_release.process_refs(dead_release_process.pid, "native")
    assert live_refs and dead_refs
    dead_wrapper = {key.replace("native", "wrapper"): value for key, value in dead_refs.items()}
    live_wrapper = {key.replace("native", "wrapper"): value for key, value in live_refs.items()}
    dead_release_process.terminate()
    dead_release_process.wait(timeout=10)
    def release_attempt(state="working", tool="panel-main", role="main", host="tmux", native=None, wrapper=None, reason="", server=None, harness="codex"):
        refs = dict(panel_role=role, panel_room_id="release-room", panel_label="Recovered main",
                    panel_heartbeat_interval="60", panel_heartbeat_ttl="300", panel_harness=harness)
        if host:
            refs["panel_host"] = host
        if host == "tmux":
            refs.update(release_server if server is None else server, panel_pane_id="%12")
        native_refs = dead_refs if native is None else native
        refs.update(dead_wrapper if wrapper is None else wrapper)
        args = [arg for key, value in refs.items() for arg in ("--ref", key + "=" + value)]
        ident = panel.events(release_repo, "start", "--provider", "codex", "--tool", tool, *args,
                             "--then", "starting", *([] if state == "starting" else ["--then", "working"]), output=True)
        if native_refs:
            panel.events(release_repo, "usage", "--attempt", ident, "--actor", "panel-main",
                         *[arg for key, value in native_refs.items() for arg in ("--ref", key + "=" + value)])
        if state not in {"starting", "working"}:
            panel.events(release_repo, "transition", "--attempt", ident, "--state", state, "--reason-code", reason)
        return ident
    working_release = release_attempt()
    blocked_release = release_attempt("blocked", host="inline")
    legacy_release = release_attempt(host=None, native={}, wrapper={})
    empty_refs_release = panel.events(release_repo, "start", "--provider", "codex", "--tool", "panel-main",
                                      "--then", "starting", "--then", "working", output=True)
    inline_alive = release_attempt(host="inline", native=live_refs, wrapper=live_wrapper)
    tmux_alive = release_attempt(native=live_refs)
    inline_orphan = release_attempt(host="inline", native=live_refs)
    inline_unknown = release_attempt(host="inline", wrapper={})
    unconfirmed_release = release_attempt("blocked", host="inline", native={}, reason="native_shutdown_unconfirmed")
    unconfirmed_gone = release_attempt("blocked", host="inline", reason="native_shutdown_unconfirmed")
    starting_release = release_attempt("starting")
    worker_release = release_attempt(tool="peer-delegate", role="worker")
    wrong_role = release_attempt(role="worker")
    herdr_release = release_attempt(host="herdr")
    waiting_release = release_attempt("waiting_input")
    finished_release = release_attempt("cancelled")
    confirmed_exit = release_attempt(host="inline", native={"panel_native_exit": "confirmed", "panel_native_exit_code": "0"})
    pi_launcher = release_attempt(host="inline", harness="pi", native={key.replace("native", "launcher"): value for key, value in dead_refs.items()})
    gone_server = dict(release_server, **{key.replace("native", "tmux_server"): value for key, value in dead_refs.items()})
    no_server_release = release_attempt(server=gone_server)
    restarted_release = release_attempt(server=gone_server)
    pi_release = release_attempt(host="inline", native={}, harness="pi")
    pi_env = dict(OMS_PANEL_MAIN_ATTEMPT=pi_release, OMS_ROOM_REPO=str(release_repo), OMS_PANEL_HARNESS="pi")
    with patch.object(panel_release, "process_refs", return_value=dead_refs):
        codex_harness.record_panel_child(pi_env, dead_release_process.pid)
    pi_row = json.loads(panel_release.events(release_repo, "show", "--attempt", pi_release, "--json"))
    assert all(pi_row["refs"][key] == value for key, value in dead_refs.items())
    pi_alive = release_attempt(host="inline", native={}, harness="pi")
    pi_live_env = dict(pi_env, OMS_PANEL_MAIN_ATTEMPT=pi_alive)
    codex_harness.record_panel_child(pi_live_env, live_release_process.pid)
    ledger = release_repo / ".oms/lifecycle/events.jsonl"
    for release_id, extra_env, extra_args, expected in (
            (working_release, {"OMS_RELEASE_PANE": working_release}, [], "live tagged tmux"),
            (working_release, {"OMS_RELEASE_WINDOW": working_release}, [], "live tagged tmux"),
            (working_release, {"OMS_RELEASE_ERROR": "1"}, [], "liveness unproven"),
            (working_release, {"OMS_RELEASE_NO_SERVER": "1"}, [], "liveness unproven"),
            (legacy_release, {}, [], "liveness unproven"),
            (empty_refs_release, {}, [], "liveness unproven"),
            (inline_alive, {}, [], "liveness unproven"),
            (tmux_alive, {}, [], "liveness unproven"),
            (inline_orphan, {}, [], "native termination is unconfirmed"),
            (inline_unknown, {}, [], "liveness unproven"),
            (unconfirmed_release, {}, [], "liveness unproven"),
            (pi_launcher, {}, [], "Pi child termination unconfirmed; separate recovery required"),
            (pi_alive, {}, [], "Pi child termination unconfirmed; separate recovery required"),
            (working_release, {"OMS_RELEASE_SOCKET": release_socket + ".other"}, [], "tmux server ownership changed"),
            (working_release, {"OMS_RELEASE_NOW": str(time.time())}, [], "liveness unproven"),
            (worker_release, {}, [], "requires a panel-main"),
            (wrong_role, {}, [], "requires a panel-main"),
            (starting_release, {}, [], "requires a panel-main"),
            (waiting_release, {}, [], "requires a panel-main"),
            (finished_release, {}, [], "requires a panel-main"),
            (herdr_release, {}, [], "liveness unproven"),
            ("../invalid", {}, [], "invalid panel main attempt"),
            (working_release, {}, ["--launch", "claude"], "unrecognized arguments"),
            (working_release, {"OMS_HARNESS_CHILD": "1"}, [], "worker cannot release")):
        before_release = ledger.read_bytes()
        refused = subprocess.run(release_cli + [release_id, "--json"] + extra_args,
                                 env=dict(release_env, **extra_env), capture_output=True, text=True, timeout=20)
        assert refused.returncode and expected in refused.stderr, (refused.stdout, refused.stderr)
        assert ledger.read_bytes() == before_release, "refused release changed lifecycle"
    for held_lock in ("panel-main-start", "panel-main-session"):
        before_release = ledger.read_bytes()
        with panel_results._lock(release_repo, held_lock):
            refused = subprocess.run(release_cli + [working_release], env=release_env,
                                     capture_output=True, text=True, timeout=20)
            assert refused.returncode and "panel operation is busy" in refused.stderr, refused.stderr
        assert ledger.read_bytes() == before_release, "release bypassed a live main lease"
    with patch.dict(os.environ, release_env, clear=True), patch.object(panel_release.time, "time", return_value=release_now):
        with patch.object(panel_release.subprocess, "run", wraps=panel_release.subprocess.run) as scans:
            dead = panel_release.dead_mains(release_repo)
            assert sum(c.args[0][1] == "list-panes" for c in scans.call_args_list) == 1
            assert sum(c.args[0][1] == "list-windows" for c in scans.call_args_list) == 1
        assert {row["attempt"] for row in dead["releasable"]} == {working_release, blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}, dead
        assert {row["attempt"] for row in dead["unverifiable"]} == {
            legacy_release, inline_alive, tmux_alive, inline_orphan, inline_unknown, unconfirmed_release, herdr_release, pi_launcher, pi_alive, empty_refs_release}, dead
        assert all(row["label"] == ("" if row["attempt"] == empty_refs_release else "Recovered main") and row["age"]
                   for group in dead.values() for row in group)
        capacity = panel_health.render({"dead_mains": dead}, 220, 100)
        assert "Dead mains" in capacity and "Unverifiable mains" in capacity, capacity
        assert "oms panel --release-main " + working_release in capacity
        assert "oms panel --release-main " + legacy_release not in capacity
        assert "Pi child termination unconfirmed; separate recovery required" in capacity
        with patch.object(panel_release.subprocess, "run", wraps=panel_release.subprocess.run) as scans:
            reused = panel_release.dead_mains(release_repo, windows={working_release})
            assert {row["attempt"] for row in reused["releasable"]} == {blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}
            assert sum(c.args[0][1] == "list-panes" for c in scans.call_args_list) == 1
            assert not any(c.args[0][1] == "list-windows" for c in scans.call_args_list)
        with patch.object(panel_release.shutil, "which", return_value=None):
            try:
                panel_release.tagged_attempts()
                raise AssertionError("missing tmux proved host absence")
            except ValueError as error:
                assert "liveness unproven" in str(error)
        with patch.dict(os.environ, OMS_RELEASE_ERROR="1"):
            failed_scan = panel_release.dead_mains(release_repo)
            assert {row["attempt"] for row in failed_scan["releasable"]} == {blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}
            assert working_release in {row["attempt"] for row in failed_scan["unverifiable"]}
        with patch.dict(os.environ, OMS_RELEASE_NO_SERVER="1"):
            absent = panel_release.dead_mains(release_repo)
            assert {no_server_release, restarted_release} <= {row["attempt"] for row in absent["releasable"]}
            assert working_release in {row["attempt"] for row in absent["unverifiable"]}
        with patch.object(panel_release, "tmux_identity", side_effect=ValueError("unavailable")), \
                patch.object(panel_host, "herdr_active", return_value=False), patch.object(sys, "stderr", new_callable=io.StringIO) as warning:
            assert panel_release.start_refs(release_repo, True)["panel_host"] == "tmux"
            assert warning.getvalue().count("tmux ownership binding unavailable") == 1
        rows = json.loads(panel_release.events(release_repo, "list", "--json"))
        stale_inline = next(row for row in rows if row["attempt_id"] == blocked_release)
        with patch.object(panel_release, "identity_gone", return_value=True) as probes:
            assert panel_release.classify([]) == {"releasable": [], "unverifiable": []}
            assert not probes.called
            capped_tick = Mock()
            capped_tick.attempts.return_value = [dict(stale_inline, attempt_id="cap-%s" % i) for i in range(4)]
            with patch.object(panel_release, "MAX_TICK_PROBES", 2):
                limited = panel_release.dead_mains(release_repo, _tick=capped_tick)
            assert len(limited["releasable"]) == len(limited["unverifiable"]) == 2
            assert probes.call_count == 6, probes.call_count
            assert all("verification pending" in row["reason"] for row in limited["unverifiable"])
            pending_capacity = panel_health.render({"dead_mains": limited}, 220, 40)
            assert "verification pending" in pending_capacity and "oms panel --release-main cap-2" not in pending_capacity
        with patch.object(panel_release.time, "time", return_value=release_now - 600):
            assert panel_release.classify(rows) == {"releasable": [], "unverifiable": []}, "fresh attempt appeared as dead"
        young = dict(next(row for row in rows if row["attempt_id"] == working_release),
                     created_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 30)))
        assert panel_release.classify([young]) == {"releasable": [], "unverifiable": []}
        fresh = dict(young, created_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 600)),
                     last_heartbeat_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 1)))
        assert panel_release.classify([fresh]) == {"releasable": [], "unverifiable": []}
        with patch.dict(os.environ, OMS_RELEASE_PANE=working_release):
            assert working_release not in {row["attempt"] for row in panel_release.dead_mains(release_repo)["releasable"]}
        for row, offered in ((next(row for row in rows if row["attempt_id"] == working_release), True),
                             (next(row for row in rows if row["attempt_id"] == legacy_release), False)):
            with patch.object(panel, "events", return_value=json.dumps([row])), \
                    patch.object(panel, "session_owner", return_value=str(release_repo)), \
                    patch.object(panel, "panel_room", return_value="release-room"), \
                    patch.object(panel, "live_mains", return_value=panel.MAX_LIVE_MAINS), \
                    patch.object(panel, "main_installed", return_value=True), \
                    patch.object(panel_host, "herdr_active", return_value=False), \
                    patch.dict(os.environ, TMUX="fixture", OMS_PANEL_SESSION="oms-fixture"):
                for refuse in (lambda: panel.check_legacy_main(release_repo),
                               lambda: panel.spawn_main(release_repo, "claude"),
                               lambda: panel.construct_panel(release_repo, "oms-fixture", str(release_repo),
                                                             os.environ.copy(), "auto", False, "claude", None, None, None),
                               lambda: panel.open_native("claude", release_repo)):
                    try:
                        refuse()
                        raise AssertionError("unreleased main allowed replacement")
                    except ValueError as error:
                        assert str(error).startswith("this panel already has a live main"), error
                        assert ("oms panel --release-main " + row["attempt_id"] in str(error)) is offered, error
                        assert "--release-main ATTEMPT" not in str(error), error
                        if not offered:
                            assert "capacity screen k" in str(error) and "unverifiable" in str(error), error
        if os.name != "nt":
            with patch.object(panel_terminals, "process", return_value={"identity": "f" * 64, "state": "S"}):
                assert not panel_terminals.matches(live_release_process.pid, live_refs["panel_native_identity"])
                assert panel_release.identity_gone(live_refs, "native"), "PID reuse was treated as the recorded live process"
            with patch.object(panel_terminals, "process", return_value=None), patch.object(panel_terminals.os, "kill", side_effect=PermissionError):
                assert not panel_release.identity_gone(live_refs, "native"), "unknown process probe proved death"
        with patch.object(panel_host, "herdr_active", return_value=True), \
                patch.object(panel_host, "socket_digest", return_value="host-digest"), \
                patch.object(panel_host, "pane", return_value={}) as host_lookup:
            for host_error in (None, ValueError("missing pane")):
                host_lookup.side_effect = host_error
                try:
                    panel_release.prove_host_absent({"panel_host": "herdr", "panel_socket_digest": "host-digest",
                                                    "panel_pane_id": "w1:p1"}, herdr_release)
                    raise AssertionError("Herdr evidence allowed unsafe release")
                except ValueError as error:
                    assert "liveness unproven" in str(error)
        mock_native = Mock(pid=live_release_process.pid)
        mock_native.wait.return_value = mock_native.poll.return_value = 0
        with patch.object(panel, "events", return_value="att_recording") as recorded, \
                patch.object(panel, "main_installed", return_value=True), patch.object(panel, "ensure_room", return_value="release-room"), \
                patch.object(panel.room, "join"), patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel_host, "herdr_active", return_value=False), patch.object(panel, "bootstrap", return_value="fixture"), \
                patch.object(panel, "native_command", return_value=["fixture-native"]), patch.object(panel.subprocess, "Popen", return_value=mock_native):
            assert panel._run_native("codex", release_repo) == 0
            start_args = recorded.call_args_list[0].args
            assert "panel_host=inline" in start_args and "panel_wrapper_pid=" + str(os.getpid()) in start_args
            assert any(isinstance(arg, str) and arg.startswith("panel_wrapper_identity=") for arg in start_args)
            assert any("panel_native_pid=" + str(live_release_process.pid) in call.args for call in recorded.call_args_list)
            assert any("panel_native_exit=confirmed" in call.args for call in recorded.call_args_list)
            recorded.reset_mock()
            assert panel._run_native("pi", release_repo) == 0
            assert any("panel_launcher_pid=" + str(live_release_process.pid) in call.args for call in recorded.call_args_list)
            assert any("panel_launcher_exit=confirmed" in call.args for call in recorded.call_args_list)
            assert not any("panel_native_exit=confirmed" in call.args for call in recorded.call_args_list)
        broker = Mock()
        broker.model.return_value = {}
        with patch.object(codex_harness, "AuthBroker", return_value=broker), \
                patch.object(codex_harness, "Gateway"), patch.object(codex_harness.threading, "Thread"), \
                patch.object(codex_harness.shutil, "which", return_value="fixture"), \
                patch.object(codex_harness, "harness_env", return_value=pi_live_env), \
                patch.object(codex_harness.subprocess, "Popen", return_value=mock_native), \
                patch.object(panel_release, "events", side_effect=[json.dumps(pi_row), ""]) as child_activity, \
                patch.object(codex_harness, "record_panel_child", wraps=codex_harness.record_panel_child) as record_child, \
                patch.object(sys, "stderr", new_callable=io.StringIO) as child_notice:
            assert codex_harness.main(["pi", "--model", "fixture-model"]) == 0
            record_child.assert_called_once_with(pi_live_env, live_release_process.pid)
            assert "panel_native_pid=" + str(live_release_process.pid) in child_activity.call_args.args
            assert "Pi child identity unavailable" not in child_notice.getvalue()
        for release_id, previous, extra_env in ((working_release, "working", {}), (blocked_release, "blocked", {}),
                (confirmed_exit, "working", {}), (unconfirmed_gone, "blocked", {}),
                (no_server_release, "working", {"OMS_RELEASE_NO_SERVER": "1"}),
                (restarted_release, "working", {"OMS_RELEASE_SERVER": str(live_release_process.pid)}),
                (pi_release, "working", {})):
            released = json.loads(call(release_cli + [release_id, "--json"], env=dict(release_env, **extra_env)))
            assert released == {"attempt": release_id, "previous_state": previous, "state": "abandoned", "reason": "panel_main_released"}
            before_release = ledger.read_bytes()
            repeated = subprocess.run(release_cli + [release_id], env=release_env, capture_output=True, text=True, timeout=20)
            assert repeated.returncode and ledger.read_bytes() == before_release
    front_release = release_attempt(host="inline")
    panel.events(release_repo, "usage", "--attempt", front_release, "--actor", "panel-main",
                 "--ref", "panel_heartbeat_interval=1", "--ref", "panel_heartbeat_ttl=1")
    time.sleep(2)
    front = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(release_repo),
                             "--release-main", front_release, "--json"], env=release_env))
    assert front == {"attempt": front_release, "previous_state": "working", "state": "abandoned", "reason": "panel_main_released"}
    release_rows = [json.loads(line) for line in ledger.read_text().splitlines()]
    assert [(r["from_state"], r["to_state"]) for r in release_rows if r.get("reason_code") == "panel_main_released"] == [
        ("working", "blocked"), ("blocked", "abandoned"), ("blocked", "abandoned"), ("working", "blocked"), ("blocked", "abandoned"), ("blocked", "abandoned")
    ] + [("working", "blocked"), ("blocked", "abandoned")] * 4
    assert all(r["actor"]["name"] == "panel-main-release" for r in release_rows if r.get("reason_code") == "panel_main_released")
    terminal_release = json.loads(panel_release.events(release_repo, "show", "--attempt", working_release, "--json"))
    with patch.object(panel, "events", return_value=json.dumps([terminal_release])), patch.object(panel.shutil, "which", return_value=None):
        panel.check_legacy_main(release_repo)
    with patch.object(panel.room, "status", return_value={"participants": [{"participant": working_release, "joined": True, "role": "main"}]}), \
            patch.object(panel, "main_history", return_value=[terminal_release]), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")):
        assert panel.live_mains(release_repo, "s", "release-room") == 0
    assert "Dead mains" not in panel_health.render({}, 180, 80)
finally:
    for child in (live_release_process, dead_release_process):
        if child.poll() is None:
            child.terminate()
        child.wait(timeout=10)
occupied = {"installed": ["claude", "pi", "codex"], "spawn_managed": True, "main_occupied": True}
assert graph_view.spawn_bar(100, occupied)[1] == []
assert "One main" in graph_view.spawn_bar(100, occupied)[0]
armed_bar = graph_view.spawn_bar(100, {"installed": ["claude"], "spawn_managed": True, "spawn_armed": True})
assert "1 Claude" in armed_bar[0] and "2 Pi" not in armed_bar[0] and "0 Codex" not in armed_bar[0]
# Closing an exit prompt requires all three window fields and a successful lookup.
exit_options = {"@oms_panel_main_attempt": "exit-attempt", "@oms_panel_room": shared,
                "@oms_panel_room_participant": "exit-attempt",
                "@oms_panel_repo": str(project.resolve())}
def exited_native(*args, **kwargs):
    kwargs["_exit_info"].update(attempt="exit-attempt", room=shared)
    return 0
for fields, query_status, closes in ((["exit-attempt", "", str(project.resolve())], 0, True),
                                    (["successor", "", str(project.resolve())], 0, False),
                                    (["exit-attempt", "%42", str(project.resolve())], 0, False),
                                    (["exit-attempt", "", "/foreign/repository"], 0, False),
                                    (["exit-attempt", "", str(project.resolve())], 1, False)):
    window_options = dict(exit_options, **{"@oms_panel_main_attempt": fields[0]})
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "pane_option", side_effect=window_options.get), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), \
            patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], query_status, "\t".join(["@42", *fields, "exit-attempt"]) + "\r\n", "")) as close_query:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0
        kills = [call for call in close_query.call_args_list if "if-shell" in call.args[0]]
        assert bool(kills) == closes, (fields, query_status, kills)
        if kills:
            args = kills[0].args[0]
            assert args[-1] == "kill-window -t @42" and args[args.index("-t") + 1] == "@42"
            assert "@oms_panel_main_attempt},exit-attempt" in args[-2] and "@oms_panel_native_pane},}" in args[-2]
            assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in args[-2]
# Restart refuses an unverified session/window or nonterminal previous attempt;
# a Pi restart deliberately ignores even a recorded session id and starts fresh.
for harness, owner, previous_terminal, succeeds in (("claude", None, True, False),
                                           ("claude", str(project.resolve()), False, False),
                                           ("claude", str(project.resolve()), True, True),
                                           ("codex", str(project.resolve()), True, True),
                                           ("pi", str(project.resolve()), True, True)):
    previous = {"attempt_id": "exit-attempt", "terminal": previous_terminal, "provider": panel.main_owner(harness),
                "tool": "panel-main", "refs": {"panel_role": "main", "panel_room_id": shared,
                                               "native_session": "recorded-session"}}
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native) as restart_main, \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "_main_window", return_value=["@42", "exit-attempt", "", str(project.resolve()), "exit-attempt"]), \
            patch.object(panel, "session_owner", return_value=owner), patch.object(panel, "live_mains", return_value=0), \
            patch.object(panel, "main_history", return_value=[previous]), \
            patch.object(panel.room, "status", return_value={"participants": [{"participant": "exit-attempt",
                "joined": True, "role": "main", "provider": panel.main_owner(harness),
                "consumer": hashlib.sha256(b"recorded-session").hexdigest()[:32]}]}), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), \
            patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native(harness, project, task="push the release tag") == 0
        assert restart_main.call_count == (2 if succeeds else 1)
        assert restart_main.call_args_list[0].args[4] == "push the release tag"
        if succeeds:
            assert restart_main.call_args.args[2] == (None if harness == "pi" else "recorded-session")
            assert restart_main.call_args.args[4] is None, "restart resent the original task"
# Context or launch setup fails before a child exists, before or after replacement
# tagging. Retry and guarded close retain the terminal window owner;
# incomplete or changed ownership must never receive it.
from unittest.mock import Mock
for failure_step, recovery in ([("write", key) for key in
        ("retry", "close", "nonterminal", "participant", "room", "repository", "native-pane", "session", "unavailable")]
        + [(step, key) for step in ("directory", "command", "popen") for key in ("retry", "close")]):
    failure_room = room.create(project, "restart-failure-" + failure_step + "-" + recovery, "Failed restart")
    failure_tags, failure_attempts, failure_launches, failure_closes = {}, [], [], []
    failure_created = []
    real_run, real_popen, real_write, real_history = subprocess.run, subprocess.Popen, Path.write_text, panel.main_history
    real_events, real_directory = panel.events, panel.tempfile.TemporaryDirectory
    def failure_events(repo, operation, *args, **kwargs):
        result = real_events(repo, operation, *args, **kwargs)
        if operation == "start":
            failure_created.append(result)
        return result
    def failure_directory(*args, **kwargs):
        if failure_step == "directory" and len(failure_created) == 2:
            assert failure_tags["@oms_panel_main_attempt"] == failure_created[0]
            assert "@oms_panel_native_pane" not in failure_tags
            assert len(failure_launches) == 1, "replacement launched before directory creation"
            raise OSError("injected replacement context directory failure")
        return real_directory(*args, **kwargs)
    def failure_tmux(command, **kwargs):
        if command[0] != "fixture-tmux":
            return real_run(command, **kwargs)
        if command[1] == "set-option":
            if "-u" in command:
                failure_tags.pop(command[-1], None)
            else:
                failure_tags[command[-2]] = command[-1]
                if command[-2] == "@oms_panel_main_attempt":
                    failure_attempts.append(command[-1])
        elif command[1] == "display-message":
            tags = dict(failure_tags)
            if len(failure_attempts) == 2:
                changed = {"participant": ("@oms_panel_room_participant", "foreign-member"),
                           "repository": ("@oms_panel_repo", "/foreign/repository"),
                           "native-pane": ("@oms_panel_native_pane", "%42")}.get(recovery)
                if changed:
                    tags[changed[0]] = changed[1]
            fields = ["@42"] + [tags.get(name, "") for name in ("@oms_panel_main_attempt", "@oms_panel_native_pane",
                      "@oms_panel_repo", "@oms_panel_room_participant")]
            return subprocess.CompletedProcess(command, 0, "\t".join(fields) + "\r\n", "")
        elif command[1] == "if-shell":
            failure_closes.append(command)
        return subprocess.CompletedProcess(command, 0, "", "")
    assert panel.launch_failed_without_child(FileNotFoundError("missing")) and not panel.launch_failed_without_child(OSError(5, "I/O"))
    def failure_process(command, **kwargs):
        if command[0] != "fixture-native":
            return real_popen(command, **kwargs)
        if (failure_step == "popen" and len(failure_created) == 2
                and kwargs["env"]["OMS_PANEL_MAIN_ATTEMPT"] == failure_created[-1]):
            assert len(failure_launches) == 1, "replacement child already exists"
            raise FileNotFoundError("injected replacement executable missing")
        failure_launches.append(kwargs["env"]["OMS_PANEL_MAIN_ATTEMPT"])
        process = Mock()
        process.wait.return_value = process.poll.return_value = 0
        return process
    def failure_command(*args, **kwargs):
        if failure_step == "command" and len(failure_created) == 2:
            assert len(failure_launches) == 1, "replacement launched before argv construction"
            raise ValueError("injected replacement native command failure")
        return ["fixture-native"]
    def failure_write(path, *args, **kwargs):
        if failure_step == "write" and path.name == "context.txt" and len(failure_attempts) == 2:
            assert failure_tags["@oms_panel_native_pane"] == "%42"
            assert failure_tags["@oms_panel_repo"] == str(project.resolve())
            assert len(failure_launches) == 1, "replacement launched before the injected startup failure"
            raise OSError("injected replacement context write failure")
        return real_write(path, *args, **kwargs)
    def failure_history(*args, **kwargs):
        rows = real_history(*args, **kwargs)
        if len(failure_attempts) == 2:
            if recovery == "unavailable":
                raise ValueError("replacement history unavailable")
            rows = deepcopy(rows)
            for row in rows:
                if row["attempt_id"] == failure_attempts[-1]:
                    if recovery == "nonterminal":
                        row["terminal"] = False
                    elif recovery == "room":
                        row["refs"]["panel_room_id"] = "foreign-room"
        return rows
    failure_keys = iter(["r", "r", "\n"] if recovery == "retry" else ["r", "\n"])
    def failure_key():
        key = next(failure_keys)
        if len(failure_created) == 2:
            history = real_history(project, failure_room)
            replacement = next(row for row in history if row["attempt_id"] == failure_created[-1])
            assert replacement["terminal"] is True and replacement["state"] == "failed"
            if failure_step in ("command", "popen"):
                assert replacement["reason_code"] == ("native_command_failed" if failure_step == "command" else "native_launch_failed")
                assert "@oms_panel_native_pane" not in failure_tags
            assert all(row["terminal"] is True for row in history if row["tool"] == "panel-main")
        return key
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-failure",
                                    OMS_ROOM_ID=failure_room), clear=True), \
            patch.object(panel, "resolve_session", return_value=("", "")), \
            patch.object(panel, "main_installed", return_value=True), patch.object(panel, "events", side_effect=failure_events), \
            patch.object(panel.tempfile, "TemporaryDirectory", side_effect=failure_directory), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel, "pane_option", side_effect=lambda name: failure_tags.get(name, "")), \
            patch.object(panel, "session_owner", side_effect=lambda session: "/foreign/repository"
                         if recovery == "session" and len(failure_attempts) == 2 else str(project.resolve())), \
            patch.object(panel, "live_mains", return_value=0), patch.object(panel, "main_history", side_effect=failure_history), \
            patch.object(panel, "native_command", side_effect=failure_command), patch.object(panel, "bootstrap", return_value="fixture"), \
            patch.object(panel.subprocess, "run", side_effect=failure_tmux), \
            patch.object(panel.subprocess, "Popen", side_effect=failure_process), patch.object(Path, "write_text", new=failure_write), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch.object(panel, "_main_exit_key", side_effect=failure_key):
        failure_status = panel.run_native("claude", project)
    succeeds = recovery in ("retry", "close")
    assert len(failure_created) == (3 if recovery == "retry" else 2), "failed restart stranded the retry prompt"
    assert len(failure_attempts) == len(failure_created) - int(failure_step == "directory")
    assert len(failure_launches) == (2 if recovery == "retry" else 1)
    assert bool(failure_closes) == succeeds, "failed restart stranded close or adopted an unverified owner"
    if succeeds:
        assert failure_status == (0 if recovery == "retry" or failure_step == "directory" else 1)
        close = failure_closes[0]
        assert close[-1] == "kill-window -t @42" and close[close.index("-t") + 1] == "@42"
        assert "#{==:#{@oms_panel_main_attempt}," + failure_attempts[-1] + "}" in close[-2]
        assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in close[-2]
        assert "#{==:#{@oms_panel_native_pane},}" in close[-2]
    if failure_step in ("directory", "command", "popen"):
        # A failed startup must not block a later fresh launch through the real
        # legacy lifecycle guard, after either retry or guarded close.
        with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=failure_room), clear=True), \
                patch.object(panel, "resolve_session", return_value=("", "")), \
                patch.object(panel, "main_installed", return_value=True), patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel, "native_command", return_value=["fixture-native"]), \
                patch.object(panel, "bootstrap", return_value="fixture"), patch.object(panel.subprocess, "Popen", side_effect=failure_process):
            assert panel.run_native("claude", project) == 0
        assert all(row["terminal"] is True for row in real_history(project, failure_room) if row["tool"] == "panel-main")
# A successor takes the window after lookup. Only the server-side condition may
# delete the pinned window; a separate kill against the moved pane would delete it.
for change in ("attempt", "native-pane", "moved-pane"):
    deleted = []
    def close_race(command, **kwargs):
        if "display-message" in command:
            fields = "exit-attempt\t\t" + str(project.resolve())
            if "#{window_id}" in command[-1]:
                fields = "@42\t" + fields + "\texit-attempt"
            return subprocess.CompletedProcess(command, 0, fields + "\n", "")
        if "kill-window" in command:
            deleted.append("successor")
        elif "if-shell" in command:
            assert command[-1] == "kill-window -t @42" and command[command.index("-t") + 1] == "@42"
            condition = command[-2]
            assert "#{==:#{@oms_panel_main_attempt},exit-attempt}" in condition
            assert "#{==:#{@oms_panel_native_pane},}" in condition
            assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in condition
            if change == "moved-pane":
                deleted.append("original")
        return subprocess.CompletedProcess(command, 0, "", "")
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run", side_effect=close_race):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0
    assert "successor" not in deleted and deleted == (["original"] if change == "moved-pane" else [])
# Run the same close path against a real isolated server: ownership changes
# after display-message returns but before if-shell evaluates on that server.
close_tmux_binary = shutil.which("tmux")
if close_tmux_binary and os.name != "nt":
    close_tmux = [close_tmux_binary, "-S", str(temporary / "close-race.sock"), "-f", "/dev/null"]
    real_run = subprocess.run
    real_run(close_tmux + ["new-session", "-d", "-s", "close-race", "sleep 60"], check=True)
    try:
        for changed in (False, True):
            window_id, native_pane = subprocess.check_output(close_tmux + ["new-window", "-d", "-P", "-F",
                "#{window_id}\t#{pane_id}", "sleep 60"], text=True).strip().split("\t")
            for option, value in (("@oms_panel_main_attempt", "exit-attempt"), ("@oms_panel_native_pane", ""),
                                  ("@oms_panel_repo", str(project.resolve())), ("@oms_panel_room_participant", "exit-attempt")):
                real_run(close_tmux + ["set-option", "-w", "-t", window_id, option, value], check=True)
            def change_repository(command, **kwargs):
                result = real_run(command, **kwargs)
                if changed and "display-message" in command:
                    real_run(close_tmux + ["set-option", "-w", "-t", window_id,
                                          "@oms_panel_repo", "/foreign/repository"], check=True)
                return result
            with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE=native_pane, OMS_PANEL_SESSION="oms-exit"), clear=True), \
                    patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
                    patch.object(panel, "managed_session", return_value=True), \
                    patch.object(panel, "tmux_command", side_effect=lambda *args: close_tmux + list(args)), \
                    patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
                    patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
                    patch.object(panel.subprocess, "run", side_effect=change_repository):
                exit_input.return_value.__enter__.return_value.fd = None
                assert panel.run_native("claude", project) == 0
            survivors = subprocess.check_output(close_tmux + ["list-windows", "-t", "close-race", "-F", "#{window_id}"], text=True).splitlines()
            assert (window_id in survivors) == changed, "server close guard ignored changed repository ownership"
    finally:
        real_run(close_tmux + ["kill-server"], capture_output=True)
# Ambiguous tmux format operands preserve the window before issuing if-shell.
from contextlib import nullcontext
for suffix in ("hash#repo", "comma,repo", "colon:repo", "brace{repo}", "backslash\\repo", "control\nrepo"):
    unsafe_repo = temporary / suffix
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "_lock", return_value=nullcontext()), patch.object(panel, "check_legacy_main"), \
            patch.object(panel, "run_single_native", side_effect=exited_native), patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "_main_window", return_value=["@42", "exit-attempt", "", str(unsafe_repo.resolve()), "exit-attempt"]), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run") as unsafe_close, patch.object(panel.sys, "stderr") as close_error:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", unsafe_repo) == 0
        assert not unsafe_close.called
        assert any("repository path cannot be safely embedded" in str(call) for call in close_error.write.call_args_list)
# A failed native marker query is unavailable evidence, even when pane_option
# would report an empty (unset) marker. Neither restart nor close may proceed.
for key in ("r", ""):
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native) as unavailable_start, \
            patch.object(panel, "managed_session", return_value=True), patch.object(panel, "pane_option", side_effect=exit_options.get), \
            patch.object(panel, "session_owner", return_value=str(project.resolve())), patch.object(panel, "live_mains", return_value=0), \
            patch.object(panel, "main_history", return_value=[dict(previous, provider="claude", terminal=True)]), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=[key, EOFError]), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")) as unavailable_query:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0 and unavailable_start.call_count == 1
        assert not any("kill-window" in call.args[0] or "if-shell" in call.args[0] for call in unavailable_query.call_args_list)
import hook_state
from unittest.mock import Mock
# Fresh Codex receives its real session only through the first native hook.
# Without a stored native_session, r must start fresh despite that consumer.
for occupied in (False, True):
    fresh_room = room.create(project, "restart-fresh-codex-" + str(occupied).lower(), "Fresh Codex restart")
    fresh_launches, fresh_exit = [], {}
    real_popen = subprocess.Popen
    real_single = panel.run_single_native
    def fresh_process(command, **kwargs):
        if command[0] != "codex":
            return real_popen(command, **kwargs)
        assert "resume" not in command
        env = kwargs["env"]
        fresh_launches.append((command, env["OMS_ROOM_PARTICIPANT"], env["OMS_PANEL_MAIN_ATTEMPT"]))
        with patch.dict(os.environ, env, clear=True):
            hook_state._live_thread_hint({"hook_event_name": "UserPromptSubmit",
                "session_id": "fresh-codex-hook-session-%s-%s" % (occupied, len(fresh_launches))}, project)
        enrolled = room.participant(room.status(project, fresh_room), env["OMS_ROOM_PARTICIPANT"])
        assert enrolled.get("consumer"), "the first hook did not bind the fresh Codex consumer"
        process = Mock()
        process.wait.return_value = process.poll.return_value = 0
        return process
    def fresh_native(*args, **kwargs):
        with patch.object(panel, "managed_session", return_value=False):
            result = real_single(*args, **kwargs)
        fresh_exit.update(kwargs["_exit_info"])
        return result
    def fresh_window(pane):
        return ["@42", fresh_exit["attempt"], "", str(project.resolve()), fresh_launches[-1][1]]
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-fresh", OMS_ROOM_ID=fresh_room), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=fresh_native), \
            patch.object(panel, "managed_session", return_value=True), patch.object(panel, "_main_window", side_effect=fresh_window), \
            patch.object(panel, "session_owner", return_value=str(project.resolve())), \
            patch.object(panel, "live_mains", return_value=int(occupied)), patch.object(panel.subprocess, "Popen", side_effect=fresh_process), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("codex", project) == 0
    assert len(fresh_launches) == (1 if occupied else 2)
    for command, participant, attempt_id in fresh_launches:
        previous = next(row for row in panel.main_history(project, fresh_room) if row["attempt_id"] == attempt_id)
        assert previous["terminal"] and not previous["refs"].get("native_session")
# /clear, exit and r retain the actual participant, join boundary and mailbox.
clear_room = room.create(project, "restart-clear-room", "Restart after clear")
clear_session = "11111111-2222-4333-8444-555555555555"
clear_launches, clear_exit = [], {}
real_popen = subprocess.Popen
def clear_process(command, **kwargs):
    if command[0] != "claude":
        return real_popen(command, **kwargs)
    env = kwargs["env"]
    clear_launches.append((command, env["OMS_ROOM_PARTICIPANT"], env["OMS_PANEL_MAIN_ATTEMPT"]))
    if len(clear_launches) == 1:
        room.join(project, clear_room, "clear-peer", "codex")
        room.send(project, clear_room, "clear-peer", env["OMS_ROOM_PARTICIPANT"], "Keep this mailbox", message_id="clear-mail")
        with patch.dict(os.environ, env, clear=True):
            assert hook_state.rebind_cleared_session({"hook_event_name": "SessionStart", "source": "clear", "session_id": clear_session})
    else:
        assert command[command.index("--resume") + 1] == clear_session
    process = Mock()
    process.wait.return_value = process.poll.return_value = 0
    return process
real_single = panel.run_single_native
def clear_native(*args, **kwargs):
    with patch.object(panel, "managed_session", return_value=False):
        result = real_single(*args, **kwargs)
    clear_exit.update(kwargs["_exit_info"])
    return result
def clear_window(pane):
    return ["@42", clear_exit["attempt"], "", str(project.resolve()), clear_launches[0][1]]
def clear_option(name):
    return dict(zip(("@oms_panel_main_attempt", "@oms_panel_native_pane", "@oms_panel_repo", "@oms_panel_room_participant"),
                    clear_window("%42")[1:])).get(name, "")
clear_env = dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-clear", OMS_ROOM_ID=clear_room)
with patch.dict(os.environ, clear_env, clear=True), patch.object(panel, "check_legacy_main"), \
        patch.object(panel, "run_single_native", side_effect=clear_native), patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "_main_window", side_effect=clear_window, create=True), patch.object(panel, "pane_option", side_effect=clear_option), \
        patch.object(panel, "session_owner", return_value=str(project.resolve())), \
        patch.object(panel, "live_mains", return_value=0), patch.object(panel.subprocess, "Popen", side_effect=clear_process), \
        patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
        patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
    exit_input.return_value.__enter__.return_value.fd = None
    assert panel.run_native("claude", project) == 0
assert len(clear_launches) == 2 and clear_launches[0][1] == clear_launches[1][1]
clear_member = clear_launches[0][1]
assert any(row["room_event"].get("id") == "clear-mail" for row in room.updates(project, clear_room, clear_member)["turns"])
# Current consumer binding also proves continuity for older stale attempt refs.
for command, who, attempt_id in clear_launches:
    panel.events(project, "usage", "--attempt", attempt_id, "--ref", "panel_session_digest=" + "0" * 32)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=clear_room), clear=True), \
        patch.object(panel, "managed_session", return_value=False), patch.object(panel.subprocess, "Popen", side_effect=clear_process):
    assert panel.run_single_native("claude", project, resume=clear_session) == 0
assert len(clear_launches) == 3 and clear_launches[-1][1] == clear_member
foreign_main = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                            "--ref", "panel_room_id=" + clear_room, "--ref", "panel_room_participant=" + clear_member,
                            "--ref", "panel_session_digest=" + "0" * 32, "--then", "starting", "--then", "working", output=True)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=clear_room), clear=True), \
        patch.object(panel, "native_command", side_effect=AssertionError("live main's session launched")):
    try:
        panel.run_single_native("claude", project, resume=clear_session)
    except ValueError as error:
        assert "terminal owner" in str(error)
    else:
        raise AssertionError("current consumer ignored another live main")
panel.events(project, "transition", "--attempt", foreign_main, "--state", "cancelled")
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=shared), clear=True), \
        patch.object(panel, "native_command", side_effect=AssertionError("another room's session launched")):
    try:
        panel.run_single_native("claude", project, resume=clear_session)
    except ValueError as error:
        assert "another room" in str(error)
    else:
        raise AssertionError("session moved to another room")
if os.name != "nt":
    import signal
    prompt_script = ("import sys,signal,os\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
        "os.environ['TMUX_PANE']='%42'\n"
        "p.check_legacy_main=lambda repo: None\np.managed_session=lambda: True\n"
        "def exited(*args,**kw): kw['_exit_info'].update(attempt='exit-attempt',room='exit-room'); return 0\n"
        "p.run_single_native=exited\n"
        "if len(sys.argv)>3:\n"
        " import panel_input\n"
        " original=panel_input.TerminalInput.__enter__\n"
        " def enter(self):\n"
        "  result=original(self); os.kill(os.getpid(),int(sys.argv[3])); return result\n"
        " panel_input.TerminalInput.__enter__=enter\n"
        "try: p.run_native('claude',Path(sys.argv[2]))\n"
        "except p.NativeShutdown as e: sys.exit(e.exit_code)\n")
    for number in (signal.SIGTERM, signal.SIGHUP):
        terminal([sys.executable, "-c", prompt_script, str(root / "scripts/lib"), str(project)], b"", b"\033[?1000l",
                 trigger=b"\033[?1000h", signal_after=number, expected_exit=128 + number)
        terminal([sys.executable, "-c", prompt_script, str(root / "scripts/lib"), str(project), str(number)], b"", b"\033[?1000l",
                 expected_exit=128 + number)
with patch.object(panel.shutil, "which", return_value="/x"), \
        patch.object(panel, "resolve_session", return_value=("legacy-panel", str(project))), \
        patch.object(panel, "recorded_room", return_value="r1"), patch.object(panel, "live_mains", return_value=1), \
        patch.object(panel, "managed_session", return_value=False), patch.object(panel, "run_single_native") as native_start:
    try:
        panel.run_native("claude", project)
        raise AssertionError("an inline launch bypassed a legacy main")
    except ValueError as error:
        assert "already has a live main" in str(error)
    assert not native_start.called
failed_worker = {"participant": "adv-worker", "provider": "codex", "role": "worker", "joined": True, "label": "Worker",
                 "parent": "adv-main", "seq": 2}
failed_board = dict(board, room={"id": "adv-room", "participants": [claude_main, failed_worker]}, attempts={"active_recent": [
    {"attempt_id": "adv-attempt", "state": "failed", "task_id": "adv-task", "panel": {"role": "worker", "room_id": "adv-room",
     "room_participant": "adv-worker", "label": "Broken patch"}}], "recent": []})
assert "Broken patch" in graph_view.render_graph(failed_board, 100, 40)
assert "Broken patch" in graph_view.render_graph(dict(failed_board, finalized={"other": "accepted"}), 100, 40)
assert "Broken patch" not in graph_view.render_graph(dict(failed_board, finalized={"adv-task": "accepted"}), 100, 40)
# A call missing from a complete active list for ten minutes may be folded as unknown; a just-joined one stays,
# and a truncated active list (the projection keeps 8 rows) proves nothing.
untracked = dict(board, attempts={"available": True, "active": 0, "active_recent": [], "recent": []},
                 room={"id": "adv-room", "participants": [claude_main, dict(
    failed_worker, label="Old patch", joined_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 900)))]})
assert "Old patch" not in graph_view.render_graph(untracked, 100, 40)
assert "Old patch" in graph_view.render_graph(dict(untracked, attempts=dict(untracked["attempts"], active=9)), 100, 40)
untracked["room"]["participants"][1]["joined_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
assert "Old patch" in graph_view.render_graph(untracked, 100, 40)
# Review and history grouping shares settler evidence and changes no report/ledger state.
grouped_board = deepcopy(failed_board)
grouped_board["collection"] = {"ok": True}
grouped_board["awaiting_admission"] = ["pending-patch"]
grouped_board["attempts"].update(available=True, active=0)
for ident, phase in (("review-call", "review"), ("pending-patch", "done"), ("past-call", "cancelled"),
                     ("blocked-call", "blocked"), ("waiting-call", "waiting_input"), ("live-call", "working")):
    grouped_board["room"]["participants"].append(dict(failed_worker, participant=ident, label=ident))
    grouped_board["attempts"]["recent"].append({"attempt_id": ident, "state": phase, "panel": {
        "room_id": "adv-room", "room_participant": ident, "role": "worker"}})
grouped_board["room"]["participants"].append(dict(failed_worker, participant="old-unknown", label="old-unknown",
                                               joined_at=minutes_ago(20)))
grouped_board["room"]["participants"].append(dict(failed_worker, participant="fresh-unknown", label="fresh-unknown",
                                               joined_at=minutes_ago(1)))
frozen_grouped = json.dumps(grouped_board, sort_keys=True)
classify = graph_view.call_classifier(grouped_board)
classified = {m["participant"]: classify(m) for m in graph_view.nodes(grouped_board) if m["role"] != "main"}
assert classified == {"adv-worker": "live", "review-call": "review", "pending-patch": "review", "past-call": "past",
                      "blocked-call": "live", "waiting-call": "live", "live-call": "live",
                      "old-unknown": "past", "fresh-unknown": "live"}, classified
# The owner's recorded decision on a task closes its review and its block; a failed decision does not.
for outcome, expected in (("completed", "past"), ("accepted", "past"), ("failed", None)):
    decided = graph_view.call_classifier(dict(grouped_board, finalized={"decided-task": outcome}))
    for ident in ("review-call", "blocked-call"):
        node = dict(next(m for m in graph_view.nodes(grouped_board) if m["participant"] == ident), task_id="decided-task")
        assert decided(node) == (expected or classified[ident]), (outcome, ident, decided(node))
# A stale admission flag cannot hide current work, uncertainty or an urgent failure.
mixed = deepcopy(grouped_board)
mixed["awaiting_admission"] = list(classified)
mixed_classify = graph_view.call_classifier(mixed)
assert {m["participant"]: mixed_classify(m) for m in graph_view.nodes(mixed) if m["role"] != "main"} == classified
from panel_view import inbox_items
mixed_inbox = {item["action"]: item["what"] for item in inbox_items(mixed)}
assert "failed" in mixed_inbox[("result", "adv-worker")]
assert ("result", "blocked-call") not in mixed_inbox  # No stale patch-review alert replaces its live board state.
assert "input" in mixed_inbox[("result", "waiting-call")]
assert ("result", "live-call") not in mixed_inbox and ("result", "fresh-unknown") not in mixed_inbox
for phase in ("working", "verifying", "waiting_input", "waiting_approval", "blocked", "failed", "timed_out", "orphaned"):
    candidate = dict(next(m for m in graph_view.nodes(mixed) if m["participant"] == "live-call"), state=phase)
    assert mixed_classify(candidate) == "live", phase
for evidence in ({"available": False, "active": 0}, {"available": True, "active": 9}, {"available": True, "active": None}):
    uncertain = dict(grouped_board, attempts=dict(grouped_board["attempts"], **evidence))
    unknown = next(m for m in graph_view.nodes(uncertain) if m["participant"] == "old-unknown")
    assert graph_view.call_classifier(uncertain)(unknown) == "live"
# Calls whose main is gone group like linked ones; the graph counts only the open ones.
orphans = deepcopy(grouped_board)
orphans["room"]["participants"] = [claude_main] + [dict(m, parent="gone-main") for m in grouped_board["room"]["participants"]
                                                  if m["participant"] in {"pending-patch", "past-call", "review-call", "live-call"}]
for mode in ("tree", "graph"):
    for glyphs in (True, False):
        nav = {"dismissed": True}
        picture = render(orphans, "codex", 120, 80, view=mode, unicode=glyphs, navigation=nav)
        if mode == "tree":
            assert ("result", "live-call") in nav["items"] and ("result", "past-call") not in nav["items"], nav
            assert "Past work (1)" in picture and "Needs review (2)" in picture, picture
            for kind in ("review", "past"):
                target = ("group", (kind, "unlinked"))
                hit = next(h for h in nav["hits"] if h["action"] == target)
                assert choose(("click", hit["x1"], hit["y"]), nav) == target
                panel.navigate(project, target, orphans, nav, 120)
            render(orphans, "codex", 120, 80, view=mode, unicode=glyphs, navigation=nav)
            assert ("result", "past-call") in nav["items"] and ("result", "review-call") in nav["items"], nav
        else:
            assert "3 calls with no known main (+1 past)" in picture, picture
for mode in ("tree", "graph"):
    nav = {"dismissed": True}
    picture = render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
    assert "Needs review (2)" in picture and "Past work (2)" in picture and "1 earlier unknown" in picture, picture
    for ident in ("review-call", "pending-patch", "past-call", "old-unknown"):
        assert ("result", ident) not in nav["items"], (mode, ident, nav)
    for ident in ("adv-worker", "blocked-call", "waiting-call", "live-call", "fresh-unknown"):
        assert ("result", ident) in nav["items"], (mode, ident, nav)
    mixed_nav = {"dismissed": True}
    mixed_picture = render(mixed, "codex", 120, 80, view=mode, navigation=mixed_nav)
    assert "Needs review (2)" in mixed_picture and "Past work (2)" in mixed_picture
    assert mixed_nav["items"] == nav["items"], (mode, mixed_nav)
    for kind in ("review", "past"):
        target = ("group", (kind, "adv-main"))
        hit = next(h for h in nav["hits"] if h["action"] == target)
        assert choose(("click", hit["x1"], hit["y"]), nav) == target
        assert choose(("enter",), nav) == choose((" ",), nav) == target
        with patch.object(panel.room, "status", side_effect=AssertionError("group read room")), \
                patch.object(panel_chats, "open_chat", side_effect=AssertionError("group opened chat")):
            panel.navigate(project, target, grouped_board, nav, 120)
        picture = render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
        assert ("result", "pending-patch" if kind == "review" else "old-unknown") in nav["items"]
    assert "patch awaits admission" in picture and "status unknown" in picture
    result_target = ("result", "pending-patch")
    result_hit = next(h for h in nav["hits"] if h["action"] == result_target)
    assert choose(("click", result_hit["x1"], result_hit["y"]), nav) == result_target
    assert choose(("enter",), nav) == result_target
    with patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel.room, "status", return_value=grouped_board["room"]):
        panel.navigate(project, result_target, grouped_board, nav, 120)
        assert nav.pop("opening") == result_target
    departed = deepcopy(grouped_board["room"])
    next(m for m in departed["participants"] if m["participant"] == "pending-patch")["joined"] = False
    with patch.object(panel, "managed_session", return_value=False), patch.object(panel.room, "status", return_value=departed):
        try:
            panel.navigate(project, result_target, grouped_board, nav, 120)
            raise AssertionError("departed history result was opened")
        except ValueError as error:
            assert "participant left" in str(error)
    assert nav["expanded_groups"] == {("review", "adv-main"), ("past", "adv-main")}
    if mode == "graph":
        assert "Esc Back to graph" in picture and "g Graph" in picture
        closing = dict(nav, opening=result_target)
        panel.reset_group_view(closing)
        assert "opening" not in closing and "expanded_groups" not in closing
    kept = panel.refreshed_navigation(nav, grouped_board)
    assert kept["expanded_groups"] == nav["expanded_groups"]
    reset = panel.refreshed_navigation(dict(nav), {"room": {"id": "different-room"}})
    assert not reset.get("expanded_groups")
    for columns, rows in ((120, 80), (76, 16), (28, 12), (2, 8)):
        for glyphs in (True, False):
            small = render(grouped_board, "codex", columns, rows, view=mode, unicode=glyphs, navigation=nav)
            assert len(small.splitlines()) <= rows and all(display_width(line) <= columns for line in small.splitlines())
    render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
    for kind in ("review", "past"):
        panel.navigate(project, ("group", (kind, "adv-main")), grouped_board, nav, 120)
    render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
    assert not nav["expanded_groups"] and nav["surface"] == mode
# Both existing return controls exit inspection; room data and other local preferences stay intact.
for back_key in ("escape", "g"):
    clock = [0.0]
    inspected = []
    real_frame = panel.frame_view
    def group_frame(*args, **kwargs):
        nav = kwargs["navigation"]
        if not inspected:
            nav.update(group_tree=True, expanded_groups={("review", "adv-main")}, pinned=["adv-main"],
                       collapsed={"unrelated-main"}, selected=("group", ("review", "adv-main")))
        picture = real_frame(*args, **kwargs)
        inspected.append(deepcopy(nav))
        return picture
    def return_graph(seconds):
        clock[0] += 5
        return [(back_key,)]
    with patch.object(panel, "BackgroundRead", ImmediateRead), \
            patch.object(panel, "read_panel", return_value=(grouped_board, 0, "codex", None, "graph", False, "")), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((120, 80))), \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel, "frame_view", side_effect=group_frame), \
            patch.object(ResponsiveInput, "wait", side_effect=return_graph), \
            patch("panel_input.TerminalInput", ResponsiveInput):
        assert panel.watch(project, "codex", count=2, view="graph", no_animation=True) == 0
    assert inspected[0]["surface"] == "tree" and inspected[-1]["surface"] == "graph"
    assert not inspected[-1].get("group_tree") and not inspected[-1].get("expanded_groups")
    assert inspected[-1]["pinned"] == ["adv-main"] and inspected[-1]["collapsed"] == {"unrelated-main"}
keyboard_nav = {"surface": "graph", "items": [("chat", "adv-main"), ("group", ("review", "adv-main"))],
                "selected": ("chat", "adv-main")}
choose(("down",), keyboard_nav)
assert choose(("enter",), keyboard_nav) == ("group", ("review", "adv-main")) and "preview" not in keyboard_nav
for columns, rows in ((120, 80), (76, 16)):
    published = dict(grouped_board, room=dict(grouped_board["room"], publications={"claude": {
        "status": "submitted", "delivery_unknown": True}}))
    shown = render(published, "codex", columns, rows, view="graph", navigation={
        "group_tree": True, "expanded_groups": {("review", "adv-main")}})
    assert "APP DELIVERY" in shown and "unconfirmed" in shown, shown
native_inspection = render(dict(grouped_board, native_advisors=open_call), "claude", 120, 80, view="graph",
                          navigation={"group_tree": True, "expanded_groups": {("review", "adv-main")}})
assert "Fable 5.1" in native_inspection and "answer stays in the main transcript" in native_inspection
attention_nav = {}
attention = render(grouped_board, "codex", 120, 80, view="tree", attention_only=True, navigation=attention_nav)
assert "Needs review (2)" in attention and "Past work" not in attention
from panel_view import inbox_items
assert {item["action"] for item in inbox_items(grouped_board)} >= {("result", "review-call"), ("result", "pending-patch")}
# Reading a call's own handoff handles its failure, but cannot settle a block or admit a patch.
for sender, recipient, pending, visible in (("adv-worker", "adv-main", [], False),
                                           ("adv-worker", "adv-main", ["adv-main"], True),
                                           ("blocked-call", "adv-main", [], True),
                                           ("adv-worker", "other-main", [], True)):
    handed_off = deepcopy(grouped_board)
    handed_off["room"]["messages"] = [{"id": "result-adv-worker", "message_kind": "handoff",
        "sender": sender, "recipient": recipient, "pending_for": pending}] + [
        {"id": "result-" + ident, "message_kind": "handoff", "sender": ident,
         "recipient": "adv-main", "pending_for": []} for ident in ("blocked-call", "review-call", "pending-patch")]
    frozen_handoff = json.dumps(handed_off, sort_keys=True)
    for mode in ("tree", "graph"):
        attention_nav = {"dismissed": True}
        attention_picture = render(handed_off, "codex", 120, 80, view=mode,
                                   attention_only=True, navigation=attention_nav)
        assert (("result", "adv-worker") in attention_nav["items"]) == visible, (mode, sender, recipient, pending, attention_nav)
        assert ("Broken patch" in attention_picture) == visible, (mode, sender, recipient, pending, attention_picture)
        assert ("result", "blocked-call") in attention_nav["items"] and "blocked-call" in attention_picture, (mode, attention_nav)
        assert "Needs review (2)" in attention_picture, (mode, attention_picture)
    assert json.dumps(handed_off, sort_keys=True) == frozen_handoff
for mark in ("completed", "accepted"):
    handled = dict(grouped_board, finalized={"adv-task": mark})
    failed = next(m for m in graph_view.nodes(handled) if m["participant"] == "adv-worker")
    assert graph_view.call_classifier(handled)(failed) == "past"
    for mode in ("tree", "graph"):
        filtered_nav = {"dismissed": True}
        filtered = render(handled, "codex", 120, 80, view=mode, attention_only=True, navigation=filtered_nav)
        assert "Broken patch" not in filtered and ("result", "adv-worker") not in filtered_nav["items"]
        assert ("result", "blocked-call") in filtered_nav["items"]
        history = render(handled, "codex", 120, 80, view=mode, navigation={"dismissed": True})
        assert "Past work" in history or mode == "graph"
    orphan_handled = deepcopy(handled)
    next(m for m in orphan_handled["room"]["participants"] if m["participant"] == "adv-worker")["parent"] = "gone-main"
    for mode in ("tree", "graph"):
        orphan_nav = {"dismissed": True}
        filtered = render(orphan_handled, "codex", 120, 80, view=mode, attention_only=True, navigation=orphan_nav)
        assert "Broken patch" not in filtered and ("result", "adv-worker") not in orphan_nav["items"]
        assert "call with no known main" not in filtered and "Calls with no known main" not in filtered
research_board = deepcopy(grouped_board)
research_board["room"]["participants"].append({"participant": "research-call", "role": "researcher",
    "provider": "codex", "model": "gpt-6-luna", "parent": "adv-main", "joined": True, "seq": 99})
research_board["attempts"]["active_recent"].append({"attempt_id": "research-attempt", "state": "working",
    "task_id": "research-task", "panel": {"role": "researcher", "room_id": "adv-room",
    "room_participant": "research-call", "model": "gpt-6-luna", "label": "Research citations"}})
for mode in ("tree", "graph"):
    for columns, rows in ((120, 80), (80, 24)):
        research_nav = {}
        candidate = deepcopy(research_board)
        if rows == 24:
            candidate['room']['participants'] = [m for m in candidate['room']['participants']
                if m['role'] == 'main' or m['participant'] == 'research-call']
        picture = render(candidate, "codex", columns, rows, view=mode, navigation=research_nav)
        assert "Research citations" in picture or "researcher" in picture.lower() or "RES" in picture, picture
        assert ("result", "research-call") in research_nav["items"], research_nav
assert json.dumps(grouped_board, sort_keys=True) == frozen_grouped
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
