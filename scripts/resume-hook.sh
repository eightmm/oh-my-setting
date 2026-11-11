#!/usr/bin/env bash
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo "error: oh-my-setting needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS run: brew install bash, then rerun" >&2; exit 1; }
set -uo pipefail

# SessionStart hook: print one bounded resume block so a fresh session starts
# knowing what this repo was doing — active task packet (goal, next step,
# verify), newest handoff digest, and unresolved failures. Everything here is
# read from .oms state that was scrubbed at write time. Its writes are the
# regenerable autopilot shadow-judgment row, which is ambient to the check
# gate, one content-free injection-ledger row (byte and line counts), and one
# advisory pickup row (handoff name, session id, time) in
# .oms/handoffs/consumed.jsonl when the handoff line was injected; receipts, claims, and every other surface stay untouched (the digest's
# `git merge-tree --write-tree` adds only unreferenced loose objects).
# Best-effort by contract: a hook that blocks session start costs more than a
# missing resume line, so every failure path exits 0. OMS_RESUME_HOOK=0
# disables; harness children stay silent (their parent already has context).
# OMS_RESUME_MEMORY=1 adds one bounded shared-memory section; that opt-in read
# goes through the memory index, which may refresh its derived database and
# recall receipts.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

[ "${OMS_RESUME_HOOK:-1}" = "1" ] || exit 0
[ "${OMS_HARNESS_CHILD:-0}" != "1" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"
# Only a harness-adopted repo has state worth resuming; a random directory
# must not produce noise (or a .oms tree) because a session started there.
cwd="$(OMS_STATE_REPO='' OMS_HOOK_PAYLOAD="$payload" python3 -X utf8 "$ROOT/scripts/lib/hook_repo.py" 2>/dev/null | tr -d '\r')" || exit 0
[ -n "$cwd" ] && cwd="$(cd "$cwd" 2>/dev/null && pwd -P)" && [ -d "$cwd/.oms" ] || exit 0

out=""
append() { out="${out}${1}
"; }

# Active task packet: id/status from the status front door, goal and next
# step straight from the packet sections. A fresh active task was previously
# completely silent at session start — that is the gap this closes.
days_since() {  # days_since ISO8601Z -> whole days when >= 1, else empty
  python3 -X utf8 -I -B - "$1" <<'PY' 2>/dev/null
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
  plan_lines="$(python3 -X utf8 -I -B - "$plan_file" <<'PY' 2>/dev/null
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
if states and all(state in ("done", "cancelled") for state in states):
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

# The land lock is shared across worktrees; its holder is not identified here.
land_line="$(python3 -X utf8 -I -B - "$ROOT/scripts/land.sh" "$cwd" <<'PYLAND' 2>/dev/null
import json, subprocess, sys, tempfile
try:
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(["bash", sys.argv[1], "status", "--repo", sys.argv[2],
                                 "--json", "--active-exit"],
                                stdin=subprocess.DEVNULL, stdout=output,
                                stderr=subprocess.DEVNULL, timeout=2)
        if result.returncode not in (0, 3):
            raise ValueError("status failed")
        output.seek(0)
        row = json.loads(output.read(65537))
    if row.get("active") is not True:
        raise ValueError("inactive")
    print("- land lock held (possibly another worktree); oms land status")
except (OSError, ValueError, TypeError, AttributeError, subprocess.SubprocessError):
    pass
PYLAND
)" || land_line=""
land_line="${land_line//$'\r'/}"
[ -z "$land_line" ] || append "$land_line"

# Prefer the current task over recency. Legacy/other-task digests stay readable,
# but never silently become the current task's continuation.
handoff_line="$(OMS_HOOK_PAYLOAD="$payload" python3 -X utf8 -I -B - "$cwd" "${task_id:-}" "${task_status:-}" "$ROOT/scripts/lib" <<'PY'
import hashlib, os, pathlib, re, shlex, subprocess, sys, time
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
    # A digest an earlier session was already shown stays visible but is no
    # longer presented as fresh. Advisory: any failure leaves the plain line.
    picked = ''
    try:
        sys.path.insert(0, sys.argv[4])
        import handoff_consumed
        first = handoff_consumed.pickups(str(repo)).get(best[3])
        if first and first['session'] != handoff_consumed.payload_session(os.environ.get('OMS_HOOK_PAYLOAD', '')):
            picked = '; already picked up by %s at %s' % (first['session'], first['ts'])
    except Exception:
        picked = ''
    print('- handoff (%s; %dh old%s): oms session-handoff show %s' % (best[4], max(0, int((time.time() - best[2]) // 3600)), picked, shlex.quote(best[3])))
    print(best[3])
PY
)" || handoff_line=""
handoff_line="${handoff_line//$'\r'/}"
handoff_name=""
case "$handoff_line" in
  *$'\n'*) handoff_name="${handoff_line#*$'\n'}"; handoff_line="${handoff_line%%$'\n'*}" ;;
esac
[ -z "$handoff_line" ] || append "$handoff_line"

# Latest imported portable capsule: validated through the runtime projection so
# a stale pointer, tampered digest, or authority-bearing payload never becomes
# resume guidance. This is advisory continuity only; it cannot restore task,
# plan, evidence, approval, lease, or publication authority.
portable_line=""
if [ -f "$cwd/.oms/portable/imports/LATEST" ] &&
  [ ! -L "$cwd/.oms/portable/imports/LATEST" ]; then
  portable_line="$("$ROOT/scripts/runtime.sh" --repo "$cwd" envelope show 2>/dev/null |
    python3 -X utf8 -I -B -c '
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

# Previous room: a reopened panel gets a fresh room, so the last main's closing
# words in the most recently written other room are shown (capped, read-only).
# Sealed rooms are skipped: their successor carries the conversation.
if [ -n "${OMS_ROOM_ID:-}" ] && [ -d "$cwd/.oms/threads" ]; then
  room_lines="$(python3 -X utf8 -I -B - "$ROOT/scripts/lib" "$cwd" "$OMS_ROOM_ID" <<'PYROOM' 2>/dev/null
import os, re, sys, time
try:
    sys.path.insert(0, sys.argv[1])
    import room
    repo, current = sys.argv[2], sys.argv[3]
    folder = os.path.join(repo, ".oms/threads")
    deadline = time.monotonic() + 1.5
    found = []
    with os.scandir(folder) as entries:
        for index, entry in enumerate(entries):
            if index >= room.MAX_SCAN_ENTRIES:
                break
            ident = entry.name[:-6]
            if entry.name.endswith(".jsonl") and ident != current and entry.is_file(follow_symlinks=False):
                found.append((entry.stat(follow_symlinks=False).st_mtime, ident))
    found.sort(reverse=True)
    for stamp, ident in found[:5]:
        if time.monotonic() > deadline:
            break
        try:
            state = room.project(room.records(repo, ident))
        except Exception:
            continue
        if state.get("successor"):
            continue
        mains = {p["participant"] for p in state["participants"] if p.get("role") == "main"}
        texts = [m for m in state["messages"] if m.get("sender") in mains and str(m.get("text") or "").strip()]
        lines = ["- previous room %s (last active %s): oms room show --id %s" % (
            ident, time.strftime("%Y-%m-%d %H:%MZ", time.gmtime(stamp)), ident)]
        for message in texts[-2:]:
            text = " ".join(str(message["text"]).split())
            text = re.sub(r"(?i)\b([\w.-]*(?:token|secret|passw(?:or)?d|api[_-]?key|credential)[\w.-]*)\s*[=:]\s*\S+", r"\1=[redacted]", text)
            text = re.sub(r"(?<![\w.~$-])(?:/(?:home|Users|root)/|[A-Za-z]:\\Users\\)\S*", "[path]", text)
            raw = text.encode("utf-8")
            if len(raw) > 300:
                text = raw[:297].decode("utf-8", errors="ignore") + "..."
            lines.append("  main: " + text)
        print("\n".join(lines))
        break
except Exception:
    pass
PYROOM
)" || room_lines=""
  room_lines="${room_lines//$'\r'/}"
  if [ -n "$room_lines" ]; then
    while IFS= read -r line; do append "$line"; done <<EOF_ROOM
$room_lines
EOF_ROOM
  fi
fi

# Unpublished work: commits on this branch (and other registered worktrees'
# branches) that no remote has. A branch without an upstream is measured
# against the remote default branch, which the upstream count cannot see.
# Counts and branch names only; every git call is time-bounded.
ahead_line="$(python3 -X utf8 -I -B - "$cwd" <<'PYAHEAD' 2>/dev/null
import subprocess, sys
repo = sys.argv[1]
def git(*args):
    try:
        done = subprocess.run(["git", "-C", repo, *args], stdin=subprocess.DEVNULL, capture_output=True,
                              text=True, timeout=2)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() if done.returncode == 0 else None
try:
    if not git("remote"):
        raise SystemExit(0)
    default = (git("symbolic-ref", "--short", "refs/remotes/origin/HEAD")
               or next((n for n in ("origin/main", "origin/master")
                        if git("rev-parse", "--verify", "-q", "refs/remotes/" + n)), None))
    def ahead(branch):
        base = git("rev-parse", "--abbrev-ref", branch + "@{u}") or default
        # Patch-id equivalence drops commits that landed under another SHA (regraft,
        # cherry-pick); merges have no patch-id, so a merge-only branch counts only
        # when its merged tree differs from the base tree. Without merge-tree
        # (git < 2.38) or on conflict, merges count: never hides work.
        if not base:
            return None
        rng = "%s...refs/heads/%s" % (base, branch)
        count = git("rev-list", "--cherry-pick", "--right-only", "--count", rng)
        if not (count and count.isdigit() and int(count)):
            return None
        if git("rev-list", "--cherry-pick", "--right-only", "--no-merges", "--count", rng) == "0":
            merged = git("merge-tree", "--write-tree", base, "refs/heads/" + branch)
            if merged and merged.split()[0] == git("rev-parse", base + "^{tree}"):
                return None
        return (int(count), base)
    here = git("symbolic-ref", "--short", "HEAD")
    parts, seen = [], set()
    if here:
        seen.add(here)
        row = ahead(here)
        if row:
            parts.append("%d commit(s) on %s not on %s" % (row[0], here, row[1]))
    others = []
    for line in (git("worktree", "list", "--porcelain") or "").splitlines():
        name = line[len("branch refs/heads/"):] if line.startswith("branch refs/heads/") else ""
        if name and name not in seen:
            seen.add(name)
            row = ahead(name)
            if row:
                others.append("%s(%d)" % (name, row[0]))
    if others:
        parts.append("other worktrees: " + ", ".join(others[:8]) + (" +%d more" % (len(others) - 8) if len(others) > 8 else ""))
    if parts:
        print("- unpublished: " + "; ".join(parts))
except Exception:
    pass
PYAHEAD
)" || ahead_line=""
ahead_line="${ahead_line//$'\r'/}"
[ -z "$ahead_line" ] || append "$ahead_line"

# Unresolved failures: one line, newest row's summary and suggested next.
ledger="$cwd/.oms/failures.jsonl"
if [ -s "$ledger" ]; then
  fail_line="$("$ROOT/scripts/fail-ledger.sh" --repo "$cwd" list --unresolved --json 2>/dev/null | python3 -X utf8 -I -B -c '
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
retiring_row = next((row for row in rows if row.get("retiring") is True), None)
stale_row = next((row for row in rows if row.get("attention") == "stale"), None)
side = ", ".join(t % (n, row.get("fingerprint", "unknown")) for t, n, row in
                  (("+%d retiring on TTL (fp: %s)", retiring, retiring_row),
                   ("+%d stale on an older commit (fp: %s)", stale, stale_row)) if n)
if actionable:
    newest = max(actionable, key=lambda row: row.get("ts") or "")
    bits = ["- failures: %d actionable%s" % (len(actionable), " (%s)" % side if side else "")]
    fp = newest.get("fingerprint")
    summary = (newest.get("summary") or newest.get("cmd") or "").strip()
    if summary:
        bits.append("latest: %s" % (summary[:119] + "…" if len(summary) > 120 else summary))
    if fp:
        bits.append("fp: %s (oms fail-ledger show --fingerprint %s)" % (fp, fp))
    nxt = (newest.get("next") or "").strip()
    if nxt:
        bits.append("next: %s" % (nxt[:119] + "…" if len(nxt) > 120 else nxt))
    print("; ".join(bits))
else:
    bits = ["%d one-shot hook failure(s), auto-retire on TTL (fp: %s)" %
            (retiring, retiring_row.get("fingerprint", "unknown"))] if retiring else []
    if stale:
        bits.append("%d stale on an older commit (fp: %s; oms fail-ledger list --unresolved)" %
                    (stale, stale_row.get("fingerprint", "unknown")))
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
  sid="$(printf '%s' "$payload" | python3 -X utf8 -I -B -c '
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

relay_line="$(printf '%s' "$payload" | python3 -X utf8 "$ROOT/scripts/lib/cached-main.py" "$ROOT/scripts/lib/hook_state.py" relay-hint --nested 2>/dev/null)" || relay_line=""
[ -z "$relay_line" ] || append "${relay_line//$'\r'/}"

# After a compaction the provider's summary replaces the conversation, and
# Claude Code does not list skill descriptions again. The task, plan, handoff
# and failure lines above are re-read from .oms state on every SessionStart,
# so they survive by construction; this line restores the one routing fact the
# summary cannot be trusted to keep. It rides an existing block only: a
# repository with nothing to resume stays silent after a compaction too.
session_source="$(printf '%s' "$payload" | python3 -X utf8 -I -B -c '
import json, sys
try:
    value = json.load(sys.stdin).get("source")
except Exception:
    value = ""
print(value if value in ("startup", "resume", "clear", "compact", "fork") else "")
' 2>/dev/null)" || session_source=""
session_source="${session_source//$'\r'/}"
if [ "$session_source" = compact ] && [ -n "$out" ]; then
  append "- after compaction: the lines above were re-read from .oms state, not from the summary; OMS skills are invoked by name (/oms-<skill>), and oms state shows the rest"
fi

# Shared memory, opt-in (OMS_RESUME_MEMORY=1). The project writes pins and
# notes all day, but only peer calls and delegations ever read them back; the
# main session had to remember to ask. This puts the newest pins and, when an
# active task names a goal, the notes ranked for that goal at the end of the
# block, where the block's own byte cap cuts them first. Everything comes
# through the memory library: its sensitive-content scan withholds a pins file
# or a recall result as it does for provider context, and recall drops notes
# whose cited source no longer verifies. No memory, a withheld read, or a
# lookup slower than its bound leaves the block as it was.
if [ "${OMS_RESUME_MEMORY:-0}" = "1" ]; then
  memory_lines="$(python3 -X utf8 -I -B - "$BASH" "$ROOT/scripts/lib/agent-memory-common.sh" \
    "$cwd" "${goal:-}" "$ROOT/scripts/lib/agent-memory-db.py" <<'PYMEM' 2>/dev/null
import importlib.util, json, os, signal, subprocess, sys, tempfile
shell, library, repo, goal, helper = sys.argv[1:6]
PINS, HITS = 3, 3          # newest pins, ranked notes
LINE, CAP = 160, 1200      # bytes of one note's text, bytes of the whole section
WAIT, READ = 2, 131072     # seconds for the lookup, bytes read back from it
LOOKUP = r"""
. "$1" 2>/dev/null || exit 0
file="$(agent_memory_project_file "$2" 2>/dev/null)" || exit 0
pins="$(agent_memory_pins_file "$file")"
if [ -s "$pins" ] && ! agent_memory_file_has_sensitive_content "$pins" 2>/dev/null; then
  printf '\036pins\n'
  cat "$pins"
  printf '\n'
fi
hits="$OMS_LIB_TMPDIR/hits"
if [ -n "$3" ] && [ -s "$file" ] &&
  python3 "$5" recall --db "$OMS_LIB_TMPDIR/memory.sqlite3" --shared "$file" \
    --pins "" --repo "$2" --query "$3" --limit "$4" --json > "$hits" 2>/dev/null &&
  [ -s "$hits" ] && ! agent_memory_file_has_sensitive_content "$hits" 2>/dev/null; then
  printf '\036recall\n'
  cat "$hits"
fi
"""

def one_line(text):
    text = " ".join("".join(ch if ch.isprintable() else " " for ch in text).split())
    raw = text.encode("utf-8")
    if len(raw) > LINE:
        text = raw[:LINE - 3].decode("utf-8", errors="ignore") + "..."
    return text

try:
    query = " ".join(goal.split())[:300]
    with tempfile.TemporaryDirectory(prefix="oms-resume-memory.") as scratch:
        env = dict(os.environ, OMS_LIB_TMPDIR=scratch)
        with open(os.path.join(scratch, "out"), "w+b") as output:
            lookup = subprocess.Popen(
                [shell, "-c", LOOKUP, "oms-resume-memory", library, repo, query, str(HITS), helper],
                stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.DEVNULL,
                env=env, start_new_session=True)
            try:
                lookup.wait(timeout=WAIT)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(lookup.pid, signal.SIGKILL)
                except OSError:
                    pass
                lookup.wait()
                raise SystemExit(0)
            output.seek(0)
            text = output.read(READ + 1)
    if len(text) > READ:
        raise SystemExit(0)
    parts = {}
    for chunk in text.decode("utf-8", errors="replace").split("\x1e")[1:]:
        name, _, body = chunk.partition("\n")
        parts[name] = body

    spec = importlib.util.spec_from_file_location("oms_resume_memory_db", helper)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    pins = [(entry.event_id, one_line(entry.body))
            for entry in module.parse_pins(parts.get("pins", ""))
            if one_line(entry.body)][-PINS:]
    shown = {event for event, _ in pins}
    hits = []
    for line in parts.get("recall", "").splitlines():
        entry = json.loads(line)
        event, note = entry["event_id"], one_line(entry["body"])
        if entry["source"] == "shared" and note and event not in shown:
            shown.add(event)
            hits.append((event, note))

    rows = (["  pin %s: %s" % (event[:48], note) if event else "  pin: %s" % note for event, note in pins]
            + ["  recall %s: %s" % (event[:48], note) for event, note in hits[:HITS]])
    head = "- memory (reference only; not approval or verification evidence):"
    more = "  more: oms agent-memory recall QUERY; oms agent-memory show"
    while rows and len("\n".join([head, *rows, more]).encode("utf-8")) + 1 > CAP:
        rows.pop()
    if rows:
        print("\n".join([head, *rows, more]))
except Exception:
    pass
PYMEM
)" || memory_lines=""
  memory_lines="${memory_lines//$'\r'/}"
  if [ -n "$memory_lines" ]; then
    while IFS= read -r line; do append "$line"; done <<EOF_MEMORY
$memory_lines
EOF_MEMORY
  fi
fi

[ -n "$out" ] || exit 0
block="$(printf '[oms resume] %s\n%s' "$(basename "$cwd")" "$out" |
  python3 -X utf8 -I -B -c '
import os, sys
cap = 4096
text = sys.stdin.read().replace("\r", "")
footer = "- more: oms state\n"
raw = text.encode("utf-8")
if len(raw) + len(footer.encode()) > cap:
    footer = "\n[resume truncated; inspect current state]\n" + footer
    text = raw[:cap - len(footer.encode())].decode("utf-8", errors="ignore")
sys.stdout.buffer.write((text + footer).encode("utf-8"))
' | tr -d '\r')" || exit 0
[ -n "$block" ] || exit 0
printf '%s\n' "$block"
# Injection ledger: how many bytes this block put into the session, never the
# block. One content-free row in .oms/hooks/events.jsonl; best-effort.
block_bytes="$(printf '%s\n' "$block" | wc -c | tr -d '[:space:]')"
block_lines="$(printf '%s\n' "$block" | wc -l | tr -d '[:space:]')"
printf '%s' "$payload" | python3 -X utf8 "$ROOT/scripts/lib/cached-main.py" "$ROOT/scripts/lib/hook_state.py" \
  injected --kind resume --bytes "${block_bytes:-0}" --lines "${block_lines:-0}" >/dev/null 2>&1 || true
# Pickup marker: only when the handoff line survived the byte cap and was
# really printed. Best-effort; resume never fails on it.
if [ -n "$handoff_name" ] && [ -n "$handoff_line" ]; then
  case "$block" in
    *"$handoff_line"*)
      printf '%s' "$payload" | python3 -X utf8 -I -B "$ROOT/scripts/lib/handoff_consumed.py" \
        --repo "$cwd" --name "$handoff_name" >/dev/null 2>&1 || true ;;
  esac
fi
exit 0
