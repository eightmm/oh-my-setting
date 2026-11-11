#!/usr/bin/env bash
set -euo pipefail

# Focused behavior tests for the operational loop built on top of the existing
# harness state: native hook telemetry, an actionable inbox, bounded CI result
# recovery, source-validated memory, and reversible session checkpoints.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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
  # Old hook registrations must not flood the ledger with metricless activity.
  for event in PostToolUse SubagentStop; do
    printf '{"hook_event_name":"%s","cwd":"%s","success":true}' "$event" "$repo" |
      bash "$ROOT/scripts/telemetry-hook.sh"
  done
  [ ! -e "$repo/.oms/hooks/events.jsonl" ] || fail "empty activity was recorded"
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
  # Boundaries remain live-peer evidence; failures and explicit debug survive.
  for event in SessionStart SessionEnd; do
    printf '{"hook_event_name":"%s","cwd":"%s"}' "$event" "$repo" |
      bash "$ROOT/scripts/telemetry-hook.sh"
  done
  printf '{"hook_event_name":"SubagentStop","cwd":"%s","success":false}' "$repo" |
    bash "$ROOT/scripts/telemetry-hook.sh"
  printf '{"hook_event_name":"PostToolUse","cwd":"%s"}' "$repo" |
    OMS_TELEMETRY_DEBUG=1 bash "$ROOT/scripts/telemetry-hook.sh"
  [ "$(wc -l < "$repo/.oms/hooks/events.jsonl" | tr -d ' ')" = 5 ] ||
    fail "lifecycle, failure or debug evidence was lost"
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
  if grep -Fq '"code": "stale-plan-claim"' "$TMP/inbox-fixed"; then
    fail "safe repair must project fresh state after reclaim, not its pre-repair snapshot"
  fi
}

test_inbox_fix_safe_reports_ci_refresh_only_when_ledger_changed() {
  local repo="$TMP/inbox-ci" bin="$TMP/inbox-ci-bin" sha mode out

  make_repo "$repo"
  sha="$(git -C "$repo" rev-parse HEAD)"
  mkdir -p "$repo/.oms" "$bin"
  # mode: fail = gh errors; success/failure = a completed run on HEAD.
  for mode in fail success failure; do
    cat > "$repo/.oms/ci.jsonl" <<'EOF'
{"schema":1,"ts":"2026-08-01T00:00:00Z","branch":"main","sha":"oldsha","status":"completed","conclusion":"success","url":"https://example.invalid/old"}
EOF
    cp "$repo/.oms/ci.jsonl" "$TMP/ci-before"
    cat > "$bin/gh" <<EOF
#!/usr/bin/env bash
case "\${1:-} \${2:-}" in
  "run list")
    [ "$mode" != fail ] || exit 1
    printf '%s\\n' '[{"status":"completed","conclusion":"$mode","workflowName":"t","headSha":"$sha","url":"https://example.invalid/r"}]' ;;
  "pr view") [ "$mode" != fail ] || exit 1; printf '%s\\n' '{}' ;;
  *) exit 2 ;;
esac
EOF
    chmod +x "$bin/gh"
    out="$(cd "$repo" && OMS_GH_BIN="$bin/gh" OMS_LOCK_DIR="$TMP/inbox-ci-locks" \
      bash "$ROOT/scripts/inbox.sh" --repo . --fix-safe --json)" ||
      fail "inbox --fix-safe failed in $mode mode"
    if [ "$mode" = fail ]; then
      cmp -s "$TMP/ci-before" "$repo/.oms/ci.jsonl" || fail "failed gh must not change the ledger"
      case "$out" in *refreshed-ci*) fail "failed CI record reported refreshed-ci" ;; esac
      case "$out" in *ci-refresh-failed*) ;; *) fail "failed CI record must report ci-refresh-failed" ;; esac
    else
      case "$out" in *refreshed-ci*) ;; *) fail "recorded $mode run must report refreshed-ci" ;; esac
      case "$out" in *ci-refresh-failed*) fail "recorded $mode run reported ci-refresh-failed" ;; esac
      grep -Fq "\"conclusion\": \"$mode\"" "$repo/.oms/ci.jsonl" || fail "$mode run not in ledger"
    fi
  done
}

test_unresolved_queue_triages_by_patch_bytes_and_clears_in_one_batch() {
  local repo="$TMP/artifact-queue"
  local index
  local before report

  make_repo "$repo"
  index="$repo/.oms/artifacts/index.jsonl"
  mkdir -p "$repo/.oms/artifacts"
  : > "$repo/.oms/artifacts/work.patch"
  : > "$repo/.oms/artifacts/empty.patch"
  : > "$repo/.oms/artifacts/other.patch"
  # evt_rewritten_fail shares the patch PATH with the later success but not its
  # bytes, and evt_peer_fail is a fan-out sibling of a synthesis that succeeded:
  # both are the wrong answers a looser join would give.
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_delegate_fail","operation_id":"delegate-1","artifact_id":"sha256:d1","ts":"2026-08-03T04:01:00Z","kind":"delegate","provider":"codex","exit":3,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_rewritten_fail","operation_id":"op_1","artifact_id":"sha256:r1","ts":"2026-08-03T04:02:00Z","kind":"patch-admit","provider":"","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
{"schema":1,"event_id":"evt_admit_fail","operation_id":"op_2","artifact_id":"sha256:a1","ts":"2026-08-03T04:03:00Z","kind":"patch-admit","provider":"","exit":1,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_admit_ok","operation_id":"op_3","artifact_id":"sha256:a2","ts":"2026-08-03T04:04:00Z","kind":"patch-admit","provider":"","exit":0,"patch":".oms/artifacts/work.patch","patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_empty_fail","operation_id":"delegate-2","artifact_id":"sha256:e1","ts":"2026-08-03T04:05:00Z","kind":"delegate","provider":"codex","exit":3,"patch":".oms/artifacts/empty.patch","patch_sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
{"schema":1,"event_id":"evt_empty_land","operation_id":"op_4","artifact_id":"sha256:e2","ts":"2026-08-03T04:06:00Z","kind":"patch-land","provider":"","exit":0,"patch":".oms/artifacts/other.patch","patch_sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
{"schema":1,"event_id":"evt_peer_fail","operation_id":"ask-1","artifact_id":"sha256:p1","ts":"2026-08-03T04:07:00Z","kind":"ask","provider":"gemini","exit":1}
{"schema":1,"event_id":"evt_peer_synthesis","operation_id":"ask-1","artifact_id":"sha256:p2","ts":"2026-08-03T04:08:00Z","kind":"ask-synthesis","provider":"claude","exit":0}
EOF

  report="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" --json unresolved 20)" ||
    fail "unresolved queue query failed"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "supersession was claimed on the wrong join"
import json, os
rows = json.loads(os.environ["OMS_TEST_REPORT"])["rows"]
superseded = {row["event_id"]: row.get("superseded_by") for row in rows}
assert superseded["evt_delegate_fail"] == "evt_admit_ok", superseded
assert superseded["evt_admit_fail"] == "evt_admit_ok", superseded
assert superseded["evt_rewritten_fail"] is None, superseded
assert superseded["evt_empty_fail"] is None, superseded
assert superseded["evt_peer_fail"] is None, superseded
assert "evt_admit_ok" not in superseded, superseded
PY

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 20 > "$TMP/queue-before"
  grep -Fq 'superseded-by: evt_admit_ok  next: oms artifact-index resolve --event-id evt_admit_fail --reason "superseded by evt_admit_ok"' \
    "$TMP/queue-before" || fail "a superseded row must print its prefilled resolve command"
  grep -Fq -- '--event-id evt_peer_fail --reason "<why this failure is no longer open>"' \
    "$TMP/queue-before" || fail "a row with no supersession still needs a resolve command"
  [ "$(grep -c 'next: oms artifact-index resolve' "$TMP/queue-before")" -eq 5 ] ||
    fail "the queue must print exactly one resolve command per row"
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" list 20 > "$TMP/list-after"
  if grep -Fq 'next: oms artifact-index resolve' "$TMP/list-after"; then
    fail "triage belongs to the queue view, not the list inventory"
  fi

  before="$(wc -l < "$index" | tr -d ' ')"
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_admit_fail --event-id evt_unknown >/dev/null 2>&1; then
    fail "a batch resolve naming an unknown event should fail"
  fi
  [ "$before" = "$(wc -l < "$index" | tr -d ' ')" ] ||
    fail "a rejected batch must leave the index untouched"

  # One resolution per distinct target: the scalar --event-id this replaced kept
  # only the last id and silently dropped the rest of the batch.
  OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_delegate_fail --event-id evt_admit_fail --event-id evt_delegate_fail \
    --reason "superseded by evt_admit_ok" >/dev/null || fail "batch resolve failed"
  [ "$((before + 2))" = "$(wc -l < "$index" | tr -d ' ')" ] ||
    fail "repeated --event-id must resolve each distinct target exactly once"
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "batch resolution produced invalid lineage"

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 20 > "$TMP/queue-after"
  if grep -Eq 'event=evt_delegate_fail|event=evt_admit_fail' "$TMP/queue-after"; then
    fail "batch-resolved rows should leave the queue"
  fi
  grep -Fq 'event=evt_peer_fail' "$TMP/queue-after" ||
    fail "untriaged rows must survive a batch that did not name them"
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
  if printf '%s' "$out" | grep >/dev/null -F 'important invariant is enforced'; then
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
    recall --json "important invariant" | grep -F '"status": "stale"' > /dev/null ||
    fail "rebuild lost citation provenance"
}

test_memory_supersession_and_retirement() {
  local repo="$TMP/memory-links" old mid pin before out newest unsafe
  make_repo "$repo"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" append --text "orchid old fact" >/dev/null
  old="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --json "orchid old fact" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$old" --text "orchid middle fact" >/dev/null
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search "orchid old fact" >/dev/null 2>&1; then fail "superseded search result visible"; fi
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" recall "orchid old fact" || true)"
  [[ "$out" != *"orchid old fact"* ]] || fail "superseded recall result visible"
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --include-superseded --json "orchid old fact")"
  OMS_TEST_REPORT="$out" python3 -c 'import json,os; assert json.loads(os.environ["OMS_TEST_REPORT"])["supersession"]["status"] == "superseded"' || fail "missing supersession marker"
  mid="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --json "orchid middle fact" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  printf 'orchid newest fact\n' | bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$mid" --stdin >/dev/null
  newest="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --json "orchid newest fact" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" context)"
  [[ "$out" == *"orchid newest fact"* && "$out" != *"orchid old fact"* && "$out" != *"orchid middle fact"* ]] || fail "context contains superseded facts"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" pin --text "violet pinned fact" >/dev/null
  pin="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --json "violet pinned fact" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" retire --id "$pin" --reason "no longer true" >/dev/null
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search --include-superseded "no longer true" >/dev/null 2>&1; then fail "retirement record returned as note"; fi
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" context)"
  [[ "$out" != *"violet pinned fact"* ]] || fail "retired pin injected"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" recall --include-superseded --json "violet pinned fact" | grep -F '"status": "retired"' >/dev/null || fail "retirement marker missing"
  before="$(wc -c < "$repo/.oms/memory/shared.md")"
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" retire --id "$pin" --reason again >/dev/null 2>&1; then fail "duplicate retirement accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" retire --id unknown --reason absent >/dev/null 2>&1; then fail "unknown ID accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$mid" --text bad >/dev/null 2>&1; then fail "inactive target accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" retire --id 'bad id' --reason absent >/dev/null 2>&1; then fail "malformed ID accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$newest" --text '' >/dev/null 2>&1; then fail "empty replacement accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$newest" --text '  ' >/dev/null 2>&1; then fail "blank replacement accepted"; fi
  if printf '\n' | bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$newest" --stdin >/dev/null 2>&1; then fail "blank stdin accepted"; fi
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" retire --id "$newest" --reason '  ' >/dev/null 2>&1; then fail "blank reason accepted"; fi
  unsafe="$(printf '%s%s%s' 'pass' 'word' '=not-a-real-value')"
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" supersede --id "$newest" --text "$unsafe" >/dev/null 2>&1; then fail "sensitive replacement accepted"; fi
  [ "$before" = "$(wc -c < "$repo/.oms/memory/shared.md")" ] || fail "failed update appended"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" rebuild >/dev/null
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" search "orchid old fact" >/dev/null 2>&1; then fail "rebuild lost supersession"; fi

  # A clean old summary cannot make freshly generated sensitive text safe.
  printf '\n## 2026-06-10T00:00:00Z agent\n\n%s%s%s\n' 'pass' 'word' '=sensitive-value' >> "$repo/.oms/memory/shared.md"
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$repo" context)"
  [[ "$out" != *"sensitive-value"* ]] || fail "sensitive source leaked into context"

  local empty_repo="$TMP/memory-unknown"
  mkdir -p "$empty_repo/.oms/memory"
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$empty_repo" retire --id unknown --reason absent >/dev/null 2>&1; then fail "unknown target accepted in empty memory"; fi
  [ ! -e "$empty_repo/.oms/.gitignore" ] || fail "unknown target created ignore file"

  local full_repo="$TMP/memory-full"
  make_repo "$full_repo"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$full_repo" append --text "sole full entry" >/dev/null
  old="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$full_repo" search --json "sole full entry" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$full_repo" retire --id "$old" --reason "obsolete item" >/dev/null
  out="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$full_repo" context --full)"
  [ -z "$out" ] || fail "empty full context emitted a header"
  out="$(bash -c '. "$1"; AGENT_MEMORY_DB_HELPER="$2/missing"; agent_memory_emit_full_section project "$3" || true' _ "$ROOT/scripts/lib/agent-memory-common.sh" "$TMP" "$full_repo/.oms/memory/shared.md" 2>/dev/null)"
  [ -z "$out" ] || fail "failed full context emitted a header"

  local partial_repo="$TMP/memory-partial"
  make_repo "$partial_repo"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$partial_repo" append --text "intact original" >/dev/null
  old="$(bash "$ROOT/scripts/agent-memory.sh" --repo "$partial_repo" search --json "intact original" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$ROOT/scripts/agent-memory.sh" --repo "$partial_repo" supersede --id "$old" --text "unfinished replacement" >/dev/null
  python3 - "$partial_repo/.oms/memory/shared.md" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
data = path.read_bytes()
marker = b'<!-- oms-memory-link-complete -->\n'
assert data.endswith(marker)
path.write_bytes(data[:-len(marker)])
PY
  bash "$ROOT/scripts/agent-memory.sh" --repo "$partial_repo" search "intact original" >/dev/null || fail "partial link hid the original"
  if bash "$ROOT/scripts/agent-memory.sh" --repo "$partial_repo" search "unfinished replacement" >/dev/null 2>&1; then fail "partial record indexed"; fi
}

test_memory_redaction_and_native_sources() {
  local repo="$TMP/memory-redact" am="$ROOT/scripts/agent-memory.sh" note pin link out before file home
  make_repo "$repo"
  bash "$am" --repo "$repo" append --text "keep heron fact" >/dev/null
  printf 'magpie line one\nmagpie line two\n' | bash "$am" --repo "$repo" append --stdin >/dev/null
  bash "$am" --repo "$repo" pin --text "wagtail pinned fact" >/dev/null
  note="$(bash "$am" --repo "$repo" search --json "magpie line one" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  pin="$(bash "$am" --repo "$repo" search --json "wagtail pinned fact" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$am" --repo "$repo" recall "magpie" >/dev/null   # leaves an access receipt in the index

  # Dry run by default: it names the entry, never its text, and changes nothing.
  before="$(cksum < "$repo/.oms/memory/shared.md")"
  out="$(bash "$am" --repo "$repo" redact --id "$note" 2>&1)" || fail "redact dry run failed: $out"
  [[ "$out" == *"would redact shared note ($note,"* && "$out" == *"dry run; nothing changed"* ]] || fail "dry run did not name the entry: $out"
  [[ "$out" != *magpie* ]] || fail "dry run printed the text it would remove"
  [ "$before" = "$(cksum < "$repo/.oms/memory/shared.md")" ] || fail "dry run changed the source log"
  bash "$am" --repo "$repo" search "magpie line one" >/dev/null || fail "dry run hid the entry"

  # Refusals change nothing: a worker, an unknown ID, a malformed ID, --apply elsewhere.
  if OMS_HARNESS_CHILD=1 bash "$am" --repo "$repo" redact --id "$note" --apply >/dev/null 2>&1; then fail "a harness child redacted memory"; fi
  if bash "$am" --repo "$repo" redact --id mem-unknown --apply >/dev/null 2>&1; then fail "unknown ID redacted"; fi
  if bash "$am" --repo "$repo" redact --id 'bad id' --apply >/dev/null 2>&1; then fail "malformed ID accepted"; fi
  if bash "$am" --repo "$repo" redact --apply >/dev/null 2>&1; then fail "redact without --id accepted"; fi
  if bash "$am" --repo "$repo" search --apply heron >/dev/null 2>&1; then fail "--apply accepted outside redact"; fi
  [ "$before" = "$(cksum < "$repo/.oms/memory/shared.md")" ] || fail "a refused redact changed the source log"

  out="$(bash "$am" --repo "$repo" redact --id "$note" --apply 2>&1)" || fail "redact failed: $out"
  [[ "$out" == *"redacted shared note ($note,"* && "$out" == *"31 bytes removed from shared.md"* ]] || fail "redact did not report what it removed: $out"
  [[ "$out" != *magpie* ]] || fail "redact printed the removed text"
  out="$(bash "$am" --repo "$repo" redact --id "$pin" --apply 2>&1)" || fail "pin redact failed: $out"
  [[ "$out" != *wagtail* ]] || fail "pin redact printed the removed text"
  # The bytes are gone from the readable store and from every derived copy, journals included.
  for file in "$repo"/.oms/memory/* "$repo"/.oms/memory/.[!.]*; do
    [ -f "$file" ] || continue
    ! LC_ALL=C grep -aq -e magpie -e wagtail "$file" || fail "removed text survives in ${file##*/}"
  done
  [ ! -e "$repo/.oms/memory/memory.sqlite3-wal" ] || ! LC_ALL=C grep -aq magpie "$repo/.oms/memory/memory.sqlite3-wal" || fail "removed text survives in the journal"
  # The ID, time and author stay under a fixed tombstone; the neighbours are untouched.
  grep -Fq "event_id: $note" "$repo/.oms/memory/shared.md" || fail "redaction dropped the entry's ID"
  grep -Eq '^\[redacted [0-9TZ:-]+ by [A-Za-z0-9._-]+: text removed with `oms agent-memory redact`; 31 bytes\]$' "$repo/.oms/memory/shared.md" || fail "tombstone missing"
  grep -Eq '^- [0-9TZ:-]+ \[[^]]+\] \[redacted .*; [0-9]+ bytes\]$' "$repo/.oms/memory/pins.md" || fail "pin tombstone missing"
  bash "$am" --repo "$repo" search "keep heron fact" >/dev/null || fail "redaction damaged a neighbouring note"
  if bash "$am" --repo "$repo" search "magpie" >/dev/null 2>&1; then fail "redacted text still searchable"; fi
  if bash "$am" --repo "$repo" recall "wagtail" >/dev/null 2>&1; then fail "redacted pin still recalled"; fi
  # A tombstone is not a memory: default search, recall and context leave it out; the audit view marks it.
  if bash "$am" --repo "$repo" search "redacted" >/dev/null 2>&1; then fail "tombstone returned as a note"; fi
  out="$(bash "$am" --repo "$repo" context)"
  [[ "$out" == *"keep heron fact"* && "$out" != *redacted* ]] || fail "tombstone injected into context: $out"
  bash "$am" --repo "$repo" search --include-superseded --json "redacted" | grep -F '"status": "redacted"' >/dev/null || fail "redaction marker missing"
  bash "$am" --repo "$repo" health >/dev/null || fail "memory unhealthy after redaction"
  # Idempotent, and a redacted note is no longer a target for supersede.
  before="$(cksum < "$repo/.oms/memory/shared.md")"
  out="$(bash "$am" --repo "$repo" redact --id "$note" --apply 2>&1)" || fail "second redact failed"
  [[ "$out" == *"already redacted"* ]] || fail "second redact did not say so: $out"
  [ "$before" = "$(cksum < "$repo/.oms/memory/shared.md")" ] || fail "second redact rewrote the log"
  if bash "$am" --repo "$repo" supersede --id "$note" --text "replacement" >/dev/null 2>&1; then fail "redacted note superseded"; fi

  # A linked record keeps its link: redacting a replacement leaves the original superseded.
  bash "$am" --repo "$repo" append --text "siskin original" >/dev/null
  note="$(bash "$am" --repo "$repo" search --json "siskin original" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$am" --repo "$repo" supersede --id "$note" --text "siskin replacement" >/dev/null
  link="$(bash "$am" --repo "$repo" search --json "siskin replacement" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')"
  bash "$am" --repo "$repo" redact --id "$link" --apply >/dev/null || fail "linked record redact failed"
  ! grep -Fq "siskin replacement" "$repo/.oms/memory/shared.md" || fail "linked record text survives"
  if bash "$am" --repo "$repo" search "siskin original" >/dev/null 2>&1; then fail "redacting a replacement revived the note it superseded"; fi
  bash "$am" --repo "$repo" search --include-superseded --json "siskin original" | grep -F '"status": "superseded"' >/dev/null || fail "redaction broke the supersession link"

  # sources: names, sizes and counts of what each provider keeps; no file is opened.
  home="$TMP/memory-sources-home"
  mkdir -p "$home/.claude/rules" "$home/.claude/projects/$(printf '%s' "$(cd "$repo" && pwd -P)" | tr -c 'A-Za-z0-9' '-')/memory" \
    "$home/.codex/memories" "$repo/.claude/agent-memory/reviewer"
  printf 'finch-marker global rules\n' > "$home/.claude/CLAUDE.md"
  printf 'finch-marker index\n' > "$home/.claude/projects/$(printf '%s' "$(cd "$repo" && pwd -P)" | tr -c 'A-Za-z0-9' '-')/memory/MEMORY.md"
  printf 'finch-marker a\n' > "$home/.codex/memories/a.md"
  printf 'finch-marker b\n' > "$home/.codex/memories/b.md"
  printf 'finch-marker agent\n' > "$repo/.claude/agent-memory/reviewer/MEMORY.md"
  printf 'finch-marker project\n' > "$repo/AGENTS.md"
  chmod 000 "$home/.codex/memories/a.md"   # unreadable: sizes come from stat, not from reading
  before="$(find "$repo" "$home" -newer "$repo/AGENTS.md" | wc -l)"
  out="$(HOME="$home" CODEX_HOME="" bash "$am" --repo "$repo" sources)" || fail "sources failed"
  chmod 600 "$home/.codex/memories/a.md"
  [[ "$out" == *"names, sizes and counts only"* ]] || fail "sources header missing: $out"
  [[ "$out" == *"~/.claude/CLAUDE.md"* && "$out" == *"auto-memory"* && "$out" == *"subagent-memory"*".claude/agent-memory/"* ]] || fail "sources missed Claude files: $out"
  [[ "$out" == *"~/.codex/memories/"*"2 file(s)"* && "$out" == *"AGENTS.md"* && "$out" == *".oms/memory/"* ]] || fail "sources missed Codex or OMS files: $out"
  [[ "$out" != *finch-marker* && "$out" != *"$home"* ]] || fail "sources printed file contents or the home path"
  [ "$before" = "$(find "$repo" "$home" -newer "$repo/AGENTS.md" | wc -l)" ] || fail "sources wrote something"
  HOME="$home" CODEX_HOME="" bash "$am" --repo "$repo" sources --json | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert report["schema"] == 1 and report["contents_read"] is False, report
roles = {(row["provider"], row["role"]) for row in report["sources"]}
assert {("claude", "auto-memory"), ("codex", "memories"), ("oms", "memory")} <= roles, roles
codex = next(row for row in report["sources"] if row["role"] == "memories")
assert codex["files"] == 2 and codex["bytes"] == 30, codex
assert all(set(row) <= {"provider", "role", "path", "kind", "bytes", "files", "modified", "link", "capped"}
           for row in report["sources"]), report
' || fail "sources --json shape"
  if bash "$am" --global sources >/dev/null 2>&1; then fail "sources accepted --global"; fi
}

