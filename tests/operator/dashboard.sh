# shellcheck shell=bash
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
import io
import inspect
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
        "provider_usage": {"input_tokens": 5, "output_tokens": 2, "cache_read_tokens": 7, "cache_write_tokens": 1,
                           "cache_in_input": False, "PRIVATE-USAGE-KEY": "x", "output_tokens_detail": 9},
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
operator_core_block dashboard

before="$(git -C "$repo" status --porcelain=v1 --untracked-files=all)"
bash "$ROOT/scripts/state.sh" --repo "$repo" --json > "$TMP/shared-state.json"
bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json > "$TMP/inbox.json"
python3 - "$TMP/shared-state.json" "$TMP/inbox.json" "$ROOT" "$repo" <<'PY' || fail "inbox projection failed"
import copy
import contextlib
import io
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[3]) / "scripts/lib"))
from inbox_projection import project_inbox

snapshot = json.load(open(sys.argv[1], encoding="utf-8"))
inbox = json.load(open(sys.argv[2], encoding="utf-8"))
repo = os.path.realpath(sys.argv[4])
assert project_inbox(snapshot, include_threads=os.environ.get("OMS_THREAD_ATTENTION") != "0",
                     repo_path=repo) == inbox
assert inbox["items"][0]["priority"] == "P1", inbox
priorities = [item["priority"] for item in inbox["items"]]
assert priorities == sorted(priorities), priorities
assert "runtime-evidence-missing" in [item["code"] for item in inbox["items"]]
assert all(repo in item["command"] for item in inbox["items"]), inbox["items"]
recommended_state = copy.deepcopy(snapshot)
recommended_state.setdefault("runtime", {})["next_actions"] = [
    {"id": "show-evidence", "command": "oms runtime evidence show"}]
assert project_inbox(recommended_state, repo_path=repo)["recommended_actions"][0]["command"] == (
    "oms runtime --repo " + shlex.quote(repo) + " evidence show")
inbox_command_state = copy.deepcopy(snapshot)
inbox_command_state.setdefault("runtime", {})["next_actions"] = [
    {"id": "show-inbox", "command": "oms inbox"}]
inbox_command = project_inbox(inbox_command_state, repo_path=repo)["recommended_actions"][0]["command"]
assert shlex.split(inbox_command) == ["oms", "inbox", "--repo", repo], inbox_command
relative_repo_state = copy.deepcopy(snapshot)
relative_repo_state.setdefault("runtime", {})["next_actions"] = [
    {"id": "repair-empty-plan", "command": "oms plan-from-spec --repo ."},
    {"id": "repair-empty-plan-equals", "command": "oms plan-from-spec --repo=."}]
relative_commands = [action["command"] for action in project_inbox(
    relative_repo_state, repo_path=repo)["recommended_actions"]]
assert shlex.split(relative_commands[0]) == ["oms", "plan-from-spec", "--repo", repo], relative_commands
assert shlex.split(relative_commands[1]) == ["oms", "plan-from-spec", "--repo=" + repo], relative_commands
mailbox_repo = "/tmp/--repo .$(printf INJECTED)"
mailbox_args = ["oms", "room", "updates", "--repo", mailbox_repo,
                "--id", "room-fixture", "--participant", "call-fixture"]
mailbox_state = copy.deepcopy(snapshot)
mailbox_state["room_mail"] = {"bound": True, "healthy": True, "pending": 1,
                              "oldest_age_seconds": 0,
                              "command": shlex.join(mailbox_args)}
mailbox_report = project_inbox(mailbox_state, repo_path=mailbox_repo)
mailbox_item = next(item for item in mailbox_report["items"] if item["code"] == "pending-room-mail")
anchored_mailbox_args = ["OMS_ROOM_REPO=" + mailbox_repo] + mailbox_args
assert shlex.split(mailbox_item["command"]) == anchored_mailbox_args, mailbox_item
assert shlex.quote(mailbox_repo) in mailbox_item["command"], mailbox_item
mailbox_json = json.loads(json.dumps(mailbox_report))
assert shlex.split(next(item for item in mailbox_json["items"]
                        if item["code"] == "pending-room-mail")["command"]) == anchored_mailbox_args
printed_mail = io.StringIO()
with contextlib.redirect_stdout(printed_mail):
    project_inbox_module = __import__("inbox_projection")
    project_inbox_module.print_inbox(mailbox_report)
