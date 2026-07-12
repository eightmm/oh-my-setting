#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-autonomy-hook.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

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

route_prompt() {
  local repo="$1"
  local session="$2"
  local turn="$3"
  local prompt="$4"
  OMS_AUTO_TASK=1 OMS_AGENT=test OMS_HOOK_PAYLOAD="$(
    python3 - "$repo" "$session" "$turn" "$prompt" <<'PY'
import json, sys
print(json.dumps({"cwd": sys.argv[1], "session_id": sys.argv[2],
                  "turn_id": sys.argv[3], "prompt": sys.argv[4]}))
PY
  )" python3 "$ROOT/scripts/lib/hook_state.py" route --manifest "$repo/manifest.json"
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

test_classifier_boundaries
test_explicit_goal_rotation
echo "autonomy-hook-smoke: ok"
