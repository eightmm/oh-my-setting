#!/usr/bin/env bash
set -euo pipefail

# Focused behavior tests for the operational loop built on top of the existing
# harness state: native hook telemetry, an actionable inbox, bounded CI result
# recovery, source-validated memory, and reversible session checkpoints.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-functional-evolution.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

make_repo() {
  local repo="$1"

  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  printf 'base\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  git -C "$repo" commit -qm base
  mkdir -p "$repo/.oms"
  printf '*\n' > "$repo/.oms/.gitignore"
}

test_native_hook_telemetry_is_content_free_and_correlated() {
  local repo="$TMP/telemetry"
  local payload report

  make_repo "$repo"
  payload="$(printf '%s' \
    '{"hook_event_name":"PostToolUse","session_id":"session-secret","turn_id":"turn-7","cwd":"'"$repo"'","tool_name":"Bash","tool_input":{"command":"echo private-command"},"tool_response":{"output":"private-output","success":true,"duration_ms":125},"usage":{"input_tokens":120,"output_tokens":30,"cache_read_input_tokens":40,"reasoning_tokens":5},"model":"claude-test"}')"
  printf '%s' "$payload" | bash "$ROOT/scripts/telemetry-hook.sh"

  [ -s "$repo/.oms/hooks/events.jsonl" ] || fail "telemetry hook did not record an event"
  if grep -Eq 'session-secret|private-command|private-output' "$repo/.oms/hooks/events.jsonl"; then
    fail "telemetry persisted raw session, command, or output content"
  fi
  python3 - "$repo/.oms/hooks/events.jsonl" <<'PY' || fail "telemetry event shape is wrong"
import json, sys
row = json.loads(open(sys.argv[1], encoding="utf-8").readline())
assert row["schema"] == 1 and row["action"] == "telemetry", row
assert row["hook"] == "PostToolUse" and row["turn_id"] == "turn-7", row
assert row["tool_name"] == "Bash" and row["model"] == "claude-test", row
assert row["duration_ms"] == 125 and row["success"] is True, row
assert row["input_tokens"] == 120 and row["output_tokens"] == 30, row
assert row["cache_read_tokens"] == 40 and row["reasoning_tokens"] == 5, row
assert len(row["session"]) == 32, row
PY

  mkdir -p "$repo/.oms/artifacts"
  cat > "$repo/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_verified","operation_id":"op_verified","artifact_id":"sha256:verified","ts":"2026-08-02T00:00:00Z","kind":"call","provider":"codex","exit":0,"verify_exit":0,"model_class":"fast","selected_model":"gpt-test","tokens":50,"duration_s":2.5}
{"schema":1,"event_id":"evt_partial","operation_id":"op_partial","artifact_id":"sha256:partial","ts":"2026-08-02T00:00:01Z","kind":"call","provider":"codex","verify_exit":0,"model_class":"fast","selected_model":"gpt-test"}
EOF
  report="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" --json telemetry)" ||
    fail "combined telemetry report failed"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "combined telemetry report lost native activity or outcomes"
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
assert r["semantic_outcome"] == "mechanical-only", r
assert r["outcomes"] == {"verified_failure": 0, "verified_success": 1, "unknown": 1}, r
assert r["native_activity"]["sessions"] == 1, r
assert r["native_activity"]["turns"] == 1, r
assert r["native_activity"]["tool_events"] == 1, r
assert r["native_activity"]["usage"]["input_tokens"] == 120, r
assert r["coverage"]["verification_reports"] == 2, r
PY
}

write_fake_gh() {
  local path="$1"
  local sha="$2"
  local counter="$3"

  mkdir -p "$(dirname "$path")"
  # The fixture is patched in as a literal script so shell expansion happens
  # when the fake is invoked, not while this test file is parsed.
  python3 - "$path" "$sha" "$counter" <<'PY'
import os, sys
path, sha, counter = sys.argv[1:]
body = '''#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-} ${2:-}" = "run list" ]; then
  count=0
  [ ! -f "COUNTER" ] || count="$(cat "COUNTER")"
  printf '%s\n' "$((count + 1))" > "COUNTER"
  printf '%s\n' '[{"status":"completed","conclusion":"success","workflowName":"test","headSha":"SHA","url":"https://example.invalid/run/1"}]'
elif [ "${1:-} ${2:-}" = "pr view" ]; then
  printf '%s\n' '{}'
else
  exit 2
fi
'''.replace("COUNTER", counter).replace("SHA", sha)
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    handle.write(body)
os.chmod(path, 0o755)
PY
}

