#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d /tmp/oms-harness-tests.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

write_install_receipt() {
  local path="$1"
  local root="$2"
  local commit="${3:-0123456789abcdef0123456789abcdef01234567}"

  mkdir -p "$(dirname "$path")"
  python3 - "$path" "$root" "$commit" <<'PY'
import json, sys
path, root, commit = sys.argv[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump({
        "schema": 1,
        "source_root": root,
        "commit": commit,
        "channel": "main",
        "version": "0.3.0",
        "installed_at": "2026-07-11T00:00:00Z",
        "plugin": {"name": "oh-my-setting", "version": "0.1.0", "sha256": "sha256"},
    }, handle)
PY
}

test_checkout_runtime_is_current() {
  if grep -RFn 'actions/checkout@v4' "$ROOT/.github/workflows" >/dev/null; then
    fail "workflows still use the deprecated Node 20 checkout runtime"
  fi
  local checkout_count checkout_v7_count
  checkout_count="$(grep -Rh 'uses: actions/checkout@' "$ROOT/.github/workflows" | wc -l | tr -d ' ')"
  checkout_v7_count="$(grep -Rh 'uses: actions/checkout@v7' "$ROOT/.github/workflows" | wc -l | tr -d ' ')"
  [ "$checkout_count" -gt 0 ] && [ "$checkout_count" = "$checkout_v7_count" ] ||
    fail "every checkout step should use actions/checkout@v7"
}

test_smoke_shards_partition_every_definition() {
  local expected="$TMP/expected"
  local combined="$TMP/combined"
  local shard

  awk '
    /^test_[[:alnum:]_]+\(\) \{/ {
      name=$1
      sub(/\(\)$/, "", name)
      print name
    }
  ' "$ROOT/tests/scripts-smoke.sh" | LC_ALL=C sort > "$expected"
  : > "$combined"
  for shard in 1 2 3 4; do
    "$ROOT/tests/run-smoke-shard.sh" --list --shard "$shard/4" > "$TMP/shard-$shard"
    "$ROOT/tests/run-smoke-shard.sh" --list --shard "$shard/4" > "$TMP/shard-$shard-again"
    cmp -s "$TMP/shard-$shard" "$TMP/shard-$shard-again" ||
      fail "smoke shard $shard is not deterministic"
    cat "$TMP/shard-$shard" >> "$combined"
  done
  LC_ALL=C sort "$combined" > "$TMP/actual"
  cmp -s "$expected" "$TMP/actual" || fail "smoke shards omit or invent test definitions"
  [ -z "$(LC_ALL=C sort "$combined" | uniq -d)" ] || fail "smoke shards overlap"
  # The cost weights keep the heaviest tests apart, and the shard holding the
  # heaviest one carries fewer tests to make up for it.
  local heavy_a heavy_b fewest
  heavy_a="$(grep -lx test_worker_guard_rejects_any_worker_authority_state_change "$TMP"/shard-[1-4])"
  heavy_b="$(grep -lx test_goal_drive_parallel_failure_recovery "$TMP"/shard-[1-4])"
  [ -n "$heavy_a" ] && [ "$heavy_a" != "$heavy_b" ] || fail "the two heaviest smoke tests share a shard"
  fewest="$(wc -l "$TMP"/shard-[1-4] | sort -n | sed -n '1p' | awk '{print $1}')"
  [ "$(wc -l < "$heavy_a")" -le "$fewest" ] || fail "the heaviest smoke test's shard is not the lightest by count"
  if "$ROOT/tests/run-smoke-shard.sh" --list --shard 0/4 >/dev/null 2>&1; then
    fail "zero-indexed smoke shard should be rejected"
  fi
  if "$ROOT/tests/run-smoke-shard.sh" --list --shard 5/4 >/dev/null 2>&1; then
    fail "out-of-range smoke shard should be rejected"
  fi
  cat > "$TMP/noncanonical-smoke.sh" <<'EOF'
test_visible() {
  :
}
test_invisible () {
  :
}
# SMOKE_TEST_CALLS_BEGIN
test_visible
EOF
  if OMS_SMOKE_SUITE="$TMP/noncanonical-smoke.sh" \
    "$ROOT/tests/run-smoke-shard.sh" --list >/dev/null 2>&1; then
    fail "noncanonical test definitions should fail closed instead of being omitted"
  fi
  cat > "$TMP/function-style-smoke.sh" <<'EOF'
function test_hidden {
  false
}
# SMOKE_TEST_CALLS_BEGIN
EOF
  if OMS_SMOKE_SUITE="$TMP/function-style-smoke.sh" \
    "$ROOT/tests/run-smoke-shard.sh" --list >/dev/null 2>&1; then
    fail "function-style test definitions should fail closed instead of being omitted"
  fi
  cat > "$TMP/orphaned-smoke-weight.sh" <<'EOF'
# smoke-weight: 240
smoke_helper() {
  :
}
test_visible() {
  :
}
# SMOKE_TEST_CALLS_BEGIN
test_visible
# SMOKE_TEST_CALLS_END
EOF
  if OMS_SMOKE_SUITE="$TMP/orphaned-smoke-weight.sh" \
    "$ROOT/tests/run-smoke-shard.sh" --list >/dev/null 2>&1; then
    fail "smoke weights above helpers should fail closed"
  fi
}

test_artifact_resolution_and_dashboard_statuses() {
  local repo="$TMP/repo"
  local index="$repo/.oms/artifacts/index.jsonl"
  local before
  local resolution_event pid_one pid_two sensitive_reason

  mkdir -p "$repo/.oms/artifacts"
  git -C "$repo" init -q
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_success","operation_id":"op_one","artifact_id":"sha256:success","ts":"2026-07-11T00:00:00Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_fail_a","operation_id":"op_shared","artifact_id":"sha256:faila","ts":"2026-07-11T00:00:01Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_fail_b","operation_id":"op_shared","artifact_id":"sha256:failb","ts":"2026-07-11T00:00:02Z","kind":"call","provider":"claude","exit":2}
EOF

  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve --event-id evt_missing >/dev/null 2>&1; then
    fail "resolving an unknown event should fail"
  fi
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve --event-id evt_success >/dev/null 2>&1; then
    fail "resolving a successful event should fail"
  fi
  printf '%s\n' '{"event_id":"evt_legacy","ts":"2026-07-11T00:00:03Z","kind":"call","provider":"codex","exit":1}' >> "$index"
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve --event-id evt_legacy >/dev/null 2>&1; then
    fail "resolving an unmigrated legacy event should fail"
  fi
  sed '/"evt_legacy"/d' "$index" > "$TMP/index-clean"
  mv "$TMP/index-clean" "$index"

  OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_fail_a --reason "verified transient worker failure" >/dev/null
  before="$(wc -l < "$index" | tr -d ' ')"
  OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_fail_a >/dev/null
  [ "$before" = "$(wc -l < "$index" | tr -d ' ')" ] ||
    fail "artifact resolution should be idempotent"

  python3 - "$index" <<'PY' || fail "resolution event has the wrong lineage"
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1])]
resolutions = [row for row in rows if row.get("kind") == "artifact-resolution"]
assert len(resolutions) == 1
row = resolutions[0]
assert row["parent_event_id"] == "evt_fail_a"
assert row["resolves_event_id"] == "evt_fail_a"
assert row["operation_id"] == "op_shared"
assert row["artifact_id"] == "sha256:faila"
assert row["reason"] == "verified transient worker failure"
PY
  resolution_event="$(python3 - "$index" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    row = json.loads(line)
    if row.get("kind") == "artifact-resolution":
        print(row["event_id"])
        break
PY
)"
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id "$resolution_event" >/dev/null 2>&1; then
    fail "a resolution event should not itself be resolvable"
  fi
  sensitive_reason="$(printf '%s%s=%s%s' 'to' 'ken' 'gh' 'p_abcdefghijklmnopqrstuvwxyz1234567890')"
  if "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve --event-id evt_fail_b \
    --reason "$sensitive_reason" >/dev/null 2>&1; then
    fail "sensitive-looking resolution reasons should be rejected"
  fi

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" unresolved 20 > "$TMP/unresolved"
  grep -Fq 'event=evt_fail_b' "$TMP/unresolved" || fail "sibling failure should remain unresolved"
  if grep -Fq 'event=evt_fail_a' "$TMP/unresolved"; then
    fail "resolved failure should not appear in unresolved output"
  fi
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" latest-run > "$TMP/latest-run"
  grep -Fq 'event=evt_fail_a' "$TMP/latest-run" || fail "latest-run should retain failed outcomes"
  grep -Fq 'status=resolved' "$TMP/latest-run" || fail "latest-run should replay resolution status"
  if grep -Fq 'artifact-resolution' "$TMP/latest-run"; then
    fail "latest-run should not treat resolution events as outcomes"
  fi

  "$ROOT/scripts/state.sh" --repo "$repo" --json > "$TMP/state.json"
  python3 - "$TMP/state.json" <<'PY' || fail "repo-state JSON status counts are wrong"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["total"] == 4
assert state["outcomes_total"] == 3
assert state["counts"] == {"success": 1, "unresolved": 1, "resolved": 1}
assert [row["status"] for row in state["latest"]] == ["success", "resolved", "unresolved"]
PY
  "$ROOT/scripts/state.sh" --repo "$repo" > "$TMP/state.txt"
  grep -Fq 'success=1 unresolved=1 resolved=1' "$TMP/state.txt" ||
    fail "repo-state text should show artifact status counts"
  grep -Fq 'status=resolved event=evt_fail_a' "$TMP/state.txt" ||
    fail "repo-state text should identify the resolved event"

  before="$(wc -l < "$index" | tr -d ' ')"
  OMS_AGENT=codex "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_fail_b > "$TMP/resolve-one" &
  pid_one=$!
  OMS_AGENT=claude "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve \
    --event-id evt_fail_b > "$TMP/resolve-two" &
  pid_two=$!
  wait "$pid_one" || fail "first concurrent resolve failed"
  wait "$pid_two" || fail "second concurrent resolve failed"
  [ "$((before + 1))" = "$(wc -l < "$index" | tr -d ' ')" ] ||
    fail "concurrent artifact resolution should append exactly one event"

  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null

  local corrupt_repo="$TMP/corrupt-repo"
  local corrupt_index="$corrupt_repo/.oms/artifacts/index.jsonl"
  mkdir -p "$corrupt_repo/.oms/artifacts"
  git -C "$corrupt_repo" init -q
  cat > "$corrupt_index" <<'EOF'
{"schema":1,"event_id":"evt_corrupt_target","operation_id":"op_corrupt","artifact_id":"sha256:corrupt","ts":"2026-07-11T01:00:00Z","kind":"call","provider":"","exit":1}
not-json
EOF
  if "$ROOT/scripts/artifact-index.sh" --repo "$corrupt_repo" resolve \
    --event-id evt_corrupt_target >/dev/null 2>&1; then
    fail "resolve should fail closed on a corrupt index"
  fi
  [ "$(wc -l < "$corrupt_index" | tr -d ' ')" = "2" ] ||
    fail "failed resolve should not mutate a corrupt index"

  local malformed_repo="$TMP/malformed-repo"
  local malformed_index="$malformed_repo/.oms/artifacts/index.jsonl"
  mkdir -p "$malformed_repo/.oms/artifacts"
  git -C "$malformed_repo" init -q
  cat > "$malformed_index" <<'EOF'
{"schema":1,"event_id":"evt_bad_target","operation_id":"op_target","artifact_id":"sha256:target","ts":"2026-07-11T02:00:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_bad_resolution","operation_id":"op_wrong","artifact_id":"sha256:wrong","ts":"2026-07-11T02:00:01Z","kind":"artifact-resolution","provider":"codex","exit":9,"parent_event_id":"evt_bad_target","resolves_event_id":"evt_bad_target","resolution":"resolved"}
EOF
  if "$ROOT/scripts/artifact-index.sh" --repo "$malformed_repo" validate >/dev/null 2>&1; then
    fail "validate should reject malformed resolution contracts"
  fi
  "$ROOT/scripts/state.sh" --repo "$malformed_repo" --json > "$TMP/malformed-state.json"
  python3 - "$TMP/malformed-state.json" <<'PY' || fail "malformed resolution false-greened dashboard state"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["invalid_rows"] == 1
