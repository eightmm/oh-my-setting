#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-goal-recovery.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "goal-drive-recovery-smoke: $*" >&2; exit 1; }

make_case() {  # DIR MODE
  local repo="$1"
  local mode="$2"
  local allowed="new.txt"

  mkdir -p "$repo/scripts"
  git -C "$repo" init -q -b main
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name Test
  printf 'base\n' > "$repo/tracked.txt"
  cat > "$repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  new) grep -Fxq autonomous-new new.txt ;;
  tracked) grep -Fxq autonomous-tracked tracked.txt ;;
  *) exit 2 ;;
esac
EOF
  chmod +x "$repo/scripts/check.sh"
  git -C "$repo" add tracked.txt scripts/check.sh
  git -C "$repo" commit -qm base
  [ "$mode" != tracked ] || allowed="tracked.txt"
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" init --goal "recover $mode" \
    --accept "bash scripts/check.sh $mode" >/dev/null
  "$ROOT/scripts/agent-plan.sh" --repo "$repo" add --id "$mode" \
    --title "fix: recover $mode landing" --allowed "$allowed" \
    --verify "bash scripts/check.sh $mode" >/dev/null
}

append_outer_intent() {  # REPO TASK SOURCE_PATCH INTENT_ID PHASE PROVIDER
  local repo="$1"
  local task="$2"
  local source_patch="$3"
  local intent_id="$4"
  local phase="$5"
  local provider="$6"
  local frozen_dir frozen

  frozen_dir="$repo/.oms/plan/commit-patches"
  mkdir -p "$frozen_dir"
  frozen="$frozen_dir/$intent_id.patch"
  cp "$source_patch" "$frozen"
  python3 - "$repo" "$task" "$frozen" "$intent_id" "$phase" "$provider" <<'PY'
import hashlib, json, pathlib, subprocess, sys, time

repo = pathlib.Path(sys.argv[1])
task_id, patch_arg, intent_id, phase, provider = sys.argv[2:]
patch = pathlib.Path(patch_arg)
plan = json.loads((repo / ".oms/plan/tasks.json").read_text(encoding="utf-8"))
task = plan["tasks"][task_id]
raw = subprocess.check_output(
    ["git", "-C", str(repo), "apply", "--numstat", "-z", str(patch)])
parts = raw.split(b"\0")
paths = []
i = 0
while i < len(parts):
    record = parts[i]
    i += 1
    if not record:
        continue
    fields = record.split(b"\t", 2)
    if len(fields) != 3:
        raise SystemExit("invalid numstat")
    if fields[2]:
        paths.append(fields[2])
    else:
        paths.extend(parts[i:i + 2])
        i += 2
paths = sorted({value.decode("utf-8", "surrogateescape") for value in paths})
row = {
    "schema": 1,
    "kind": "commit-intent",
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "run_id": "fixture-run",
    "cycle": 1,
    "intent_id": intent_id,
    "phase": phase,
    "reason": "recovery-fixture",
    "task_id": task_id,
    "base_sha": subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip(),
    "patch": str(patch.relative_to(repo)).replace("\\", "/"),
    "patch_sha256": hashlib.sha256(patch.read_bytes()).hexdigest(),
    "paths": paths,
    "title": task["title"],
    "provider": provider,
    "lease_id": task["lease_id"],
    "verify_sha256": hashlib.sha256(
        task["verify"].replace("\r", "").encode("utf-8")).hexdigest(),
}
with open(repo / ".oms/plan/progress.jsonl", "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
PY
  printf '%s\n' "$frozen"
}

bin="$TMP/bin"
home="$TMP/home"
calls="$TMP/provider-calls"
mkdir -p "$bin" "$home"
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
case "${OMS_TASK_ID:-}" in
  new) printf 'autonomous-new\n' > new.txt ;;
  tracked) printf 'autonomous-tracked\n' > tracked.txt ;;
  *) exit 9 ;;
esac
echo worker-ok
EOF
chmod +x "$bin/codex"

for mode in new tracked; do
  repo="$TMP/$mode"
  make_case "$repo" "$mode"
  before="$(git -C "$repo" rev-parse HEAD)"
  rc=0
  HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" CALL_LOG="$calls" \
    OMS_GOAL_DRIVE_TEST_STOP_AFTER_LAND=1 \
    "$ROOT/scripts/goal-drive.sh" --repo "$repo" --to codex --max-cycles 2 \
    > "$TMP/$mode-stop.out" 2>&1 || rc=$?
  [ "$rc" = 75 ] || fail "$mode crash fixture should stop after landing with 75, got $rc"
  [ "$(git -C "$repo" rev-parse HEAD)" = "$before" ] ||
    fail "$mode stop committed before the injected crash"
  [ -n "$(git -C "$repo" status --porcelain)" ] ||
    fail "$mode stop did not leave the landed bytes to recover"

  HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" CALL_LOG="$calls" \
    "$ROOT/scripts/goal-drive.sh" --repo "$repo" --to codex --max-cycles 2 \
    > "$TMP/$mode-resume.out" 2>&1 ||
    fail "$mode recovery failed: $(tail -8 "$TMP/$mode-resume.out")"
  [ "$(git -C "$repo" rev-list --count "$before"..HEAD)" = 1 ] ||
    fail "$mode recovery should create exactly one commit"
  [ -z "$(git -C "$repo" status --porcelain)" ] ||
    fail "$mode recovery left tracked or untracked bytes"
  grep -Fq 'recovered commit intent' "$TMP/$mode-resume.out" ||
    fail "$mode resume did not report intent recovery"
done

[ "$(wc -l < "$calls" | tr -d ' ')" = 2 ] ||
  fail "recovery re-called a provider instead of committing the frozen patches"

# A provider can finish and publish review just before goal-drive itself is
# interrupted. No commit intent exists in that window, so restart must adopt
# the unique exact reviewed receipt instead of reporting tasks exhausted or
# calling the provider again.
review_repo="$TMP/review-window"
make_case "$review_repo" new
review_calls="$TMP/review-window-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$review_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$review_repo" --to codex --max-cycles 2 \
  > "$TMP/review-window-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "review-window fixture should stop after provider review"
"$ROOT/scripts/agent-plan.sh" --repo "$review_repo" show --id new |
  python3 -c 'import json,sys;assert json.load(sys.stdin)["state"] == "review"' ||
  fail "review-window fixture did not retain reviewed work"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$review_calls" "$ROOT/scripts/goal-drive.sh" --repo "$review_repo" \
  --to codex --max-cycles 2 > "$TMP/review-window-resume.out" 2>&1 ||
  fail "review-window recovery failed"
