#!/usr/bin/env bash
set -euo pipefail

# `oms tick` against fixture repos under TMP with an XDG home of its own and a
# systemctl stub, so nothing here reaches the real user manager, crontab,
# journal config, or Codex. Covers the registry, the sweep receipt, idle
# thread/task/plan thresholds, gc staying off by default, and install/uninstall ownership.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tick.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TICK="$ROOT/scripts/tick.sh"
PLAN="$ROOT/scripts/agent-plan.sh"
export XDG_CONFIG_HOME="$TMP/xdg" XDG_RUNTIME_DIR="$TMP/rt" XDG_STATE_HOME="$TMP/state" OMS_CAPABILITY_DIR="$TMP/capabilities" OMS_INSTALL_RECEIPT="$TMP/no-receipt.json" \
  OMS_WORK_JOURNAL_CONFIG="$TMP/no-journal.json" OMS_TICK_THREAD_IDLE_DAYS=7 OMS_HARNESS_CHILD=0
# A fresh refresh stamp keeps tick from listing real provider catalogs.
mkdir -p "$TMP/state/oh-my-setting" && touch "$TMP/state/oh-my-setting/catalog-refresh.stamp"
mkdir -p "$XDG_RUNTIME_DIR" "$TMP/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/ntn"; chmod +x "$TMP/bin/ntn"
cat > "$TMP/bin/systemctl" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/systemctl.log"
[ "\$2" != show-environment ] || exit "\${OMS_TEST_SYSTEMD_AVAILABLE:-0}"
[ "\$2" != show ] || printf '%s\n' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer"
EOF
chmod +x "$TMP/bin/systemctl"
cat > "$TMP/bin/crontab" <<'EOF'
#!/usr/bin/env bash
[ "$1" = -l ] && exit 0
echo 'unexpected crontab mutation' >&2
exit 1
EOF
chmod +x "$TMP/bin/crontab"
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $*" >&2; exit 1; }
make_repo() {
  mkdir -p "$1/.oms"; git -C "$1" init -q; printf '*\n' > "$1/.oms/.gitignore"
}
make_committed_repo() {
  make_repo "$1"
  touch "$1/fixture"
  git -C "$1" add fixture
  git -C "$1" -c user.email=test@example.com -c user.name='Test User' commit -qm fixture
}
set_task_activity() {
  python3 - "$1/.oms/task/current.md" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
path.write_text(re.sub(
    r"^- last_activity:.*$", "- last_activity: 2020-01-01T00:00:00Z",
    path.read_text(), flags=re.MULTILINE
))
PY
}
make_stale_plan() {  # REPO STATE
  "$PLAN" --repo "$1" init --goal retired-plan --accept false >/dev/null
  "$PLAN" --repo "$1" add --id t1 --title retired-plan >/dev/null
  python3 - "$1/.oms/plan/tasks.json" "$2" <<'PY'
import json
import sys

path, state = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    plan = json.load(handle)
task = plan["tasks"]["t1"]
task["state"] = state
task["updated"] = "2020-01-01T00:00:00Z"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(plan, handle, ensure_ascii=False, indent=2)
PY
}
set_plan_activity() {
  python3 - "$1/.oms/plan/tasks.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    plan = json.load(handle)
plan["tasks"]["t1"]["updated"] = "2020-01-01T00:00:00Z"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(plan, handle, ensure_ascii=False, indent=2)
PY
}
make_recovered_artifact_index() {
  mkdir -p "$1/.oms/artifacts"
  cat > "$1/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_wall_death","operation_id":"op_1","artifact_id":"sha256:a1","ts":"2026-08-12T01:01:00Z","kind":"ask","provider":"codex","exit":124}
{"schema":1,"event_id":"evt_seat_back","operation_id":"op_2","artifact_id":"sha256:a2","ts":"2026-08-12T01:02:00Z","kind":"ask","provider":"codex","exit":0}
EOF
}
make_superseded_artifacts_and_stale_failures() {
  mkdir -p "$1/.oms/artifacts"
  cat > "$1/.oms/artifacts/index.jsonl" <<'EOF'
{"schema":1,"event_id":"evt_superseded_failure","operation_id":"op_superseded","artifact_id":"sha256:s1","ts":"2026-08-12T01:01:00Z","kind":"patch-admit","provider":"codex","exit":1,"patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
{"schema":1,"event_id":"evt_superseding_success","operation_id":"op_success","artifact_id":"sha256:s2","ts":"2026-08-12T01:02:00Z","kind":"patch-admit","provider":"codex","exit":0,"patch_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
EOF
  python3 - "$1/.oms/failures.jsonl" <<'PY'
import datetime
import json
import sys

path = sys.argv[1]
now = datetime.datetime.now(datetime.timezone.utc)
foreign_state = "0" * 40 + ":" + "a" * 64 + ":" + "b" * 64

def failure(fingerprint, age_days, kind="cmd", state=foreign_state):
    return {
        "schema": 2,
        "event": "fail",
        "ts": (now - datetime.timedelta(days=age_days)).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "agent": "codex",
        "fingerprint": fingerprint,
        "kind": kind,
        "cmd": "fixture " + fingerprint,
        "exit": 1,
        "state_fingerprint": state,
    }

rows = [
    failure("1111111111111111", 15),
    failure("2222222222222222", 1),
    failure("3333333333333333", 16, state="1" * 40 + ":" + "a" * 64 + ":" + "b" * 64),
    failure("3333333333333333", 15),
    failure("5555555555555555", 15),
    failure("5555555555555555", 15),
    failure("4444444444444444", 0, "hook"),
]
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    for row in rows:
        handle.write(json.dumps(row) + "\n")
PY
}

# --- shared scheduler detection, without host writes --------------------------
(
  # shellcheck source=scripts/lib/install-contract.sh
  . "$ROOT/scripts/lib/install-contract.sh"
  [ "$(oms_install_scheduler_method auto)" = systemd ] || fail "a usable user manager must win"
  [ "$(OMS_TEST_SYSTEMD_AVAILABLE=1 oms_install_scheduler_method auto)" = cron ] ||
    fail "an unreachable user manager must fall back to cron"
  [ "$(XDG_RUNTIME_DIR='' oms_install_scheduler_method auto)" = cron ] ||
    fail "a missing user runtime must fall back to cron"
  [ "$(PATH=/nonexistent oms_install_scheduler_method auto)" = none ] ||
    fail "missing schedulers must be reported"
  [ "$(PATH=/nonexistent oms_install_scheduler_method auto "$TMP/cron-fixture")" = cron ] ||
    fail "the auto-update cron fixture must remain supported"
  [ "$(PATH=/nonexistent oms_install_scheduler_method systemd)" = systemd ] ||
    fail "explicit method selection must remain caller-controlled"
  if oms_install_scheduler_method invalid >/dev/null 2>&1; then
    fail "invalid scheduler method must fail"
  else
    [ "$?" -eq 2 ] || fail "invalid scheduler method must return usage error"
  fi
)
for scheduler in tick install-autoupdate; do
  scheduler_args=(--method auto --dry-run)
  [ "$scheduler" != tick ] || scheduler_args=(install "${scheduler_args[@]}")
  out="$("$ROOT/scripts/$scheduler.sh" "${scheduler_args[@]}")"
  printf '%s' "$out" | grep >/dev/null 'would install systemd user timer' ||
    fail "$scheduler must select the shared usable user manager: $out"
  out="$(OMS_TEST_SYSTEMD_AVAILABLE=1 "$ROOT/scripts/$scheduler.sh" "${scheduler_args[@]}")"
  printf '%s' "$out" | grep >/dev/null 'would install cron' ||
    fail "$scheduler must select the shared cron fallback: $out"
done
[ ! -e "$XDG_CONFIG_HOME/systemd" ] || fail "scheduler dry-run must not write units"
[ ! -e "$OMS_INSTALL_RECEIPT" ] || fail "scheduler dry-run must not write a receipt"

# --- registry -----------------------------------------------------------------
a="$TMP/a"; b="$TMP/b"; make_repo "$a"; make_repo "$b"
"$TICK" register --repo "$a" | grep '^registered' > /dev/null || fail "register must add a repo"
"$TICK" register --repo "$a" | grep 'already registered' > /dev/null || fail "register must dedupe"
out="$("$TICK" register --repo "$TMP" 2>&1 || true)"
printf '%s' "$out" | grep >/dev/null 'no .oms' || fail "an unadopted dir must be refused: $out"
[ "$(wc -l < "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt")" -eq 1 ] || fail "registry must hold one line"

# The receipt stays in the purity inventory: defer the entire sweep instead.
# Multiple checks can register, and only the last live check permits a sweep.
maintenance="$TMP/check-coordination"; make_repo "$maintenance"
for backend in 0 1; do
  (
    export OMS_LOCK_FORCE_MKDIR="$backend" OMS_LOCK_DIR="$TMP/coordination-$backend"
    . "$ROOT/scripts/lib/check-maintenance.sh"
    oms_check_maintenance_begin "$maintenance"
    first_marker="$OMS_CHECK_MAINTENANCE_MARKER"
    first_owner="$OMS_CHECK_MAINTENANCE_OWNER"
    oms_check_maintenance_begin "$maintenance"
    out="$("$TICK" run --repo "$maintenance")"
    printf '%s' "$out" | grep >/dev/null 'verification running' || fail "tick ignored active checks: $out"
    [ ! -e "$maintenance/.oms/tick" ] || fail "deferred sweep wrote a receipt"
    oms_check_maintenance_end
    out="$("$TICK" run --repo "$maintenance")"
    printf '%s' "$out" | grep >/dev/null 'verification running' || fail "one check released another"
    OMS_CHECK_MAINTENANCE_MARKER="$first_marker"
    OMS_CHECK_MAINTENANCE_OWNER="$first_owner"
    oms_check_maintenance_end
    # A finished/crashed process cannot suppress future maintenance.
    (oms_check_maintenance_begin "$maintenance")
    oms_hold_file_lock "$maintenance/.oms/tick/last.json" 8
    trap oms_release_held_file_lock EXIT
    oms_check_maintenance_active "$maintenance" && fail "dead check remained active"
    out="$("$TICK" run --repo "$maintenance")"
    printf '%s' "$out" | grep >/dev/null 'maintenance already running' || fail "overlapping sweep was allowed"
  )
done
out="$("$TICK" run --repo "$maintenance")"
printf '%s' "$out" | grep >/dev/null 'swept ' || fail "maintenance did not resume: $out"

# --- sweep: idle thread/task closed, fresh/live task kept, gc off, receipt ----
THREAD="$ROOT/scripts/thread.sh"
TASK="$ROOT/scripts/agent-task.sh"
"$THREAD" new --repo "$a" --id old-thread --topic old >/dev/null
"$THREAD" append --repo "$a" --id old-thread --role note --text hello >/dev/null
"$THREAD" new --repo "$a" --id old-thread-two --topic old >/dev/null
"$THREAD" new --repo "$a" --id new-thread --topic new >/dev/null
touch -d '10 days ago' "$a/.oms/threads/old-thread.jsonl"
touch -d '10 days ago' "$a/.oms/threads/old-thread-two.jsonl"
touch -d '10 days ago' "$a/.oms/threads/new-thread.jsonl"
"$TASK" --repo "$a" init >/dev/null
set_task_activity "$a"
out="$(bash -x "$TICK" run --repo "$a" 2>"$TMP/tick.trace")"
printf '%s' "$out" | grep >/dev/null 'threads_closed=2' || fail "idle non-current threads must be closed: $out"
[ "$(grep -c '^++ bounded .* thread list ' "$TMP/tick.trace")" -eq 1 ] ||
  fail "the sweep must query the thread catalog once"
grep -Fq 'work_journal_sync' "$TMP/tick.trace" || fail "tick must own journal sync"
grep -Fq -- '--force' "$TMP/tick.trace" || fail "tick must include current summaries"
printf '%s' "$out" | grep >/dev/null 'tasks_closed=1' || fail "the idle goal-less task must be closed: $out"
"$THREAD" list --repo "$a" | grep 'new-thread' > /dev/null || fail "a fresh thread must stay open"
if "$THREAD" list --repo "$a" | grep 'old-thread' > /dev/null; then fail "the idle thread must not stay open"; fi
[ ! -e "$a/.oms/task/current.md" ] || fail "the idle goal-less task must not stay active"
archive="$(find "$a/.oms/task/archive" -type f -name '*.md' -print | sed -n '1p')"
[ -n "$archive" ] && grep -Fq 'closed by oms tick' "$archive" ||
  fail "the archived task must retain the tick close reason"
python3 - "$a/.oms/tick/last.json" <<'PY' || fail "sweep receipt is wrong: $(cat "$a/.oms/tick/last.json")"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["gc"] == "skipped" and set(r["threads_closed"]) == {"old-thread", "old-thread-two"}, r
assert r["tasks_closed"] == 1, r
assert r["artifacts_resolved"] == 0 and r["artifact_resolve_rc"] == 0, r
assert r["artifacts_superseded"] == 0 and r["artifact_supersede_rc"] == 0, r
assert r["failures_retired"] == 0, r
assert isinstance(r["journal_rc"], int) and isinstance(r["reconcile_rc"], int), r
PY
"$TASK" --repo "$a" init >/dev/null
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep >/dev/null 'tasks_closed=0' || fail "a fresh goal-less task must stay active: $out"
[ -f "$a/.oms/task/current.md" ] || fail "a fresh goal-less task must stay active"
"$TASK" --repo "$a" update --goal 'live task must not be retired' >/dev/null
set_task_activity "$a"
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep >/dev/null 'tasks_closed=0' || fail "a stale task with a goal must stay active: $out"
[ -f "$a/.oms/task/current.md" ] || fail "a stale task with a goal must stay active"
c="$TMP/nonactive-task"; make_repo "$c"
"$TASK" --repo "$c" init --verify true >/dev/null
"$TASK" --repo "$c" verify >/dev/null
set_task_activity "$c"
out="$("$TICK" run --repo "$c")"
printf '%s' "$out" | grep >/dev/null 'tasks_closed=0' || fail "a non-active task must stay active: $out"
[ -f "$c/.oms/task/current.md" ] || fail "a non-active task must stay active"

# --- sweep: idle all-done plans retire; undone, fresh, or claimed plans stay --
p="$TMP/idle-plan"; make_committed_repo "$p"; make_stale_plan "$p" 'done'
out="$("$TICK" run --repo "$p")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=1' || fail "the idle all-done plan must retire: $out"
[ ! -e "$p/.oms/plan/tasks.json" ] || fail "the retired plan must not stay active"
python3 - "$p/.oms/tick/last.json" "$p/.oms/plan/retirements.jsonl" "$p/.oms/work-journal/events.jsonl" <<'PY' || fail "plan retirement receipts are wrong"
import json
import sys

tick = json.load(open(sys.argv[1], encoding="utf-8"))
retirement = json.loads(open(sys.argv[2], encoding="utf-8").readline())
journal = [json.loads(line) for line in open(sys.argv[3], encoding="utf-8")]
assert tick["plans_retired"] == 1 and tick["plan_retire_rc"] == 0, tick
assert retirement["kind"] == "plan-retirement", retirement
assert retirement["disposition"] == "superseded", retirement
assert retirement["acceptance_verified"] is False, retirement
assert any(row["source"]["type"] == "oms-run" and
           row["source"]["id"].startswith("plan-retire:") and
           row["verification_status"] == "not_verified" for row in journal), journal
PY
r="$TMP/ready-plan"; make_committed_repo "$r"; make_stale_plan "$r" ready
out="$("$TICK" run --repo "$r")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "an idle plan with undone tasks is a parent decision, not a sweep: $out"
[ -e "$r/.oms/plan/tasks.json" ] || fail "an undone plan must not be retired"
f="$TMP/fresh-plan"; make_committed_repo "$f"; make_stale_plan "$f" 'done'
python3 - "$f/.oms/plan/tasks.json" <<'PY'
import datetime, json, sys
p = sys.argv[1]; d = json.load(open(p, encoding="utf-8"))
d["tasks"]["t1"]["updated"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
json.dump(d, open(p, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
PY
out="$("$TICK" run --repo "$f")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "a plan finished inside the idle window must stay: $out"
[ -e "$f/.oms/plan/tasks.json" ] || fail "a fresh plan must not be retired"
q="$TMP/claimed-plan"; make_committed_repo "$q"; make_stale_plan "$q" ready
"$PLAN" --repo "$q" claim --id t1 --provider codex >/dev/null
set_plan_activity "$q"
out="$("$TICK" run --repo "$q")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "a claimed plan must stay active: $out"
[ -f "$q/.oms/plan/tasks.json" ] || fail "a claimed plan must stay active"
e="$TMP/empty-plan"; make_committed_repo "$e"
"$PLAN" --repo "$e" init --goal empty-plan --accept false >/dev/null
out="$("$TICK" run --repo "$e")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "a plan without tasks must stay active: $out"
[ -f "$e/.oms/plan/tasks.json" ] || fail "a plan without tasks must stay active"
i="$TMP/invalid-plan"; make_repo "$i"; mkdir -p "$i/.oms/plan"
printf '{not json\n' > "$i/.oms/plan/tasks.json"
out="$("$TICK" run --repo "$i")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "an invalid plan must stay active: $out"
[ -f "$i/.oms/plan/tasks.json" ] || fail "an invalid plan must stay active"
u="$TMP/uncommitted-plan"; make_repo "$u"; make_stale_plan "$u" 'done'
out="$("$TICK" run --repo "$u")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "a failed plan retirement must not abort a sweep: $out"
[ -f "$u/.oms/plan/tasks.json" ] || fail "a failed plan retirement must keep the active plan"
python3 - "$u/.oms/tick/last.json" <<'PY' || fail "a failed plan retirement must record its exit"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
assert isinstance(receipt["plan_retire_rc"], int) and receipt["plan_retire_rc"] != 0, receipt
PY

# --- sweep: mechanically recovered artifact failures resolve every pass -----
v="$TMP/recovered-artifacts"; make_repo "$v"; make_recovered_artifact_index "$v"
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$v")"
printf '%s' "$out" | grep >/dev/null 'artifacts_resolved=1' ||
  fail "a recovered artifact failure must resolve even with retirement off: $out"
python3 - "$v/.oms/tick/last.json" "$v/.oms/artifacts/index.jsonl" <<'PY' || fail "artifact recovery sweep evidence is wrong"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
rows = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
resolutions = [row for row in rows if row.get("kind") == "artifact-resolution"]
assert receipt["artifacts_resolved"] == 1 and receipt["artifact_resolve_rc"] == 0, receipt
assert len(resolutions) == 1 and resolutions[0]["resolves_event_id"] == "evt_wall_death", resolutions
PY
out="$("$TICK" run --repo "$v")"
printf '%s' "$out" | grep >/dev/null 'artifacts_resolved=0' ||
  fail "a second artifact recovery sweep must be idempotent: $out"
w="$TMP/invalid-artifacts"; make_repo "$w"; mkdir -p "$w/.oms/artifacts"
printf '{not json\n' > "$w/.oms/artifacts/index.jsonl"
out="$("$TICK" run --repo "$w")"
printf '%s' "$out" | grep >/dev/null 'artifacts_resolved=0' ||
  fail "a failed artifact resolver must not abort a sweep: $out"
python3 - "$w/.oms/tick/last.json" <<'PY' || fail "a failed artifact resolver must record its exit"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
assert isinstance(receipt["artifact_resolve_rc"], int) and receipt["artifact_resolve_rc"] != 0, receipt
assert isinstance(receipt["artifact_supersede_rc"], int) and receipt["artifact_supersede_rc"] != 0, receipt
PY

# --- sweep: one default sweep settles both a supersession and a stale row ----
y="$TMP/both-at-once"; make_committed_repo "$y"; make_superseded_artifacts_and_stale_failures "$y"
out="$("$TICK" run --repo "$y")"
printf '%s' "$out" | grep >/dev/null 'artifacts_superseded=1 failures_retired=2' ||
  fail "one sweep must report both counters at once: $out"
grep -q 'retired by oms tick' "$y/.oms/failures.jsonl" || fail "the retirement must carry the tick provenance"

# --- sweep: repeated failures outside git have no commit to age against -----
ng="$TMP/non-git-failures"; mkdir -p "$ng/.oms"
python3 - "$ng/.oms/failures.jsonl" <<'PY'
import datetime, json, sys
ts = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    for _ in range(2):
        handle.write(json.dumps({"schema": 2, "event": "fail", "ts": ts, "agent": "codex",
                                 "fingerprint": "6666666666666666", "kind": "cmd", "cmd": "fixture",
                                 "exit": 1, "state_fingerprint": "non-git"}) + "\n")
PY
out="$("$TICK" run --repo "$ng")"
printf '%s' "$out" | grep >/dev/null 'failures_retired=0' ||
  fail "repeated non-git failures must not be retired as stale: $out"
"$ROOT/scripts/fail-ledger.sh" --repo "$ng" show --fingerprint 6666666666666666 --json |
  grep >/dev/null '"attention": "actionable"' || fail "repeated non-git failures must stay actionable"

# --- sweep: superseded artifacts and stale one-shot failures settle safely --
x="$TMP/superseded-and-stale"; make_committed_repo "$x"; make_superseded_artifacts_and_stale_failures "$x"
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$x")"
printf '%s' "$out" | grep >/dev/null 'artifacts_superseded=1' ||
  fail "a superseded artifact failure must resolve with retirement off: $out"
printf '%s' "$out" | grep >/dev/null 'failures_retired=0' ||
  fail "retirement opt-out must leave stale failure rows alone: $out"
python3 - "$x/.oms/tick/last.json" "$x/.oms/failures.jsonl" <<'PY' || fail "retirement opt-out receipt is wrong"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
ledger = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert receipt["artifacts_superseded"] == 1 and receipt["artifact_supersede_rc"] == 0, receipt
assert receipt["failures_retired"] == 0, receipt
assert not any(row.get("event") == "resolved" and row.get("fingerprint") == "1111111111111111"
               for row in ledger), ledger
PY
out="$("$TICK" run --repo "$x")"
printf '%s' "$out" | grep >/dev/null 'artifacts_superseded=0' ||
  fail "an already-resolved superseded artifact must be idempotent: $out"
printf '%s' "$out" | grep >/dev/null 'failures_retired=2' ||
  fail "one old stale one-shot failure must retire: $out"
"$ROOT/scripts/fail-ledger.sh" --repo "$x" list --unresolved --json > "$TMP/superseded-unresolved.json"
python3 - "$x/.oms/tick/last.json" "$x/.oms/artifacts/index.jsonl" "$x/.oms/failures.jsonl" "$TMP/superseded-unresolved.json" <<'PY' || fail "superseded and stale sweep evidence is wrong"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
artifacts = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
ledger = [json.loads(line) for line in open(sys.argv[3], encoding="utf-8") if line.strip()]
unresolved = {row["fingerprint"]: row for row in json.load(open(sys.argv[4], encoding="utf-8"))["failures"]}
assert receipt["artifacts_superseded"] == 0 and receipt["artifact_supersede_rc"] == 0, receipt
assert receipt["failures_retired"] == 2, receipt
assert len([row for row in artifacts if row.get("kind") == "artifact-resolution" and
            row.get("resolves_event_id") == "evt_superseded_failure"]) == 1, artifacts
assert any(row.get("event") == "resolved" and row.get("fingerprint") == "1111111111111111"
           for row in ledger), ledger
assert set(unresolved) == {"2222222222222222", "3333333333333333", "4444444444444444"}, unresolved
assert unresolved["2222222222222222"]["attention"] == "stale", unresolved
assert unresolved["3333333333333333"]["count"] == 2 and unresolved["3333333333333333"]["attention"] == "actionable", unresolved
assert unresolved["4444444444444444"]["attention"] == "retiring", unresolved
assert any(row.get("event") == "resolved" and row.get("fingerprint") == "5555555555555555"
           for row in ledger), "two retries on one old commit must retire as stale"
PY
out="$("$TICK" run --repo "$x")"
printf '%s' "$out" | grep >/dev/null 'artifacts_superseded=0' ||
  fail "a second superseded sweep must remain idempotent: $out"
printf '%s' "$out" | grep >/dev/null 'failures_retired=0' ||
  fail "a second stale-failure sweep must remain idempotent: $out"
o="$TMP/retire-off"; make_committed_repo "$o"; make_stale_plan "$o" 'done'
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$o")"
printf '%s' "$out" | grep >/dev/null 'plans_retired=0' || fail "retirement opt-out must keep plans: $out"
[ -f "$o/.oms/plan/tasks.json" ] || fail "retirement opt-out must keep plans"
r="$TMP/retire-off-task"; make_repo "$r"
"$TASK" --repo "$r" init >/dev/null
set_task_activity "$r"
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$r")"
printf '%s' "$out" | grep >/dev/null 'tasks_closed=0' || fail "retirement opt-out must keep tasks: $out"
[ -f "$r/.oms/task/current.md" ] || fail "retirement opt-out must keep tasks"
"$TICK" run | grep "swept $a" > /dev/null || fail "run without --repo must use the registry"
"$TICK" run --repo "$b" --dry-run | grep "would sweep $b" > /dev/null || fail "dry-run must not sweep"
[ ! -f "$b/.oms/tick/last.json" ] || fail "dry-run must not write a receipt"
set +e
bad_idle="$(OMS_TICK_TASK_IDLE_DAYS=bad "$TICK" status 2>&1)"
bad_idle_rc=$?
set -e
[ "$bad_idle_rc" -eq 2 ] && printf '%s' "$bad_idle" | grep >/dev/null 'must be integers' ||
  fail "a non-numeric task idle threshold must be rejected: $bad_idle"
set +e
bad_plan_idle="$(OMS_TICK_PLAN_IDLE_DAYS=bad "$TICK" status 2>&1)"
bad_plan_idle_rc=$?
set -e
[ "$bad_plan_idle_rc" -eq 2 ] && printf '%s' "$bad_plan_idle" | grep >/dev/null 'must be integers' ||
  fail "a non-numeric plan idle threshold must be rejected: $bad_plan_idle"
set +e
bad_failure_stale="$(OMS_TICK_FAILURE_STALE_DAYS=bad "$TICK" status 2>&1)"
bad_failure_stale_rc=$?
set -e
[ "$bad_failure_stale_rc" -eq 2 ] && printf '%s' "$bad_failure_stale" | grep >/dev/null 'must be integers' ||
  fail "a non-numeric failure stale threshold must be rejected: $bad_failure_stale"

# --- opt-in idle-main wake ----------------------------------------------------
# A tmux stub with no sessions keeps the real server out: it logs every call,
# serves WAKE_MODE and the WAKE_SCREEN file as the main pane, and accepts keys.
# The panel main itself is stubbed in Python below.
wk="$TMP/wake"; make_repo "$wk"; mkdir -p "$TMP/wake-bin"
cat > "$TMP/wake-bin/tmux" <<EOF_TMUX
#!/usr/bin/env bash
printf '%s\\n' "\$*" >> "$TMP/tmux.log"
case "\$1" in
  display-message) [ -n "\${WAKE_MODE:-}" ] && printf '%s\\n' "\$WAKE_MODE" ;;
  capture-pane) [ -f "\${WAKE_SCREEN:-}" ] && cat "\$WAKE_SCREEN" ;;
  send-keys) exit 0 ;;
  list-windows) [ -n "\${WAKE_WINDOWS+x}" ] && printf '%b' "\$WAKE_WINDOWS" || exit 1 ;;
  *) exit 1 ;;
