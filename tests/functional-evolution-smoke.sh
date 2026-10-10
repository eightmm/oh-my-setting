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

test_review_uptake_withholds_rates_until_both_cohorts_are_large_enough() {
  local repo="$TMP/review-uptake"
  local report

  make_repo "$repo"
  mkdir -p "$repo/.oms/artifacts"
  # One delegation per cohort: far below the floor, so the report owes counts
  # and no rate at all.
  cat > "$repo/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_f1","operation_id":"op_f1","artifact_id":"sha256:f1","ts":"2026-08-03T00:00:01Z","kind":"delegate","provider":"codex","exit":0,"verify_exit":0,"prompt_sha256":"p1","source":".oms/artifacts/review-1.md","tokens":10,"duration_s":1.0}
{"schema":1,"event_id":"evt_d1","operation_id":"op_d1","artifact_id":"sha256:d1","ts":"2026-08-03T00:00:02Z","kind":"delegate","provider":"codex","exit":0,"prompt_sha256":"p2","tokens":5,"duration_s":5.0}
EOF
  report="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" --json telemetry --review-uptake)" ||
    fail "review-uptake telemetry failed"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "undersized cohorts were reported as rates"
import json, os
block = json.loads(os.environ["OMS_TEST_REPORT"])["review_uptake"]
assert block["status"] == "insufficient-data", block
assert block["minimum_sample"] == 5 and block["delegations"] == 2, block
cohorts = {entry["cohort"]: entry for entry in block["cohorts"]}
assert set(cohorts) == {"review-fed", "direct"}, block
for entry in block["cohorts"]:
    assert entry["status"] == "insufficient-data", entry
    assert entry["size"] == 1 and entry["recorded_exit"]["zero"] == 1, entry
    assert entry["recorded_exit_zero_rate"] is None, entry
    assert entry["verifier_coverage_rate"] is None, entry
    assert entry["verifier_exit_zero_rate"] is None, entry
    assert entry["wall_seconds"]["median"] is None, entry
assert cohorts["review-fed"]["wall_seconds"]["total"] == 1.0, block
PY

  # Enough rows for both cohorts, one review fed from outside the repo, plus a
  # non-delegate row that must stay out of the partition.
  cat >> "$repo/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_f2","operation_id":"op_f2","artifact_id":"sha256:f2","ts":"2026-08-03T00:00:03Z","kind":"delegate","provider":"codex","exit":0,"verify_exit":0,"prompt_sha256":"p3","source":".oms/artifacts/review-2.md","tokens":30,"duration_s":3.0}
{"schema":1,"event_id":"evt_f3","operation_id":"op_f3","artifact_id":"sha256:f3","ts":"2026-08-03T00:00:04Z","kind":"delegate","provider":"codex","exit":0,"prompt_sha256":"p4","source":".oms/artifacts/review-3.md"}
{"schema":1,"event_id":"evt_f4","operation_id":"op_f4","artifact_id":"sha256:f4","ts":"2026-08-03T00:00:05Z","kind":"delegate","provider":"codex","exit":0,"prompt_sha256":"p5","source_external":{"name":"review-4.md","owned":false,"sha256":"deadbeef"}}
{"schema":1,"event_id":"evt_f5","operation_id":"op_f5","artifact_id":"sha256:f5","ts":"2026-08-03T00:00:06Z","kind":"delegate","provider":"codex","exit":1,"prompt_sha256":"p6","source":".oms/artifacts/review-5.md"}
{"schema":1,"event_id":"evt_d2","operation_id":"op_d2","artifact_id":"sha256:d2","ts":"2026-08-03T00:00:07Z","kind":"delegate","provider":"codex","exit":0,"prompt_sha256":"p7"}
{"schema":1,"event_id":"evt_d3","operation_id":"op_d3","artifact_id":"sha256:d3","ts":"2026-08-03T00:00:08Z","kind":"delegate","provider":"codex","exit":0,"prompt_sha256":"p8"}
{"schema":1,"event_id":"evt_d4","operation_id":"op_d4","artifact_id":"sha256:d4","ts":"2026-08-03T00:00:09Z","kind":"delegate","provider":"codex","exit":1,"prompt_sha256":"p9"}
{"schema":1,"event_id":"evt_d5","operation_id":"op_d5","artifact_id":"sha256:d5","ts":"2026-08-03T00:00:10Z","kind":"delegate","provider":"codex","exit":1,"verify_exit":1,"prompt_sha256":"p10"}
{"schema":1,"event_id":"evt_c1","operation_id":"op_c1","artifact_id":"sha256:c1","ts":"2026-08-03T00:00:11Z","kind":"call","provider":"codex","exit":0,"source":".oms/artifacts/review-6.md"}
EOF
  report="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" --json telemetry --review-uptake)" ||
    fail "review-uptake telemetry failed on the larger window"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "cohort figures are wrong"