displayed_mail = next(line.split("  next: ", 1)[1] for line in printed_mail.getvalue().splitlines()
                      if "  next: " in line and "room-fixture" in line)
assert shlex.split(displayed_mail) == anchored_mailbox_args, displayed_mail
assert "$(printf INJECTED)" in shlex.quote(mailbox_repo)
unsafe_repositories = ["/tmp/repo\x1b[2J", "/tmp/repo\nname", "/tmp/repo\u200bname"]
from inbox_projection import COMMAND_HIDDEN_NOTICE
for unsafe_repo in unsafe_repositories:
    unsafe_args = ["oms", "room", "updates", "--repo", unsafe_repo,
                   "--id", "room-fixture", "--participant", "call-fixture"]
    unsafe_state = copy.deepcopy(snapshot)
    unsafe_state["room_mail"] = dict(mailbox_state["room_mail"],
                                     command=shlex.join(unsafe_args))
    unsafe_state["plan"] = {"stale": [{}]}
    unsafe_state["ci"] = {"state": "unpushed", "ahead": 1}
    unsafe_report = project_inbox(unsafe_state, repo_path=unsafe_repo)
    unsafe_item = next(item for item in unsafe_report["items"]
                       if item["code"] == "pending-room-mail")
    unsafe_command = unsafe_item["command"]
    assert shlex.split(unsafe_command) == ["OMS_ROOM_REPO=" + unsafe_repo] + unsafe_args, unsafe_command
    unsafe_json = json.loads(json.dumps(unsafe_report))
    json_command = next(item["command"] for item in unsafe_json["items"]
                        if item["code"] == "pending-room-mail")
    assert shlex.split(json_command) == ["OMS_ROOM_REPO=" + unsafe_repo] + unsafe_args
    plan_command = next(item["command"] for item in unsafe_report["items"]
                        if item["code"] == "stale-plan-claim")
    git_command = next(item["command"] for item in unsafe_report["items"]
                       if item["code"] == "unpushed-head")
    assert shlex.split(plan_command) == ["oms", "agent-plan", "--repo", unsafe_repo, "reclaim"]
    assert shlex.split(git_command) == ["git", "-C", unsafe_repo, "push"]
    json_commands = {item["code"]: shlex.split(item["command"]) for item in unsafe_json["items"]}
    assert json_commands["stale-plan-claim"] == ["oms", "agent-plan", "--repo", unsafe_repo, "reclaim"]
    assert json_commands["unpushed-head"] == ["git", "-C", unsafe_repo, "push"]
    terminal = io.StringIO()
    with contextlib.redirect_stdout(terminal):
        project_inbox_module.print_inbox(unsafe_report)
    terminal_text = terminal.getvalue()
    assert COMMAND_HIDDEN_NOTICE in terminal_text and unsafe_command not in terminal_text
    assert "oms room updates" not in terminal_text and "oms agent-plan" not in terminal_text
    assert "git push" not in terminal_text and "\x1b" not in terminal_text
