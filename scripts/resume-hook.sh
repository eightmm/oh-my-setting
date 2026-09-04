#!/usr/bin/env bash
set -uo pipefail

# SessionStart hook: print one bounded resume block so a fresh session starts
# knowing what this repo was doing — active task packet (goal, next step,
# verify), newest handoff digest, and unresolved failures. Everything here is
# read from .oms state that was scrubbed at write time. The two writes this
# hook makes are the autopilot shadow-judgment row and a detached project-graph build, both
# regenerable and both ambient to the check gate; receipts, claims, and every
# other surface stay untouched.
# Best-effort by contract: a hook that blocks session start costs more than a
# missing resume line, so every failure path exits 0. OMS_RESUME_HOOK=0
# disables; harness children stay silent (their parent already has context).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[ "${OMS_RESUME_HOOK:-1}" = "1" ] || exit 0
[ "${OMS_HARNESS_CHILD:-0}" != "1" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"
cwd="$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
print(data.get("cwd", "") or "")
' 2>/dev/null)" || cwd=""
cwd="${cwd//$'\r'/}"
[ -n "$cwd" ] || cwd="$PWD"

# Only a harness-adopted repo has state worth resuming; a random directory
# must not produce noise (or a .oms tree) because a session started there.
[ -d "$cwd/.oms" ] || exit 0

out=""
append() { out="${out}${1}
"; }

# Active task packet: id/status from the status front door, goal and next
# step straight from the packet sections. A fresh active task was previously
# completely silent at session start — that is the gap this closes.
days_since() {  # days_since ISO8601Z -> whole days when >= 1, else empty
  python3 - "$1" <<'PY' 2>/dev/null
import datetime, sys
try:
    then = datetime.datetime.strptime(sys.argv[1], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)
except Exception:
    raise SystemExit(0)
days = (datetime.datetime.now(datetime.timezone.utc) - then).days
print(days if days >= 1 else "", end="")
PY
}

task_file="$cwd/.oms/task/current.md"
if [ -f "$task_file" ]; then
  status_line="$(cd "$cwd" && "$ROOT/scripts/agent-task.sh" status 2>/dev/null)" || status_line=""
  task_id="$(printf '%s\n' "$status_line" | sed -n 's/^task_id: //p')"
  task_status="$(printf '%s\n' "$status_line" | sed -n 's/^status: //p')"
  if [ -n "$task_id" ] && [ "$task_status" != "closed" ]; then
    goal="$(awk '/^## Goal$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    next_step="$(awk '/^## Next Step$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    verify_cmd="$(awk '/^## Verify$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    idle="$(days_since "$(printf '%s\n' "$status_line" | sed -n 's/^last_activity: //p')")"
    append "- task $task_id ($task_status${idle:+, idle ${idle}d}): ${goal:-no goal recorded}"
    [ -z "$next_step" ] || append "  next: $next_step"
    [ -z "$verify_cmd" ] || append "  verify: $verify_cmd (oms agent-task verify)"
  fi
fi

# Active plan contract: the goal and its executable acceptance survive a
# compaction the same way the task packet does. A live plan with ready work
# was previously silent at session start — the contract the conversation is
# bound by has to outlive the summary that dropped it.
plan_file="$cwd/.oms/plan/tasks.json"
if [ -s "$plan_file" ]; then
  plan_lines="$(python3 - "$plan_file" <<'PY' 2>/dev/null
import datetime, json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        plan = json.load(fh)
except Exception:
    sys.exit(0)
goal = " ".join(str(plan.get("goal") or "").split())
if not goal:
    sys.exit(0)
tasks = plan.get("tasks") or {}
if isinstance(tasks, dict):
    states = [str(t.get("state") or "?") for t in tasks.values()
              if isinstance(t, dict)]
else:
    states = [str(t.get("state") or "?") for t in tasks if isinstance(t, dict)]
if states and all(state == "done" for state in states):
    sys.exit(0)  # a finished plan is history, not a resumption duty
counts = {}
for state in states:
    counts[state] = counts.get(state, 0) + 1
summary = " ".join("%s=%d" % item for item in sorted(counts.items()))
try:
    newest = max(str(t.get("updated") or "") for t in
                 (tasks.values() if isinstance(tasks, dict) else tasks) if isinstance(t, dict))
    idle = (datetime.datetime.now(datetime.timezone.utc) - datetime.datetime.strptime(
        newest, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)).days
    summary += " idle=%dd" % idle if idle >= 1 else ""
except Exception:
    pass
if len(goal) > 160:
    goal = goal[:157] + "..."
print("- plan: %s%s" % (goal, (" [%s]" % summary) if summary else ""))
accept = " ".join(str(plan.get("accept") or "").split())
if accept:
    if len(accept) > 140:
        accept = accept[:137] + "..."
    print("  accept: %s" % accept)
PY
)" || plan_lines=""
  plan_lines="${plan_lines//$'\r'/}"
  if [ -n "$plan_lines" ]; then
    while IFS= read -r line; do append "$line"; done <<EOF_PLAN
$plan_lines
EOF_PLAN
  fi
fi

# Newest handoff digest, capped at 72h: old enough to survive an overnight
# gap, young enough not to anchor a new week on stale context. Pointer only —
# the digest is one `show` away and inlining it would blow the line budget.
newest_handoff="$(find "$cwd/.oms/handoffs" -maxdepth 1 -name '*.md' -mmin -4320 2>/dev/null |
  xargs -r ls -t 2>/dev/null | head -n 1)"
if [ -n "$newest_handoff" ]; then
  name="$(basename "$newest_handoff")"
  age_h="$(python3 -c '
import os, sys, time
try:
    print(int((time.time() - os.path.getmtime(sys.argv[1])) // 3600))
except Exception:
    print("?")
' "$newest_handoff" 2>/dev/null)" || age_h="?"
  age_h="${age_h//$'\r'/}"
  append "- handoff (${age_h}h old): oms session-handoff show $name"
fi

# Latest imported portable capsule: validated through the runtime projection so
# a stale pointer, tampered digest, or authority-bearing payload never becomes
# resume guidance. This is advisory continuity only; it cannot restore task,
# plan, evidence, approval, lease, or publication authority.
portable_line=""
if [ -f "$cwd/.oms/portable/imports/LATEST" ] &&
  [ ! -L "$cwd/.oms/portable/imports/LATEST" ]; then
  portable_line="$("$ROOT/scripts/runtime.sh" --repo "$cwd" envelope show 2>/dev/null |
    python3 -c '
import json, sys
try:
    row = json.load(sys.stdin).get("continuity", {}).get("latest_import", {})
except Exception:
    raise SystemExit(0)
if row.get("present"):
    print("- portable capsule %s (%s; advisory only, no authority transferred): oms state" % (
        row.get("capsule_id") or "unknown", row.get("status", "unknown")))
' 2>/dev/null)" || portable_line=""
fi
portable_line="${portable_line//$'\r'/}"
[ -z "$portable_line" ] || append "$portable_line"

# Unresolved failures: one line, newest row's summary and suggested next.
ledger="$cwd/.oms/failures.jsonl"
if [ -s "$ledger" ]; then
  fail_line="$("$ROOT/scripts/fail-ledger.sh" --repo "$cwd" list --unresolved --json 2>/dev/null | python3 -c '
import json, sys
try:
    rows = json.load(sys.stdin).get("failures", [])
except Exception:
    raise SystemExit(0)
if not rows:
    raise SystemExit(0)
actionable = [row for row in rows if row.get("actionable") is True]
retiring = sum(1 for row in rows if row.get("retiring") is True)
stale = sum(1 for row in rows if row.get("attention") == "stale")
side = ", ".join(t % n for t, n in (("+%d retiring on TTL", retiring),
                                    ("+%d stale on an older commit", stale)) if n)
if actionable:
    newest = max(actionable, key=lambda row: row.get("ts") or "")
    bits = ["- failures: %d actionable%s" % (len(actionable), " (%s)" % side if side else "")]
    summary = (newest.get("summary") or newest.get("cmd") or "").strip()
    if summary:
        bits.append("latest: %s" % summary[:120])
    nxt = (newest.get("next") or "").strip()
    if nxt:
        bits.append("next: %s" % nxt[:120])
    print("; ".join(bits))
else:
    bits = ["%d one-shot hook failure(s), auto-retire on TTL" % retiring] if retiring else []
    if stale:
        bits.append("%d stale on an older commit (oms fail-ledger list)" % stale)
    print("- failures: " + "; ".join(bits))
' )" || fail_line=""
  fail_line="${fail_line//$'\r'/}"
  [ -z "$fail_line" ] || append "$fail_line"
fi

# Autopilot shadow judgment: when a live outer receipt exists, record what
# the reenter gate would decide right now — observe-only evidence for the
# raise-after-evidence autonomy protocol — and surface the verdict. This is
# the hook's one write (append-only shadow ledger, ambient to the check
# gate); the receipt and the claim ledger stay untouched.
if [ -f "$cwd/.oms/plan/autopilot-run.json" ]; then
  sid="$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
print(data.get("session_id", "") or "")
' 2>/dev/null)" || sid=""
  sid="${sid//$'\r'/}"
  shadow_line="$(bash "$ROOT/scripts/autopilot.sh" --repo "$cwd" shadow \
    ${sid:+--session "$sid"} 2>/dev/null)" || shadow_line=""
  [ -z "$shadow_line" ] || append "- autopilot: $shadow_line"
fi

# Auto-update attention: the session start is where a silently failing or
# stalled daily updater finally meets a human. Shared verdict; ok and
# disabled stay quiet.
au_line="$("$ROOT/scripts/auto-update.sh" attention 2>/dev/null || true)"
case "$au_line" in
  "attention: ok"*|"attention: disabled"*|"") ;;
  *) append "- auto-update ${au_line#attention: }" ;;
esac

# Project graph: `oms graph project find` used to fail in any repository
# nobody had built one in, so the session opens by saying whether this one has
# a current graph — and starts the build itself when it does not. The build is
# detached and its output discarded: session start never waits on it, never
# fails because of it, and an inherited stdout would hang every caller that
# captures this hook. setsid, never nohup — nohup makes SIGHUP unignorable by
# a trap for descendants, which the harness's signal tests depend on. The
# check is bounded well inside the hook's own 10s budget.
graph_state=""
# Only a repository that already uses OMS gets a graph on its own: a session
# opened in a plain directory (or a home directory) must not start a scan.
if [ -f "$ROOT/scripts/graph.sh" ] && [ -d "$cwd/.oms" ] &&
  git -C "$cwd" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  # `check` exits 3 on a stale or absent graph and still prints the verdict,
  # so the status is discarded rather than the payload.
  graph_json=""
  if command -v timeout >/dev/null 2>&1; then
    graph_json="$(timeout 5 bash "$ROOT/scripts/graph.sh" --repo "$cwd" \
      project check --json 2>/dev/null || true)"
  else
    graph_json="$(bash "$ROOT/scripts/graph.sh" --repo "$cwd" \
      project check --json 2>/dev/null || true)"
  fi
  graph_state="$(printf '%s' "$graph_json" | python3 -c '
import json, sys
try:
    row = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
print("fresh %s" % str(row.get("revision") or "")[:12] if row.get("fresh") else "refresh")
' 2>/dev/null)" || graph_state=""
  graph_state="${graph_state//$'\r'/}"
fi
# Execution graph shadow: with a plan present, record where the bundled
# goal-drive graph would stand against current reality and whether that agrees
# with the control plane's own next action. Observe-only evidence, one
# append-only row (ambient to the check gate, like the autopilot shadow);
# never a run, never an action. Bounded well inside the hook's budget.
if [ -f "$ROOT/scripts/graph.sh" ] && [ -f "$cwd/.oms/plan/tasks.json" ] &&
  [ "${OMS_GRAPH_SHADOW:-1}" != "0" ]; then
  route_json=""
  if command -v timeout >/dev/null 2>&1; then
    route_json="$(timeout 8 bash "$ROOT/scripts/graph.sh" --repo "$cwd" \
      exec shadow --json 2>/dev/null || true)"
  else
    route_json="$(bash "$ROOT/scripts/graph.sh" --repo "$cwd" \
      exec shadow --json 2>/dev/null || true)"
  fi
  route_line="$(printf '%s' "$route_json" | python3 -c '
import json, sys
try:
    row = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
primary = row.get("route", {}).get("primary") or row.get("route", {}).get("status") or "-"
plane = row.get("control_plane", {})
verdict = "agrees" if row.get("agree") else "disagrees"
action = plane.get("action")
print("- graph route: %s (%s)" % (
    primary, "%s with runtime next %s" % (verdict, action) if action else "runtime next unavailable"))
' 2>/dev/null)" || route_line=""
  route_line="${route_line//$'\r'/}"
  [ -z "$route_line" ] || append "$route_line"
fi

case "$graph_state" in
  "fresh "*) append "- graph: fresh (${graph_state#fresh })" ;;
  refresh)
    if [ "${OMS_GRAPH_AUTOBUILD:-1}" = "0" ]; then
      append "- graph: absent (OMS_GRAPH_AUTOBUILD=0)"
    else
      if command -v setsid >/dev/null 2>&1; then
        setsid bash "$ROOT/scripts/graph.sh" --repo "$cwd" project ensure \
          > /dev/null 2>&1 < /dev/null &
      else
        ( bash "$ROOT/scripts/graph.sh" --repo "$cwd" project ensure \
          > /dev/null 2>&1 < /dev/null & )
      fi
      append "- graph: building in the background (oms graph project ensure)"
    fi
    ;;
esac

[ -n "$out" ] || exit 0
printf '[oms resume] %s\n%s- more: oms state\n' "$(basename "$cwd")" "$out"
exit 0