esac
EOF_TMUX
chmod +x "$TMP/wake-bin/tmux"
"$PLAN" --repo "$wk" init --goal wake-goal --accept true >/dev/null
"$PLAN" --repo "$wk" add --id t1 --title wake-task >/dev/null
out="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --repo "$wk" --dry-run)"
printf '%s' "$out" | grep >/dev/null "^wake $wk: skipped: " || fail "a dry-run wake must report its probe: $out"
[ ! -e "$wk/.oms/tick" ] && [ ! -e "$wk/.oms/artifacts/panel-main-start.lock" ] ||
  fail "a dry-run wake must write nothing"
rm -f "$TMP/tmux.log"
out="$(PATH="$TMP/wake-bin:$PATH" "$TICK" run --repo "$wk")"
printf '%s' "$out" | grep >/dev/null 'wake=skipped' || fail "the wake must be off by default: $out"
if printf '%s' "$out" | grep >/dev/null "^wake "; then fail "a disabled wake must not probe: $out"; fi
[ ! -e "$TMP/tmux.log" ] || fail "a disabled wake must not query tmux"
out="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --repo "$wk")"
printf '%s' "$out" | grep >/dev/null 'wake=0' && printf '%s' "$out" | grep >/dev/null "^wake $wk: skipped: " ||
  fail "an enabled wake without a panel main must submit nothing: $out"
