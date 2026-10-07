#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-autonomy-hook.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
export HOME="$TMP/home"
export XDG_CACHE_HOME="$TMP/cache"
export TMPDIR="$TMP/runtime"
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$TMPDIR"

# Every fixture here assumes operator-shell semantics: a harness child's
# inherited session identity suppresses auto-task and hook behavior by
# design, which is the invoker's state, not the fixture's. check.sh scrubs
# these for gate runs; scrub them here too so a direct run from a council
# seat or worker shell sees the same suite. A test that needs child
# semantics sets the variables explicitly.
unset OMS_HARNESS_CHILD OMS_HARNESS_ORIGIN OMS_HARNESS_PARENT_AGENT \
  OMS_HARNESS_CALL_ID OMS_STATE_REPO OMS_ATTEMPT_ID OMS_PLAN_LEASE_ID \
  OMS_LEASE_ID OMS_EXECUTOR_ID OMS_SOUL_SHA256 OMS_APPROVAL_ID \
  OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE OMS_HARNESS_DELEGATE_DEPTH \
  OMS_ROOM_ID OMS_ROOM_PARTICIPANT OMS_ROOM_REPO OMS_ROOM_ADMITTED_PARTICIPANT OMS_HOOK_AGENT OMS_AGENT TMUX TMUX_PANE
for oms_inherited in $(compgen -e | grep -E '^OMS_(PANEL|ROOM)_' || true); do unset "$oms_inherited"; done

fail() {
  echo "autonomy-hook-smoke: $*" >&2
  exit 1
}

test_classifier_boundaries() {
  python3 - "$ROOT/scripts/lib/hook_state.py" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("hook_state", sys.argv[1])
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)

cases = {
    "implement improvements": "task",
    "fix precision issue": "task",
    "consider constraints": "question",
    "fix CI failure": "release",
    "open a PR": "release",
    "train model": "research",
    "이 부분을 재구현해": "task",
    "배포판을 수정해": "release",
}
for prompt, expected in cases.items():
    actual = module.classify_prompt(prompt)["workflow"]
    if actual != expected:
        raise SystemExit(f"{prompt!r}: expected {expected}, got {actual}")

import os, tempfile
from pathlib import Path
from unittest.mock import Mock, patch
with tempfile.TemporaryDirectory() as directory:
    repo = Path(directory).resolve()
    other = repo / "other"
    other.mkdir()
    module.repo_root.cache_clear()
    git = Mock(return_value=Mock(returncode=0, stdout=str(repo) + "\n"))
    with patch.dict(os.environ, {"OMS_HOOK_RESOLVED_REPO": str(repo)}), patch.object(module.subprocess, "run", git):
        assert module.hook_repo({"cwd": str(other)}) == repo
        assert module.hook_repo({}) == repo
        assert git.call_count == 1, "one hook process resolved the same root twice"
        # Explicit missing and empty roots must not fall back to payload cwd.
        os.environ["OMS_HOOK_RESOLVED_REPO"] = str(repo / "missing")
        assert module.hook_repo({"cwd": str(other)}) is None
        os.environ["OMS_HOOK_RESOLVED_REPO"] = ""
        assert module.hook_repo({"cwd": str(other)}) is None
    module.repo_root.cache_clear()
PY
}

test_peer_tail_is_bounded() {
  python3 - "$ROOT/scripts/lib/hook_state.py" <<'PY'
import importlib.util
import io
import json
import sys
from datetime import datetime, timezone

spec = importlib.util.spec_from_file_location("hook_state", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
stamp = datetime.now(timezone.utc)
def row(session, hook="SessionStart"):
    return json.dumps({"session": session, "hook": hook, "ts": stamp.isoformat()}).encode() + b"\n"

class Tail(io.BytesIO):
    read_bytes = 0
    def read(self, size=-1):
        result = super().read(size)
        self.read_bytes += len(result)
        assert self.read_bytes <= 262144, "peer advisory read more than 256 KiB"
        return result
    def readlines(self, *args):
        raise AssertionError("peer advisory read the entire event history")

class Events:
    def __init__(self, data):
        self.data = data
    def open(self, *args, **kwargs):
        return Tail(self.data)

# Recent tail must survive a huge historical row and an unterminated last row.
history = row("old") + b"x" * 1048576 + b"\n"
data = history + row("ended") + row("ended", "SessionEnd") + row("live").rstrip(b"\n")
assert set(module.live_peer_sessions(Events(data), "me", stamp.timestamp())) == {"live"}
# Keep the existing row window even when many tiny records fit the byte bound.
data = row("outside") + b"{}\n" * 199 + row("inside")
assert set(module.live_peer_sessions(Events(data), "me", stamp.timestamp())) == {"inside"}
assert module.live_peer_sessions(Events(b""), "me", stamp.timestamp()) == {}
PY
}


route_prompt() {
  local repo="$1"
  local session="$2"
  local turn="$3"
  local prompt="$4"
  # The invoking shell may itself be a harness child (a council seat running
  # this suite); its session identity and capability variables are not part
  # of any fixture. Scrub them so the route sees only what the test sets.
  (
    unset OMS_HARNESS_CHILD OMS_HARNESS_ORIGIN OMS_HARNESS_PARENT_AGENT \
      OMS_HARNESS_CALL_ID OMS_STATE_REPO OMS_ATTEMPT_ID OMS_PLAN_LEASE_ID \
      OMS_LEASE_ID OMS_EXECUTOR_ID OMS_SOUL_SHA256 OMS_APPROVAL_ID \
      OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE
    OMS_AUTO_TASK=1 OMS_AGENT=test OMS_HOOK_PAYLOAD="$(
      python3 - "$repo" "$session" "$turn" "$prompt" <<'PY'
import json, sys
print(json.dumps({"cwd": sys.argv[1], "session_id": sys.argv[2],
                  "turn_id": sys.argv[3], "prompt": sys.argv[4]}))
PY
    )" python3 "$ROOT/scripts/lib/hook_state.py" route --manifest "$repo/manifest.json"
  )
}

task_id() {
  awk '$1 == "-" && $2 == "task_id:" { print $3; exit }' "$1/.oms/task/current.md"
}

state_bullets() {
  awk '/^## Current State$/{inside=1; next} /^## /{inside=0} inside && /^- /{count++} END{print count+0}' \
    "$1/.oms/task/current.md"
}

test_explicit_goal_rotation() {
  local repo="$TMP/repo"
  local first_id second_id before after archives
  mkdir -p "$repo"
  git -C "$repo" init -q
  printf '{"skills":[]}\n' > "$repo/manifest.json"

  route_prompt "$repo" session-a turn-1 "Goal: Ship alpha"
  first_id="$(task_id "$repo")"
  [ -n "$first_id" ] || fail "initial explicit goal did not create a task"
  before="$(state_bullets "$repo")"

  route_prompt "$repo" session-b turn-2 "Objective:   ship   ALPHA"
  [ "$(task_id "$repo")" = "$first_id" ] || fail "equivalent explicit goal rotated"
  after="$(state_bullets "$repo")"
  [ "$after" = "$before" ] || fail "equivalent explicit goal was appended instead of deduped"

  route_prompt "$repo" session-b turn-3 "Goal: Ship alpha
Constraint: preserve API"
  [ "$(task_id "$repo")" = "$first_id" ] || fail "same goal with new constraint rotated"
  after_constraint="$(state_bullets "$repo")"
  [ "$after_constraint" -eq $((before + 1)) ] || fail "same goal dropped new prompt content"
  grep -Fq 'Constraint: preserve API' "$repo/.oms/task/current.md" || fail "new constraint was not recorded"

  route_prompt "$repo" session-c turn-4 "Objective: Ship beta"
  second_id="$(task_id "$repo")"
  [ "$second_id" != "$first_id" ] || fail "different explicit goal did not rotate"
  grep -Fqx 'Ship beta' "$repo/.oms/task/current.md" || fail "rotated task missed the new goal"
  archives="$(find "$repo/.oms/task/archive" -type f -name "$first_id-*.md" | wc -l | tr -d ' ')"
  [ "$archives" = 1 ] || fail "rotation did not archive the prior task exactly once"

  before="$(state_bullets "$repo")"
  route_prompt "$repo" session-d turn-5 "Continue with the next implementation step"
  [ "$(task_id "$repo")" = "$second_id" ] || fail "ordinary continuation rotated the task"
  after="$(state_bullets "$repo")"
  [ "$after" -eq $((before + 1)) ] || fail "ordinary continuation was not appended"
}