assert state["healthy"] is False
assert state["counts"] == {"success": 0, "unresolved": 1, "resolved": 0}
PY

  local invalid_exit_repo="$TMP/invalid-exit-repo"
  local invalid_exit_index="$invalid_exit_repo/.oms/artifacts/index.jsonl"
  mkdir -p "$invalid_exit_repo/.oms/artifacts"
  git -C "$invalid_exit_repo" init -q
  printf '%s\n' '{"schema":1,"event_id":"evt_invalid_exit","operation_id":"op_invalid","artifact_id":"sha256:invalid","ts":"2026-07-11T02:30:00Z","kind":"call","provider":"codex","exit":"oops"}' > "$invalid_exit_index"
  if "$ROOT/scripts/artifact-index.sh" --repo "$invalid_exit_repo" validate >/dev/null 2>&1; then
    fail "validate should reject a non-integer artifact exit"
  fi
  "$ROOT/scripts/state.sh" --repo "$invalid_exit_repo" --json > "$TMP/invalid-exit-state.json"
  python3 - "$TMP/invalid-exit-state.json" <<'PY' || fail "invalid exit false-greened dashboard state"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["invalid_rows"] == 1
assert state["healthy"] is False
assert state["counts"] == {"success": 0, "unresolved": 0, "resolved": 0}
PY

  local missing_resolves_repo="$TMP/missing-resolves-repo"
  local missing_resolves_index="$missing_resolves_repo/.oms/artifacts/index.jsonl"
  mkdir -p "$missing_resolves_repo/.oms/artifacts"
  git -C "$missing_resolves_repo" init -q
  cat > "$missing_resolves_index" <<'EOF'
{"schema":1,"event_id":"evt_parent_only_target","operation_id":"op_parent_only","artifact_id":"sha256:parent","ts":"2026-07-11T02:40:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_parent_only_resolution","operation_id":"op_parent_only","artifact_id":"sha256:parent","ts":"2026-07-11T02:40:01Z","kind":"artifact-resolution","provider":"codex","exit":0,"parent_event_id":"evt_parent_only_target","resolution":"resolved"}
EOF
  if "$ROOT/scripts/artifact-index.sh" --repo "$missing_resolves_repo" validate >/dev/null 2>&1; then
    fail "validate should require an explicit resolves_event_id"
  fi

  local bounded_repo="$TMP/bounded-repo"
  local bounded_index="$bounded_repo/.oms/artifacts/index.jsonl"
  mkdir -p "$bounded_repo/.oms/artifacts"
  git -C "$bounded_repo" init -q
  cat > "$bounded_index" <<'EOF'
{"schema":1,"event_id":"evt_old_one","operation_id":"op_old_one","artifact_id":"sha256:old1","ts":"2026-07-11T03:00:00Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_old_two","operation_id":"op_old_two","artifact_id":"sha256:old2","ts":"2026-07-11T03:00:01Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_bounded_target","operation_id":"op_bounded","artifact_id":"sha256:bounded","ts":"2026-07-11T03:00:02Z","kind":"call","provider":"","exit":1}
EOF
  OMS_ARTIFACT_INDEX_KEEP=2 OMS_ARTIFACT_INDEX_HIGH_WATER=3 \
    "$ROOT/scripts/artifact-index.sh" --repo "$bounded_repo" resolve \
      --event-id evt_bounded_target >/dev/null
  [ "$(wc -l < "$bounded_index" | tr -d ' ')" = "2" ] ||
    fail "resolution append should honor artifact index high-water retention"
  grep -Fq 'evt_bounded_target' "$bounded_index" ||
    fail "retention should preserve the resolved target at the tail"
}

test_artifact_retention_corruption_and_source_tracking() {
  local repo="$TMP/retention-repo"
  local index="$repo/.oms/artifacts/index.jsonl"

  mkdir -p "$repo/.oms/artifacts"
  git -C "$repo" init -q
  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_retained_target","operation_id":"op_retained","artifact_id":"sha256:retained","ts":"2026-07-11T04:00:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_newer","operation_id":"op_newer","artifact_id":"sha256:newer","ts":"2026-07-11T04:00:01Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_retained_resolution","operation_id":"op_retained","artifact_id":"sha256:retained","ts":"2026-07-11T04:00:02Z","kind":"artifact-resolution","provider":"codex","exit":0,"parent_event_id":"evt_retained_target","resolves_event_id":"evt_retained_target","resolution":"resolved"}
EOF
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" prune 2 >/dev/null
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "prune should not leave dangling artifact resolution lineage"
  if grep -Fq 'evt_retained_resolution' "$index" && ! grep -Fq 'evt_retained_target' "$index"; then
    fail "prune retained a resolution without its target"
  fi

  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_boundary_target","operation_id":"op_boundary","artifact_id":"sha256:boundary","ts":"2026-07-11T04:05:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_boundary_resolution","operation_id":"op_boundary","artifact_id":"sha256:boundary","ts":"2026-07-11T04:05:01Z","kind":"artifact-resolution","provider":"codex","exit":0,"parent_event_id":"evt_boundary_target","resolves_event_id":"evt_boundary_target","resolution":"resolved"}
{"schema":1,"event_id":"evt_boundary_newer","operation_id":"op_boundary_newer","artifact_id":"sha256:boundary-newer","ts":"2026-07-11T04:05:02Z","kind":"call","provider":"codex","exit":0}
EOF
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" prune 2 >/dev/null
  if grep -Fq 'evt_boundary_target' "$index" || grep -Fq 'evt_boundary_resolution' "$index"; then
    fail "retention should not reopen a resolved target when its pair does not fit"
  fi

  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_high_target","operation_id":"op_high","artifact_id":"sha256:high","ts":"2026-07-11T04:10:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_high_newer_one","operation_id":"op_one","artifact_id":"sha256:one","ts":"2026-07-11T04:10:01Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_high_newer_two","operation_id":"op_two","artifact_id":"sha256:two","ts":"2026-07-11T04:10:02Z","kind":"call","provider":"codex","exit":0}
EOF
  OMS_ARTIFACT_INDEX_KEEP=2 OMS_ARTIFACT_INDEX_HIGH_WATER=3 \
    "$ROOT/scripts/artifact-index.sh" --repo "$repo" resolve --event-id evt_high_target >/dev/null
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "resolve high-water retention should not leave dangling lineage"
  grep -Fq 'evt_high_target' "$index" && grep -Fq 'artifact-resolution' "$index" ||
    fail "resolve retention should keep the target and resolution atomically"

  cat > "$index" <<'EOF'
{"schema":1,"event_id":"evt_writer_target","operation_id":"op_writer","artifact_id":"sha256:writer","ts":"2026-07-11T04:20:00Z","kind":"call","provider":"codex","exit":1}
{"schema":1,"event_id":"evt_writer_newer","operation_id":"op_writer_newer","artifact_id":"sha256:writer-newer","ts":"2026-07-11T04:20:01Z","kind":"call","provider":"codex","exit":0}
{"schema":1,"event_id":"evt_writer_resolution","operation_id":"op_writer","artifact_id":"sha256:writer","ts":"2026-07-11T04:20:02Z","kind":"artifact-resolution","provider":"codex","exit":0,"parent_event_id":"evt_writer_target","resolves_event_id":"evt_writer_target","resolution":"resolved"}
EOF
  (
    # shellcheck source=scripts/lib/peer-common.sh
    . "$ROOT/scripts/lib/peer-common.sh"
    OMS_ARTIFACT_INDEX_KEEP=2 OMS_ARTIFACT_INDEX_HIGH_WATER=3 \
      ma_append_artifact_index "$repo" call codex 0 "" "" "" "" ""
  )
  "$ROOT/scripts/artifact-index.sh" --repo "$repo" validate >/dev/null ||
    fail "provider append high-water retention should not leave dangling lineage"

  printf 'not-json\n' > "$index"
  "$ROOT/scripts/state.sh" --repo "$repo" --json > "$TMP/corrupt-state.json"
  python3 - "$TMP/corrupt-state.json" <<'PY' || fail "repo-state hid artifact corruption"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["invalid_rows"] == 1
assert state["healthy"] is False
PY
  "$ROOT/scripts/state.sh" --repo "$repo" > "$TMP/corrupt-state.txt"
  grep -Fq 'CORRUPT invalid_rows=1' "$TMP/corrupt-state.txt" ||
    fail "repo-state text should make artifact corruption visible"

  printf '%s\n' '{"schema":1,"kind":"call","exit":0}' > "$index"
  "$ROOT/scripts/state.sh" --repo "$repo" --json > "$TMP/invalid-contract-state.json"
  python3 - "$TMP/invalid-contract-state.json" <<'PY' || fail "repo-state false-greened invalid schema-1 outcome"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["invalid_rows"] == 1
assert state["healthy"] is False
assert state["counts"] == {"success": 0, "unresolved": 0, "resolved": 0}
PY

  local source_repo="$TMP/source-tracking-repo"
  mkdir -p "$source_repo/.oms/artifacts"
  printf 'source\n' > "$source_repo/.oms/artifacts/source.md"
  printf '%s\n' '{"source":".oms/artifacts/source.md"}' > "$source_repo/.oms/artifacts/index.jsonl"
  # shellcheck source=scripts/lib/harness-residue.sh
  . "$ROOT/scripts/lib/harness-residue.sh"
  [ "$(oms_harness_count_unindexed_artifacts "$source_repo")" = "0" ] ||
    fail "source-only artifact references should count as indexed"
}