if grep -q send-keys "$TMP/tmux.log"; then fail "a wake without a verified main must not type"; fi
PATH="$TMP/wake-bin:$PATH" OMS_ROOT="$ROOT" python3 - "$wk" "$TMP/wake-proposal" "$TMP/tmux.log" <<'PY' || fail "idle-main wake decisions are wrong"
import contextlib, json, os, sys, time
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, os.environ["OMS_ROOT"] + "/scripts/lib")
import panel_interaction, panel_native, terminal_panel
repo, bare, tmux_log = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
member, attempt, now = "call-wake", "att_" + "a" * 32, float(int(time.time()))
activity = repo / ".oms" / "hooks" / "panel-activity" / (member + ".json")
activity.parent.mkdir(parents=True)
ledger = repo / ".oms" / "tick" / "last.json"
E = "\x1b"
# Screens the stub tmux serves: Codex (the main under test is Codex; the proof is provider-neutral) and Claude shapes.
CODEX_EMPTY = "› " + E + "[2mAsk Codex to do anything" + E + "[22m\n\n  " + E + "[2m⏎ send   ⌃J newline" + E + "[0m\n\n"
EMPTY = {
    "codex placeholder": CODEX_EMPTY,
    "claude box": ("> earlier question\n  an answer\n╭──────────────────────╮\n│ > " + E + "[7mT" + E + "[27m" + E +
                   "[2mry \"fix lint\"" + E + "[22m   │\n╰──────────────────────╯\n  ? for shortcuts\n"),
    "claude rule": "────────\n" + E + "[38;2;153;153;153m❯ " + E + "[0m" + E + "[7m " + E + "[0m\n────────\n  accept edits on\n",
    "grey 256-colour placeholder": "› " + E + "[38;5;244mType a message" + E + "[39m\n",
    "bright-black placeholder": "│ > " + E + "[90mType your message" + E + "[39m │\n╰─────╯\n",
}
UNPROVEN = {
    "codex draft": ("› fix the tests\n\n  " + E + "[2m⏎ send" + E + "[0m\n", "holds text"),
    "claude draft": ("│ > half typed reply   │\n╰──────╯\n", "holds text"),
    "cursor over a typed character": ("│ > " + E + "[7ma" + E + "[27m   │\n╰──╯\n", "holds text"),
    "malformed colour": ("› " + E + "[38;5mtyped\n", "holds text"),
    "multi-line draft": ("│ > " + E + "[7m " + E + "[27m   │\n│   second line   │\n╰──╯\n", "continue below"),
    "undimmed text below": ("› \n\nnot dim\n", "continue below"),
    "boxed draft ending in a bare prompt": ("│ > Please review: │\n│ >   │\n╰──╯\n", "more than one prompt"),
    "draft ending in a bare prompt": ("› Please review:\n\n› \n", "more than one prompt"),
    "one typed character, then a hint": ("› " + E + "[7ma" + E + "[27m " + E + "[2mhint" + E + "[22m\n", "holds text"),
    "cursor before undimmed text": ("› " + E + "[7ma" + E + "[27mbc\n", "holds text"),
    "two cursor cells": ("› " + E + "[7mab" + E + "[27m" + E + "[2mhint" + E + "[22m\n", "holds text"),
    "cursor after the placeholder": ("› " + E + "[2mType" + E + "[7mx" + E + "[0m\n", "holds text"),
    # A choice dialog draws its selection with the prompt marker; typing there would answer the dialog.
    "claude permission dialog": ("╭──────────────────────────────╮\n│ Do you want to proceed?      │\n"
                                 "│ ❯ 1. Yes                     │\n│   2. No, and tell Claude what to do differently (esc) │\n"
                                 "╰──────────────────────────────╯\n", "holds text"),
    "claude question dialog": ("────────\n Which approach?\n\n" + E + "[38;5;153m❯ 1. Keep the cache" + E + "[39m\n"
                               "  2. Drop the cache\n────────\n Enter to select · Esc to cancel\n", "holds text"),
    "codex approval dialog": ("  Would you like to run the following command?\n\n› 1. Yes, proceed (y)\n"
                              "  2. No, and tell Codex what to do differently (esc)\n", "holds text"),
    "no prompt": ("Working (esc to interrupt)\n", "no input prompt"),
    "empty capture": ("", "no input prompt"),
}
screen = Path(str(tmux_log) + ".screen")
def show(text, mode="0"):
    screen.write_text(text)
    os.environ.update(WAKE_SCREEN=str(screen), WAKE_MODE=mode)
