#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-work-journal-smoke.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/tmp" "$TMP/config" "$TMP/locks"
export HOME="$TMP/home"
export TMPDIR="$TMP/tmp"
export XDG_CONFIG_HOME="$TMP/config"
export GIT_CONFIG_NOSYSTEM=1
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1
export OMS_WORK_JOURNAL_TIMEZONE=UTC
unset OMS_WORK_JOURNAL_NOTION_TOKEN
unset OMS_WORK_JOURNAL_NOTION_DATABASE_ID
unset OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID

# Keep the repository gate to one Work Journal stage. This boundary test owns
# the focused Python contracts as well as the cross-process lifecycle fixture.
python3 "$ROOT/tests/work_journal_test.py"
python3 "$ROOT/tests/notion_journal_test.py"

# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.name "Test"
git -C "$repo" config user.email "test@example.com"
printf 'seed\n' > "$repo/seed.txt"
git -C "$repo" add seed.txt
git -C "$repo" commit -qm "seed"

make_row() {
  local path="$1"
  local id="$2"
  python3 - "$path" "$id" <<'PY'
import json
import sys

path, ident = sys.argv[1:]
row = {
    "schema": 1,
    "id": ident,
    "ts": "2026-07-31T02:00:00Z",
    "exit": 0,
    "duration_s": 1,
    "git_sha": "abc123",
    "gate": "passed",
    "note": "focused check",
    "cmd": ["bash", "scripts/check.sh"],
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
}

row="$TMP/row.json"
make_row "$row" "run-one"
work_journal_observe "$repo" run-ledger "$row"
work_journal_observe "$repo" run-ledger "$row"

count="$(wc -l < "$repo/.oms/work-journal/events.jsonl" | tr -d ' ')"
[ "$count" = 1 ] || fail "same source row was not idempotent ($count events)"
[ -f "$repo/.oms/work-journal/daily/2026-07-31.md" ] ||
  fail "daily summary was not materialized"
[ -f "$repo/.oms/work-journal/weekly/2026-W31.md" ] ||
  fail "weekly summary was not materialized"

# Durable observations stay local while the agent works. The top-level finish
# boundary owns the only remote sync and publishes only today's daily summary.
fake_boundary_journal="$TMP/fake-boundary-journal.py"
boundary_calls="$TMP/boundary-calls"
cat > "$fake_boundary_journal" <<'PY'
import os
import pathlib
import sys

if sys.argv[1] == "sync":
    with pathlib.Path(os.environ["OMS_TEST_BOUNDARY_CALLS"]).open("a") as handle:
        handle.write(" ".join(sys.argv[1:]) + "\n")
PY
access_name="OMS_WORK_JOURNAL_NOTION_"'TOKEN'
export "$access_name=test-credential"
export OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID=fake-target
export OMS_WORK_JOURNAL_PYTHON="$fake_boundary_journal"
export OMS_TEST_BOUNDARY_CALLS="$boundary_calls"
work_journal_observe "$repo" run-ledger "$row"
[ ! -e "$boundary_calls" ] || fail "work-time observation called the remote mirror"
work_journal_finish "$repo"
[ "$(wc -l < "$boundary_calls" | tr -d ' ')" = 1 ] ||
  fail "finish boundary should perform exactly one remote sync"
grep -Fq -- '--force --today' "$boundary_calls" ||
  fail "finish boundary did not limit sync to today's daily summary"
unset "$access_name"
unset OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID
unset OMS_WORK_JOURNAL_PYTHON
unset OMS_TEST_BOUNDARY_CALLS

# Distinct processes append concurrently through the same existing lock helper.
pids=""
i=1
while [ "$i" -le 12 ]; do
  r="$TMP/row-$i.json"
  make_row "$r" "parallel-$i"
  (work_journal_observe "$repo" run-ledger "$r") &
  pids="$pids $!"
  i=$((i + 1))
done
for pid in $pids; do
  wait "$pid"
done

python3 - "$repo" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1]) / ".oms" / "work-journal"
events = (root / "events.jsonl").read_text(encoding="utf-8").splitlines()
assert len(events) == 13, len(events)
ids = set()
for line in events:
    row = json.loads(line)
    assert row["event_id"] not in ids
    ids.add(row["event_id"])
index = json.loads((root / "index.json").read_text(encoding="utf-8"))
assert index["event_count"] == 13
assert not list(root.rglob("*.tmp-*"))
PY

# Explicit remote sync owns a separate, non-blocking lock. Local observation
# remains independent of that network lock.
fake_journal="$TMP/fake-work-journal.py"
sync_started="$TMP/sync-started"
sync_release="$TMP/sync-release"
second_done="$TMP/second-done"
cat > "$fake_journal" <<'PY'
import os
import pathlib
import sys
import time

