#!/usr/bin/env bash
set -euo pipefail

# `oms tick` against fixture repos under TMP with an XDG home of its own and a
# systemctl stub, so nothing here reaches the real user manager, crontab,
# journal config, or Codex. Covers the registry, the sweep receipt, idle
# thread/task/plan thresholds, gc staying off by default, and install/uninstall ownership.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tick.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TICK="$ROOT/scripts/tick.sh"
PLAN="$ROOT/scripts/agent-plan.sh"
export XDG_CONFIG_HOME="$TMP/xdg" XDG_RUNTIME_DIR="$TMP/rt" OMS_INSTALL_RECEIPT="$TMP/no-receipt.json" \
  OMS_WORK_JOURNAL_CONFIG="$TMP/no-journal.json" OMS_TICK_THREAD_IDLE_DAYS=7 OMS_HARNESS_CHILD=0
mkdir -p "$XDG_RUNTIME_DIR" "$TMP/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/ntn"; chmod +x "$TMP/bin/ntn"
cat > "$TMP/bin/systemctl" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/systemctl.log"
[ "\$2" != show ] || printf '%s\n' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer"
EOF
chmod +x "$TMP/bin/systemctl"
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

# --- registry -----------------------------------------------------------------
a="$TMP/a"; b="$TMP/b"; make_repo "$a"; make_repo "$b"
"$TICK" register --repo "$a" | grep -q '^registered' || fail "register must add a repo"
"$TICK" register --repo "$a" | grep -q 'already registered' || fail "register must dedupe"
out="$("$TICK" register --repo "$TMP" 2>&1 || true)"
printf '%s' "$out" | grep -q 'no .oms' || fail "an unadopted dir must be refused: $out"
[ "$(wc -l < "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt")" -eq 1 ] || fail "registry must hold one line"

# --- sweep: idle thread/task closed, fresh/live task kept, gc off, receipt ----
THREAD="$ROOT/scripts/thread.sh"
TASK="$ROOT/scripts/agent-task.sh"
"$THREAD" new --repo "$a" --id old-thread --topic old >/dev/null
"$THREAD" append --repo "$a" --id old-thread --role note --text hello >/dev/null
"$THREAD" new --repo "$a" --id new-thread --topic new >/dev/null
touch -d '10 days ago' "$a/.oms/threads/old-thread.jsonl"
"$TASK" --repo "$a" init >/dev/null
set_task_activity "$a"
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep -q 'threads_closed=1' || fail "the idle thread must be closed: $out"
printf '%s' "$out" | grep -q 'tasks_closed=1' || fail "the idle goal-less task must be closed: $out"
"$THREAD" list --repo "$a" | grep -q 'new-thread' || fail "a fresh thread must stay open"
if "$THREAD" list --repo "$a" | grep -q 'old-thread'; then fail "the idle thread must not stay open"; fi
[ ! -e "$a/.oms/task/current.md" ] || fail "the idle goal-less task must not stay active"
archive="$(find "$a/.oms/task/archive" -type f -name '*.md' -print | sed -n '1p')"
[ -n "$archive" ] && grep -Fq 'closed by oms tick' "$archive" ||
  fail "the archived task must retain the tick close reason"
python3 - "$a/.oms/tick/last.json" <<'PY' || fail "sweep receipt is wrong: $(cat "$a/.oms/tick/last.json")"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["gc"] == "skipped" and r["threads_closed"] == ["old-thread"], r
assert r["tasks_closed"] == 1, r
assert r["artifacts_resolved"] == 0 and r["artifact_resolve_rc"] == 0, r
assert isinstance(r["journal_rc"], int) and isinstance(r["reconcile_rc"], int), r
PY
"$TASK" --repo "$a" init >/dev/null
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep -q 'tasks_closed=0' || fail "a fresh goal-less task must stay active: $out"
[ -f "$a/.oms/task/current.md" ] || fail "a fresh goal-less task must stay active"
"$TASK" --repo "$a" update --goal 'live task must not be retired' >/dev/null
set_task_activity "$a"
out="$("$TICK" run --repo "$a")"
printf '%s' "$out" | grep -q 'tasks_closed=0' || fail "a stale task with a goal must stay active: $out"
[ -f "$a/.oms/task/current.md" ] || fail "a stale task with a goal must stay active"
c="$TMP/nonactive-task"; make_repo "$c"
"$TASK" --repo "$c" init --verify true >/dev/null
"$TASK" --repo "$c" verify >/dev/null
set_task_activity "$c"
out="$("$TICK" run --repo "$c")"
printf '%s' "$out" | grep -q 'tasks_closed=0' || fail "a non-active task must stay active: $out"
[ -f "$c/.oms/task/current.md" ] || fail "a non-active task must stay active"