def idle(age, state="idle", who=attempt):
    activity.write_text(json.dumps({"schema": 1, "participant": member, "state": state, "since": now - age,
                                    "updated_at": now - age, "attempt": who, "room": "room-wake"}))
def wakes():
    return json.loads(ledger.read_text()).get("wakes", []) if ledger.exists() else []
def keys():
    return [line for line in tmux_log.read_text().splitlines() if line.startswith("send-keys")] if tmux_log.exists() else []
history = []
@contextlib.contextmanager
def main(submit=None):
    with patch.object(panel_interaction, "_live_main_blocker", return_value="a panel main is live"), \
            patch.object(panel_native, "live_main_pane", return_value=("%7", "codex")), \
            patch.object(panel_native, "_swap_main_window", return_value=("@1", attempt, "%7", member, "room-wake")), \
            patch.object(terminal_panel, "resolve_session", return_value=("oms-wake", str(repo))), \
            patch.object(terminal_panel, "main_history", side_effect=lambda *a: history), \
            (patch.object(terminal_panel, "_swap_submit") if submit is None else contextlib.nullcontext()) as typed:
        yield typed
show(CODEX_EMPTY)
wake = lambda **k: panel_interaction.wake_main(repo, now=now, **k)
with main() as typed:
    # Busy: missing, malformed, recent (one second short), waiting or another attempt's record.
    assert "not idle" in wake(), wake()
    activity.write_text("{")
    assert "not idle" in wake()
    for args in ((1799,), (3600, "waiting"), (3600, "busy"), (3600, "idle", "att_" + "b" * 32)):
        idle(*args)
        assert "not idle" in wake(), args
    idle(1800)
    history[:] = [{"terminal": False, "refs": {"panel_role": "worker"}}]
    assert wake() == "skipped: main has a live child call", wake()
    history[:] = [{"terminal": True, "refs": {"panel_role": "reviewer"}}]
    with terminal_panel._lock(repo, "panel-main-start"):
        assert wake() == "skipped: panel typing lock held"
    typed.assert_not_called()
    assert wakes() == []
    # Dry run: the prompt it would type, and nothing typed or recorded.
    shown = wake(dry_run=True)
    assert shown.startswith("would wake call-wake: OMS tick wake: ready plan tasks t1. "), shown
    typed.assert_not_called()
    assert wakes() == []
    # Idle at exactly the threshold with ready work: one submit, recorded first; a retry never repeats it.
    assert wake() == "woke call-wake: " + shown.split(": ", 1)[1]
    assert typed.call_count == 1 and typed.call_args.args == ("%7", shown.split(": ", 1)[1]), typed.call_args
    assert "\n" not in typed.call_args.args[1] and "/goal" not in typed.call_args.args[1]
    assert len(wakes()) == 1 and wakes()[0]["main"] == member and wakes()[0]["outcome"] == "woke", wakes()
    assert "already woken today" in wake() and typed.call_count == 1
    # Daily cap: five prior wakes leave room for a sixth; six leave none; yesterday's do not count.
    day = time.strftime("%Y-%m-%d", time.gmtime(now))
    def seed(count, when=day, **extra):
        ledger.write_text(json.dumps({"schema": 1, "wakes": [
            dict({"day": when, "main": member, "key": "k%d" % n}, **extra) for n in range(count)]}))
    seed(6)
    assert wake() == "skipped: call-wake reached 6 wakes today" and typed.call_count == 1
    seed(5)
    assert wake().startswith("woke ") and typed.call_count == 2 and len(wakes()) == 6
    seed(6, "2000-01-01")
    assert wake().startswith("woke ") and typed.call_count == 3 and len(wakes()) == 1
    # The input line: only a provably empty one is typed into; any other pane is skipped, recorded, never entered.
    for name, text in EMPTY.items():
        seed(0)
        show(text)
        assert wake(dry_run=True).startswith("would wake "), name
    for name, (text, why) in UNPROVEN.items():
        seed(0)
        show(text)
        assert wake(dry_run=True).startswith("would skip call-wake: ") and wakes() == [], name
        status = wake()
        assert status.startswith("skipped: call-wake; ") and why in status, (name, status)
        assert [(w["outcome"], w["reason"]) for w in wakes()] == [("skipped", status.split("; ", 1)[1])], wakes()
    for mode in ("1", ""):
        seed(0)
        show(CODEX_EMPTY, mode)
        assert "copy mode or unreadable" in wake(), mode
    seed(0)
    os.environ["WAKE_SCREEN"] = str(screen) + ".missing"
    os.environ["WAKE_MODE"] = "0"
    assert "capture unavailable" in wake()
    assert typed.call_count == 3
    # Skips hold neither the dedupe key nor the cap: six skips today, then an empty line wakes.
    seed(6, outcome="skipped", reason="main input line holds text")
    show(CODEX_EMPTY)
    assert wake().startswith("woke ") and typed.call_count == 4
    # A malformed or unreadable ledger refuses the wake and is kept byte for byte.
    for bad in ("", "{", "[]", '{"wakes": {}}', '{"wakes": [1]}', '{"wakes": [{"day": 1, "main": "x"}]}', '{"wakes": [{"day": "d"}]}'):
        ledger.write_text(bad)
        for dry in (False, True):
            status = wake(dry_run=dry)
            assert status.startswith("refused: ") and "kept as found" in status, (bad, status)
            assert ledger.read_text() == bad
    # Only a missing receipt is empty history: a linked or non-regular one refuses and stays as found.
    ledger.unlink()
    ledger.mkdir()
    assert wake().startswith("refused: tick receipt is a symlink, a hard link or not a regular file")
    ledger.rmdir()
    target = ledger.with_name("elsewhere.json")
    target.write_text('{"wakes": []}')
    for link in (ledger.symlink_to, lambda path: os.link(path, ledger)):
        link(target)
        for dry in (False, True):
            assert wake(dry_run=dry).startswith("refused: tick receipt is a symlink, a hard link"), link
        assert target.read_text() == '{"wakes": []}' and ledger.exists()
        ledger.unlink()
    target.unlink()
    assert typed.call_count == 4
    # No shared work: nothing to wake for, however idle; a proposal under review is a reason.
    (bare / ".oms" / "plan").mkdir(parents=True)
    bare_activity = bare / activity.relative_to(repo)
    bare_activity.parent.mkdir(parents=True)
    bare_activity.write_text(activity.read_text())
    assert panel_interaction.wake_main(bare, now=now) == "skipped: no ready plan work or pending proposal"
    (bare / ".oms" / "plan" / "autopilot-run.json").write_text(json.dumps({"stage": "proposing"}))
    assert panel_interaction.wake_main(bare, now=now).startswith("skipped: no ready")
    (bare / ".oms" / "plan" / "autopilot-run.json").write_text(json.dumps({"stage": "proposal-review"}))
    assert "autopilot proposal waits in proposal-review" in panel_interaction.wake_main(bare, now=now)
    receipt = bare / ".oms" / "plan" / "autopilot-run.json"
    receipt.unlink()
    receipt.symlink_to(bare / ".oms" / "plan" / "elsewhere.json")
    assert panel_interaction.wake_main(bare, now=now) == "skipped: autopilot receipt unavailable or unsafe"
    assert typed.call_count == 5
    # One pool with the headless resumes: six spent by autopilot rows leave an interactive wake none, five leave one.
    seed(6, main="autopilot", outcome="resumed")
    assert wake() == "skipped: call-wake reached 6 wakes today" and typed.call_count == 5
    seed(3, main="autopilot", outcome="resumed")
    assert wake().startswith("woke ") and typed.call_count == 6 and len(wakes()) == 4
    seed(0)