if sys.argv[1] == "sync":
    pathlib.Path(os.environ["OMS_TEST_SYNC_STARTED"]).touch()
    release = pathlib.Path(os.environ["OMS_TEST_SYNC_RELEASE"])
    deadline = time.time() + 15
    while not release.exists() and time.time() < deadline:
        time.sleep(0.05)
PY
access_name="OMS_WORK_JOURNAL_NOTION_"'TOKEN'
export "$access_name=test-credential"
export OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID=fake-target
export OMS_WORK_JOURNAL_PYTHON="$fake_journal"
export OMS_TEST_SYNC_STARTED="$sync_started"
export OMS_TEST_SYNC_RELEASE="$sync_release"
(work_journal_sync "$repo") &
first_sync_pid=$!
i=0
while [ ! -f "$sync_started" ] && [ "$i" -lt 100 ]; do
  sleep 0.05
  i=$((i + 1))
done
[ -f "$sync_started" ] || fail "fake remote sync did not start"
(work_journal_observe "$repo" run-ledger "$row"; touch "$second_done") &
second_sync_pid=$!
i=0
while [ ! -f "$second_done" ] && [ "$i" -lt 40 ]; do
  sleep 0.05
  i=$((i + 1))
done
if [ ! -f "$second_done" ]; then
  touch "$sync_release"
  wait "$first_sync_pid" "$second_sync_pid"
  fail "local journal observation queued behind remote sync"
fi
touch "$sync_release"
wait "$first_sync_pid" "$second_sync_pid"
unset "$access_name"
unset OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID
unset OMS_WORK_JOURNAL_PYTHON
unset OMS_TEST_SYNC_STARTED
unset OMS_TEST_SYNC_RELEASE

# Disabled and reentrant observers are exact no-ops.
before="$(wc -l < "$repo/.oms/work-journal/events.jsonl" | tr -d ' ')"
OMS_WORK_JOURNAL=0 work_journal_observe "$repo" run-ledger "$row"
OMS_WORK_JOURNAL_ACTIVE=1 work_journal_observe "$repo" run-ledger "$row"
after="$(wc -l < "$repo/.oms/work-journal/events.jsonl" | tr -d ' ')"
[ "$before" = "$after" ] || fail "disabled/reentrant observer wrote an event"

# A real authoritative ledger write is captured automatically after it commits.
lifecycle_repo="$TMP/lifecycle-repo"
mkdir -p "$lifecycle_repo"
git -C "$lifecycle_repo" init -q
git -C "$lifecycle_repo" config user.name "Test"
git -C "$lifecycle_repo" config user.email "test@example.com"
printf 'seed\n' > "$lifecycle_repo/seed.txt"
git -C "$lifecycle_repo" add seed.txt
git -C "$lifecycle_repo" commit -qm "seed"
(
  cd "$lifecycle_repo"
  "$ROOT/scripts/run-ledger.sh" --file "$lifecycle_repo/ledger.jsonl" -- sh -c 'exit 0'
) >/dev/null 2>&1
(
  cd "$lifecycle_repo"
  OMS_OPERATION_ID=shared-operation \
    "$ROOT/scripts/run-ledger.sh" --file "$lifecycle_repo/ledger.jsonl" -- \
    sh -c 'exit 0'
  OMS_OPERATION_ID=shared-operation \
    "$ROOT/scripts/run-ledger.sh" --file "$lifecycle_repo/ledger.jsonl" -- \
    sh -c 'exit 0'
) >/dev/null 2>&1
python3 - "$lifecycle_repo" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1]) / ".oms" / "work-journal" / "events.jsonl"
rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
assert len(rows) == 3
assert all(row["source"]["type"] == "run-ledger" for row in rows)
assert all(row["source"]["id"].startswith("ledger-") for row in rows)
assert len({row["source"]["id"] for row in rows}) == 3
assert all(
    row["evidence"][0]["ref"].startswith("ledger.jsonl#") for row in rows
)
shared = [
    row
    for row in rows
    if row.get("correlation", {}).get("operation_id") == "shared-operation"
]
assert len(shared) == 2
PY

# Capsule owns the richer experiment event; its companion ledger is evidence,
# not a second Work Journal outcome.
(
  cd "$lifecycle_repo"
  "$ROOT/scripts/run-capsule.sh" run --note "capsule outcome" -- sh -c 'exit 0'
) >/dev/null 2>&1

# Explicit Agent State outcomes, verification, and close are automatic.
"$ROOT/scripts/agent-task.sh" --repo "$lifecycle_repo" init \
  --goal "ship journal fixture" --verify "true" >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$lifecycle_repo" update \
  --result "implementation complete" --decision "keep the observer local-first" \
  --next "run the gate" >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$lifecycle_repo" verify >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$lifecycle_repo" close >/dev/null