test_memory_hygiene_report_is_read_only() {
  local repo="$TMP/memory-hygiene" fresh="$TMP/memory-hygiene-fresh" am="$ROOT/scripts/agent-memory.sh"
  local home="$TMP/memory-hygiene-home" key out before after
  make_repo "$repo"
  make_repo "$fresh"
  mkdir -p "$home"
  printf '# Rules\n\n- Always run the focused heron test\n  before pushing parser changes.\n' > "$repo/AGENTS.md"
  printf '@AGENTS.md\n' > "$repo/CLAUDE.md"
  bash "$am" --repo "$repo" append --text "cold ibis note nobody asks about" >/dev/null
  bash "$am" --repo "$repo" append --text "warm egret note someone recalls" >/dev/null
  bash "$am" --repo "$repo" append --text "Always run the focused heron test before pushing parser changes." >/dev/null
  bash "$am" --repo "$repo" recall "egret" >/dev/null
  # Back-date the log: the report only calls a note cold after --cold-days.
  python3 - "$repo/.oms/memory/shared.md" <<'PY'
import re, sys
path = sys.argv[1]
text = open(path, encoding="utf-8").read()
open(path, "w", encoding="utf-8").write(re.sub(r"^## 20\d\d-\d\d-\d\dT", "## 2026-01-01T", text, flags=re.M))
PY
  key="$(printf '%s' "$(cd "$repo" && pwd -P)" | tr -c 'A-Za-z0-9' '-')"
  mkdir -p "$home/.claude/projects/$key/memory"
  seq 1 230 > "$home/.claude/projects/$key/memory/MEMORY.md"

  before="$(cksum "$repo"/.oms/memory/* | sort)"
  out="$(HOME="$home" CODEX_HOME="" bash "$am" --repo "$repo" hygiene)" || fail "hygiene failed: $out"
  after="$(cksum "$repo"/.oms/memory/* | sort)"
  [ "$before" = "$after" ] || fail "hygiene changed the memory store"
  [[ "$out" == *"cold ibis note"* ]] || fail "never-recalled old note not listed: $out"
  [[ "$out" != *"warm egret"* ]] || fail "a recalled note was called cold: $out"
  [[ "$out" == *"exact in AGENTS.md"* && "$out" == *"focused heron test"* ]] || fail "wrapped duplicate of AGENTS.md not found: $out"
  [[ "$out" == *"230 lines"*"-> over"*"30 line(s)"* && "$out" == *"code.claude.com/docs/en/memory"* ]] || fail "index cap warning missing: $out"
  out="$(HOME="$home" CODEX_HOME="" OMS_MEMORY_INDEX_CAP_LINES=0 bash "$am" --repo "$repo" hygiene)" || fail "hygiene with cap off failed"
  [[ "$out" == *"-> ok"* && "$out" == *"overridden"* ]] || fail "cap override ignored: $out"
  if HOME="$home" OMS_MEMORY_INDEX_CAP_LINES=many bash "$am" --repo "$repo" hygiene >/dev/null 2>&1; then fail "bad cap accepted"; fi
  HOME="$home" CODEX_HOME="" bash "$am" --repo "$repo" hygiene --json | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert report["schema"] == 1 and report["read_only"] is True, report
assert [row["reason"] for row in report["cold"]["items"]] == ["never", "never"], report
assert report["duplicated"]["items"][0]["match"] == "exact", report
assert report["index_caps"][0]["status"] == "over" and report["index_caps"][0]["lines_not_loaded"] == 30, report
' || fail "hygiene --json shape"
  # No index yet: cold notes are not assessed, and the report does not create one.
  bash "$am" --repo "$fresh" append --text "lonely note in a fresh repository" >/dev/null
  rm -f "$fresh"/.oms/memory/*.sqlite3*
  out="$(HOME="$home" CODEX_HOME="" bash "$am" --repo "$fresh" hygiene)" || fail "hygiene without an index failed"
  [[ "$out" == *"cold notes: not assessed"* ]] || fail "missing index not reported: $out"
  ! ls "$fresh"/.oms/memory/*.sqlite3 >/dev/null 2>&1 || fail "hygiene created the index"
  if bash "$am" --global hygiene >/dev/null 2>&1; then fail "hygiene accepted --global"; fi
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
  git -C "$repo" diff --cached -- file.txt | grep -F 'staged-a' > /dev/null ||
    fail "checkpoint did not restore the index state"
  git -C "$repo" diff -- b.txt | grep -F 'unstaged-b' > /dev/null ||
    fail "checkpoint did not restore unstaged state"
  [ "$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" list --json | wc -l | tr -d ' ')" -ge 2 ] ||
    fail "restore did not create a recovery checkpoint"

  git -C "$repo" add file.txt b.txt
  git -C "$repo" commit -qm new-head
  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) \
    >/dev/null 2>&1; then
    fail "checkpoint restore must refuse a different HEAD"
  fi

  # --untracked copies the regular untracked files; a restore writes back only
  # the ones that are missing and never replaces a file that exists.
  local cp="$ROOT/scripts/checkpoint.sh" kept reused report rc
  mkdir -p "$repo/notes/deep" "$TMP/checkpoint-outside"
  printf 'note-one\n' > "$repo/notes/deep/n.txt"
  printf 'scratch-old\n' > "$repo/scratch.txt"
  ln -s "$TMP/checkpoint-outside" "$repo/link-out"
  kept="$(cd "$repo" && bash "$cp" create --untracked --label keep --json)" || fail "create --untracked failed"
  checkpoint_id="$(OMS_TEST_REPORT="$kept" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])')"
  OMS_TEST_REPORT="$kept" python3 -c '
import json, os
meta = json.loads(os.environ["OMS_TEST_REPORT"])
assert meta["untracked_captured"] == 3 and meta["untracked_omitted"] == 1, meta  # local.txt from above too
assert len(meta["untracked_sha256"]) == 64 and len(meta["untracked_fingerprint"]) == 64, meta
' || fail "create --untracked must count captured files and the symlink it left out: $kept"
  [ -f "$repo/notes/deep/n.txt" ] && [ -L "$repo/link-out" ] || fail "create --untracked must not touch the worktree"
  reused="$(cd "$repo" && bash "$cp" create --untracked --reuse --label keep)" || fail "create --reuse failed"
  printf '%s\n' "$reused" | grep -Fq "checkpoint: reused $checkpoint_id" ||
    fail "--reuse must return the identical checkpoint: $reused"
  printf 'more\n' >> "$repo/notes/deep/n.txt"
  reused="$(cd "$repo" && bash "$cp" create --untracked --reuse --label keep)" || fail "create --reuse failed after a change"
  printf '%s\n' "$reused" | grep -Fq "checkpoint: created " ||
    fail "--reuse must create a new checkpoint once an untracked file changed: $reused"
  printf 'note-one\n' > "$repo/notes/deep/n.txt"
  rc=0; (cd "$repo" && bash "$cp" list --untracked) >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || fail "--untracked outside create must be refused (rc=$rc)"

  rm -rf "$repo/notes"
  printf 'scratch-newer\n' > "$repo/scratch.txt"
  git -C "$repo" commit -q --allow-empty -m moved-head
  (cd "$repo" && bash "$cp" restore "$checkpoint_id" --untracked-only) > "$TMP/checkpoint-untracked-dry" ||
    fail "restore --untracked-only dry-run failed"
  grep -Fq "1 captured untracked file(s) are missing and would be written back; 2 already present" \
    "$TMP/checkpoint-untracked-dry" || fail "the dry-run counts missing and present files: $(cat "$TMP/checkpoint-untracked-dry")"
  [ ! -e "$repo/notes" ] || fail "restore --untracked-only wrote without --apply"
  report="$(cd "$repo" && bash "$cp" restore "$checkpoint_id" --untracked-only --apply --json)" ||
    fail "restore --untracked-only --apply failed on a later HEAD"
  OMS_TEST_REPORT="$report" python3 -c '
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])["untracked"]
assert (r["written"], r["present"], r["refused"]) == (1, 2, 0), r
' || fail "restore --untracked-only reports what it wrote: $report"
  [ "$(cat "$repo/notes/deep/n.txt")" = note-one ] || fail "the missing untracked file was not written back"
  [ "$(cat "$repo/scratch.txt")" = scratch-newer ] || fail "restore must never replace an untracked file that exists"
  [ -z "$(ls -A "$TMP/checkpoint-outside")" ] || fail "restore wrote through a symlink"
  # A tampered archive is refused, and a member that escapes the repository is never written.
  rm -rf "$repo/notes"
  printf 'x' >> "$repo/.oms/checkpoints/$checkpoint_id/untracked.tar"
  if (cd "$repo" && bash "$cp" restore "$checkpoint_id" --untracked-only --apply) >/dev/null 2>&1; then
    fail "a checkpoint whose untracked archive changed must be refused"
  fi
  [ ! -e "$repo/notes" ] || fail "a refused restore must write nothing"
  python3 - "$TMP/checkpoint-evil.tar" <<'PY'
import io, sys, tarfile
with tarfile.open(sys.argv[1], "w") as archive:
    for name in ("../escaped.txt", ".git/hooks/pre-commit", "ok/inside.txt"):
        member = tarfile.TarInfo(name)
        member.size = 2
        archive.addfile(member, io.BytesIO(b"x\n"))
PY
  report="$(python3 "$ROOT/scripts/lib/checkpoint_untracked.py" restore "$repo" "$TMP/checkpoint-evil.tar" --apply)" ||
    fail "the untracked restore helper failed on a hostile archive"
  OMS_TEST_REPORT="$report" python3 -c '
import json, os
r = json.loads(os.environ["OMS_TEST_REPORT"])
assert (r["written"], r["refused"]) == (1, 2), r
' || fail "members outside the repository or under .git are refused: $report"
  [ ! -e "$TMP/escaped.txt" ] && [ ! -e "$repo/.git/hooks/pre-commit" ] && [ -f "$repo/ok/inside.txt" ] ||
    fail "a hostile archive wrote outside its bounds"
}

checkpoint_ids() {
  local checkpoint_root="$1" checkpoint_path
  for checkpoint_path in "$checkpoint_root"/cp-*; do
    [ -e "$checkpoint_path" ] || continue
    printf '%s\n' "${checkpoint_path##*/}"
  done
}

# Each diff option is exercised in its own repository so one fix cannot mask the
# other. Verify and dry-run, then restore after both staged and unstaged content
# have diverged from the checkpoint.
checkpoint_config_roundtrip() {
  local repo="$1" config_key="$2" config_value="$3"
  local created checkpoint_id before_status

  make_repo "$repo"
  git -C "$repo" config core.autocrlf false
  printf 'b-base\n' > "$repo/b.txt"
  git -C "$repo" add b.txt
  git -C "$repo" commit -qm add-b
  printf 'staged-checkpoint\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  printf 'mixed-unstaged-checkpoint\n' > "$repo/file.txt"
  printf 'unstaged-checkpoint\n' > "$repo/b.txt"
  : > "$repo/new-empty.txt"
  printf 'new staged content\n' > "$repo/new-added.txt"
  git -C "$repo" add new-empty.txt new-added.txt
  git -C "$repo" config "$config_key" "$config_value"

  created="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json)" ||
    fail "checkpoint creation failed with $config_key=$config_value"
  checkpoint_id="$(OMS_TEST_REPORT="$created" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])' | tr -d '\r')"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" verify "$checkpoint_id") >/dev/null ||
    fail "checkpoint patch format depended on $config_key=$config_value"

  before_status="$(git -C "$repo" status --porcelain=v1)"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id") >/dev/null ||
    fail "checkpoint dry-run failed with $config_key=$config_value"
  [ "$before_status" = "$(git -C "$repo" status --porcelain=v1)" ] ||
    fail "checkpoint dry-run changed status with $config_key=$config_value"

  printf 'later-staged\n' > "$repo/file.txt"
  # Force stale stat data without sleeping: undoing unstaged bytes must refresh
  # the index before the next --index apply checks worktree/index agreement.
  python3 - "$repo/file.txt" <<'PYTIME'
import os, sys
os.utime(sys.argv[1], (946684800, 946684800))
PYTIME
  git -C "$repo" add file.txt
  printf 'mixed-later-unstaged\n' > "$repo/file.txt"
  printf 'later-unstaged\n' > "$repo/b.txt"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) >/dev/null ||
    fail "checkpoint apply failed with $config_key=$config_value"
  python3 - "$repo" <<'PY' || fail "checkpoint changed index or worktree bytes"
import pathlib, subprocess, sys
repo = pathlib.Path(sys.argv[1])
for name, staged, working in (
    ("file.txt", b"staged-checkpoint\n", b"mixed-unstaged-checkpoint\n"),
    ("b.txt", b"b-base\n", b"unstaged-checkpoint\n"),
    ("new-empty.txt", b"", b""),
    ("new-added.txt", b"new staged content\n", b"new staged content\n"),
):
    assert (repo / name).read_bytes() == working, name
    assert subprocess.check_output(["git", "-C", str(repo), "show", ":" + name]) == staged, name
PY
  git -C "$repo" ls-files --error-unmatch new-empty.txt new-added.txt >/dev/null ||
    fail "checkpoint lost ordinary staged additions"
  if [ ! -f "$repo/new-empty.txt" ] || [ -s "$repo/new-empty.txt" ]; then
    fail "checkpoint lost a staged empty file"
  fi
  git -C "$repo" diff --cached --name-only -- new-empty.txt | grep -Fx new-empty.txt > /dev/null ||
    fail "checkpoint converted an ordinary staged empty file to intent-to-add"
}

test_checkpoint_restores_from_unstaged_only_with_stale_stat() {
  local repo="$TMP/checkpoint-unstaged-stat" created checkpoint_id
  make_repo "$repo"
  git -C "$repo" config core.autocrlf false
  printf 'target-staged\n' > "$repo/file.txt"
  git -C "$repo" add file.txt
  created="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json)" ||
    fail "checkpoint creation failed for unstaged-only restore"
  checkpoint_id="$(OMS_TEST_REPORT="$created" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])' | tr -d '\r')"
  git -C "$repo" show HEAD:file.txt > "$repo/file.txt"
  python3 - "$repo/file.txt" <<'PYTIME'
import os, sys
os.utime(sys.argv[1], (946684800, 946684800))
PYTIME
  git -C "$repo" add file.txt
  git -C "$repo" diff --cached --quiet || fail "fixture must have no staged changes"
  printf 'later-unstaged\n' > "$repo/file.txt"
  (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) >/dev/null ||
    fail "checkpoint could not apply target index after unstaged-only state"
  python3 - "$repo" <<'PYBYTES' || fail "unstaged-only restore changed target bytes"
import pathlib, subprocess, sys
repo = pathlib.Path(sys.argv[1])
assert (repo / "file.txt").read_bytes() == b"target-staged\n"
assert subprocess.check_output(["git", "-C", str(repo), "show", ":file.txt"]) == b"target-staged\n"
PYBYTES
}

test_checkpoint_ignores_diff_prefix_and_color_config() {
  checkpoint_config_roundtrip "$TMP/checkpoint-noprefix" diff.noprefix true
  checkpoint_config_roundtrip "$TMP/checkpoint-color" color.ui always
}

# Intent-to-add is not represented by the current two-patch checkpoint format.
# Reject it before creating checkpoint files or mutating tracked state.
test_checkpoint_rejects_intent_to_add_without_mutation() {
  local repo variant
  local before_status before_index before_worktree checkpoint_artifacts

  for variant in content empty missing; do
    repo="$TMP/checkpoint-ita-create-$variant"
    make_repo "$repo"
    printf 'intent content\n' > "$repo/new.txt"
    [ "$variant" != empty ] || : > "$repo/new.txt"
    git -C "$repo" add -N new.txt
    [ "$variant" != missing ] || rm "$repo/new.txt"
    before_status="$(git -C "$repo" status --porcelain=v1)"
    before_index="$(git -C "$repo" ls-files --stage)"
    before_worktree="$(git -C "$repo" hash-object new.txt 2>/dev/null || printf missing)"

    if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json) >/dev/null 2>&1; then
      fail "checkpoint creation accepted an intent-to-add entry"
    fi
    [ "$before_status" = "$(git -C "$repo" status --porcelain=v1)" ] ||
      fail "intent-to-add create refusal changed status"
    [ "$before_index" = "$(git -C "$repo" ls-files --stage)" ] ||
      fail "intent-to-add create refusal changed the index"
    [ "$before_worktree" = "$(git -C "$repo" hash-object new.txt 2>/dev/null || printf missing)" ] ||
      fail "intent-to-add create refusal changed file content"
    [ "$variant" != missing ] || [ ! -e "$repo/new.txt" ] ||
      fail "intent-to-add create refusal recreated a missing file"
    checkpoint_artifacts="$(checkpoint_ids "$repo/.oms/checkpoints")"
    [ -z "$checkpoint_artifacts" ] ||
      fail "intent-to-add create refusal left checkpoint artifacts"
  done
}

# Also reject a current intent-to-add entry before restore creates its mandatory
# backup or attempts reverse/apply operations; preserve the complete user state.
test_checkpoint_restore_refuses_current_intent_to_add_without_mutation() {
  local repo="$TMP/checkpoint-ita-restore"
  local created checkpoint_id before_status before_index before_worktree before_checkpoints

  make_repo "$repo"
  created="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json)" ||
    fail "clean checkpoint creation failed"
  checkpoint_id="$(OMS_TEST_REPORT="$created" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])' | tr -d '\r')"
  printf 'current intent content\n' > "$repo/new.txt"
  git -C "$repo" add -N new.txt
  before_status="$(git -C "$repo" status --porcelain=v1)"
  before_index="$(git -C "$repo" ls-files --stage)"
  before_worktree="$(git -C "$repo" hash-object new.txt)"
  before_checkpoints="$(checkpoint_ids "$repo/.oms/checkpoints")"

  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id") >/dev/null 2>&1; then
    fail "checkpoint dry-run accepted a current intent-to-add entry"
  fi
  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) >/dev/null 2>&1; then
    fail "checkpoint restore accepted a current intent-to-add entry"
  fi
  [ "$before_status" = "$(git -C "$repo" status --porcelain=v1)" ] ||
    fail "intent-to-add restore refusal changed status"
  [ "$before_index" = "$(git -C "$repo" ls-files --stage)" ] ||
    fail "intent-to-add restore refusal changed the index"
  [ "$before_worktree" = "$(git -C "$repo" hash-object new.txt)" ] ||
    fail "intent-to-add restore refusal changed file content"
  [ "$before_checkpoints" = "$(checkpoint_ids "$repo/.oms/checkpoints")" ] ||
    fail "intent-to-add restore refusal created a backup checkpoint"
}

# Reproduce a schema-1 checkpoint made while new.txt had intent-to-add state.
# Git accepted this old binary patch in verify, but restore later can lose the
# current staged file and fail its automatic rollback. Reject before mutation.
test_checkpoint_rejects_legacy_intent_to_add_snapshot_without_mutation() {
  local repo="$TMP/checkpoint-ita-legacy"
  local created checkpoint_id before_status before_index before_worktree before_checkpoints

  make_repo "$repo"
  created="$(cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" create --json)" ||
    fail "clean checkpoint creation failed for legacy fixture"
  checkpoint_id="$(OMS_TEST_REPORT="$created" python3 -c 'import json,os; print(json.loads(os.environ["OMS_TEST_REPORT"])["id"])' | tr -d '\r')"

  # Manufacture only in this temporary fixture: capture the old ITA diff and
  # update schema-1 integrity metadata exactly as a valid historical artifact.
  printf 'legacy intent snapshot\n' > "$repo/new.txt"
  git -C "$repo" add -N new.txt
  git -C "$repo" diff --binary --full-index --no-ext-diff --no-textconv -- \
    > "$repo/.oms/checkpoints/$checkpoint_id/worktree.patch"
  python3 - "$repo/.oms/checkpoints/$checkpoint_id" <<'PY' ||
import hashlib, json, os, sys
root = sys.argv[1]
patch = os.path.join(root, "worktree.patch")
with open(patch, "rb") as handle:
    data = handle.read()
meta_path = os.path.join(root, "meta.json")
with open(meta_path, encoding="utf-8") as handle:
    meta = json.load(handle)
meta["worktree_sha256"] = hashlib.sha256(data).hexdigest()
meta["unstaged_bytes"] = len(data)
with open(meta_path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(meta, handle, ensure_ascii=False, sort_keys=True)
    handle.write("\n")
PY
    fail "could not build a checksummed legacy fixture"
  printf 'later normal staged content\n' > "$repo/new.txt"
  git -C "$repo" add new.txt
  before_status="$(git -C "$repo" status --porcelain=v1)"
  before_index="$(git -C "$repo" ls-files --stage)"
  before_worktree="$(git -C "$repo" hash-object new.txt)"
  before_checkpoints="$(checkpoint_ids "$repo/.oms/checkpoints")"

  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" verify "$checkpoint_id") >/dev/null 2>&1; then
    fail "verify accepted a legacy intent-to-add checkpoint"
  fi
  [ "$before_status" = "$(git -C "$repo" status --porcelain=v1)" ] ||
    fail "legacy checkpoint verify changed status"
  [ "$before_index" = "$(git -C "$repo" ls-files --stage)" ] ||
    fail "legacy checkpoint verify changed the index"
  [ "$before_worktree" = "$(git -C "$repo" hash-object new.txt)" ] ||
    fail "legacy checkpoint verify changed file content"

  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id") >/dev/null 2>&1; then
    fail "dry-run accepted a legacy intent-to-add checkpoint"
  fi
  if (cd "$repo" && bash "$ROOT/scripts/checkpoint.sh" restore "$checkpoint_id" --apply) >/dev/null 2>&1; then
    fail "restore applied a legacy intent-to-add checkpoint"
  fi
  [ "$before_status" = "$(git -C "$repo" status --porcelain=v1)" ] ||
    fail "legacy checkpoint restore refusal changed status"
  [ "$before_index" = "$(git -C "$repo" ls-files --stage)" ] ||
    fail "legacy checkpoint restore refusal changed the index"
  [ "$before_worktree" = "$(git -C "$repo" hash-object new.txt)" ] ||
    fail "legacy checkpoint restore refusal changed file content"
  [ "$before_checkpoints" = "$(checkpoint_ids "$repo/.oms/checkpoints")" ] ||
    fail "legacy checkpoint restore refusal created a backup checkpoint"
}