test_large_artifact_index_compacts_before_append() {
  # A ledger already past the durable byte ceiling must compact before the
  # append, not only after it: the durable writer refuses the row first
  # otherwise, retention never runs, and the || true call sites drop every
  # later row silently.
  local repo="$TMP/oversized-repo"
  local index="$repo/.oms/artifacts/index.jsonl"
  mkdir -p "$repo/.oms/artifacts"
  git -C "$repo" init -q
  python3 - "$index" <<'PY'
import json, sys
row = {"schema": 1, "kind": "call", "provider": "codex", "exit": 0,
       "filler": "x" * 65536}
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    for i in range(270):
        row["event_id"] = "evt_bulk_%d" % i
        row["operation_id"] = "op_bulk_%d" % i
        handle.write(json.dumps(row) + "\n")
PY
  [ "$(wc -c < "$index")" -gt 16777216 ] ||
    fail "fixture must start beyond the durable byte ceiling"
  (
    # shellcheck source=scripts/lib/peer-common.sh
    . "$ROOT/scripts/lib/peer-common.sh"
    export OMS_ARTIFACT_INDEX_KEEP=5 OMS_ARTIFACT_INDEX_HIGH_WATER=5
    ma_append_artifact_index "$repo" call codex 0 "" "" "" "" ""
  ) || fail "an over-ceiling ledger must compact before the append, not refuse"
  [ "$(wc -l < "$index" | tr -d ' ')" -le 6 ] ||
    fail "pre-append compaction must bound the ledger"
  if tail -n 1 "$index" | grep -F 'evt_bulk' > /dev/null; then
    fail "the appended row must land at the tail after compaction"
  fi
}

test_smoke_runner_tail_and_signal_cleanup() {
  local suite="$TMP/overlap-smoke.sh"
  local out="$TMP/overlap.out"

  cat > "$suite" <<'EOF'
set -e
meet_peer() {
  [ -n "${OMS_TEST_BARRIER:-}" ] || return 0
  touch "$OMS_TEST_BARRIER/$1"
  local tries=0
  while [ ! -f "$OMS_TEST_BARRIER/$2" ]; do
    tries=$((tries + 1))
    [ "$tries" -le 50 ] || return 1
    sleep 0.1
  done
}
test_first() {
  meet_peer first second
  echo first
}
test_second() {
  meet_peer second first
  echo second
}
test_selected_failure() {
  return 7
}
# SMOKE_TEST_CALLS_BEGIN
  test_first # legacy formatting must be discarded
test_second
# SMOKE_TEST_CALLS_END
echo done
EOF
  # Which tests ran is verbose-mode evidence now: a passing run prints one
  # summary line, so the partition has to be inspected with the output on.
  OMS_VERBOSE=1 OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" --shard 2/2 > "$out"
  grep -Fq 'second' "$out" || fail "owning shard did not execute its test"
  if grep -Fq 'first' "$out"; then
    fail "legacy call block formatting caused cross-shard duplicate execution"
  fi

  # Select before partitioning; repeats must not execute twice, and extra
  # workers must not silently widen a small selection to the full suite.
  OMS_VERBOSE=1 OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
    --jobs 4 --only test_second --only test_second > "$out"
  [ "$(grep -c '^second$' "$out")" = 1 ] || fail 'selected parallel test must run exactly once'
  if grep -q '^first$' "$out"; then fail 'parallel selection ran an unselected test'; fi
  mkdir -p "$TMP/selected-barrier"
  OMS_TEST_BARRIER="$TMP/selected-barrier" OMS_VERBOSE=1 OMS_SMOKE_SUITE="$suite" \
    "$ROOT/tests/run-smoke-shard.sh" --jobs 2 --only test_first --only test_second > "$out" ||
    fail 'selected workers did not overlap'
  [ "$(grep -c '^first$' "$out")" = 1 ] && [ "$(grep -c '^second$' "$out")" = 1 ] ||
    fail 'each selected parallel worker must execute once'
  if OMS_VERBOSE=1 OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
      --jobs 2 --only test_first --only test_missing > "$out" 2>&1; then
    fail 'parallel selection accepted an unknown test'
  fi
  if grep -q '^first$' "$out"; then fail 'invalid selection launched workers before validation'; fi
  if OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
      --jobs 0 --only test_second > "$out" 2>&1; then
    fail 'selected execution accepted zero workers'
  fi
  if OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
      --jobs 2 --list > "$out" 2>&1; then
    fail 'parallel execution accepted list mode'
  fi
  if OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
      --jobs 2 --shard 4/4 --only test_second > "$out" 2>&1; then
    fail 'empty selection bypassed mutually exclusive worker controls'
  fi
  if OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" \
      --jobs 2 --only test_first --only test_selected_failure > "$out" 2>&1; then
    fail 'a failed selected worker must fail the parallel run'
  fi
  grep -Fxq 'check: overlap-smoke.test_selected_failure FAILED' "$out" ||
    fail "a failed smoke test must be named for the fail ledger: $(cat "$out")"

  local signal_suite="$TMP/signal-smoke.sh"
  local signal_tmp="$TMP/signal-tmp"
  local leader child fast linger linger_state="" status=0
  mkdir -p "$signal_tmp"
  cat > "$signal_suite" <<'EOF'
test_slow_one() {
  trap '' TERM
  echo "$$" >> "$OMS_TEST_PID_FILE"
  sleep 30 &
  echo "$!" >> "$OMS_TEST_PID_FILE"
  wait "$!"
}
test_slow_two() {
  trap '' TERM
  echo "$$" >> "$OMS_TEST_PID_FILE"
  sleep 30 &
  echo "$!" >> "$OMS_TEST_PID_FILE"
  wait "$!"
}
test_fast() {
  echo "$PPID" > "$OMS_TEST_FAST_FILE"
}
test_lingering() {
  sleep 30 &
  echo "$!" > "$OMS_TEST_LINGER_FILE"
}
# SMOKE_TEST_CALLS_BEGIN
test_slow_one
test_slow_two
test_fast
test_lingering
# SMOKE_TEST_CALLS_END
echo done
EOF
  # Every kill the runner sends is logged, so a signal to the already-finished
  # shard's group (whose PID another process may reuse) is caught.
  printf '%s\n' 'kill() { printf "%s %s\n" "$$" "$*" >> "$OMS_TEST_KILL_LOG"; builtin kill "$@"; }' \
    > "$signal_tmp/kill-log.bash"
  TMPDIR="$signal_tmp" OMS_TEST_PID_FILE="$signal_tmp/pids" OMS_TEST_FAST_FILE="$signal_tmp/fast" \
    OMS_TEST_LINGER_FILE="$signal_tmp/linger" OMS_TEST_KILL_LOG="$signal_tmp/kills" BASH_ENV="$signal_tmp/kill-log.bash" OMS_SMOKE_SUITE="$signal_suite" \
    "$ROOT/tests/run-smoke-shard.sh" --jobs 4 >/dev/null 2>&1 &
  leader=$!
  for _ in $(seq 1 100); do
    [ -f "$signal_tmp/pids" ] && [ "$(wc -l < "$signal_tmp/pids")" -ge 4 ] && [ -s "$signal_tmp/fast" ] && [ -s "$signal_tmp/linger" ] && break
    sleep 0.05
  done
  [ -s "$signal_tmp/fast" ] || fail "the fast smoke shard did not run"
  fast="$(cat "$signal_tmp/fast")"
  for _ in $(seq 1 100); do
    kill -0 "$fast" 2>/dev/null || break
    sleep 0.05
  done
  [ -f "$signal_tmp/pids" ] || fail "parallel smoke workers did not start"
  kill -TERM "$leader"
  wait "$leader" || status=$?
  [ "$status" -eq 143 ] || fail "TERM should propagate as 143, got $status"
  # Children may linger as unreaped zombies after the leader's TERM — briefly
  # on a loaded runner, and for the whole phase under a deferring subreaper
  # (autopilot-receipt supervise), where kill -0 keeps succeeding on the
  # corpse forever. Poll process state instead: gone or zombie is dead; only
  # a still-runnable child is a survivor.
  lingering=""
  for _ in $(seq 1 100); do
    lingering=""
    while IFS= read -r child; do
      child_state="$(ps -o stat= -p "$child" 2>/dev/null | tr -d '[:space:]' || true)"
      case "$child_state" in
        ''|Z*) ;;
        *) lingering="$child"; break ;;
      esac
    done < "$signal_tmp/pids"
    [ -n "$lingering" ] || break
    sleep 0.05
  done
  if [ -n "$lingering" ]; then
    kill -KILL "$lingering" 2>/dev/null || true
    fail "parallel smoke child survived parent TERM: $lingering"
  fi
  [ -z "$(find "$signal_tmp" -maxdepth 1 -type d -name 'oms-smoke-shards.*' -print -quit)" ] ||
    fail "parallel smoke log directory survived parent TERM"
  # Existence probes (kill -0) are allowed; only a delivered signal is a fault.
  ! grep -Eq "^$leader -(TERM|KILL) .*(^| )-?$fast( |\$)" "$signal_tmp/kills" ||
    fail "parallel smoke cleanup signalled the finished shard $fast: $(grep "^$leader " "$signal_tmp/kills" | tr '\n' ';')"
  grep -q "^$leader " "$signal_tmp/kills" || fail "parallel smoke cleanup sent no signal to running shards"
  # A finished shard's leftover process still names the shard's group, so the
  # group is reserved and its member must not outlive the cancelled run.
  linger="$(cat "$signal_tmp/linger")"
  for _ in $(seq 1 100); do
    linger_state="$(ps -o stat= -p "$linger" 2>/dev/null | tr -d '[:space:]' || true)"
    case "$linger_state" in ''|Z*) break ;; esac
    sleep 0.05
  done
  case "$linger_state" in
    ''|Z*) ;;
    *) kill -KILL "$linger" 2>/dev/null || true
       fail "a finished smoke shard's background process survived parent TERM: $linger" ;;
  esac
  [ -z "$(find "$signal_tmp" -maxdepth 1 -name 'oms-smoke-run.*' -print -quit)" ] ||
    fail "a terminated smoke shard left its run log behind"
}

test_smoke_runner_reports_bounded_opt_in_timings() {
  local suite="$TMP/timed-smoke.sh"
  local timed_out="$TMP/timed-smoke.out"
  local quiet_out="$TMP/quiet-smoke.out"

  cat > "$suite" <<'EOF'
set -euo pipefail
test_slow() {
  # SECONDS is a Bash 3.2 special variable. Advancing it makes this fixture
  # deterministic without sleeping in every full gate run.
  SECONDS=$((SECONDS + 2))
}
test_fast() {
  [ "${OMS_SMOKE_TIMINGS+x}" != x ] || {
    echo "runner control leaked into the suite" >&2
    return 1
  }
}
# SMOKE_TEST_CALLS_BEGIN
test_slow
test_fast
# SMOKE_TEST_CALLS_END
EOF

  OMS_SMOKE_SUITE="$suite" OMS_SMOKE_TIMINGS=1 OMS_SMOKE_TIMING_LIMIT=1 \
    "$ROOT/tests/run-smoke-shard.sh" > "$timed_out"
  grep -Eq '^smoke-timing: [0-9]+s test_slow$' "$timed_out" ||
    fail "timing mode did not report the slow test: $(cat "$timed_out")"
  [ "$(grep -c '^smoke-timing:' "$timed_out")" = 1 ] ||
    fail "timing output ignored OMS_SMOKE_TIMING_LIMIT: $(cat "$timed_out")"
  grep -Eq '^scripts-smoke: ok \(2 tests, duration=[0-9]+s\)$' "$timed_out" ||
    fail "smoke summary did not report shard duration: $(cat "$timed_out")"

  OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" > "$quiet_out"
  if grep -q '^smoke-timing:' "$quiet_out"; then
    fail "default smoke output should remain one compact summary line"
  fi
  [ "$(wc -l < "$quiet_out" | tr -d ' ')" = 1 ] ||
    fail "default smoke output became noisy: $(cat "$quiet_out")"
}

