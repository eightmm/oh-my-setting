#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
  [ "$(grep -Rh 'uses: actions/checkout@v7' "$ROOT/.github/workflows" | wc -l | tr -d ' ')" = "5" ] ||
    fail "all five checkout steps should use actions/checkout@v7"
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

  "$ROOT/scripts/repo-state.sh" --repo "$repo" --json > "$TMP/state.json"
  python3 - "$TMP/state.json" <<'PY' || fail "repo-state JSON status counts are wrong"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["total"] == 4
assert state["outcomes_total"] == 3
assert state["counts"] == {"success": 1, "unresolved": 1, "resolved": 1}
assert [row["status"] for row in state["latest"]] == ["success", "resolved", "unresolved"]
PY
  "$ROOT/scripts/repo-state.sh" --repo "$repo" > "$TMP/state.txt"
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
  "$ROOT/scripts/repo-state.sh" --repo "$malformed_repo" --json > "$TMP/malformed-state.json"
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
  "$ROOT/scripts/repo-state.sh" --repo "$invalid_exit_repo" --json > "$TMP/invalid-exit-state.json"
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
  "$ROOT/scripts/repo-state.sh" --repo "$repo" --json > "$TMP/corrupt-state.json"
  python3 - "$TMP/corrupt-state.json" <<'PY' || fail "repo-state hid artifact corruption"
import json, sys
state = json.load(open(sys.argv[1]))["artifacts"]
assert state["invalid_rows"] == 1
assert state["healthy"] is False
PY
  "$ROOT/scripts/repo-state.sh" --repo "$repo" > "$TMP/corrupt-state.txt"
  grep -Fq 'CORRUPT invalid_rows=1' "$TMP/corrupt-state.txt" ||
    fail "repo-state text should make artifact corruption visible"

  printf '%s\n' '{"schema":1,"kind":"call","exit":0}' > "$index"
  "$ROOT/scripts/repo-state.sh" --repo "$repo" --json > "$TMP/invalid-contract-state.json"
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

test_smoke_runner_tail_and_signal_cleanup() {
  local suite="$TMP/overlap-smoke.sh"
  local out="$TMP/overlap.out"

  cat > "$suite" <<'EOF'
test_first() {
  echo first
}
test_second() {
  echo second
}
# SMOKE_TEST_CALLS_BEGIN
  test_first # legacy formatting must be discarded
test_second
# SMOKE_TEST_CALLS_END
echo done
EOF
  OMS_SMOKE_SUITE="$suite" "$ROOT/tests/run-smoke-shard.sh" --shard 2/2 > "$out"
  grep -Fq 'second' "$out" || fail "owning shard did not execute its test"
  if grep -Fq 'first' "$out"; then
    fail "legacy call block formatting caused cross-shard duplicate execution"
  fi

  local signal_suite="$TMP/signal-smoke.sh"
  local signal_tmp="$TMP/signal-tmp"
  local leader child status=0
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
# SMOKE_TEST_CALLS_BEGIN
test_slow_one
test_slow_two
# SMOKE_TEST_CALLS_END
echo done
EOF
  TMPDIR="$signal_tmp" OMS_TEST_PID_FILE="$signal_tmp/pids" OMS_SMOKE_SUITE="$signal_suite" \
    "$ROOT/tests/run-smoke-shard.sh" --jobs 2 >/dev/null 2>&1 &
  leader=$!
  for _ in $(seq 1 100); do
    [ -f "$signal_tmp/pids" ] && [ "$(wc -l < "$signal_tmp/pids")" -ge 4 ] && break
    sleep 0.05
  done
  [ -f "$signal_tmp/pids" ] || fail "parallel smoke workers did not start"
  kill -TERM "$leader"
  wait "$leader" || status=$?
  [ "$status" -eq 143 ] || fail "TERM should propagate as 143, got $status"
  sleep 0.2
  while IFS= read -r child; do
    if kill -0 "$child" 2>/dev/null; then
      kill -KILL "$child" 2>/dev/null || true
      fail "parallel smoke child survived parent TERM: $child"
    fi
  done < "$signal_tmp/pids"
  [ -z "$(find "$signal_tmp" -maxdepth 1 -type d -name 'oms-smoke-shards.*' -print -quit)" ] ||
    fail "parallel smoke log directory survived parent TERM"
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
  PATH="$bin_dir:/usr/bin:/bin" OMS_INSTALL_RECEIPT="$receipt" \
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
  OMS_INSTALL_RECEIPT="$receipt" OMS_TEST_OPS_LOG="$TMP/uninstall-ops" \
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
  printf '%s' "$out" | grep -Fq -- '- status: stale' ||
    fail "status should flag auto-update state from a different canonical HEAD"
  printf '%s' "$out" | grep -Fq -- '- recorded_status: up_to_date' ||
    fail "stale status should preserve the recorded auto-update conclusion"
}

test_checkout_runtime_is_current
test_smoke_shards_partition_every_definition
test_artifact_resolution_and_dashboard_statuses
test_artifact_retention_corruption_and_source_tracking
test_smoke_runner_tail_and_signal_cleanup
test_install_owner_guards_and_stale_status

echo "harness-enhancements-smoke: ok"