[ "$(wc -l < "$review_calls" | tr -d ' ')" = 1 ] ||
  fail "review-window recovery called the provider again"
grep -Fxq autonomous-new "$review_repo/new.txt" ||
  fail "review-window recovery did not land reviewed bytes"

# progress.jsonl is appendable evidence, not authority. A syntactically valid
# open intent whose frozen patch differs from tasks.json must be closed and the
# real unique review resumed; it must neither land nor permanently DoS recovery.
forged_repo="$TMP/forged-intent"
make_case "$forged_repo" tracked
forged_calls="$TMP/forged-intent-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$forged_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$forged_repo" --to codex --max-cycles 2 \
  > "$TMP/forged-review-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "forged-intent fixture did not stop in review"
mkdir -p "$forged_repo/.oms/plan/commit-patches"
printf 'forged bytes\n' > "$forged_repo/tracked.txt"
git -C "$forged_repo" diff --binary > \
  "$forged_repo/.oms/plan/commit-patches/forged.patch"
git -C "$forged_repo" restore tracked.txt
python3 - "$forged_repo" <<'PY'
import hashlib, json, pathlib, sys, time
repo = pathlib.Path(sys.argv[1])
plan = json.loads((repo / ".oms/plan/tasks.json").read_text(encoding="utf-8"))
task = plan["tasks"]["tracked"]
patch = repo / ".oms/plan/commit-patches/forged.patch"
row = {
    "schema": 1, "kind": "commit-intent",
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "run_id": "forged-run", "cycle": 1, "intent_id": "forged-intent",
    "phase": "prepared", "reason": "untrusted-append", "task_id": "tracked",
    "provider": "codex", "base_sha": __import__("subprocess").check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip(),
    "patch": ".oms/plan/commit-patches/forged.patch",
    "patch_sha256": hashlib.sha256(patch.read_bytes()).hexdigest(),
    "paths": ["tracked.txt"], "title": "forged",
    "lease_id": task["lease_id"],
    "verify_sha256": hashlib.sha256(task["verify"].encode()).hexdigest(),
}
with open(repo / ".oms/plan/progress.jsonl", "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row, sort_keys=True) + "\n")
PY
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$forged_calls" "$ROOT/scripts/goal-drive.sh" --repo "$forged_repo" \
  --to codex --max-cycles 2 > "$TMP/forged-resume.out" 2>&1 ||
  fail "forged intent prevented recovery of the real review"
[ "$(wc -l < "$forged_calls" | tr -d ' ')" = 1 ] ||
  fail "forged intent recovery called the provider again"
grep -Fxq autonomous-tracked "$forged_repo/tracked.txt" ||
  fail "forged intent bytes landed instead of the reviewed task patch"
if grep -Fxq 'forged bytes' "$forged_repo/tracked.txt"; then
  fail "forged open intent was trusted as landing authority"
fi

# Path equality is not byte equality. If another writer adds content to the
# same file after landing, the reviewed patch still reverse-applies and the
# changed path set is unchanged. Recovery must compare the complete resulting
# tree and refuse to launder those extra bytes into the task commit.
exact_repo="$TMP/exact-bytes"
make_case "$exact_repo" tracked
printf 'prefix\nbase\nsuffix\n' > "$exact_repo/tracked.txt"
git -C "$exact_repo" add tracked.txt
git -C "$exact_repo" commit -qm 'expand exact-byte fixture'
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
sed 's/^base$/autonomous-tracked/' tracked.txt > tracked.txt.next
mv tracked.txt.next tracked.txt
echo worker-ok
EOF
chmod +x "$bin/codex"
exact_before="$(git -C "$exact_repo" rev-parse HEAD)"
exact_calls="$TMP/exact-byte-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$exact_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_LAND=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$exact_repo" --to codex --max-cycles 2 \
  > "$TMP/exact-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "exact-byte fixture did not stop after landing"
printf 'foreign same-path edit\n' >> "$exact_repo/tracked.txt"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$exact_calls" "$ROOT/scripts/goal-drive.sh" --repo "$exact_repo" \
  --to codex --max-cycles 2 > "$TMP/exact-resume.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "same-path extra bytes were not parked, got $rc"
[ "$(git -C "$exact_repo" rev-parse HEAD)" = "$exact_before" ] ||
  fail "same-path extra bytes were committed with the frozen intent"
grep -Fq 'foreign same-path edit' "$exact_repo/tracked.txt" ||
  fail "exact-byte refusal discarded the foreign edit"
[ "$(wc -l < "$exact_calls" | tr -d ' ')" = 1 ] ||
  fail "exact-byte recovery called the provider again"

# The worktree alone is not the complete publication precondition. Another
# session can stage different bytes for the same reviewed path, then leave the
# worktree at the exact frozen candidate. Publishing must see that real-index
# divergence, preserve the staged blob, and leave HEAD at the recorded base.
index_repo="$TMP/staged-index-before-publish"
make_case "$index_repo" tracked
index_before="$(git -C "$index_repo" rev-parse HEAD)"
index_calls="$TMP/staged-index-before-publish-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$index_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_LAND=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$index_repo" --to codex --max-cycles 2 \
  > "$TMP/staged-index-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "staged-index fixture did not stop after landing"
printf 'foreign staged-only bytes\n' > "$index_repo/tracked.txt"
git -C "$index_repo" add tracked.txt
index_foreign_blob="$(git -C "$index_repo" rev-parse :tracked.txt)"
printf 'autonomous-tracked\n' > "$index_repo/tracked.txt"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$index_calls" "$ROOT/scripts/goal-drive.sh" --repo "$index_repo" \
  --to codex --max-cycles 2 > "$TMP/staged-index-resume.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "staged-only index collision should park before publication, got $rc"
[ "$(git -C "$index_repo" rev-parse HEAD)" = "$index_before" ] ||
  fail "staged-only index collision advanced HEAD"
[ "$(git -C "$index_repo" rev-parse :tracked.txt)" = "$index_foreign_blob" ] ||
  fail "staged-only index collision lost the foreign staged blob"
[ "$(git -C "$index_repo" show :tracked.txt | tr -d '\r')" = \
  'foreign staged-only bytes' ] ||
  fail "staged-only index collision changed the foreign staged bytes"
grep -Fxq autonomous-tracked "$index_repo/tracked.txt" ||
  fail "staged-only index collision changed the exact candidate worktree"
[ "$(wc -l < "$index_calls" | tr -d ' ')" = 1 ] ||
  fail "staged-only index collision called the provider again"

# If interruption happens after the exact ref CAS but before index alignment,
# recovery may align only an index that still proves the recorded base. A
# same-path staged-only write in that window must survive unchanged even though
# the exact autonomous commit is already attached.
post_ref_repo="$TMP/staged-index-after-ref"
make_case "$post_ref_repo" tracked
post_ref_before="$(git -C "$post_ref_repo" rev-parse HEAD)"
post_ref_calls="$TMP/staged-index-after-ref-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$post_ref_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REF=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$post_ref_repo" --to codex --max-cycles 2 \
  > "$TMP/staged-index-after-ref-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "post-ref fixture did not stop between ref and index publication"
[ "$(git -C "$post_ref_repo" rev-list --count "$post_ref_before"..HEAD)" = 1 ] ||
  fail "post-ref fixture did not attach exactly one reviewed commit"
post_ref_commit="$(git -C "$post_ref_repo" rev-parse HEAD)"
printf 'foreign post-ref staged bytes\n' > "$post_ref_repo/tracked.txt"
git -C "$post_ref_repo" add tracked.txt
post_ref_foreign_blob="$(git -C "$post_ref_repo" rev-parse :tracked.txt)"
printf 'autonomous-tracked\n' > "$post_ref_repo/tracked.txt"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$post_ref_calls" "$ROOT/scripts/goal-drive.sh" --repo "$post_ref_repo" \
  --to codex --max-cycles 2 > "$TMP/staged-index-after-ref-resume.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "post-ref staged-only collision should park, got $rc"
[ "$(git -C "$post_ref_repo" rev-parse HEAD)" = "$post_ref_commit" ] ||
  fail "post-ref staged-only collision changed the exact published commit"
[ "$(git -C "$post_ref_repo" rev-parse :tracked.txt)" = "$post_ref_foreign_blob" ] ||
  fail "post-ref staged-only collision lost the foreign staged blob"
[ "$(git -C "$post_ref_repo" show :tracked.txt | tr -d '\r')" = \
  'foreign post-ref staged bytes' ] ||
  fail "post-ref staged-only collision changed the foreign staged bytes"
grep -Fxq autonomous-tracked "$post_ref_repo/tracked.txt" ||
  fail "post-ref staged-only collision changed the exact candidate worktree"
[ "$(wc -l < "$post_ref_calls" | tr -d ' ')" = 1 ] ||
  fail "post-ref staged-only collision called the provider again"

# A second crash window exists after the one-shot repair publishes a new
# review but before goal-drive freezes its replacement intent. Journal the
# repair phase first, then recover the new same-lease receipt without a third
# provider call.
repair_review_repo="$TMP/repair-review-window"
mkdir -p "$repair_review_repo/scripts"
git -C "$repair_review_repo" init -q -b main
git -C "$repair_review_repo" config user.email test@example.com
git -C "$repair_review_repo" config user.name Test
printf 'base\n' > "$repair_review_repo/tracked.txt"
printf '#!/usr/bin/env bash\nexit 0\n' > "$repair_review_repo/scripts/check.sh"
chmod +x "$repair_review_repo/scripts/check.sh"
git -C "$repair_review_repo" add -A
git -C "$repair_review_repo" commit -qm base
"$ROOT/scripts/agent-plan.sh" --repo "$repair_review_repo" init \
  --goal repair-review --accept 'grep -Fxq repaired tracked.txt' >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$repair_review_repo" add --id repair-review \
  --title 'fix: recover repaired review' --allowed 'tracked.txt,scripts/check.sh' \
  --verify true >/dev/null
repair_review_calls="$TMP/repair-review-window-calls"
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
count=0
[ ! -f "$CALL_LOG" ] || count="$(wc -l < "$CALL_LOG" | tr -d ' ')"
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
if [ "$count" -eq 0 ]; then
  printf '#!/usr/bin/env bash\n# candidate verifier edit\nexit 0\n' > scripts/check.sh
else
  printf 'repaired\n' > tracked.txt
fi
echo worker-ok
EOF
chmod +x "$bin/codex"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$repair_review_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REPAIR_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$repair_review_repo" --to codex \
  --max-cycles 2 --auto-repair > "$TMP/repair-review-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "repair-review fixture did not stop after repaired review"
[ "$(wc -l < "$repair_review_calls" | tr -d ' ')" = 2 ] ||
  fail "repair-review fixture did not make exactly initial+repair calls"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$repair_review_calls" "$ROOT/scripts/goal-drive.sh" \
  --repo "$repair_review_repo" --to codex --max-cycles 2 --auto-repair \
  > "$TMP/repair-review-resume.out" 2>&1 ||
  fail "repaired review intent recovery failed"
[ "$(wc -l < "$repair_review_calls" | tr -d ' ')" = 2 ] ||
  fail "repaired review recovery made a third provider call"
grep -Fxq repaired "$repair_review_repo/tracked.txt" ||
  fail "repaired review recovery did not land replacement bytes"
grep -Fq 'recovered commit intent' "$TMP/repair-review-resume.out" ||
  fail "repaired review was not re-frozen under its existing outer intent"

# If the one repair worker itself fails, peer-delegate must block the exact
# repair lease before returning. The driver then terminalizes the prepared
# intent instead of leaving every future run behind an unrecoverable patch.
repair_repo="$TMP/repair-worker-failure"
make_case "$repair_repo" tracked
repair_calls="$TMP/repair-worker-calls"
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
count=0
[ ! -f "$CALL_LOG" ] || count="$(wc -l < "$CALL_LOG" | tr -d ' ')"
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
if [ "$count" -ge 1 ]; then
  echo "repair provider failed" >&2
  exit 9
fi
printf 'autonomous-tracked\n' > tracked.txt
echo worker-ok
EOF
chmod +x "$bin/codex"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$repair_calls" OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$repair_repo" --to codex \
  --max-cycles 2 --auto-repair > "$TMP/repair-worker-failure.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "failed repair worker should park the run, got $rc"
[ "$(wc -l < "$repair_calls" | tr -d ' ')" = 2 ] ||
  fail "failed repair fixture did not make one initial and one repair call"
grep -Fq 'plan: tracked -> blocked' "$TMP/repair-worker-failure.out" ||
  fail "failed goal repair did not block inside delegated execution"
if grep -Fq 'plan: tracked -> ready' "$TMP/repair-worker-failure.out"; then
  fail "failed goal repair exposed an intermediate ready state"
fi
[ -z "$(git -C "$repair_repo" status --porcelain)" ] ||
  fail "failed repair worker left main-tree bytes"
if ! python3 - "$repair_repo/.oms/plan/progress.jsonl" <<'PY'
import json, sys

rows = []
for line in open(sys.argv[1], encoding="utf-8"):
    row = json.loads(line)
    if row.get("kind") == "commit-intent":
        rows.append(row)
assert rows and rows[-1].get("phase") == "abandoned", rows[-3:]
PY
then
  fail "failed repair worker left an unrecoverable open commit intent"
fi
"$ROOT/scripts/agent-plan.sh" --repo "$repair_repo" show --id tracked |
  python3 -c 'import json,sys
d=json.load(sys.stdin)
assert d["state"] == "blocked" and d["repair_count"] == 1, d
assert d["lease_id"] and d["review_lease_id"] == d["lease_id"], d' ||
  fail "failed bounded repair did not terminalize its task"
repair_calls_before="$(wc -l < "$repair_calls" | tr -d ' ')"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$repair_calls" "$ROOT/scripts/goal-drive.sh" --repo "$repair_repo" \
  --to codex --max-cycles 2 --auto-repair \
  > "$TMP/repair-worker-rerun.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "blocked repair rerun should park with no actionable task"
[ "$(wc -l < "$repair_calls" | tr -d ' ')" = "$repair_calls_before" ] ||
  fail "a later goal-drive re-delegated the failed bounded repair task"

# A successful repair process can still publish no patch when its verifier is
# intentionally broad. Publishing that empty review replaces the old review
# evidence, so the old frozen intent must be abandoned just like a failed
# repair process instead of blocking every restart.
empty_repo="$TMP/empty-repair"
mkdir -p "$empty_repo/scripts"
git -C "$empty_repo" init -q -b main
git -C "$empty_repo" config user.email test@example.com
git -C "$empty_repo" config user.name Test
printf 'base\n' > "$empty_repo/tracked.txt"
printf '#!/usr/bin/env bash\ngrep -Fxq autonomous-tracked tracked.txt\n' \
  > "$empty_repo/scripts/check.sh"
chmod +x "$empty_repo/scripts/check.sh"
git -C "$empty_repo" add tracked.txt scripts/check.sh
git -C "$empty_repo" commit -qm base
"$ROOT/scripts/agent-plan.sh" --repo "$empty_repo" init --goal empty-repair \
  --accept 'bash scripts/check.sh' >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$empty_repo" add --id empty-repair \
  --title 'fix: reject empty repair intent' --allowed tracked.txt \
  --verify true >/dev/null
empty_calls="$TMP/empty-repair-calls"
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
count=0
[ ! -f "$CALL_LOG" ] || count="$(wc -l < "$CALL_LOG" | tr -d ' ')"
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
if [ "$count" -eq 0 ]; then
  printf 'autonomous-tracked\n' > tracked.txt
else
  echo "nothing to repair"
fi
EOF
chmod +x "$bin/codex"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$empty_calls" OMS_REQUIRE_LANDING_APPROVAL=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$empty_repo" --to codex \
  --max-cycles 2 --auto-repair > "$TMP/empty-repair.out" 2>&1 || rc=$?
[ "$rc" = 3 ] || fail "empty repair should park the run, got $rc"
if ! python3 - "$empty_repo/.oms/plan/progress.jsonl" <<'PY'
import json, sys

rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
intents = [row for row in rows if row.get("kind") == "commit-intent"]
assert intents and intents[-1].get("phase") == "abandoned", intents[-3:]
PY
then
  fail "empty repaired review left the superseded commit intent open"
fi

# progress.jsonl is appendable evidence. A malformed JSON value and a second
# forged open row must be ignored before valid-intent multiplicity is decided;
# neither may hide the exact outer intent or force another provider call.
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
printf 'autonomous-tracked\n' > tracked.txt
echo worker-ok
EOF
chmod +x "$bin/codex"
multi_repo="$TMP/valid-plus-forged"
make_case "$multi_repo" tracked
multi_before="$(git -C "$multi_repo" rev-parse HEAD)"
multi_calls="$TMP/valid-plus-forged-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$multi_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$multi_repo" --to codex --max-cycles 2 \
  > "$TMP/valid-plus-forged-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "valid-plus-forged fixture did not stop in review"
multi_patch="$("$ROOT/scripts/agent-plan.sh" --repo "$multi_repo" show --id tracked |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["patch"])' | tr -d '\r')"
append_outer_intent "$multi_repo" tracked "$multi_patch" valid-outer prepared codex >/dev/null
printf 'forged concurrent bytes\n' > "$multi_repo/tracked.txt"
git -C "$multi_repo" diff --binary > "$TMP/forged-open.patch"
git -C "$multi_repo" restore tracked.txt
append_outer_intent "$multi_repo" tracked "$TMP/forged-open.patch" forged-outer prepared claude >/dev/null
printf '[]\n' >> "$multi_repo/.oms/plan/progress.jsonl"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$multi_calls" "$ROOT/scripts/goal-drive.sh" --repo "$multi_repo" \
  --to codex --max-cycles 2 > "$TMP/valid-plus-forged-resume.out" 2>&1 ||
  fail "valid outer intent was blocked by forged progress evidence"
[ "$(wc -l < "$multi_calls" | tr -d ' ')" = 1 ] ||
  fail "forged progress evidence caused a duplicate provider call"
[ "$(git -C "$multi_repo" rev-list --count "$multi_before"..HEAD)" = 1 ] ||
  fail "valid outer intent did not publish exactly one commit"
grep -Fxq autonomous-tracked "$multi_repo/tracked.txt" ||
  fail "forged progress evidence displaced the reviewed bytes"

# Simulate patch-land crashing after its durable inner intent, plan fence, and
# exact apply but before plan completion. goal-drive must run receipt recovery,
# keep its outer intent, and publish the exact commit without another provider.
landing_repo="$TMP/interrupted-landing"
make_case "$landing_repo" tracked
landing_before="$(git -C "$landing_repo" rev-parse HEAD)"
landing_calls="$TMP/interrupted-landing-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$landing_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$landing_repo" --to codex --max-cycles 2 \
  > "$TMP/interrupted-landing-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "interrupted-landing fixture did not stop in review"
landing_task_json="$("$ROOT/scripts/agent-plan.sh" --repo "$landing_repo" show --id tracked)"
landing_patch="$(printf '%s' "$landing_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["patch"])' | tr -d '\r')"
landing_lease="$(printf '%s' "$landing_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["lease_id"])' | tr -d '\r')"
landing_verify="$(printf '%s' "$landing_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["verify"])' | tr -d '\r')"
landing_patch_sha="$(python3 - "$landing_patch" <<'PY' | tr -d '\r'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
landing_frozen="$(append_outer_intent \
  "$landing_repo" tracked "$landing_patch" interrupted-outer prepared codex)"