test_gate_fingerprints_full_oms_state() {
  local gate="$TMP/check-state-gate"
  local base head mutation out status

  mkdir -p "$gate/scripts/lib" "$gate/tests"
  cp "$ROOT/scripts/check.sh" "$gate/scripts/check.sh"
  cp "$ROOT/scripts/lib/oms-state-inventory.py" "$gate/scripts/lib/oms-state-inventory.py"
  cp "$ROOT/scripts/lib/check-maintenance.sh" "$ROOT/scripts/lib/file-lock.sh" \
    "$ROOT/scripts/lib/poll.sh" "$ROOT/scripts/lib/check-parallel.sh" "$gate/scripts/lib/"
  printf '.oms/\n' > "$gate/.gitignore"
  cat > "$gate/scripts/graph.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' '{"schema":1,"mode":"affected","tests":["tests/source-distribution-smoke.sh"]}'
EOF
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/scripts/check-python.sh"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/scripts/check-bash32.sh"
  cat > "$gate/fake-shellcheck" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${OMS_TEST_CHECK_MUTATION:-}" in
  lint) printf 'after\n' > .oms/state ;;
esac
EOF
  chmod +x "$gate/fake-shellcheck"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/tests/codex-hud-config-smoke.sh"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/tests/run-smoke-shard.sh"
  for suite in source-distribution-smoke.sh platform-portability-smoke.sh \
      bsd-portability-smoke.sh functional-evolution-smoke.sh; do
    cat > "$gate/tests/$suite" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ "${0##*/}" = source-distribution-smoke.sh ] || exit 0
case "${OMS_TEST_CHECK_MUTATION:-}" in
  content) printf 'after\n' > "$root/.oms/state" ;;
  symlink) ln -s state "$root/.oms/state-link" ;;
  directory) mkdir "$root/.oms/empty-state" ;;
  mode) chmod 700 "$root/.oms/state" ;;
  failed-content) printf 'after\n' > "$root/.oms/state"; exit 29 ;;
  timings)
    python3 -c 'import signal; assert signal.getsignal(signal.SIGINT) != signal.SIG_IGN'
    for n in 1 2 3 4 5 6 7; do
      echo "operator-section: ${n}s core/section-$n"
      echo "operator-shard: $n/7 ${n}s"
    done ;;
  deadline)
    python3 - <<'PY'
import os, pathlib, signal, subprocess, sys
child = subprocess.Popen([sys.executable, '-c',
    'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)'],
    start_new_session=os.name == 'posix')
pathlib.Path(os.environ['OMS_TEST_STAGE_CHILD']).write_text(str(child.pid))
child.wait()
PY
    ;;
  hooks) mkdir -p "$root/.oms/hooks"; printf 'ambient\n' >> "$root/.oms/hooks/events.jsonl" ;;
  journal) mkdir -p "$root/.oms/work-journal"; printf 'ambient\n' >> "$root/.oms/work-journal/index.json" ;;
  ci) printf 'ambient\n' >> "$root/.oms/ci.jsonl"; printf '*\n' > "$root/.oms/.gitignore" ;;
  nested-ci) mkdir -p "$root/.oms/plan"; printf 'leak\n' >> "$root/.oms/plan/ci.jsonl" ;;
  graph-shadow) mkdir -p "$root/.oms/graph"; printf 'ambient\n' >> "$root/.oms/graph/shadow.jsonl" ;;
  graph-run) mkdir -p "$root/.oms/graph/runs/r1"; printf 'leak\n' >> "$root/.oms/graph/runs/r1/events.jsonl" ;;
  tick-write) mkdir -p "$root/.oms/tick"; printf 'leak\n' > "$root/.oms/tick/last.json" ;;
  tick)
    out="$(OMS_LOCK_DIR="$OMS_TEST_TICK_LOCK_DIR" OMS_WORK_JOURNAL=0 \
      OMS_INSTALL_RECEIPT="$root/no-receipt" "$OMS_TEST_TICK" run --repo "$root")"
    printf '%s' "$out" | grep >/dev/null 'verification running'
    ;;
esac
EOF
  done
  git -C "$gate" init -q
  git -C "$gate" config user.name test
  git -C "$gate" config user.email test@example.com
  printf 'one\n' > "$gate/README.md"
  git -C "$gate" add .
  git -C "$gate" commit -qm base
  base="$(git -C "$gate" rev-parse HEAD)"
  printf 'two\n' >> "$gate/README.md"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/tests/changed.sh"
  git -C "$gate" add README.md tests/changed.sh
  git -C "$gate" commit -qm head
  head="$(git -C "$gate" rev-parse HEAD)"

  for mutation in content symlink directory mode lint failed-content nested-ci graph-run tick-write; do
    rm -rf "$gate/.oms"
    mkdir -p "$gate/.oms"
    printf 'before\n' > "$gate/.oms/state"
    status=0
    out="$(cd "$gate" && OMS_SHELLCHECK_BIN="$gate/fake-shellcheck" \
      OMS_TEST_CHECK_MUTATION="$mutation" \
      bash scripts/check.sh --quick --changed-from "$base" --changed-to "$head" 2>&1)" || status=$?
    [ "$status" -ne 0 ] || fail "check gate missed .oms $mutation mutation"
    printf '%s' "$out" | grep >/dev/null -F "a suite wrote into this checkout's .oms state" ||
      fail "check gate did not explain .oms $mutation mutation: $out"
  done

  # These belong to the live session and Work Journal, not to the suite under
  # test: two trees it writes every turn, plus the CI row it records whenever a
  # push lands. Their ambient writes must not make a clean gate flaky, while
  # the same file name one level down stays a leak (see nested-ci above).
  for mutation in hooks journal ci graph-shadow tick; do
    rm -rf "$gate/.oms"
    mkdir -p "$gate/.oms"
    printf 'before\n' > "$gate/.oms/state"
    (cd "$gate" && OMS_SHELLCHECK_BIN="$gate/fake-shellcheck" \
      OMS_TEST_TICK="$ROOT/scripts/tick.sh" OMS_TEST_TICK_LOCK_DIR="${OMS_LOCK_DIR:-}" \
      OMS_TEST_CHECK_MUTATION="$mutation" \
      bash scripts/check.sh --quick --changed-from "$base" --changed-to "$head" >/dev/null) ||
      fail "check gate treated ambient $mutation state as a suite leak"
  done

  # A live session may create only an ignored ambient tree while the gate is
  # running. That must compare equal to an initially absent .oms directory.
  rm -rf "$gate/.oms"
  (cd "$gate" && OMS_SHELLCHECK_BIN="$gate/fake-shellcheck" \
    OMS_TEST_CHECK_MUTATION=hooks \
    bash scripts/check.sh --quick --changed-from "$base" --changed-to "$head" >/dev/null) ||
    fail "check gate treated ambient-only .oms creation as a suite leak"

  (cd "$gate" && OMS_SHELLCHECK_BIN="$gate/fake-shellcheck" \
    OMS_TEST_CHECK_MUTATION=timings GITHUB_STEP_SUMMARY="$TMP/operator-summary" \
    bash scripts/check.sh --quick --changed-from "$base" --changed-to "$head" > "$TMP/timing-output") ||
    fail "timing gate failed"
  for kind in operator-section operator-shard; do
    [ "$(grep -c "^slow-$kind:" "$TMP/timing-output")" = 5 ] || fail "unbounded or missing $kind timings"
    grep -Fq "slow-$kind: 7s" "$TMP/operator-summary" || fail "CI summary discarded successful $kind timings"
  done
  python3 - "$gate" "$base" "$head" "$TMP" <<'PY' || fail "stage deadline missed a descendant or lost diagnostics"
import os, pathlib, signal, subprocess, sys, time
gate, base, head, tmp = sys.argv[1:]
if os.name != 'posix':
    print('SKIP: detached descendant deadline probe requires POSIX signals')
    raise SystemExit(0)
child_file = pathlib.Path(tmp) / 'stage-child'
env = dict(os.environ, OMS_SHELLCHECK_BIN=gate+'/fake-shellcheck', OMS_TEST_CHECK_MUTATION='deadline',
           OMS_CHECK_STAGE_TIMEOUT_SECONDS='2', OMS_TEST_STAGE_CHILD=str(child_file))
command = ['bash', 'scripts/check.sh', '--quick', '--changed-from', base, '--changed-to', head]
result = subprocess.run(command, cwd=gate, env=env, capture_output=True, text=True, timeout=25)
assert result.returncode == 124, (result.stdout, result.stderr)
assert child_file.exists(), (result.stdout, result.stderr)
child = int(child_file.read_text())
def alive(pid):
    stat = pathlib.Path('/proc') / str(pid) / 'stat'
    try:
        if stat.read_text().split(') ', 1)[1].startswith('Z '):
            return False
    except FileNotFoundError:
        pass
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
assert not alive(child), child
prefix = 'check: deadline diagnostics retained: '
log = pathlib.Path(next(line[len(prefix):] for line in result.stderr.splitlines() if line.startswith(prefix)))
assert 'stage deadline exceeded' in log.read_text(), log
log.unlink()
# A caller cancellation uses the same cleanup path rather than waiting out the deadline.
env['OMS_CHECK_STAGE_TIMEOUT_SECONDS'] = '60'
for mode in ('--quick', '--affected'):
    child_file.unlink()
    command[2] = mode
    process = subprocess.Popen(command, cwd=gate, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        deadline = time.monotonic() + 15
        while not child_file.exists() and time.monotonic() < deadline:
            assert process.poll() is None
            time.sleep(.02)
        assert child_file.exists(), 'stage did not start'
        process.send_signal(signal.SIGTERM)
        output, error = process.communicate(timeout=15)
        assert process.returncode == 143, (output, error)
        assert not alive(int(child_file.read_text()))
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=15)
# Cancel inside Popen, after the session exists but before its handle returns.
# This deterministically covers the launch window without production test hooks.
race = r'''
import os, pathlib, signal, subprocess, sys
source = pathlib.Path(sys.argv.pop(1)).read_text()
source = source.split('run_stage_with_deadline() {', 1)[1].split("<<'PY'\n", 1)[1].split('\nPY\n}', 1)[0]
launch = subprocess.Popen
def cancel_launch(*args, **kwargs):
    child = launch(*args, **kwargs)
    if kwargs.get('start_new_session'):
        pathlib.Path(os.environ['OMS_TEST_STAGE_CHILD']).write_text(str(child.pid))
        os.kill(os.getpid(), int(os.environ['OMS_TEST_STAGE_SIGNAL']))
    return child
subprocess.Popen = cancel_launch
exec(compile(source, sys.argv[0], 'exec'))
'''
for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    child_file.unlink()
    try:
        result = subprocess.run([sys.executable, '-c', race, gate+'/scripts/check.sh', 'bash', '60',
                                 sys.executable, '-c', 'import time; time.sleep(60)'],
                                env=dict(env, OMS_TEST_STAGE_SIGNAL=str(int(sig))),
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
        child = int(child_file.read_text())
        assert result.returncode == 128 + sig, (sig, result.returncode, result.stderr)
        assert not alive(child), (sig, child)
    finally:
        child = int(child_file.read_text()) if child_file.exists() else None
        if child is not None and alive(child):
            os.killpg(child, signal.SIGKILL)
print('stage-launch cancellation: TERM/INT/HUP cleaned up')
PY
}

