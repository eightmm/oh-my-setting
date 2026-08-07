#!/usr/bin/env bash
set -euo pipefail

# A council seat that dies must leave the same bookkeeping as one that answered.
# Three ledgers record a seat, and a timed-out provider used to reach only one
# of them: the artifact row carried the true exit, but the thread — the
# conversation the next agent reads as the record of what was asked and
# answered — silently lost the seat, the failure ledger never learned that this
# provider hangs, and the run summary reported the loss only as the differential
# in "2/3 providers succeeded" without ever naming who was missing.
#
# The fixture is the live incident: three seats, one of which hangs past the
# wall clock and is killed at exit 124, while the other two answer and keep the
# council at quorum.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-council-failure.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com

# The dead seat is produced by the wall clock itself, so without a timeout
# binary there is no failure to record and the stub would hang the suite.
if ! command -v timeout >/dev/null 2>&1 && ! command -v gtimeout >/dev/null 2>&1; then
  echo "council-failure-symmetry-smoke: skipped (no timeout/gtimeout binary)"
  exit 0
fi

make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  printf 'base\n' > "$dir/file.txt"
  git -C "$dir" add file.txt
  git -C "$dir" commit -q -m init
}

write_stubs() {  # write_stubs BIN_DIR
  local bin_dir="$1"
  local provider

  mkdir -p "$bin_dir"
  # The seat that dies: it reads its prompt and then hangs past OMS_PEER_TIMEOUT.
  # exec keeps it a single process, so the timeout's SIGTERM lands on the sleep
  # itself and the seat exits 124 instead of escalating to a SIGKILL 137.
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
exec sleep 30
EOF
  # Two seats that answer, so the run stays a quorum and the dead seat is a
  # missing voice in a council that still produced a synthesis.
  for provider in claude agy; do
    cat > "$bin_dir/$provider" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
echo "Verdict: the seat bookkeeping is safe to land. Evidence: the artifact row,"
echo "the thread turn, and the failure ledger are written from one place, and"
echo "this run exercises all three. Risk: none beyond the missing regression."
echo "Next: run the smoke suite, then land it."
EOF
  done
  chmod +x "$bin_dir/codex" "$bin_dir/claude" "$bin_dir/agy"
}

run_council() {  # run_council PROJECT BIN_DIR THREAD OUT ERR [ENV...]
  local project="$1" bin_dir="$2" thread="$3" out="$4" err="$5"
  local rc=0
  shift 5

  HOME="$project/home" NVM_DIR="$project/home/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    OMS_PEER_TIMEOUT=2 OMS_PEER_KILL_AFTER=1 "$@" \
    "$ROOT/scripts/peer-ask.sh" \
    --repo "$project" \
    --artifact-dir "$project/artifacts" \
    --providers codex,claude,antigravity \
    --no-memory --no-task --no-ml-context \
    --thread "$thread" \
    --prompt "Council failure symmetry" > "$out" 2> "$err" || rc=$?
  printf '%s' "$rc"
}