python3 - "$landing_repo" "$landing_frozen" "$landing_lease" <<'PY'
import hashlib, json, pathlib, subprocess, sys, time

repo = pathlib.Path(sys.argv[1])
patch = pathlib.Path(sys.argv[2])
task = json.loads(
    (repo / ".oms/plan/tasks.json").read_text(encoding="utf-8")
)["tasks"]["tracked"]

def plan_receipt(value):
    value = dict(value)
    for name in ("state", "updated", "claim_expired", "claim_age_s"):
        value.pop(name, None)
    raw = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(raw).hexdigest()

done_task = dict(task)
done_task["patch"] = str(patch)
row = {
    "schema": 1,
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "landing_id": "interrupted-inner",
    "event": "intent",
    "patch": str(patch),
    "patch_sha": hashlib.sha256(patch.read_bytes()).hexdigest(),
    "base_sha": subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip(),
    "task": "tracked",
    "lease": sys.argv[3],
    "plan_receipt_sha": plan_receipt(task),
    "plan_done_receipt_sha": plan_receipt(done_task),
    "approval": "",
    "approval_version": "",
}
receipt = {"schema": 1}
for name in (
    "landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
    "plan_receipt_sha", "plan_done_receipt_sha", "approval",
    "approval_version",
):
    receipt[name] = row[name]
