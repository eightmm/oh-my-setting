#!/usr/bin/env bash
set -Eeuo pipefail

# Harness evaluation scenarios: does OMS as a whole keep its promises across a
# realistic sequence? Each scenario builds a temp repository, runs real `oms`
# commands with fake provider CLIs on PATH (no model call, no network, no real
# provider), and asserts on the state those commands recorded. This is a
# regression instrument for harness behaviour, not a model-quality benchmark.
#
# Usage: harness-eval-smoke.sh [--json] [--only NAME]
#
# One line per scenario: `eval: NAME: pass|fail|xfail(BRANCH)|unexpected-pass`,
# then a summary. Exit 0 only with no `fail` and no `unexpected-pass`.
#
# A scenario function returns 0 when the property holds and calls `violated`
# (exit 3) when the property under test does not hold. Anything else (a setup
# or control failure, an unexpected command error) is a plain `fail` even for a
# scenario listed in XFAIL, so a known defect can never hide a broken fixture.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

SCENARIOS=(
  resume-no-duplicate
  criterion-change-invalidates-evidence
  unrelated-verification-no-resolve
  panel-matches-source
  memory-fits-budget
)

# Known defects, each fixed by a branch that is open when this suite was
# written. When that branch lands the scenario starts passing and the suite
# reports `unexpected-pass`: delete the case line here.
xfail_branch() {
  case "$1" in
    criterion-change-invalidates-evidence) printf '%s\n' oms/criterion-evidence-freshness-20261110 ;;
    unrelated-verification-no-resolve) printf '%s\n' oms/review-receipt-binding-20261110 ;;
  esac
}

# --------------------------------------------------------------------------
# Child mode: run one scenario in a fresh process so `set -e` is honoured.
# --------------------------------------------------------------------------
if [ "${1:-}" = "--run-one" ]; then
  # shellcheck source=tests/lib/smoke-env.sh
  . "$ROOT/tests/lib/smoke-env.sh"
  NAME="$2"
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-eval.XXXXXX")"
  trap 'rm -rf "$TMP"' EXIT HUP INT TERM
  trap 'echo "error: command failed at line $LINENO: $BASH_COMMAND" >&2' ERR
  OMS="$ROOT/scripts/oms"
  export OMS_CAPABILITY_SKIP_MODELS=1 OMS_AGENT=claude

  violated() { echo "property violated: $*" >&2; exit 3; }

  new_repo() {  # DIR: a committed repository with a private .oms and a fixture HOME
    mkdir -p "$1/.oms" "$1/home" "$1/bin"
    git -C "$1" init -q -b main
    printf '*\n' > "$1/.oms/.gitignore"
    printf '/bin/\n/home/\n/artifacts*/\n' > "$1/.gitignore"
  }

  # Count rows of a JSONL file whose KEY equals VALUE (rows that do not parse are ignored).
  jsonl_count() {  # FILE KEY VALUE
    python3 - "$@" <<'PY'
import json, sys
path, key, value = sys.argv[1:4]
count = 0
try:
    lines = open(path, encoding="utf-8").read().splitlines()
except OSError:
    lines = []
for line in lines:
    try:
        row = json.loads(line)
    except ValueError:
        continue
    if isinstance(row, dict) and row.get(key) == value:
        count += 1
print(count)
PY
  }

  # ---- 1. A resumed drive does not repeat work, verification or landing ----
  scenario_resume_no_duplicate() {
    local repo="$TMP/repo" calls="$TMP/provider-calls" rc=0 base
    new_repo "$repo"
    git -C "$repo" config user.email test@example.com
    git -C "$repo" config user.name Test
    mkdir -p "$repo/scripts"
    printf 'base\n' > "$repo/tracked.txt"
    cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
grep -Fxq autonomous-new new.txt
EOF
    chmod +x "$repo/scripts/check.sh"
    git -C "$repo" add -A
    git -C "$repo" commit -qm base
    base="$(git -C "$repo" rev-parse HEAD)"
    cat > "$repo/bin/codex" <<'EOF'
#!/usr/bin/env bash
case "${1:-}:${2:-}" in
  --version:|--help:|exec:--help) printf 'codex 1.0\n'; exit 0 ;;
