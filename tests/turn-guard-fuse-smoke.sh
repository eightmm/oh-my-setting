#!/usr/bin/env bash
set -euo pipefail

# Smoke tests for the Stop-hook turn guard's fuse. Two properties: the block
# budget is spent per turn even though Claude Code's Stop payload carries no
# turn identifier, and a guard that produced no verdict says so instead of
# passing for an approval.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-turn-guard-fuse.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

# The hooks read their whole configuration from the environment, so an
# inherited session would route these fixtures at the wrong repo or switch the
# guard off before the first assertion runs.
unset OMS_HOOK_PAYLOAD OMS_STATE_REPO OMS_TURN_GUARD_OFF OMS_TURN_GUARD_STRICT \
  OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN OMS_AGENT
export OMS_HARNESS_CHILD=0 OMS_CI_TICK=0 OMS_STATE_HINTS=0
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

# A high-risk prompt is what arms the guard; routing runs from inside the
# fixture so the router's journal tick never reaches the real repo.
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

guard_blocks() {
  local project="$1"
  local session="$2"
  python3 - "$project" "$session" <<'PY'
import hashlib, json, pathlib, sys

digest = hashlib.sha256(sys.argv[2].encode("utf-8")).hexdigest()[:32]
path = pathlib.Path(sys.argv[1]) / ".oms/hooks/sessions" / (digest + ".json")
state = json.loads(path.read_text(encoding="utf-8"))
print(json.dumps(state.get("guard_blocks"), sort_keys=True))
print(state.get("stop_seq"))
PY
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local what="$3"
  printf '%s' "$haystack" | grep -Fq -- "$needle" ||
    fail "$what: expected $needle in: $haystack"
}

# --- the fuse: a Stop payload without a turn id -----------------------------

test_missing_turn_id_spends_one_block_per_turn() {
  local project="$TMP/counter/project"
  local out
  local state

  make_dirty_repo "$project"
  route_prompt "$project" s-counter ""

  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-counter)"
  assert_contains "$out" '"decision": "block"' "first unverified turn"

  # The bug: this second turn used the same empty key and read as spent.
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-counter)"
  assert_contains "$out" '"decision": "block"' "second unverified turn"

  state="$(guard_blocks "$project" s-counter)"
  [ "$(printf '%s' "$state" | head -1)" = '{"stop-1": 1, "stop-2": 1}' ] ||
    fail "each turn needs its own block key: $state"
  assert_contains "$(cat "$project/.oms/hooks/events.jsonl")" \
    '"turn_key_source": "counter"' "telemetry names the key source"

  # A Stop that continues an already blocked turn keeps that turn's key, so the
  # cap stays a loop fuse rather than a per-event allowance.
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-counter \
    ',"stop_hook_active":true')"
  [ -z "$out" ] || fail "a continuation Stop must not spend a second block: $out"
  assert_contains "$(cat "$project/.oms/hooks/events.jsonl")" \
    '"status": "allow_block_limit"' "the capped continuation is recorded"

  # A new prompt resets the budget and keeps the counter monotonic.
  route_prompt "$project" s-counter ""
  state="$(guard_blocks "$project" s-counter)"
  [ "$(printf '%s' "$state" | head -1)" = '{}' ] ||
    fail "a routed prompt must clear the previous turn's budget: $state"
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-counter)"
  assert_contains "$out" '"decision": "block"' "turn after a fresh prompt"
  state="$(guard_blocks "$project" s-counter)"
  [ "$(printf '%s' "$state" | head -1)" = '{"stop-3": 1}' ] ||
    fail "the counter must survive the route rewrite: $state"
}

test_payload_turn_id_keeps_previous_behaviour() {
  local project="$TMP/payload/project"
  local out
  local state

  make_dirty_repo "$project"
  route_prompt "$project" s-payload ',"turn_id":"t1"'

  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-payload ',"turn_id":"t1"')"
  assert_contains "$out" '"decision": "block"' "first unverified turn"
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-payload ',"turn_id":"t1"')"
  [ -z "$out" ] || fail "a payload turn id still caps the turn at one block: $out"

  state="$(guard_blocks "$project" s-payload)"
  [ "$(printf '%s' "$state" | head -1)" = '{"t1": 1}' ] ||
    fail "a payload turn id must stay the key: $state"
  assert_contains "$(cat "$project/.oms/hooks/events.jsonl")" \
    '"turn_key_source": "payload"' "telemetry names the key source"
}

# --- the absent verdict -----------------------------------------------------

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

test_allowed_turn_stays_silent() {
  local project="$TMP/quiet/project"
  local out

  make_dirty_repo "$project"
  route_prompt "$project" s-quiet ""
  out="$(printf '{"hook_event_name":"Stop","session_id":"s-quiet","cwd":"%s","last_assistant_message":"Done. Verification: bash tests passed."}' \
    "$project" | bash "$ROOT/scripts/turn-guard.sh")"
  [ -z "$out" ] || fail "a verified turn must produce no notice: $out"
}

test_guard_rows_carry_observation_identity() {
  local project="$TMP/obs/project"
  local out

  make_dirty_repo "$project"
  route_prompt "$project" s-obs ""

  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-obs)"
  assert_contains "$out" '"decision": "block"' "unverified turn blocks"
  # The corrected delivery continues the same turn with a verification line.
  out="$(printf '{"hook_event_name":"Stop","session_id":"s-obs","cwd":"%s","stop_hook_active":true,"last_assistant_message":"Verification: bash tests passed."}' \
    "$project" | bash "$ROOT/scripts/turn-guard.sh")"
  [ -z "$out" ] || fail "verified continuation must stay silent: $out"

  python3 - "$project/.oms/hooks/events.jsonl" <<'PY' || fail "observation identity missing or unpaired"
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
guard = [r for r in rows if r.get("action") == "turn_guard"]
block = [r for r in guard if r.get("status") == "block_unverified"]
corrected = [r for r in guard if r.get("status") == "allow_verified"]
assert block and corrected, guard
assert all(r.get("turn_obs") and r.get("eligible") is True for r in block + corrected), guard
# The corrected allow rides the same observation key as its block, so a rate
# reader can pair them without content.
assert block[-1]["turn_obs"] == corrected[-1]["turn_obs"], (block, corrected)
PY

  # A fresh routed prompt opens a new observation key.
  route_prompt "$project" s-obs ""
  out="$(run_stop "$ROOT/scripts/turn-guard.sh" "$project" s-obs)"
  assert_contains "$out" '"decision": "block"' "next routed turn blocks"
  python3 - "$project/.oms/hooks/events.jsonl" <<'PY' || fail "route boundary did not advance the observation key"
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
blocks = [r for r in rows
          if r.get("action") == "turn_guard" and r.get("status") == "block_unverified"]
assert len(blocks) >= 2 and blocks[-1]["turn_obs"] != blocks[-2]["turn_obs"], blocks
PY
}

test_missing_turn_id_spends_one_block_per_turn
test_payload_turn_id_keeps_previous_behaviour
test_guard_rows_carry_observation_identity
test_unreadable_helper_reports_an_unguarded_turn
test_unparseable_verdict_reports_an_unguarded_turn
test_crashing_guard_command_reports_an_unguarded_turn
test_allowed_turn_stays_silent

echo "turn-guard-fuse-smoke: ok"