test_ci_tick_records_once_and_skips_a_fresh_sha() {
  local repo="$TMP/ci-tick"
  local bin="$TMP/ci-bin"
  local counter="$TMP/ci-count"
  local sha

  make_repo "$repo"
  sha="$(git -C "$repo" rev-parse HEAD)"
  write_fake_gh "$bin/gh" "$sha" "$counter"

  (cd "$repo" && OMS_GH_BIN="$bin/gh" OMS_CI_TICK_INTERVAL=3600 \
    OMS_LOCK_DIR="$TMP/locks" bash "$ROOT/scripts/ci-status.sh" tick main) >/dev/null ||
    fail "first CI tick should record the completed run"
  [ "$(cat "$counter")" = 1 ] || fail "first CI tick should make one run query"
  grep -Fq "\"sha\": \"$sha\"" "$repo/.oms/ci.jsonl" ||
    fail "CI tick did not record the current SHA"

  (cd "$repo" && OMS_GH_BIN="$bin/gh" OMS_CI_TICK_INTERVAL=3600 \
    OMS_LOCK_DIR="$TMP/locks" bash "$ROOT/scripts/ci-status.sh" tick main) >/dev/null ||
    fail "fresh CI tick should be a no-op"
  [ "$(cat "$counter")" = 1 ] || fail "fresh CI was queried again"
}

test_inbox_ranks_state_and_applies_only_safe_repairs() {
  local repo="$TMP/inbox"
  local bin="$TMP/inbox-bin"
  local counter="$TMP/inbox-count"
  local sha report

  make_repo "$repo"
  sha="$(git -C "$repo" rev-parse HEAD)"
  mkdir -p "$repo/.oms/plan" "$repo/.oms/artifacts"
  cat > "$repo/.oms/plan/tasks.json" <<'EOF'
{"schema":1,"goal":"finish","tasks":{"old":{"id":"old","title":"old","state":"claimed","depends":[],"provider":"claude","claimed_at":"2020-01-01T00:00:00Z","updated":"2020-01-01T00:00:00Z"}}}
EOF
  cat > "$repo/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_open","operation_id":"op_open","artifact_id":"sha256:open","ts":"2026-08-02T00:00:00Z","kind":"call","provider":"codex","exit":1}
EOF
  cat > "$repo/.oms/failures.jsonl" <<'EOF'
{"schema":1,"ts":"2026-08-02T00:00:00Z","event":"fail","fingerprint":"f1","count":1,"summary":"broken"}
EOF
  cat > "$repo/.oms/ci.jsonl" <<'EOF'
{"schema":1,"ts":"2026-08-01T00:00:00Z","branch":"main","sha":"oldsha","status":"completed","conclusion":"success","url":"https://example.invalid/old"}
EOF

  report="$(bash "$ROOT/scripts/inbox.sh" --repo "$repo" --json)" || fail "inbox query failed"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "inbox did not rank the actionable state"
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
codes = [item["code"] for item in r["items"]]
assert r["schema"] == 1 and r["actionable"] >= 4, r
assert codes[:2] == ["stale-plan-claim", "ci-stale"], codes
assert "unresolved-artifacts" in codes and "open-failures" in codes, codes
assert all(item.get("command") for item in r["items"]), r
PY

  write_fake_gh "$bin/gh" "$sha" "$counter"
  (cd "$repo" && OMS_GH_BIN="$bin/gh" OMS_LOCK_DIR="$TMP/inbox-locks" \
    bash "$ROOT/scripts/inbox.sh" --repo . --fix-safe --json) > "$TMP/inbox-fixed" ||
    fail "safe inbox repair failed"
  python3 - "$repo/.oms/plan/tasks.json" "$repo/.oms/ci.jsonl" "$sha" <<'PY' || fail "safe inbox repair did not reclaim/refresh exactly its safe targets"
import json, sys
plan = json.load(open(sys.argv[1], encoding="utf-8"))
assert plan["tasks"]["old"]["state"] == "ready", plan
rows = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert rows[-1]["sha"] == sys.argv[3], rows[-1]
PY
  grep -Fq '"code": "unresolved-artifacts"' "$TMP/inbox-fixed" ||
    fail "safe repair must leave judgment-requiring artifacts visible"
  grep -Fq '"code": "open-failures"' "$TMP/inbox-fixed" ||
    fail "safe repair must leave failures visible"
}