hook_payload() {  # COMMAND EXIT EVENT
  python3 - "$@" <<'PY'
import json, sys
print(json.dumps({
    "hook_event_name": sys.argv[3], "tool_name": "Bash",
    "tool_input": {"command": sys.argv[1]},
    "tool_response": {"exit_code": int(sys.argv[2])},
}))
PY
}

ledger_rows() {
  [ -f "$1/.oms/failures.jsonl" ] || { echo 0; return 0; }
  wc -l < "$1/.oms/failures.jsonl" | tr -d ' '
}

# The record/resolve pair has to close in the writer that opened it: rows the
# hook files on failure used to stay OPEN until a human swept them, polluting
# the resume hook, the inbox, and every advise prompt in between.
test_fail_ledger_hook_resolves_on_success() {
  local repo="$TMP/hook-ledger"
  local hook="$ROOT/scripts/fail-ledger-hook.sh"
  local cmd="bash $repo/scripts/check.sh focused"
  local failed passed rows out

  mkdir -p "$repo/.oms"
  git -C "$repo" init -q
  git -C "$repo" -c user.email=test@example.com -c user.name=Test \
    commit -q --allow-empty -m base
  failed="$(hook_payload "$cmd" 2 PostToolUseFailure)"
  passed="$(hook_payload "$cmd" 0 PostToolUse)"

  # A repo with no failure history pays one stat and writes nothing.
  out="$(cd "$repo" && printf '%s' "$passed" | bash "$hook")" ||
    fail "the hook must never exit nonzero"
  [ -z "$out" ] || fail "a success is not agent context: $out"
  [ ! -e "$repo/.oms/failures.jsonl" ] || fail "a success seeded a ledger"

  (cd "$repo" && printf '%s' "$failed" | bash "$hook") >/dev/null ||
    fail "a first failure has nothing to say, so the hook must exit 0"
  # The repeat speaks — on stderr with exit 2, the non-blocking channel Claude
  # Code shows to the model on this event; stdout would reach only the log.
  if (cd "$repo" && printf '%s' "$failed" | bash "$hook") >/dev/null 2>"$repo/hook-ledger.err"; then
    fail "a repeated failure must surface ledger speech through exit 2"
  fi
  grep -Fq 'oms advise' "$repo/hook-ledger.err" ||
    fail "the repeat should carry the ledger's advise hint: $(cat "$repo/hook-ledger.err")"
  [ "$(ledger_rows "$repo")" = 2 ] || fail "two failures should file two rows"

  out="$(cd "$repo" && printf '%s' "$passed" | bash "$hook")" ||
    fail "the success side must never exit nonzero"
  [ -z "$out" ] || fail "the resolve receipt is bookkeeping, not context: $out"
  if bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved |
    grep -Fq 'check.sh focused'; then
    fail "the passing command should have resolved its own rows"
  fi

  # Nothing open means nothing to say: no resolve-row bloat on every green run.
  rows="$(ledger_rows "$repo")"
  (cd "$repo" && printf '%s' "$passed" | bash "$hook") >/dev/null
  [ "$(ledger_rows "$repo")" = "$rows" ] ||
    fail "a success with no open failure must append nothing"

  # A nonzero exit on the success event belongs to the failure event, which
  # already recorded it — neither side may act on it here.
  (cd "$repo" && printf '%s' "$(hook_payload "$cmd" 7 PostToolUse)" | bash "$hook") >/dev/null
  [ "$(ledger_rows "$repo")" = "$rows" ] ||
    fail "a failing command on the success event must not write"

  # Both opt-outs: the narrow one silences only resolution, the old one the
  # whole script.
  (cd "$repo" && printf '%s' "$failed" | bash "$hook") >/dev/null
  rows="$(ledger_rows "$repo")"
  (cd "$repo" && printf '%s' "$passed" | OMS_FAIL_LEDGER_RESOLVE=0 bash "$hook") >/dev/null
  [ "$(ledger_rows "$repo")" = "$rows" ] || fail "OMS_FAIL_LEDGER_RESOLVE=0 still resolved"
  (cd "$repo" && printf '%s' "$passed" | OMS_FAIL_LEDGER_HOOK=0 bash "$hook") >/dev/null
  [ "$(ledger_rows "$repo")" = "$rows" ] || fail "OMS_FAIL_LEDGER_HOOK=0 still resolved"
  (cd "$repo" && printf '%s' "$passed" | bash "$hook") >/dev/null
  if bash "$ROOT/scripts/fail-ledger.sh" --repo "$repo" list --unresolved |
    grep -Fq 'check.sh focused'; then
    fail "a later pass should still resolve after the opt-outs"
  fi

  # An unadopted repo stays untouched on the success side too.
  mkdir -p "$TMP/hook-plain"
  out="$(cd "$TMP/hook-plain" && printf '%s' "$passed" | bash "$hook")"
  [ -z "$out" ] || fail "an unadopted repo gets no ledger speech: $out"
  [ ! -d "$TMP/hook-plain/.oms" ] || fail "the hook must not seed .oms"
}

test_route_is_hermetic_to_inherited_harness_session() {
  local repo="$TMP/repo-child-env"
  mkdir -p "$repo"
  git -C "$repo" init -q
  printf '{"skills":[]}\n' > "$repo/manifest.json"

  # A council seat or delegated worker that runs this suite inherits its own
  # session capability variables, and OMS_HARNESS_CHILD=1 suppresses auto-task
  # by design. The invoker's identity is not part of any fixture: the recorded
  # 2026-08-10 red gate was exactly a seat-run suite failing here while the
  # operator's gate was green.
  OMS_HARNESS_CHILD=1 OMS_HARNESS_ORIGIN=ask OMS_STATE_REPO="$TMP/elsewhere" \
    OMS_PLAN_LEASE_ID=lease_test OMS_ATTEMPT_ID=attempt_test \
    route_prompt "$repo" session-child turn-1 "Goal: Ship hermetic"
  [ -n "$(task_id "$repo")" ] ||
    fail "inherited harness-child identity suppressed the fixture's auto-task"
  bash -s -- "$ROOT" "$repo" <<'SH' || fail "room identity leaked from parent into a generic call"
. "$1/scripts/lib/peer-common.sh"
export OMS_ROOM_ID=team OMS_ROOM_PARTICIPANT=sol OMS_ROOM_REPO="$2"
unset OMS_ROOM_ADMITTED_PARTICIPANT
ma_export_child_env codex consult "$2" fixture read
[ -z "${OMS_ROOM_PARTICIPANT:-}" ] && [ -z "${OMS_ROOM_ID:-}" ] && [ -z "${OMS_ROOM_REPO:-}" ]
export OMS_ROOM_ID=team OMS_ROOM_PARTICIPANT=worker OMS_ROOM_REPO="$2" OMS_ROOM_ADMITTED_PARTICIPANT=worker
ma_export_child_env claude delegate "$2" fixture read
[ "$OMS_ROOM_PARTICIPANT" = worker ] && [ "$OMS_ROOM_ID" = team ] && [ "$OMS_ROOM_REPO" = "$2" ]
SH
}