esac
cat >/dev/null
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
printf 'autonomous-new\n' > new.txt
echo worker-ok
EOF
    chmod +x "$repo/bin/codex"
    "$OMS" agent-plan --repo "$repo" init --goal "eval resume" --accept "bash scripts/check.sh" > /dev/null
    "$OMS" agent-plan --repo "$repo" add --id new --title "fix: eval resume landing" \
      --allowed new.txt --verify "bash scripts/check.sh" > /dev/null

    drive() {  # EXTRA_ENV... : one `oms goal-drive` run, output to $TMP/drive.out
      env HOME="$repo/home" NVM_DIR="$repo/home/.nvm" PATH="$repo/bin:/usr/bin:/bin" CALL_LOG="$calls" "$@" \
        "$OMS" goal-drive --repo "$repo" --to codex --max-cycles 2 > "$TMP/drive.out" 2>&1
    }
    # The driver's own interruption hook stops it after the patch landed but
    # before the commit: the same window a killed process would leave.
    drive OMS_GOAL_DRIVE_TEST_STOP_AFTER_LAND=1 || rc=$?
    [ "$rc" = 75 ] || { tail -8 "$TMP/drive.out" >&2; echo "setup: interruption hook did not stop the drive (rc=$rc)" >&2; exit 1; }
    [ "$(git -C "$repo" rev-parse HEAD)" = "$base" ] || { echo "setup: HEAD moved before the interruption" >&2; exit 1; }

    local index="$repo/.oms/artifacts/index.jsonl" landings="$repo/.oms/landings.jsonl"
    local work_1 admit_1 land_1
    work_1="$(jsonl_count "$index" kind delegate)"
    admit_1="$(jsonl_count "$index" kind patch-admit)"
    land_1="$(jsonl_count "$index" kind patch-land)"
    [ "$work_1:$admit_1:$land_1" = 1:1:1 ] || { echo "setup: expected one work, admit and land record before resume, got $work_1:$admit_1:$land_1" >&2; exit 1; }

    drive || { tail -8 "$TMP/drive.out" >&2; echo "setup: resume did not finish" >&2; exit 1; }
    grep -Fq 'status=done' "$TMP/drive.out" || { tail -8 "$TMP/drive.out" >&2; echo "setup: resume did not reach acceptance" >&2; exit 1; }

    local counts
    counts="work=$(jsonl_count "$index" kind delegate) verify=$(jsonl_count "$index" kind patch-admit)"
    counts="$counts land=$(jsonl_count "$index" kind patch-land) landing-complete=$(jsonl_count "$landings" event complete)"
    counts="$counts provider-calls=$(wc -l < "$calls" | tr -d ' ') commits=$(git -C "$repo" rev-list --count "$base..HEAD")"
    counts="$counts delegate-verify-files=$(find "$repo/.oms/artifacts/delegate" -name '*.verify.txt' | wc -l | tr -d ' ')"
    [ "$counts" = "work=1 verify=1 land=1 landing-complete=1 provider-calls=1 commits=1 delegate-verify-files=1" ] ||
      violated "a resumed drive repeated a step: $counts"
    [ -z "$(git -C "$repo" status --porcelain)" ] || violated "resume left the worktree dirty"
    echo "resume: $counts"
  }

  # ---- 2. A reworded criterion makes its recorded evidence stale ----------
  scenario_criterion_change_invalidates_evidence() {
    local repo="$TMP/repo" status
    new_repo "$repo"
    mkdir -p "$repo/scripts" "$repo/.oms/artifacts"
    printf '# Eval\n\n## Goal\n\nShip it.\n\n## Acceptance Criteria\n\n- [id:crit-a] Public API remains compatible.\n' > "$repo/PROJECT.md"
    printf 'value = 1\n' > "$repo/scripts/sample.py"
    git -C "$repo" add -A
    git -C "$repo" commit -qm base
    printf '{"schema":1,"event_id":"evt-1","kind":"verify","status":"verified"}\n' > "$repo/.oms/artifacts/index.jsonl"
    criterion_status() {
      "$OMS" runtime --repo "$repo" evidence show | python3 -c '
import json, sys
rows = [c for c in json.load(sys.stdin)["criteria"] if c["id"] == "crit-a"]
print(rows[0]["status"] if rows else "absent")'
    }
    "$OMS" runtime --repo "$repo" evidence bind --criterion crit-a --ref evt-1 --status verified \
      --depends scripts/sample.py > /dev/null
    status="$(criterion_status)"
    [ "$status" = verified ] || { echo "control: fresh binding reads '$status', expected verified" >&2; exit 1; }

    # Same id, different wording; the declared dependency is untouched.
    printf '# Eval\n\n## Goal\n\nShip it.\n\n## Acceptance Criteria\n\n- [id:crit-a] Public API and the CLI remain compatible.\n' > "$repo/PROJECT.md"
    status="$(criterion_status)"
    [ "$status" != verified ] || violated "criterion crit-a was reworded but its evidence still reads '$status'"
    echo "criterion status after rewording: $status"
  }

  # ---- 3. An unrelated verification receipt does not verify a finding -----
  scenario_unrelated_verification_no_resolve() {
    local repo="$TMP/repo" review="$ROOT/scripts/peer-review.sh" id receipt rc=0 status
    local -a where
    new_repo "$repo"
    git -C "$repo" config user.email test@example.com
    git -C "$repo" config user.name Test
    printf 'base\n' > "$repo/file.txt"
    printf 'one\ntwo\n' > "$repo/other.txt"
    git -C "$repo" add -A
    git -C "$repo" commit -qm base
    # Scope A: an uncommitted change to file.txt that a seat reports a finding on.
    printf 'reviewed change\n' >> "$repo/file.txt"
    cat > "$repo/bin/claude" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
if [ -n "${EVAL_FINDING:-}" ]; then
  printf 'Findings:\nsee the block\n```findings\n'
  printf '%s\n' '{"file": "file.txt", "line": 2, "severity": "blocking", "summary": "The appended line is wrong", "quote": "reviewed change"}'
  printf '```\nCONFIDENCE: 0.8\nGATE: fail\n'
else
  printf 'Findings:\nNo findings\nRisks:\nnone\nMissing tests:\nnone\nRecommendation:\nproceed\nGATE: pass\n'
fi
EOF
    chmod +x "$repo/bin/claude"
    gate() {  # ARTIFACT_DIR PROMPT [ENV...]
      local dir="$1" prompt="$2"; shift 2
      env HOME="$repo/home" PATH="$repo/bin:/usr/bin:/bin" "$@" "$review" --repo "$repo" \
        --artifact-dir "$dir" --providers claude --gate --verify true --prompt "$prompt" > "$TMP/gate.out" 2>&1
    }
    where=(--artifact-dir "$repo/artifacts-a" --repo "$repo")
    gate "$repo/artifacts-a" "Scope A" EVAL_FINDING=1 || rc=$?
    [ "$rc" = 1 ] || { cat "$TMP/gate.out" >&2; echo "setup: the failing seat should fail the gate with 1, got $rc" >&2; exit 1; }
    id="$("$review" findings list "${where[@]}" | awk '/in-diff/{print $1}')"
    [ -n "$id" ] || { echo "setup: no in-diff finding was recorded" >&2; exit 1; }
    rc=0
    "$review" findings verify "$id" "${where[@]}" --run no-such-receipt > "$TMP/verify.out" 2>&1 || rc=$?
    [ "$rc" = 2 ] || { echo "control: verifying against a missing receipt returned $rc, expected 2" >&2; exit 1; }

    # The fix is claimed; then a different change (scope B, other.txt) passes verification.
    git -C "$repo" add -A
    git -C "$repo" commit -qm "fix: scope A"
    "$review" findings fix "$id" "${where[@]}" --note "rewrote the line" > /dev/null
    sleep 1   # receipt timestamps have one-second resolution; B must be recorded after the fix claim
    printf 'three\n' >> "$repo/other.txt"
    gate "$repo/artifacts-b" "Scope B" || { cat "$TMP/gate.out" >&2; echo "setup: the unrelated gate should pass" >&2; exit 1; }
    receipt="$(basename "$repo"/artifacts-b/_verify-*.md)"

    rc=0
    "$review" findings verify "$id" "${where[@]}" --run "$receipt" > "$TMP/verify.out" 2>&1 || rc=$?
    status="$("$review" findings show "$id" "${where[@]}" | awk 'NR==1{print $2}')"
    if [ "$rc" = 0 ] || [ "$status" = verified ]; then
      violated "a passing receipt for scope B verified a finding from scope A (verify rc=$rc, status=$status): $(head -1 "$TMP/verify.out")"
    fi
    [ "$rc" = 2 ] || { echo "control: refusal should exit 2, got $rc" >&2; exit 1; }
    echo "refused: $(head -1 "$TMP/verify.out")"
  }

  # ---- 4. The panel report shows what the lifecycle records say -----------
  scenario_panel_matches_source() {
    local repo="$TMP/repo" room main attempt
    new_repo "$repo"
    echo base > "$repo/f"
    git -C "$repo" add -A
    git -C "$repo" commit -qm base
    export HOME="$repo/home"
    room="$("$OMS" room --repo "$repo" new --topic "eval room" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')"
    main="$("$OMS" agent-events --repo "$repo" start --provider claude --tool panel-main \
      --ref "panel_room_id=$room" --ref panel_role=main --then starting --then working)"
    "$OMS" room --repo "$repo" join --id "$room" --participant "$main" --provider claude --role main > /dev/null
    call() {  # TASK: a call in the room's main, left working
      "$OMS" agent-events --repo "$repo" start --provider codex --tool peer-ask --task-id "$1" \
        --parent-attempt-id "$main" --ref "panel_room_id=$room" --ref "panel_room_participant=$main" \
        --then starting --then working
    }
    call t-working > /dev/null
    attempt="$(call t-done)"
    "$OMS" agent-events --repo "$repo" transition --attempt "$attempt" --state verifying --then review --then done > /dev/null
    attempt="$(call t-failed)"
    "$OMS" agent-events --repo "$repo" transition --attempt "$attempt" --state failed --reason-code eval-fixture > /dev/null
    attempt="$(call t-blocked)"
    "$OMS" agent-events --repo "$repo" transition --attempt "$attempt" --state blocked > /dev/null

    "$OMS" agent-events --repo "$repo" list --json > "$TMP/source.json"
    OMS_ROOM_ID="$room" "$OMS" panel --repo "$repo" --json > "$TMP/panel.json" ||
      { echo "setup: panel --json failed on a healthy lifecycle stream" >&2; exit 1; }
    python3 - "$TMP/source.json" "$TMP/panel.json" "$room" <<'PY' || exit $?
import json, sys
source = {r["attempt_id"]: r["state"] for r in json.load(open(sys.argv[1]))}
dash = json.load(open(sys.argv[2]))["dashboard"]
if len(set(source.values())) < 4 or len(source) != 5:
    print("setup: fixture did not produce the expected mix of states: %r" % source, file=sys.stderr); sys.exit(1)
shown = {r["attempt_id"]: r["state"] for r in dash["attempts"]["recent"]}
if shown != source:
    print("property violated: panel states differ from the source records: panel=%r source=%r" % (shown, source), file=sys.stderr); sys.exit(3)
wanted = {}
for state in source.values():
    wanted[state] = wanted.get(state, 0) + 1
if dash["attempts"]["by_state"] != wanted:
    print("property violated: by_state %r != source %r" % (dash["attempts"]["by_state"], wanted), file=sys.stderr); sys.exit(3)
if dash["room"].get("id") != sys.argv[3]:
    print("property violated: panel shows room %r, expected %r" % (dash["room"].get("id"), sys.argv[3]), file=sys.stderr); sys.exit(3)
PY

    # An unreadable lifecycle stream must not leave stale "working" rows on the board.
    printf '{unreadable\n' >> "$repo/.oms/lifecycle/events.jsonl"
    OMS_ROOM_ID="$room" "$OMS" panel --repo "$repo" --json > "$TMP/panel-broken.json" 2> /dev/null || true
    python3 - "$TMP/panel-broken.json" <<'PY' || exit $?
import json, sys
try:
    dash = json.load(open(sys.argv[1]))["dashboard"]
except Exception as error:
    print("property violated: panel produced no readable report for an unreadable source: %s" % error, file=sys.stderr); sys.exit(3)
attempts = dash["attempts"]
live = [r["attempt_id"] for key in ("recent", "active_recent") for r in attempts.get(key, []) if r.get("state") == "working"]
if live or attempts.get("available") is not False or attempts.get("active"):
    print("property violated: unreadable source still shown as live: available=%r active=%r rows=%r"
          % (attempts.get("available"), attempts.get("active"), live), file=sys.stderr); sys.exit(3)
if dash["collection"].get("ok") is not False:
    print("property violated: collection health hides the unreadable source: %r" % dash["collection"], file=sys.stderr); sys.exit(3)
PY
    echo "panel: 5 attempts match source; unreadable stream reported as unavailable"
  }

  # ---- 5. The note a task needs reaches a bounded session-start block -----
  scenario_memory_fits_budget() {
    local repo="$TMP/repo" i relevant_id size section
    new_repo "$repo"
    echo base > "$repo/f"
    git -C "$repo" add -A
    git -C "$repo" commit -qm base
    export HOME="$repo/home"
    # The relevant note is the OLDEST; recency alone would surface the filler.
    "$OMS" agent-memory --repo "$repo" append \
      --text "attention kernel NaN loss was caused by fp16 softmax overflow; accumulate in fp32 and clamp logits" > /dev/null
    pad="$(printf "padding %.0s" $(seq 1 80))"
    for i in $(seq 1 10); do
      "$OMS" agent-memory --repo "$repo" append \
        --text "Filler note $i: rotated scratch directories and refreshed the dataset mirror listing for the weekly sync job, $pad" > /dev/null
    done
    "$OMS" agent-memory --repo "$repo" pin --text "Pinned: use uv for environments" > /dev/null
    "$OMS" agent-task --repo "$repo" init --goal "Fix the NaN loss in the attention kernel" --verify true > /dev/null
    relevant_id="$("$OMS" agent-memory --repo "$repo" search "fp16 softmax" --json | python3 -c '
import json, sys
print(json.loads(sys.stdin.readline())["event_id"])')"
    size="$("$OMS" agent-memory --repo "$repo" show | wc -c | tr -d ' ')"
    [ "$size" -gt 6144 ] || { echo "control: memory is only $size bytes; the budget would not be exercised" >&2; exit 1; }

    printf '{"cwd":"%s"}' "$repo" | OMS_RESUME_MEMORY=1 "$ROOT/scripts/resume-hook.sh" > "$TMP/block.out"
    size="$(wc -c < "$TMP/block.out" | tr -d ' ')"
    section="$(awk '/^- memory /{f=1} f&&/^  more:/{print; exit} f{print}' "$TMP/block.out" | wc -c | tr -d ' ')"
    [ "$section" -gt 0 ] || violated "the session-start block has no memory section"
    grep -Fq "recall $relevant_id:" "$TMP/block.out" ||
      violated "the relevant note $relevant_id is not in the block: $(grep -F 'recall ' "$TMP/block.out" | cut -c1-90 | tr '\n' '|')"
    grep -F "recall $relevant_id:" "$TMP/block.out" | grep -Fq 'fp16 softmax overflow' ||
      violated "the relevant note id is shown without its text"
    [ "$section" -le 1200 ] || violated "memory section is $section bytes, over its 1200-byte bound"
    [ "$size" -le 4096 ] || violated "block is $size bytes, over the 4096-byte cap"
    echo "memory: relevant note $relevant_id shown; memory section ${section}B of 1200, block ${size}B of 4096"
  }

  fn="scenario_${NAME//-/_}"
  declare -F "$fn" > /dev/null || { echo "unknown scenario: $NAME" >&2; exit 64; }
  "$fn"
  exit 0