row["receipt_sha"] = hashlib.sha256(json.dumps(
    receipt, ensure_ascii=False, sort_keys=True, separators=(",", ":")
).encode()).hexdigest()
with open(repo / ".oms/landings.jsonl", "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row, ensure_ascii=False) + "\n")
PY
"$ROOT/scripts/agent-plan.sh" --repo "$landing_repo" land --id tracked \
  --lease-id "$landing_lease" \
  --expected-review-patch "$landing_patch" \
  --expected-review-patch-sha256 "$landing_patch_sha" \
  --expected-review-verify "$landing_verify" \
  --expected-review-executor-id "" \
  --expected-review-executor-soul-sha256 "" \
  --expected-review-lease-id "$landing_lease" >/dev/null
git -C "$landing_repo" apply --binary "$landing_frozen"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$landing_calls" "$ROOT/scripts/goal-drive.sh" --repo "$landing_repo" \
  --to codex --max-cycles 2 > "$TMP/interrupted-landing-resume.out" 2>&1 ||
  fail "interrupted landing did not converge through patch-land recovery"
[ "$(wc -l < "$landing_calls" | tr -d ' ')" = 1 ] ||
  fail "interrupted landing recovery called the provider again"
[ "$(git -C "$landing_repo" rev-list --count "$landing_before"..HEAD)" = 1 ] ||
  fail "interrupted landing recovery did not publish exactly one commit"
[ -z "$(git -C "$landing_repo" status --porcelain)" ] ||
  fail "interrupted landing recovery left the repository dirty"

# A same-id append cannot terminalize a valid landed handle without carrying
# and matching its authority receipt. This forged minimal abandoned row must be
# ignored so the exact dirty tree still commits without another provider call.
terminal_repo="$TMP/forged-same-id-terminal"
make_case "$terminal_repo" tracked
terminal_before="$(git -C "$terminal_repo" rev-parse HEAD)"
terminal_calls="$TMP/forged-same-id-terminal-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$terminal_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$terminal_repo" --to codex --max-cycles 2 \
  > "$TMP/forged-same-id-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "same-id terminal fixture did not stop in review"
terminal_task_json="$("$ROOT/scripts/agent-plan.sh" --repo "$terminal_repo" show --id tracked)"
terminal_patch="$(printf '%s' "$terminal_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["patch"])' | tr -d '\r')"
terminal_verify="$(printf '%s' "$terminal_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["verify"])' | tr -d '\r')"
terminal_frozen="$(append_outer_intent \
  "$terminal_repo" tracked "$terminal_patch" same-id-outer prepared codex)"