# A prompt rollover tick is local-only and captures the authoritative HEAD.
prompt_digest="$(
  cd "$lifecycle_repo"
  OMS_SKILL_ROUTER_OFF=1 bash "$ROOT/scripts/skill-router.sh" </dev/null
)"
printf '%s' "$prompt_digest" | grep -Fq '[work-journal]' ||
  fail "first prompt of the day did not surface the bounded journal digest"
second_prompt_digest="$(
  cd "$lifecycle_repo"
  OMS_SKILL_ROUTER_OFF=1 bash "$ROOT/scripts/skill-router.sh" </dev/null
)"
[ -z "$second_prompt_digest" ] ||
  fail "journal digest was injected more than once in one local day"
"$ROOT/scripts/oms" journal show --repo "$lifecycle_repo" --today |
  grep -Fq 'Daily Work Journal' ||
  fail "public journal read path did not return today's summary"

# Terminal job reconciliation supplies observed state without parsing raw logs.
job_ledger="$lifecycle_repo/job-ledger.jsonl"
python3 - "$job_ledger" <<'PY'
import json
import sys
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    handle.write(json.dumps({"slurm_job_id": "42"}) + "\n")
PY
fake_sacct="$TMP/sacct"
printf '%s\n' '#!/usr/bin/env bash' \
  'printf "COMPLETED|0:0|00:01:00\n"' > "$fake_sacct"
chmod +x "$fake_sacct"
(
  cd "$lifecycle_repo"
  OMS_SACCT_CMD="$fake_sacct" OMS_SQUEUE_CMD=__missing_squeue__ \
    "$ROOT/scripts/run-reconcile.sh" apply --ledger "$job_ledger"
) >/dev/null 2>&1

# Completed CI and a matching PR are one structured reconciliation receipt.
fake_gh="$TMP/gh"
cat > "$fake_gh" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  run)
    printf '%s\n' '[{"status":"completed","conclusion":"success","workflowName":"test","headSha":"abc123","url":"https://example.invalid/actions/1"}]'
    ;;
  pr)
    printf '%s\n' '{"number":7,"state":"OPEN","url":"https://example.invalid/pull/7","headRefOid":"abc123"}'
    ;;
esac
EOF
chmod +x "$fake_gh"
(
  cd "$lifecycle_repo"
  OMS_GH_BIN="$fake_gh" "$ROOT/scripts/ci-status.sh" record main
) >/dev/null 2>&1

python3 - "$lifecycle_repo" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1]) / ".oms" / "work-journal" / "events.jsonl"
rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
types = [row["event_type"] for row in rows]
sources = [row["source"]["type"] for row in rows]
assert sources.count("run-ledger") == 3, sources
assert sources.count("run-capsule") == 1, sources
assert "agent_state" in types
assert "task_outcome" in types
assert "commit" in types
assert "job" in types
assert "ci" in types
task_rows = [row for row in rows if row["event_type"] == "agent_state"]
assert any(row.get("decision") == "keep the observer local-first" for row in task_rows)
close = next(row for row in rows if row["event_type"] == "task_outcome")
assert close["verification_status"] == "passed"
job = next(row for row in rows if row["event_type"] == "job")
assert job["verification_status"] == "not_verified"
ci = next(row for row in rows if row["event_type"] == "ci")
assert any(ref["type"] == "pull_request" and ref["id"] == 7 for ref in ci["refs"])
PY

# The agent read path returns summaries, annotations, and recent events.
show_today="$("$ROOT/scripts/journal.sh" show --repo "$lifecycle_repo" --today)"
printf '%s\n' "$show_today" | grep -q '^# Daily Work Journal — ' ||
  fail "journal show --today did not return the daily summary"
"$ROOT/scripts/journal.sh" show --repo "$lifecycle_repo" --blockers --json |
  python3 -c 'import json,sys; data = json.load(sys.stdin); assert any(row["text"] == "run the gate" for row in data["next_actions"]), data' ||
  fail "journal show --blockers missed the recorded next action"
recent_out="$("$ROOT/scripts/journal.sh" show --repo "$lifecycle_repo" --recent 3)"
[ "$(printf '%s\n' "$recent_out" | wc -l | tr -d ' ')" = 3 ] ||
  fail "journal show --recent 3 did not return three event lines"

# Distillation promotes a recurring unresolved blocker once, then advances its
# cursor so a second run cannot append the same lesson.
distill_repo="$TMP/distill-repo"
mkdir -p "$distill_repo"
git -C "$distill_repo" init -q
git -C "$distill_repo" config user.name "Test"
git -C "$distill_repo" config user.email "test@example.com"
printf 'seed\n' > "$distill_repo/seed.txt"
git -C "$distill_repo" add seed.txt
git -C "$distill_repo" commit -qm "seed"
python3 - "$ROOT/scripts/lib" "$distill_repo" <<'PY'
import sys