test_live_thread_delivery_at_existing_safe_points() {
  python3 - "$ROOT" "$TMP/live-hook" <<'PY'
import json, os, pathlib, subprocess, sys, time
root, repo = map(pathlib.Path, sys.argv[1:])
repo.mkdir()
subprocess.run(["git", "init", "-q", str(repo)], check=True)
sys.path.insert(0, str(root / "scripts/lib"))
import hook_state
thread = ["bash", str(root / "scripts/thread.sh"), "--repo", str(repo)]
def call(*args):
    return subprocess.run(thread + list(args), capture_output=True, text=True, check=True).stdout
def payload(session, provider=None):
    result = {"cwd": str(repo), "session_id": session, "hook_event_name": "PostToolUse",
              "tool_name": "apply_patch", "tool_input": {"command": "*** Begin Patch\n*** Update File: a.py\n*** End Patch"}}
    if provider == "codex":
        result["turn_id"] = "fixture-codex-turn"
    return result
call("new", "--id", "ordinary", "--topic", "old discussion")
assert not hook_state.live_thread_hint(payload("codex")), "ordinary threads are not subscriptions"
call("new", "--id", "live", "--live", "--topic", "Caller contract")
call("append", "--id", "live", "--role", "question", "--text", "Can the caller accept named options?")
first = hook_state.live_thread_hint(payload("codex"))
assert "named options" in first and "untrusted peer data" in first
assert not hook_state.live_thread_hint(payload("codex")), "same session must not replay"
assert "named options" in hook_state.live_thread_hint(payload("claude")), "each session needs its own cursor"
path = repo / ".oms/threads/live.jsonl"
assert not any(json.loads(line).get("receipt") for line in path.read_text().splitlines())
(repo / "a.py").write_text("def broken(:\n")
call("append", "--id", "live", "--role", "decision", "--text", "Keep backward compatibility in a.py.")
proc = subprocess.run(["bash", str(root / "scripts/syntax-guard-hook.sh")], cwd=repo,
                      input=json.dumps(payload("codex")), capture_output=True, text=True, check=True)
message = json.loads(proc.stdout)["hookSpecificOutput"]["additionalContext"]
assert "backward compatibility" in message and "does not parse" in message
assert not hook_state.live_thread_hint(payload("codex")), "edit and prompt delivery must share the cursor"
# A session that has not acked the thread gets answers as a bounded preview and
# every other role whole; acking makes it a participant that gets answers whole.
call("ack", "--id", "live", "--after", first.rsplit("--after ", 1)[1].strip(),
     "--consumer", hook_state.session_hash(payload("codex")))
call("append", "--id", "live", "--role", "answer", "--text", "가" * 300 + "tail-of-answer")
stray = repo.parent / "stray-repo"
(stray / ".oms/threads").mkdir(parents=True)
(stray / ".oms/threads/x.jsonl").write_text('["receipt"]\n{"receipt": "ack", "consumer": "c1", "after": "z"}\n')
assert hook_state.thread_participant(stray, "x", "c1"), "a stray non-object row must not break the ack scan"
assert not hook_state.thread_participant(stray, "x", "c2")
real_relation = hook_state.thread_relation
def broken(*args):
    raise ValueError("thread identity changed")
hook_state.thread_relation = broken
assert not hook_state.live_thread_hint(payload("claude")), "a failed participation check delivers nothing"
hook_state.thread_relation = real_relation
glance = hook_state.live_thread_hint(payload("claude"))
assert "backward compatibility" in glance, "a failed check must not advance the cursor; decisions arrive whole"
assert "tail-of-answer" not in glance and '"text_bytes": 914' in glance, glance
assert "--max-bytes 65536" in glance and "ack to receive answers whole" in glance, glance
assert "tail-of-answer" in hook_state.live_thread_hint(payload("codex")), "a participant gets answers whole"
# The session that asked already holds the answers through its own command:
# only other sessions receive them, and non-answer turns still reach it.
asker = dict(os.environ, OMS_AGENT="claude", CLAUDE_CODE_SESSION_ID="asker")
subprocess.run(thread + ["append", "--id", "live", "--role", "question", "--text", "Owner question?"],
               env=asker, capture_output=True, text=True, check=True)
assert "Owner question?" not in hook_state.live_thread_hint(payload("asker")), "the asker must not get its own question back"
call("append", "--id", "live", "--role", "answer", "--text", "owner-answer-body")
call("append", "--id", "live", "--role", "note", "--text", "owner-seat-note")
call("append", "--id", "live", "--role", "decision", "--text", "owner-sees-decisions")
owned = hook_state.live_thread_hint(payload("asker"))
assert "owner-sees-decisions" in owned and "owner-answer-body" not in owned and "owner-seat-note" not in owned, owned
assert not hook_state.live_thread_hint(payload("asker")), "suppressed turns must not replay"
assert "owner-answer-body" in hook_state.live_thread_hint(payload("codex")), "other sessions still get answers"
hook_state.live_thread_hint(payload("claude"))
large = "\\" * 3000
for suffix in ("one", "two"):
    call("append", "--id", "live", "--role", "answer", "--text", large + suffix)
call("append", "--id", "live", "--role", "note", "--text", "after-large-turns")
seen = []
for expected in (large + "one", large + "two", "after-large-turns"):
    message = hook_state.live_thread_hint(payload("codex"))
    assert "needs inspection" not in message, "a valid stored turn must not wedge live delivery"
    turns = json.loads(message.splitlines()[1])["turns"]
    assert [row["text"] for row in turns] == [expected], "oversized delivery must stop after one row"
    seen.extend(turns)
    assert len(message.encode()) < 68000, "delivery must retain a bounded output"
assert [row["text"] for row in seen] == [large + "one", large + "two", "after-large-turns"]
assert not hook_state.live_thread_hint(payload("codex")), "delivered turns must not replay"
# Ordinary messages still use the existing 6000-byte batch budget.
ordinary = ["m" * 3500 + suffix for suffix in ("one", "two")]
for text in ordinary:
    call("append", "--id", "live", "--role", "note", "--text", text)
for text in ordinary:
    message = hook_state.live_thread_hint(payload("codex"))
    turns = json.loads(message.splitlines()[1])["turns"]
    assert [row["text"] for row in turns] == [text], "normal delivery must keep the 6000-byte batch budget"
assert not hook_state.live_thread_hint(payload("codex"))
# Explicit CLI byte budgets remain strict even when hooks can recover a large row.
small = subprocess.run(thread + ["updates", "--id", "live", "--max-bytes", "10"],
                       capture_output=True, text=True)
assert small.returncode, "an explicit user byte limit must not silently grow"
code = "\r\n    def nested():  \r\n\r\n        return 1  \r\n"
expected_code = "\n    def nested():  \n\n        return 1  "
code_file = repo / "snippet.txt"
code_file.write_bytes(code.encode("utf-8"))
call("append", "--id", "live", "--role", "note", "--text-file", str(code_file))
saved = json.loads(path.read_text().splitlines()[-1])["text"]
assert saved == expected_code, "file-based turns must retain indentation, blank lines and trailing spaces"
message = hook_state.live_thread_hint(payload("codex"))
assert json.loads(message.splitlines()[1])["turns"][0]["text"] == expected_code
# Oversize recovery must never skip invalid history or advance its cursor.
state_path = repo / ".oms/hooks/sessions" / (hook_state.session_hash(payload("codex")) + ".thread.json")
invalid_rows = [
    b"{" + b" " * 6100 + b"\n",
    (json.dumps({"thread": "other", "text": large}) + "\n").encode(),
    (json.dumps({"thread": "live", "text": "x" * 65536}) + "\n").encode(),
]
for invalid in invalid_rows:
    prefix = path.read_bytes()
    cursor = json.loads(state_path.read_text())["cursor"]
    with path.open("ab") as handle:
        handle.write(invalid)
    assert "needs inspection" in hook_state.live_thread_hint(payload("codex"))
    state = json.loads(state_path.read_text())
    assert state["cursor"] == cursor and state["delivery_error"], "invalid history must not advance delivery"
    assert not hook_state.live_thread_hint(payload("codex")), "repeat inspection warnings must be deduplicated"
    # Repair only the unconsumed suffix in this isolated fixture, preserving the
    # file identity and consumed prefix to exercise the existing error latch.
    path.write_bytes(prefix)
    call("append", "--id", "live", "--role", "note", "--text", large + "recovered")
    call("append", "--id", "live", "--role", "note", "--text", "after-recovery")
    for expected in (large + "recovered", "after-recovery"):
        message = hook_state.live_thread_hint(payload("codex"))
        assert [row["text"] for row in json.loads(message.splitlines()[1])["turns"]] == [expected]
    assert not json.loads(state_path.read_text()).get("delivery_error"), "successful delivery must clear the error latch"
    assert not hook_state.live_thread_hint(payload("codex")), "recovered turns must not replay"
# A first row that is valid JSON but not an object is not a live thread; it
# must not raise out of the hook (found by an ask seat during an A/B round).
original = path.read_bytes()
path.write_bytes(b'["live"]\n' + original.split(b"\n", 1)[1])
assert not hook_state.live_thread_hint(payload("new"))
path.write_bytes(original)
os.environ["OMS_LIVE_COLLAB"] = "0"
assert not hook_state.live_thread_hint(payload("new"))
os.environ.pop("OMS_LIVE_COLLAB")
os.environ["OMS_HARNESS_CHILD"] = "1"
assert not hook_state.live_thread_hint(payload("child")), "do not give workers a new state capability"
os.environ.pop("OMS_HARNESS_CHILD")
call("close", "--id", "live")
assert not hook_state.live_thread_hint(payload("new"))
assert not hook_state.live_thread_hint({"cwd": str(repo)}), "no shared nosession cursor"
# Rooms deliver only to admitted native sessions, even when CURRENT changes.
import room
room.create(repo, "team", "Shared caller work")
room.join(repo, "team", "sol", "codex", model="gpt-6-sol", native_session="room-sol")
room.join(repo, "team", "opus", "claude", model="claude-opus-5-5", native_session="room-opus")
room.send(repo, "team", "sol", "opus", "Inspect the caller signature", "question", message_id="caller-question")
call("new", "--id", "different-room-topic", "--live", "--topic", "Separate task")
message = hook_state.live_thread_hint(payload("room-opus"))
assert "caller-question" in message and "different-room-topic" not in message, message
assert not hook_state.live_thread_hint(payload("room-opus")), "room safe points share their cursor"
assert "caller-question" not in hook_state.live_thread_hint(payload("room-sol", "codex")), "no sender echo"
room.send(repo, "team", "sol", "opus", "Inspect the caller signature", "question", message_id="caller-question")
assert room.status(repo, "team")["message_count"] == 1, "stable sends must deduplicate"
assert room.status(repo, "team")["pending_count"] == 1, "delivery is not consumption"
room.acknowledge(repo, "team", "opus", ["caller-question"])
room.send(repo, "team", "opus", "sol", "Caller contract confirmed", "answer", reply_to="caller-question")
assert room.status(repo, "team")["answered_count"] == 1
assert "Caller contract confirmed" in hook_state.live_thread_hint(payload("room-sol", "codex"))
room.join(repo, "team", "later", "grok", "worker", native_session="room-later", parent="sol")
assert not room.updates(repo, "team", "later")["turns"], "joining does not replay earlier broadcasts or direct mail"
room.send(repo, "team", "sol", "all", "Shared review note", message_id="shared-note")
assert room.status(repo, "team")["pending_count"] == 3  # reply to Sol plus two broadcast recipients
room.join(repo, "team", "after-note", "antigravity", native_session="room-after-note")
assert not room.updates(repo, "team", "after-note")["turns"], "broadcast audience is fixed at send time"
for who, to, text, kind, reply in [("stranger", "opus", "Forged", "note", None),
                                   ("sol", "missing", "Missing recipient", "note", None),
                                   ("later", "sol", "Unaddressed answer", "answer", "caller-question")]:
    try:
        room.send(repo, "team", who, to, text, kind, reply)
    except ValueError:
        pass
    else:
        raise AssertionError("room accepted invalid sender, recipient or reply")
os.environ.update(OMS_HARNESS_CHILD="1", OMS_ROOM_ID="team", OMS_ROOM_PARTICIPANT="later")
room.send(repo, "team", "later", "sol", "Scoped worker observation")
for operation in (lambda: room.send(repo, "team", "opus", "sol", "Forged worker identity"),
                  lambda: room.join(repo, "team", "escalated", "codex", "main")):
    try:
        operation()
    except ValueError:
        pass
    else:
        raise AssertionError("worker widened room identity")
for key in ("OMS_HARNESS_CHILD", "OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT"):
    os.environ.pop(key)
room.create(repo, "isolation", "Unrelated task")
assert not hook_state.live_thread_hint(payload("room-stranger")), "CURRENT does not enroll strangers into rooms"
# The hook skips mail its participant already consumed, yet moves its cursor past it.
room.create(repo, "acked", "Consumed mail")
room.join(repo, "acked", "writer", "codex", model="gpt-6-sol", native_session="ack-writer")
room.join(repo, "acked", "reader", "claude", model="claude-opus-5-5", native_session="ack-reader")
def ack_turns(*mids, ack=()):
    for mid in mids:
        room.send(repo, "acked", "writer", "reader", "text " + mid, "note", message_id=mid)
    if ack:
        room.acknowledge(repo, "acked", "reader", list(ack))
    out = hook_state.live_thread_hint(payload("ack-reader"))
    return [row["room_event"]["id"] for row in json.loads(out.splitlines()[1])["turns"]] if out else []
assert ack_turns("old-1", "new-1", ack=["old-1"]) == ["new-1"], "acked mail is not re-shown before the first delivery"
assert ack_turns() == [], "the cursor moved past consumed rows"
assert ack_turns("part-1", "part-2", "part-3", ack=["part-2"]) == ["part-1", "part-3"], "partial batch shows the unconsumed rows"
assert ack_turns("all-1", ack=["all-1"]) == [], "a fully consumed batch prints nothing"
assert ack_turns("next-1") == ["next-1"], "mail after consumed rows still arrives"
# A backlog older than status()'s newest-12 window is filtered too.
backlog = ["deep-%02d" % n for n in range(20)]
assert ack_turns(*backlog, ack=backlog[:18]) == backlog[18:], "an acked backlog beyond the newest 12 was re-shown"
assert room.status(repo, "acked")["pending_count"] == 6, "delivery still acks nothing"
# Open questions escalate on their own; a linked answer stops every tier. The clock is the room's own time function.
import time as clock_time
real_now = room.now
base = clock_time.time() + 20  # slack: message stamps are whole seconds taken after the setup
at = lambda minutes: setattr(room, "now", lambda: base + minutes * 60)
room.create(repo, "asks", "Open questions")
room.join(repo, "asks", "asker", "codex", model="gpt-6.1-sol", native_session="ask-asker")
room.join(repo, "asks", "target", "claude", model="claude-opus-5-5", native_session="ask-target")
asker, target = (lambda: hook_state.live_thread_hint(payload("ask-asker", "codex"))), (lambda: hook_state.live_thread_hint(payload("ask-target")))
room.send(repo, "asks", "asker", "target", "SECRET-QUESTION-TEXT", "question", message_id="ask-q1")
at(0.5)
first = target().splitlines()
assert first[-1].startswith("[oms room] 1 question to you is open: ask-q1 from Sol 6.1 (0 min)") and "SECRET" not in first[-1], first
assert "--reply-to ask-q1" in first[-1] and "--to asker --kind answer" in first[-1], first
at(4)
assert not target() and not asker(), "under five minutes nothing repeats on a tool hook"
kept = {k: os.environ.pop(k, None) for k in ("OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT")}
os.environ.update(OMS_ROOM_ID="asks", OMS_ROOM_PARTICIPANT="target")
prompted = hook_state.live_thread_hint(dict(payload("ask-target"), hook_event_name="UserPromptSubmit"))
assert "ask-q1 from Sol 6.1 (4 min)" in prompted, ("a panel main hears its open question on every prompt", prompted)
for name, value in kept.items():
    os.environ.pop(name, None)
    if value is not None:
        os.environ[name] = value
at(6)
lines = target().splitlines()
assert lines[0].startswith("[oms room] 1 question to you is open: ask-q1") and "overdue" not in lines[0], lines
assert lines[1] == "[oms room] 1 delivered message not acknowledged: oms room ack --id asks --participant target --message ask-q1", lines
assert not asker(), "the asker hears nothing before ten minutes"
at(11)
told = asker()
assert told == "Your question ask-q1 to Opus 5.5 has no answer for 11 min; it was re-sent as a reminder.", told
assert "reminder already recorded" in asker() and [m["id"] for m in room.messages(repo, "asks")].count("remind-ask-q1-1") == 1, "one reminder, ever per tier"
again = target().splitlines()
assert any("Reminder: open question ask-q1 (11 min)" in l for l in again) and any("ask-q1 from Sol 6.1 (11 min)" in l for l in again), again
at(16)
assert "[oms room] overdue 1 question to you is open" in target(), "fifteen minutes is overdue"
assert room.status(repo, "asks")["open_questions"][0]["id"] == "ask-q1"
at(31)
asker()
asker()
assert [m["id"] for m in room.messages(repo, "asks") if m["id"].startswith("remind-")] == ["remind-ask-q1-1", "remind-ask-q1-2"], "a second reminder at thirty minutes, never more"
at(90)
asker()
assert len([m for m in room.messages(repo, "asks") if m["id"].startswith("remind-")]) == 2
import contextlib, io
captured = io.StringIO()
with contextlib.redirect_stderr(captured):
    room.send(repo, "asks", "target", "asker", "done", "answer")
assert "linked this answer to open question ask-q1" in captured.getvalue()
assert room.status(repo, "asks")["answered_count"] == 1 and not room.status(repo, "asks")["open_questions"], "one open question: auto-linked"
assert "open:" not in target() and "Your question" not in asker(), "a linked answer closes the question and stops the reminders"
for mid in ("ask-q2", "ask-q3"):
    room.send(repo, "asks", "asker", "target", "again " + mid, "question", message_id=mid)
try:
    room.send(repo, "asks", "target", "asker", "ambiguous", "answer")
except ValueError as error:
    assert "ask-q2" in str(error) and "ask-q3" in str(error), error
else:
    raise AssertionError("an answer between two open questions must name which one")
room.send(repo, "asks", "target", "asker", "just a note", "note")
room.send(repo, "asks", "target", "asker", "second", "answer", reply_to="ask-q2")
with contextlib.redirect_stderr(captured):
    room.send(repo, "asks", "target", "asker", "third", "answer")
assert not room.status(repo, "asks")["open_questions"]
# One hook reuses a single room projection, and a reminder burst budgets attempts, including failures.
from unittest.mock import patch
bulk = room.project(room.records(repo, "asks"))
question = next(m for m in bulk["messages"] if m["message_kind"] == "question")
bulk["messages"] = [dict(question, id="bulk-%d" % n, answered=False) for n in range(300)]
for failure in (None, ValueError("room message limit reached")):
    with patch.object(room, "send", side_effect=failure) as sends:
        room.remind(repo, "asks", "asker", bulk)
        assert 0 < sends.call_count <= 3, "reminder work was not bounded per hook"
bulk["messages"] = [dict(question, id="full-%d" % n, answered=False) for n in range(1024)]
with patch.object(room, "send") as sends:
    assert not room.remind(repo, "asks", "asker", bulk) and not sends.called, "a full room retried doomed sends"
with patch.dict(os.environ, {"OMS_ROOM_ID": "asks", "OMS_ROOM_PARTICIPANT": "asker"}), \
        patch.object(room, "records", wraps=room.records) as reads:
    asker()
    assert reads.call_count == 1, "hook re-read its room projection"
# The longest valid question ID can receive both reminder tiers; failed sends cannot claim success.
long_id = "q" * 160
room.send(repo, "asks", "asker", "target", "PRIVATE LONG QUESTION", "question", message_id=long_id)
at(11)
assert "it was re-sent as a reminder" in asker()
assert "reminder already recorded" in asker()
at(31)
assert "it was re-sent as a reminder" in asker()
long_reminders = [m for m in room.messages(repo, "asks") if m["message_kind"] == "note" and long_id in m["text"]]
assert len(long_reminders) == 2 and len({room.identifier(m["id"]) for m in long_reminders}) == 2
room.send(repo, "asks", "target", "asker", "done", "answer", reply_to=long_id)
room.send(repo, "asks", "asker", "target", "PRIVATE FAILED QUESTION", "question", message_id="failed-reminder")
with patch.object(room, "send", side_effect=ValueError("room full")):
    failed = asker()
assert "reminder pending" in failed and "re-sent" not in failed and "PRIVATE" not in failed, failed
room.now = real_now
room.send(repo, "team", "opus", "sol", "Final scoped note")
call("close", "--id", "team")
call("new", "--id", "unrelated-ordinary", "--live", "--topic", "Unrelated legacy broadcast")
assert not hook_state.live_thread_hint(payload("room-sol", "codex")), "closed room sessions never fall into unrelated legacy broadcasts"
# Fresh native launches enroll before their session ID exists. Bind the first
# real hook payload without moving the join boundary past waiting messages.
room.create(repo, "binding-room", "Fresh native binding")
room.join(repo, "binding-room", "fresh-main", "claude")
room.join(repo, "binding-room", "fresh-peer", "codex")
joined_seq = room.participant(room.status(repo, "binding-room"), "fresh-main")["seq"]
room.send(repo, "binding-room", "fresh-peer", "fresh-main", "Mail queued before the first native hook", message_id="before-bind")
os.environ.update(OMS_ROOM_ID="binding-room", OMS_ROOM_PARTICIPANT="fresh-main", OMS_ROOM_REPO=str(repo), OMS_HOOK_AGENT="claude")
delivered = hook_state.live_thread_hint(payload("native-first-session"))
assert "before-bind" in delivered, delivered
bound = room.participant(room.status(repo, "binding-room"), "fresh-main")
assert bound["seq"] == joined_seq and bound["consumer"] == hook_state.session_hash(payload("native-first-session"))
assert room.status(repo, "binding-room")["pending_count"] == 1
record_count = len(room.records(repo, "binding-room"))
room.bind(repo, "binding-room", "fresh-main", "native-first-session")
assert len(room.records(repo, "binding-room")) == record_count
room.send(repo, "binding-room", "fresh-peer", "fresh-main", "Only the enrolled native session may read this", message_id="bound-only")
assert not hook_state.live_thread_hint(payload("different-native")), "explicit environment bypassed the enrolled native identity"
notice = hook_state.live_thread_hint(dict(payload("different-native"), hook_event_name="UserPromptSubmit"))
assert "mail was withheld" in notice and "bound-only" not in notice
assert "bound-only" in hook_state.live_thread_hint(payload("native-first-session"))
for who, native in (("fresh-main", "different-native"), ("fresh-peer", "native-first-session")):
    try:
        room.bind(repo, "binding-room", who, native)
    except ValueError:
        pass
    else:
        raise AssertionError("native binding identity changed or was duplicated")
# /clear starts a new native session in the same main: only Claude Code's own
# SessionStart "clear" moves the binding, and it keeps the join boundary and mail.
def cleared(session, source="clear"):
    return dict(payload(session), hook_event_name="SessionStart", source=source)
before = room.participant(room.status(repo, "binding-room"), "fresh-main")
assert not hook_state.rebind_cleared_session(cleared("after-startup", "startup")), "a fresh start took over a bound main"
assert not hook_state.rebind_cleared_session(dict(payload("after-prompt"), source="clear")), "a non-SessionStart event rebound"
os.environ["OMS_HARNESS_CHILD"] = "1"
assert not hook_state.rebind_cleared_session(cleared("child-clear")), "a worker rebound its parent main"
os.environ.pop("OMS_HARNESS_CHILD")
try:
    room.bind(repo, "binding-room", "fresh-main", "stale-native", replaces="0" * 32)
except ValueError:
    pass
else:
    raise AssertionError("a rebind naming a stale identity moved the binding")
room.send(repo, "binding-room", "fresh-peer", "fresh-main", "Mail sent while the main cleared", message_id="during-clear")
assert hook_state.rebind_cleared_session(cleared("after-clear"))
after = room.participant(room.status(repo, "binding-room"), "fresh-main")
assert after["consumer"] == hook_state.session_hash(payload("after-clear")) and after["seq"] == before["seq"], after
assert after["initial_consumer"] == before["initial_consumer"], after
assert not hook_state.rebind_cleared_session(cleared("after-clear")), "a repeated clear event rebound twice"
assert "during-clear" in hook_state.live_thread_hint(payload("after-clear"))
assert not hook_state.live_thread_hint(payload("native-first-session")), "the cleared session kept reading the main's mail"
# A panel main is reminded on each prompt that it delegates; other events and sessions are not.
os.environ.update(OMS_PANEL_MAIN_ATTEMPT="fresh-main", OMS_PANEL_SESSION="oms-fixture")
# A launch attempt cannot prove a saved digest, and skipped captures remain retryable.
pressure_payload = payload("capture-main")
capture_path = hook_state.ctx_state_path(hook_state.ensure_oms(repo), pressure_payload)
with patch.object(hook_state, "percent_left_from_cache", return_value=25), \
        patch.object(hook_state, "start_handoff_capture", return_value=False) as launch:
    notice = hook_state.context_pressure_hint(pressure_payload)
    assert notice and "skipped" in notice and "saved" not in notice, notice
    assert json.loads(capture_path.read_text())["captured"] is False, "failed capture latched success"
    hook_state.context_pressure_hint(pressure_payload)
    assert launch.call_count == 2, "failed early capture suppressed retry"
with patch.object(hook_state, "percent_left_from_cache", return_value=25), \
        patch.object(hook_state, "start_handoff_capture", return_value=True) as launch:
    notice = hook_state.context_pressure_hint(pressure_payload)
    assert "started" in notice and "saved" not in notice, notice
    assert json.loads(capture_path.read_text())["captured"] is True
    assert not hook_state.context_pressure_hint(pressure_payload) and launch.call_count == 1
with patch.object(hook_state, "percent_left_from_cache", return_value=40):
    assert not hook_state.context_pressure_hint(pressure_payload)
with patch.object(hook_state, "percent_left_from_cache", return_value=10), \
        patch.object(hook_state, "start_handoff_capture", return_value=False) as launch:
    assert "context low" in hook_state.context_pressure_hint(pressure_payload)
    assert json.loads(capture_path.read_text())["captured"] is False, "warning band latched a skipped capture"
    hook_state.context_pressure_hint(pressure_payload)
    assert launch.call_count == 2, "an announced band suppressed the failed capture retry"
tower = hook_state.panel_main_hint(dict(payload("tower"), hook_event_name="UserPromptSubmit"))
assert "control-tower main" in tower and "--owner claude --room binding-room" in tower, tower
assert "#1 Opus 5.5" in tower and "trust boundaries" in tower, tower
assert not hook_state.panel_main_hint(payload("tower")), "only prompts carry the reminder"
# The shared plan rides on the same line: no plan, no plan sentence; then progress, the main's own task and the next ready one.
assert "[oms plan]" not in tower, tower
assert "set your status: oms room send" in tower and "--kind status" in tower, tower
plan_dir = repo / ".oms" / "plan"
plan_dir.mkdir(parents=True)
from dashboard_projection import MAX_SOURCE_BYTES, plan_tasks as dashboard_tasks
plan_path = plan_dir / "tasks.json"
def refused_plan():
    assert hook_state.plan_tasks(repo) == ({}, [])
    assert dashboard_tasks(str(repo)) == []
    assert not hook_state.panel_plan_hint(payload("tower"))

for malformed in ({"tasks": 1}, {"tasks": "task"}, [],
                  {"tasks": [{"id": ["bad"], "state": "done"}]},
                  {"tasks": [{"id": "bad", "state": "ready", "depends": 1}]},
                  {"tasks": [{"id": "bad", "state": "ready", "depends": [{}]}]}):
    plan_path.write_text(json.dumps(malformed))
    refused_plan()
plan_path.write_bytes(b'{"tasks": [{"id": "oversized", "state": "ready"}]}' + b" " * MAX_SOURCE_BYTES)
refused_plan()
plan_path.unlink()
plan_path.mkdir()
refused_plan()
plan_path.rmdir()
if os.name != "nt":
    outside = repo.parent / "external-plan.json"
    outside.write_text(json.dumps({"goal": "EXTERNAL PLAN", "tasks": [{"id": "external", "state": "ready"}]}))
    plan_path.symlink_to(outside)
    refused_plan()
    plan_path.unlink()
    plan_dir.rmdir()
    plan_dir.symlink_to(outside.parent)
    (outside.parent / "tasks.json").write_text(outside.read_text())
    refused_plan()
    plan_dir.unlink()
    plan_dir.mkdir()
    probe = '''
import os, sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, sys.argv[1])
import hook_state, dashboard_projection
repo = Path(sys.argv[2])
path = repo / '.oms/plan/tasks.json'
original = os.open
def raced(filename, flags, *args, **kwargs):
    if str(filename) == str(path):
        path.unlink()
        os.mkfifo(path)
    return original(filename, flags, *args, **kwargs)
for reader, empty in ((hook_state.plan_tasks, ({}, [])), (dashboard_projection.plan_tasks, [])):
    if path.exists():
        path.unlink()
    os.mkfifo(path)
    assert reader(repo) == empty
    path.unlink()
    path.write_text('{"tasks": []}')
    with patch.object(os, 'open', side_effect=raced):
        assert reader(repo) == empty
path.unlink()
'''
    subprocess.run([sys.executable, "-c", probe, str(root / "scripts/lib"), str(repo)], check=True, timeout=5)
stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
def plan_file(tasks):
    (plan_dir / "tasks.json").write_text(json.dumps({"goal": "Ship the shared plan board " * 5, "tasks": {
        t["id"]: dict(t, title=t.get("title", t["id"]), created=stamp, updated=stamp) for t in tasks}}))
plan_file([{"id": "t1", "state": "done"}, {"id": "t2", "state": "ready", "depends": ["t1"], "title": "Wire banner"},
           {"id": "t3", "state": "ready", "depends": ["t9"]}])
free = hook_state.panel_main_hint(dict(payload("tower"), hook_event_name="UserPromptSubmit"))
assert "[oms plan] Goal: Ship the shared plan board" in free and "1 of 3 verified" in free, free
assert "yours:" not in free and "next ready: t2 Wire banner → oms agent-plan claim --id t2 --provider claude" in free, free
assert "claim the next ready task before starting new work" in free and "t3" not in free.split("next ready:")[1], free
assert len(free.split("[oms plan]")[1].split("set your status")[0]) < 420 and "\n" not in free, free
plan_file([{"id": "t1", "state": "done"}, {"id": "t2", "state": "running", "claimed_by_participant": os.environ["OMS_ROOM_PARTICIPANT"]},
           {"id": "t3", "state": "ready"}])
held = hook_state.panel_main_hint(dict(payload("tower"), hook_event_name="UserPromptSubmit"))
assert "yours: t2 (running)" in held and "next ready: t3" in held and "before starting new work" not in held, held
room.send(repo, "binding-room", os.environ["OMS_ROOM_PARTICIPANT"], "all", "Wiring the banner", kind="status")
assert "set your status" not in hook_state.panel_main_hint(dict(payload("tower"), hook_event_name="UserPromptSubmit"))
# Context pressure: a panel main is told to compact in place at each band; any other session keeps the fresh-session advice.
hud = repo.parent / "ctx-hud"
hud.mkdir(exist_ok=True)
os.environ["OMS_HUD_CACHE_DIR"] = str(hud)
def pressure(session, left):
    (hud / ("ctx-%s.json" % hook_state.sha256_text(session)[:24])).write_text(json.dumps({"used_percentage": 100 - left}))
    return hook_state.context_pressure_hint(payload(session))
for session, panel in (("ctx-panel", True), ("ctx-plain", False)):
    if not panel:
        os.environ.pop("OMS_PANEL_MAIN_ATTEMPT")
    said = [pressure(session, left) for left in (29, 25, 14, 14, 7)]
    assert said[1] is None and said[3] is None, said
    if panel:
        assert said[0].startswith("[oms] ~29% context left — handoff digest capture started; check capture completion"), said
        assert "compact in place (Claude: /compact; Codex: /compact) — the panel keeps your room and task." in said[2], said
        assert "context low (~14% left)" in said[2] and "context low, compact now (~7% left)" in said[4], said
    else:
        assert said[0] is None and "migrate" not in said[2] and "continue in a fresh session" in said[2], said
        assert "migrate to a fresh session" in said[4] and "compact in place" not in said[2] + said[4], said
os.environ["OMS_PANEL_MAIN_ATTEMPT"] = "fresh-main"
# After compact or clear the panel main gets one line of ids and counts; mail text never appears.
room.send(repo, "binding-room", "fresh-peer", os.environ["OMS_ROOM_PARTICIPANT"], "SECRET QUESTION BODY", kind="question")
resumed = hook_state.panel_resume_line(dict(payload("tower"), hook_event_name="SessionStart", source="compact"))
assert resumed.startswith("[oms panel] resumed after compact: you are ") and "in room binding-room" in resumed, resumed
assert "your plan task: t2 (running); 1 open question to you; 0 workers running for you; status: Wiring the banner." in resumed, resumed
assert "SECRET" not in resumed and "\n" not in resumed, resumed
plan_file([{"id": "t1", "state": "done"}])
assert "your plan task: none;" in hook_state.panel_resume_line(dict(payload("tower"), source="clear"))
def start(source, **env):
    return subprocess.run(["python3", str(root / "scripts/lib/hook_state.py"), "relay-hint"], capture_output=True, text=True,
                          input=json.dumps(dict(payload("resume-" + source), hook_event_name="SessionStart", source=source)),
                          env=dict(os.environ, **env)).stdout
# (A "clear" start would rebind the fixture main; its wording is covered by the direct call above.)
assert "[oms panel] resumed after compact" in start("compact")
assert "resumed after" not in start("startup") and "resumed after" not in start("compact", OMS_PANEL_MAIN_ATTEMPT="")
# Codex may not show SessionStart output to the model: its first prompt repeats the line once.
start("compact", OMS_HOOK_AGENT="codex")
first = subprocess.run(["python3", str(root / "scripts/lib/hook_state.py"), "route", "--manifest", str(repo / "manifest.json")],
                       capture_output=True, text=True, env=dict(os.environ, OMS_HOOK_AGENT="codex"),
                       input=json.dumps(dict(payload("resume-compact"), hook_event_name="UserPromptSubmit", prompt="/x"))).stdout
assert first.count("resumed after compact") == 1, first
again = subprocess.run(["python3", str(root / "scripts/lib/hook_state.py"), "route", "--manifest", str(repo / "manifest.json")],
                       capture_output=True, text=True, env=dict(os.environ, OMS_HOOK_AGENT="codex"),
                       input=json.dumps(dict(payload("resume-compact"), hook_event_name="UserPromptSubmit", prompt="/x"))).stdout
assert "resumed after" not in again, again
os.environ.pop("OMS_PANEL_MAIN_ATTEMPT")
assert not hook_state.panel_main_hint(dict(payload("tower"), hook_event_name="UserPromptSubmit"))
os.environ.pop("OMS_PANEL_SESSION")
# Codex mains switch threads on clear/resume/fork only; provider must be proven.
room.join(repo, "binding-room", "codex-main", "codex")
room.bind(repo, "binding-room", "codex-main", "codex-thread-0")
os.environ["OMS_ROOM_PARTICIPANT"] = "codex-main"
os.environ.pop("OMS_HOOK_AGENT")
def codex_start(session, source, transcript="/h/.codex/sessions/rollout-1.jsonl"):
    return dict(cleared(session, source), transcript_path=transcript)
def codex_bound():
    return room.participant(room.status(repo, "binding-room"), "codex-main")
codex_before = codex_bound()
for case, source in (("startup", "startup"), ("compact", "compact")):
    assert not hook_state.rebind_cleared_session(codex_start("codex-" + case, source)), source + " rebound a Codex main"
assert not hook_state.rebind_cleared_session(cleared("codex-as-claude")), "a Claude payload rebound a Codex main"
assert not hook_state.rebind_cleared_session(codex_start("codex-unproven", "clear", None)), "an unproven provider rebound"
assert codex_bound()["consumer"] == codex_before["consumer"]
room.send(repo, "binding-room", "fresh-peer", "codex-main", "Mail sent while Codex switched", message_id="during-codex-switch")
last = "codex-thread-0"
for source in ("clear", "resume", "fork"):
    prior = codex_bound()["consumer"]
    nxt = "codex-" + source
    assert hook_state.rebind_cleared_session(codex_start(nxt, source)), source
    now = codex_bound()
    assert now["consumer"] == hook_state.session_hash(payload(nxt)) != prior
    assert now["seq"] == codex_before["seq"] and now["initial_consumer"] == codex_before["initial_consumer"], now
    last = nxt
assert "during-codex-switch" in hook_state.live_thread_hint(payload(last, "codex"))
try:
    hook_state.rebind_cleared_session(codex_start("after-clear", "resume"))
except ValueError:
    pass  # cmd_relay_hint suppresses this at SessionStart
else:
    raise AssertionError("resume took a thread another participant holds")
assert codex_bound()["consumer"] == hook_state.session_hash(payload(last))
assert room.participant(room.status(repo, "binding-room"), "fresh-main")["consumer"] == hook_state.session_hash(payload("after-clear"))
os.environ["OMS_ROOM_PARTICIPANT"] = "fresh-main"
assert not hook_state.rebind_cleared_session(codex_start("claude-as-codex", "clear")), "a Codex payload rebound a Claude main"
os.environ["OMS_HOOK_AGENT"] = "codex"
os.environ["OMS_ROOM_PARTICIPANT"] = "codex-main"
assert hook_state.rebind_cleared_session(codex_start("codex-env-proven", "clear", None)), "OMS_HOOK_AGENT should prove Codex"
os.environ["OMS_HOOK_AGENT"] = "claude"
os.environ["OMS_ROOM_PARTICIPANT"] = "fresh-main"
os.environ["OMS_HARNESS_CHILD"] = "1"
try:
    room.bind(repo, "binding-room", "fresh-peer", "worker-native")
except ValueError:
    pass
else:
    raise AssertionError("worker gained native binding authority")
os.environ.pop("OMS_HARNESS_CHILD")
room.join(repo, "binding-room", "provider-mismatch", "codex")
os.environ["OMS_ROOM_PARTICIPANT"] = "provider-mismatch"
room.send(repo, "binding-room", "fresh-main", "provider-mismatch", "Only the enrolled provider may read this", message_id="provider-only")
assert not hook_state.live_thread_hint(payload("not-a-codex-session")), "wrong provider consumed an explicitly addressed message"
assert not room.participant(room.status(repo, "binding-room"), "provider-mismatch").get("consumer")
for key in ("OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_ROOM_REPO", "OMS_HOOK_AGENT"):
    os.environ.pop(key)
room.append(repo, "binding-room", {"kind": "leave", "participant": "fresh-main"}, "Explicit subscription change")
room.join(repo, "binding-room", "fresh-main", "claude", native_session="replacement-session")
assert room.selected(repo, hook_state.session_hash(payload("native-first-session"))) == (None, None)
assert not hook_state.live_thread_hint(payload("native-first-session")), "former receiver fell into unrelated CURRENT"
# Ordinary discussions cannot hide a second enrollment or evict work rooms
# from the bounded inventory. An incomplete scan never proves uniqueness.
from unittest.mock import patch
scan_repo = repo.parent / "scan-rooms"
scan_repo.mkdir()
for ident in ("000-first", "zzz-second"):
    room.create(scan_repo, ident, "Discovery fixture")
    room.join(scan_repo, ident, "main", "claude", native_session="scan-native")
consumer = hook_state.session_hash(payload("scan-native"))
for index in range(64):
    ident = "middle-%03d" % index
    (scan_repo / ".oms/threads" / (ident + ".jsonl")).write_text(json.dumps({"thread": ident, "seq": 0, "live": True}) + "\n")
states, incomplete = room.discover(scan_repo)
assert not incomplete and {state["id"] for state in states} == {"000-first", "zzz-second"}
assert room.selected(scan_repo, consumer) == (None, None)
for name, limit in (("MAX_ROOMS", 1), ("MAX_SCAN_ENTRIES", 1), ("MAX_SCAN_BYTES", 1)):
    with patch.object(room, name, limit):
        assert room.discover(scan_repo)[1]
        assert room.selected(scan_repo, consumer) == (None, None), "partial scan selected a supposedly unique receiver"
        assert room.selected(scan_repo, consumer, "zzz-second", "main") == ("zzz-second", "main")
        assert not hook_state.live_thread_hint(payload("scan-native"), scan_repo), "partial discovery fell back to CURRENT"
inventory = subprocess.run(["bash", str(root / "scripts/oms"), "room", "list", "--repo", str(scan_repo), "--json"],
                           capture_output=True, text=True, check=True)
report = json.loads(inventory.stdout)
assert len(report["rooms"]) == 2 and report["incomplete"] is False
# Claude Code's background service continues a main under a new session id
# without the panel environment; a shared root record uuid re-attaches it.
import time, uuid
projects = pathlib.Path.home() / ".claude/projects/fixture-slug"
projects.mkdir(parents=True)
def transcript(root_uuid, sid=None, kind="bg"):
    sid = sid or str(uuid.uuid4())
    rows = [{"type": "summary", "summary": "x"}, dict({"type": "user", "uuid": root_uuid, "sessionId": sid},
                                                      **({"sessionKind": kind} if kind else {}))]
    (projects / (sid + ".jsonl")).write_text("".join(json.dumps(row) + "\n" for row in rows))
    return sid
def moved(sid, event="SessionStart", source="fork"):
    return {"cwd": str(repo), "session_id": sid, "transcript_path": str(projects / (sid + ".jsonl")),
            "hook_event_name": event, "source": source}
def run_relay(sid, **env):
    subprocess.run(["python3", str(root / "scripts/lib/hook_state.py"), "relay-hint"], input=json.dumps(moved(sid)),
                   env=dict(os.environ, **env), capture_output=True, text=True, check=True)
lineage = repo.parent / "lineage-repo"
lineage.mkdir()
subprocess.run(["git", "init", "-q", str(lineage)], check=True)
repo = lineage
old_sid = transcript("root-a")
room.create(repo, "moved-room", "Moved main")
room.join(repo, "moved-room", "moved-main", "claude", native_session=old_sid)
room.join(repo, "moved-room", "moved-peer", "codex")
before = room.participant(room.status(repo, "moved-room"), "moved-main")
room.send(repo, "moved-room", "moved-peer", "moved-main", "Mail sent before the move", message_id="before-move")
for sid, env in ((transcript("root-b"), {}), (transcript("root-a"), {"OMS_HARNESS_CHILD": "1"}),
                 (transcript("root-a"), {"OMS_HOOK_AGENT": "codex"})):
    run_relay(sid, **env)
    assert room.participant(room.status(repo, "moved-room"), "moved-main")["consumer"] == before["consumer"], env
# A conversation forked by hand into another terminal (no background sessionKind) keeps its hands off.
run_relay(transcript("root-a", kind=None))
assert room.participant(room.status(repo, "moved-room"), "moved-main")["consumer"] == before["consumer"]
new_sid = transcript("root-a")
run_relay(new_sid)
after = room.participant(room.status(repo, "moved-room"), "moved-main")
assert after["consumer"] == hook_state.session_hash(moved(new_sid)), after
assert (after["seq"], after["initial_consumer"]) == (before["seq"], before["initial_consumer"]), after
assert "before-move" in hook_state.live_thread_hint(moved(new_sid, "UserPromptSubmit"))
# Two mains sharing the root are no unique lineage.
twin = transcript("root-c")
room.create(repo, "twin-room", "Twin mains")
room.join(repo, "twin-room", "twin-one", "claude", native_session=twin)
room.join(repo, "twin-room", "twin-two", "claude", native_session=transcript("root-c"))
assert not hook_state.rebind_lineage_session(moved(transcript("root-c")))
assert room.participant(room.status(repo, "twin-room"), "twin-one")["consumer"] == hook_state.session_hash(moved(twin))
# Missing, unreadable, truncated and ambiguous comparisons cannot establish uniqueness.
import panel_chats
states, incomplete = room.discover(repo)
assert not incomplete
enrolled = {m["consumer"] for state in states for m in state["participants"]
            if m["joined"] and m["role"] == "main" and m["provider"] == "claude"}
complete_index = panel_chats.native_index(enrolled)
missing = dict(complete_index["claude"])
other = room.participant(room.status(repo, "twin-room"), "twin-two")["consumer"]
missing.pop(other)
with patch.object(panel_chats, "native_index", return_value={"claude": missing}):
    assert not hook_state.rebind_lineage_session(moved(transcript("root-c"))), "partial native index rebound a twin main"
old_path = projects / (next(iter(complete_index["claude"][other])) + ".jsonl")
old_text = old_path.read_text()
for damaged in ("", " " * hook_state.LINEAGE_READ + old_text):
    old_path.write_text(damaged)
    assert not hook_state.rebind_lineage_session(moved(transcript("root-c"))), "unresolved root was treated as a nonmatch"
old_path.write_text(old_text)
real_root = hook_state.transcript_root
def unreadable_root(path, background=False):
    if path == old_path:
        raise OSError("unreadable")
    return real_root(path, background)
with patch.object(hook_state, "transcript_root", side_effect=unreadable_root):
    assert not hook_state.rebind_lineage_session(moved(transcript("root-c")))
with patch.object(panel_chats, "MAX_FILES", 0):
    assert not hook_state.rebind_lineage_session(moved(transcript("root-c"))), "truncated project scan rebound a main"
duplicate_slug = projects.parent / "duplicate-slug"
duplicate_slug.mkdir()
(duplicate_slug / old_path.name).write_text(old_text)
assert not hook_state.rebind_lineage_session(moved(transcript("root-c"))), "ambiguous transcript copies rebound a main"
(duplicate_slug / old_path.name).unlink()
duplicate_slug.rmdir()
assert room.participant(room.status(repo, "twin-room"), "twin-one")["consumer"] == hook_state.session_hash(moved(twin))
# A negative result is cached for ten minutes; a prompt then heals the moved session.
room.create(repo, "late-room", "Late main")
late_sid = transcript("root-d")
healed = transcript("root-d")
cache = repo / ".oms/hooks/sessions" / (hook_state.session_hash(moved(healed)) + ".lineage.json")
assert not hook_state.rebind_lineage_session(moved(healed)) and cache.is_file()
room.join(repo, "late-room", "late-main", "claude", native_session=late_sid)
room.join(repo, "late-room", "late-peer", "codex")
room.send(repo, "late-room", "late-peer", "late-main", "Mail for the late main", message_id="late-mail")
assert not hook_state.live_thread_hint(moved(healed, "UserPromptSubmit")), "the negative cache was rescanned"
cache.write_text(json.dumps({"schema": 1, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 660))}))
assert "late-mail" in hook_state.live_thread_hint(moved(healed, "UserPromptSubmit"))
assert room.participant(room.status(repo, "late-room"), "late-main")["consumer"] == hook_state.session_hash(moved(healed))
PY
}

test_classifier_boundaries
test_peer_tail_is_bounded
test_explicit_goal_rotation
test_fail_ledger_hook_resolves_on_success
test_route_is_hermetic_to_inherited_harness_session
test_live_thread_delivery_at_existing_safe_points
echo "autonomy-hook-smoke: ok"
