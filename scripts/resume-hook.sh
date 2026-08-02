#!/usr/bin/env bash
set -uo pipefail

# SessionStart hook: print one bounded resume block so a fresh session starts
# knowing what this repo was doing — active task packet (goal, next step,
# verify), newest handoff digest, unresolved failures, and whether another
# live session is using the same worktree. Everything here is read from .oms
# state that was scrubbed at write time; nothing is captured or mutated.
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
task_file="$cwd/.oms/task/current.md"
if [ -f "$task_file" ]; then
  status_line="$(cd "$cwd" && "$ROOT/scripts/agent-task.sh" status 2>/dev/null)" || status_line=""
  task_id="$(printf '%s\n' "$status_line" | sed -n 's/^task_id: //p')"
  task_status="$(printf '%s\n' "$status_line" | sed -n 's/^status: //p')"
  if [ -n "$task_id" ] && [ "$task_status" != "closed" ]; then
    goal="$(awk '/^## Goal$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    next_step="$(awk '/^## Next Step$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    verify_cmd="$(awk '/^## Verify$/{f=1;next} /^## /{f=0} f&&NF{print;exit}' "$task_file" 2>/dev/null)"
    append "- task $task_id ($task_status)${goal:+: $goal}"
    [ -z "$next_step" ] || append "  next: $next_step"
    [ -z "$verify_cmd" ] || append "  verify: $verify_cmd (oms agent-task verify)"
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
  append "- handoff (${age_h}h old): oms session-handoff show $name"
fi

# Unresolved failures: one line, newest row's summary and suggested next.
ledger="$cwd/.oms/failures.jsonl"
if [ -s "$ledger" ]; then
  fail_line="$(python3 - "$ledger" <<'PY' 2>/dev/null
import json, sys
rows = {}
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        for line in fh:
            try:
                row = json.loads(line)
            except Exception:
                continue
            fp = row.get("fingerprint")
            if not fp:
                continue
            # The ledger names its resolution event "resolved"; legacy rows
            # may carry a resolved flag instead (same reading as skill-router).
            if row.get("event") == "resolved" or row.get("resolved") is True:
                rows.pop(fp, None)
            else:
                rows[fp] = row
except OSError:
    sys.exit(0)
if not rows:
    sys.exit(0)
newest = max(rows.values(), key=lambda r: r.get("ts") or "")
bits = ["- failures: %d unresolved" % len(rows)]
summary = (newest.get("summary") or newest.get("cmd") or "").strip()
if summary:
    bits.append("latest: %s" % summary[:120])
nxt = (newest.get("next") or "").strip()
if nxt:
    bits.append("next: %s" % nxt[:120])
print("; ".join(bits))
PY
)" || fail_line=""
  [ -z "$fail_line" ] || append "$fail_line"
fi

# Peer-session advisory: another session hash with hook events in this cwd in
# the last 15 minutes means a live neighbor. The incident this prevents:
# `git add <file>` from a shared dirty tree committing the neighbor's hunks.
events="$cwd/.oms/hooks/events.jsonl"
if [ -s "$events" ]; then
  peer_line="$(python3 - "$events" "$payload" <<'PY' 2>/dev/null
import hashlib, json, sys, time
from datetime import datetime, timezone

try:
    payload = json.loads(sys.argv[2]) if sys.argv[2].strip() else {}
except Exception:
    payload = {}
# Mirrors hook_state.py session_hash exactly, fallback included, so our own
# rows are always recognized as self.
sid = str(payload.get("session_id") or payload.get("sessionId") or "nosession")
me = hashlib.sha256(sid.encode()).hexdigest()[:32]
now = time.time()
newest = {}
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        lines = fh.readlines()[-200:]
except OSError:
    sys.exit(0)
for line in lines:
    try:
        row = json.loads(line)
    except Exception:
        continue
    session = row.get("session") or ""
    ts = row.get("ts") or ""
    if not session or session == me:
        continue
    try:
        when = datetime.strptime(ts, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()
    except Exception:
        continue
    if now - when <= 900:
        newest[session] = max(newest.get(session, 0), when)
if newest:
    minutes = int((now - max(newest.values())) // 60)
    print("- peers: another session used this worktree %dm ago — dirty-tree `git add`/`commit` can pick up its hunks; use `git add -p` or a worktree" % minutes)
PY
)" || peer_line=""
  [ -z "$peer_line" ] || append "$peer_line"
fi

[ -n "$out" ] || exit 0
printf '[oms resume] %s\n%s- more: oms repo-state\n' "$(basename "$cwd")" "$out"
exit 0