test_hook_installers_keep_telemetry_off_the_tool_hot_path() {
  local settings="$TMP/claude-settings.json"
  local home_dir="$TMP/claude-home"

  mkdir -p "$home_dir"
  python3 - "$ROOT" "$settings" <<'PYTEST'
import json, sys
from pathlib import Path
root, settings = map(Path, sys.argv[1:])
old = "panel-" + "direct-work-hook.sh"
command = 'bash "' + str(root / "scripts" / old) + '"'
settings.write_text(json.dumps({"hooks": {"PostToolUse": [{"matcher": "Bash", "hooks": [
    {"type": "command", "command": command}, {"type": "command", "command": "user-hook"}]}]}}))
PYTEST
  HOME="$home_dir" OMS_CLAUDE_SETTINGS="$settings" \
    bash "$ROOT/scripts/install-claude-hooks.sh" >/dev/null
  python3 - "$settings" <<'PY' || fail "Claude telemetry hooks were not installed"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
commands = [h.get("command", "") for entries in hooks.values() for entry in entries for h in entry.get("hooks", [])]
assert not any(("panel-" + "direct-work-hook.sh") in command for command in commands), commands
assert "user-hook" in commands, commands
for event in ("SessionStart", "SessionEnd"):
    commands = [h.get("command", "") for entry in hooks.get(event, []) for h in entry.get("hooks", [])]
    assert any("telemetry-hook.sh" in command for command in commands), (event, commands)
for event in ("PostToolUse", "SubagentStop"):
    commands = [h.get("command", "") for entry in hooks.get(event, []) for h in entry.get("hooks", [])]
    assert not any("telemetry-hook.sh" in command for command in commands), (event, commands)
PY
  python3 - "$ROOT/plugins/oh-my-setting/hooks.json" <<'PY' || fail "Codex telemetry hooks use the wrong events"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
for event in ("SessionStart", "SessionEnd"):
    commands = [h.get("command", "") for entry in hooks.get(event, []) for h in entry.get("hooks", [])]
    assert any("telemetry-hook" in command for command in commands), (event, commands)
for event in ("PostToolUse", "SubagentStop"):
    commands = [h.get("command", "") for entry in hooks.get(event, []) for h in entry.get("hooks", [])]
    assert not any("telemetry-hook" in command for command in commands), (event, commands)
expected = {
    "UserPromptSubmit": [("", "skill-router")],
    "Stop": [("", "turn-guard")],
    "PreCompact": [("", "precompact-handoff codex")],
    "SessionStart": [("", "resume-hook"), ("", "telemetry-hook")],
    "PostToolUse": [("apply_patch", "syntax-guard-hook")],
    "SessionEnd": [("", "telemetry-hook")],
}
actual = {event: [(entry.get("matcher", ""), h.get("command", "").split('harness-hook.sh" ', 1)[-1])
                  for entry in entries for h in entry.get("hooks", [])]
          for event, entries in hooks.items()}
assert actual == expected, actual
PY
}