test_invariants_gate_checks_committed_staged_and_worktree_sources() {
  local gate="$TMP/check-invariants-gate"
  local base log="$TMP/invariants-python-files" shell_log="$TMP/invariants-shell-files"
  local smoke_log="$TMP/invariants-smoke-args" real_bash out

  mkdir -p "$gate/scripts/lib" "$gate/tests"
  cp "$ROOT/scripts/check.sh" "$gate/scripts/check.sh"
  cp "$ROOT/scripts/lib/oms-state-inventory.py" "$ROOT/scripts/lib/check-maintenance.sh" \
    "$ROOT/scripts/lib/file-lock.sh" "$ROOT/scripts/lib/poll.sh" \
    "$ROOT/scripts/lib/provider-registry.sh" "$gate/scripts/lib/"
  cat > "$gate/scripts/check-python.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >> "$OMS_TEST_INVARIANTS_PYTHON_LOG"
EOF
  cat > "$gate/tests/run-smoke-shard.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$OMS_TEST_INVARIANTS_SMOKE_LOG"
EOF
  real_bash="$(command -v bash)"
  mkdir -p "$gate/bin"
  cat > "$gate/bin/bash" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -n ]; then printf '%s\n' "$2" >> "$OMS_TEST_INVARIANTS_SHELL_LOG"; fi
exec "$OMS_TEST_INVARIANTS_REAL_BASH" "$@"
EOF
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/scripts/install-skills.sh"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/tests/source-distribution-smoke.sh"
  chmod +x "$gate/bin/bash" "$gate/scripts/check-python.sh" "$gate/tests/run-smoke-shard.sh" \
    "$gate/scripts/install-skills.sh" "$gate/tests/source-distribution-smoke.sh"
  git -C "$gate" init -q -b main
  git -C "$gate" config user.name test
  git -C "$gate" config user.email test@example.com
  printf 'true\n' > "$gate/committed.sh"
  printf 'value = 0\n' > "$gate/committed.py"
  git -C "$gate" add .
  git -C "$gate" commit -qm base
  base="$(git -C "$gate" rev-parse HEAD)"
  git -C "$gate" update-ref refs/remotes/origin/main "$base"
  git -C "$gate" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

  printf 'true\n# committed\n' > "$gate/committed.sh"
  printf 'value = 1\n' > "$gate/committed.py"
  git -C "$gate" add committed.sh committed.py
  git -C "$gate" commit -qm committed-changes
  printf 'true\n' > "$gate/staged.sh"
  printf 'value = 2\n' > "$gate/staged.py"
  git -C "$gate" add staged.sh staged.py
  printf 'true\n' > "$gate/unstaged.sh"
  printf 'value = 3\n' > "$gate/unstaged.py"
  git -C "$gate" add unstaged.sh unstaged.py
  printf '# worktree edit\n' >> "$gate/unstaged.sh"
  printf 'value = 4\n' > "$gate/untracked.py"
  printf 'true\n' > "$gate/untracked.sh"

  out="$(cd "$gate" && PATH="$gate/bin:$PATH" OMS_TEST_INVARIANTS_REAL_BASH="$real_bash" \
    OMS_LOCK_DIR="$gate/locks" OMS_TEST_INVARIANTS_PYTHON_LOG="$log" \
    OMS_TEST_INVARIANTS_SHELL_LOG="$shell_log" OMS_TEST_INVARIANTS_SMOKE_LOG="$smoke_log" \
    "$real_bash" scripts/check.sh invariants 2>&1)" || fail "invariants lane failed in fixture: $out"
  for file in committed.py staged.py unstaged.py untracked.py; do
    grep -Fxq "$file" "$log" || fail "invariants syntax stage omitted $file"
  done
  for file in committed.sh staged.sh unstaged.sh untracked.sh; do
    grep -Fxq "$file" "$shell_log" || fail "invariants syntax stage omitted $file"
  done
  for name in test_oms_allowlist_stays_in_sync_with_scripts test_peer_review_warns_untracked_files \
      test_peer_review_gate_binds_complete_diff_and_refuses_truncation; do
    grep -Fq -- "--only $name" "$smoke_log" || fail "invariants lane omitted $name"
  done
}

test_affected_gate_runs_positive_evidence_and_fails_open() {
  local gate="$TMP/check-affected-gate"
  local base head boundary_base out policy_path log="$TMP/check-affected.log"

  mkdir -p "$gate/scripts/lib" "$gate/tests" "$gate/lib"
  cp "$ROOT/scripts/check.sh" "$gate/scripts/check.sh"
  cp "$ROOT/scripts/lib/oms-state-inventory.py" "$gate/scripts/lib/oms-state-inventory.py"
  cp "$ROOT/scripts/lib/check-maintenance.sh" "$ROOT/scripts/lib/file-lock.sh" \
    "$ROOT/scripts/lib/poll.sh" "$ROOT/scripts/lib/provider-registry.sh" \
    "$ROOT/scripts/lib/check-parallel.sh" "$gate/scripts/lib/"
  cp "$ROOT/tests/run-smoke-shard.sh" "$gate/tests/run-smoke-shard.sh"
  for script in check-python.sh check-bash32.sh; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/scripts/$script"
  done
  printf '#!/usr/bin/env bash\nprintf "syntax:%%s\\n" "$*" >> "$OMS_TEST_AFFECTED_LOG"\n' > "$gate/scripts/check-python.sh"
  for suite in codex-hud-config-smoke.sh source-distribution-smoke.sh \
      platform-portability-smoke.sh bsd-portability-smoke.sh functional-evolution-smoke.sh; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$gate/tests/$suite"
  done
  printf '#!/usr/bin/env bash\nprintf "distribution:%%s\\n" "$*" >> "$OMS_TEST_AFFECTED_LOG"\n' > "$gate/tests/source-distribution-smoke.sh"
  cat > "$gate/scripts/graph.sh" <<'EOF'
#!/usr/bin/env bash
case "${OMS_TEST_GRAPH_MODE:-affected}" in
  full)
    printf '%s\n' '{"schema":1,"mode":"full","reasons":["unmatched:lib/leaf.py"],"tests":[],"test_cases":[]}'
    ;;
  operator-unmapped)
    printf '%s\n' '{"schema":1,"mode":"affected","reasons":[],"tests":["tests/operator/section.sh","tests/operator/section.py","tests/operator/unmapped.py"],"test_cases":[]}'
    ;;
  no-operator)
    printf '%s\n' '{"schema":1,"mode":"affected","reasons":[],"tests":["tests/leaf-smoke.sh"],"test_cases":[]}'
    ;;
  error) exit 23 ;;
  *)
    printf '%s\n' '{"schema":1,"mode":"affected","reasons":[],"tests":["tests/operator/section.sh","tests/operator/section.py","tests/leaf-smoke.sh","tests/scripts-smoke.sh","tests/autonomy-plan-run-smoke.sh","tests/test_leaf.py","tests/run-smoke-shard.sh"],"test_cases":[{"id":"symbol:tests/scripts-smoke.sh::test_selected","language":"shell","name":"test_selected","path":"tests/scripts-smoke.sh"},{"id":"symbol:tests/autonomy-plan-run-smoke.sh::test_context_pack","language":"shell","name":"test_context_pack","path":"tests/autonomy-plan-run-smoke.sh"},{"id":"symbol:tests/test_leaf.py::LeafTest.test_selected","language":"python","name":"test_selected","path":"tests/test_leaf.py"},{"id":"symbol:tests/operator/section.py::SectionTest.test_selected","language":"python","name":"test_selected","path":"tests/operator/section.py"}]}'
    ;;
esac
EOF
  chmod +x "$gate/scripts/graph.sh"
  mkdir -p "$gate/tests/operator"
  printf '#!/usr/bin/env bash\necho operator-fragment-standalone >> "$OMS_TEST_AFFECTED_LOG"\n' > "$gate/tests/operator/section.sh"
  printf '# Operator section module; its suite runs as a whole.\n' > "$gate/tests/operator/section.py"
  cat > "$gate/tests/operator/runner.py" <<'EOF'
SECTIONS = (
    ("section-shell", 0, "section.sh"),
    ("section-python", 0, "section.py"),
)
EOF
  printf '# Unmapped operator helper.\n' > "$gate/tests/operator/unmapped.py"
  cat > "$gate/tests/operator-tools-smoke.sh" <<'EOF'
#!/usr/bin/env bash
printf 'operator-args:%s\n' "$*" >> "$OMS_TEST_AFFECTED_LOG"
. tests/parallel-probe.sh
[ "${OMS_TEST_AFFECTED_FAIL:-0}" != 1 ] || exit 23
EOF
  cat > "$gate/tests/scripts-smoke.sh" <<'EOF'
#!/usr/bin/env bash
test_selected() {
  printf 'shell-selected\n' >> "$OMS_TEST_AFFECTED_LOG"
}

test_unselected() {
  printf 'shell-unselected\n' >> "$OMS_TEST_AFFECTED_LOG"
}
test_scrubber_passes_harness_sources() { printf 'invariant:scrubber\n' >> "$OMS_TEST_AFFECTED_LOG"; }
test_global_rules_stay_compact_and_route_workflows() { printf 'invariant:global-rules\n' >> "$OMS_TEST_AFFECTED_LOG"; }
test_project_policy_templates_stay_compact() { printf 'invariant:policy-templates\n' >> "$OMS_TEST_AFFECTED_LOG"; }
test_large_skills_use_progressive_disclosure() { printf 'invariant:large-skills\n' >> "$OMS_TEST_AFFECTED_LOG"; }
test_generated_project_contracts_stay_compact() { printf 'invariant:project-contracts\n' >> "$OMS_TEST_AFFECTED_LOG"; }
test_apply_loader_allows_clear_bounded_changes() { printf 'invariant:apply-loader\n' >> "$OMS_TEST_AFFECTED_LOG"; }
# SMOKE_TEST_CALLS_BEGIN
test_selected
test_unselected
test_scrubber_passes_harness_sources
test_global_rules_stay_compact_and_route_workflows
test_project_policy_templates_stay_compact
test_large_skills_use_progressive_disclosure
test_generated_project_contracts_stay_compact
test_apply_loader_allows_clear_bounded_changes
# SMOKE_TEST_CALLS_END
EOF
  cat > "$gate/tests/leaf-smoke.sh" <<'EOF'
#!/usr/bin/env bash
printf 'shell-file\n' >> "$OMS_TEST_AFFECTED_LOG"
. tests/parallel-probe.sh
EOF
  cat > "$gate/tests/autonomy-plan-run-smoke.sh" <<'EOF'