"$ROOT/scripts/patch-land.sh" --repo "$terminal_repo" --patch "$terminal_frozen" \
  --plan-task tracked --verify "$terminal_verify" >/dev/null 2>&1 ||
  fail "same-id terminal fixture could not land its exact reviewed patch"
append_outer_intent \
  "$terminal_repo" tracked "$terminal_patch" same-id-outer landed codex >/dev/null
printf '%s\n' '{"kind":"commit-intent","intent_id":"same-id-outer","phase":"abandoned"}' \
  >> "$terminal_repo/.oms/plan/progress.jsonl"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$terminal_calls" "$ROOT/scripts/goal-drive.sh" --repo "$terminal_repo" \
  --to codex --max-cycles 2 > "$TMP/forged-same-id-resume.out" 2>&1 ||
  fail "forged same-id terminal hid the valid landed commit handle"
[ "$(wc -l < "$terminal_calls" | tr -d ' ')" = 1 ] ||
  fail "forged same-id terminal caused another provider call"
[ "$(git -C "$terminal_repo" rev-list --count "$terminal_before"..HEAD)" = 1 ] ||
  fail "same-id terminal recovery did not publish exactly one commit"
[ -z "$(git -C "$terminal_repo" status --porcelain)" ] ||
  fail "same-id terminal recovery left the repository dirty"

# A later clean commit must not resurrect the exact committed handle. Only the
# current branch's first-parent lineage is admissible evidence; this exercises
# the same A -> B restart shape as a multi-task goal without trusting --all.
printf 'first-parent descendant\n' > "$terminal_repo/descendant.txt"
git -C "$terminal_repo" add descendant.txt
git -C "$terminal_repo" commit -qm 'test: advance first-parent lineage'
terminal_descendant="$(git -C "$terminal_repo" rev-parse HEAD)"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$terminal_calls" "$ROOT/scripts/goal-drive.sh" --repo "$terminal_repo" \
  --to codex --max-cycles 1 > "$TMP/committed-ancestor-resume.out" 2>&1 ||
  fail "first-parent descendant resurrected a committed intent"
[ "$(git -C "$terminal_repo" rev-parse HEAD)" = "$terminal_descendant" ] ||
  fail "committed ancestor recovery published an unexpected commit"
[ "$(wc -l < "$terminal_calls" | tr -d ' ')" = 1 ] ||
  fail "committed ancestor recovery called the provider"

# A clean external base advance can leave the reviewed patch applicable. The
# old prepared handle is legitimately abandoned before the same review is
# re-frozen on the new HEAD; that terminal must remain closed after the rebased
# exact commit instead of resurfacing as a stale second intent on the next run.
base_move_repo="$TMP/frozen-base-mismatch"
make_case "$base_move_repo" tracked
base_move_calls="$TMP/frozen-base-mismatch-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$base_move_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$base_move_repo" --to codex --max-cycles 2 \
  > "$TMP/frozen-base-review-stop.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "frozen-base fixture did not stop in review"