sys.path.insert(0, sys.argv[1])
import work_journal as wj

store = wj.JournalStore(sys.argv[2], timezone_name="UTC")
for source_id, occurred_at in (
    ("blocker-first", "2026-07-30T02:00:00Z"),
    ("blocker-repeat", "2026-08-02T02:00:00Z"),
):
    store.record_event(
        {
            "event_type": "agent_state",
            "occurred_at": occurred_at,
            "source": {"type": "fixture", "id": source_id},
            "verification_status": "not_verified",
            "blocker": "CI red on shellcheck",
        }
    )
PY
distill_out="$("$ROOT/scripts/journal.sh" distill --repo "$distill_repo")"
printf '%s\n' "$distill_out" | grep -Fq 'journal distill: 1 lesson(s) promoted, marker advanced' ||
  fail "journal distill did not report the promoted blocker"
shared_memory="$distill_repo/.oms/memory/shared.md"
[ "$(grep -Fc 'journal-distill:' "$shared_memory")" = 1 ] ||
  fail "journal distill did not append exactly one shared-memory lesson"
second_distill_out="$("$ROOT/scripts/journal.sh" distill --repo "$distill_repo")"
[ "$second_distill_out" = 'journal distill: nothing to promote' ] ||
  fail "journal distill did not become idempotent"
[ "$(grep -Fc 'journal-distill:' "$shared_memory")" = 1 ] ||
  fail "journal distill appended a duplicate lesson"

# The prompt tick distills once per local day. A decision that agent-task close
# already promoted must be skipped rather than duplicated into the bounded
# memory context; a decision on a never-closed task has no other promotion path
# and still graduates.
auto_repo="$TMP/autodistill-repo"
mkdir -p "$auto_repo"
git -C "$auto_repo" init -q
git -C "$auto_repo" config user.name "Test"
git -C "$auto_repo" config user.email "test@example.com"
printf 'seed\n' > "$auto_repo/seed.txt"
git -C "$auto_repo" add seed.txt
git -C "$auto_repo" commit -qm "seed"
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" init \
  --goal "closed fixture" --verify "true" >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" update \
  --decision "close path already promoted this one" >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" verify >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" close >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" init \
  --goal "open fixture" --verify "true" >/dev/null
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" update \
  --decision "never closed so nothing promoted it" >/dev/null

auto_memory="$auto_repo/.oms/memory/shared.md"
auto_marker="$auto_repo/.oms/work-journal/distill.json"
auto_out="$(OMS_JOURNAL_AUTODISTILL=0 work_journal_prompt_tick "$auto_repo")"
if printf '%s\n' "$auto_out" | grep -Fq 'distill:'; then
  fail "autodistill opt-out still ran the distill: $auto_out"
fi
if grep -Fq 'journal-distill:' "$auto_memory"; then
  fail "autodistill opt-out still appended a lesson"
fi
[ ! -e "$auto_marker" ] ||
  fail "autodistill opt-out advanced the once-per-day marker"

auto_out="$(work_journal_prompt_tick "$auto_repo")"
printf '%s\n' "$auto_out" | grep -Fq '[work-journal] distill: 1 promoted' ||
  fail "prompt tick did not distill the never-closed decision: $auto_out"
printf '%s\n' "$auto_out" | grep -Fq '1 already in shared memory' ||
  fail "prompt tick did not report the shared-memory dedup skip: $auto_out"
[ "$(grep -Fc 'journal-distill:' "$auto_memory")" = 1 ] ||
  fail "tick distill did not append exactly one lesson"
grep -Fq 'journal-distill: decision' "$auto_memory" ||
  fail "tick distill did not promote a decision lesson"
if ! grep -F 'journal-distill:' "$auto_memory" |
  grep -Fq 'never closed so nothing promoted it'; then
  fail "tick distill promoted the wrong decision"
fi
if grep -F 'journal-distill:' "$auto_memory" |
  grep -Fq 'close path already promoted this one'; then
  fail "tick distill duplicated a decision agent-task close already promoted"
fi
auto_out="$(work_journal_prompt_tick "$auto_repo")"
if printf '%s\n' "$auto_out" | grep -Fq 'distill:'; then
  fail "tick distilled twice in one local day: $auto_out"
fi
[ "$(grep -Fc 'journal-distill:' "$auto_memory")" = 1 ] ||
  fail "second tick appended another lesson"

# The distill owns its day marker: the digest advances its own marker only when
# it emits, so the digest opt-out must not silence the distill.
rm -f "$auto_marker"
"$ROOT/scripts/agent-task.sh" --repo "$auto_repo" update \
  --decision "digest off must not gate the distill" >/dev/null