test_dead_seat_reaches_every_ledger() {
  local project="$TMP/seat"
  local bin_dir="$project/bin"
  local index="$project/.oms/artifacts/index.jsonl"
  local rc
  local list

  make_repo "$project"
  mkdir -p "$project/home"
  write_stubs "$bin_dir"

  rc="$(run_council "$project" "$bin_dir" seatfail1 "$project/out" "$project/err")"
  [ "$rc" = "0" ] || fail "two live seats are still a quorum, got exit $rc: $(cat "$project/err")"

  # 1. Artifact row: existing behavior, anchored here so the other two writes
  #    cannot be "fixed" by weakening the one ledger that was already honest.
  [ -f "$index" ] || fail "no artifact index was written"
  OMS_T_INDEX="$index" python3 - <<'PY' || fail "the dead seat needs an artifact row with its true exit"
import json, os
rows = [json.loads(line) for line in open(os.environ["OMS_T_INDEX"], encoding="utf-8") if line.strip()]
codex = [r for r in rows if r.get("provider") == "codex"]
assert len(codex) == 1, codex
assert codex[0].get("exit") == 124, codex[0]
PY

  # 2. Thread turn: one typed line, one line long, and never the artifact body —
  #    thread turns are replayed into later prompts.
  OMS_T_THREAD="$project/.oms/threads/seatfail1.jsonl" python3 - <<'PY' ||
import json, os
path = os.environ["OMS_T_THREAD"]
rows = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
notes = [r for r in rows if r.get("role") == "note" and r.get("provider") == "codex"]
assert len(notes) == 1, "expected exactly one non-answer turn for the dead seat: %r" % (rows,)
text = notes[0].get("text", "")
assert "\n" not in text, "the non-answer turn must be one line: %r" % (text,)
assert text.startswith("no answer (exit 124)"), text
assert "artifact: " in text, text
assert notes[0].get("artifact"), notes[0]
# The two live seats still answer, and the dead seat is not one of them.
answers = [r for r in rows if r.get("role") == "answer"]
assert len(answers) == 2, answers
assert all(r.get("provider") != "codex" for r in answers), answers
assert any(r.get("role") == "question" for r in rows), rows
PY
    fail "the thread must carry one typed non-answer turn for the dead seat"

  # 3. Run summary: the seat is named, not left as a silent differential.
  grep -Fq 'summary: 2/3 providers succeeded (1 failed)' "$project/out" ||
    fail "the summary should count the dead seat: $(cat "$project/out")"
  grep -Fq 'note: no answer: codex (exit 124)' "$project/err" ||
    fail "the summary should name the dead seat: $(cat "$project/err")"

  # 4. Fail ledger: a later session asks this before spending another call.
  list="$("$ROOT/scripts/fail-ledger.sh" --repo "$project" list)"
  printf '%s' "$list" | grep -Fq 'codex ask seat returned no answer (exit 124)' ||
    fail "the seat failure should be in the fail ledger: $list"
  printf '%s' "$list" | grep -Fq 'count=1' || fail "first failure should count once: $list"
  printf '%s' "$list" | grep -Fq 'OPEN' || fail "an unresolved seat failure should be open: $list"

  # 5. A provider that hangs every time has to accumulate onto one fingerprint,
  #    or the advise threshold never sees a chronically dead seat.
  rc="$(run_council "$project" "$bin_dir" seatfail2 "$project/out2" "$project/err2")"
  [ "$rc" = "0" ] || fail "the second run should also reach quorum, got exit $rc"
  list="$("$ROOT/scripts/fail-ledger.sh" --repo "$project" list)"
  printf '%s' "$list" | grep -Fq 'count=2' ||
    fail "repeat failures of the same seat should accumulate: $list"
  [ "$(printf '%s\n' "$list" | grep -c 'count=')" = "1" ] ||
    fail "the same seat timing out twice must be one fingerprint, not two: $list"
  # Each run's own thread records its own seat, exactly once.
  grep -Fq '"role": "note"' "$project/.oms/threads/seatfail2.jsonl" ||
    fail "the second run's thread also needs its non-answer turn"
}

test_dry_run_records_no_durable_failure() {
  local project="$TMP/dry"
  local bin_dir="$project/bin"
  local rc
  # Assembled at runtime so this file stays clean for the repo's own path scan.
  local machine_path="/ho""me/sentinel-user/proj/data.txt"

  make_repo "$project"
  mkdir -p "$project/home"
  write_stubs "$bin_dir"

  # A dry run never calls the provider, but the outbound scrubber still blocks
  # the seat (exit 3) — the one dry-run path that reaches the failure branch.
  # The thread still records the seat, because dry-run thread turns are how the
  # rest of the harness already behaves; the durable failure ledger must not,
  # because no provider command ever ran.
  rc=0
  HOME="$project/home" NVM_DIR="$project/home/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    OH_MY_SETTING_ASK_DRY_RUN=1 \
    "$ROOT/scripts/peer-ask.sh" \
    --repo "$project" \
    --artifact-dir "$project/artifacts" \
    --providers codex,claude \
    --no-memory --no-task --no-ml-context \
    --thread dryseat \
    --prompt "read $machine_path and summarize it" \
    > "$project/out" 2> "$project/err" || rc=$?

  [ "$rc" = "1" ] || fail "a fully blocked council should exit 1, got $rc: $(cat "$project/err")"
  grep -Fq 'summary: 0/2 providers succeeded (2 failed)' "$project/out" ||
    fail "blocked seats should be counted and named: $(cat "$project/out")"
  [ ! -f "$project/.oms/failures.jsonl" ] ||
    fail "a dry run must not write durable failure memory: $(cat "$project/.oms/failures.jsonl")"
  grep -Fq 'no answer (exit 3)' "$project/.oms/threads/dryseat.jsonl" ||
    fail "a blocked seat still belongs in the thread"
}