base_move_task_json="$("$ROOT/scripts/agent-plan.sh" --repo "$base_move_repo" show --id tracked)"
base_move_patch="$(printf '%s' "$base_move_task_json" |
  python3 -c 'import json,sys;print(json.load(sys.stdin)["patch"])' | tr -d '\r')"
append_outer_intent \
  "$base_move_repo" tracked "$base_move_patch" old-base-outer prepared codex >/dev/null
printf 'external clean base advance\n' > "$base_move_repo/base-move.txt"
git -C "$base_move_repo" add base-move.txt
git -C "$base_move_repo" commit -qm 'test: advance clean base'
base_move_advanced="$(git -C "$base_move_repo" rev-parse HEAD)"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$base_move_calls" "$ROOT/scripts/goal-drive.sh" --repo "$base_move_repo" \
  --to codex --max-cycles 2 > "$TMP/frozen-base-first-resume.out" 2>&1 ||
  fail "review did not re-freeze and commit on the clean advanced base"
[ "$(wc -l < "$base_move_calls" | tr -d ' ')" = 1 ] ||
  fail "clean base advance caused another provider call"
[ "$(git -C "$base_move_repo" rev-list --count "$base_move_advanced"..HEAD)" = 1 ] ||
  fail "clean base advance did not publish one rebased exact commit"
base_move_committed="$(git -C "$base_move_repo" rev-parse HEAD)"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$base_move_calls" "$ROOT/scripts/goal-drive.sh" --repo "$base_move_repo" \
  --to codex --max-cycles 1 > "$TMP/frozen-base-second-resume.out" 2>&1 ||
  fail "legitimate frozen-base abandonment resurfaced on the next run"
[ "$(git -C "$base_move_repo" rev-parse HEAD)" = "$base_move_committed" ] ||
  fail "second frozen-base resume published an unexpected commit"
[ "$(wc -l < "$base_move_calls" | tr -d ' ')" = 1 ] ||
  fail "second frozen-base resume called the provider"

# The exact publisher must not run a Git commit hook that can restage foreign
# bytes. Admission/verifier evidence is the hook policy for autonomous commits;
# commit-tree + update-ref publishes only the frozen tree and parent.
hook_repo="$TMP/commit-hook"
make_case "$hook_repo" tracked
hook_calls="$TMP/commit-hook-calls"
mkdir -p "$hook_repo/.git/hooks"
cat > "$hook_repo/.git/hooks/pre-commit" <<'EOF'
#!/bin/sh
printf 'foreign hook bytes\n' > foreign-hook.txt
git add foreign-hook.txt
EOF
cat > "$hook_repo/.git/hooks/reference-transaction" <<'EOF'
#!/bin/sh
printf '%s\n' "${1:-missing}" >> reference-transaction-hook.txt
exit 1
EOF
chmod +x "$hook_repo/.git/hooks/pre-commit" \
  "$hook_repo/.git/hooks/reference-transaction"
git -C "$hook_repo" config core.hooksPath "$hook_repo/.git/hooks"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$hook_calls" "$ROOT/scripts/goal-drive.sh" --repo "$hook_repo" \
  --to codex --max-cycles 2 > "$TMP/commit-hook.out" 2>&1 || rc=$?
[ ! -e "$hook_repo/reference-transaction-hook.txt" ] ||
  fail "autonomous commit executed a rejecting reference-transaction hook"
[ "$rc" = 0 ] ||
  fail "exact commit publisher failed with disabled commit hooks"
[ "$(wc -l < "$hook_calls" | tr -d ' ')" = 1 ] ||
  fail "commit-hook fixture made a duplicate provider call"
[ ! -e "$hook_repo/foreign-hook.txt" ] ||
  fail "autonomous commit executed a mutating pre-commit hook"
[ -z "$(git -C "$hook_repo" ls-tree --name-only HEAD -- foreign-hook.txt)" ] ||
  fail "autonomous commit included bytes created by a Git hook"

# A terminal result without its durable row is not success. Acceptance already
# passes, so no provider or landing work is needed before the injected append
# failure.
terminal_write_repo="$TMP/terminal-write-failure"
make_case "$terminal_write_repo" tracked
printf 'autonomous-tracked\n' > "$terminal_write_repo/tracked.txt"
git -C "$terminal_write_repo" add tracked.txt
git -C "$terminal_write_repo" commit -qm 'test: satisfy acceptance before drive'
terminal_write_calls="$TMP/terminal-write-failure-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$terminal_write_calls" OMS_GOAL_DRIVE_TEST_FAIL_TERMINAL_APPEND=1 \
  "$ROOT/scripts/goal-drive.sh" \
  --repo "$terminal_write_repo" --to codex --max-cycles 1 \
  --run-id terminal-write-failure > "$TMP/terminal-write-failure.out" 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "terminal append failure should fail closed, got $rc"
[ ! -s "$terminal_write_calls" ] ||
  fail "terminal append failure fixture unexpectedly called the provider"
if grep -Fq 'goal-drive: done' "$TMP/terminal-write-failure.out"; then
  fail "terminal append failure was reported as a successful drive"
fi

# Freezing reviewed bytes is a private repository write. A planted
# commit-patches symlink must park before chmod or file creation can mutate its
# external directory target. Windows Git Bash can lack symlink privilege, so
# only that unavailable primitive skips this fixture.
patch_link_repo="$TMP/commit-patches-symlink"
make_case "$patch_link_repo" tracked
patch_link_calls="$TMP/commit-patches-symlink-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$patch_link_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$patch_link_repo" --to codex \
  --max-cycles 2 > "$TMP/commit-patches-symlink-review.out" 2>&1 || rc=$?
[ "$rc" = 75 ] ||
  fail "commit-patches symlink fixture did not stop after review"
[ "$(wc -l < "$patch_link_calls" | tr -d ' ')" = 1 ] ||
  fail "commit-patches symlink review made a duplicate provider call"
patch_link_outside="$TMP/commit-patches-external"
patch_link_sentinel="$patch_link_outside/user-owned.txt"
patch_link_expected="$TMP/commit-patches-external.expected"
mkdir -p "$patch_link_outside"
printf 'user-owned external bytes\n' > "$patch_link_sentinel"
cp "$patch_link_sentinel" "$patch_link_expected"
chmod 751 "$patch_link_outside"
patch_link_mode="$(stat -c '%a' "$patch_link_outside" 2>/dev/null ||
  stat -f '%Lp' "$patch_link_outside" 2>/dev/null)"