auto_out="$(OMS_WORK_JOURNAL_DIGEST=0 work_journal_prompt_tick "$auto_repo")"
printf '%s\n' "$auto_out" | grep -Fq '[work-journal] distill: 1 promoted' ||
  fail "digest opt-out suppressed the independent distill: $auto_out"

# A distill that cannot write its own marker stays inside the tick: no degraded
# warning, no lost primary result.
rm -f "$auto_marker"
mkdir -p "$auto_marker"
auto_err="$(work_journal_prompt_tick "$auto_repo" 2>&1 >/dev/null)"
if printf '%s\n' "$auto_err" | grep -Fq 'degraded'; then
  fail "a failed distill degraded the tick: $auto_err"
fi
[ -d "$auto_marker" ] ||
  fail "the distill marker write was expected to fail in this fixture"
rm -rf "$auto_marker"

# The first prompt of a local day injects one bounded digest; later prompts and
# the opt-out stay silent. The earlier skill-router call may have consumed
# today's digest, and the marker is derived state, so reset it explicitly. That
# router prompt also consumed this repo's distill day, so the tick below carries
# the digest alone.
rm -f "$lifecycle_repo/.oms/work-journal/digest.json"
digest_out="$(OMS_WORK_JOURNAL_DIGEST=0 work_journal_prompt_tick "$lifecycle_repo")"
[ -z "$digest_out" ] || fail "digest opt-out still injected output"
[ ! -f "$lifecycle_repo/.oms/work-journal/digest.json" ] ||
  fail "digest opt-out advanced the once-per-day marker"
digest_out="$(work_journal_prompt_tick "$lifecycle_repo")"
printf '%s\n' "$digest_out" | grep -q '^\[work-journal\]' ||
  fail "prompt digest did not fire on the first prompt of the day"
printf '%s\n' "$digest_out" | grep -q 'run the gate' ||
  fail "prompt digest missed the open next action"
digest_out="$(work_journal_prompt_tick "$lifecycle_repo")"
[ -z "$digest_out" ] || fail "prompt digest fired twice in one local day"

# A broken observer cannot replace the primary command's exit code or ledger row.
set +e
(
  cd "$repo"
  OMS_WORK_JOURNAL_PYTHON="$TMP/missing.py" \
    "$ROOT/scripts/run-ledger.sh" --file "$repo/ledger.jsonl" -- \
    sh -c 'exit 7'
) >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = 7 ] || fail "journal failure changed primary exit code ($rc)"
[ "$(wc -l < "$repo/ledger.jsonl" | tr -d ' ')" = 1 ] ||
  fail "primary ledger row was lost when observer failed"

# The operator surface is `journal`; the internal implementation name stays
# private.
tools_list="$TMP/oms-tools.txt"
"$ROOT/scripts/oms" list > "$tools_list"
if ! grep -q '^journal' "$tools_list"; then
  fail "journal is missing from the public oms catalog"
fi
if grep -q '^work-journal' "$tools_list"; then
  fail "work-journal leaked into oms list"
fi
set +e
"$ROOT/scripts/oms" work-journal >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = 2 ] || fail "work-journal became a public oms command"

# A harness child may inspect the journal, but host-global configuration and
# the remote-sync boundary stay with the parent. Every red path uses an
# isolated config or repo and must fail before changing either one.
child_expected="a harness child cannot mutate parent-owned host or global state; return the request to the parent agent"
child_config="$TMP/child-configure.json"
child_out="$TMP/child-configure.out"
set +e
OMS_HARNESS_CHILD=1 OMS_WORK_JOURNAL_CONFIG="$child_config" \
  "$ROOT/scripts/journal.sh" configure --lang ko >"$child_out" 2>&1
rc=$?
set -e
[ "$rc" = 2 ] || fail "child journal configure should exit 2 ($rc)"
grep -Fq "$child_expected" "$child_out" ||
  fail "child journal configure did not explain the parent boundary"
[ ! -e "$child_config" ] || fail "child journal configure wrote global config"

child_disconnect_config="$TMP/child-disconnect.json"
child_disconnect_before="$TMP/child-disconnect.before"
printf '%s\n' '{"schema_version":1,"managed_by":"oh-my-setting","notion":{}}' \
  > "$child_disconnect_config"
cp "$child_disconnect_config" "$child_disconnect_before"
child_out="$TMP/child-disconnect.out"
set +e
OMS_HARNESS_CHILD=1 OMS_WORK_JOURNAL_CONFIG="$child_disconnect_config" \
  "$ROOT/scripts/journal.sh" disconnect --managed >"$child_out" 2>&1
rc=$?
set -e
[ "$rc" = 2 ] || fail "child journal disconnect should exit 2 ($rc)"
grep -Fq "$child_expected" "$child_out" ||
  fail "child journal disconnect did not explain the parent boundary"
cmp -s "$child_disconnect_before" "$child_disconnect_config" ||
  fail "child journal disconnect changed global config"