test_failed_single_call_is_recorded_like_a_seat() {
  local project="$TMP/call"
  local bin_dir="$project/bin"
  local rc=0
  local list

  make_repo "$project"
  mkdir -p "$project/home" "$bin_dir"
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
echo "provider failed mid-run"
exit 42
EOF
  chmod +x "$bin_dir/codex"

  # One call is a council of one: a thread that keeps only the calls that
  # worked is not the conversation of record it claims to be.
  HOME="$project/home" NVM_DIR="$project/home/.nvm" PATH="$bin_dir:/usr/bin:/bin" \
    "$ROOT/scripts/agent-call.sh" \
    --repo "$project" \
    --artifact-dir "$project/artifacts" \
    --to codex \
    --thread callfail \
    --prompt "Single call failure symmetry" > "$project/out" 2> "$project/err" || rc=$?

  [ "$rc" = "42" ] || fail "agent-call must still propagate the provider's exit, got $rc"
  grep -Fq 'artifact: ' "$project/out" || fail "the artifact line must survive the new writes"

  OMS_T_THREAD="$project/.oms/threads/callfail.jsonl" python3 - <<'PY' ||
import json, os
rows = [json.loads(line) for line in open(os.environ["OMS_T_THREAD"], encoding="utf-8") if line.strip()]
# The question is recorded even though nothing answered it: a thread that shows
# no question cannot show that this one went unanswered.
assert [r for r in rows if r.get("role") == "question"], rows
notes = [r for r in rows if r.get("role") == "note" and r.get("provider") == "codex"]
assert len(notes) == 1, rows
text = notes[0].get("text", "")
assert "\n" not in text, text
assert text.startswith("no answer (exit 42)"), text
assert not [r for r in rows if r.get("role") == "answer"], rows
PY
    fail "a failed call must leave one typed non-answer turn in its thread"

  list="$("$ROOT/scripts/fail-ledger.sh" --repo "$project" list)"
  printf '%s' "$list" | grep -Fq 'codex call seat returned no answer (exit 42)' ||
    fail "a failed call belongs in the fail ledger: $list"
}

test_exit_zero_non_answer_threads_but_stays_out_of_the_ledger() {
  local project="$TMP/banner"
  local bin_dir="$project/bin"
  local rc=0

  make_repo "$project"
  mkdir -p "$project/home"
  write_stubs "$bin_dir"
  # A seat that exits 0 having printed only its reason for saying nothing. It is
  # a non-answer, not a failed command: it belongs in the thread like any other
  # silent seat, and nowhere near the failure ledger, whose fingerprints are
  # meant to say "this command keeps failing".
  cat > "$bin_dir/codex" <<'EOF'
#!/usr/bin/env bash
cat > /dev/null
echo 'add an allow-rule for the read tool, then re-run the council.'
EOF
  chmod +x "$bin_dir/codex"

  rc="$(run_council "$project" "$bin_dir" bannerseat "$project/out" "$project/err")"
  [ "$rc" = "0" ] || fail "a banner seat should not sink the council, got exit $rc"
  # The established accounting for this population is untouched.
  grep -Fq 'summary: 2/3 providers succeeded (1 non-answer(s))' "$project/out" ||
    fail "answer-quality accounting must not change: $(cat "$project/out")"

  OMS_T_THREAD="$project/.oms/threads/bannerseat.jsonl" python3 - <<'PY' ||
import json, os
rows = [json.loads(line) for line in open(os.environ["OMS_T_THREAD"], encoding="utf-8") if line.strip()]
notes = [r for r in rows if r.get("role") == "note" and r.get("provider") == "codex"]
assert len(notes) == 1, rows
assert notes[0].get("text") == "no answer (blocked)" or \
    notes[0].get("text", "").startswith("no answer (blocked); artifact: "), notes[0]
assert notes[0].get("quality") == "blocked", notes[0]
# The banner itself must not be replayed as this seat's contribution.
assert "allow-rule" not in notes[0].get("text", ""), notes[0]
PY
    fail "a seat that never answered belongs in the thread, named not quoted"

  [ ! -f "$project/.oms/failures.jsonl" ] ||
    fail "an exit-0 non-answer is not a failed command: $(cat "$project/.oms/failures.jsonl")"
}

test_dead_seat_reaches_every_ledger
test_dry_run_records_no_durable_failure
test_failed_single_call_is_recorded_like_a_seat
test_exit_zero_non_answer_threads_but_stays_out_of_the_ledger

echo "council-failure-symmetry-smoke: ok"