# Through the real submit and the tmux stub: a draft gets no keys at all; an empty line gets the text, then Enter.
with main(submit=True):
    tmux_log.unlink()
    show(UNPROVEN["codex draft"][0])
    assert wake().startswith("skipped: call-wake; ") and keys() == [], keys()
    show(CODEX_EMPTY)
    typed_text = wake().split(": ", 1)[1]
    assert keys() == ["send-keys -t %7 -l " + typed_text, "send-keys -t %7 Enter"], keys()
for argv in ([], [str(repo), "--apply"], [str(repo), "--dry-run", "x"]):
    with contextlib.redirect_stderr(open(os.devnull, "w")):
        assert panel_interaction.wake_cli(argv) == 2, argv
PY
# The sweep receipt carries today's wake ledger forward, so the cap survives later ticks.
out="$(PATH="$TMP/wake-bin:$PATH" "$TICK" run --repo "$wk")"
python3 - "$wk/.oms/tick/last.json" <<'PY' || fail "the sweep receipt must keep today's wake ledger: $(cat "$wk/.oms/tick/last.json")"
import json, sys
r = json.load(open(sys.argv[1]))
outcomes = [w["outcome"] for w in r["wakes"] if w["main"] == "call-wake"]
assert r["wake"] == "skipped" and outcomes == ["skipped", "woke"], r
PY
# A malformed receipt is never rewritten: the sweep keeps it and prints one recovery line.
for bad in '' '{' '{"wakes": "x"}'; do
  printf '%s' "$bad" > "$wk/.oms/tick/last.json"
  out="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --repo "$wk")"
  [ -f "$wk/.oms/tick/last.json" ] && [ "$(cat "$wk/.oms/tick/last.json")" = "$bad" ] || fail "a malformed receipt must be kept: $bad"
  [ "$(printf '%s\n' "$out" | grep -c '^tick receipt kept: ')" -eq 1 ] && printf '%s' "$out" | grep >/dev/null "^wake $wk: refused: " ||
    fail "a malformed receipt must refuse the wake and print one recovery line: $out"
done
rm -f "$wk/.oms/tick/last.json"
# A symlinked or hard-linked receipt is neither followed nor replaced: the link and its target stay as found.
printf '{"wakes": []}' > "$wk/.oms/tick/elsewhere.json"
for kind in symlink hardlink; do
  if [ "$kind" = symlink ]; then ln -s elsewhere.json "$wk/.oms/tick/last.json"; else ln "$wk/.oms/tick/elsewhere.json" "$wk/.oms/tick/last.json"; fi
  dry="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --dry-run --repo "$wk")"
  out="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --repo "$wk")"
  printf '%s' "$dry" | grep >/dev/null "refused: tick receipt is a symlink" && printf '%s' "$out" | grep >/dev/null "^wake $wk: refused: " &&
    [ "$(printf '%s\n' "$out" | grep -c '^tick receipt kept: ')" -eq 1 ] || fail "a $kind receipt must refuse the wake and the sweep: $dry / $out"
  [ "$(cat "$wk/.oms/tick/elsewhere.json")" = '{"wakes": []}' ] && [ "$(find "$wk/.oms/tick" -name '.tick.*' -o -name '.wake.*' | wc -l)" -eq 0 ] ||
    fail "a $kind receipt's target must stay unchanged with no temp file left"
  if [ "$kind" = symlink ]; then [ -L "$wk/.oms/tick/last.json" ]; else [ ! -L "$wk/.oms/tick/last.json" ] && [ "$(cat "$wk/.oms/tick/last.json")" = '{"wakes": []}' ]; fi ||
    fail "a $kind receipt must not be replaced"
  rm -f "$wk/.oms/tick/last.json"