#!/usr/bin/env bash
test_context_pack() {
  printf 'plan-context-selected\n' >> "$OMS_TEST_AFFECTED_LOG"
}
test_plan_lifecycle() {
  printf 'plan-lifecycle-unselected\n' >> "$OMS_TEST_AFFECTED_LOG"
}
# SMOKE_TEST_CALLS_BEGIN
test_context_pack
test_plan_lifecycle
# SMOKE_TEST_CALLS_END
EOF
  cat > "$gate/tests/test_leaf.py" <<'EOF'
import os
import unittest

class LeafTest(unittest.TestCase):
    def test_selected(self):
        with open(os.environ["OMS_TEST_AFFECTED_LOG"], "a", encoding="utf-8") as handle:
            handle.write("python-selected\n")

    def test_unselected(self):
        with open(os.environ["OMS_TEST_AFFECTED_LOG"], "a", encoding="utf-8") as handle:
            handle.write("python-unselected\n")
EOF
  cat > "$gate/tests/parallel-probe.sh" <<'EOF'
if [ -n "${OMS_TEST_AFFECTED_EVENTS:-}" ]; then
  [ "$OMS_SMOKE_JOBS:$OMS_OPERATOR_JOBS:$OMS_LINT_JOBS_MAX" = 1:1:1 ] || exit 24
  printf 'start %s\n' "$0" >> "$OMS_TEST_AFFECTED_EVENTS"
  sleep 1
  printf 'end %s\n' "$0" >> "$OMS_TEST_AFFECTED_EVENTS"
fi
EOF
  printf 'one\n' > "$gate/lib/leaf.py"
  printf '__pycache__/\n' > "$gate/.gitignore"
  git -C "$gate" init -q
  git -C "$gate" config user.name test
  git -C "$gate" config user.email test@example.com
  git -C "$gate" add .
  git -C "$gate" commit -qm base
  base="$(git -C "$gate" rev-parse HEAD)"
  printf 'two\n' >> "$gate/lib/leaf.py"
  git -C "$gate" add lib/leaf.py
  git -C "$gate" commit -qm head
  head="$(git -C "$gate" rev-parse HEAD)"

  out="$(cd "$gate" && bash scripts/check.sh --affected --print-affected-mode \
    --changed-from "$base" --changed-to "$head" 2>/dev/null)" ||
    fail "affected gate could not expose its plan mode"
  [ "$out" = affected ] || fail "positive graph plan reported the wrong mode: $out"
  out="$(cd "$gate" && bash scripts/check.sh --affected --changed-from "$base" \
    --changed-to "$head" --list-stages 2>/dev/null)" ||
    fail "affected gate could not list positive-plan stages"
  if grep -Fxq 'affected:tests/run-smoke-shard.sh' <<< "$out"; then
    fail "affected plan staged the smoke runner bare: $out"
  fi
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=full \
    bash scripts/check.sh --affected --print-affected-mode \
    --changed-from "$base" --changed-to "$head" 2>/dev/null)" ||
    fail "full fallback could not expose its plan mode"
  [ "$out" = full ] || fail "uncertain graph plan reported the wrong mode: $out"

  : > "$log"
  (cd "$gate" && OMS_TEST_AFFECTED_LOG="$log" GITHUB_STEP_SUMMARY="$TMP/affected-summary.md" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      --ci-output "$TMP/ci-mode" >/dev/null) ||
    fail "affected gate rejected a positive graph plan"
  grep -Fxq 'mode=affected' "$TMP/ci-mode" || fail "CI mode was not recorded"
  grep -Fq 'mode=affected' "$TMP/affected-summary.md" || fail "CI summary omitted selected mode"
  grep -Fq 'slow-stage:' "$TMP/affected-summary.md" || fail "CI summary omitted stage timings"
  grep -Fxq shell-selected "$log" || fail "affected gate omitted the selected shell case"
  grep -Fxq plan-context-selected "$log" || fail "affected gate omitted the selected plan context case"
  grep -Fxq python-selected "$log" || fail "affected gate omitted the selected Python case"
  grep -Fxq shell-file "$log" || fail "affected gate omitted the selected shell file"
  for invariant in scrubber global-rules policy-templates large-skills project-contracts apply-loader; do
    [ "$(grep -Fxc "invariant:$invariant" "$log")" = 1 ] ||
      fail "affected gate did not run whole-tree invariant $invariant exactly once"
  done
  grep -Fxq 'syntax:lib/leaf.py' "$log" || fail "affected syntax check did not select only the changed Python"
  grep -Fxq 'distribution:--docs-only' "$log" || fail "leaf change ran unrelated CI orchestration probes"
  grep -Fxq 'operator=section-shell,section-python' "$TMP/ci-mode" ||
    fail "CI output omitted the mapped operator sections"
  ! grep -Fq 'operator-args:' "$log" ||
    fail "CI output still staged operator-tools in the planning job"
  grep -Fq 'operator-tools deferred' "$TMP/affected-summary.md" ||
    fail "CI summary omitted the operator-tools deferral"
  ! grep -Fq operator-fragment-standalone "$log" ||
    fail "affected selection ran an operator section fragment outside its suite"
  if grep -Fq unselected "$log"; then
    fail "affected gate ran a test case absent from the graph plan: $(cat "$log")"
  fi
  : > "$log"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=operator-unmapped OMS_TEST_AFFECTED_LOG="$log" \
    GITHUB_STEP_SUMMARY="$TMP/operator-unmapped-summary.md" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" >/dev/null) ||
    fail "affected gate rejected an unmapped operator path"
  [ "$(grep -Fxc "operator-args:--jobs ${OMS_OPERATOR_JOBS:-4}" "$log")" = 1 ] ||
    fail "an unmapped operator path did not fail open to the whole suite"
  : > "$log"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=operator-unmapped OMS_TEST_AFFECTED_LOG="$log" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      --ci-output "$TMP/ci-operator-all" >/dev/null) ||
    fail "CI selector rejected an unmapped operator path"
  grep -Fxq 'operator=all' "$TMP/ci-operator-all" ||
    fail "an unmapped operator path did not defer the whole suite to CI"
  ! grep -Fq 'operator-args:' "$log" || fail "CI output ran the whole operator suite in the planning job"
  : > "$log"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=no-operator OMS_TEST_AFFECTED_LOG="$log" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      --ci-output "$TMP/ci-no-operator" >/dev/null) ||
    fail "CI selector rejected a plan without operator tests"
  grep -Fxq 'operator=' "$TMP/ci-no-operator" || fail "CI output omitted the empty operator line"
  # Without --ci-output the local run keeps operator-tools in place.
  : > "$log"
  # The same real stage runner must overlap selected suites, cap nested
  # workers, retain timings and propagate a selected lane's failure.
  : > "$TMP/affected-events"
  (cd "$gate" && OMS_CHECK_JOBS=2 OMS_TEST_AFFECTED_EVENTS="$TMP/affected-events" \
    OMS_TEST_AFFECTED_LOG="$log" GITHUB_STEP_SUMMARY="$TMP/parallel-summary.md" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      > "$TMP/parallel-output" 2>&1) || fail "parallel affected gate failed"
  python3 - "$TMP/affected-events" <<'PYTEST' || fail "selected suites serialized or exceeded their budget"
import pathlib, sys
active = peak = 0
rows = pathlib.Path(sys.argv[1]).read_text().splitlines()
for row in rows:
    active += 1 if row.startswith('start ') else -1
    peak = max(peak, active)
assert len(rows) == 4 and active == 0 and peak == 2, (rows, peak)
PYTEST
  [ "$(grep -Fxc 'operator-args:--section section-shell,section-python' "$log")" = 1 ] ||
    fail "affected selection did not run exactly the mapped operator sections"
  grep -Fq 'operator-tools' "$TMP/parallel-summary.md" || fail "parallel operator timing lost"
  grep -Fq 'affected:tests/leaf-smoke.sh' "$TMP/parallel-output" || fail "parallel stage renamed"
  if (cd "$gate" && OMS_TEST_AFFECTED_FAIL=1 OMS_TEST_AFFECTED_LOG="$log" \
      bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
        > "$TMP/parallel-failure" 2>&1); then
    fail "affected parallel runner swallowed a selected suite failure"
  fi
  grep -Fq 'check: operator-tools FAILED' "$TMP/parallel-failure" || fail "failing stage name lost"
  grep -Fq 'slow-stage:' "$TMP/parallel-failure" || fail "failing run lost timing records"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=full GITHUB_STEP_SUMMARY="$TMP/full-summary.md" bash scripts/check.sh --affected \
    --changed-from "$base" --changed-to "$head" --ci-output "$TMP/ci-full") ||
    fail "CI selector tried to execute full lanes inline"
  grep -Fxq 'mode=full' "$TMP/ci-full" || fail "CI full mode was not recorded"
  grep -Fq 'unmatched:lib/leaf.py' "$TMP/full-summary.md" || fail "CI summary omitted full fallback reason"

  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=full \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" --list-stages)" ||
    fail "affected gate rejected a fail-open graph plan"
  printf '%s\n' "$out" | grep >/dev/null -Fx autonomy-hook ||
    fail "uncertain affected plan did not fall back to the full focused gate: $out"
  printf '%s\n' "$out" | grep >/dev/null -Fx shellcheck ||
    fail "full affected fallback omitted lint: $out"
  printf '%s\n' "$out" | grep >/dev/null -Fx artifact-supersession ||
    fail "fail-open affected stage listing stopped before the full focused gate: $out"
  # The uncapped full plan runs all of scripts-smoke, so the whole-tree
  # invariants must not be staged a second time.
  if grep -Fxq repo-invariants <<< "$out"; then fail "full plan staged repo-invariants twice: $out"; fi
  # The protected-push gate is standalone --quick, which must carry them too.
  out="$(cd "$gate" && bash scripts/check.sh --quick --changed-from "$base" \
    --changed-to "$head" --list-stages 2>/dev/null)" || fail "quick gate rejected the range"
  [ "$(grep -Fxc repo-invariants <<< "$out")" = 1 ] || fail "quick gate did not stage repo-invariants once: $out"
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=error \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" --list-stages 2>/dev/null)" ||
    fail "affected gate propagated a graph-reader failure"
  printf '%s\n' "$out" | grep >/dev/null -Fx autonomy-hook ||
    fail "graph-reader failure did not fail open to the focused gate: $out"


  boundary_base="$head"
  printf '# selector changed\n' >> "$gate/scripts/graph.sh"
  git -C "$gate" add scripts/graph.sh
  git -C "$gate" commit -qm selector-boundary
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && bash scripts/check.sh --affected \
    --changed-from "$boundary_base" --changed-to "$head" --list-stages 2>/dev/null)" ||
    fail "affected gate rejected its own selector-boundary change"
  printf '%s\n' "$out" | grep >/dev/null -Fx autonomy-hook ||
    fail "check.sh trusted the graph to validate its own selector change: $out"
  # oms land's default caps that fallback: whole lint, the quick lane and a
  # real Bash 3.2 parse here; the focused and smoke lanes are CI's.
  out="$(cd "$gate" && bash scripts/check.sh --affected --ci-parity \
    --changed-from "$boundary_base" --changed-to "$head" --list-stages 2>/dev/null)" ||
    fail "ci-parity gate rejected a selector-boundary change"
  for stage_name in shellcheck source-distribution bash32-parse repo-invariants; do
    grep -Fxq "$stage_name" <<< "$out" || fail "capped fallback omitted $stage_name: $out"
  done
  if grep -Eqx 'autonomy-hook|shellcheck-changed|operator-tools' <<< "$out"; then
    fail "capped fallback ran full lanes, repeated lint or an untouched routing suite: $out"
  fi
  # A tier-routing input adds the suite that routes among installed providers
  # (f698ac9 shipped a fixture that passed only where a provider CLI existed).
  boundary_base="$head"
  printf '# routing changed\n' >> "$gate/scripts/lib/panel_routing.py"
  git -C "$gate" add scripts/lib/panel_routing.py
  git -C "$gate" commit -qm routing
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && bash scripts/check.sh --affected --ci-parity \
    --changed-from "$boundary_base" --changed-to "$head" --list-stages 2>/dev/null)" ||
    fail "ci-parity gate rejected a routing change"
  grep -Fxq operator-tools <<< "$out" || fail "a routing change skipped provider-less operator-tools: $out"
  # A selected test sees PATH as a CI host does: no provider CLI, every other tool.
  # codex and the helper share a directory reached through a relative PATH
  # entry, so its shadow links must still resolve from the shadow directory.
  mkdir -p "$TMP/parity-bin" "$TMP/parity-rel"
  for provider_cli in cursor-agent oms-agent-adapter-parity; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/parity-bin/$provider_cli"
  done
  printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/parity-rel/codex"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/parity-rel/parity-helper"
  chmod +x "$TMP/parity-rel/"*
  parity_rel="$(python3 -c 'import os, sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' "$TMP/parity-rel" "$gate")"
  printf '#!/usr/bin/env bash\nprintf "docker:%%s\\n" "$*" >> "$OMS_TEST_AFFECTED_LOG"\n' > "$TMP/parity-bin/fake-docker"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/parity-bin/fake-shellcheck"
  chmod +x "$TMP/parity-bin/"*
  cat > "$gate/tests/leaf-smoke.sh" <<'EOF'