child_sync_repo="$TMP/child-sync-repo"
mkdir -p "$child_sync_repo"
git -C "$child_sync_repo" init -q
git -C "$child_sync_repo" config user.name "Test"
git -C "$child_sync_repo" config user.email "test@example.com"
printf 'seed\n' > "$child_sync_repo/seed.txt"
git -C "$child_sync_repo" add seed.txt
git -C "$child_sync_repo" commit -qm "seed"
child_out="$TMP/child-sync.out"
set +e
OMS_HARNESS_CHILD=1 OMS_WORK_JOURNAL_CONFIG="$TMP/child-sync-config.json" \
  "$ROOT/scripts/journal.sh" sync --repo "$child_sync_repo" --force \
    >"$child_out" 2>&1
rc=$?
set -e
[ "$rc" = 2 ] || fail "child journal sync should exit 2 ($rc)"
grep -Fq "$child_expected" "$child_out" ||
  fail "child journal sync did not explain the parent boundary"
[ ! -e "$child_sync_repo/.oms" ] ||
  fail "child journal sync materialized local state before refusing"
OMS_HARNESS_CHILD=1 "$ROOT/scripts/journal.sh" show --repo "$repo" --today \
  >/dev/null || fail "a harness child should retain journal read access"

# One installer-side service connection flow owns both logins. It discovers
# the existing Work Journal target after Notion authentication and persists no
# credential in the harness config.
connect_bin="$TMP/connect-bin"
gh_marker="$TMP/gh-authenticated"
ntn_marker="$TMP/ntn-authenticated"
mkdir -p "$connect_bin"
cat > "$connect_bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1:$2" in
  auth:status) [ -f "$OMS_TEST_GH_MARKER" ] ;;
  auth:login) touch "$OMS_TEST_GH_MARKER" ;;
  *) exit 2 ;;
esac
EOF
cat > "$connect_bin/ntn" <<'EOF'
#!/usr/bin/env bash
case "$1:$2" in
  login:*) touch "$OMS_TEST_NTN_MARKER" ;;
  api:v1/user"s"/me)
    [ -f "$OMS_TEST_NTN_MARKER" ] || exit 1
    printf '%s\n' '{"object":"user","id":"user"}'
    ;;
  api:v1/search)
    printf '%s\n' '{"results":[{"object":"data_source","id":"ea343dea-4a66-4421-9653-dfc4fe68ed10"}],"has_more":false,"next_cursor":null}'
    ;;
  api:v1/data_sources/ea343dea-4a66-4421-9653-dfc4fe68ed10)
    printf '%s\n' '{"id":"ea343dea-4a66-4421-9653-dfc4fe68ed10","properties":{"Name":{"type":"title"},"Work Journal Key":{"type":"rich_text"},"Content Hash":{"type":"rich_text"},"Project":{"type":"rich_text"},"Kind":{"type":"select"},"Period":{"type":"date"},"Has Blocker":{"type":"checkbox"}}}'
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$connect_bin/gh" "$connect_bin/ntn"
rm -f "$XDG_CONFIG_HOME/oh-my-setting/work-journal.json"
PATH="$connect_bin:$PATH" OMS_CONNECT_INTERACTIVE=1 \
  OMS_GH_BIN="$connect_bin/gh" OMS_NOTION_CLI="$connect_bin/ntn" \
  OMS_TEST_GH_MARKER="$gh_marker" OMS_TEST_NTN_MARKER="$ntn_marker" \
  "$ROOT/scripts/connect-services.sh" --required >/dev/null
[ -f "$gh_marker" ] || fail "service connection did not authenticate gh"
[ -f "$ntn_marker" ] || fail "service connection did not authenticate Notion"
python3 - "$XDG_CONFIG_HOME/oh-my-setting/work-journal.json" <<'PY'
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["notion"]["data_source_id"] == "ea343dea-4a66-4421-9653-dfc4fe68ed10"
assert row["notion"]["auth_mode"] == "ntn"
assert "token" not in json.dumps(row).lower()
PY

# A machine without a usable OS keychain: ntn errors with the keychain hint
# until NOTION_KEYRING=0 selects the file-based store. connect-services must
# detect that, finish the connection file-based, and persist the transport
# choice so hooks inherit it from config rather than from ambient env.
keyring_bin="$TMP/keyring-bin"
keyring_marker="$TMP/ntn-file-authenticated"
mkdir -p "$keyring_bin"
cp "$connect_bin/gh" "$keyring_bin/gh"
cat > "$keyring_bin/ntn" <<'EOF'
#!/usr/bin/env bash
if [ "${NOTION_KEYRING:-}" != "0" ]; then
  echo "error: Failed to create keychain entry" >&2
  echo "  hint: Set NOTION_KEYRING=0 to use file-based auth instead of the OS keychain." >&2
  exit 1
