#!/usr/bin/env bash
set -euo pipefail

# Stop-hook budgets are opt-in; retired answer-format settings cannot block.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-turn-guard-fuse.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

# The hooks read their whole configuration from the environment, so an
# inherited session would route these fixtures at the wrong repo or switch the
# guard off before the first assertion runs.
unset OMS_HOOK_PAYLOAD OMS_STATE_REPO OMS_TURN_GUARD_OFF OMS_TURN_GUARD_STRICT \
  OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN OMS_AGENT OMS_SESSION_BUDGET_TURNS \
  OMS_SESSION_BUDGET_HOURS OMS_SKILL_HINTS OMS_AUTO_TASK
export OMS_HARNESS_CHILD=0 OMS_CI_TICK=0 OMS_STATE_HINTS=0
export OMS_SESSION_BUDGET_TURNS=2
export HOME="$TMP/home"
export TMPDIR="$TMP/runtime"
mkdir -p "$HOME" "$TMPDIR"

fail() {
  echo "turn-guard-fuse-smoke: $*" >&2
  exit 1
}

make_dirty_repo() {
  local project="$1"
  mkdir -p "$project"
  git -C "$project" init -q -b main
  printf 'base\n' > "$project/file.txt"
  git -C "$project" add file.txt
  git -C "$project" -c user.email=test@example.com -c user.name='Test User' \
    commit -q -m init
  printf 'change\n' >> "$project/file.txt"
}

# Exercise the installed prompt-to-Stop path in an isolated repository.
route_prompt() {
  local project="$1"
  local session="$2"
  local turn_field="$3"
  printf '{"prompt":"fix this and push까지 진행","session_id":"%s"%s,"cwd":"%s"}' \
    "$session" "$turn_field" "$project" |
    (cd "$project" && bash "$ROOT/scripts/skill-router.sh") > /dev/null
}

# Exactly the wiring install-claude-hooks.sh registers for Stop: the payload on
# stdin, the verdict on stdout.
run_stop() {
  local script="$1"
  local project="$2"
  local session="$3"
  local extra="${4:-}"
  printf '{"hook_event_name":"Stop","session_id":"%s","cwd":"%s","last_assistant_message":"Done."%s}' \
    "$session" "$project" "$extra" |
    bash "$script"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local what="$3"
  printf '%s' "$haystack" | grep -Fq -- "$needle" ||
    fail "$what: expected $needle in: $haystack"
}