test_gc_bounds_old_checkpoint_and_hook_state() {
  local repo="$TMP/retention"
  local checkpoint_id out landing_dir landing_lock_mode retained_name

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
  landing_dir="$repo/.oms/landing-patches"
  mkdir -p "$landing_dir" "$repo/.oms/artifacts"
  printf 'terminal\n' > "$landing_dir/land-terminal.patch"
  printf 'live\n' > "$landing_dir/land-live.patch"
  printf 'referenced\n' > "$landing_dir/land-referenced.patch"
  cat > "$repo/.oms/landings.jsonl" <<EOF
{"schema":1,"ts":"2020-01-01T00:00:00Z","landing_id":"land-terminal","event":"intent","patch":"$landing_dir/land-terminal.patch"}
{"schema":1,"ts":"2020-01-01T00:00:01Z","landing_id":"land-terminal","event":"complete"}
{"schema":1,"ts":"2020-01-01T00:00:00Z","landing_id":"land-live","event":"intent","patch":"$landing_dir/land-live.patch"}
{"schema":1,"ts":"2020-01-01T00:00:00Z","landing_id":"land-referenced","event":"intent","patch":"$landing_dir/land-referenced.patch"}
{"schema":1,"ts":"2020-01-01T00:00:01Z","landing_id":"land-referenced","event":"abandoned"}
EOF
  printf '%s\n' '{"patch":".oms/landing-patches/land-referenced.patch"}' \
    > "$repo/.oms/artifacts/index.jsonl"
  python3 - "$repo/.oms/checkpoints/$checkpoint_id/meta.json" \
    "$repo/.oms/hooks/sessions/old.json" "$landing_dir/land-terminal.patch" \
    "$landing_dir/land-live.patch" "$landing_dir/land-referenced.patch" <<'PY'
import json, os, sys, time
meta_path, session_path, *landing_paths = sys.argv[1:]
with open(meta_path, encoding="utf-8") as handle:
    meta = json.load(handle)
meta["created_at"] = "2020-01-01T00:00:00Z"
with open(meta_path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(meta, handle, sort_keys=True)
    handle.write("\n")
old = time.time() - 10 * 86400
os.utime(session_path, (old, old))
for path in landing_paths:
    os.utime(path, (old, old))
PY

  out="$(bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --dry-run)"
  printf '%s' "$out" | grep >/dev/null -F 'checkpoint:' ||
    fail "gc did not report the old local checkpoint"
  printf '%s' "$out" | grep >/dev/null -F 'hook-events: compact 3 -> 2 rows' ||
    fail "gc did not report old hook-event compaction"
  printf '%s' "$out" | grep >/dev/null -F 'hook-session:' ||
    fail "gc did not report the old hook session"
  printf '%s' "$out" | grep >/dev/null -F 'landing-patch:' ||
    fail "gc did not report an unreferenced terminal landing snapshot"
  [ -d "$repo/.oms/checkpoints/$checkpoint_id" ] ||
    fail "gc dry-run removed a checkpoint"

  bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --apply >/dev/null
  [ ! -e "$repo/.oms/checkpoints/$checkpoint_id" ] ||
    fail "gc apply kept an expired checkpoint"
  [ ! -e "$repo/.oms/hooks/sessions/old.json" ] ||
    fail "gc apply kept an expired hook session"
  [ -e "$landing_dir/land-terminal.patch" ] ||
    fail "gc trusted terminal hints to delete frozen evidence"
  [ -e "$landing_dir/land-live.patch" ] ||
    fail "gc removed a snapshot whose landing is still recoverable"
  [ -e "$landing_dir/land-referenced.patch" ] ||
    fail "gc removed a terminal snapshot still referenced by the artifact index"
  grep -Fq '"session":"new"' "$repo/.oms/hooks/events.jsonl" ||
    fail "gc removed the current hook event"
  grep -Fq 'not-json-is-preserved' "$repo/.oms/hooks/events.jsonl" ||
    fail "gc removed an unparseable diagnostic hook row"
  if grep -Fq '"session":"old"' "$repo/.oms/hooks/events.jsonl"; then
    fail "gc kept the expired hook event"
  fi

  # A filename/terminal hint is not native ownership, and a matching digest
  # cannot distinguish a replacement generation. No-intent leftovers also stay.
  python3 - "$repo" <<'PY'
import json, os, pathlib, sys, time
repo = pathlib.Path(sys.argv[1])
root = repo / ".oms/landing-patches"
old = time.time() - 10 * 86400
with (repo / ".oms/landings.jsonl").open("a") as journal:
    for name in ("forged", "replacement", "orphan"):
        path = root / ("land-" + name + ".patch")
        path.write_bytes(b"retained evidence\n")
        if name != "orphan":
            journal.write(json.dumps({"landing_id": "land-" + name, "event": "intent", "patch": str(path)}) + "\n")
            journal.write(json.dumps({"landing_id": "land-" + name, "event": "abandoned", "reason": "unadmitted"}) + "\n")
        if name == "replacement":
            inode = path.stat().st_ino
            replacement = root / "replacement.tmp"
            replacement.write_bytes(path.read_bytes())
            replacement.replace(path)
            assert path.stat().st_ino != inode
        os.utime(path, (old, old))
PY
  for landing_lock_mode in 0 1; do
    # flock files and mkdir locks cannot share a pathname across backends.
    OMS_LOCK_DIR="$TMP/landing-locks-$landing_lock_mode" OMS_LOCK_TIMEOUT=5 \
      OMS_LOCK_FORCE_MKDIR="$landing_lock_mode" bash "$ROOT/scripts/gc.sh" \
      --repo "$repo" --days 1 --apply >/dev/null
    for retained_name in forged replacement orphan; do
      grep -Fxq 'retained evidence' "$landing_dir/land-$retained_name.patch" ||
        fail "gc deleted unproven $retained_name evidence (lock mode $landing_lock_mode)"
    done
  done

  # Frozen landing retention is a safety decision. One malformed landing or
  # artifact row makes the reference set unknowable, so apply mode must keep
  # every candidate and fail instead of silently deleting evidence.
  printf 'corrupt-input-guard\n' > "$landing_dir/land-corrupt.patch"
  python3 - "$landing_dir/land-corrupt.patch" <<'PY'
import os, sys, time
old = time.time() - 10 * 86400
os.utime(sys.argv[1], (old, old))
PY
  cat >> "$repo/.oms/landings.jsonl" <<EOF
{"schema":1,"ts":"2020-01-01T00:00:00Z","landing_id":"land-corrupt","event":"intent","patch":"$landing_dir/land-corrupt.patch"}
{"schema":1,"ts":"2020-01-01T00:00:01Z","landing_id":"land-corrupt","event":"complete"}
not-json
EOF
  if bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --apply \
    >"$TMP/gc-corrupt.out" 2>&1; then
    fail "gc accepted malformed landing retention input"
  fi
  [ -e "$landing_dir/land-corrupt.patch" ] ||
    fail "gc deleted frozen evidence after malformed landing input"

  sed '$d' "$repo/.oms/landings.jsonl" > "$TMP/landings-valid.jsonl"
  mv "$TMP/landings-valid.jsonl" "$repo/.oms/landings.jsonl"
  printf 'corrupt-artifact-guard\n' > "$landing_dir/land-corrupt-index.patch"
  cat >> "$repo/.oms/landings.jsonl" <<EOF
{"schema":1,"ts":"2020-01-01T00:00:00Z","landing_id":"land-corrupt-index","event":"intent","patch":"$landing_dir/land-corrupt-index.patch"}
{"schema":1,"ts":"2020-01-01T00:00:01Z","landing_id":"land-corrupt-index","event":"complete"}
EOF
  printf 'not-json\n' > "$repo/.oms/artifacts/index.jsonl"
  python3 - "$landing_dir/land-corrupt-index.patch" <<'PY'
import os, sys, time
old = time.time() - 10 * 86400
os.utime(sys.argv[1], (old, old))
PY
  if bash "$ROOT/scripts/gc.sh" --repo "$repo" --days 1 --apply \
    >"$TMP/gc-corrupt-index.out" 2>&1; then
    fail "gc accepted malformed artifact retention input"
  fi
  [ -e "$landing_dir/land-corrupt-index.patch" ] ||
    fail "gc deleted frozen evidence after malformed artifact input"
}

# These are real native writer generations; the preceding retention fixture
# deliberately contains only legacy/forged rows and must continue preserving all.
test_managed_landing_capture_retention() {
  make_repo "$TMP/managed-capture/repo"
  python3 - "$ROOT" "$TMP/managed-capture" <<'PY' || fail "managed landing capture retention"
import hashlib, json, os, pathlib, runpy, shutil, subprocess, sys, tempfile, time
root, sandbox = map(pathlib.Path, sys.argv[1:])
sandbox.mkdir(exist_ok=True)
tempfile.tempdir = str(sandbox)
h = runpy.run_path(str(root / 'scripts/lib/landing-capture.py'))
repo = sandbox / 'repo'
(repo / '.oms/artifacts').mkdir(parents=True)
(repo / '.oms/plan/completions').mkdir(parents=True)
source = sandbox / 'input.patch'
source.write_bytes(b'bounded payload\n')
sha = hashlib.sha256(source.read_bytes()).hexdigest()

def new(name, publish=True, target=None):
    capture = pathlib.Path(h['allocate'](str(repo), str(source), 'land-' + name, sha))
    patch = target or repo / '.oms/landing-patches' / ('land-' + name + '.patch')
    if publish:
        h['publish'](str(repo), str(capture), str(patch))
        # The owner must seal the exact canonical intent and terminal receipt,
        # not an unrelated shared terminal hint for the same basename.
        fields = dict(landing_id='land-' + name, patch=str(patch), patch_sha=sha,
                      base_sha='fixture-base', task='', lease='', plan_receipt_sha='',
                      plan_done_receipt_sha='', approval='', approval_version='')
        receipt = hashlib.sha256(json.dumps(dict(schema=1, **fields), sort_keys=True,
                                            separators=(',', ':')).encode()).hexdigest()
        for event in ('intent', 'complete'):
            row = dict(schema=1, event=event, receipt_sha=receipt, ts='2020-01-01T00:00:00Z', **fields)
            h['D']['append'](str(repo / '.oms/landings.jsonl'), h['encoded'](row), 'fixture native receipt')
    h['seal'](str(repo), 'land-' + name, 'complete' if publish else 'before-admission')
    return capture, patch

def collect(apply=True):
    h['collect'](str(repo), 0, apply)

capture, patch = new('pin')
assert capture.stat().st_nlink == patch.stat().st_nlink == 2
assert h['read_patch'](str(repo), str(patch)) == source.read_bytes()
try:
    h['D']['read_no_follow'](str(repo), str(patch), 'ordinary reader')
except (ValueError, SystemExit):
    pass
else:
    raise AssertionError('ordinary durable reader accepted hardlinked evidence')
receipt = repo / '.oms/plan/completions/pin.json'
receipt.write_text(json.dumps({'artifact': {'path': str(patch)}}))
collect()
assert capture.exists() and patch.exists(), 'completion reference was not pinned'
public_gc = subprocess.run(['bash', str(root / 'scripts/gc.sh'), '--repo', str(repo), '--days', '0', '--apply'],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert public_gc.returncode == 0, (public_gc.stdout.decode(), public_gc.stderr.decode())
assert capture.exists() and patch.exists(), 'public GC lost completion pin'
receipt.unlink()
before = (repo / '.oms/landings.jsonl').read_bytes()
collect(False)
assert patch.exists() and (repo / '.oms/landings.jsonl').read_bytes() == before
for backend in ('0', '1'):
    native_capture, native_patch = new('backend-' + backend)
    result = subprocess.run(['bash', str(root / 'scripts/gc.sh'), '--repo', str(repo), '--days', '0', '--apply'],
                            env=dict(os.environ, OMS_LOCK_FORCE_MKDIR=backend,
                                     OMS_LOCK_DIR=str(sandbox / ('locks-' + backend))),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert result.returncode == 0, (result.stdout.decode(), result.stderr.decode())
    assert not native_capture.exists() and not native_patch.exists(), 'public GC did not reclaim native generation'
assert not capture.exists() and not patch.exists() and h['usage'](str(repo)) == (0, 0)
validated = subprocess.run(['bash', str(root / 'scripts/run.sh'), 'validate', '--dir', str(repo / '.oms')],
                           cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert validated.returncode == 0, (validated.stdout.decode(), validated.stderr.decode())
assert any(row['event'] == 'capture-release-ready' for row in h['rows'](str(repo)))

# Private ADMIT-failure inputs consume the same budget and are independently
# reclaimable only when no persistent report/index references their exact input.
capture, _ = new('rejected', False)
report = repo / '.oms/artifacts/rejected.md'
report.write_text('- patch: ' + str(capture))
collect()
assert capture.exists()
report.unlink()
collect()
assert not capture.exists()

# Quota refuses before creating another payload, including unknown/crashed
# allocations whose writer never produced a terminal ownership seal.
unknown = pathlib.Path(h['allocate'](str(repo), str(source), 'land-unknown', sha))
before_retry = (repo / '.oms/landings.jsonl').read_bytes()
try:
    h['allocate'](str(repo), str(source), 'land-unknown', sha)
except ValueError as error:
    assert 'already reserved' in str(error)
else:
    raise AssertionError('retry duplicated the same native reservation')
assert (repo / '.oms/landings.jsonl').read_bytes() == before_retry
os.environ['OMS_LANDING_CAPTURE_COUNT'] = '1'
old = (repo / '.oms/landings.jsonl').read_bytes()
try:
    h['allocate'](str(repo), str(source), 'land-overflow', sha)
except ValueError as error:
    msg = str(error)
    assert 'quota exhausted' in msg and 'count 1/1' in msg and 'oms gc' in msg and 'OMS_LANDING_CAPTURE_COUNT' in msg, msg
else:
    raise AssertionError('count quota bypassed')
os.environ['OMS_LANDING_CAPTURE_COUNT'] = '0'
try:
    h['allocate'](str(repo), str(source), 'land-overflow-zero', sha)
except ValueError as error:
    assert 'invalid OMS_LANDING_CAPTURE_COUNT' in str(error), str(error)
else:
    raise AssertionError('zero count quota limit accepted')
assert (repo / '.oms/landings.jsonl').read_bytes() == old
collect()
assert unknown.exists()
os.environ.pop('OMS_LANDING_CAPTURE_COUNT')

# A reference namespace that cannot be traversed makes the pin set unknown.
# Inject the walk error in-process: chmod(000) is unreliable when the suite
# runs as root, while the onerror callback is the exact OS-walk failure path.
unknown_namespace = repo / '.oms/tasks/inaccessible/unknown.json'
unknown_namespace.parent.mkdir(parents=True)
unknown_namespace.write_bytes(b'{"retained":true}\n')
error_capture, error_patch = new('reference-read-error')
journal_before_read_error = (repo / '.oms/landings.jsonl').read_bytes()
namespace_before_read_error = unknown_namespace.read_bytes()
original_walk = h['os'].walk
def unreadable_namespace(top, *args, **kwargs):
    if os.path.realpath(top) == os.path.realpath(repo / '.oms/tasks'):
        kwargs['onerror'](PermissionError('injected reference namespace read error'))
        return iter(())
    return original_walk(top, *args, **kwargs)
h['os'].walk = unreadable_namespace
try:
    for apply in (True, False):
        try:
            h['collect'](str(repo), 0, apply)
        except PermissionError as error:
            assert 'injected reference namespace read error' in str(error)
        else:
            raise AssertionError('unknown reference namespace did not veto GC')
        assert unknown.exists(), 'read-error GC removed an unknown reservation'
        assert error_capture.exists() and error_patch.exists(), 'read-error GC collected eligible evidence'
        assert unknown_namespace.read_bytes() == namespace_before_read_error
        assert (repo / '.oms/landings.jsonl').read_bytes() == journal_before_read_error
finally:
    h['os'].walk = original_walk
unknown_namespace.unlink()
unknown_namespace.parent.rmdir()
unknown_namespace.parent.parent.rmdir()
os.environ['OMS_LANDING_CAPTURE_BYTES'] = str(2 * len(source.read_bytes()))
try:
    h['allocate'](str(repo), str(source), 'land-byte-overflow', sha)
except ValueError as error:
    assert 'quota' in str(error)
else:
    raise AssertionError('peak byte quota bypassed')
os.environ.pop('OMS_LANDING_CAPTURE_BYTES')

# Inconsistent reservation/release metadata cannot purchase new quota.
quota_repo = sandbox / 'quota-invalid'
(quota_repo / '.oms').mkdir(parents=True)
quota_capture = pathlib.Path(h['allocate'](str(quota_repo), str(source), 'land-quota-invalid', sha))
quota_journal = quota_repo / '.oms/landings.jsonl'
quota_before = quota_journal.read_bytes()
quota_rows = [json.loads(line) for line in quota_before.splitlines()]
for amount in (0, 2):
    altered = json.loads(json.dumps(quota_rows))
    altered[0]['managed_capture']['bytes'] = amount
    quota_journal.write_bytes(b''.join(h['encoded'](r) for r in altered))
    before_refusal = quota_journal.read_bytes()
    assert h['usage'](str(quota_repo)) == (1, 2 * h['MAX_OBJECT'])
    os.environ['OMS_LANDING_CAPTURE_BYTES'] = str(2 * h['MAX_OBJECT'])
    try:
        h['allocate'](str(quota_repo), str(source), 'land-quota-bypass', sha)
    except ValueError as error:
        assert 'quota' in str(error)
    else:
        raise AssertionError('unknown generation waived its maximum quota charge')
    finally:
        os.environ.pop('OMS_LANDING_CAPTURE_BYTES')
    assert quota_journal.read_bytes() == before_refusal
quota_journal.write_bytes(quota_before)
quota_item = h['find'](str(quota_repo), capture=str(quota_capture))
held_directory = quota_capture.parent.with_name(quota_capture.parent.name + '-held')
os.rename(quota_capture.parent, held_directory)
for forged_release in (False, True):
    quota_journal.write_bytes(quota_before)
    if forged_release:
        h['append'](str(quota_repo), 'capture-released', quota_item)
    expected_charge = 2 * h['MAX_OBJECT'] if forged_release else quota_item['bytes']
    assert h['usage'](str(quota_repo)) == (1, expected_charge)
# Hash consistency is not enough: the probe's partial terminal/certificate and
# object inventory must never waive a moved, still-retained generation.
quota_journal.write_bytes(quota_before)
terminal = h['append'](str(quota_repo), 'capture-terminal', quota_item)
ready = h['append'](str(quota_repo), 'capture-release-ready', quota_item,
    release_proof=dict(version=1, reservation_sha256=h['digest'](h['encoded'](quota_item)),
                       certificate_sha256='0' * 64, terminal_event_sha256=h['digest'](h['encoded'](terminal)),
                       objects={'capture': {'size': len(source.read_bytes())}}))
h['append'](str(quota_repo), 'capture-released', quota_item, release_ready_sha256=h['digest'](h['encoded'](ready)))
assert h['usage'](str(quota_repo)) == (1, 2 * h['MAX_OBJECT'])
assert (held_directory / quota_capture.name).read_bytes() == source.read_bytes()
quota_journal.write_bytes(quota_before)
os.rename(held_directory, quota_capture.parent)
assert h['usage'](str(quota_repo)) == (1, 2 * len(source.read_bytes()))

# A proven empty object is valid; only missing ownership proof is unknown.
empty_repo = sandbox / 'quota-empty'
(empty_repo / '.oms').mkdir(parents=True)
empty = sandbox / 'empty.patch'
empty.write_bytes(b'')
empty_capture = pathlib.Path(h['allocate'](str(empty_repo), str(empty), 'land-empty', hashlib.sha256(b'').hexdigest()))
assert h['usage'](str(empty_repo)) == (1, 0)
h['seal'](str(empty_repo), 'land-empty', 'before-admission')
h['collect'](str(empty_repo), 0, True)
assert not empty_capture.exists() and h['usage'](str(empty_repo)) == (0, 0)

# Rehashing a genuine disposed generation's chain cannot make a malformed
# native certificate valid; its unknown history remains maximally charged.
empty_journal = empty_repo / '.oms/landings.jsonl'
valid_release = empty_journal.read_bytes()
for mutation in ('missing', 'bool', 'nan', 'infinite', 'negative', 'certificate-extra', 'terminal-extra'):
    release_rows = [json.loads(line) for line in valid_release.splitlines()]
    ready = next(row for row in release_rows if row['event'] == 'capture-release-ready')
    proof = ready['release_proof']
    cert = proof['certificate']
    if mutation == 'missing':
        cert['terminal'].pop('time')
    elif mutation == 'certificate-extra':
        cert['unknown'] = True
    elif mutation == 'terminal-extra':
        cert['terminal']['unknown'] = True
    else:
        cert['terminal']['time'] = {'bool': True, 'nan': float('nan'), 'infinite': float('inf'), 'negative': -1}[mutation]
    proof['certificate_sha256'] = h['digest'](h['encoded'](cert))
    next(row for row in release_rows if row['event'] == 'capture-released')['release_ready_sha256'] = h['digest'](h['encoded'](ready))
    empty_journal.write_bytes(b''.join(h['encoded'](row) for row in release_rows))
    assert h['usage'](str(empty_repo)) == (1, 2 * h['MAX_OBJECT']), mutation
empty_journal.write_bytes(valid_release)
assert h['usage'](str(empty_repo)) == (0, 0)

# A crash after payload disposal but before its final release row stays charged.
crash_repo = sandbox / 'quota-release-crash'
(crash_repo / '.oms').mkdir(parents=True)
crash_capture = pathlib.Path(h['allocate'](str(crash_repo), str(source), 'land-release-crash', sha))
h['seal'](str(crash_repo), 'land-release-crash', 'before-admission')
collect_globals = h['collect'].__globals__
original_append = collect_globals['append']
def fail_release(repo_arg, event, item, **extra):
    if event == 'capture-released':
        raise OSError('injected final release append failure')
    return original_append(repo_arg, event, item, **extra)
collect_globals['append'] = fail_release
try:
    h['collect'](str(crash_repo), 0, True)
finally:
    collect_globals['append'] = original_append
assert not crash_capture.parent.exists()
assert h['usage'](str(crash_repo)) == (1, 2 * len(source.read_bytes()))
assert any(row['event'] == 'capture-release-ready' for row in h['rows'](str(crash_repo)))
assert not any(row['event'] == 'capture-released' for row in h['rows'](str(crash_repo)))

# The rejected best-effort chain has one budget including seal and scans.
# These disposable probes intentionally have no managed generation: seal is a
# real no-op and no test invents native admission authority for deletion.
if os.name != 'nt':
    import signal
    deadline_repo = sandbox / 'cleanup-deadline'
    (deadline_repo / '.oms/landing-patches').mkdir(parents=True)
    legacy = deadline_repo / '.oms/landing-patches/unknown.patch'
    legacy.write_bytes(b'unknown retained evidence')
    def deadline_call():
        started = time.monotonic()
        try:
            h['cleanup_rejected'](str(deadline_repo), 'land-no-generation')
        except SystemExit as error:
            assert error.code == 75, error
        else:
            raise AssertionError('slow cleanup did not exhaust its shared deadline')
        elapsed = time.monotonic() - started
        assert 1.7 <= elapsed < 2.6, elapsed
        print('rejected-cleanup total deadline: %.3fs' % elapsed)
        assert legacy.read_bytes() == b'unknown retained evidence'
    def ready_wait(path, process):
        end = time.monotonic() + 3
        while not path.exists() and time.monotonic() < end:
            assert process.poll() is None
            time.sleep(.02)
        assert path.exists(), path
    def stop_owned(process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    previous = dict(os.environ)
    try:
        for backend in ('0', '1'):
            os.environ['OMS_LOCK_FORCE_MKDIR'] = backend
            os.environ['OMS_LOCK_DIR'] = str(sandbox / ('deadline-locks-' + backend))
            os.environ['OMS_LOCK_TIMEOUT'] = '60'
            marker_ready = sandbox / ('marker-ready-' + backend)
            native_ready = sandbox / ('native-ready-' + backend)
            marker = subprocess.Popen(['bash', '-c',
                '. "$1"; oms_with_file_lock "$2" python3 -c "$3" "$4"', 'deadline-marker',
                str(root / 'scripts/lib/file-lock.sh'),
                str(deadline_repo / '.oms/delegations/.marker-set-lock-target'),
                'import pathlib,sys,time; pathlib.Path(sys.argv[1]).touch(); time.sleep(.9)', str(marker_ready)],
                start_new_session=True)
            native = subprocess.Popen([sys.executable, '-c',
                'import pathlib,runpy,sys,time; e=runpy.run_path(sys.argv[1]); '
                'lock=e["file_lock"](pathlib.Path(sys.argv[2])); lock.__enter__(); '
                'pathlib.Path(sys.argv[3]).touch(); time.sleep(8)',
                str(root / 'scripts/lib/agent-events.py'), str(deadline_repo / '.oms/lifecycle/events.jsonl'),
                str(native_ready)], start_new_session=True)
            try:
                ready_wait(marker_ready, marker)
                ready_wait(native_ready, native)
                deadline_call()
                assert native.poll() is None, 'cleanup killed a foreign lock owner'
            finally:
                stop_owned(marker)
                stop_owned(native)
        os.environ['OMS_LOCK_FORCE_MKDIR'] = '0'
        os.environ['OMS_LOCK_DIR'] = str(sandbox / 'deadline-slow-locks')
        slow_bin = sandbox / 'deadline-bin'
        slow_bin.mkdir()
        wrapper = slow_bin / 'python3'
        wrapper.write_text('#!' + sys.executable + '\n' +
            'import os,pathlib,sys,time\n'
            'args=sys.argv[1:]\n'
            'slow=(os.environ["OMS_FIXTURE_SLOW_PHASE"] == "seal" and "seal" in args) or '
            '(os.environ["OMS_FIXTURE_SLOW_PHASE"] == "collect" and len(args)>1 and args[0]=="-c" and "collect" in args[1])\n'
            'if slow:\n pathlib.Path(os.environ["OMS_FIXTURE_SLOW_READY"]).touch(); time.sleep(8)\n'
            'os.execv(' + repr(sys.executable) + ', [' + repr(sys.executable) + ']+args)\n')
        wrapper.chmod(0o755)
        os.environ['PATH'] = str(slow_bin) + os.pathsep + previous['PATH']
        for phase in ('seal', 'collect'):
            slow_ready = sandbox / ('slow-' + phase)
            os.environ['OMS_FIXTURE_SLOW_PHASE'] = phase
            os.environ['OMS_FIXTURE_SLOW_READY'] = str(slow_ready)
            deadline_call()
            assert slow_ready.exists(), 'probe did not reach ' + phase
    finally:
        os.environ.clear()
        os.environ.update(previous)

# A linked execution worktree can be on another Windows drive. Only capture
# versus STATE filesystem equality matters; cross-drive ancestry is disjoint.
original_commonpath = os.path.commonpath
other_volume = str(sandbox / 'execution-on-another-volume')
def cross_volume(paths):
    if other_volume in paths:
        raise ValueError('different drives')
    return original_commonpath(paths)
os.path.commonpath = cross_volume
try:
    cross_capture = pathlib.Path(h['allocate'](str(repo), str(source), 'land-cross-volume', sha, other_volume))
finally:
    os.path.commonpath = original_commonpath
assert cross_capture.exists()

# Replace the shared name after the last observation but before quarantine.
# An equal-content new inode must survive, as must its native generation anchor.
capture, patch = new('race')
original_rename = os.rename
ran = []
def race(src, dst, *args, **kwargs):
    if str(src) == str(patch) and not ran:
        ran.append(True)
        foreign = sandbox / 'foreign.patch'
        foreign.write_bytes(patch.read_bytes())
        os.replace(foreign, patch)
    return original_rename(src, dst, *args, **kwargs)
os.rename = race
try:
    collect()
finally:
    os.rename = original_rename
assert ran and patch.read_bytes() == source.read_bytes()
assert (capture.parent / 'publication.anchor').exists()
assert patch.stat().st_ino != (capture.parent / 'publication.anchor').stat().st_ino
try:
    h['read_patch'](str(repo), str(patch))
except ValueError:
    pass
else:
    raise AssertionError('managed replacement escaped through the legacy reader')

# Even private ownership does not permit collecting a changed intent contract.
capture_contract, patch_contract = new('contract')
journal = repo / '.oms/landings.jsonl'
frozen_journal = journal.read_bytes()
changed = [json.loads(line) for line in frozen_journal.splitlines()]
for row in changed:
    if row.get('landing_id') == 'land-contract' and row.get('event') == 'intent':
        row['task'] = 'different-obligation'
journal.write_bytes(b''.join(h['encoded'](row) for row in changed))
collect()
assert capture_contract.exists() and patch_contract.exists()
journal.write_bytes(frozen_journal)

# Crash after quarantine: recovery uses the still-retained native inode anchor.
capture2, patch2 = new('interrupted')
quarantine = capture2.parent / 'quarantine-publication.anchor'
os.rename(patch2, quarantine)
collect()
assert not quarantine.exists() and not capture2.exists()

# No-clobber restore: a new original-name occupant survives alongside foreign
# data captured by the rename. Neither is eligible merely because bytes match.
capture3, patch3 = new('reoccupied')
ran = []
def reoccupied(src, dst, *args, **kwargs):
    if str(src) == str(patch3) and not ran:
        ran.append(True)
        foreign = sandbox / 'foreign3.patch'
        foreign.write_bytes(b'foreign before capture')
        os.replace(foreign, patch3)
        result = original_rename(src, dst, *args, **kwargs)
        patch3.write_bytes(b'new occupant')
        return result
    return original_rename(src, dst, *args, **kwargs)
os.rename = reoccupied
try:
    collect()
finally:
    os.rename = original_rename
assert patch3.read_bytes() == b'new occupant'
assert (capture3.parent / 'quarantine-publication.anchor').read_bytes() == b'foreign before capture'

# Copied certificates do not authorize a different private directory generation.
item = h['find'](str(repo), capture=str(capture))
owner = json.loads((capture.parent / 'owner.json').read_text())
replacement = sandbox / 'copied-owner'
shutil.copytree(capture.parent, replacement)
original = capture.parent.with_name(capture.parent.name + '-original')
os.rename(capture.parent, original)
os.rename(replacement, capture.parent)
try:
    h['certificate'](item)
except ValueError:
    pass
else:
    raise AssertionError('copied owner metadata became generation authority')
assert h['usage'](str(repo))[1] >= 2 * h['MAX_OBJECT']
# Restore the fixture's actual owned generation without deleting the foreign copy.
os.rename(capture.parent, replacement)
os.rename(original, capture.parent)

# The helper CLI validates every destructive argument before taking locks.
# Invalid days and APPLY values must not alter retained bytes or create locks.
validation_before = {}
for path in (repo / '.oms/landings.jsonl', repo / '.oms/artifacts/index.jsonl',
             capture, patch):
    validation_before[path] = path.read_bytes() if path.exists() else None
for invalid_args in (('-1', '1'), ('36501', '1'), ('0', '2'), ('x', '1'), ('0',), ('01', '1')):
    invalid_gc = subprocess.run([sys.executable, str(root / 'scripts/lib/landing-capture.py'),
                                 'gc', str(repo), *invalid_args],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert invalid_gc.returncode == 2, (invalid_args, invalid_gc.returncode,
                                        invalid_gc.stdout.decode(), invalid_gc.stderr.decode())
for path, before in validation_before.items():
    assert (path.read_bytes() if path.exists() else None) == before, path

# The locked collector rejects truncated JSONL instead of accepting a final
# complete row that was never durably terminated.
journal = repo / '.oms/landings.jsonl'
journal_before = journal.read_bytes()
rows_before = h['rows'](str(repo))
journal.write_bytes(journal_before + b'{"schema":1,"event":"truncated"}')
try:
    h['rows'](str(repo))
except ValueError as error:
    assert 'truncated' in str(error)
else:
    raise AssertionError('landing journal accepted a final row without LF')
assert journal.read_bytes() == journal_before + b'{"schema":1,"event":"truncated"}'
journal.write_bytes(journal_before)
assert h['rows'](str(repo)) == rows_before
reference_dir = repo / '.oms/tasks'
reference_dir.mkdir(exist_ok=True)
truncated_reference = reference_dir / 'truncated.jsonl'
truncated_reference.write_bytes(b'{"task":"complete"}')
try:
    h['reference_bytes'](str(repo))
except ValueError as error:
    assert 'truncated' in str(error)
else:
    raise AssertionError('reference scan accepted a final JSONL row without LF')
assert truncated_reference.read_bytes() == b'{"task":"complete"}'
truncated_reference.unlink()

# Large evidence trees are scanned without the former 8192-file/64 MiB cap.
# Missing aged reservations release only when neither path nor digest is pinned.
evidence_root = repo / '.oms/plan/completions/evidence'
evidence_root.mkdir(parents=True)
for index in range(8205):
    (evidence_root / ('row-%05d.txt' % index)).write_bytes(b'ordinary evidence\n')

def missing_reservation(name, source_bytes):
    source = sandbox / (name + '.patch')
    source.write_bytes(source_bytes)
    expected = hashlib.sha256(source_bytes).hexdigest()
    path = pathlib.Path(h['allocate'](str(repo), str(source), 'land-' + name, expected))
    item = h['find'](str(repo), capture=str(path))
    shutil.rmtree(path.parent)
    rows = h['rows'](str(repo))
    reservation = next(row for row in rows if row.get('event') == 'capture-reserved' and
                       row.get('managed_capture', {}).get('generation') == item['generation'])
    reservation['ts'] = '2020-01-01T00:00:00Z'
    journal.write_bytes(b''.join(h['encoded'](row) for row in rows))
    return item

# Missing private directories retain the recorded charge only when the canonical
# publication is absent or agrees with the reservation.
quota_journal = journal.read_bytes()
for name, payload, kind in (('charge-empty', b'', 'matching'),
                            ('charge-matching', b'quota payload', 'matching'),
                            ('charge-limit', b'x' * h['MAX_OBJECT'], 'matching'),
                            ('charge-absent', b'absent payload', 'absent'),
                            ('charge-mismatch', b'mismatch payload', 'mismatch'),
                            ('charge-directory', b'directory payload', 'directory'),
                            ('charge-link', b'link payload', 'link')):
    item = missing_reservation(name, payload)
    publication = repo / '.oms/landing-patches' / (item['landing_id'] + '.patch')
    if kind == 'matching':
        publication.write_bytes(payload)
    elif kind == 'mismatch':
        publication.write_bytes(payload + b'x')
    elif kind == 'directory':
        publication.mkdir()
    elif kind == 'link':
        try:
            publication.symlink_to(source)
        except (OSError, NotImplementedError):
            journal.write_bytes(quota_journal)
            continue  # Symlink creation may be unavailable on Windows.
    count, charged = h['usage'](str(repo))
    journal.write_bytes(quota_journal)
    baseline_count, baseline = h['usage'](str(repo))
    expected = 2 * h['MAX_OBJECT'] if kind in ('mismatch', 'directory', 'link') else item['bytes']
    assert (count, charged) == (baseline_count + 1, baseline + expected), (name, charged, baseline, expected)
    if kind == 'directory':
        publication.rmdir()
    elif publication.is_symlink() or publication.exists():
        publication.unlink()

os.environ['OMS_LANDING_CAPTURE_BYTES'] = str(16 * h['MAX_OBJECT'])
missing_free = missing_reservation('missing-free', b'free payload')
missing_pinned = missing_reservation('missing-pinned', b'pinned payload')
missing_terminal = missing_reservation('missing-terminal', b'terminal payload')
missing_recent = missing_reservation('missing-recent', b'recent terminal payload')
missing_path_pin = missing_reservation('missing-path-pin', b'path pin payload')
missing_ready = missing_reservation('missing-ready', b'ready payload')
missing_unknown = missing_reservation('missing-unknown', b'unknown payload')
missing_bad_time = missing_reservation('missing-bad-time', b'bad time payload')
for item in (missing_terminal, missing_recent, missing_path_pin, missing_ready, missing_unknown, missing_bad_time):
    h['append'](str(repo), 'capture-terminal', item)
rows = h['rows'](str(repo))
for row in rows:
    if row.get('event') == 'capture-terminal' and row.get('managed_capture') in (
            missing_terminal, missing_path_pin, missing_ready, missing_unknown, missing_bad_time):
        row['ts'] = '2020-01-01T00:00:00Z'
    if row.get('event') == 'capture-terminal' and row.get('managed_capture') == missing_bad_time:
        row['ts'] = 'malformed'
journal.write_bytes(b''.join(h['encoded'](row) for row in rows))
h['append'](str(repo), 'capture-release-ready', missing_ready)
h['append'](str(repo), 'capture-unrecognized', missing_unknown)
(evidence_root / 'path-pin.txt').write_text(str(repo / '.oms/landing-patches' /
                                               (missing_path_pin['landing_id'] + '.patch')))
(evidence_root / 'sha-pin.txt').write_bytes(missing_pinned['sha256'].encode())
h['collect'](str(repo), 1, True)
os.environ.pop('OMS_LANDING_CAPTURE_BYTES')
released = h['rows'](str(repo))
assert any(row.get('event') == 'capture-released' and row.get('release_missing') is True and
           row.get('managed_capture', {}).get('generation') == missing_free['generation'] for row in released), \
    'large unreferenced evidence tree prevented releasing the missing reservation'
assert not any(row.get('event') == 'capture-released' and row.get('release_missing') is True and
               row.get('managed_capture', {}).get('generation') == missing_pinned['generation'] for row in released), \
    'sha256 evidence did not pin the missing reservation'
def missing_released(item):
    return any(row.get('event') == 'capture-released' and row.get('release_missing') is True and
               row.get('managed_capture') == item for row in released)
assert missing_released(missing_terminal), 'aged missing terminal generation stayed charged'
for item in (missing_recent, missing_path_pin, missing_ready, missing_unknown, missing_bad_time):
    assert not missing_released(item), 'unsafe missing generation was released: ' + item['landing_id']
journal.write_bytes(b''.join(h['encoded'](row) for row in released if row.get('managed_capture') not in (
    missing_terminal, missing_recent, missing_path_pin, missing_ready, missing_unknown, missing_bad_time)))
journal_before = journal.read_bytes()

# publish() records whatever repository-local target it was given; a path-only
# pin on that recorded path must keep the publication.
custom = repo / '.oms/landing-patches/custom.patch'
custom_capture, _ = new('custom', True, custom)
custom_pin = repo / '.oms/plan/completions/custom-pin.json'
custom_pin.write_text(json.dumps({'artifact': {'path': str(custom)}}))
collect()
assert custom_capture.exists() and custom.exists(), 'path-only pin on a nonstandard publication was lost'
# An unreadable certificate while building needles must veto GC, not drop that
# generation's recorded paths while a later read succeeds.
custom_journal = journal.read_bytes()
real_certificate = h['collect'].__globals__['certificate']
state = dict(failed=False)
def flaky_certificate(item):
    if item['landing_id'] == 'land-custom' and not state['failed']:
        state['failed'] = True
        raise ValueError('injected first certificate read failure')
    return real_certificate(item)
h['collect'].__globals__['certificate'] = flaky_certificate
try:
    for apply in (True, False):
        state['failed'] = False
        try:
            h['collect'](str(repo), 0, apply)
        except ValueError as error:
            assert 'certificate unreadable' in str(error) and 'generation' in str(error), str(error)
        else:
            raise AssertionError('unreadable certificate did not veto GC')
        assert custom_capture.exists() and custom.exists(), 'certificate read failure lost the publication'
        assert journal.read_bytes() == custom_journal
finally:
    h['collect'].__globals__['certificate'] = real_certificate
# A lookup error is not absence: the first existence probe of that generation
# fails with PermissionError and later probes succeed; GC must preserve that
# generation (not collect it on a later probe) without aborting the pass.
custom_directory = h['find'](str(repo), capture=str(custom_capture))['directory']
custom_bytes = custom.read_bytes()
real_lstat = h['os'].lstat
probe = dict(failed=False)
def flaky_lstat(path, *args, **kwargs):
    if not probe['failed'] and os.fspath(path) == custom_directory:
        probe['failed'] = True
        raise PermissionError('injected lookup failure')
    return real_lstat(path, *args, **kwargs)
h['os'].lstat = flaky_lstat
try:
    for apply in (True, False):
        probe['failed'] = False
        h['collect'](str(repo), 0, apply)
        assert probe['failed'], 'lookup failure was not injected'
        assert custom_capture.exists() and custom.read_bytes() == custom_bytes, 'lookup error was treated as absence'
        assert journal.read_bytes() == custom_journal
finally:
    h['os'].lstat = real_lstat
custom_pin.unlink()
collect()
assert not custom_capture.exists() and not custom.exists(), 'unpinned nonstandard publication was not reclaimed'
journal_before = journal.read_bytes()

# Matching cost is linear in evidence size and independent of how many
# historical reservations contribute needles; a pin split by a megabyte
# boundary or sitting at a file edge is still found.
perf = sandbox / 'perf-repo'
(perf / '.oms/plan').mkdir(parents=True)
perf_real = os.path.realpath(str(perf))
perf_needles, perf_paths = set(), {}
for number in range(512):
    perf_id = 'land-hist-%04d' % number
    path = perf_real + '/.oms/landing-patches/' + perf_id + '.patch'
    perf_paths[number] = path
    perf_needles.update(v.encode() for v in h['reference_spellings'](perf_real, path))
    perf_needles.add(hashlib.sha256(perf_id.encode()).hexdigest().encode())
filler = (b'ordinary evidence line 0123456789abcdef /.oms/plan "x"\n' * 20000)[:1 << 20]
perf_pin = perf_paths[300].encode()
perf_sha = hashlib.sha256(b'land-hist-0007').hexdigest().encode()
bodies = [filler] * 40
bodies[3] = filler[:-10] + perf_pin + filler  # path pin straddling the 1 MiB mark
bodies[39] = filler + perf_sha  # digest pin ending exactly at end of file
for number, body in enumerate(bodies):
    (perf / '.oms/plan' / ('f%d.txt' % number)).write_bytes(body)
started = time.monotonic()
perf_found = h['reference_bytes'](perf_real, needles=perf_needles)
elapsed = time.monotonic() - started
assert elapsed < 20, 'reference scan took %.1fs for 512 reservations' % elapsed
expected = {v.encode() for v in h['reference_spellings'](perf_real, perf_paths[300])
            if v.encode() in perf_pin} | {perf_sha}
assert perf_found == expected, (sorted(perf_found), sorted(expected))
assert perf_pin in perf_found and perf_sha in perf_found

# Entry, byte and deadline budgets fail closed before anything is released.
missing_limited = missing_reservation('missing-limited', b'limited payload')
limited_journal = journal.read_bytes()
scan_globals = h['reference_bytes'].__globals__
for constant, small, words in (('MAX_REFERENCE_ENTRIES', 100, ('entries', '.oms/plan', 'limit 100')),
                               ('MAX_REFERENCE_BYTES', 1000, ('bytes', '.oms/plan', 'limit 1000')),
                               ('REFERENCE_SCAN_SECONDS', -1, ('deadline', '.oms/plan', 'limit -1'))):
    saved = scan_globals[constant]
    scan_globals[constant] = small
    try:
        try:
            h['collect'](str(repo), 1, True)
        except ValueError as error:
            assert 'reference scan' in str(error) and all(word in str(error) for word in words), (constant, str(error))
        else:
            raise AssertionError(constant + ' did not fail closed')
    finally:
        scan_globals[constant] = saved
    assert journal.read_bytes() == limited_journal, constant + ' released despite a refused scan'

# Time spent after the last file also counts: reading and matching it, then
# trailing directory-only traversal. The clock expires on the second check
# that follows the last file read, which only a directory step or the final
# check before returning can make.
class Clock:
    def __init__(self):
        self.armed, self.after = False, 0
    def monotonic(self):
        if self.armed:
            self.after += 1
        return 1e9 if self.after == 2 else 0.0
    def __getattr__(self, name):
        return getattr(time, name)
real_time, real_regular = scan_globals['time'], scan_globals['regular']
trailing = repo / '.oms/task/trailing/empty'
trailing.mkdir(parents=True)
clock = Clock()
reads = []
def counted_regular(*args, **kwargs):
    result = real_regular(*args, **kwargs)
    if os.path.relpath(args[0], str(repo)).split(os.sep)[1:2] and \
            os.path.relpath(args[0], str(repo)).split(os.sep)[1] in ('plan', 'artifacts', 'tasks', 'task'):
        reads.append(args[0])
        if len(reads) == files:
            clock.armed = True
    return result
try:
    files = sum(len(names) for ns in ('plan', 'artifacts', 'tasks', 'task')
                for _, _, names in os.walk(str(repo / '.oms' / ns)))
    scan_globals['time'], scan_globals['regular'] = clock, counted_regular
    try:
        h['collect'](str(repo), 1, True)
    except ValueError as error:
        assert 'deadline' in str(error) and '.oms/' in str(error), str(error)
    else:
        raise AssertionError('deadline expiring after the last file did not fail closed')
    assert len(reads) == files
finally:
    scan_globals['time'], scan_globals['regular'] = real_time, real_regular
    shutil.rmtree(str(repo / '.oms/task'))
assert journal.read_bytes() == limited_journal, 'released despite a late deadline'
h['collect'](str(repo), 1, True)
assert any(row.get('event') == 'capture-released' and row.get('release_missing') is True and
           row.get('managed_capture', {}).get('generation') == missing_limited['generation']
           for row in h['rows'](str(repo))), 'default limits did not release the unreferenced reservation'
journal_before = journal.read_bytes()

# The direct helper CLI must honor both inner quiescence locks as bounded
# contention, while preserving all evidence and returning the native busy code.
if os.name != 'nt':
    lock_lib = root / 'scripts/lib/file-lock.sh'
    engine_path = root / 'scripts/lib/agent-events.py'
    for lock_name, lock_target, holder_command in (
            ('lifecycle', repo / '.oms/lifecycle/events.jsonl',
             [sys.executable, '-c',
              'import runpy,sys,time; from pathlib import Path; '
              'e=runpy.run_path(sys.argv[1]); cm=e["file_lock"](Path(sys.argv[2])); '
              'cm.__enter__(); Path(sys.argv[3]).write_text("ready"); '
              'end=time.time()+60\n'
              'while not Path(sys.argv[4]).exists() and time.time()<end: time.sleep(0.05)\n'
              'cm.__exit__(None,None,None)', str(engine_path)]),
            ('index', repo / '.oms/artifacts/index.jsonl',
             ['bash', '-c',
              '. "$0"; oms_with_file_lock "$1" sh -c \'printf ready > "$1"; i=0; '
              'while [ ! -e "$2" ] && [ "$i" -lt 600 ]; do sleep 0.1; i=$((i+1)); done\' '
              'holder "$2" "$3"',
              str(lock_lib)])):
        lock_dir = sandbox / ('inner-gc-locks-' + lock_name)
        ready = sandbox / ('inner-gc-lock-ready-' + lock_name)
        lock_env = dict(os.environ, OMS_LOCK_DIR=str(lock_dir), OMS_LOCK_TIMEOUT='1')
        release = sandbox / ('inner-gc-lock-release-' + lock_name)
        holder_args = holder_command + [str(lock_target), str(ready), str(release)]
        holder = subprocess.Popen(holder_args, env=lock_env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.time() + 5
            while not ready.exists() and time.time() < deadline:
                time.sleep(0.01)
            assert ready.exists(), 'inner lock fixture did not acquire ' + lock_name
            direct_gc = subprocess.run([sys.executable, str(root / 'scripts/lib/landing-capture.py'),
                                        'gc', str(repo), '0', '1'], env=lock_env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
            assert direct_gc.returncode == 75, (lock_name, direct_gc.returncode,
                                                direct_gc.stdout.decode(), direct_gc.stderr.decode())
            assert journal.read_bytes() == journal_before, 'busy GC changed landing journal'
            assert capture.exists() and patch.exists(), 'busy GC changed retained evidence'
        finally:
            release.write_text('release')
            holder.communicate(timeout=5)
        ready.unlink()
        release.unlink()

# Marker uncertainty vetoes the locked collector, independently of age/PID.
# A readable live marker pins only generations it (or an unsettled lease) references.
def new_leased(name, task_id='', lease_id=''):
    capture = pathlib.Path(h['allocate'](str(repo), str(source), 'land-' + name, sha))
    patch = repo / '.oms/landing-patches' / ('land-' + name + '.patch')
    h['publish'](str(repo), str(capture), str(patch))
    fields = dict(landing_id='land-' + name, patch=str(patch), patch_sha=sha,
                  base_sha='fixture-base', task=task_id, lease=lease_id, plan_receipt_sha='',
                  plan_done_receipt_sha='', approval='', approval_version='')
    receipt = hashlib.sha256(json.dumps(dict(schema=1, **fields), sort_keys=True,
                                        separators=(',', ':')).encode()).hexdigest()
    for event in ('intent', 'complete'):
        row = dict(schema=1, event=event, receipt_sha=receipt, ts='2020-01-01T00:00:00Z', **fields)
        h['D']['append'](str(repo / '.oms/landings.jsonl'), h['encoded'](row), 'fixture native receipt')
    h['seal'](str(repo), 'land-' + name, 'complete')
    return capture, patch

if os.name != 'nt':
    plan_dir = repo / '.oms/plan'
    siblings = {
        'proposal-fixture.json': {'kind': 'agent-plan-proposal', 'tasks': []},
        'autopilot-run.json': {'kind': 'autopilot-run', 'stage': 'driving'},
        'autopilot-run.abc.json': {'kind': 'autopilot-run', 'stage': 'done'},
        'autopilot-run.abc.abandoned.json': {'kind': 'autopilot-abandon'},
        'discovery-fixture.json': {'schema': 1, 'sections': {}, 'bytes': 0},
        'candidates-fixture.json': {'kind': 'work-discovery-candidate-report', 'candidates': []},
    }
    (plan_dir / 'tasks.json').write_text(json.dumps({'tasks': {}}))
    for name, data in siblings.items():
        (plan_dir / name).write_text(json.dumps(data))
    cap, pub = new('plan-siblings')
    h['gc'](str(repo), 0, True)
    assert not cap.exists() and not pub.exists(), 'known plan siblings vetoed GC'
    probe = plan_dir / 'discovery-edge.json'
    for value in (None, -1, True, '0'):
        probe.write_text(json.dumps({'sections': {}, 'bytes': value}))
        assert h['delegation_state'](str(repo)) is None, value
    probe.write_text(json.dumps({'sections': {}, 'bytes': 0}))
    assert h['delegation_state'](str(repo)) is not None
    probe.write_text(json.dumps({'sections': {}, 'bytes': 1}))
    assert h['delegation_state'](str(repo)) is not None
    probe.unlink()
    disguised = plan_dir / 'autopilot-run.disguised.json'
    disguised.write_text(json.dumps({'kind': 'autopilot-run', 'tasks': {
        't': {'id': 't', 'state': 'running', 'lease_id': 'lease_disguised'}}}))
    assert 'lease_disguised' in h['delegation_state'](str(repo))[1]
    disguised.write_text(json.dumps({'kind': 'autopilot-run', 'state': 'malformed'}))
    assert h['delegation_state'](str(repo)) is None
    disguised.write_text(json.dumps({'kind': 'autopilot-run', 'tasks': []}))
    assert h['delegation_state'](str(repo)) is None
    disguised.unlink()
    for name, body in (('unknown.json', '{}'), ('unknown.json', '{malformed'),
                       ('proposal-bad.json', '{}'), ('tasks.json', '{"tasks":[]}')):
        target = plan_dir / name
        target.write_text(body)
        cap, pub = new('plan-invalid-' + str(len(body)) + '-' + name.replace('.', '-'))
        h['gc'](str(repo), 0, True)
        assert cap.exists() and pub.exists(), 'invalid plan sibling did not veto GC: ' + name
        target.unlink()
        h['gc'](str(repo), 0, True)
        assert not cap.exists() and not pub.exists(), 'removed invalid plan still vetoed GC'
    # A marker or journal can name a custom plan with a reserved sibling name.
    explicit = plan_dir / 'proposal-explicit.json'
    explicit.write_text(json.dumps(siblings['proposal-fixture.json']))
    assert h['delegation_state'](str(repo), [{'plan_file': str(explicit)}]) is None
    explicit.write_text(json.dumps({'tasks': {'t': {'id': 't', 'state': 'running',
                                                     'lease_id': 'lease_explicit'}}}))
    state = h['delegation_state'](str(repo), [{'plan_file': str(explicit)}])
    assert state is not None and 'lease_explicit' in state[1]
    explicit.unlink()
    for name in siblings:
        (plan_dir / name).unlink()

    # Keep the capture predicate aligned with the guard that actually requires
    # the lease; preceding state refusals do not define live lease authority.
    import ast
    engine = ast.parse((root / 'scripts/lib/agent_plan_core.py').read_text())
    block = next(node for node in ast.walk(engine) if isinstance(node, ast.If) and
                 ast.dump(node.test) == ast.dump(ast.parse('act == "block"', mode='eval').body))
    lease_call = ast.dump(ast.parse('require_current_lease(t)', mode='eval').body)
    guards = [node for node in block.body if isinstance(node, ast.If) and
              isinstance(node.test, ast.Compare) and
              any(ast.dump(child) == lease_call for child in ast.walk(node))]
    assert len(guards) == 1, 'block must have one live lease authority guard'
    guard = guards[0]
    assert len(guard.test.ops) == 1 and isinstance(guard.test.ops[0], ast.In)
    active_states = ast.literal_eval(guard.test.comparators[0])
    assert isinstance(active_states, tuple) and active_states
    # A completed/blocked/cancelled task retains its lease as history; only
    # the active states pin the journal's exact task/lease link.
    for state in h['PLAN_STATES']:
        cap, pub = new_leased('current-' + state, 'task-current', 'lease_current')
        plan = repo / '.oms/plan/tasks.json'
        task = {'id': 'task-current', 'state': state, 'lease_id': 'lease_current',
                'review_lease_id': '', 'patch': 'elsewhere.patch'}
        plan.write_text(json.dumps({'tasks': {'task-current': task}}))
        h['gc'](str(repo), 0, True)
        assert cap.exists() == pub.exists() == (state in active_states), state + ' retained lease has wrong capture authority'
        assert h['_task_lease_is_live'](task) == (state in active_states), state + ' lease authority differs from engine'
        task['lease_id'] = ''
        plan.write_text(json.dumps({'tasks': {'task-current': task}}))
        h['gc'](str(repo), 0, True)
        assert not cap.exists() and not pub.exists(), state + ' empty current lease pinned capture'
        plan.unlink()

    marker_root = repo / '.oms/delegations'
    marker_root.mkdir()
    for name in ('council-context.', 'council-context.ABCDE', 'council-context.ABCDEFx',
                 'unknown-directory'):
        entry = marker_root / name
        entry.mkdir()
        cap, pub = new('unknown-context-' + name)
        h['gc'](str(repo), 0, True)
        assert cap.exists() and pub.exists(), 'unknown directory was accepted: ' + name
        entry.rmdir()
        h['gc'](str(repo), 0, True)
        assert not cap.exists() and not pub.exists()
    context = marker_root / 'council-context.ABCDEF'
    context.mkdir()
    (context / 'answer-0.md').write_text('council evidence')
    cap, pub = new('council-context')
    h['gc'](str(repo), 0, True)
    assert not cap.exists() and not pub.exists(), 'council context vetoed GC'
    shutil.rmtree(context)
    context.symlink_to(sandbox, target_is_directory=True)
    cap, pub = new('linked-council-context')
    h['gc'](str(repo), 0, True)
    assert cap.exists() and pub.exists(), 'symlinked council context was accepted'
    context.unlink()
    h['gc'](str(repo), 0, True)
    assert not cap.exists() and not pub.exists()
    marker_root.rmdir()

(repo / '.oms/delegations').mkdir()
unreferenced_cap, unreferenced_pub = new_leased('marker-unreferenced', 'task-done', 'lease_done')
released_review_cap, released_review_pub = new_leased('released-review', 'task-released', 'lease_released_review')
replaced_arch_cap, replaced_arch_pub = new_leased('replaced-archive', 'task-replaced-archive', 'lease_replaced_archive')
unreceipted_arch_cap, unreceipted_arch_pub = new_leased('unreceipted-archive', 'task-unreceipted-archive', 'lease_unreceipted_archive')
ref_gen_cap, ref_gen_pub = new_leased('marker-ref-gen', 'task-gen', 'lease_gen')
ref_land_cap, ref_land_pub = new_leased('marker-ref-land', 'task-land', 'lease_land')
ref_esc_gen_cap, ref_esc_gen_pub = new_leased('marker-ref-esc-gen', 'task-esc-gen', 'lease_esc_gen')
ref_esc_land_cap, ref_esc_land_pub = new_leased('marker-ref-esc-land', 'task-esc-land', 'lease_esc_land')
ref_lease_cap, ref_lease_pub = new_leased('marker-ref-lease', 'task-live-lease', 'lease_live_marker')
unsettled_cap, unsettled_pub = new_leased('unsettled-lease', 'task-unsettled', 'lease_unsettled_plan')
unreferenced_item = h['find'](str(repo), capture=str(unreferenced_cap))
released_review_item = h['find'](str(repo), capture=str(released_review_cap))
replaced_arch_item = h['find'](str(repo), capture=str(replaced_arch_cap))
unreceipted_arch_item = h['find'](str(repo), capture=str(unreceipted_arch_cap))
ref_gen_item = h['find'](str(repo), capture=str(ref_gen_cap))
ref_land_item = h['find'](str(repo), capture=str(ref_land_cap))
ref_esc_gen_item = h['find'](str(repo), capture=str(ref_esc_gen_cap))
ref_esc_land_item = h['find'](str(repo), capture=str(ref_esc_land_cap))

for bad_name, bad_body in (
        ('unknown.json', '{malformed'),
        ('empty.json', ''),
        ('missing-id.json', json.dumps({'schema': 1, 'pid': os.getpid()})),
        ('bad-schema.json', json.dumps({'schema': 999, 'id': 'bad', 'pid': os.getpid()})),
        ('no-native.json', json.dumps({'schema': 4, 'id': 'no-native', 'pid': os.getpid()})),
        ('bad-gen.json', json.dumps({'schema': 1, 'id': 'bad-gen', 'pid': os.getpid(), 'generation': 'invalid'})),
        ('bad-refs.json', json.dumps({'schema': 1, 'id': 'bad-refs', 'pid': os.getpid(), 'references': None})),
        ('bad-nested-refs.json', json.dumps({'schema': 1, 'id': 'bad-nested', 'pid': os.getpid(), 'references': {'nested': ['']}})),
        ('bad-key-refs.json', json.dumps({'schema': 1, 'id': 'bad-key', 'pid': os.getpid(), 'references': {'': ['x']}})),
        ('bad name.json', json.dumps({'schema': 1, 'id': 'unsafe-name', 'pid': os.getpid()}))):
    bad_marker = repo / '.oms/delegations' / bad_name
    bad_marker.write_text(bad_body)
    h['gc'](str(repo), 0, True)
    assert unknown.exists() and unreferenced_cap.exists() and unreferenced_pub.exists(), bad_name
    bad_marker.unlink()

if os.name != 'nt':
    def write_marker(name, **extra):
        payload = {'schema': 4, 'id': name, 'pid': os.getpid(),
                   'native_pid': os.getpid(), 'native_pid_source': 'msys-proc-v1'}
        payload.update(extra)
        (repo / '.oms/delegations' / (name + '.json')).write_text(json.dumps(payload))

    def u_escape(text):
        return ''.join('\\u%04x' % ord(ch) for ch in text)

    def write_raw_marker(name, raw_extra, expected_text):
        base = json.dumps({'schema': 4, 'id': name, 'pid': os.getpid(),
                           'native_pid': os.getpid(), 'native_pid_source': 'msys-proc-v1'})
        raw_json = base[:-1] + ',' + raw_extra + '}'
        assert expected_text.encode() not in raw_json.encode()
        (repo / '.oms/delegations' / (name + '.json')).write_text(raw_json)

    write_marker('live-unrelated', task_id='task-unrelated', lease_id='lease_unrelated')
    write_marker('dead-ref', pid=2147483647, native_pid=2147483647,
                 generation=unreferenced_item['generation'], landing_id=unreferenced_item['landing_id'])
    write_marker('live-ref-gen', generation=ref_gen_item['generation'])
    write_marker('live-ref-land', landing_id=ref_land_item['landing_id'])
    write_raw_marker('live-ref-esc-gen',
                     '"generation":"' + u_escape(ref_esc_gen_item['generation']) + '"',
                     ref_esc_gen_item['generation'])
    write_raw_marker('live-ref-esc-land',
                     '"landing_id":"' + u_escape(ref_esc_land_item['landing_id']) + '"',
                     ref_esc_land_item['landing_id'])
    write_marker('live-ref-lease', task_id='task-live-lease', lease_id='lease_live_marker')
    retire = runpy.run_path(str(root / 'scripts/lib/plan-retire.py'))
    (repo / '.oms/plan/tasks.json').write_text(json.dumps({
        'schema': 3, 'plan_id': 'plan_' + 'a' * 32, 'goal': 'replaced archive test',
        'tasks': {
            'task-replaced-archive': {
                'id': 'task-replaced-archive', 'title': 'replaced running', 'state': 'running',
                'lease_id': 'lease_replaced_archive', 'review_lease_id': '',
                'patch': os.path.relpath(replaced_arch_pub, repo)
            }
        }
    }))
    os.environ['OMS_REPO'] = str(repo)
    try:
        replaced_archive_rel = retire['archive_replacement']({
            'path': str(repo / '.oms/plan/tasks.json'),
            'ts': '2026-01-01T00:00:00Z',
            'states': set(h['PLAN_STATES']),
            'id_re': h['ID_RE'],
            'die': lambda msg: (_ for _ in ()).throw(AssertionError(msg)),
            'durable_jsonl': str(root / 'scripts/lib/durable-jsonl.py'),
        })
    finally:
        os.environ.pop('OMS_REPO', None)
    replaced_archive_path = repo / replaced_archive_rel
    assert replaced_archive_path.exists() and (repo / '.oms/plan/retirements.jsonl').exists()
    unreceipted_bytes = json.dumps({
        'schema': 3, 'plan_id': 'plan_' + 'b' * 32, 'goal': 'unreceipted archive test',
        'tasks': {
            'task-unreceipted-archive': {
                'id': 'task-unreceipted-archive', 'title': 'unreceipted running', 'state': 'running',
                'lease_id': 'lease_unreceipted_archive', 'review_lease_id': ''
            }
        }
    }, sort_keys=True).encode('utf-8')
    unreceipted_sha = hashlib.sha256(unreceipted_bytes).hexdigest()
    unreceipted_archive_path = repo / '.oms/plan' / ('tasks.%s.archive.json' % unreceipted_sha)
    unreceipted_archive_path.write_bytes(unreceipted_bytes)
    (repo / '.oms/plan/tasks.json').write_text(json.dumps({
        'schema': 3, 'plan_id': 'plan_' + 'c' * 32, 'goal': 'lease test',
        'tasks': {
            'task-unsettled': {
                'id': 'task-unsettled', 'title': 'unsettled', 'state': 'running',
                'lease_id': 'lease_unsettled_plan', 'review_lease_id': ''
            },
            'task-released': {
                'id': 'task-released', 'title': 'released after review', 'state': 'ready',
                'lease_id': '', 'review_lease_id': 'lease_released_review'
            }
        }
    }))
    count_before, bytes_before = h['usage'](str(repo))
    h['gc'](str(repo), 0, True)
    count_mid, bytes_mid = h['usage'](str(repo))
    assert not unreferenced_cap.exists() and not unreferenced_pub.exists(), 'live unrelated marker blocked unreferenced GC'
    assert not released_review_cap.exists() and not released_review_pub.exists(), 'released review_lease_id pinned GC'
    assert not replaced_arch_cap.exists() and not replaced_arch_pub.exists(), 'receipt-backed replaced archive pinned GC'
    assert unreceipted_arch_cap.exists() and unreceipted_arch_pub.exists(), 'archive with no receipt was not preserved'
    reclaimed_bytes = unreferenced_item['bytes'] + released_review_item['bytes'] + replaced_arch_item['bytes']
    assert count_mid == count_before - 3 and bytes_mid == bytes_before - reclaimed_bytes, (
        count_before, count_mid, bytes_before, bytes_mid)
    assert ref_gen_cap.exists() and ref_gen_pub.exists(), 'live marker generation reference was not preserved'
    assert ref_land_cap.exists() and ref_land_pub.exists(), 'live marker landing_id reference was not preserved'
    assert ref_esc_gen_cap.exists() and ref_esc_gen_pub.exists(), 'escaped live marker generation reference was not preserved'
    assert ref_esc_land_cap.exists() and ref_esc_land_pub.exists(), 'escaped live marker landing_id reference was not preserved'
    assert ref_lease_cap.exists() and ref_lease_pub.exists(), 'live marker lease reference was not preserved'
    assert unsettled_cap.exists() and unsettled_pub.exists(), 'unsettled plan lease reference was not preserved'

    for name in ('live-ref-gen', 'live-ref-land', 'live-ref-lease'):
        (repo / '.oms/delegations' / (name + '.json')).unlink()
    (repo / '.oms/plan/tasks.json').write_text(json.dumps({
        'schema': 3, 'plan_id': 'plan_' + 'c' * 32, 'goal': 'lease test',
        'tasks': {
            'task-unsettled': {
                'id': 'task-unsettled', 'title': 'settled', 'state': 'done',
                'lease_id': 'lease_unsettled_plan', 'review_lease_id': ''
            }
        }
    }))
    h['gc'](str(repo), 0, True)
    count_esc, bytes_esc = h['usage'](str(repo))
    assert not ref_gen_cap.exists() and not ref_land_cap.exists() and not ref_lease_cap.exists() and not unsettled_cap.exists()
    assert unreceipted_arch_cap.exists() and unreceipted_arch_pub.exists(), 'archive with no receipt was collected'
    assert ref_esc_gen_cap.exists() and ref_esc_gen_pub.exists(), 'escaped generation capture was collected'
    assert ref_esc_land_cap.exists() and ref_esc_land_pub.exists(), 'escaped landing_id capture was collected'
    assert count_esc == count_mid - 4, (count_mid, count_esc)

    write_raw_marker('live-ref-esc-gen',
                     '"references":{"nested":["' + u_escape(ref_esc_gen_item['generation']) + '"]}',
                     ref_esc_gen_item['generation'])
    write_raw_marker('live-ref-esc-land',
                     '"references":{"nested":["' + u_escape(ref_esc_land_item['landing_id']) + '"]}',
                     ref_esc_land_item['landing_id'])
    h['gc'](str(repo), 0, True)
    assert ref_esc_gen_cap.exists() and ref_esc_gen_pub.exists(), 'nested escaped generation reference was not preserved'
    assert ref_esc_land_cap.exists() and ref_esc_land_pub.exists(), 'nested escaped landing_id reference was not preserved'
    assert h['usage'](str(repo)) == (count_esc, bytes_esc), 'quota accounting changed while escaped markers remained live'

    for name in ('live-ref-esc-gen', 'live-ref-esc-land', 'live-unrelated', 'dead-ref'):
        (repo / '.oms/delegations' / (name + '.json')).unlink()
    unreceipted_archive_path.unlink()
    replaced_archive_path.unlink()
    (repo / '.oms/plan/retirements.jsonl').unlink()
    (repo / '.oms/plan/tasks.json').unlink()
    h['gc'](str(repo), 0, True)
    count_after, bytes_after = h['usage'](str(repo))
    assert not ref_esc_gen_cap.exists() and not ref_esc_land_cap.exists() and not unreceipted_arch_cap.exists()
    assert count_after == count_esc - 3 and bytes_after == bytes_esc - ref_esc_gen_item['bytes'] - ref_esc_land_item['bytes'] - unreceipted_arch_item['bytes'], (
        count_esc, count_after, bytes_esc, bytes_after)
else:
    for cap, pub in ((unreferenced_cap, unreferenced_pub), (released_review_cap, released_review_pub),
                     (replaced_arch_cap, replaced_arch_pub), (unreceipted_arch_cap, unreceipted_arch_pub),
                     (ref_gen_cap, ref_gen_pub),
                     (ref_land_cap, ref_land_pub), (ref_esc_gen_cap, ref_esc_gen_pub),
                     (ref_esc_land_cap, ref_esc_land_pub), (ref_lease_cap, ref_lease_pub),
                     (unsettled_cap, unsettled_pub)):
        shutil.rmtree(cap.parent)
        pub.unlink()
# Live attempts pin only their references. Expired ordinary attempts have no
# authority; landing-reference writers retain their references regardless of age.
ev = runpy.run_path(str(root / 'scripts/lib/agent-events.py'))
def stale_attempt(provider, tool, refs=None):
    attempt = ev['create_attempt'](pathlib.Path(repo), provider=provider, tool=tool, refs=refs)
    row = ev['new_event'](attempt, 2, 'attempt.state_changed', from_state='queued', to_state='starting',
                          actor={'kind': 'system', 'name': 'fixture'})
    row['ts'] = '2020-01-01T00:00:00Z'
    ev['append_lifecycle'](pathlib.Path(repo), row)
if os.name != 'nt':  # Windows gc preserves before reaching the lifecycle veto
    stale_attempt('codex', 'expired-fixture')
    capture3, patch3 = new('expired')
    h['gc'](str(repo), 0, True)
    assert not capture3.exists() and not patch3.exists(), 'long-expired foreign attempt pinned GC'
    capture3b, patch3b = new('marked')
    stale_attempt('local', 'patch-admit', {'reference_writer': 'landing-reference-v1',
                                        'patch': os.path.relpath(patch3b, repo)})
    h['gc'](str(repo), 0, True)
    assert capture3b.exists() and patch3b.exists(), 'expired reference-writer attempt did not veto GC'
subprocess.run(['bash', str(root / 'scripts/agent-events.sh'), '--repo', str(repo), 'start',
                '--provider', 'codex', '--tool', 'capture-fixture', '--then', 'starting'],
               stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
capture4, patch4 = new('active')
h['gc'](str(repo), 0, True)
if os.name != 'nt':
    assert not capture4.exists() and not patch4.exists(), 'unrelated fresh attempt vetoed GC'
    hashed_cap, hashed_pub = new('hashed-writer-reference')
    ev['create_attempt'](repo, provider='local', tool='patch-admit', refs={
        'reference_writer': 'landing-reference-v1',
        'reference_input_sha256': h['digest'](str(hashed_cap).encode())})
    h['gc'](str(repo), 0, True)
    assert hashed_cap.exists() and hashed_pub.exists(), 'hashed reference-writer input did not pin capture'
    unrelated_cap, unrelated_pub = new('unrelated-to-hashed-writer')
    h['gc'](str(repo), 0, True)
    assert not unrelated_cap.exists() and not unrelated_pub.exists(), 'hashed writer vetoed unrelated capture'
    # Task packet writers bind the destination, not the capture they may add.
    reference = root / 'scripts/lib/landing-reference.py'
    packet = repo / '.oms/task/current.md'
    packet.parent.mkdir(parents=True, exist_ok=True)
    packet.write_text('pending task packet')
    packet_cap, packet_pub = new('pending-task-packet')
    operation = subprocess.check_output([
        sys.executable, str(reference), 'begin', str(repo), 'agent-task', str(packet)],
        text=True).strip()
    for result in (None, '1'):
        if result is not None:
            subprocess.run([sys.executable, str(reference), 'finish', str(repo), operation, result], check=True)
        h['gc'](str(repo), 0, True)
        assert packet_cap.exists() and packet_pub.exists(), 'task packet writer lost pending capture'
    subprocess.run([sys.executable, str(reference), 'finish', str(repo), operation, 'refuse-no-write'], check=True)
    h['gc'](str(repo), 0, True)
    assert not packet_cap.exists() and not packet_pub.exists(), 'terminal task writer pinned capture'

    observer_cap, observer_pub = new('unscoped-local-observer')
    observer = ev['create_attempt'](repo, provider='local', tool='capture-observer')
    h['gc'](str(repo), 0, True)
    assert observer_cap.exists() and observer_pub.exists(), 'unscoped local observer lost capture'
    ev['append_lifecycle'](repo, ev['new_event'](
        observer, 2, 'attempt.state_changed', from_state='queued', to_state='failed'))
    h['gc'](str(repo), 0, True)
    assert not observer_cap.exists() and not observer_pub.exists(), 'terminal local observer pinned capture'

    for number, (task_id, refs) in enumerate((
            ('task-lifecycle', {}),
            ('task-lifecycle', {'panel_lease_id': 'lease_lifecycle'}),
            ('', {'panel_lease_id': 'lease_lifecycle'}))):
        linked_cap, linked_pub = new_leased('lifecycle-linked-' + str(number), 'task-lifecycle', 'lease_lifecycle')
        other_cap, other_pub = new('lifecycle-unrelated-' + str(number))
        attempt = ev['create_attempt'](repo, provider='codex', tool='peer-delegate', task_id=task_id, refs=refs)
        h['gc'](str(repo), 0, True)
        assert linked_cap.exists() and linked_pub.exists(), 'standard lifecycle task/lease did not pin capture'
        assert not other_cap.exists() and not other_pub.exists(), 'standard lifecycle task/lease vetoed unrelated capture'
        ev['append_lifecycle'](repo, ev['new_event'](
            attempt, 2, 'attempt.state_changed', from_state='queued', to_state='failed'))
        h['gc'](str(repo), 0, True)
        assert not linked_cap.exists() and not linked_pub.exists(), 'terminal lifecycle task/lease pinned capture'

    capture4, patch4 = new('active-reference')
    ev['create_attempt'](repo, provider='codex', tool='capture-ref-fixture',
                         refs={'patch': os.path.relpath(patch4, repo)})
    h['gc'](str(repo), 0, True)
    assert capture4.exists() and patch4.exists(), 'fresh referencing attempt did not pin capture'
    for number, input_hash in enumerate(('', '0', 'z' * 64, '0' * 64)):
        edge_cap, edge_pub = new('writer-hash-edge-' + str(number))
        attempt = ev['create_attempt'](repo, provider='local', tool='patch-admit', refs={
            'reference_writer': 'landing-reference-v1', 'reference_input_sha256': input_hash})
        h['gc'](str(repo), 0, True)
        if input_hash == '0' * 64:
            assert not edge_cap.exists() and not edge_pub.exists(), 'unrelated valid writer hash vetoed GC'
        else:
            assert edge_cap.exists() and edge_pub.exists(), 'unknown writer hash did not preserve capture'
        ev['append_lifecycle'](repo, ev['new_event'](
            attempt, 2, 'attempt.state_changed', from_state='queued', to_state='failed',
            actor={'kind': 'system', 'name': 'fixture'}))
        h['gc'](str(repo), 0, True)
        assert not edge_cap.exists() and not edge_pub.exists(), 'terminal unknown writer pinned capture'
    unreadable_cap, unreadable_pub = new('unreadable-attempt')
    lifecycle = repo / '.oms/lifecycle/events.jsonl'
    lifecycle_before = lifecycle.read_bytes()
    lifecycle.write_bytes(lifecycle_before + b'{malformed\n')
    try:
        h['gc'](str(repo), 0, True)
        assert unreadable_cap.exists() and unreadable_pub.exists(), 'unreadable attempt did not preserve captures'
    finally:
        lifecycle.write_bytes(lifecycle_before)
    h['gc'](str(repo), 0, True)
    assert not unreadable_cap.exists() and not unreadable_pub.exists(), 'repaired unrelated attempt pinned capture'
else:
    assert capture4.exists() and patch4.exists()

# The destructive collector is reachable only through the fully locked gc
# entrypoint. Its former direct `collect` CLI path must now be rejected.
rejected_collect = subprocess.run([sys.executable, str(root / 'scripts/lib/landing-capture.py'),
                                   'collect', str(repo), '0', '1'],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert rejected_collect.returncode != 0, 'direct collect CLI bypassed outer locks'
assert capture4.exists() and patch4.exists(), 'rejected collect CLI changed retained evidence'

# A direct gc CLI invocation must contend on every outer writer lock before
# reaching quiescence or collection. Inject lock holders rather than depending
# on timing between two unlocked GC processes.
if os.name != 'nt':
    lock_lib = root / 'scripts/lib/file-lock.sh'
    for lock_name, lock_target in (
            ('landings', repo / '.oms/landings.jsonl'),
            ('markers', repo / '.oms/delegations/.marker-set-lock-target'),
            ('plan', repo / '.oms/plan/tasks.json')):
        lock_dir = sandbox / ('direct-gc-locks-' + lock_name)
        ready = sandbox / ('direct-gc-lock-ready-' + lock_name)
        lock_env = dict(os.environ, OMS_LOCK_DIR=str(lock_dir), OMS_LOCK_TIMEOUT='1')
        holder = subprocess.Popen([
            'bash', '-c',
            '. "$1"; oms_with_file_lock "$2" sh -c \'printf ready > "$1"; sleep 3\' lock-holder "$3"',
            'direct-gc-lock-holder', str(lock_lib), str(lock_target), str(ready)],
            env=lock_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.time() + 5
            while not ready.exists() and time.time() < deadline:
                time.sleep(0.01)
            assert ready.exists(), 'outer lock fixture did not acquire ' + lock_name
            direct_gc = subprocess.run([sys.executable, str(root / 'scripts/lib/landing-capture.py'),
                                        'gc', str(repo), '0', '1'], env=lock_env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            assert direct_gc.returncode == 75, (lock_name, direct_gc.returncode,
                                                direct_gc.stdout.decode(), direct_gc.stderr.decode())
            assert capture4.exists() and patch4.exists(), 'blocked direct GC changed retained evidence'
        finally:
            holder.wait(timeout=5)
        ready.unlink()

    # A custom plan publishes capture references too, so its writers must take
    # the default plan lock that GC holds. Hold it with the real helper: a
    # custom-file mutation waits (and leaves its file alone) while GC preserves.
    (repo / '.oms/plan').mkdir(parents=True, exist_ok=True)
    plan_env = {k: v for k, v in os.environ.items()
                if k not in ('OMS_HARNESS_CHILD', 'OMS_HARNESS_DELEGATE_DEPTH')}
    plan_env.update(OMS_LOCK_DIR=str(sandbox / 'custom-plan-locks'), OMS_LOCK_TIMEOUT='1')
    def custom_plan(*words, name='alternate.json'):
        return subprocess.run(['bash', str(root / 'scripts/agent-plan.sh'), '--repo', str(repo),
                               '--file', str(repo / '.oms/plan' / name)] + list(words),
                              env=plan_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    created = custom_plan('init', '--goal', 'custom')
    assert created.returncode == 0, created.stderr.decode()
    custom_before = (repo / '.oms/plan/alternate.json').read_bytes()
    # Locks are keyed by path string, so aliases of the default plan are
    # distinct keys that must still contend on the default plan lock.
    default_created = subprocess.run(['bash', str(root / 'scripts/agent-plan.sh'), '--repo', str(repo),
                                      'init', '--goal', 'default'], env=plan_env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert default_created.returncode == 0, default_created.stderr.decode()
    os.symlink('tasks.json', repo / '.oms/plan/symlink-alias.json')
    os.link(repo / '.oms/plan/tasks.json', repo / '.oms/plan/hardlink-alias.json')
    default_before = (repo / '.oms/plan/tasks.json').read_bytes()
    ready = sandbox / 'custom-plan-lock-ready'
    holder = subprocess.Popen([
        'bash', '-c',
        '. "$1"; oms_with_file_lock "$2" sh -c \'printf ready > "$1"; sleep 10\' lock-holder "$3"',
        'custom-plan-lock-holder', str(lock_lib), str(repo / '.oms/plan/tasks.json'), str(ready)],
        env=plan_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        deadline = time.time() + 5
        while not ready.exists() and time.time() < deadline:
            time.sleep(0.01)
        assert ready.exists(), 'default plan lock fixture did not start'
        blocked = custom_plan('add', '--id', 'held', '--title', 'held')
        assert blocked.returncode != 0, 'custom plan writer ignored the default plan lock'
        assert (repo / '.oms/plan/alternate.json').read_bytes() == custom_before
        for alias in ('symlink-alias.json', 'hardlink-alias.json'):
            aliased = custom_plan('add', '--id', 'alias', '--title', 'alias', name=alias)
            assert aliased.returncode != 0, 'aliased default plan writer skipped the default plan lock: ' + alias
            assert (repo / '.oms/plan/tasks.json').read_bytes() == default_before, alias
        gc_busy = subprocess.run([sys.executable, str(root / 'scripts/lib/landing-capture.py'),
                                  'gc', str(repo), '0', '1'], env=plan_env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert gc_busy.returncode == 75, gc_busy.returncode
        assert capture4.exists() and patch4.exists()
    finally:
        holder.wait(timeout=30)
    plan_env['OMS_LOCK_TIMEOUT'] = '30'
    released = custom_plan('add', '--id', 'held', '--title', 'held')
    assert released.returncode == 0, released.stderr.decode()
    aliased = custom_plan('add', '--id', 'alias', '--title', 'alias', name='symlink-alias.json')
    assert aliased.returncode == 0, aliased.stderr.decode()

    # GC scans references only under its roots, so review/finish must refuse a
    # managed capture reference in a plan outside them (or symlinked out).
    plan_capture, plan_patch = new('plan-root')
    plan_env['OMS_LOCK_TIMEOUT'] = '30'
    def review_capture(plan_path, name, via=None):
        def run(target, *words):
            return subprocess.run(['bash', str(root / 'scripts/agent-plan.sh'), '--repo', str(repo),
                                   '--file', str(target)] + list(words),
                                  env=plan_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for words in (('init', '--goal', name), ('add', '--id', 't', '--title', 't'),
                      ('claim', '--id', 't', '--provider', 'codex'), ('start', '--id', 't')):
            step = run(plan_path, *words)
            assert step.returncode == 0, (words, step.stderr.decode())
        before = plan_path.read_bytes()
        return before, run(via or plan_path, 'review', '--id', 't', '--patch', os.path.relpath(plan_patch, repo))
    outside = repo / 'custom-plan.json'
    for plan_path, via in ((outside, None),):
        before, refused = review_capture(plan_path, 'outside', via)
        assert refused.returncode != 0 and b'.oms/plan' in refused.stderr, (
            refused.returncode, refused.stdout.decode(), refused.stderr.decode())
        assert plan_path.read_bytes() == before, 'refused review changed the outside plan'
    # An outside symlink to an inside plan is replaced by save(), not followed.
    os.unlink(repo / '.oms/plan/hardlink-alias.json')  # a hard-linked default plan is unusable
    os.symlink('.oms/plan/tasks.json', repo / 'outside.json')
    for words in (('add', '--id', 'alias', '--title', 'alias'), ('claim', '--id', 'alias', '--provider', 'codex'),
                  ('start', '--id', 'alias')):
        step = subprocess.run(['bash', str(root / 'scripts/agent-plan.sh'), '--repo', str(repo)] + list(words),
                              env=plan_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert step.returncode == 0, (words, step.stderr.decode())
    tasks_before = (repo / '.oms/plan/tasks.json').read_bytes()
    refused = subprocess.run(['bash', str(root / 'scripts/agent-plan.sh'), '--repo', str(repo),
                              '--file', str(repo / 'outside.json'), 'review', '--id', 'alias',
                              '--patch', os.path.relpath(plan_patch, repo)],
                             env=plan_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert refused.returncode != 0 and b'.oms/plan' in refused.stderr, refused.stderr.decode()
    assert os.readlink(repo / 'outside.json') == '.oms/plan/tasks.json'
    assert (repo / '.oms/plan/tasks.json').read_bytes() == tasks_before
    inside = repo / '.oms/plan/inroot.json'
    _, accepted = review_capture(inside, 'inside')
    assert accepted.returncode == 0, accepted.stderr.decode()
    assert plan_capture.exists() and plan_patch.exists()
PY
}

test_work_discovery_packet_is_bounded_and_complete() {
  local repo="$TMP/discovery" readers="$TMP/discovery-readers" out rc remote="$TMP/discovery-origin.git" \
    sibling="$TMP/discovery-sibling" handoff="$TMP/discovery-handoff" \
    duplicate="$TMP/zz-discovery-duplicate" foreign="$TMP/discovery-foreign" \
    foreign_remote="$TMP/discovery-foreign.git" state_a state_b state_c state_d state_e state_parent
  mkdir -p "$repo" "$readers"
  export XDG_STATE_HOME="$TMP/land-state"
  git init -q "$repo"
  git -C "$repo" config user.name discovery-test
  git -C "$repo" config user.email discovery@example.invalid
  printf 'seed\n' > "$repo/seed"
  git -C "$repo" add seed
  git -C "$repo" commit -qm seed
  git clone --quiet --bare "$repo" "$remote"
  git -C "$repo" remote add origin "$remote"
  git clone --quiet "$remote" "$sibling"
  git clone --quiet "$remote" "$handoff"
  git clone --quiet "$remote" "$duplicate"
  git clone --quiet --bare "$repo" "$foreign_remote"
  git clone --quiet "$foreign_remote" "$foreign"
  state_a="$(bash -c '. "$1"; oms_land_state_dir "$2"' land-state "$ROOT/scripts/lib/land-receipts.sh" "$repo")"
  state_b="$(bash -c '. "$1"; oms_land_state_dir "$2"' land-state "$ROOT/scripts/lib/land-receipts.sh" "$sibling")"
  state_c="$(bash -c '. "$1"; oms_land_state_dir "$2"' land-state "$ROOT/scripts/lib/land-receipts.sh" "$foreign")"
  state_d="$(bash -c '. "$1"; oms_land_state_dir "$2"' land-state "$ROOT/scripts/lib/land-receipts.sh" "$handoff")"
  state_e="$(bash -c '. "$1"; oms_land_state_dir "$2"' land-state "$ROOT/scripts/lib/land-receipts.sh" "$duplicate")"
  state_parent="${state_a%/*}"
  # Place the receipt-only handoff namespace before the producer namespace.
  mkdir -p "$state_d" "$state_b" "$state_c" "$state_e"
  [ ! -e "$state_a" ] || fail "discovery fixture must start without local land state"
  python3 - "$repo" "$sibling" "$foreign" "$state_a" "$state_b" "$state_c" "$state_d" "$state_e" <<'PY'
import hashlib, json, pathlib, subprocess, sys
repo, _, foreign, _, state_b, state_c, state_d, state_e = map(pathlib.Path, sys.argv[1:])
def git(path, *args):
    return subprocess.check_output(['git', '-C', str(path), *args], text=True).replace('\r', '').strip()
sha = git(repo, 'rev-parse', 'HEAD')
url = git(repo, 'remote', 'get-url', '--push', 'origin')
foreign_url = git(foreign, 'remote', 'get-url', '--push', 'origin')
def receipt(directory, remote_url, command, receipt_sha=sha, rc=None, state='passed'):
    request_hash = hashlib.sha256(json.dumps([remote_url, 'origin', 'main', command],
                                             ensure_ascii=True).encode()).hexdigest()[:24]
    request = receipt_sha + '-' + request_hash
    gate = {'command': command}
    if rc is not None:
        gate['rc'] = rc
    row = {'schema': 1, 'state': state, 'sha': receipt_sha, 'request': request,
           'remote': 'origin', 'target': 'main', 'gate': gate,
           'log': '/ignored/arbitrary/path'}
    path = directory / (request + '.json')
    path.write_text(json.dumps(row, separators=(',', ':')) + '\n')
    return path, request
command = 'bash scripts/check.sh --affected'
passed, request = receipt(state_b, url, command)
(state_d / passed.name).write_bytes(passed.read_bytes())
producer = json.loads(passed.read_bytes())
producer['finished_at'] = '2026-01-12T12:00:00Z'
passed.write_text(json.dumps(producer, separators=(',', ':')) + '\n')
log = state_b / (request + '.log')
log.write_text(
    'land attempt: 1 started_at=2026-01-10T10:00:00Z sha=' + sha + '\n'
    'check: affected:tests/functional-evolution-smoke.sh FAILED (3s)\n'
    + ''.join('check: scripts-smoke.test_older_%02d FAILED (1s)\n' % n for n in range(17)) +
    'check: scripts-smoke.test_agent_plan_lifecycle_receipt_history FAILED (1s)\n'
    'check: scripts-smoke.test_agent_plan_lifecycle_receipt_history FAILED (1s)\n'
    'land attempt: 2 started_at=2026-01-11T10:00:00Z sha=' + sha + '\n'
    'check: scripts-smoke.test_agent_plan_lifecycle_receipt_history FAILED (2s)\n'
    'land attempt: 3 started_at=2026-01-12T10:00:00Z sha=' + sha + '\n'
    'check: scripts-smoke.test_newest_failure FAILED (1s)\n'
    'check: ok\n'
    + ''.join('slow-stage: %ss scripts-smoke\n' % n for n in (100, 200, 300, 600, 700, 800, 900, 1000))
    + 'slow-stage: 12s short-stage\nslow-stage: 18s short-stage\n'
    + ''.join('slow-stage: 210s shared-stage\n' for _ in range(6)))
(state_e / passed.name).write_bytes(passed.read_bytes())
independent_log = []
for n in range(1, 10):
    independent_log.append('land attempt: %d started_at=2026-01-%02dT10:00:00Z sha=%s\n' %
                           (n, n + 9, sha))
    if n == 3:
        independent_log.extend('slow-stage: 210s shared-stage\n' for _ in range(6))
    if n >= 4:
        independent_log.append(
            'slow-stage: %ds affected:tests/goal-drive-recovery-smoke.sh#2\n' % (170 + n * 10))
    independent_log.append('check: ok\n')
(state_e / log.name).write_text(''.join(independent_log))
receipt(state_b, url, command + ' --missing-log')
_, legacy = receipt(state_b, url, command + ' --legacy-log')
(state_b / (legacy + '.log')).write_text(
    'check: scripts-smoke.test_legacy_log FAILED (1s)\n')
sha_retry, sha_never = 'a' * 40, 'b' * 40
_, retry_failure = receipt(state_b, url, command + ' --retry-failure', sha_retry, 1, 'failed')
(state_b / (retry_failure + '.log')).write_text(
    'land attempt: 1 started_at=2026-01-10T10:00:00Z sha=' + sha_retry + '\n'
    'check: affected:tests/same-stage.py FAILED (1s)\n')
_, retry_pass = receipt(state_b, url, command + ' --retry-pass', sha_retry, 0)
(state_b / (retry_pass + '.log')).write_text(
    'land attempt: 1 started_at=2026-01-11T10:00:00Z sha=' + sha_retry + '\ncheck: ok\n')
_, never_failure = receipt(state_b, url, command + ' --never-failure', sha_never, 1, 'failed')
(state_b / (never_failure + '.log')).write_text(
    'land attempt: 1 started_at=2026-01-10T10:00:00Z sha=' + sha_never + '\n'
    'check: affected:tests/never-stage.py FAILED (1s)\n')
receipt(state_c, foreign_url, command + ' --unbound-pass', sha_never, 0)
_, oversized = receipt(state_b, url, command + ' --oversized-log')
(state_b / (oversized + '.log')).write_bytes(b'x' * (1024 * 1024 + 1))
receipt(state_c, foreign_url, command)
PY
  cat > "$readers/fail-ledger.sh" <<'EOF'
#!/usr/bin/env bash
python3 - <<'PY'
import json
rows = [
    {'fingerprint': 'failure-%02d' % n, 'count': n + 1, 'summary': ('summary-%02d ' % n) + 'x' * 260 + '\nsecond line'}
    for n in range(14)
]
for row in rows:
    row.update(kind='test', attention='actionable', exit=1)
rows.append({'fingerprint': 'failure-14', 'count': 99, 'kind': 'hook', 'attention': 'retiring', 'exit': 129,
             'cmd': 'private-command-text'})
rows.extend([
    {'fingerprint': 'failure-%02d' % n, 'count': 0, 'attention': 'stale', 'summary': ('summary-%02d ' % n) + 'x' * 260 + '\nsecond line'}
    for n in range(15, 19)
])
rows.append({'fingerprint': 'failure-00-null', 'count': None, 'summary': 'null count row\nsecond line'})
print(json.dumps({'schema': 1, 'failures': rows, 'invalid_rows': 2}))
PY
EOF
  cat > "$readers/ci-status.sh" <<'EOF'
#!/usr/bin/env bash
exec bash "$OMS_REAL_CI_STATUS" "$@"
EOF
  cat > "$readers/gh" <<'EOF'
#!/usr/bin/env bash
case "$2" in
  list)
    printf '[{"databaseId":1,"headSha":"abcdef1234567890","conclusion":"failure","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:05:00Z"}'
    [ -z "$OMS_TEST_CI_MANY" ] || printf ',{"databaseId":2,"headSha":"abcdef1234567891","conclusion":"failure","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:05:00Z"}'
    printf ']\n' ;;
  view)
    printf 'job-a\tstep\tmessage\njob-a\tstep\tcheck: test_x FAILED\n'
    if [ -n "$OMS_TEST_CI_MANY" ]; then
      pad="$(printf 'x%.0s' $(seq 1 200))"
      for n in $(seq 10 21); do printf 'job-%s-%s\tstep\tmessage\n' "$n" "$pad"; done
    fi ;;
  *) printf '[]\n' ;;
esac
EOF
  cat > "$readers/agent-plan.sh" <<'EOF'
#!/usr/bin/env bash
printf '[{"id":"open-1","state":"ready","claim_expired":true,"reason":"blocked on review"},{"id":"done-1","state":"done"}]\r\n'
EOF
  cat > "$readers/agent-memory.sh" <<'EOF'
#!/usr/bin/env bash
printf 'early line\nhypothesis: early clue\n'
for n in $(seq 1 30); do printf 'ordinary line %s\n' "$n"; done
printf 'hypothesis: late clue\r\n'
EOF
  cat > "$readers/oms" <<'EOF'
#!/usr/bin/env bash
if [ -n "$OMS_TEST_METRICS_JSON" ]; then printf '%s\r\n' "$OMS_TEST_METRICS_JSON"; exit 0; fi
printf '{"since":"2026-01-01","until":"2026-01-02","coverage":{"complete":true},"write_state":{"reconciled":true},"failures":[],"stages":[]}\r\n'
EOF
  cat > "$readers/sitecustomize.py" <<'PY'
import os
target = os.environ.get('OMS_TEST_LAND_PARENT')
original_scandir = os.scandir
class OrderedEntries:
    def __init__(self, source):
        self.source = source
        self.entries = sorted(source, key=lambda entry: entry.name)
    def __enter__(self):
        return iter(self.entries)
    def __exit__(self, *args):
        self.source.close()
def ordered_scandir(path):
    entries = original_scandir(path)
    if target and os.path.abspath(path) == os.path.abspath(target):
        return OrderedEntries(entries)
    return entries
os.scandir = ordered_scandir
PY
  chmod +x "$readers"/*
  export OMS_WORK_DISCOVERY_SCRIPTS_DIR="$readers" OMS_WORK_DISCOVERY_OMS="$readers/oms"
  export OMS_GH_BIN="$readers/gh" OMS_REAL_CI_STATUS="$ROOT/scripts/ci-status.sh"
  export OMS_TEST_LAND_PARENT="$state_parent" PYTHONPATH="$readers${PYTHONPATH:+:$PYTHONPATH}"
  out="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo" 2>"$TMP/discovery-err")" ||
    fail "work discovery collection failed"
  [ ! -s "$TMP/discovery-err" ] || fail "successful collection wrote to stderr"
  python3 - "$out" "$repo" <<'PY' || fail "work discovery packet omitted evidence or normalization"
import json, sys
import re
r = json.loads(sys.argv[1])
repo = sys.argv[2]
assert r["schema"] == 1 and r["sections"].keys() == {
    "failures", "metrics", "reviews", "ci", "tasks", "rejected"}, r
failures = r["sections"]["failures"]["items"]
ledger = [row for row in failures if row["ref"].startswith("failure-")]
assert len(ledger) == 16, ledger
assert [row["count"] for row in ledger[:14]] == list(range(14, 0, -1)), ledger
assert ledger[14]["fingerprint"] == "failure-00-null", ledger[14]
assert "count" not in ledger[14], ledger[14]
retiring = ledger[15]
assert retiring["fingerprint"] == "failure-14" and retiring["count"] == 99, ledger
assert (retiring["last_exit"], retiring["kind"], retiring["attention"]) == (129, "hook", "retiring"), retiring
assert "summary" not in retiring and "cmd" not in retiring, retiring
assert "private-command-text" not in json.dumps(r), r
assert (ledger[0]["last_exit"], ledger[0]["kind"], ledger[0]["attention"]) == (1, "test", "actionable"), ledger[0]
assert all("\n" not in row["summary"] and len(row["summary"]) <= 240 for row in ledger[:15]), ledger
assert r["sections"]["failures"]["coverage"]["count"] == 20, r
assert r["sections"]["failures"]["coverage"]["selected"] == 16, r
assert r["sections"]["failures"]["coverage"]["complete"] is False, r
assert r["sections"]["failures"]["coverage"]["invalid_rows"] == 2, r
land = [row for row in r["sections"]["failures"]["items"] if row.get("ref", "").startswith("land:")]
by_name = {row["name"]: row for row in land}
assert "scripts-smoke.test_newest_failure" in by_name, by_name
assert len(land) == 16, land
test = by_name["scripts-smoke.test_agent_plan_lifecycle_receipt_history"]
assert test["kind"] == "test" and test["failed_attempts"] == 2, test
assert test["first_attempt_date"] == "2026-01-10" and test["last_attempt_date"] == "2026-01-11", test
assert "affected:tests/functional-evolution-smoke.sh" in by_name, by_name
assert "scripts-smoke.test_legacy_log" not in by_name, by_name
same_stage = by_name["affected:tests/same-stage.py"]
assert same_stage["failed_attempts"] == 1 and same_stage["same_sha_passed"] == 1, same_stage
never_stage = by_name["affected:tests/never-stage.py"]
assert never_stage["failed_attempts"] == 1 and never_stage["same_sha_passed"] == 0, never_stage
assert len([row for row in land if row["name"] == test["name"]]) == 1, land
coverage = r["sections"]["failures"]["coverage"]
assert coverage["land_logs_legacy"] == 1, coverage
assert coverage["land_logs_incomplete"] == 0, coverage
assert coverage["land_logs_missing"] == 1 and coverage["land_logs_oversized"] == 1, coverage
assert coverage["land_scan_complete"] is False and "SSH/HTTPS aliases may be missed" in coverage["association"], coverage
assert r["sections"]["ci"]["status"] == "available", r
ci = r["sections"]["ci"]
assert ci["coverage"]["stage_timing"] == "land-logs", ci
assert ci["coverage"]["stage_timing_observations"] == 28, ci
assert ci["coverage"]["stage_timing_source"] == "local land gates (not CI runners)", ci
affected_name = "affected:tests/goal-drive-recovery-smoke.sh#2"
stages = {item["ref"]: item for item in ci["items"]
          if item["ref"] in {"scripts-smoke", "short-stage", affected_name, "shared-stage"}}
assert "scripts-smoke" in stages and "short-stage" not in stages, ci
assert stages["scripts-smoke"]["summary"] == (
    "stage scripts-smoke median 650s max 1000s (n=8), recent 800s vs 200s; local land gates, not CI runners"), stages
assert stages[affected_name]["summary"] == (
    "stage affected:tests/goal-drive-recovery-smoke.sh#2 median 235s max 260s (n=6), recent 240s vs 210s; local land gates, not CI runners"), stages
assert stages["shared-stage"]["summary"] == (
    "stage shared-stage median 210s max 210s (n=12), recent 210s vs 210s; local land gates, not CI runners"), stages
assert ci["items"].index(stages["scripts-smoke"]) == len(
    [item for item in ci["items"] if item["ref"] not in {"scripts-smoke", "short-stage", affected_name, "shared-stage"}]), ci
ci_refs = {item["ref"]: item for item in ci["items"]}
assert ci_refs["job:job-a"]["summary"] == "job-a: 1", ci
assert ci_refs["test:test_x"]["summary"] == "test_x: 1", ci
assert "abcdef123456" in ci_refs, ci
task = next(row for row in r["sections"]["tasks"]["items"] if row["id"] == "open-1")
assert task["claim_expired"] is True and task["reason"] == "blocked on review", task
assert [x["text"] for x in r["sections"]["rejected"]["items"]] == [
    "hypothesis: early clue", "hypothesis: late clue"], r
assert r["sections"]["metrics"]["coverage"]["complete"] is True, r
metric = r["sections"]["metrics"]["items"][0]
assert set(metric) == {"ref", "window", "coverage", "incomplete", "write_state_reconciliation",
                       "failure_reason_counts", "failure_stage_buckets", "cancel_totals"}, metric
assert all("\\r" not in json.dumps(section) for section in r["sections"].values()), r
# Recollect the same admitted logs after removing timing records to verify the
# unavailable state without adding another fixture or collector path.
import pathlib, os, subprocess
for log_path in pathlib.Path(os.environ["OMS_TEST_LAND_PARENT"]).glob("*/*.log"):
    try:
        text = log_path.read_text()
    except (UnicodeError, OSError):
        continue
    log_path.write_text("".join(line for line in text.splitlines(True)
                                 if not line.startswith("slow-stage: ")))
without_stage = json.loads(subprocess.check_output(
    [sys.executable, "scripts/lib/work_discovery.py", "collect", "--repo", repo], text=True))
assert without_stage["sections"]["ci"]["coverage"]["stage_timing"] == "unavailable", without_stage
assert without_stage["sections"]["ci"]["coverage"]["stage_timing_observations"] == 0, without_stage
import sys
sys.path.insert(0, "scripts/lib")
import panel_results, work_discovery
import panel_metrics, tempfile
# An exit-6 no-change attempt is counted under its own reason and outside every
# failing stage; a genuine delegate failure keeps the stage it failed in.
def _write_attempt(attempt_id, reason):
    return {"attempt_id": attempt_id, "tool": "peer-delegate", "state": "failed", "reason_code": reason,
            "updated_at": "2026-10-26T00:00:00Z", "task_id": "t-" + attempt_id,
            "refs": {"panel_role": "worker", "panel_access": "write"}}
with tempfile.TemporaryDirectory() as empty_repo:
    original_history = panel_metrics._attempt_history
    panel_metrics._attempt_history = lambda _repo: (
        [_write_attempt("a-nochange", "no_change"), _write_attempt("a-failed", "delegate_failed")], False)
    try:
        old_state = pathlib.Path(empty_repo) / ".oms/hooks/sessions" / (("a" * 32) + ".direct-work.json")
        old_state.parent.mkdir(parents=True)
        old_state.write_text('{"counts":{"edit":1}}')
        report = panel_metrics.aggregate(empty_repo, now=1792972800)
        assert ("direct" + "_work") not in report, report
        assert "main direct work" not in panel_metrics.metrics_text(report)
        assert old_state.read_text() == '{"counts":{"edit":1}}'
        counted = report["write_attempts"]
    finally:
        panel_metrics._attempt_history = original_history
failed_data = counted["failed_and_undecided"]
assert failed_data["reason_code"] == {"no_change": 1, "delegate_failed": 1}, counted
assert failed_data["first_failing_stage"] == {"none": 1, "unknown": 1}, counted
# Coordination uses declared scopes, digest-bound gates, provider usage and
# the same retained owner decisions as decisions_by_model; never infer a writer
# from the most recent attempt or turn absent usage into a zero-cost handoff.
from unittest.mock import patch
def coordination_attempt(ident, task, start, end, provider="codex", role="builder", access="write"):
    def stamp(second):
        return "2026-10-26T00:00:%02dZ" % second
    return {"attempt_id": ident, "task_id": task, "tool": "peer-delegate",
            "provider": provider, "state": "done", "terminal": True,
            "created_at": stamp(start), "updated_at": stamp(end),
            "transitions": [{"state": "running", "ts": stamp(start)},
                            {"state": "done", "ts": stamp(end)}],
            "refs": {"panel_role": "worker" if access == "write" else "reviewer",
                     "panel_access": access, "panel_purpose": role, "panel_model": "fixture"}}
with tempfile.TemporaryDirectory() as metrics_repo:
    root = pathlib.Path(metrics_repo)
    (root / ".oms/artifacts").mkdir(parents=True)
    (root / ".oms/plan").mkdir()
    attempt_rows = [coordination_attempt("w1", "alpha", 0, 10),
                    coordination_attempt("w2", "beta", 5, 15),
                    coordination_attempt("w3", "gamma", 5, 15),
                    coordination_attempt("w4", "delta", 5, 15),
                    coordination_attempt("w5", "alpha", 20, 30),
                    coordination_attempt("w6", "alpha", 30, 40),
                    coordination_attempt("r1", "alpha", 10, 12, "claude", "reviewer", "read"),
                    coordination_attempt("r2", "beta", 15, 17, "codex", "reviewer", "read"),
                    coordination_attempt("r3", "gamma", 15, 17, "codex", "reviewer", "read")]
    plan_tasks = [{"id": task, "state": "done", "allowed_paths": paths}
                  for task, paths in (("alpha", ["src"]), ("beta", ["src/file.py"]),
                                      ("gamma", ["src-other"]), ("delta", []))]
    (root / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": plan_tasks}))
    artifact_rows = [{"kind": "delegate", "attempt_id": ident, "task_id": task,
                      "patch_sha256": ident, "provider_usage": {
                          "input_tokens": units, "cache_read_tokens": cache}}
                     for ident, task, units, cache in (("w1", "alpha", 80, 20),
                                                       ("w2", "beta", 0, 0),
                                                       ("w3", "gamma", 10, 0),
                                                       ("w5", "alpha", 120, 30))]
    for ident, task, writer, verdict in (("r1", "alpha", "w1", "fail"),
                                         ("r2", "beta", "w2", "pass"),
                                         ("r3", "gamma", "missing", "fail")):
        artifact_rows.append({"kind": "review-outcome", "attempt_id": ident, "task_id": task,
                              "ts": next(a["updated_at"] for a in attempt_rows if a["attempt_id"] == ident),
                              "review": {"candidate": {"patch_sha256": writer}, "gate_verify_exit": 0,
                                         "seats": [{"provider": "claude" if ident == "r1" else "codex",
                                                    "verdict": verdict}]}})
    artifact_rows.extend({"kind": "panel-result", "task_id": "alpha", "ts": stamp}
                         for stamp in ("2026-10-26T00:00:00Z", "2026-10-26T00:00:41Z"))
    def collect_coordination(rows=attempt_rows, artifacts=artifact_rows, since=None, owner_result=None, min_samples=3):
        (root / ".oms/artifacts/index.jsonl").write_text(
            "".join(json.dumps(row) + "\n" for row in artifacts))
        with patch.object(panel_metrics, "_attempt_history", return_value=(rows, False)), \
                patch.object(panel_results, "outcomes", return_value={
                    "alpha": "accepted", "beta": "rejected", "gamma": "accepted"}), \
                patch.object(panel_results, "_indexed", return_value=json.dumps(owner_result or {
                    "kind": "oms-panel-result", "task_id": "alpha", "outcome": "accepted",
                    "attempt_ids": ["w1", "w5", "w6"],
                    "superseded_attempts": [{"attempt_id": "w1"}]}).encode()):
            return panel_metrics.aggregate(root, since=since, now=1792972850, min_samples=min_samples)
    measured = collect_coordination()
    coordination = measured["coordination"]
    duplicate = coordination["duplicate_work"]
    assert (duplicate["numerator"], duplicate["denominator"], duplicate["unknown"]) == (1, 3, 3), duplicate
    cross = coordination["cross_catch"]
    assert cross["different_provider"]["reviews"] == 1, cross
    assert cross["different_provider"]["blocking_verdicts"] == 1, cross
    assert cross["different_provider"]["confirmed_catches"] == 1, cross
    assert cross["same_provider"]["reviews"] == 1 and cross["unknown"] == 1, cross
    handoff = coordination["handoff_rework"]
    assert (handoff["median"], handoff["n"], handoff["unknown"]) == (1.5, 1, 1), handoff
    cell = coordination["outcome_by_provider_role"]["codex"]["builder"]
    assert (cell["accepted"], cell["rejected"], cell["total"], cell["unknown"]) == (2, 1, 3, 1), cell
    text = panel_metrics.metrics_text(measured)
    assert "codex/builder accepted=2 rejected=1 total=3 unknown=1" in text, text
    # Route history: same retained rows grouped by role/provider/model/effort, derived not stored.
    route = measured["route_history"]
    (group,) = route["groups"]
    assert (group["role"], group["effort"], group["attempts"], group["tasks"], group["decided"]) == (
        "builder", "unrecorded", 6, 4, 3), group
    assert (group["admitted"], group["rejected"], group["reworked"], group["admit_rate"]) == (2, 1, 1, 0.6667), group
    assert group["median_tokens"] is None, group
    assert route["suggest"]["builder"]["model"] == "fixture" and route["hidden_groups"] == 0, route
    assert "suggest builder: codex/fixture" in text and "not a decision" in text, text
    sparse = collect_coordination(min_samples=4)["route_history"]
    assert sparse["groups"] == [] and sparse["hidden_groups"] == 1 and sparse["suggest"] == {}, sparse
    assert "hidden under --min-samples 4: 1" in panel_metrics.metrics_text(collect_coordination(min_samples=4))
    effort_rows = [dict(row, refs=dict(row["refs"], panel_effort="high"), usage_tokens=100 * (n + 1), usage_reports=1)
                   if row["refs"]["panel_access"] == "write" else row for n, row in enumerate(attempt_rows)]
    tuned = collect_coordination(effort_rows)["route_history"]["groups"][0]
    assert tuned["effort"] == "high" and tuned["median_tokens"] == 350 and tuned["token_reports"] == 6, tuned
    # Fold audit and per-role model preference come from the same rows; they nominate, never decide.
    import route_stats
    audit = measured["role_fold_audit"]
    assert [(r["role"], r["attempts"], r["decided"], r["admit_rate"]) for r in audit["roles"]] == [
        ("builder", 6, 3, 0.6667)], audit
    assert "builder" not in audit["never_used"] and "reviewer" in audit["never_used"], audit
    assert audit["pairs"] == [] and measured["model_preference"]["roles"] == {}, measured
    assert "fold candidates (reported history, not a decision" in text and "fold never used in window:" in text, text
    assert "model preference: no role has two groups" in text, text
    def fold_row(role, task, bucket, model="m1", tokens=None):
        return {"role": role, "provider": "codex", "model": model, "effort": None,
                "task": task, "bucket": bucket, "tokens": tokens}
    fold_rows = [fold_row("builder", "t%d" % n, "accepted") for n in range(4)]
    fold_rows += [fold_row("documenter", "t%d" % n, "accepted") for n in range(3)]
    fold_rows += [fold_row("critic", "t0", "accepted"), fold_row("critic", "x1", "rejected")]
    pairs = route_stats.role_fold_audit(fold_rows, 3, ["builder", "critic", "documenter", "scholar"])
    assert pairs["pairs"] == [{"roles": ["builder", "documenter"], "shared_tasks": 3,
                               "rarer_role_tasks": 3, "co_occurrence": 1.0}], pairs
    assert pairs["never_used"] == ["scholar"], pairs
    assert [r["admit_rate"] for r in pairs["roles"] if r["role"] == "critic"] == [None], pairs
    assert route_stats.role_fold_audit(fold_rows, 4, [])["pairs"] == [], "rarer role below min-samples"
    split = [fold_row("builder", "a%d" % n, "accepted" if n < 4 else "rejected", "big", 900) for n in range(5)]
    split += [fold_row("builder", "b%d" % n, "accepted" if n < 2 else "rejected", "small", 100) for n in range(5)]
    pref = route_stats.model_preference(split, 3)["roles"]["builder"]
    assert pref["gap"] == 0.4 and pref["clear"] and pref["groups"][0]["model"] == "big", pref
    assert pref["groups"][0]["median_tokens"] == 900 and pref["groups"][1]["median_tokens"] == 100, pref
    close = [fold_row("builder", "a%d" % n, "accepted" if n < 4 else "rejected", "big", 900) for n in range(5)]
    close += [fold_row("builder", "b%d" % n, "accepted" if n < 3 else "rejected", "small", 100) for n in range(4)]
    close += [fold_row("builder", "b4", "accepted", "small", 100)]
    same = route_stats.model_preference(close, 3)["roles"]["builder"]
    assert not same["clear"], same
    quiet = {"model_preference": route_stats.model_preference(close, 3), "role_fold_audit": None}
    assert "no clear difference" in "\n".join(route_stats.fold_text(quiet)), quiet
    assert route_stats.model_preference(split, 6)["roles"] == {}, "groups under min-samples are dropped"
    # No write pair, no second round and no review is a missing denominator,
    # rather than a 0% rate. Small provider/role cells stay in JSON only.
    empty = collect_coordination([], [])
    assert empty["coordination"]["duplicate_work"]["rate"] is None, empty
    assert empty["coordination"]["handoff_rework"]["median"] is None, empty
    assert "duplicate_work=0/0 (none)" in panel_metrics.metrics_text(empty), empty
    single = collect_coordination(attempt_rows[:1], [])
    assert "hidden smaller cells=1" in panel_metrics.metrics_text(single), single
    # The strict interval boundary is not concurrency; equal-path and root
    # declarations overlap, while a string prefix without a slash does not.
    boundary = collect_coordination([attempt_rows[0], dict(attempt_rows[1],
                                   created_at=attempt_rows[0]["updated_at"], transitions=[])], [])
    assert boundary["coordination"]["duplicate_work"]["denominator"] == 0, boundary
    recent = collect_coordination(since="10s")
    assert recent["coordination"]["duplicate_work"]["denominator"] == 0, recent
    assert recent["coordination"]["cross_catch"]["different_provider"]["reviews"] == 0, recent
    assert recent["coordination"]["handoff_rework"]["n"] == 0, recent
    malformed = [dict(attempt_rows[0], created_at="invalid", transitions=[]), attempt_rows[1]]
    assert collect_coordination(malformed, [])["coordination"]["duplicate_work"]["unknown"] == 1
    zero = [attempt_rows[1], dict(attempt_rows[4], task_id="beta")]
    assert collect_coordination(zero)["coordination"]["handoff_rework"]["unknown"] == 1
    bad_usage = [dict(row, provider_usage={"input_tokens": "bad", "cache_read_tokens": 0})
                 if row.get("attempt_id") == "w1" else row for row in artifact_rows]
    assert collect_coordination(artifacts=bad_usage)["coordination"]["handoff_rework"]["n"] == 0
    # Old gates without lifecycle linkage still contribute unknowns. The seat
    # provider and candidate digest suffice when both are retained, even if the
    # reviewer attempt has aged out of the lifecycle tail.
    legacy = [{"kind": "review-outcome", "ts": "2026-10-26T00:00:12Z"}]
    assert collect_coordination([], legacy)["coordination"]["cross_catch"]["unknown"] == 1
    gated = [dict(row, attempt_id="expired") if row.get("kind") == "review-outcome" else row
             for row in artifact_rows]
    assert collect_coordination(artifacts=gated)["coordination"]["cross_catch"]["different_provider"]["reviews"] == 1
    for bad_verdict in (None, "no-verdict", "malformed"):
        malformed_gate = [dict(row, review=dict(row["review"], seats=[{
            "provider": "claude", "verdict": bad_verdict}])) if row.get("attempt_id") == "r1" else row
                          for row in artifact_rows]
        entry = collect_coordination(artifacts=malformed_gate)["coordination"]["cross_catch"]["different_provider"]
        assert entry["reviews"] == 0 and entry["unknown"] == 1, entry
    for failed_verify in (None, 1, "0", False):
        failed_gate = [dict(row, review=dict(row["review"], gate_verify_exit=failed_verify))
                       if row.get("attempt_id") == "r1" else row for row in artifact_rows]
        assert collect_coordination(artifacts=failed_gate)["coordination"]["cross_catch"]["different_provider"]["unknown"] == 1
    unresolved = [row for row in artifact_rows if row.get("kind") != "panel-result"]
    entry = collect_coordination(artifacts=unresolved)["coordination"]["cross_catch"]["different_provider"]
    assert entry["catch_unknown"] == 1 and entry["confirmed_catch_rate"]["rate"] is None, entry
    # An already-created parallel write round is not a later correction, and
    # identical bytes written by different providers make attribution unknown.
    before_gate = [dict(row, created_at="2026-10-26T00:00:01Z") if row["attempt_id"] in {"w5", "w6"} else row
                   for row in attempt_rows]
    assert collect_coordination(before_gate)["coordination"]["cross_catch"]["different_provider"]["confirmed_catches"] == 0
    ambiguous = artifact_rows + [{"kind": "delegate", "attempt_id": "w5", "task_id": "alpha", "patch_sha256": "w1"}]
    other_provider = [dict(attempt_rows[0], attempt_id="w5", provider="claude")
                      if row["attempt_id"] == "w5" else row for row in attempt_rows]
    assert collect_coordination(other_provider, ambiguous)["coordination"]["cross_catch"]["unknown"] == 2
    assert collect_coordination(artifacts=ambiguous)["coordination"]["cross_catch"]["different_provider"]["reviews"] == 1
    # Equal and root path declarations overlap; absent/malformed declarations
    # remain unknown. A zero follow-up over a nonzero baseline is a valid ratio.
    for paths, expected in ((["src"], 1), (["."], 1), (["../invalid"], 0), ([""], 0), ("src", 0)):
        plan_tasks[1]["allowed_paths"] = paths
        (root / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": plan_tasks}))
        duplicate = collect_coordination(attempt_rows[:2], [])["coordination"]["duplicate_work"]
        assert duplicate["numerator"] == expected, duplicate
        assert duplicate["unknown"] == (0 if expected else 1), duplicate
    plan_tasks[1]["allowed_paths"] = ["src/file.py"]
    (root / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": plan_tasks}))
    zero_followup = [dict(row, provider_usage={"input_tokens": 0, "cache_read_tokens": 0})
                    if row.get("attempt_id") == "w5" else row for row in artifact_rows]
    assert collect_coordination(artifacts=zero_followup)["coordination"]["handoff_rework"]["median"] == 0
    for value in (-1, True, None):
        invalid_usage = [dict(row, provider_usage={"input_tokens": value, "cache_read_tokens": 0})
                         if row.get("attempt_id") == "w1" else row for row in artifact_rows]
        assert collect_coordination(artifacts=invalid_usage)["coordination"]["handoff_rework"]["unknown"] == 2
    two_ratios = artifact_rows + [{"kind": "delegate", "task_id": "alpha", "attempt_id": "w6",
                                  "provider_usage": {"input_tokens": 100, "cache_read_tokens": 0}}]
    assert collect_coordination(artifacts=two_ratios)["coordination"]["handoff_rework"]["median"] == 1.25
    for owner_result in ({"outcome": "malformed"}, {"outcome": "accepted"}, {"outcome": "rejected"}):
        owner_result.update(kind="oms-panel-result", task_id="alpha")
        entry = collect_coordination(owner_result=owner_result)["coordination"]["cross_catch"]["different_provider"]
        assert entry["confirmed_catches"] == 0, entry
        assert entry["catch_unknown"] == (owner_result["outcome"] != "rejected"), entry
    with patch.object(panel_results, "_worker_owns", return_value=["."]):
        duplicate = collect_coordination(attempt_rows[:3], [])["coordination"]["duplicate_work"]
        assert duplicate["numerator"] == duplicate["denominator"] == 3, duplicate
    for row in (dict(attempt_rows[0], created_at="2026-10-26T00:00:20Z", transitions=[]),
                dict(attempt_rows[0], task_id=""),
                dict(attempt_rows[0], state="queued", transitions=[])):
        assert collect_coordination([row, attempt_rows[1]], [])["coordination"]["duplicate_work"]["unknown"] == 1
    for at_boundary in ("2026-10-26T00:00:05Z", "2026-10-26T00:00:15Z"):
        no_interval = dict(attempt_rows[0], created_at=at_boundary, updated_at=at_boundary, transitions=[])
        assert collect_coordination([no_interval, attempt_rows[1]], [])["coordination"]["duplicate_work"]["denominator"] == 0
    assert collect_coordination([attempt_rows[0], dict(attempt_rows[1], task_id="alpha")], [])["coordination"]["duplicate_work"]["denominator"] == 0
indexed_reader = panel_results._indexed
attempts = [
    {"attempt_id": "a-decided", "refs": {"panel_role": "reviewer"}},
    {"attempt_id": "a-pending", "refs": {"panel_role": "critic"}},
]
rows = [
    {"attempt_id": "a-decided", "task_id": "done-task", "artifact_id": "r-decided", "kind": "review", "artifact": ".oms/artifacts/decided"},
    {"attempt_id": "a-pending", "task_id": "open-task", "artifact_id": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "kind": "review", "artifact": ".oms/artifacts/pending"},
]
panel_results._records = lambda *args, **kwargs: (attempts, rows)
panel_results.outcomes = lambda repo, ids: {"done-task": "accepted"}
artifact = (b"## Prompt\n```text\nquoted prompt\n## Findings\nnot a result\n"
            b"## Output\nGATE: fail\n## Findings\nActual finding\n## Exit\n0\n")
panel_results._indexed = lambda repo, row: artifact
reviews = work_discovery._collect_reviews("unused")
assert reviews["coverage"]["considered"] == 2 and reviews["coverage"]["selected"] == 1, reviews
assert len(reviews["items"]) == 1, reviews
assert re.fullmatch(r"[0-9a-f]{16}", reviews["items"][0]["ref"]), reviews
assert reviews["items"][0]["ref"] == "0123456789abcdef", reviews
assert reviews["items"][0]["artifact_id"] == "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", reviews
assert reviews["items"][0]["findings"] == "Actual finding", reviews
assert reviews["items"][0]["gate"] == "fail" and "outcome" not in reviews["items"][0], reviews
panel_results._indexed = indexed_reader
import contextlib, hashlib, io, shutil, tempfile
from pathlib import Path
oversized_repo = Path(tempfile.mkdtemp())
artifact_path = oversized_repo / ".oms" / "artifacts" / "pending"
artifact_path.parent.mkdir(parents=True)
artifact_bytes = (b"## Output\n## Findings\nActual finding\n" +
                  b"x" * (panel_results.MAX_RESULT + 1) + b"\n## Exit\n0\n")
artifact_path.write_bytes(artifact_bytes)
rows[1]["artifact_sha256"] = hashlib.sha256(artifact_bytes).hexdigest()
captured_stderr = io.StringIO()
with contextlib.redirect_stderr(captured_stderr):
    unreadable = work_discovery._collect_reviews(str(oversized_repo))
assert captured_stderr.getvalue() == "", captured_stderr.getvalue()
assert unreadable["coverage"]["complete"] is False, unreadable
assert unreadable["coverage"]["unreadable_results"] == 1, unreadable
shutil.rmtree(oversized_repo)
import subprocess
panel_results.outcomes = lambda repo, ids: (_ for _ in ()).throw(
    subprocess.TimeoutExpired("owner outcomes", 30))
timed_out = work_discovery._collect_reviews("unused")
assert timed_out["status"] == "unavailable" and timed_out["reason"] == "timeout", timed_out
print(json.dumps(r, ensure_ascii=False, separators=(",", ":")))
PY
  # The full packet example is captured by the test's JSON assertion above.

  mv "$readers/fail-ledger.sh" "$readers/fail-ledger.off"
  out="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo")" ||
    fail "missing optional fail ledger should remain collectable"
  OMS_TEST_PACKET="$out" python3 - <<'PY' || fail "missing fail ledger was not labelled unavailable"
import json, os
assert json.loads(os.environ["OMS_TEST_PACKET"])["sections"]["failures"]["status"] == "unavailable"
PY

  cat > "$readers/fail-ledger.sh" <<'EOF'
#!/usr/bin/env bash
python3 -c 'import json; print(json.dumps({"failures": [{"fingerprint": "f", "summary": "é" * 2500}]}))'
EOF
  chmod +x "$readers/fail-ledger.sh"
  out="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo" 2>"$TMP/discovery-err")" ||
    fail "multibyte summary should be bounded before the section budget check"
  OMS_TEST_PACKET="$out" python3 - <<'PY' || fail "multibyte summary was not truncated by characters"
import json, os
summary = json.loads(os.environ["OMS_TEST_PACKET"])["sections"]["failures"]["items"][0]["summary"]
assert len(summary) == 240 and summary.endswith("..."), summary
PY

  cat > "$readers/fail-ledger.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$OMS_TEST_LEDGER_JSON"
EOF
  ROOT="$ROOT" REPO="$repo" python3 - <<'PY' || fail "work discovery bounds or producer keys regressed"
import json, os, sys
sys.path.insert(0, os.environ["ROOT"] + "/scripts/lib")
import work_discovery
work_discovery._collect_land_failures = lambda repo: ([], {"land_scan_complete": True}, [])
repo = os.environ["REPO"]
def packet(ledger, many=False, metrics=None):
    os.environ["OMS_TEST_LEDGER_JSON"] = json.dumps(ledger)
    os.environ.pop("OMS_TEST_CI_MANY", None)
    os.environ.pop("OMS_TEST_METRICS_JSON", None)
    if many:
        os.environ["OMS_TEST_CI_MANY"] = "1"
    if metrics is not None:
        os.environ["OMS_TEST_METRICS_JSON"] = json.dumps(metrics)
    return work_discovery.collect(repo)["sections"]
row = {"fingerprint": "f", "count": 1}
clean = packet({"schema": 1, "failures": [row], "invalid_rows": 0})
assert clean["failures"]["coverage"]["complete"] is True, clean["failures"]
assert "invalid_rows" not in clean["failures"]["coverage"], clean["failures"]
bad = packet({"schema": 1, "failures": [row], "invalid_rows": 3})
assert bad["failures"]["coverage"]["complete"] is False, bad["failures"]
assert bad["failures"]["coverage"]["invalid_rows"] == 3, bad["failures"]
alias = packet({"schema": 1, "items": [row]})
assert alias["failures"]["items"] == [] and alias["failures"]["status"] == "unavailable", alias["failures"]
assert "incomplete_reason" not in clean["metrics"]["items"][0], clean["metrics"]
metrics = packet({"failures": []}, metrics={"incomplete": True, "incomplete_reason": "plan evidence malformed"})
assert metrics["metrics"]["items"][0]["incomplete_reason"] == "plan evidence malformed", metrics["metrics"]
many = packet({"failures": []}, many=True)["ci"]
refs = [item["ref"] for item in many["items"]]
assert "test:test_x" in refs, refs
assert many["coverage"]["complete"] is False and many["coverage"]["failing_rows_omitted"] > 0, many
assert len(json.dumps(many, separators=(",", ":")).encode()) <= work_discovery.SECTION_BUDGETS["ci"], many
kept = sum(ref.startswith("job:job-") for ref in refs)
assert kept + many["coverage"]["failing_rows_omitted"] == 13, many
PY

  cat > "$readers/agent-plan.sh" <<'EOF'
#!/usr/bin/env bash
python3 - <<'PY'
import json
print(json.dumps([{"id": "overflow", "state": "ready", "title": "é" * 2100}], ensure_ascii=False))
PY
EOF
  chmod +x "$readers/agent-plan.sh"
  rc=0
  python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo" >"$TMP/discovery-out" 2>"$TMP/discovery-err" || rc=$?
  [ "$rc" = 2 ] && grep -Fq 'work-discovery: tasks is ' "$TMP/discovery-err" ||
    fail "multibyte tasks section overflow did not name tasks and exit 2"
  cat > "$readers/agent-plan.sh" <<'EOF'
#!/usr/bin/env bash
printf '[{"id":"open-1","state":"ready"},{"id":"done-1","state":"done"}]\r\n'
EOF
  chmod +x "$readers/agent-plan.sh"

  cat > "$readers/fail-ledger.sh" <<'EOF'
#!/usr/bin/env bash
printf 'helper failed\n' >&2
exit 7
EOF
  chmod +x "$readers/fail-ledger.sh"
  out="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo" 2>"$TMP/discovery-err")" ||
    fail "optional helper failure should remain collectable"
  OMS_TEST_PACKET="$out" python3 - <<'PY' || fail "optional helper failure omitted exit detail"
import json, os
reason = json.loads(os.environ["OMS_TEST_PACKET"])["sections"]["failures"]["reason"]
assert "exited 7" in reason and "helper failed" in reason, reason
PY

  cat > "$readers/agent-plan.sh" <<'EOF'
#!/usr/bin/env bash
printf '{broken json}\n'
EOF
  rc=0
  python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo" >"$TMP/discovery-out" 2>"$TMP/discovery-err" || rc=$?
  [ "$rc" = 3 ] && grep -Fq 'tasks' "$TMP/discovery-err" ||
    fail "corrupt plan JSON did not name tasks and exit 3"

  cat > "$readers/agent-plan.sh" <<'EOF'
#!/usr/bin/env bash
printf '[]\n'
EOF
  cat > "$readers/agent-memory.sh" <<'EOF'
#!/usr/bin/env bash
printf 'hypothesis: pipe shape | evidence | effect | DEFERRED | round 1\n'
printf 'hypothesis: verdict shape | evidence: notes | verdict: ReJeCtEd | round 2\n'
printf 'hypothesis: option | evidence: earlier verdict: rejected was overturned | effect: pending | deferred | round 3\n'
printf 'hypothesis: other | evidence: once verdict: rejected | verdict: Adopted | round 4\n'
EOF
  chmod +x "$readers/agent-plan.sh" "$readers/agent-memory.sh"
  out="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo")" ||
    fail "verdict discovery collection failed"
  repeated="$(python3 "$ROOT/scripts/lib/work_discovery.py" collect --repo "$repo")" ||
    fail "repeated verdict discovery collection failed"
  python3 - "$out" "$repeated" <<'PY' || fail "verdicts or stable metrics reference missing"
import json, re, sys
packet, repeated = [json.loads(value) for value in sys.argv[1:]]
items = packet["sections"]["rejected"]["items"]
assert [item["verdict"] for item in items] == ["deferred", "rejected", "deferred", "adopted"], items
assert packet["sections"]["rejected"]["coverage"]["verdict_counts"] == {
    "deferred": 2, "rejected": 1, "adopted": 1}, packet
metric = packet["sections"]["metrics"]["items"][0]
assert re.fullmatch(r"metrics-[0-9a-f]{12}", metric["ref"]), metric
assert repeated["sections"]["metrics"]["items"][0]["ref"] == metric["ref"], repeated
PY
}

test_managed_landing_capture_retention
test_work_discovery_packet_is_bounded_and_complete

test_native_hook_telemetry_is_content_free_and_correlated
test_ci_tick_records_once_and_skips_a_fresh_sha
test_inbox_ranks_state_and_applies_only_safe_repairs
test_inbox_fix_safe_reports_ci_refresh_only_when_ledger_changed
test_unresolved_queue_triages_by_patch_bytes_and_clears_in_one_batch
test_memory_citations_revalidate_and_stay_out_of_default_context
test_memory_supersession_and_retirement
test_memory_redaction_and_native_sources
test_memory_hygiene_report_is_read_only
test_checkpoint_restores_staged_and_unstaged_content_with_a_backup
test_checkpoint_restores_from_unstaged_only_with_stale_stat

test_checkpoint_ignores_diff_prefix_and_color_config
test_checkpoint_rejects_intent_to_add_without_mutation
test_checkpoint_restore_refuses_current_intent_to_add_without_mutation
test_checkpoint_rejects_legacy_intent_to_add_snapshot_without_mutation
test_hook_installers_keep_telemetry_off_the_tool_hot_path
test_gc_bounds_old_checkpoint_and_hook_state

echo "functional evolution smoke: ok"