import json, os
report = json.loads(os.environ["OMS_TEST_REPORT"])
block = report["review_uptake"]
assert block["status"] == "reported", block
assert block["basis"] == "observational, mechanical-only", block
# Only delegations are partitioned; the call row stays in the wider report.
assert block["delegations"] == 10 and report["operations"]["eligible"] == 11, block
cohorts = {entry["cohort"]: entry for entry in block["cohorts"]}
fed, direct = cohorts["review-fed"], cohorts["direct"]
assert fed["size"] == 5 and direct["size"] == 5, block
assert fed["status"] == "reported" and direct["status"] == "reported", block
assert fed["recorded_exit_zero_rate"] == 0.8, fed
assert direct["recorded_exit_zero_rate"] == 0.6, direct
assert fed["verifier_coverage_rate"] == 0.4, fed
assert fed["verifier_exit_zero_rate"] == 1.0, fed
assert direct["verifier_coverage_rate"] == 0.2, direct
assert direct["verifier_exit_zero_rate"] == 0.0, direct
# The externally recorded review counts as fed and is the only hashed source.
assert fed["lineage"] == {"operation_id": 5, "prompt_hash": 5, "source_hash": 1}, fed
assert direct["lineage"]["source_hash"] == 0, direct
# Cached row metrics, so a pruned artifact cannot erase them.
assert fed["wall_seconds"] == {"median": 2.0, "reports": 2, "total": 4.0}, fed
assert fed["provider_reported_tokens"] == {"reports": 2, "total": 40}, fed
# An exit zero is not semantic task success, and nothing here may say it is.
assert "success_rate" not in json.dumps(block), block
PY

  report="$("$ROOT/scripts/artifact-index.sh" --repo "$repo" --json telemetry)" ||
    fail "default telemetry failed"
  OMS_TEST_REPORT="$report" python3 - <<'PY' || fail "omitting --review-uptake changed the default report"
import json, os
assert "review_uptake" not in json.loads(os.environ["OMS_TEST_REPORT"])
PY

  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" --review-uptake list >/dev/null 2>&1; then
    fail "--review-uptake was accepted outside telemetry"
  fi
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
  git -C "$repo" diff --cached --name-only -- new-empty.txt | grep -Fxq new-empty.txt ||
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

# Native Windows Python emits CRLF on stdout; parsed Git IDs must lose CR.
test_checkpoint_accepts_crlf_python_output() {
  local shim_dir="$TMP/checkpoint-crlf-bin" real_python="$1"
  mkdir -p "$shim_dir"
  python3 - "$shim_dir/python3" "$real_python" <<'PYSHIM'
import pathlib, shlex, sys
script = "#!/usr/bin/env bash\n" + shlex.quote(sys.argv[2])
script += """ "$@" | awk '{printf "%s\\r\\n", $0}'
exit "${PIPESTATUS[0]}"
"""
pathlib.Path(sys.argv[1]).write_bytes(script.encode("utf-8"))
PYSHIM
  chmod +x "$shim_dir/python3"
  PATH="$shim_dir:$PATH" checkpoint_config_roundtrip "$TMP/checkpoint-crlf" diff.noprefix false
}