#!/usr/bin/env bash
printf 'shell-file\n' >> "$OMS_TEST_AFFECTED_LOG"
printf 'codex:%s cursor:%s adapter:%s helper:%s\n' "$(command -v codex || echo absent)" \
  "$(command -v cursor-agent || echo absent)" "$(command -v oms-agent-adapter-parity || echo absent)" \
  "$(command -v parity-helper >/dev/null && echo present)" >> "$OMS_TEST_AFFECTED_LOG"
EOF
  git -C "$gate" add tests/leaf-smoke.sh
  git -C "$gate" commit -qm parity-leaf
  base="$head"
  head="$(git -C "$gate" rev-parse HEAD)"
  : > "$log"
  out="$(cd "$gate" && PATH="$parity_rel:$TMP/parity-bin:$PATH" OMS_TEST_AFFECTED_LOG="$log" \
    OMS_SHELLCHECK_BIN="$TMP/parity-bin/fake-shellcheck" OMS_DOCKER_BIN="$TMP/parity-bin/fake-docker" \
    bash scripts/check.sh --affected --ci-parity --changed-from "$base" --changed-to "$head" 2>&1)" ||
    fail "ci-parity gate failed: $out"
  grep -Fxq 'check: ok (affected tests; mode=affected; ci-parity)' <<< "$out" || fail "ci-parity verdict missing: $out"
  # Git Bash cannot shadow PATH with links and only Linux parses through docker
  # (macOS has /bin/bash); those hosts must say so instead.
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      grep -Fq 'skip: provider-less PATH' <<< "$out" || fail "Windows provider-less skip was silent: $out" ;;
    *) grep -Fxq 'codex:absent cursor:absent adapter:absent helper:present' "$log" ||
         fail "ci-parity PATH kept a provider CLI or lost a tool: $(cat "$log")" ;;
  esac
  if [ "$(uname -s)" = Linux ]; then
    grep -Eq '^docker:run .* bash:3\.2 .* tests/leaf-smoke\.sh$' "$log" ||
      fail "ci-parity did not parse the changed shell file with Bash 3.2: $(cat "$log")"
    out="$(cd "$gate" && OMS_TEST_AFFECTED_LOG="$log" OMS_SHELLCHECK_BIN="$TMP/parity-bin/fake-shellcheck" \
      OMS_DOCKER_BIN="$TMP/parity-bin/absent-docker" \
      bash scripts/check.sh --affected --ci-parity --changed-from "$base" --changed-to "$head" 2>&1)" ||
      fail "ci-parity without docker failed instead of skipping: $out"
    grep -Fq 'skip: bash32-parse (docker is not installed)' <<< "$out" || fail "absent docker skip was silent: $out"
  fi

  # Prose changes need no graph or unrelated runtime suites. Agent policy is
  # not prose-only, and the changed tree must still match what gets checked.
  base="$head"
  printf 'Documentation\n' > "$gate/README.md"
  git -C "$gate" add README.md
  git -C "$gate" commit -qm docs
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=error bash scripts/check.sh --affected \
    --changed-from "$base" --changed-to "$head" --list-stages 2>/dev/null)"
  # Whole-tree invariants run for every affected plan (ba477b8 passed the
  # gate while the scrubber test over the tracked tree failed in CI).
  [ "$out" = "$(printf 'source-distribution\nrepo-invariants')" ] || fail "docs expanded into unrelated checks: $out"
  printf 'dirty\n' >> "$gate/README.md"
  out="$(cd "$gate" && bash scripts/check.sh --affected --print-affected-mode \
    --changed-from "$base" --changed-to "$head" 2>/dev/null)"
  [ "$out" = full ] || fail "docs shortcut trusted an uncommitted change"
  git -C "$gate" add README.md
  git -C "$gate" commit -qm docs-followup
  head="$(git -C "$gate" rev-parse HEAD)"
  printf 'untracked\n' > "$gate/lib/new.py"
  out="$(cd "$gate" && bash scripts/check.sh --affected --print-affected-mode \
    --changed-from "$base" --changed-to "$head" 2>/dev/null)"
  [ "$out" = full ] || fail "docs shortcut ignored untracked code"
  git -C "$gate" add lib/new.py
  git -C "$gate" commit -qm new-code
  mkdir -p "$gate/docs"
  for policy_path in AGENTS.md docs/AGENTS.md scripts/update.sh scripts/lib/durable-jsonl.py; do
    base="$(git -C "$gate" rev-parse HEAD)"
    printf '# policy\n' > "$gate/$policy_path"
    git -C "$gate" add "$policy_path"
    git -C "$gate" commit -qm policy
    head="$(git -C "$gate" rev-parse HEAD)"
    out="$(cd "$gate" && bash scripts/check.sh --affected --print-affected-mode \
      --changed-from "$base" --changed-to "$head" 2>/dev/null)"
    [ "$out" = full ] || fail "critical boundary escaped full verification: $policy_path"
  done
  base="$head"
  printf '# shared runtime contract\n' > "$gate/scripts/lib/oms-common.sh"
  git -C "$gate" add scripts/lib/oms-common.sh
  git -C "$gate" commit -qm common-runtime-contract
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=affected bash scripts/check.sh --affected \
    --print-affected-mode --changed-from "$base" --changed-to "$head" 2>/dev/null)" ||
    fail "affected gate rejected the common runtime contract change"
  [ "$out" = full ] || fail "shared runtime contract escaped full verification: $out"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=affected GITHUB_STEP_SUMMARY="$TMP/common-summary.md" \
    OMS_TEST_AFFECTED_LOG="$TMP/common-inline.log" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      --ci-output "$TMP/common-ci-mode" >/dev/null) ||
    fail "CI selector tried to execute full lanes inline for common runtime contract change"
  grep -Fxq 'mode=full' "$TMP/common-ci-mode" || fail "common runtime CI mode was not recorded as full"
  grep -Fq 'contract-boundary:scripts/lib/oms-common.sh' "$TMP/common-summary.md" ||
    fail "common runtime CI summary omitted the contract boundary reason"
  [ ! -s "$TMP/common-inline.log" ] ||
    fail "common runtime CI selector executed inline affected checks: $(cat "$TMP/common-inline.log")"
  base="$head"
  mkdir -p "$gate/scripts/lib"
  printf '# panel view\n' > "$gate/scripts/lib/room_view.py"
  git -C "$gate" add scripts/lib/room_view.py
  git -C "$gate" commit -qm panel-view
  head="$(git -C "$gate" rev-parse HEAD)"
  (cd "$gate" && OMS_TEST_GRAPH_MODE=affected GITHUB_STEP_SUMMARY="$TMP/panel-summary.md" \
    OMS_TEST_AFFECTED_LOG="$TMP/panel-inline.log" \
    bash scripts/check.sh --affected --changed-from "$base" --changed-to "$head" \
      --ci-output "$TMP/panel-ci-mode" >/dev/null) ||
    fail "CI selector rejected the panel view change"
  grep -Fxq 'mode=affected' "$TMP/panel-ci-mode" || fail "panel cost alone forced unrelated full lanes"
  grep -Fxq 'operator=section-shell,section-python' "$TMP/panel-ci-mode" ||
    fail "panel selection did not hand its mapped operator sections to CI"
  ! grep -Fq 'operator-args:' "$TMP/panel-inline.log" ||
    fail "panel selection ran operator-tools in the planning job"
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=full bash scripts/check.sh --affected \
    --print-affected-mode --changed-from "$base" --changed-to "$head" 2>/dev/null)" ||
    fail "uncertain panel plan failed"
  [ "$out" = full ] || fail "uncertain panel plan lost conservative full fallback"
  base="$head"
  git -C "$gate" mv lib/new.py docs/code.md
  git -C "$gate" commit -qm rename-code
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=error bash scripts/check.sh --affected \
    --print-affected-mode --changed-from "$base" --changed-to "$head" 2>/dev/null)"
  [ "$out" = full ] || fail "renamed code was treated as ordinary docs"
  for policy_path in custom-skills/example/SKILL.md templates/project-skills/example/SKILL.md templates/project-example.md; do
    base="$head"
    mkdir -p "$(dirname "$gate/$policy_path")"
    printf '{}\n' > "$gate/$policy_path"
    git -C "$gate" add "$policy_path"
    git -C "$gate" commit -qm scoped-contract
    head="$(git -C "$gate" rev-parse HEAD)"
    out="$(cd "$gate" && OMS_TEST_GRAPH_MODE=error bash scripts/check.sh --affected \
      --changed-from "$base" --changed-to "$head" --list-stages 2>/dev/null)"
    case "$policy_path" in
      *)
        grep -Fxq skill-manifest <<< "$out" || fail "direct guidance checks missing: $out"
        if grep -Eq 'prompt-budget|skill-lifecycle|functional-evolution' <<< "$out"; then
          fail "guidance checks run unrelated execution suites: $out"
        fi
        ;;
    esac
    if grep -Fxq install-lifecycle <<< "$out"; then fail "scoped contract expanded to unrelated lifecycle tests"; fi
    [ "$(grep -Fxc repo-invariants <<< "$out")" = 1 ] || fail "affected plan did not stage repo-invariants once: $out"
  done
  base="$head"
  git -C "$gate" rm -q templates/project-skills/example/SKILL.md
  git -C "$gate" commit -qm remove-template
  head="$(git -C "$gate" rev-parse HEAD)"
  out="$(cd "$gate" && bash scripts/check.sh --affected --changed-from "$base" \
    --changed-to "$head" --list-stages 2>/dev/null)"
  grep -Fxq skill-manifest <<< "$out" || fail "deleted guidance skipped resource validation"
}