done
rm -f "$wk/.oms/tick/elsewhere.json"

# --- no live main: the opt-in tick resumes an approved autopilot contract --------
# Receipts come from the autopilot's own row builder, so its validators judge them. The spawn seam is stubbed at the
# process boundary: nothing here starts autopilot.sh or a provider.
rs="$TMP/resume"; make_committed_repo "$rs"
cat > "$TMP/resume_fx.py" <<'PY'
import argparse, hashlib, json, os, runpy, subprocess
from pathlib import Path


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


def build(lib, repo, stage, text="spec\n", diagnostic=None, state=None):
    repo = Path(repo)  # the execution checkout; STATE holds the receipt and plan (a scratch worktree runs from its parent)
    state = Path(state or repo)
    rules = runpy.run_path(os.path.join(lib, "autopilot-receipt.py"))
    base_file = state / ".oms" / "fx-base"
    if not base_file.exists():
        base_file.write_text(git(state, "symbolic-ref", "--short", "HEAD"))
    base, plan = base_file.read_text(), state / ".oms" / "plan"
    (repo / "PROJECT.md").write_text(text)
    plan.mkdir(parents=True, exist_ok=True)
    proposal = plan / "proposal-fx.json"
    proposal.write_bytes(b'{"tasks": []}\n')
    branch = "oms/autopilot-" + hashlib.sha256(b"spec\n").hexdigest()[:12]
    onto = base if stage in ("proposing", "proposal-review", "parked") else branch
    head = subprocess.run(["git", "-C", str(repo), "symbolic-ref", "-q", "--short", "HEAD"], capture_output=True, text=True)
    if head.stdout.strip() != onto:
        held = [ln for ln in git(repo, "worktree", "list", "--porcelain").splitlines() if ln == "branch refs/heads/" + onto]
        # Newer git refuses -B onto a branch another worktree holds: the scratch fixture then detaches at it.
        subprocess.run(["git", "-C", str(repo), "checkout", "-q", "--detach", onto] if held else
                       ["git", "-C", str(repo), "checkout", "-q", "-B", onto], check=True)
    diag = (diagnostic or {}) if stage == "parked" else {}
    args = argparse.Namespace(
        stage=stage, repo=str(repo), spec_sha256=hashlib.sha256(b"spec\n").hexdigest(), planner="claude",
        worker="codex", reviewer="claude", provider_timeout="5m", planner_model="", planner_fallback_model="",
        planner_reasoning_effort="auto", planner_timeout="15m", worker_model="", worker_fallback_model="",
        worker_reasoning_effort="auto", worker_timeout="30m", reviewer_model="", reviewer_fallback_model="",
        reviewer_reasoning_effort="auto", reviewer_timeout="20m", allowed="scripts", base=base,
        base_sha=git(repo, "rev-parse", base), remote="origin", max_cycles=3, initial_tasks=2, replan_tasks=1,
        auto_repair=False, retry_known=False, allow_verifier_change=False, allow_floor_change="",
        review_mode="shadow", collaboration="off", draft_pr=False,
        proposal="" if stage == "proposing" else str(proposal),
        proposal_sha256="" if stage == "proposing" else hashlib.sha256(proposal.read_bytes()).hexdigest(),
        branch=branch, owner_id="", updated="2026-11-09T00:00:00Z", diagnostic_phase=diag.get("phase", ""),
        diagnostic_reason_code=diag.get("code", ""), diagnostic_reason=diag.get("reason", ""),
        diagnostic_semantic_verdict="unknown", diagnostic_evidence_path="")
    row = rules["row_from_args"](args)
    (plan / "autopilot-run.json").write_bytes((json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode())
    return rules, row
PY
rs_managed="$TMP/resume-managed"
rs_scratch="$(OMS_HARNESS_CHILD=0 OMS_HARNESS_DELEGATE_DEPTH=0 OMS_DELEGATE_WORKTREE_ROOT="$rs_managed" "$ROOT/scripts/scratch-worktree.sh" add --repo "$rs" --owner-pid $$)" ||
  fail "resume scratch fixture creation failed"
PATH="$TMP/wake-bin:$PATH" OMS_ROOT="$ROOT" OMS_HARNESS_DELEGATE_DEPTH=0 OMS_DELEGATE_WORKTREE_ROOT="$rs_managed" python3 - "$rs" "$TMP" "$rs_scratch" <<'PY' || fail "no-main autopilot resume decisions are wrong"
import contextlib, json, os, sys, time
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, os.environ["OMS_ROOT"] + "/scripts/lib")
sys.path.insert(0, sys.argv[2])
import panel_interaction, panel_release, resume_fx, room, terminal_panel
repo, tmp, lib = Path(sys.argv[1]), Path(sys.argv[2]), os.environ["OMS_ROOT"] + "/scripts/lib"
scratch = Path(sys.argv[3])
now = float(int(time.time()))
day = time.strftime("%Y-%m-%d", time.gmtime(now))
ledger, receipt_file = repo / ".oms" / "tick" / "last.json", repo / ".oms" / "plan" / "autopilot-run.json"
state = {"n": 0}
def marker():
    state["n"] += 1
    return str(tmp / ("resume-marker-%d" % state["n"]))
def seed(count, **extra):
    ledger.parent.mkdir(parents=True, exist_ok=True)
    ledger.write_text(json.dumps({"schema": 1, "wakes": [
        dict({"day": day, "main": "call-other", "key": "k%d" % n}, **extra) for n in range(count)]}))
def rows():
    return json.loads(ledger.read_text()).get("wakes", []) if ledger.exists() else []
def resume(**kw):
    return panel_interaction.wake_main(repo, now=now, once=kw.pop("once", None) or marker(), **kw)
def stage(name, **kw):
    return resume_fx.build(lib, repo, name, **kw)