test_memory_citations_revalidate_and_stay_out_of_default_context() {
  local repo="$TMP/memory-citation"
  local source_path="src/규칙 파일.txt"
  local out

  make_repo "$repo"
  mkdir -p "$repo/src"
  printf 'alpha\nimportant invariant\nomega\n' > "$repo/$source_path"
  git -C "$repo" add "$source_path"
  git -C "$repo" commit -qm rules

  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" append --agent codex \
    --source-file "$source_path" --source-line 2 \
    --text "The important invariant is enforced here." >/dev/null
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" context)"
  if printf '%s' "$out" | grep -Fq 'important invariant is enforced'; then
    fail "source-derived facts must be recalled after validation, not injected blindly"
  fi
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" recall --json "important invariant")" ||
    fail "valid cited fact was not recalled"
  OMS_TEST_REPORT="$out" python3 - <<'PY' || fail "valid citation metadata is wrong"
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
assert r["citation"]["status"] == "valid", r
assert r["citation"]["path"] == "src/규칙 파일.txt" and r["citation"]["line"] == 2, r
PY

  printf 'prefix\nalpha\nimportant invariant\nomega\n' > "$repo/$source_path"
  git -C "$repo" add "$source_path"
  git -C "$repo" commit -qm move-rule
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" recall --json "important invariant")" ||
    fail "a uniquely moved citation should remain valid"
  OMS_TEST_REPORT="$out" python3 - <<'PY' || fail "moved citation was not relocated"
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
assert r["citation"]["status"] == "moved" and r["citation"]["current_line"] == 3, r
PY

  printf 'prefix\nalpha\nchanged invariant\nomega\n' > "$repo/$source_path"
  git -C "$repo" add "$source_path"
  git -C "$repo" commit -qm change-rule
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" recall --json \
    "important invariant" >/dev/null 2>&1; then
    fail "stale cited facts must be omitted from normal recall"
  fi
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" --include-stale \
    recall --json "important invariant")" || fail "stale citation should remain auditable"
  OMS_TEST_REPORT="$out" python3 - <<'PY' || fail "stale citation status is missing"
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
assert r["citation"]["status"] == "stale", r
PY
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" rebuild >/dev/null
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" --include-stale \
    recall --json "important invariant" | grep -Fq '"status": "stale"' ||
    fail "rebuild lost citation provenance"
}

test_checkpoint_restores_staged_and_unstaged_content_with_a_backup() {
  local repo="$TMP/checkpoint"
  local created checkpoint_id before_dry

  make_repo "$repo"
  printf 'b-base\n' > "$repo/b.txt"
  git -C "$repo" add b.txt
  git -C "$repo" commit -qm add-b

  printf 'staged-a\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  printf 'unstaged-b\n' > "$repo/b.txt"
  created="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --label before --json)" ||
    fail "checkpoint creation failed"
  checkpoint_id="$(OMS_TEST_REPORT="$created" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])')"
  [ -n "$checkpoint_id" ] || fail "checkpoint create returned no id"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" verify "$checkpoint_id") >/dev/null ||
    fail "fresh checkpoint did not verify"

  printf 'later-a\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  printf 'later-b\n' > "$repo/b.txt"
  printf 'untracked survives\n' > "$repo/local.txt"
  before_dry="$(git -C "$repo" diff HEAD -- file.txt b.txt)"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id") > "$TMP/checkpoint-dry" ||
    fail "checkpoint dry-run failed"
  [ "$before_dry" = "$(git -C "$repo" diff HEAD -- file.txt b.txt)" ] ||
    fail "checkpoint restore mutated files without --apply"
  grep -Fq 'dry-run' "$TMP/checkpoint-dry" || fail "restore did not disclose dry-run"

  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) >/dev/null ||
    fail "checkpoint apply failed"
  grep -Fq 'staged-a' "$repo/file.txt" || fail "staged checkpoint content was not restored"
  grep -Fq 'unstaged-b' "$repo/b.txt" || fail "unstaged checkpoint content was not restored"
  grep -Fq 'untracked survives' "$repo/local.txt" || fail "untracked file was touched"
  git -C "$repo" diff --cached -- file.txt | grep -Fq 'staged-a' ||
    fail "checkpoint did not restore the index state"
  git -C "$repo" diff -- b.txt | grep -Fq 'unstaged-b' ||
    fail "checkpoint did not restore unstaged state"
  [ "$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" list --json | wc -l | tr -d ' ')" -ge 2 ] ||
    fail "restore did not create a recovery checkpoint"

  git -C "$repo" add file.txt b.txt
  git -C "$repo" commit -qm new-head
  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) \
    >/dev/null 2>&1; then
    fail "checkpoint restore must refuse a different HEAD"
  fi
}