[ -n "$patch_link_mode" ] || fail "could not read external fixture mode"
[ ! -e "$patch_link_repo/.oms/plan/commit-patches" ] &&
  [ ! -L "$patch_link_repo/.oms/plan/commit-patches" ] ||
  fail "commit-patches symlink fixture path already exists"
if ln -s "$patch_link_outside" \
  "$patch_link_repo/.oms/plan/commit-patches" 2>/dev/null; then
  rc=0
  HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
    CALL_LOG="$patch_link_calls" "$ROOT/scripts/goal-drive.sh" \
    --repo "$patch_link_repo" --to codex --max-cycles 2 \
    > "$TMP/commit-patches-symlink.out" 2>&1 || rc=$?
  [ "$rc" = 3 ] ||
    fail "commit-patches symlink should park, got $rc"
  grep -Fq 'reason=unsafe-patch-path' "$TMP/commit-patches-symlink.out" ||
    fail "commit-patches symlink did not report unsafe-patch-path"
  [ "$(wc -l < "$patch_link_calls" | tr -d ' ')" = 1 ] ||
    fail "commit-patches symlink resume called the provider again"
  [ "$patch_link_mode" = \
    "$(stat -c '%a' "$patch_link_outside" 2>/dev/null ||
      stat -f '%Lp' "$patch_link_outside" 2>/dev/null)" ] ||
    fail "unsafe commit-patches path changed the external directory mode"
  cmp -s "$patch_link_expected" "$patch_link_sentinel" ||
    fail "unsafe commit-patches path changed external bytes"
  [ "$(find "$patch_link_outside" ! -path "$patch_link_outside" -print |
    wc -l | tr -d ' ')" = 1 ] ||
    fail "unsafe commit-patches path created external entries"
else
  echo "goal-drive-recovery-smoke: skip commit-patches symlink fixture (symlinks unavailable)" >&2
fi

# A planted final patch leaf may itself point at an external directory. Plain
# `mv temp leaf` follows that directory symlink and creates an external entry;
# the freeze transaction must atomically replace the leaf instead.
patch_leaf_repo="$TMP/commit-patch-leaf-symlink"
make_case "$patch_leaf_repo" tracked
patch_leaf_calls="$TMP/commit-patch-leaf-symlink-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$patch_leaf_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$patch_leaf_repo" --to codex \
  --max-cycles 2 > "$TMP/commit-patch-leaf-review.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "commit-patch leaf fixture did not stop after review"
patch_leaf_dir="$patch_leaf_repo/.oms/plan/commit-patches"
patch_leaf_path="$patch_leaf_dir/leaf-symlink-1-tracked.patch"
patch_leaf_outside="$TMP/commit-patch-leaf-external"
patch_leaf_sentinel="$patch_leaf_outside/user-owned.txt"
patch_leaf_expected="$TMP/commit-patch-leaf-external.expected"
patch_leaf_head_before="$(git -C "$patch_leaf_repo" rev-parse HEAD)"
mkdir -p "$patch_leaf_dir" "$patch_leaf_outside"
printf 'leaf-external user bytes\n' > "$patch_leaf_sentinel"
cp "$patch_leaf_sentinel" "$patch_leaf_expected"
chmod 751 "$patch_leaf_outside"
patch_leaf_mode="$(stat -c '%a' "$patch_leaf_outside" 2>/dev/null ||
  stat -f '%Lp' "$patch_leaf_outside" 2>/dev/null)"
if ln -s "$patch_leaf_outside" "$patch_leaf_path" 2>/dev/null &&
   [ -L "$patch_leaf_path" ]; then
  rc=0
  HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
    CALL_LOG="$patch_leaf_calls" "$ROOT/scripts/goal-drive.sh" \
    --repo "$patch_leaf_repo" --to codex --max-cycles 2 \
    --run-id leaf-symlink > "$TMP/commit-patch-leaf.out" 2>&1 || rc=$?
  case "$rc" in
    0)
      [ -f "$patch_leaf_path" ] && [ ! -L "$patch_leaf_path" ] ||
        fail "final patch leaf was not replaced by the frozen regular file"
      [ "$(git -C "$patch_leaf_repo" rev-list --count \
        "$patch_leaf_head_before..HEAD" | tr -d '\r')" = 1 ] ||
        fail "final patch leaf recovery did not publish exactly one commit"
      ;;
    3)
      grep -Fq 'reason=freeze-patch-failed' "$TMP/commit-patch-leaf.out" ||
        fail "platform-safe leaf refusal used the wrong park reason"
      [ "$(git -C "$patch_leaf_repo" rev-parse HEAD)" = "$patch_leaf_head_before" ] ||
        fail "platform-safe leaf refusal moved HEAD"
      ;;
    *) fail "final patch leaf symlink did not fail safely, got $rc" ;;
  esac
  [ -z "$(git -C "$patch_leaf_repo" status --porcelain)" ] ||
    fail "final patch leaf handling changed the worktree"
  [ "$(wc -l < "$patch_leaf_calls" | tr -d ' ')" = 1 ] ||
    fail "commit-patch leaf recovery called the provider again"
  [ "$patch_leaf_mode" = \
    "$(stat -c '%a' "$patch_leaf_outside" 2>/dev/null ||
      stat -f '%Lp' "$patch_leaf_outside" 2>/dev/null)" ] ||
    fail "final patch leaf symlink changed the external directory mode"
  cmp -s "$patch_leaf_expected" "$patch_leaf_sentinel" ||
    fail "final patch leaf symlink changed external bytes"
  [ "$(find "$patch_leaf_outside" ! -path "$patch_leaf_outside" -print |
    wc -l | tr -d ' ')" = 1 ] ||
    fail "final patch leaf symlink created an external entry"
else
  echo "goal-drive-recovery-smoke: skip final patch leaf symlink fixture (symlinks unavailable)" >&2
fi

# Race the directory after goal-drive has entered the validated directory but
# before mktemp creates the patch. Relative writes must remain on the original
# directory handle; the replacement symlink target stays untouched and the
# changed absolute lookup parks before the frozen path is consumed.
patch_race_repo="$TMP/commit-patches-race"
make_case "$patch_race_repo" tracked
patch_race_calls="$TMP/commit-patches-race-calls"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$patch_race_calls" OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW=1 \
  "$ROOT/scripts/goal-drive.sh" --repo "$patch_race_repo" --to codex \
  --max-cycles 2 > "$TMP/commit-patches-race-review.out" 2>&1 || rc=$?