with patch.object(panel_interaction, "_spawn_continuation", return_value=4242) as spawn:
    # No main (the real evidence: no attempts, no session) and a validated driving contract: exactly one spawn.
    rules, row = stage("driving")
    argv = rules["option_args"](row)
    first_marker = marker()
    status = resume(once=first_marker)
    assert status.startswith("resumed autopilot (stage driving, receipt "), status
    assert spawn.call_count == 1, spawn.call_args_list
    (command, log, cwd), unused = spawn.call_args
    assert command[0] == "bash" and command[1] == os.environ["OMS_ROOT"] + "/scripts/autopilot.sh" and command[2:] == argv[2:], command
    assert command[-2:] == ["--expected-proposal-sha256", row["proposal"]["sha256"]] and "propose" not in command[2:], command
    assert cwd == row["repo"] and log.startswith(str(repo / ".oms" / "tick" / "autopilot-")), (cwd, log)
    assert os.path.exists(first_marker)
    # The launch is recorded in the wake ledger before the spawn is observable, and carries the receipt digest.
    assert [(w["main"], w["outcome"], w["stage"]) for w in rows()] == [("autopilot", "resumed", "driving")], rows()
    # The same receipt bytes never launch twice in a day; the next state does, but not twice in one tick run.
    assert "already resumed today at this receipt state" in resume() and spawn.call_count == 1
    receipt_file.write_text(receipt_file.read_text().replace("2026-11-09T00:00:00Z", "2026-11-09T00:00:01Z"))
    status = resume(once=first_marker)
    assert "this tick run already resumed one autopilot" in status and spawn.call_count == 1, status
    assert resume().startswith("resumed ") and spawn.call_count == 2 and len(rows()) == 2, rows()
    # Dry run: the command it would run; no spawn, marker or ledger row.
    receipt_file.write_text(receipt_file.read_text().replace("2026-11-09T00:00:01Z", "2026-11-09T00:00:02Z"))
    before, dry_marker = ledger.read_text(), marker()
    shown = resume(dry_run=True, once=dry_marker)
    assert shown.startswith("would resume autopilot (stage driving") and "--expected-proposal-sha256 " in shown, shown
    assert spawn.call_count == 2 and ledger.read_text() == before and not os.path.exists(dry_marker)
    # Daily cap, one pool with the wake rows: six spent leave none, five leave one, skipped rows and yesterday's do not count.
    stage("driving")
    seed(6, outcome="woke")
    assert "6 wakes already spent today; resumes when the UTC day rolls over" in resume() and spawn.call_count == 2
    seed(6, outcome="skipped", reason="main input line holds text")
    assert resume().startswith("resumed ") and spawn.call_count == 3
    stage("driving"); seed(5, outcome="woke")
    assert resume().startswith("resumed ") and spawn.call_count == 4 and len(rows()) == 6
    stage("driving"); seed(0)
    # Never resumed: a proposal in review, a parked receipt, a finished run, a proposing contract, an invalid or absent one.
    expected = {
        "proposal-review": ("autopilot proposal-review: a proposal awaits parent review", "runs the printed `oms autopilot run --proposal"),
        "parked": ("autopilot parked: the run is parked for an operator decision; resolve the park reason or abandon; next step: "
                   "inspect the wall (wall-kill)", "the operator resolves the park"),
        "done": ("autopilot done: the run is complete", "a new `oms autopilot propose`"),
        "proposing": ("autopilot continuation at stage proposing is not an approved `run`", "oms autopilot run --proposal FILE"),
    }
    for name, (why, until) in expected.items():
        seed(0)
        stage(name, diagnostic={"phase": "driving", "code": "wall-kill", "reason": "inspect the wall"} if name == "parked" else None)
        status = resume()
        assert status.startswith("skipped: ") and why in status and until in status, (name, status)
    stage("driving", text="spec changed\n")
    status = resume()
    assert "autopilot receipt invalid" not in status and "digest gate: PROJECT.md differs from the outer receipt" in status, status
    for bad in ("{", "[]", ""):
        receipt_file.write_text(bad)
        status = resume()
        assert status.startswith("skipped: autopilot receipt invalid"), (bad, status)
    receipt_file.unlink()
    receipt_file.symlink_to(repo / ".oms" / "plan" / "elsewhere.json")
    assert resume().startswith("skipped: autopilot receipt invalid"), resume()
    receipt_file.unlink()
    assert "no live main and no autopilot contract; resumes when" in resume()
    assert spawn.call_count == 4, spawn.call_count
    # A living claimant (the run is already held) is the autopilot's own refusal: nothing a second time.
    stage("driving"); seed(0)
    receipt_file.with_name("autopilot-reentries.jsonl").write_text(json.dumps({
        "schema": 1, "kind": "autopilot-drive-claim", "spec_sha256": row["spec_sha256"], "branch": row["branch"],
        "owner_id": "", "claim_pid": os.getpid(), "updated": "2026-11-09T00:00:00Z"}) + "\n")
    status = resume()
    assert status.startswith("skipped: autopilot driving: this run is already held by a live session") and spawn.call_count == 4, status
    receipt_file.with_name("autopilot-reentries.jsonl").unlink()
    # A live main (or unverified liveness) goes the idle-main wake path; nothing is resumed or spawned.
    for blocker in ("a panel main is live", "main liveness unverified: tmux status unavailable"):
        with patch.object(panel_interaction, "_live_main_blocker", return_value=blocker):
            status = resume()
        assert status.startswith("skipped: ") and "resumed" not in status and "contract" not in status, status
    assert spawn.call_count == 4 and rows() == [], rows()
    # Without a pane to type into, the blocker's reason and its recovery command are the skip result, not "no ready work".
    hint = "this panel already has a live main or unverified owner; if crashed: oms panel --release-main att_" + "a" * 32
    for dry in (False, True):
        with patch.object(panel_interaction, "_live_main_blocker", return_value=hint):
            status = resume(dry_run=dry)
        assert status.startswith("skipped: no live main pane to type into") and status.endswith(hint), status
    assert spawn.call_count == 4 and rows() == [], rows()
    # A receipt bound to another checkout of this state root (a scratch worktree) is refused before the per-run
    # marker, the ledger and the spawn: the caller is the checkout the tick runs for, never the receipt's own repo.
    foreign_marker = marker()
    resume_fx.build(lib, scratch, "driving", state=repo)
    foreign = resume(once=foreign_marker)
    assert foreign.startswith("skipped: autopilot driving: digest gate: outer receipt belongs to a different repository"), foreign
    assert not os.path.exists(foreign_marker) and rows() == [] and spawn.call_count == 4, (foreign, rows())
    assert panel_interaction.wake_main(repo, dry_run=True, now=now).startswith("skipped: autopilot driving: digest gate"), "dry"
    stage("driving")
    # A control-only session holding a releasable dead main is not occupied: the resume goes ahead, and the tick
    # releases nothing (release stays the panel's job). The same history without the release proof does block.
    dead = {"tool": "panel-main", "terminal": False, "state": "working", "attempt_id": "att_" + "d" * 32,
            "refs": {"panel_role": "main", "panel_room_participant": "call-dead", "panel_room_id": "room-dead"}}
    os.environ["WAKE_WINDOWS"] = "\t\t\tcontrol\t@0\t\n"
    def session(releasable):
        return (patch.object(terminal_panel, "events", return_value=json.dumps([dead])),
                patch.object(terminal_panel, "resolve_session", return_value=("oms-dead", str(repo))),
                patch.object(terminal_panel, "recorded_room", return_value="room-dead"),
                patch.object(panel_release, "classify", return_value={
                    "releasable": [{"attempt": dead["attempt_id"]}] if releasable else [], "unverifiable": []}),
                patch.object(room, "resolve", side_effect=lambda unused, ident: ident),
                patch.object(room, "same_room", return_value=True),
                patch.object(room, "status", return_value={"participants": [
                    {"participant": "call-dead", "joined": True, "role": "main"}]}),
                patch.object(panel_release, "release_held", side_effect=AssertionError("the tick must not release")))
    with contextlib.ExitStack() as stack:
        for item in session(True):
            stack.enter_context(item)
        assert panel_interaction._live_main_blocker(repo) == ""
        status = resume()
        assert status.startswith("resumed autopilot (stage driving"), status
    assert spawn.call_count == 5 and len(rows()) == 1
    seed(0)
    with contextlib.ExitStack() as stack:
        for item in session(False):
            stack.enter_context(item)
        assert panel_interaction._live_main_blocker(repo) != ""
    del os.environ["WAKE_WINDOWS"]
    stage("driving"); seed(0)
    # A held start lock and a malformed ledger stop the resume before any record or spawn; the ledger is kept as found.
    with terminal_panel._lock(repo, "panel-main-start"):
        assert resume() == "skipped: panel typing lock held"
    ledger.write_text("{")
    status = resume()
    assert status.startswith("refused: ") and "kept as found" in status and ledger.read_text() == "{" and spawn.call_count == 5
    ledger.unlink()
    # An unwritable marker fails closed.
    assert "cannot claim this tick run's resume marker" in resume(once=str(tmp / "no-such-dir" / "m")) and spawn.call_count == 5
# The real spawn seam: detached (its own session), output to the log, never waited on.
log = tmp / "spawn.log"
pid = panel_interaction._spawn_continuation(["bash", "-c", "echo spawned; exit 0"], str(log), str(tmp))
assert os.getsid(pid) == pid or not os.path.exists("/proc/%d" % pid)
deadline = time.time() + float(os.environ.get("OMS_TEST_CALL_TIMEOUT", "60"))
while "spawned" not in (log.read_text() if log.exists() else "") and time.time() < deadline:
    time.sleep(.05)
assert log.read_text() == "spawned\n", log.read_text()
for argv in ([str(repo), "--once"], [str(repo), "--dry-run", "--once"]):
    with contextlib.redirect_stderr(open(os.devnull, "w")):
        assert panel_interaction.wake_cli(argv) == 2, argv
PY
# Through the tick: a dry run shows the command it would run; a real sweep with a parked contract spawns nothing and says why.
rs_stage() { OMS_ROOT="$ROOT" python3 -c 'import os, sys; sys.path[:0] = [sys.argv[1], os.environ["OMS_ROOT"] + "/scripts/lib"]; import resume_fx; resume_fx.build(os.environ["OMS_ROOT"] + "/scripts/lib", sys.argv[2], sys.argv[3], diagnostic={"phase": "driving", "code": "wall-kill", "reason": "inspect the wall"})' "$TMP" "$rs" "$1"; }
rm -rf "$rs/.oms/tick"
rs_stage driving
out="$(PATH="$TMP/wake-bin:$PATH" OMS_TICK_WAKE=1 "$TICK" run --repo "$rs" --dry-run)"
printf '%s' "$out" | grep >/dev/null "^wake $rs: would resume autopilot (stage driving, receipt .*--expected-proposal-sha256 " ||
  fail "a dry-run tick must show the approved continuation: $out"
[ ! -e "$rs/.oms/tick" ] || fail "a dry-run resume must write nothing"
out="$(PATH="$TMP/wake-bin:$PATH" "$TICK" run --repo "$rs")"
printf '%s' "$out" | grep >/dev/null '^wake ' && fail "the resume must be off unless OMS_TICK_WAKE=1: $out"
rs_stage parked
mkdir -p "$TMP/tickrun"
out="$(PATH="$TMP/wake-bin:$PATH" TMPDIR="$TMP/tickrun" OMS_TICK_WAKE=1 "$TICK" run --repo "$rs")"
printf '%s' "$out" | grep >/dev/null "^wake $rs: skipped: autopilot parked: .*next step: inspect the wall (wall-kill); resumes when the operator resolves the park" &&
  printf '%s' "$out" | grep >/dev/null 'wake=0' || fail "a parked contract must stay parked with its reason: $out"