# A faithful mini-install: only the helper is replaced, so the hook still runs
# its real tail after the guard fails.
broken_helper_root() {
  local fake="$1"
  mkdir -p "$fake/scripts/lib"
  cp "$ROOT/scripts/turn-guard.sh" "$fake/scripts/turn-guard.sh"
  cp "$ROOT"/scripts/lib/*.sh "$ROOT"/scripts/lib/*.py "$fake/scripts/lib/"
}

test_unreadable_helper_reports_an_unguarded_turn() {
  local project="$TMP/crash/project"
  local fake="$TMP/crash/root"
  local out
  local rc

  make_dirty_repo "$project"
  broken_helper_root "$fake"
  printf 'def broken(\n' > "$fake/scripts/lib/hook_state.py"

  rc=0
  out="$(run_stop "$fake/scripts/turn-guard.sh" "$project" s-crash)" || rc=$?
  [ "$rc" = 0 ] || fail "the Stop hook must stay fail-open, got exit $rc"
  assert_contains "$out" 'oh-my-setting turn guard: unavailable' "a dead helper"
  assert_contains "$out" 'this turn was not checked' "a dead helper"
}

test_unparseable_verdict_reports_an_unguarded_turn() {
  local project="$TMP/garbage/project"
  local fake="$TMP/garbage/root"
  local out
  local rc

  make_dirty_repo "$project"
  broken_helper_root "$fake"
  printf 'print("this is not a verdict")\n' > "$fake/scripts/lib/hook_state.py"

  rc=0
  out="$(run_stop "$fake/scripts/turn-guard.sh" "$project" s-garbage)" || rc=$?
  [ "$rc" = 0 ] || fail "the Stop hook must stay fail-open, got exit $rc"
  assert_contains "$out" 'oh-my-setting turn guard: unavailable' "garbage output"
  if printf '%s' "$out" | grep -Fq 'this is not a verdict'; then
    fail "unparseable helper output must not reach the Stop protocol: $out"
  fi
}

# The helper swallows its own exceptions to stay fail-open, which is the one
# crash class that exits 0 with an empty stdout — indistinguishable from an
# approval until it names itself.
test_crashing_guard_command_reports_an_unguarded_turn() {
  local project="$TMP/raise/project"
  local fake="$TMP/raise/root"
  local out
  local rc

  make_dirty_repo "$project"
  broken_helper_root "$fake"
  python3 - "$ROOT/scripts/lib/hook_state.py" "$fake/scripts/lib/hook_state.py" <<'PY'
import sys

marker = "def cmd_guard(_: argparse.Namespace) -> int:\n"
source = open(sys.argv[1], encoding="utf-8").read()
if marker not in source:
    raise SystemExit("cmd_guard signature moved; fault injection needs an update")
injected = source.replace(marker, marker + '    raise RuntimeError("boom")\n', 1)
open(sys.argv[2], "w", encoding="utf-8").write(injected)
PY

  rc=0
  out="$(run_stop "$fake/scripts/turn-guard.sh" "$project" s-raise)" || rc=$?
  [ "$rc" = 0 ] || fail "the Stop hook must stay fail-open, got exit $rc"
  assert_contains "$out" 'oh-my-setting turn guard: unavailable' "a raising guard"
  printf '%s' "$out" | python3 -c 'import json, sys; json.loads(sys.stdin.read())' ||
    fail "the notice must stay one JSON document: $out"
}

test_default_hooks_skip_guard_state_and_keep_journal() (
  local project="$TMP/default/project"
  local fake="$TMP/default/root"
  local out
  unset OMS_SESSION_BUDGET_TURNS
  export OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN=99 OMS_TURN_GUARD_STRICT=1
  export OMS_WORK_JOURNAL=0
  make_dirty_repo "$project"

  out="$(printf '{"prompt":"Use the project graph and push","session_id":"s-default","cwd":"%s"}' \
    "$project" | bash "$ROOT/scripts/skill-router.sh")"
  [ -z "$out" ] || fail "default routing must not inject keyword hints: $out"
  [ ! -d "$project/.oms" ] || fail "disabled guards must not create route state"
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-default)"
  [ -z "$out" ] || fail "disabled guards must stay silent: $out"
  [ ! -d "$project/.oms" ] || fail "disabled guards must not create Stop state"

  # The shell must not even launch the guard helper in the default path, yet
  # keep both journal callbacks and resolve the payload's repository.
  broken_helper_root "$fake"
  printf 'import sys\nassert sys.argv[1] == "repo", sys.argv\nprint(%s)\n' \
    "\"$project\"" > "$fake/scripts/lib/hook_state.py"
  cat > "$fake/scripts/lib/work-journal.sh" <<'SH'
work_journal_enabled() { return 0; }
work_journal_finish() { printf 'finish:%s\n' "$1"; }
work_journal_defer_finish() { printf 'defer:%s\n' "$1"; }
SH
  out="$(run_stop "$fake/scripts/turn-guard.sh" "$project" s-default)"
  [ "$out" = "$(printf 'finish:%s\ndefer:%s' "$project" "$project")" ] ||
    fail "default Stop must bypass guard execution but retain journal finish: $out"

  # Disabling guard routes must not hide a session after its SessionStart row
  # expires. Presence stays content-free, adopted-only, and minute-bounded.
  python3 - "$ROOT/scripts/lib" "$project" <<'PY' || fail "default live presence contract"
import importlib, json, pathlib, sys
from datetime import datetime, timedelta, timezone
sys.path.insert(0, sys.argv[1])
hooks = importlib.import_module("hook_state")
repo = pathlib.Path(sys.argv[2])
hooks.hook_repo = lambda payload: repo
payload = {"session_id": "s-default", "hook_event_name": "UserPromptSubmit"}
assert hooks.peer_advisory_hint(payload) is None
assert not (repo / ".oms").exists(), "presence must not adopt a repository"
hooks.ensure_oms(repo)
events = repo / ".oms/hooks/events.jsonl"
old = datetime.now(timezone.utc) - timedelta(seconds=hooks.PEER_WINDOW_SEC + 1)
events.write_text(json.dumps({"session": hooks.session_hash(payload),
    "hook": "SessionStart", "ts": old.isoformat()}) + "\n")
assert hooks.peer_advisory_hint(payload) is None
assert hooks.peer_advisory_hint(payload) is None
rows = [json.loads(line) for line in events.read_text().splitlines()]
presence = [row for row in rows if row.get("action") == "presence"]
assert len(presence) == 1, rows
assert not any("prompt" in key for key in presence[0]), presence
other = {"session_id": "s-neighbor", "hook_event_name": "UserPromptSubmit"}
assert "another session is live" in hooks.peer_advisory_hint(other)
assert hooks.peer_advisory_hint(other) is None, "peer latch must still dedupe"
assert not hooks.session_state_path(repo / ".oms/hooks", payload).exists()
PY
)

test_session_budget_survives_prompt_rewrites() {
  python3 - "$ROOT/scripts/lib" "$TMP/budget-state" <<'PY' || fail "session budget rewrite contract"
import importlib, os, pathlib, sys
sys.path.insert(0, sys.argv[1])
hooks = importlib.import_module("hook_state")
repo = pathlib.Path(sys.argv[2])
repo.mkdir()
hooks.hook_repo = lambda payload: repo
hooks.start_handoff_capture = lambda *args, **kwargs: False
os.environ["OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN"] = "0"
os.environ["OMS_SESSION_BUDGET_TURNS"] = "2"
payload = {"session_id": "budget-state", "turn_id": "t1"}
hooks.route_state(payload)
path = hooks.session_state_path(repo / ".oms/hooks", payload)
state = hooks.load_state(path)
assert hooks.session_budget_reason(repo, payload, state, path) is None
started = state["started_at"]
payload["turn_id"] = "t2"
hooks.route_state(payload)
state = hooks.load_state(path)
assert state["budget_turns"] == 1 and state["started_at"] == started, state
assert "2 turns" in hooks.session_budget_reason(repo, payload, state, path)
payload["turn_id"] = "t3"
hooks.route_state(payload)
state = hooks.load_state(path)
assert state["budget_turns"] == 2 and state["budget_band"] == 4, state
hooks.load_payload = lambda: ({**payload, "stop_hook_active": True}, "")
assert hooks.cmd_guard(None) == 0
before = path.read_bytes()
os.environ["OMS_SESSION_BUDGET_TURNS"] = "0"
assert hooks.session_budget_reason(repo, payload, state, path) is None
assert hooks.cmd_guard(None) == 0
hooks.route_state(payload)
assert path.read_bytes() == before, "disabled budget must not rewrite state"
PY
}

test_unreadable_helper_reports_an_unguarded_turn
test_unparseable_verdict_reports_an_unguarded_turn
test_crashing_guard_command_reports_an_unguarded_turn
test_default_hooks_skip_guard_state_and_keep_journal
test_session_budget_survives_prompt_rewrites

echo "turn-guard-fuse-smoke: ok"