fi

# --------------------------------------------------------------------------
# Parent mode: run each scenario, classify, report.
# --------------------------------------------------------------------------
JSON=0
ONLY=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON=1; shift ;;
    --only) [ "$#" -ge 2 ] || { echo "error: --only needs a scenario name" >&2; exit 2; }; ONLY="$2"; shift 2 ;;
    -h|--help) sed -n '3,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
done

RESULTS="$(mktemp "${TMPDIR:-/tmp}/oms-eval-results.XXXXXX")"
trap 'rm -f "$RESULTS"' EXIT HUP INT TERM
# With --json the human lines go to stderr so stdout is exactly one JSON object.
say() { if [ "$JSON" = 1 ]; then printf '%s\n' "$*" >&2; else printf '%s\n' "$*"; fi; }

failed=0 selected=0
for name in "${SCENARIOS[@]}"; do
  [ -z "$ONLY" ] || [ "$ONLY" = "$name" ] || continue
  selected=$((selected + 1))
  branch="$(xfail_branch "$name")"
  rc=0
  out="$(bash "${BASH_SOURCE[0]}" --run-one "$name" 2>&1)" || rc=$?
  last="$(printf '%s\n' "$out" | tail -n 1)"
  if [ "$rc" = 0 ]; then
    if [ -n "$branch" ]; then status="unexpected-pass"; else status="pass"; fi
  elif [ "$rc" = 3 ] && [ -n "$branch" ]; then
    status="xfail($branch)"
  else
    status="fail"
  fi
  say "eval: $name: $status"
  case "$status" in
    pass) printf '%s\t%s\t%s\t%s\n' "$name" pass "" "$last" >> "$RESULTS" ;;
    xfail*)
      say "eval:   observed: $last"
      printf '%s\t%s\t%s\t%s\n' "$name" xfail "$branch" "$last" >> "$RESULTS" ;;
    unexpected-pass)
      say "eval:   $name now holds on this tree: remove it from xfail_branch (fixed by $branch)"
      printf '%s\t%s\t%s\t%s\n' "$name" unexpected-pass "$branch" "$last" >> "$RESULTS"; failed=1 ;;
    fail)
      printf '%s\n' "$out" | tail -n 12 | sed 's/^/eval:   | /' | while IFS= read -r line; do say "$line"; done
      printf '%s\t%s\t%s\t%s\n' "$name" fail "$branch" "$last" >> "$RESULTS"; failed=1 ;;
  esac