conflicting_env = dict(os.environ, OMS_ROOM_REPO="/tmp/conflicting-repository")
mail_probe = """oms() {
  printf '%s\\0' "$OMS_ROOM_REPO"
  while [ "$#" -gt 1 ]; do
    if [ "$1" = --repo ]; then printf '%s\\0' "$2"; return; fi
    shift
  done
}
""" + displayed_mail
mail_result = subprocess.run(["bash", "-c", mail_probe], env=conflicting_env,
                             cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert mail_result == os.fsencode(mailbox_repo) + b"\0" + os.fsencode(mailbox_repo) + b"\0", mail_result
inbox_probe = """oms() { printf '%s\\0' "$PWD" "$OMS_ROOM_REPO" "$@"; }
""" + inbox_command
inbox_result = subprocess.run(["bash", "-c", inbox_probe], env=conflicting_env,
                              cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert inbox_result == (b"/tmp\0/tmp/conflicting-repository\0inbox\0--repo\0" +
                        os.fsencode(repo) + b"\0"), inbox_result

runtime_command = project_inbox(recommended_state, repo_path=repo)["recommended_actions"][0]["command"]
runtime_probe = """oms() {
  printf '%s\\0' "$PWD" "$OMS_ROOM_REPO"
  while [ "$#" -gt 0 ]; do
    if [ "$1" = --repo ]; then printf '%s\\0' "$2"; return; fi
    shift
  done
}
""" + runtime_command
runtime_result = subprocess.run(["bash", "-c", runtime_probe], env=conflicting_env,
                                cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert runtime_result == (b"/tmp\0/tmp/conflicting-repository\0" + os.fsencode(repo) + b"\0"), runtime_result

git_state = copy.deepcopy(snapshot)
git_state.setdefault("runtime", {})["next_actions"] = [
    {"id": "git-status", "command": "git status --short"}]
git_command = project_inbox(git_state, repo_path=repo)["recommended_actions"][0]["command"]
assert shlex.split(git_command) == ["git", "-C", repo, "status", "--short"], git_command
git_probe = """git() { printf '%s\\0' "$PWD" "$@"; }
""" + git_command
git_result = subprocess.run(["bash", "-c", git_probe], env=conflicting_env,
                            cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert git_result == b"/tmp\0-C\0" + os.fsencode(repo) + b"\0status\0--short\0", git_result

fallback_state = copy.deepcopy(snapshot)
fallback_state.setdefault("runtime", {})["next_actions"] = [
    {"id": "fallback-probe", "command": "oms ci-status"}]
fallback_command = project_inbox(fallback_state, repo_path=repo)["recommended_actions"][0]["command"]
assert fallback_command == "cd " + shlex.quote(repo) + " && oms ci-status", fallback_command
fallback_probe = """oms() { printf '%s\\0' "$PWD" "$OMS_ROOM_REPO" "$@"; }
""" + fallback_command
fallback_result = subprocess.run(["bash", "-c", fallback_probe], env=conflicting_env,
                                 cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert fallback_result.split(bytes((0,))) == [os.fsencode(repo), b"/tmp/conflicting-repository", b"ci-status", b""], fallback_result
snapshot["threads"] = {"stale_open": 2}
original = copy.deepcopy(snapshot)
thread_item = next(item for item in project_inbox(snapshot, repo_path=repo)["items"]
                   if item["code"] == "stale-threads")
thread_command = thread_item["command"]
assert shlex.split(thread_command) == ["oms", "thread", "--repo", repo, "list", "--stale"], thread_command
thread_probe = """oms() { printf '%s\\0' "$PWD" "$OMS_ROOM_REPO" "$@"; }
""" + thread_command
thread_result = subprocess.run(["bash", "-c", thread_probe], env=conflicting_env,
                                cwd="/tmp", stdout=subprocess.PIPE, check=True).stdout
assert thread_result == (b"/tmp\0/tmp/conflicting-repository\0thread\0--repo\0" +
                         os.fsencode(repo) + b"\0list\0--stale\0"), thread_result
muted = project_inbox(snapshot, include_threads=False, safe_actions=["reclaimed-stale-plan"], repo_path=repo)
assert not any(item["code"] == "stale-threads" for item in muted["items"])
assert muted["safe_actions"] == ["reclaimed-stale-plan"]
assert snapshot == original
PY
read_repo="$TMP/inbox repo with spaces"
mkdir -p "$read_repo"
git -C "$read_repo" init -q
blocked_attempt="$(bash "$ROOT/scripts/agent-events.sh" --repo "$read_repo" start \
  --provider codex --tool dashboard-fixture)" || fail "blocked inbox fixture start failed"
bash "$ROOT/scripts/agent-events.sh" --repo "$read_repo" transition \
  --attempt "$blocked_attempt" --state blocked >/dev/null || fail "blocked inbox fixture transition failed"
bash "$ROOT/scripts/inbox.sh" --repo "$read_repo" --json > "$TMP/read-inbox.json" || \
  fail "separate repository inbox query failed"
suggested_read="$(python3 - "$TMP/read-inbox.json" "$read_repo" <<'PY'
import json, sys
inbox = json.load(open(sys.argv[1], encoding="utf-8"))
import shlex
repo = shlex.quote(sys.argv[2])
item = next(item for item in inbox["items"] if item["code"] == "agent-blocked")
assert "--repo " + repo in item["command"], item
print(item["command"])
PY
)"
PATH="$ROOT/scripts:$PATH" bash -c "$suggested_read" > "$TMP/suggested-inside.txt" || \
  fail "suggested inbox read failed inside its repository"
(cd "$TMP" && PATH="$ROOT/scripts:$PATH" bash -c "$suggested_read") > "$TMP/suggested-outside.txt" || \
  fail "suggested inbox read failed outside its repository"
cmp -s "$TMP/suggested-inside.txt" "$TMP/suggested-outside.txt" || \
  fail "suggested inbox read changed when run from another directory"
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
                  "PRIVATE PATCH PATH", "change.patch", "PRIVATE-USAGE-KEY"):
    assert forbidden not in raw, forbidden
report_without_commands = json.loads(json.dumps(report))
for item in report_without_commands.get("attention", {}).get("items", []):
    item["command"] = ""
for action in report_without_commands.get("attention", {}).get("next", []):
    action["command"] = ""
assert sys.argv[4] not in json.dumps(report_without_commands), "dashboard leaked repository outside commands"
text_before_attention = open(sys.argv[2], encoding="utf-8").read().split("attention:", 1)[0]
assert sys.argv[4] not in text_before_attention, "dashboard leaked repository outside attention commands"
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
assert call["provider_usage"] == {"input_tokens": 5, "output_tokens": 2, "cache_read_tokens": 7,
                                  "cache_write_tokens": 1, "cache_in_input": False,
                                  "session_cumulative": False}, call
# Unknown model, tokens and cost stay unknown rather than turning into zero.
for key in ("selected_model", "served_model", "route_class", "tokens", "cost_usd", "provider_usage"):
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
bash "$ROOT/scripts/agent-events.sh" --repo "$TMP/panel-active" list --limit 8 --json > "$TMP/panel-active-list.json"
python3 - "$TMP/panel-active.json" "$TMP/panel-active-list.json" <<'PY' || fail "dashboard recent attempts differ from agent-events list --limit 8"
import json, sys
recent = json.load(open(sys.argv[1]))["attempts"]["recent"]
listed = json.load(open(sys.argv[2]))
assert len(listed) == 8, len(listed)
for key in ("attempt_id", "state", "provider", "tool", "updated_at"):
    assert [row[key] for row in recent] == [row[key] for row in reversed(listed)], key
PY
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
corrupt_text_before_attention="$(sed '/^attention:/,$d' "$TMP/dash-corrupt.txt")"
! printf '%s' "$corrupt_text_before_attention" | grep -F "$corrupt" > /dev/null || \
  fail "dashboard leaked the repo path outside recovery commands"

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
rows[-8]["provider_usage"] = {"input_tokens": -1, "output_tokens": "9", "cache_read_tokens": True, "cache_in_input": "yes"}
rows[-7]["provider_usage"] = ["not", "a", "mapping"]
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
# Malformed usage counts degrade to null instead of a made-up zero; a non-mapping usage is absent.
assert report["operations"]["recent"][7]["provider_usage"] == {
    "input_tokens": None, "output_tokens": None, "cache_read_tokens": None,
    "cache_write_tokens": None, "cache_in_input": None, "session_cumulative": False,
}, report["operations"]["recent"][7]
assert report["operations"]["recent"][6]["provider_usage"] is None
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
import copy, json, runpy, shlex, subprocess, sys
from pathlib import Path
module = runpy.run_path(sys.argv[1])
assert module["panel_metadata"]({"refs": {"panel_role": "researcher",
    "panel_room_participant": "research-call"}, "parent_attempt_id": "main-call"})["panel"]["room_participant"] == "research-call"
assert module["panel_metadata"]({"refs": {"panel_role": "unrecognized"}}) == {}
swapped = module["panel_metadata"]({"refs": {"panel_role": "worker", "panel_provider": "claude",
    "panel_model": "claude-sonnet-5-5", "panel_fallback": "provider-capacity"}})["panel"]
assert swapped["provider"] == "claude" and swapped["fallback"] == "provider-capacity", swapped
report = json.load(open(sys.argv[2], encoding="utf-8"))
report["operations"]["recent"] *= 4
report["attempts"]["recent"] *= 4
text = module["render"](report, 100, interval=5, height=24)
assert len(text.splitlines()) <= 24, text
assert "model=gpt-test route=explicit" in text, text
assert "acceptance:" in text and "attention:" in text and "detail line(s) hidden" in text, text
assert sum(max(1, (module["display_width"](line) + 99) // 100)
           for line in text.splitlines()) <= 24, text
assert module["display_width"](module["fit"]("한글" * 30, 40)) <= 40
long_command = "oms room updates --repo /canonical/repository --id room-123456789 --participant call-abcdefghijklmnopqrstuvwxyz"
report["attention"] = {"actionable": 1, "items": [{"priority": "P1", "code": "mail",
    "summary": "pending", "command": long_command}], "omitted": 0, "next": []}
standalone = module["render"](report, 60)
standalone_lines = standalone.splitlines()
command_label = next(i for i, line in enumerate(standalone_lines) if line.strip() == "next:")
assert standalone_lines[command_label + 1] == long_command, standalone
assert module["display_width"](standalone_lines[command_label + 1]) > 60, standalone
operation = report["operations"]["recent"][0]
operation["task_id"] = "task-full-123"
op_text = module["render"](report, 100)
op_line = next(line for line in op_text.splitlines() if "task-full-123" in line)
assert op_line.index("task-full-123") < op_line.index("model=")
assert module["display_width"](op_line) <= 100, op_line
narrow_text = module["render"](report, 60)
narrow_row = next(line for line in narrow_text.splitlines() if "task-full-123" in line)
assert "task=task-full-123" in narrow_row and module["display_width"](narrow_row) <= 60, narrow_row
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
state = json.load(open(Path(sys.argv[2]).parent / "shared-state.json", encoding="utf-8"))
state.setdefault("plan", {})["stale"] = [{}]
state.setdefault("artifacts", {}).setdefault("counts", {})["unresolved"] = 1
(snap / "state.json").write_text(json.dumps(state), encoding="utf-8")
base = Path(sys.argv[2]).parent
long_repo = str(base / ("repo  " + "r" * 32))
assert len(long_repo) == len(str(base)) + 1 + len("repo  " + "r" * 32)
Path(long_repo).mkdir()
rebuilt = module["build"](snap, "inspected elsewhere", repo_path=long_repo)
quoted_repo = shlex.quote(long_repo)
plan_command = next(item["command"] for item in rebuilt["attention"]["items"]
                    if item["code"] == "stale-plan-claim")
artifact_command = next(item["command"] for item in rebuilt["attention"]["items"]
                        if item["code"] == "unresolved-artifacts")
assert "--repo " + quoted_repo in plan_command, plan_command
assert len(artifact_command) > 200 and artifact_command.endswith("resolve-recovered --dry-run"), artifact_command
assert artifact_command in json.dumps(rebuilt), "dashboard JSON truncated a runnable command"
assert "--repo " + quoted_repo in artifact_command and "repo  " in artifact_command, artifact_command
minimal = copy.deepcopy(rebuilt)
minimal["attempts"] = {"available": True, "total": 0, "active": 0, "by_state": {},
                       "active_recent": [], "recent": []}
minimal["delegations"] = []
minimal["operations"] = {"available": True, "recent": [], "omitted": 0}
minimal["reviews"] = {"passed": 0, "failed": 0, "unknown": 0, "outcomes": 0, "seat_answers": 0}
minimal["acceptance"] = None
minimal["task"] = None
minimal["attention"] = {"actionable": 0, "items": [], "omitted": 0,
    "next": [{"id": "initialize_project", "authority": "repo_write", "command": artifact_command}]}
narrow = module["render"](minimal, 40)
narrow_lines = narrow.splitlines()
assert "initialize_project" in narrow and "repo_write" in narrow, narrow
command_start = next(i for i, line in enumerate(narrow_lines) if line == artifact_command)
assert command_start > 0 and "next decision: initialize_project" in narrow_lines[command_start - 1], narrow
assert "resolve-recovered --dry-run" in narrow_lines[command_start], narrow
assert long_repo in narrow and "repo  " in narrow, narrow
for unsafe_repo in ("/tmp/repo\x1b[2J", "/tmp/repo\nname", "/tmp/repo\u200bname"):
    unsafe_args = ["oms", "room", "updates", "--repo", unsafe_repo,
                   "--id", "room-fixture", "--participant", "call-fixture"]
    unsafe_state = {"room_mail": {
        "bound": True, "healthy": True, "pending": 1, "oldest_age_seconds": 0,
        "command": shlex.join(unsafe_args)},
        "plan": {"stale": [{}]}, "ci": {"state": "unpushed", "ahead": 1}}
    unsafe_inbox = module["project_inbox"](unsafe_state, repo_path=unsafe_repo)
    unsafe_dashboard = copy.deepcopy(minimal)
    unsafe_dashboard["attention"] = {"actionable": 1,
        "items": [{"priority": item["priority"], "code": item["code"],
                   "summary": item["summary"], "command": item["command"]}
                  for item in unsafe_inbox["items"]],
        "omitted": 0, "next": []}
    unsafe_terminal = module["render"](unsafe_dashboard, 160)
    assert module["COMMAND_HIDDEN_NOTICE"] in unsafe_terminal, unsafe_terminal
    assert "oms room updates" not in unsafe_terminal and "oms agent-plan" not in unsafe_terminal
    assert "git push" not in unsafe_terminal
    assert "\x1b" not in unsafe_terminal, unsafe_terminal
terminal_command = module["render"](minimal, 160)
terminal_lines = terminal_command.splitlines()
decision_prefix = "next decision: initialize_project (repo_write):"
command_start = next(i for i, line in enumerate(terminal_lines) if line.startswith(decision_prefix))
assert terminal_lines[command_start + 1] == artifact_command, terminal_command
assert long_repo in terminal_command and "repo  " in terminal_command, terminal_command
nbsp_repo = str(base / "repo\u00a0's name")
Path(nbsp_repo).mkdir()
nbsp_report = module["build"](snap, "inspected with NBSP", repo_path=nbsp_repo)
nbsp_item = next(item for item in nbsp_report["attention"]["items"]
                 if item["code"] == "stale-plan-claim")
assert r"\xc2\xa0" in nbsp_item["command"] and "\u00a0" not in nbsp_item["command"], nbsp_item
nbsp_terminal = module["render"](nbsp_report, 160)
nbsp_lines = nbsp_terminal.splitlines()
nbsp_summary = next(i for i, line in enumerate(nbsp_lines) if "stale-plan-claim" in line)
assert nbsp_lines[nbsp_summary + 1].strip() == "next:", nbsp_terminal
nbsp_display = nbsp_lines[nbsp_summary + 2]
assert nbsp_display == nbsp_item["command"], (nbsp_display, nbsp_item)
nbsp_probe = """oms() { printf '%s\\0' "$@"; }
""" + nbsp_display
nbsp_result = subprocess.run(["bash", "-c", nbsp_probe], stdout=subprocess.PIPE, check=True).stdout
assert nbsp_result == (b"agent-plan\0--repo\0" + nbsp_repo.encode("utf-8") + b"\0reclaim\0"), nbsp_result
height_limited = module["render"](minimal, 60, height=23)
height_lines = height_limited.splitlines()
terminal_rows = sum(max(1, (module["display_width"](line) + 59) // 60) for line in height_lines)
assert terminal_rows <= 23, (terminal_rows, height_limited)
diagnostic = copy.deepcopy(minimal)
diagnostic["acceptance"] = {"available": True, "total": 1,
    "counts": {"failed": 1}, "note": "fixture", "criteria": [{
        "status": "failed", "id": "criterion-1", "source": "test",
        "text": "failed criteria remains visible"}], "omitted": 0}
diagnostic["operations"] = {"available": True, "omitted": 0, "recent": [{
    "provider": "codex", "kind": "delegate", "task_id": None,
    "selected_model": "gpt-test", "route_class": "explicit", "fallback_used": False,
    "fallback_reason": None, "exit": 0, "status": "success", "served_model": "gpt-test",
    "tokens": 1, "ts": "2026-08-01T00:00:10Z"}]}
huge_repo = str(base / ("p" * 100) / ("q" * 100) / ("repo  " + "r" * 100))
huge_inbox = module["project_inbox"](
    {"artifacts": {"counts": {"unresolved": 1}}}, repo_path=huge_repo)
huge_command = next(item["command"] for item in huge_inbox["items"]
                    if item["code"] == "unresolved-artifacts")
diagnostic["attention"] = {"actionable": 1, "items": [{"priority": "P1",
    "code": "unresolved-artifacts", "summary": "long repository command",
    "command": huge_command}], "omitted": 0, "next": []}
diagnostic_text = module["render"](diagnostic, 60, height=23)
diagnostic_rows = sum(max(1, (module["display_width"](line) + 59) // 60)
                      for line in diagnostic_text.splitlines())
assert diagnostic_rows <= 23, (diagnostic_rows, diagnostic_text)
assert "failed criteria remains visible" in diagnostic_text, diagnostic_text
assert "route=explicit" in diagnostic_text, diagnostic_text
assert "omitted for space" in diagnostic_text and "oms inbox --json" in diagnostic_text, diagnostic_text
short_limited = module["render"](minimal, 60, height=10)
short_lines = short_limited.splitlines()
short_rows = sum(max(1, (module["display_width"](line) + 59) // 60) for line in short_lines)
assert short_rows <= 10 and "omitted for space" in short_limited, short_limited
for budget in (2, 3):
    tiny = module["render"](diagnostic, 20, height=budget)
    tiny_lines = tiny.splitlines()
    assert len(tiny_lines) <= budget, (budget, tiny)
    assert all(module["display_width"](line) <= 20 for line in tiny_lines), (budget, tiny)
PY

# Arguments: watch-only options, a sensible interval floor, and no JSON stream.
for bad in '--interval 5' '--count 1' '--json --watch' '--watch --interval 1' \
  '--watch --interval abc' '--watch --count 0'; do
  rc=0
  # shellcheck disable=SC2086 # deliberate word splitting of the case
  dash --repo "$repo" $bad >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "dashboard $bad should be a usage error, got $rc"
done
"$ROOT/scripts/oms" dashboard --help | grep -F 'Usage: dashboard.sh' > /dev/null || fail "oms dashboard --help failed"
# A non-terminal stream is appended to, never cleared.
dash --repo "$repo" --watch --interval 2 --count 2 > "$TMP/dash-watch.txt" || fail "dashboard watch failed"
[ "$(grep -c "^OMS dashboard .* refresh=2s$" "$TMP/dash-watch.txt")" = 2 ] || fail "watch did not refresh twice"
[ "$(grep -c "^----$" "$TMP/dash-watch.txt")" = 1 ] || fail "watch snapshots lack a separator"
! grep -q "$(printf "\033")" "$TMP/dash-watch.txt" || fail "watch erased a non-terminal stream"

trace_repo="$TMP/trace-repo"
mkdir -p "$trace_repo"
git -C "$trace_repo" init -q
git -C "$trace_repo" config user.email test@example.com
git -C "$trace_repo" config user.name test
printf 'trace\n' > "$trace_repo/README.md"
git -C "$trace_repo" add README.md
git -C "$trace_repo" commit -qm init
incoming="00-11111111111111111111111111111111-2222222222222222-01"
attempt="$(OMS_TRACEPARENT="$incoming" OMS_TRACESTATE="vendor=must-not-persist" \
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
[ "\${PANEL_REAL:-0}" != 1 ] || exec "$real_python" "\$@"
[ "\$count" -ge 2 ]
BOARD
chmod +x "$board_bin/python3"
TMUX=fixture OMS_PANEL_SESSION=oms-fixture PATH="$board_bin:$PATH" bash "$ROOT/scripts/panel.sh" --repo "$repo" --watch 2>"$TMP/board-err" ||
  fail "a crashed panel board must restart, not exit: $(cat "$TMP/board-err")"
[ "$(cat "$TMP/board-runs")" = 2 ] && grep -q 'OMS board stopped (exit 1); restarting' "$TMP/board-err" ||
  fail "the board should say it restarts and run again: $(cat "$TMP/board-err")"
rm -f "$TMP/board-runs"
mkdir -p "$TMP/non-repo-panel"
python3 - "$ROOT" "$TMP" <<'PY' || fail "a non-repository board must exit promptly without restarting"
import os
from pathlib import Path
import subprocess
import sys

root, temporary = map(Path, sys.argv[1:])
env = dict(os.environ, TMUX="fixture", OMS_PANEL_SESSION="oms-fixture", PANEL_REAL="1")
env["PATH"] = str(temporary / "board-bin") + os.pathsep + env.get("PATH", "")
try:
    result = subprocess.run(["bash", str(root / "scripts/panel.sh"), "--watch"],
                            cwd=temporary / "non-repo-panel", env=env,
                            capture_output=True, text=True, timeout=5)
except subprocess.TimeoutExpired:
    raise SystemExit("managed non-repository board looped instead of exiting")
assert result.returncode == 3, (result.returncode, result.stderr)
assert "is not a git repository" in result.stderr, result.stderr
assert (temporary / "board-runs").read_text().strip() == "1"
PY
rm -f "$TMP/board-runs"
if TMUX='' OMS_PANEL_SESSION=oms-fixture PATH="$board_bin:$PATH" bash "$ROOT/scripts/panel.sh" --repo "$repo" --watch 2>/dev/null; then
  fail "outside tmux a failed board must exit with its failure"
fi
[ "$(cat "$TMP/board-runs")" = 1 ] || fail "outside tmux the board must not loop"