[ -z "$(find "$rs/.oms/tick" -name 'autopilot-*.log' 2>/dev/null)" ] && [ -z "$(ls "$TMP/tickrun")" ] ||
  fail "a skipped resume leaves no log, and the run scratch is removed"

# --- install / status / uninstall through the stub --------------------------
ci_repo="$TMP/ci-owner"; make_committed_repo "$ci_repo"
git -C "$ci_repo" remote add origin https://github.com/example/fixture.git
cat > "$TMP/bin/gh-ci" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  'run list')
    printf '%s\n' queried >> "$OMS_TEST_CI_CALLS"
    printf '[{"status":"completed","conclusion":"success","workflowName":"test","headSha":"%s","url":"https://example.invalid/run/1"}]\n' "$OMS_TEST_CI_SHA"
    ;;
  'pr view') exit 1 ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$TMP/bin/gh-ci"
export OMS_GH_BIN="$TMP/bin/gh-ci" OMS_TEST_CI_CALLS="$TMP/ci-calls"
OMS_TEST_CI_SHA="$(git -C "$ci_repo" rev-parse HEAD)"; export OMS_TEST_CI_SHA
OMS_CI_TICK=0 "$TICK" run --repo "$ci_repo" >/dev/null
[ ! -e "$OMS_TEST_CI_CALLS" ] || fail "CI tick opt-out must not query GitHub"
"$TICK" run --repo "$ci_repo" >/dev/null
[ -s "$ci_repo/.oms/ci.jsonl" ] || fail "periodic tick must record available CI results"
"$TICK" run --repo "$ci_repo" >/dev/null
[ "$(wc -l < "$OMS_TEST_CI_CALLS")" -eq 1 ] || fail "completed HEAD must not be queried twice"
unset OMS_GH_BIN OMS_TEST_CI_CALLS OMS_TEST_CI_SHA
journal_off="$TMP/journal-off"; make_committed_repo "$journal_off"
OMS_WORK_JOURNAL=0 "$TICK" run --repo "$journal_off" >/dev/null
[ ! -e "$journal_off/.oms/work-journal" ] || fail "disabled journal must remain untouched by tick"

(cd "$b" && "$TICK" install --method systemd --dry-run) | grep 'would install' > /dev/null || fail "install dry-run must print"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "dry-run must not write units"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" && fail "a dry-run install must not register the cwd"
(cd "$b" && "$TICK" install --method systemd) | grep 'systemd timer installed' > /dev/null || fail "install must report"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" || fail "install must register the adopted cwd"
grep -q 'OnCalendar=hourly' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" || fail "timer must be hourly"
grep -q "tick.sh\" run" "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service" || fail "service must run the tick"
grep -q "^Environment=PATH=$TMP/bin:" "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service" ||
  fail "the unit must carry the resolved tool dirs first in PATH: $(grep Environment "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service")"
grep -q 'enable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "install must enable the timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep >/dev/null 'timer: systemd (owned)' || fail "status must see the owned timer: $st"
printf '%s' "$st" | grep >/dev/null "$a  last:" || fail "status must show the last sweep: $st"
printf '%s' "$st" | grep >/dev/null 'tasks_closed=0' || fail "status must report task closures: $st"
printf '%s' "$st" | grep >/dev/null 'plans_retired=0' || fail "status must report plan retirements: $st"
printf '%s' "$st" | grep >/dev/null 'artifacts_resolved=0' || fail "status must report artifact recoveries: $st"
printf '%s' "$st" | grep >/dev/null 'artifacts_superseded=0' || fail "status must report artifact supersession: $st"
printf '%s' "$st" | grep >/dev/null 'failures_retired=0' || fail "status must report stale failure retirements: $st"
"$TICK" uninstall | grep 'removed' > /dev/null || fail "uninstall must report"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "uninstall must remove the timer"
grep -q 'disable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "uninstall must disable the owned timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep >/dev/null 'timer: none' || fail "status must report no timer: $st"
"$TICK" unregister --repo "$a" >/dev/null
"$TICK" unregister --repo "$b" >/dev/null
"$TICK" run | grep 'nothing registered' > /dev/null || fail "an empty registry must say so"

# The catalog refresh is shared: `refresh-catalogs` and the sweep claim one daily
# stamp, and only a listing older than seven days is asked for again.
cap="$TMP/cap-tick"; stubs="$TMP/stubs-tick"; stamp="$TMP/state/oh-my-setting/catalog-refresh.stamp"
mkdir -p "$cap" "$stubs"
cat > "$stubs/agy" <<EOF_STUB
#!/usr/bin/env bash
case "\${1:-}" in
  models) echo x >> "$TMP/agy-lists"; printf 'gemini-a\n' ;;
  *) printf -- '--effort (low, medium, high)\n' ;;
esac
EOF_STUB
chmod +x "$stubs/agy"
age() { python3 -c 'import os,sys,time; t=time.time()-int(sys.argv[2]); os.utime(sys.argv[1],(t,t))' "$1" "$2"; }
lists() { if [ -f "$TMP/agy-lists" ]; then wc -l < "$TMP/agy-lists" | tr -d ' '; else echo 0; fi; }
tick_cat() { PATH="$stubs:/usr/bin:/bin" OMS_CAPABILITY_DIR="$cap" OMS_CAPABILITY_SETTLE_WAIT=0 OMS_CAPABILITY_RPC_WAIT=0 "$TICK" "$@"; }
printf 'old-model\n' > "$cap/antigravity.models"; age "$cap/antigravity.models" 864000
age "$stamp" 172800
tick_cat refresh-catalogs
[ "$(lists)" = 1 ] && grep -Fxq gemini-a "$cap/antigravity.models" || fail "refresh-catalogs must relist a stale catalog once"
age "$stamp" 172800
tick_cat refresh-catalogs
[ "$(lists)" = 1 ] || fail "a fresh-enough catalog must not be listed again"
age "$cap/antigravity.models" 864000
tick_cat refresh-catalogs
[ "$(lists)" = 1 ] || fail "a claimed daily stamp must stop a second refresh"
age "$stamp" 172800
age "$cap/antigravity.models" 864000
tick_cat run | grep 'nothing registered' > /dev/null || fail "the sweep must still run"
[ "$(lists)" = 2 ] || fail "the sweep must share the refresh and its stamp: $(lists)"

# --dry-run reports the stale catalog and mutates nothing: no stamp claim, no listing, no catalog change.
touch "$stamp"; age "$stamp" 172800; age "$cap/antigravity.models" 864000
before_stamp="$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$stamp")"
before_cat="$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$cap/antigravity.models")"
dry="$(tick_cat run --dry-run)"
printf '%s' "$dry" | grep -Fq 'would refresh catalog: antigravity' || fail "dry-run must name the stale catalog: $dry"
[ "$(lists)" = 2 ] || fail "dry-run must not list a catalog"
[ "$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$stamp")" = "$before_stamp" ] || fail "dry-run must not touch the stamp"
[ "$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$cap/antigravity.models")" = "$before_cat" ] || fail "dry-run must not change the catalog"
rm -f "$stamp"
tick_cat run --dry-run >/dev/null
[ ! -e "$stamp" ] || fail "dry-run must not create the stamp"

# The direct verb honors the same dry-run, and a harness child is refused before any mutation.
touch "$stamp"; age "$stamp" 172800; age "$cap/antigravity.models" 864000
before_stamp="$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$stamp")"
before_cat="$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$cap/antigravity.models")"
dry="$(tick_cat refresh-catalogs --dry-run)"
printf '%s' "$dry" | grep -Fq 'would refresh catalog: antigravity' || fail "direct dry-run must name the stale catalog: $dry"
dry="$(OH_MY_SETTING_DRY_RUN=1 tick_cat refresh-catalogs)"
printf '%s' "$dry" | grep -Fq 'would refresh catalog: antigravity' || fail "the dry-run environment must be honored: $dry"
rc=0; out="$(OMS_HARNESS_CHILD=1 tick_cat refresh-catalogs 2>&1)" || rc=$?
[ "$rc" = 2 ] && printf '%s' "$out" | grep -Fq 'harness child' || fail "a harness child must be refused ($rc): $out"
[ "$(lists)" = 2 ] || fail "dry-run and child refusal must not list a catalog"
[ "$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$stamp")" = "$before_stamp" ] || fail "dry-run or child refusal touched the stamp"
[ "$(python3 -c 'import os,sys; print(os.stat(sys.argv[1]).st_mtime)' "$cap/antigravity.models")" = "$before_cat" ] || fail "dry-run or child refusal changed the catalog"

echo "tick-smoke: ok"