test_uninstall_stops_before_unlink_on_removal_failure() {
  local checkout="$TMP/uninstall-removal-failure"
  local log="$TMP/uninstall-removal-failure.log"
  local out="$TMP/uninstall-removal-failure.out"
  local status=0 name

  mkdir -p "$checkout/scripts/lib" "$checkout/.git" "$TMP/uninstall-home"
  cp "$ROOT/scripts/uninstall.sh" "$checkout/scripts/uninstall.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$checkout/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$checkout/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$checkout/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$checkout/scripts/lib/poll.sh"
  for name in uninstall-autoupdate tick install-claude-hooks install-codex-plugin install-mcp install-agy-plugin provider-permissions unlink journal; do
    cat > "$checkout/scripts/$name.sh" <<'EOF'
#!/usr/bin/env bash
name="${0##*/}"
printf '%s\n' "$name" >> "$OMS_TEST_OPS_LOG"
case ",${OMS_TEST_REMOVAL_FAILS:-}," in
  *",$name,"*) exit 23 ;;
esac
EOF
    chmod +x "$checkout/scripts/$name.sh"
  done

  HOME="$TMP/uninstall-home" XDG_CONFIG_HOME="$TMP/uninstall-home/.config" \
    OMS_TEST_OPS_LOG="$log" \
    OMS_TEST_REMOVAL_FAILS="install-claude-hooks.sh,install-mcp.sh,provider-permissions.sh" \
    "$checkout/scripts/uninstall.sh" --yes --purge --purge-dirty > "$out" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "uninstall ignored integration removal failures"
  [ -d "$checkout" ] || fail "uninstall purged its recovery checkout after a removal failure"
  for name in uninstall-autoupdate.sh tick.sh install-claude-hooks.sh install-codex-plugin.sh install-mcp.sh install-agy-plugin.sh provider-permissions.sh; do
    grep -Fxq "$name" "$log" 2>/dev/null ||
      fail "uninstall stopped before attempting $name: $(tail -n 25 "$out" 2>/dev/null)"
  done
  if grep -Eq '^(unlink|journal)\.sh$' "$log"; then
    fail "uninstall crossed the unlink/purge boundary after a removal failure"
  fi
  if grep -Fq 'purged ' "$out"; then
    fail "uninstall printed purge success after a removal failure"
  fi

  : > "$log"
  : > "$out"
  status=0
  HOME="$TMP/uninstall-home" XDG_CONFIG_HOME="$TMP/uninstall-home/.config" \
    OMS_TEST_OPS_LOG="$log" \
    OMS_TEST_REMOVAL_FAILS="journal.sh" \
    "$checkout/scripts/uninstall.sh" --yes --purge --purge-dirty > "$out" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "uninstall ignored Work Journal disconnect failure"
  [ -d "$checkout" ] || fail "uninstall purged its recovery checkout after Journal failure"
  grep -Fxq "journal.sh" "$log" || fail "purge did not attempt managed Work Journal cleanup"
  if grep -Fxq "unlink.sh" "$log"; then
    fail "uninstall crossed the unlink boundary after Journal failure"
  fi
  if grep -Fq 'purged ' "$out"; then
    fail "uninstall printed purge success after Journal failure"
  fi

  : > "$log"
  : > "$out"
  status=0
  # No controlling terminal (CI, containers): /dev/tty stays readable by mode
  # but cannot be opened, so purge must refuse before removing anything.
  HOME="$TMP/uninstall-home" XDG_CONFIG_HOME="$TMP/uninstall-home/.config" \
    OMS_TEST_OPS_LOG="$log" python3 -c 'import os, sys; os.setsid(); os.execv(sys.argv[1], sys.argv[1:])' \
    "$checkout/scripts/uninstall.sh" --purge --purge-dirty > "$out" 2>&1 </dev/null || status=$?
  [ "$status" -ne 0 ] || fail "purge without a terminal or --yes reported success: $(cat "$out")"
  grep -Fq 'no tty; rerun with --yes' "$out" || fail "purge without a terminal must say how to confirm: $(cat "$out")"
  [ ! -s "$log" ] || fail "purge without confirmation removed integrations: $(cat "$log")"
  [ -d "$checkout" ] || fail "purge without confirmation deleted the checkout"
}

test_install_owner_guards_and_stale_status() {
  local home_dir="$TMP/install-owner-home"
  local receipt="$home_dir/install.json"
  local canonical="$TMP/canonical-install"
  local foreign="$TMP/foreign-install"
  local bin_dir="$TMP/install-owner-bin"
  local out status=0

  mkdir -p "$canonical" "$foreign/scripts/lib" "$foreign/.git" "$bin_dir"
  write_install_receipt "$receipt" "$canonical"
  cp "$ROOT/scripts/update.sh" "$foreign/scripts/update.sh"
  cp "$ROOT/scripts/lib/install-contract.sh" "$foreign/scripts/lib/install-contract.sh"
  cp "$ROOT/scripts/lib/install-lifecycle-lock.sh" "$foreign/scripts/lib/install-lifecycle-lock.sh"
  cp "$ROOT/scripts/lib/file-lock.sh" "$foreign/scripts/lib/file-lock.sh"
  cp "$ROOT/scripts/lib/poll.sh" "$foreign/scripts/lib/poll.sh"
  cat > "$foreign/scripts/link.sh" <<'EOF'
#!/usr/bin/env bash
touch "$OMS_TEST_MUTATION_MARKER"
EOF
  chmod +x "$foreign/scripts/update.sh" "$foreign/scripts/link.sh"
  cat > "$bin_dir/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *rev-parse*) echo abcdef0 ;;
esac
exit 0
EOF
  chmod +x "$bin_dir/git"
  status=0
  HOME="$home_dir" PATH="$bin_dir:/usr/bin:/bin" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_TEST_MUTATION_MARKER="$TMP/update-mutated" OH_MY_SETTING_CLAUDE_HOOKS=0 \
    OH_MY_SETTING_CODEX_PLUGIN=0 OH_MY_SETTING_AUTO_UPDATE=0 \
    "$foreign/scripts/update.sh" --no-tools --no-doctor > "$TMP/foreign-update.out" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "foreign update should refuse canonical ownership takeover"
  [ ! -e "$TMP/update-mutated" ] || fail "foreign update mutated install state"

  for name in uninstall-autoupdate install-claude-hooks install-codex-plugin unlink; do
    cat > "$foreign/scripts/$name.sh" <<'EOF'
#!/usr/bin/env bash
echo mutation >> "$OMS_TEST_OPS_LOG"
EOF
    chmod +x "$foreign/scripts/$name.sh"
  done
  cp "$ROOT/scripts/uninstall.sh" "$foreign/scripts/uninstall.sh"
  chmod +x "$foreign/scripts/uninstall.sh"
  status=0
  HOME="$home_dir" OMS_INSTALL_RECEIPT="$receipt" \
    OMS_TEST_OPS_LOG="$TMP/uninstall-ops" \
    "$foreign/scripts/uninstall.sh" --yes > "$TMP/foreign-uninstall.out" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "foreign uninstall should refuse canonical install mutation"
  [ ! -e "$TMP/uninstall-ops" ] || fail "foreign uninstall ran global removal steps"

  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "$OMS_TEST_CODEX_LOG"
EOF
  chmod +x "$bin_dir/codex"
  status=0
  HOME="$home_dir" PATH="$bin_dir:/usr/bin:/bin" OMS_INSTALL_LOCK_HELD=1 \
    OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_CODEX_LOG="$TMP/codex-remove.log" \
    "$ROOT/scripts/install-codex-plugin.sh" --remove > "$TMP/foreign-plugin.out" 2>&1 || status=$?
  [ "$status" -ne 0 ] || fail "foreign plugin removal should refuse canonical install mutation"
  [ ! -e "$TMP/codex-remove.log" ] || fail "foreign plugin removal invoked codex"

  git -C "$canonical" init -q
  git -C "$canonical" config user.name test
  git -C "$canonical" config user.email test@example.com
  printf 'canonical\n' > "$canonical/file"
  git -C "$canonical" add file
  git -C "$canonical" commit -qm initial
  write_install_receipt "$receipt" "$canonical" "$(git -C "$canonical" rev-parse HEAD)"
  cat > "$TMP/stale-auto-update.status" <<'EOF'
last_run=2026-07-10T00:00:00Z
mode=check
status=up_to_date
message=already up to date
local=deadbeef
remote=deadbeef
upstream=origin/main
EOF
  out="$(HOME="$home_dir" NVM_DIR="$home_dir/.nvm" PATH="/usr/bin:/bin" OMS_INSTALL_RECEIPT="$receipt" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$TMP/stale-auto-update.status" \
    "$ROOT/scripts/status.sh" 2>/dev/null)"
  printf '%s' "$out" | grep >/dev/null -F -- '- status: stale' ||
    fail "status should flag auto-update state from a different canonical HEAD"
  printf '%s' "$out" | grep >/dev/null -F -- '- recorded_status: up_to_date' ||
    fail "stale status should preserve the recorded auto-update conclusion"
  printf '%s' "$out" | grep >/dev/null -F -- "- installed_actual_commit: $(git -C "$canonical" rev-parse HEAD)" ||
    fail "status omitted live installed revision"
  mkdir -p "$canonical/scripts" "$canonical/rules"
  touch "$canonical/scripts/oms" "$canonical/rules/global-AGENTS.md"
  git -C "$canonical" add .
  git -C "$canonical" commit -qm markers
  out="$(cd "$canonical" && HOME="$home_dir" OMS_INSTALL_RECEIPT="$receipt" "$ROOT/scripts/status.sh" 2>/dev/null)"
  printf '%s' "$out" | grep >/dev/null -F -- '- alignment: in-sync' || fail "same clean checkout should be in sync"
  printf 'edited\n' >> "$canonical/file"
  out="$(cd "$canonical" && HOME="$home_dir" OMS_INSTALL_RECEIPT="$receipt" "$ROOT/scripts/status.sh" 2>/dev/null)"
  printf '%s' "$out" | grep >/dev/null -F -- '- alignment: installed-uncommitted' || fail "live edits should not be reported as a clean installation"
}

test_checkout_runtime_is_current
test_smoke_shards_partition_every_definition
test_artifact_resolution_and_dashboard_statuses
test_artifact_retention_corruption_and_source_tracking
test_large_artifact_index_compacts_before_append
test_smoke_runner_tail_and_signal_cleanup
test_smoke_runner_reports_bounded_opt_in_timings
test_affected_gate_runs_positive_evidence_and_fails_open
test_invariants_gate_checks_committed_staged_and_worktree_sources
test_gate_fingerprints_full_oms_state
test_uninstall_stops_before_unlink_on_removal_failure
test_install_owner_guards_and_stale_status

echo "harness-enhancements-smoke: ok"