fi
case "$1:$2" in
  whoami:*) [ -f "$OMS_TEST_NTN_MARKER" ] || exit 1 ;;
  login:*) touch "$OMS_TEST_NTN_MARKER" ;;
  api:v1/user"s"/me)
    [ -f "$OMS_TEST_NTN_MARKER" ] || exit 1
    printf '%s\n' '{"object":"user","id":"user"}'
    ;;
  api:v1/search)
    printf '%s\n' '{"results":[{"object":"data_source","id":"ea343dea-4a66-4421-9653-dfc4fe68ed10"}],"has_more":false,"next_cursor":null}'
    ;;
  api:v1/data_sources/ea343dea-4a66-4421-9653-dfc4fe68ed10)
    printf '%s\n' '{"id":"ea343dea-4a66-4421-9653-dfc4fe68ed10","properties":{"Name":{"type":"title"},"Work Journal Key":{"type":"rich_text"},"Content Hash":{"type":"rich_text"},"Project":{"type":"rich_text"},"Kind":{"type":"select"},"Period":{"type":"date"},"Has Blocker":{"type":"checkbox"}}}'
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$keyring_bin/ntn"
rm -f "$XDG_CONFIG_HOME/oh-my-setting/work-journal.json"
keyring_out="$(PATH="$keyring_bin:$PATH" OMS_CONNECT_INTERACTIVE=1 \
  OMS_GH_BIN="$keyring_bin/gh" OMS_NOTION_CLI="$keyring_bin/ntn" \
  OMS_TEST_GH_MARKER="$gh_marker" OMS_TEST_NTN_MARKER="$keyring_marker" \
  "$ROOT/scripts/connect-services.sh" --required 2>&1)" ||
  fail "keychain-less connection should fall back to file auth: $keyring_out"
printf '%s\n' "$keyring_out" | grep -q 'file-based credential store' ||
  fail "keychain fallback should be announced: $keyring_out"
[ -f "$keyring_marker" ] || fail "file-based login did not run"
python3 - "$XDG_CONFIG_HOME/oh-my-setting/work-journal.json" <<'PY'
import json
import sys

row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["notion"]["keyring"] == "file", row["notion"]
assert row["notion"]["cli_command"].endswith("/ntn"), row["notion"]
PY

# --- Passive observers never seed an unadopted repo, and the Notion mirror
# honors the per-repo exclusion list (the human surface is curated: a repo
# that was merely cloned does not belong on it even when sessions ran there).
unadopted="$TMP/unadopted"
mkdir -p "$unadopted"
git -C "$unadopted" init -q
git -C "$unadopted" config user.name "Test"
git -C "$unadopted" config user.email "test@example.com"
work_journal_prompt_tick "$unadopted" >/dev/null 2>&1 || true
[ ! -e "$unadopted/.oms" ] ||
  fail "a prompt tick must not seed .oms in an unadopted repo"
work_journal_finish "$unadopted" >/dev/null 2>&1 || true
[ ! -e "$unadopted/.oms" ] ||
  fail "a Stop finish must not seed .oms in an unadopted repo"
mkdir -p "$unadopted/.oms"
work_journal_prompt_tick "$unadopted" >/dev/null 2>&1 || true
[ -d "$unadopted/.oms/work-journal" ] ||
  fail "an adopted repo must still get its journal on the first tick"

exclusion_cfg="$XDG_CONFIG_HOME/oh-my-setting/work-journal.json"
mkdir -p "$(dirname "$exclusion_cfg")"
python3 - "$exclusion_cfg" "$unadopted" <<'PY'
import json, sys
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump({
        "schema_version": 1,
        "managed_by": "oh-my-setting",
        "notion": {
            "data_source_id": "ea343dea-4a66-4421-9653-dfc4fe68ed10",
            "excluded_repos": [sys.argv[2]],
        },
    }, handle)
PY
"$ROOT/scripts/journal.sh" status --repo "$unadopted" --json | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["notion"]["excluded"] is True, row["notion"]
' || fail "status must report an excluded repo"
# The sync bounds are the caller's, and the front door has to carry them: the
# session hooks want a couple of summaries inside a two-second budget while the
# operator's repair has to be able to clear a backlog. journal.sh parses this
# subcommand itself, so a flag it does not know is refused before the Python
# entry ever sees it.
rc=0
"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --budget 5 --max-per-tick 2 \
  >/dev/null 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "journal sync should accept the caller's bounds (rc=$rc)"