[ "$rc" = 75 ] || fail "commit-patches race fixture did not stop after review"
patch_race_dir="$patch_race_repo/.oms/plan/commit-patches"
patch_race_parked="$patch_race_repo/.oms/plan/commit-patches.original"
patch_race_outside="$TMP/commit-patches-race-external"
patch_race_sentinel="$patch_race_outside/user-owned.txt"
patch_race_expected="$TMP/commit-patches-race-external.expected"
patch_race_marker="$TMP/commit-patches-race-swapped"
patch_race_bin="$TMP/commit-patches-race-bin"
real_mktemp="$(command -v mktemp)"
mkdir -p "$patch_race_outside" "$patch_race_bin"
printf 'race-external user bytes\n' > "$patch_race_sentinel"
cp "$patch_race_sentinel" "$patch_race_expected"
chmod 751 "$patch_race_outside"
patch_race_mode="$(stat -c '%a' "$patch_race_outside" 2>/dev/null ||
  stat -f '%Lp' "$patch_race_outside" 2>/dev/null)"
cat > "$patch_race_bin/mktemp" <<'EOF'
#!/usr/bin/env bash
case " $* " in
  *' .goal-patch.XXXXXX '*)
    if mv "$OMS_T_PATCH_DIR" "$OMS_T_PATCH_PARKED" 2>/dev/null &&
       ln -s "$OMS_T_PATCH_OUTSIDE" "$OMS_T_PATCH_DIR" 2>/dev/null; then
      : > "$OMS_T_PATCH_SWAPPED"
    fi
    ;;
esac
exec "$OMS_T_REAL_MKTEMP" "$@"
EOF
chmod +x "$patch_race_bin/mktemp"
rc=0
HOME="$home" NVM_DIR="$home/.nvm" PATH="$patch_race_bin:$bin:/usr/bin:/bin" \
  CALL_LOG="$patch_race_calls" OMS_T_REAL_MKTEMP="$real_mktemp" \
  OMS_T_PATCH_DIR="$patch_race_dir" OMS_T_PATCH_PARKED="$patch_race_parked" \
  OMS_T_PATCH_OUTSIDE="$patch_race_outside" OMS_T_PATCH_SWAPPED="$patch_race_marker" \
  "$ROOT/scripts/goal-drive.sh" --repo "$patch_race_repo" --to codex \
  --max-cycles 2 > "$TMP/commit-patches-race.out" 2>&1 || rc=$?
if [ -f "$patch_race_marker" ]; then
  [ "$rc" = 3 ] || fail "commit-patches path race should park, got $rc"
  grep -Fq 'reason=unsafe-patch-path' "$TMP/commit-patches-race.out" ||
    fail "commit-patches path race did not report unsafe-patch-path"
  [ "$(wc -l < "$patch_race_calls" | tr -d ' ')" = 1 ] ||
    fail "commit-patches path race called the provider again"
  [ "$patch_race_mode" = \
    "$(stat -c '%a' "$patch_race_outside" 2>/dev/null ||
      stat -f '%Lp' "$patch_race_outside" 2>/dev/null)" ] ||
    fail "commit-patches path race changed the external directory mode"
  cmp -s "$patch_race_expected" "$patch_race_sentinel" ||
    fail "commit-patches path race changed external bytes"
  [ "$(find "$patch_race_outside" ! -path "$patch_race_outside" -print |
    wc -l | tr -d ' ')" = 1 ] ||
    fail "commit-patches path race created an external entry"
else
  echo "goal-drive-recovery-smoke: skip commit-patches race fixture (directory swap unavailable)" >&2
fi

# A real rename patch names both the removed and added path. Git's default
# rename detection may report only the destination from `diff --name-only`;
# exact intent matching must normalize the actual tree to old+new instead of
# parking a valid autonomous rename.
rename_repo="$TMP/rename-intent"
mkdir -p "$rename_repo/scripts"
git -C "$rename_repo" init -q -b main
git -C "$rename_repo" config user.email test@example.com
git -C "$rename_repo" config user.name Test
printf 'rename-base\n' > "$rename_repo/old.txt"
cat > "$rename_repo/scripts/check.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ ! -e old.txt ]
grep -Fxq rename-base new.txt
EOF
chmod +x "$rename_repo/scripts/check.sh"
git -C "$rename_repo" add old.txt scripts/check.sh
git -C "$rename_repo" commit -qm base
"$ROOT/scripts/agent-plan.sh" --repo "$rename_repo" init --goal rename-intent \
  --accept 'bash scripts/check.sh' >/dev/null
"$ROOT/scripts/agent-plan.sh" --repo "$rename_repo" add --id rename-intent \
  --title 'fix: publish exact rename intent' --allowed 'old.txt,new.txt' \
  --verify 'bash scripts/check.sh' >/dev/null
cat > "$bin/codex" <<'EOF'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' "${OMS_TASK_ID:-missing}" >> "$CALL_LOG"
git mv old.txt new.txt
echo worker-ok
EOF
chmod +x "$bin/codex"
rename_before="$(git -C "$rename_repo" rev-parse HEAD)"
rename_calls="$TMP/rename-intent-calls"
HOME="$home" NVM_DIR="$home/.nvm" PATH="$bin:/usr/bin:/bin" \
  CALL_LOG="$rename_calls" "$ROOT/scripts/goal-drive.sh" --repo "$rename_repo" \
  --to codex --max-cycles 2 --run-id exact-run-receipt \
  > "$TMP/rename-intent.out" 2>&1 ||
  fail "valid autonomous rename intent did not commit: $(tail -8 "$TMP/rename-intent.out")"
[ "$(wc -l < "$rename_calls" | tr -d ' ')" = 1 ] ||
  fail "rename intent called the provider more than once"
[ "$(git -C "$rename_repo" rev-list --count "$rename_before"..HEAD)" = 1 ] ||
  fail "rename intent did not publish exactly one commit"
rename_status="$(git -C "$rename_repo" diff-tree --no-commit-id --name-status -r -M \
  "$rename_before" HEAD | tr -d '\r')"
printf '%s\n' "$rename_status" | grep -Eq '^R[0-9]+[[:space:]]+old\.txt[[:space:]]+new\.txt$' ||
  fail "rename intent commit did not preserve old/new rename lineage"
[ -z "$(git -C "$rename_repo" status --porcelain)" ] ||
  fail "rename intent left the repository dirty"
python3 - "$rename_repo/.oms/plan/progress.jsonl" <<'PY' || fail "caller-supplied run id did not bind the terminal receipt"
import json, sys

rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
terminal = [row for row in rows if row.get("kind") == "terminal"]
assert terminal and terminal[-1].get("run_id") == "exact-run-receipt", terminal
PY

echo "goal-drive-recovery-smoke: ok"