done
[ -z "$ONLY" ] || [ "$selected" -gt 0 ] || { echo "error: unknown scenario: $ONLY" >&2; exit 2; }

summary="$(python3 - "$RESULTS" <<'PY'
import json, sys
rows = [line.rstrip("\n").split("\t") for line in open(sys.argv[1], encoding="utf-8")]
counts = {key: 0 for key in ("pass", "xfail", "fail", "unexpected-pass")}
scenarios = []
for name, status, branch, detail in rows:
    counts[status] += 1
    entry = {"name": name, "status": status, "detail": detail}
    if branch:
        entry["fixed_by"] = branch
    scenarios.append(entry)
ok = counts["fail"] == 0 and counts["unexpected-pass"] == 0
print(json.dumps({"schema": 1, "ok": ok, "counts": counts, "scenarios": scenarios}, sort_keys=True))
PY
)"
read -r total_pass total_xfail total_fail total_unexpected < <(printf '%s' "$summary" | python3 -c '
import json, sys
c = json.load(sys.stdin)["counts"]
print(c["pass"], c["xfail"], c["fail"], c["unexpected-pass"])')
say "eval: summary: $total_pass pass, $total_xfail xfail, $total_fail fail, $total_unexpected unexpected-pass"
if [ "$total_xfail" -gt 0 ]; then
  say "eval: xfail (known defects, each fixed by an open branch; not counted as failures):"
  awk -F'\t' '$2 == "xfail" {print "eval:   " $1 " -> " $3}' "$RESULTS" | while IFS= read -r line; do say "$line"; done
fi
[ "$JSON" != 1 ] || printf '%s\n' "$summary"
if [ "$failed" = 0 ]; then
  say "harness-eval-smoke: ok"
fi
exit "$failed"