test_hook_installers_include_native_telemetry_events() {
  local settings="$TMP/claude-settings.json"
  local home_dir="$TMP/claude-home"

  mkdir -p "$home_dir"
  printf '{}\n' > "$settings"
  HOME="$home_dir" OMS_CLAUDE_SETTINGS="$settings" \
    bash "$ROOT/scripts/install-claude-hooks.sh" >/dev/null
  python3 - "$settings" <<'PY' || fail "Claude telemetry hooks were not installed"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
for event in ("SessionStart", "PostToolUse", "SubagentStop", "SessionEnd"):
    commands = [h.get("command", "") for entry in hooks.get(event, []) for h in entry.get("hooks", [])]
    assert any("telemetry-hook.sh" in command for command in commands), (event, commands)
PY
  for event in SessionStart PostToolUse SubagentStop SessionEnd; do
    grep -Fq "\"$event\"" "$ROOT/plugins/oh-my-setting/hooks.json" ||
      fail "Codex plugin is missing $event telemetry"
  done
}

test_gc_bounds_old_checkpoint_and_hook_state() {
  local repo="$TMP/retention"
  local checkpoint_id out

  make_repo "$repo"
  checkpoint_id="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  mkdir -p "$repo/.oms/hooks/sessions"
  cat > "$repo/.oms/hooks/events.jsonl" <<'EOF'
{"schema":1,"ts":"2020-01-01T00:00:00Z","action":"telemetry","session":"old"}
{"schema":1,"ts":"2099-01-01T00:00:00Z","action":"telemetry","session":"new"}
not-json-is-preserved
EOF
  printf '{}\n' > "$repo/.oms/hooks/sessions/old.json"
  python3 - "$repo/.oms/checkpoints/$checkpoint_id/meta.json" \
    "$repo/.oms/hooks/sessions/old.json" <<'PY'
import json, os, sys, time
meta_path, session_path = sys.argv[1:]
with open(meta_path, encoding="utf-8") as handle:
    meta = json.load(handle)
meta["created_at"] = "2020-01-01T00:00:00Z"
with open(meta_path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(meta, handle, sort_keys=True)
    handle.write("\n")
old = time.time() - 10 * 86400
os.utime(session_path, (old, old))
PY

  out="$(bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --dry-run)"
  printf '%s' "$out" | grep -Fq 'checkpoint:' ||
    fail "gc did not report the old local checkpoint"
  printf '%s' "$out" | grep -Fq 'hook-events: compact 3 -> 2 rows' ||
    fail "gc did not report old hook-event compaction"
  printf '%s' "$out" | grep -Fq 'hook-session:' ||
    fail "gc did not report the old hook session"
  [ -d "$repo/.oms/checkpoints/$checkpoint_id" ] ||
    fail "gc dry-run removed a checkpoint"

  bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --apply >/dev/null
  [ ! -e "$repo/.oms/checkpoints/$checkpoint_id" ] ||
    fail "gc apply kept an expired checkpoint"
  [ ! -e "$repo/.oms/hooks/sessions/old.json" ] ||
    fail "gc apply kept an expired hook session"
  grep -Fq '"session":"new"' "$repo/.oms/hooks/events.jsonl" ||
    fail "gc removed the current hook event"
  grep -Fq 'not-json-is-preserved' "$repo/.oms/hooks/events.jsonl" ||
    fail "gc removed an unparseable diagnostic hook row"
  if grep -Fq '"session":"old"' "$repo/.oms/hooks/events.jsonl"; then
    fail "gc kept the expired hook event"
  fi
}

test_native_hook_telemetry_is_content_free_and_correlated
test_ci_tick_records_once_and_skips_a_fresh_sha
test_inbox_ranks_state_and_applies_only_safe_repairs
test_memory_citations_revalidate_and_stay_out_of_default_context
test_checkpoint_restores_staged_and_unstaged_content_with_a_backup
test_hook_installers_include_native_telemetry_events
test_gc_bounds_old_checkpoint_and_hook_state

echo "functional evolution smoke: ok"
