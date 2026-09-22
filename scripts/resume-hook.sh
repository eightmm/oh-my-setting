#!/usr/bin/env bash
set -uo pipefail

# SessionStart hook: print one bounded resume block so a fresh session starts
# knowing what this repo was doing — active task packet (goal, next step,
# verify), newest handoff digest, and unresolved failures. Everything here is
# read from .oms state that was scrubbed at write time. Its only write is the
# regenerable autopilot shadow-judgment row, which is ambient to the check
# gate; receipts, claims, and every other surface stay untouched.
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
repo="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null)" || repo=""
repo="${repo//$'\r'/}"
[ -z "$repo" ] || cwd="$repo"
cwd="$(cd "$cwd" && pwd -P)" || exit 0
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

# Prefer the current task over recency. Legacy/other-task digests stay readable,
# but never silently become the current task's continuation.
handoff_line="$(python3 - "$cwd" "${task_id:-}" "${task_status:-}" <<'PY'
import hashlib, pathlib, re, shlex, subprocess, sys, time
repo = pathlib.Path(sys.argv[1])
active = sys.argv[2] if sys.argv[3] != 'closed' else ''
root = repo / '.oms/handoffs'
if any(p.is_symlink() for p in (root, *root.parents)):
    raise SystemExit(0)
try:
    head = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], stderr=subprocess.DEVNULL, text=True).strip()
except (OSError, subprocess.CalledProcessError):
    head = ''
task_digest = ''
task = repo / '.oms/task/current.md'
try:
    if not any(p.is_symlink() for p in (task, *task.parents)):
        with task.open('rb') as handle:
            raw = handle.read(65537)
        if len(raw) <= 65536:
            task_digest = hashlib.sha256(raw).hexdigest()
except OSError:
    pass
best = None
for path in root.glob('*.md'):
    try:
        if path.is_symlink() or not path.is_file():
            continue
        stamp = path.stat().st_mtime
        if time.time() - stamp > 72 * 3600:
            continue
        with path.open(encoding='utf-8') as handle:
            header = handle.read(4096)
        meta = dict(re.findall(r'^- (task_id|head|task_digest): ([^\r\n]+)$', header, re.M))
        matched = bool(active and meta.get('task_id') == active)
        current = matched and bool(head) and meta.get('head') == head and meta.get('task_digest') == task_digest
        label = 'current task snapshot; recheck source' if current else 'same task, recheck changed state' if matched else 'historical/unbound reference'
        candidate = (int(matched), int(current), stamp, path.name, label)
        if best is None or candidate[:4] > best[:4]:
            best = candidate
    except (OSError, ValueError):
        continue
if best:
    print('- handoff (%s; %dh old): oms session-handoff show %s' % (best[4], max(0, int((time.time() - best[2]) // 3600)), shlex.quote(best[3])))
PY
)" || handoff_line=""
handoff_line="${handoff_line//$'\r'/}"
[ -z "$handoff_line" ] || append "$handoff_line"

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

[ -n "$out" ] || exit 0
printf '[oms resume] %s\n%s' "$(basename "$cwd")" "$out" |
  python3 -c '
import os, sys
try:
    cap = max(512, min(16384, int(os.environ.get("OMS_RESUME_MAX_BYTES", "4096"))))
except ValueError:
    cap = 4096
text = sys.stdin.read().replace("\r", "")
footer = "- more: oms state\n"
raw = text.encode("utf-8")
if len(raw) + len(footer.encode()) > cap:
    footer = "\n[resume truncated; inspect current state]\n" + footer
    text = raw[:cap - len(footer.encode())].decode("utf-8", errors="ignore")
sys.stdout.buffer.write((text + footer).encode("utf-8"))
' | tr -d '\r'
exit 0
