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
  OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE

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
}

test_live_thread_delivery_at_existing_safe_points() {
  python3 - "$ROOT" "$TMP/live-hook" <<'PY'
import json, os, pathlib, subprocess, sys
root, repo = map(pathlib.Path, sys.argv[1:])
repo.mkdir()
subprocess.run(["git", "init", "-q", str(repo)], check=True)
sys.path.insert(0, str(root / "scripts/lib"))
import hook_state
thread = ["bash", str(root / "scripts/thread.sh"), "--repo", str(repo)]
def call(*args):
    return subprocess.run(thread + list(args), capture_output=True, text=True, check=True).stdout
def payload(session):
    return {"cwd": str(repo), "session_id": session, "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch", "tool_input": {"command": "*** Begin Patch\n*** Update File: a.py\n*** End Patch"}}
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
PY
}

test_classifier_boundaries
test_peer_tail_is_bounded
test_explicit_goal_rotation
test_fail_ledger_hook_resolves_on_success
test_route_is_hermetic_to_inherited_harness_session
test_live_thread_delivery_at_existing_safe_points
echo "autonomy-hook-smoke: ok"
