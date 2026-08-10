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
PY
}

test_verification_disclosure_boundaries() {
  python3 - "$ROOT/scripts/lib/hook_state.py" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("hook_state", sys.argv[1])
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)

for text in ("파일을 확인했습니다.", "Tests were not run.", "Done."):
    assert not module.has_verification_disclosure(text), text
for text in (
    "Verification: bash tests passed.",
    "Not verified: shellcheck is unavailable.",
    "검증: pytest 통과.",
    "미검증: GPU를 사용할 수 없음.",
):
    assert module.has_verification_disclosure(text), text
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

  (cd "$repo" && printf '%s' "$failed" | bash "$hook") >/dev/null
  (cd "$repo" && printf '%s' "$failed" | bash "$hook") >/dev/null
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

test_classifier_boundaries
test_verification_disclosure_boundaries
test_explicit_goal_rotation
test_fail_ledger_hook_resolves_on_success
test_route_is_hermetic_to_inherited_harness_session
echo "autonomy-hook-smoke: ok"