# --- sweep: idle all-done plans retire; nonempty active plans stay ---------
p="$TMP/idle-plan"; make_committed_repo "$p"; make_stale_plan "$p" done
out="$("$TICK" run --repo "$p")"
printf '%s' "$out" | grep -q 'plans_retired=1' || fail "the idle all-done plan must retire: $out"
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
q="$TMP/claimed-plan"; make_committed_repo "$q"; make_stale_plan "$q" ready
"$PLAN" --repo "$q" claim --id t1 --provider codex >/dev/null
set_plan_activity "$q"
out="$("$TICK" run --repo "$q")"
printf '%s' "$out" | grep -q 'plans_retired=0' || fail "a claimed plan must stay active: $out"
[ -f "$q/.oms/plan/tasks.json" ] || fail "a claimed plan must stay active"
e="$TMP/empty-plan"; make_committed_repo "$e"
"$PLAN" --repo "$e" init --goal empty-plan --accept false >/dev/null
out="$("$TICK" run --repo "$e")"
printf '%s' "$out" | grep -q 'plans_retired=0' || fail "a plan without tasks must stay active: $out"
[ -f "$e/.oms/plan/tasks.json" ] || fail "a plan without tasks must stay active"
i="$TMP/invalid-plan"; make_repo "$i"; mkdir -p "$i/.oms/plan"
printf '{not json\n' > "$i/.oms/plan/tasks.json"
out="$("$TICK" run --repo "$i")"
printf '%s' "$out" | grep -q 'plans_retired=0' || fail "an invalid plan must stay active: $out"
[ -f "$i/.oms/plan/tasks.json" ] || fail "an invalid plan must stay active"
u="$TMP/uncommitted-plan"; make_repo "$u"; make_stale_plan "$u" done
out="$("$TICK" run --repo "$u")"
printf '%s' "$out" | grep -q 'plans_retired=0' || fail "a failed plan retirement must not abort a sweep: $out"
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
printf '%s' "$out" | grep -q 'artifacts_resolved=1' ||
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
printf '%s' "$out" | grep -q 'artifacts_resolved=0' ||
  fail "a second artifact recovery sweep must be idempotent: $out"
w="$TMP/invalid-artifacts"; make_repo "$w"; mkdir -p "$w/.oms/artifacts"
printf '{not json\n' > "$w/.oms/artifacts/index.jsonl"
out="$("$TICK" run --repo "$w")"
printf '%s' "$out" | grep -q 'artifacts_resolved=0' ||
  fail "a failed artifact resolver must not abort a sweep: $out"
python3 - "$w/.oms/tick/last.json" <<'PY' || fail "a failed artifact resolver must record its exit"
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
assert isinstance(receipt["artifact_resolve_rc"], int) and receipt["artifact_resolve_rc"] != 0, receipt
PY
o="$TMP/retire-off"; make_committed_repo "$o"; make_stale_plan "$o" done
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$o")"
printf '%s' "$out" | grep -q 'plans_retired=0' || fail "retirement opt-out must keep plans: $out"
[ -f "$o/.oms/plan/tasks.json" ] || fail "retirement opt-out must keep plans"
r="$TMP/retire-off-task"; make_repo "$r"
"$TASK" --repo "$r" init >/dev/null
set_task_activity "$r"
out="$(OMS_TICK_RETIRE=0 "$TICK" run --repo "$r")"
printf '%s' "$out" | grep -q 'tasks_closed=0' || fail "retirement opt-out must keep tasks: $out"
[ -f "$r/.oms/task/current.md" ] || fail "retirement opt-out must keep tasks"
"$TICK" run | grep -q "swept $a" || fail "run without --repo must use the registry"
"$TICK" run --repo "$b" --dry-run | grep -q "would sweep $b" || fail "dry-run must not sweep"
[ ! -f "$b/.oms/tick/last.json" ] || fail "dry-run must not write a receipt"
set +e
bad_idle="$(OMS_TICK_TASK_IDLE_DAYS=bad "$TICK" status 2>&1)"
bad_idle_rc=$?
set -e
[ "$bad_idle_rc" -eq 2 ] && printf '%s' "$bad_idle" | grep -q 'must be integers' ||
  fail "a non-numeric task idle threshold must be rejected: $bad_idle"
set +e
bad_plan_idle="$(OMS_TICK_PLAN_IDLE_DAYS=bad "$TICK" status 2>&1)"
bad_plan_idle_rc=$?
set -e
[ "$bad_plan_idle_rc" -eq 2 ] && printf '%s' "$bad_plan_idle" | grep -q 'must be integers' ||
  fail "a non-numeric plan idle threshold must be rejected: $bad_plan_idle"

# --- install / status / uninstall through the stub --------------------------
(cd "$b" && "$TICK" install --method systemd --dry-run) | grep -q 'would install' || fail "install dry-run must print"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "dry-run must not write units"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" && fail "a dry-run install must not register the cwd"
(cd "$b" && "$TICK" install --method systemd) | grep -q 'systemd timer installed' || fail "install must report"
grep -Fxq "$b" "$XDG_CONFIG_HOME/oh-my-setting/tick-repos.txt" || fail "install must register the adopted cwd"
grep -q 'OnCalendar=hourly' "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" || fail "timer must be hourly"
grep -q "tick.sh\" run" "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service" || fail "service must run the tick"
grep -q "^Environment=PATH=$TMP/bin:" "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service" ||
  fail "the unit must carry the resolved tool dirs first in PATH: $(grep Environment "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.service")"
grep -q 'enable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "install must enable the timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep -q 'timer: systemd (owned)' || fail "status must see the owned timer: $st"
printf '%s' "$st" | grep -q "$a  last:" || fail "status must show the last sweep: $st"
printf '%s' "$st" | grep -q 'tasks_closed=0' || fail "status must report task closures: $st"
printf '%s' "$st" | grep -q 'plans_retired=0' || fail "status must report plan retirements: $st"
printf '%s' "$st" | grep -q 'artifacts_resolved=0' || fail "status must report artifact recoveries: $st"
"$TICK" uninstall | grep -q 'removed' || fail "uninstall must report"
[ ! -f "$XDG_CONFIG_HOME/systemd/user/oh-my-setting-tick.timer" ] || fail "uninstall must remove the timer"
grep -q 'disable --now oh-my-setting-tick.timer' "$TMP/systemctl.log" || fail "uninstall must disable the owned timer"
st="$("$TICK" status)"
printf '%s' "$st" | grep -q 'timer: none' || fail "status must report no timer: $st"
"$TICK" unregister --repo "$a" >/dev/null
"$TICK" unregister --repo "$b" >/dev/null
"$TICK" run | grep -q 'nothing registered' || fail "an empty registry must say so"

echo "tick-smoke: ok"