rc=0
"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --budget >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "a bound without a value must be refused (rc=$rc)"
rc=0
"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --budget soon >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "a non-numeric bound must be refused (rc=$rc)"
rc=0
"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --nope >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "an unknown sync argument must still be refused (rc=$rc)"
# Accepting a flag and dropping it is the failure this passes through to catch:
# a fractional tick count is valid to this wrapper's numeric check and invalid
# to the entry that has to receive it, so only a real passthrough refuses it.
rc=0
"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --max-per-tick 1.5 >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "the bounds must reach the journal entry, not stop here (rc=$rc)"

"$ROOT/scripts/journal.sh" sync --repo "$unadopted" --force >/dev/null 2>&1 ||
  fail "sync in an excluded repo must stay a quiet no-op"
[ ! -e "$unadopted/.oms/work-journal/sync/notion.json" ] ||
  fail "an excluded repo must not accumulate Notion sync state"
"$ROOT/scripts/journal.sh" status --repo "$repo" --json | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["notion"]["excluded"] is False, row["notion"]
' || fail "a repo not on the list must not read as excluded"
"$ROOT/scripts/journal.sh" configure --include-repo "$unadopted" |
  grep -q "excludes no repos" ||
  fail "configure --include-repo must clear the exclusion"
"$ROOT/scripts/journal.sh" configure --exclude-repo "$unadopted" |
  grep -q "excludes 1 repo" ||
  fail "configure --exclude-repo must add and report the exclusion"

# --- Project identity: drift between the pinned and detected identity is
# visible, and --adopt-detected re-pins, resets the Notion mapping, and
# rebuilds the derived views — events stay append-only throughout.
idrepo="$TMP/identity-repo"
mkdir -p "$idrepo"
git -C "$idrepo" init -q
git -C "$idrepo" config user.name "Test"
git -C "$idrepo" config user.email "test@example.com"
mkdir -p "$idrepo/.oms/work-journal/sync"
printf '{"schema_version":1,"project_id":"proj_legacy0000000000000","project_name":"legacy"}\n' \
  > "$idrepo/.oms/work-journal/project.json"
printf '{"schema_version":1,"target_fingerprint":"x","summaries":{}}\n' \
  > "$idrepo/.oms/work-journal/sync/notion.json"
"$ROOT/scripts/journal.sh" identity --repo "$idrepo" --json | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["drift"] is True, row
assert row["pinned_name"] == "legacy", row
assert row["detected_name"] == "identity-repo", row
' || fail "identity must report drift between a legacy pin and detection"
"$ROOT/scripts/journal.sh" status --repo "$idrepo" | grep -q "adopt-detected" ||
  fail "status must surface identity drift with its remedy"
"$ROOT/scripts/journal.sh" identity --repo "$idrepo" --adopt-detected |
  grep -q "adopted: identity-repo" ||
  fail "adopt-detected must re-pin to the detected identity"
[ ! -e "$idrepo/.oms/work-journal/sync/notion.json" ] ||
  fail "adopt-detected must reset the disposable Notion mapping"
"$ROOT/scripts/journal.sh" identity --repo "$idrepo" --json | python3 -c '
import json, sys
row = json.load(sys.stdin)
assert row["drift"] is False, row
assert row["pinned_name"] == "identity-repo", row
' || fail "after adoption the pin must match detection"
"$ROOT/scripts/journal.sh" identity --repo "$idrepo" --adopt-detected |
  grep -q "already canonical" ||
  fail "a second adoption must be an explicit no-op"

# The rendered language follows the pinned configuration, not the locale of
# whichever shell a lifecycle hook happened to run in.
langrepo="$TMP/lang-repo"
mkdir -p "$langrepo"
git -C "$langrepo" init -q
git -C "$langrepo" config user.name "Test"
git -C "$langrepo" config user.email "test@example.com"
printf 'seed\n' > "$langrepo/seed.txt"
git -C "$langrepo" add seed.txt
git -C "$langrepo" commit -qm "seed"
(
  export OMS_WORK_JOURNAL_CONFIG="$TMP/lang-config.json"
  export LANG=en_US.UTF-8
  unset OMS_WORK_JOURNAL_LANG LC_ALL LC_MESSAGES
  "$ROOT/scripts/journal.sh" configure --lang ko | grep -q "journal language: ko" ||
    fail "configure --lang did not confirm the pinned language"
  row="$TMP/lang-row.json"
  make_row "$row" "lang-one"
  work_journal_observe "$langrepo" run-ledger "$row"
  "$ROOT/scripts/journal.sh" show --repo "$langrepo" --period 2026-07-31 |
    grep -q '^## 핵심 진전' ||
    fail "pinned language did not reach the rendered summary"
  export OMS_WORK_JOURNAL_LANG=en
  "$ROOT/scripts/journal.sh" rebuild --repo "$langrepo" >/dev/null
  "$ROOT/scripts/journal.sh" show --repo "$langrepo" --period 2026-07-31 |
    grep -q '^## Key progress' ||
    fail "environment override did not beat the pinned language"
)

echo "work-journal-smoke: ok"