test_hook_installers_keep_telemetry_off_the_tool_hot_path() {
  local settings="$TMP/claude-settings.json"
  local home_dir="$TMP/claude-home"

  mkdir -p "$home_dir"
  printf '{}\n' > "$settings"
  HOME="$home_dir" OMS_CLAUDE_SETTINGS="$settings" \
    bash "$ROOT/scripts/install-claude-hooks.sh" >/dev/null
  python3 - "$settings" <<'PY' || fail "Claude telemetry hooks were not installed"
import json, sys
hooks = json.load(open(sys.argv[1], encoding="utf-8"))["hooks"]
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
assert sum(len(entry.get("hooks", [])) for entries in hooks.values() for entry in entries) == 7, hooks
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

def new(name, publish=True):
    capture = pathlib.Path(h['allocate'](str(repo), str(source), 'land-' + name, sha))
    patch = repo / '.oms/landing-patches' / ('land-' + name + '.patch')
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
    assert 'quota' in str(error)
else:
    raise AssertionError('count quota bypassed')
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
    assert h['usage'](str(quota_repo)) == (1, 2 * h['MAX_OBJECT'])
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
assert h['usage'](str(crash_repo)) == (1, 2 * h['MAX_OBJECT'])
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
              'cm.__enter__(); Path(sys.argv[3]).write_text("ready"); time.sleep(2); '
              'cm.__exit__(None,None,None)', str(engine_path)]),
            ('index', repo / '.oms/artifacts/index.jsonl',
             ['bash', '-c',
              '. "$0"; oms_with_file_lock "$1" sh -c \'printf ready > "$1"; sleep 2\' holder "$2"',
              str(lock_lib)])):
        lock_dir = sandbox / ('inner-gc-locks-' + lock_name)
        ready = sandbox / ('inner-gc-lock-ready-' + lock_name)
        lock_env = dict(os.environ, OMS_LOCK_DIR=str(lock_dir), OMS_LOCK_TIMEOUT='1')
        if lock_name == 'lifecycle':
            holder_args = holder_command + [str(lock_target), str(ready)]
        else:
            holder_args = holder_command + [str(lock_target), str(ready)]
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
            if holder.poll() is None:
                holder.terminate()
            holder.communicate(timeout=5)
        ready.unlink()

# Marker uncertainty vetoes the locked collector, independently of age/PID.
(repo / '.oms/delegations').mkdir()
(repo / '.oms/delegations/unknown.json').write_text('{malformed')
h['gc'](str(repo), 0, True)
assert unknown.exists()
(repo / '.oms/delegations/unknown.json').unlink()
subprocess.run(['bash', str(root / 'scripts/agent-events.sh'), '--repo', str(repo), 'start',
                '--provider', 'codex', '--tool', 'capture-fixture', '--then', 'starting'],
               stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
capture4, patch4 = new('active')
h['gc'](str(repo), 0, True)
assert capture4.exists() and patch4.exists(), 'active native attempt did not veto GC'

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
PY
}

test_managed_landing_capture_retention

test_native_hook_telemetry_is_content_free_and_correlated
test_review_uptake_withholds_rates_until_both_cohorts_are_large_enough
test_ci_tick_records_once_and_skips_a_fresh_sha
test_inbox_ranks_state_and_applies_only_safe_repairs
test_inbox_fix_safe_reports_ci_refresh_only_when_ledger_changed
test_unresolved_queue_triages_by_patch_bytes_and_clears_in_one_batch
test_memory_citations_revalidate_and_stay_out_of_default_context
test_checkpoint_restores_staged_and_unstaged_content_with_a_backup
test_checkpoint_restores_from_unstaged_only_with_stale_stat

test_checkpoint_ignores_diff_prefix_and_color_config
test_checkpoint_rejects_intent_to_add_without_mutation
test_checkpoint_restore_refuses_current_intent_to_add_without_mutation
test_checkpoint_rejects_legacy_intent_to_add_snapshot_without_mutation
test_checkpoint_accepts_crlf_python_output "$(command -v python3)"
test_hook_installers_keep_telemetry_off_the_tool_hot_path
test_gc_bounds_old_checkpoint_and_hook_state

echo "functional evolution smoke: ok"
