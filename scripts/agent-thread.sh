#!/usr/bin/env bash
set -euo pipefail

# Durable multi-turn conversations between agents. Every cross-agent call the
# harness had was one-shot: the peer answered, the answer went into an artifact,
# and the next call started from nothing — so agents could not actually exchange
# context, only fire isolated questions. A thread is an append-only JSONL
# transcript in .oms/threads/<id>.jsonl that any provider can read and extend,
# so codex, claude, and antigravity build on each other's answers across calls
# and across sessions. Turns store a bounded, scrubbed excerpt plus the artifact
# path for the full text; sensitive content is refused like memory and ledger
# rows.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/agent-task-common.sh
. "$ROOT/scripts/lib/agent-task-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

REPO="$PWD"
ACTION=""
THREAD_ID=""
TOPIC=""
ROLE="note"
TEXT=""
TEXT_FILE=""
PROVIDER=""
MODEL=""
ARTIFACT=""
QUALITY=""
SUMMARY=""
MAX_BYTES="${OMS_THREAD_CONTEXT_BYTES:-6000}"
MAX_TURNS="${OMS_THREAD_CONTEXT_TURNS:-12}"
AS_JSON=0
ALL=0
STALE=0

usage() {
  cat <<'EOF'
Usage: agent-thread.sh new     [--id ID] [--topic TEXT] [--repo PATH]
       agent-thread.sh append  [--id ID] --role ROLE (--text TEXT | --text-file F)
                               [--provider P] [--model M] [--artifact PATH]
       agent-thread.sh context [--id ID] [--max-bytes N] [--turns N]
       agent-thread.sh show    [--id ID] [--json]
       agent-thread.sh list    [--all] [--stale] [--json]
       agent-thread.sh stats   [--json]
       agent-thread.sh current
       agent-thread.sh close   [--id ID] [--summary TEXT]

A shared conversation any agent CLI can join. Turns live in
.oms/threads/<id>.jsonl (append-only); `context` renders the recent turns for
injection into a peer prompt, so the next provider answers with everything the
previous ones already said.

Commands:
new      Create a thread and make it current. Prints the thread id.
append   Add one turn. Roles: question, answer, note, decision, summary.
context  Print the recent transcript, oldest first, within the byte budget.
show     Print the full thread (or --json for the raw rows).
list     Open threads, newest first; --all includes closed ones, --stale keeps
         only the abandoned ones and prints the close command for each.
stats    Mechanical counts over every thread, open and closed: follow-up
         questions, multi-provider threads, recorded answer quality.
current  Print the active thread id, or exit 1 when there is none.
close    Mark the thread closed with an optional summary turn.

Options:
  --repo PATH     Repo holding the state (default: PWD, git-root anchored).
  --id ID         Thread id (default: the current thread).
  --topic TEXT    What the thread is about (new).
  --role ROLE     Turn role (append).
  --text TEXT     Turn text (append).
  --text-file F   Read turn text from a file (append).
  --provider P    Provider that produced the turn (append).
  --model M       Model that produced the turn (append).
  --artifact PATH Full-output artifact for this turn (append).
  --quality Q     ok, thin, empty, or blocked: whether the peer actually
                  answered (append); blocked means the CLI printed only its own
                  reason for doing nothing (denied tool, no login). Non-ok turns
                  are marked in context and show.
  --summary TEXT  Closing summary (close).
  --max-bytes N   Context byte budget (default: 6000, OMS_THREAD_CONTEXT_BYTES).
  --turns N       Context turn limit (default: 12, OMS_THREAD_CONTEXT_TURNS).
  --all           list: include closed threads.
  --stale         list: only stale open threads, each with its close command.
  --json          Machine-readable output (show, list, stats).
  -h, --help      Show help.

A stale current pointer expires after OMS_THREAD_CURRENT_TTL seconds
(default 86400), and a pointer written under a different active task is not
reused, so a new consult never silently joins last week's — or another task's —
conversation.

An open thread that is not current counts as stale once its last valid turn is
older than OMS_THREAD_STALE_TTL seconds (default 259200, three days). Turns
whose timestamp will not parse leave the thread `unknown`, never stale.
Staleness is advisory: nothing here closes, deletes, or rewrites a thread.
An open thread is the only record that an exchange happened and there is no
reopen path, so closing one stays a deliberate call — `--stale` only prints
the command you would run.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    new|append|context|show|list|stats|current|close)
      [ -z "$ACTION" ] || fail "only one command allowed"
      ACTION="$1"; shift ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires a value"; THREAD_ID="$2"; shift 2 ;;
    --topic) [ "$#" -ge 2 ] || fail "--topic requires a value"; TOPIC="$2"; shift 2 ;;
    --role) [ "$#" -ge 2 ] || fail "--role requires a value"; ROLE="$2"; shift 2 ;;
    --text) [ "$#" -ge 2 ] || fail "--text requires a value"; TEXT="$2"; shift 2 ;;
    --text-file) [ "$#" -ge 2 ] || fail "--text-file requires a path"; TEXT_FILE="$2"; shift 2 ;;
    --provider) [ "$#" -ge 2 ] || fail "--provider requires a value"; PROVIDER="$2"; shift 2 ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires a value"; MODEL="$2"; shift 2 ;;
    --artifact) [ "$#" -ge 2 ] || fail "--artifact requires a path"; ARTIFACT="$2"; shift 2 ;;
    --quality)
      [ "$#" -ge 2 ] || fail "--quality requires a value"
      case "$2" in
        ok|thin|empty|blocked) QUALITY="$2" ;;
        *) fail "--quality must be ok, thin, empty, or blocked" ;;
      esac
      shift 2 ;;
    --summary) [ "$#" -ge 2 ] || fail "--summary requires a value"; SUMMARY="$2"; shift 2 ;;
    --max-bytes) [ "$#" -ge 2 ] || fail "--max-bytes requires a value"; MAX_BYTES="$2"; shift 2 ;;
    --turns) [ "$#" -ge 2 ] || fail "--turns requires a value"; MAX_TURNS="$2"; shift 2 ;;
    --all) ALL=1; shift ;;
    --stale) STALE=1; shift ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"
[ "$STALE" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--stale applies to list"
case "$MAX_BYTES" in *[!0-9]*|"") fail "--max-bytes must be a positive integer" ;; esac
case "$MAX_TURNS" in *[!0-9]*|"") fail "--turns must be a positive integer" ;; esac
[ "$MAX_BYTES" -gt 0 ] || fail "--max-bytes must be a positive integer"
[ "$MAX_TURNS" -gt 0 ] || fail "--turns must be a positive integer"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
THREADS="$STATE_ROOT/.oms/threads"
CURRENT="$THREADS/CURRENT"
TTL="${OMS_THREAD_CURRENT_TTL:-86400}"
case "$TTL" in *[!0-9]*|"") TTL=86400 ;; esac
STALE_TTL="${OMS_THREAD_STALE_TTL:-259200}"
case "$STALE_TTL" in *[!0-9]*|"") STALE_TTL=259200 ;; esac

thread_file() { printf '%s/%s.jsonl\n' "$THREADS" "$1"; }

# The pointer is advisory: an expired or dangling one is simply not current.
read_current() {
  local id minted now owner
  [ -f "$CURRENT" ] || return 1
  id="$(awk 'NR==1{print $1}' "$CURRENT")"
  minted="$(awk 'NR==1{print $2}' "$CURRENT")"
  owner="$(awk 'NR==1{print $3}' "$CURRENT")"
  [ -n "$id" ] || return 1
  # A pointer left by another task is not this task's conversation. Pointers
  # written before this field existed carry no owner and stay usable.
  if [ -n "$owner" ] && [ "$owner" != "$(current_task_id)" ]; then
    return 1
  fi
  valid_id "$id" || return 1
  [ -f "$(thread_file "$id")" ] || return 1
  case "$minted" in *[!0-9]*|"") return 1 ;; esac
  now="$(date +%s)"
  [ $((now - minted)) -le "$TTL" ] || return 1
  printf '%s\n' "$id"
}

# The pointer records which task it belongs to: a thread is continuity for the
# work that opened it, and inheriting last task's conversation would inject
# unrelated context into a new one.
current_task_id() {
  agent_task_metadata_value "$STATE_ROOT/.oms/task/current.md" task_id 2>/dev/null || printf ''
}

write_current() {
  local tmp
  mkdir -p "$THREADS"
  agent_memory_ensure_oms_ignore "$STATE_ROOT" 2>/dev/null || true
  tmp="$(mktemp "$THREADS/.CURRENT.XXXXXX")" || fail "mktemp failed"
  printf '%s %s %s\n' "$1" "$(date +%s)" "$(current_task_id)" > "$tmp"
  mv "$tmp" "$CURRENT"
}

valid_id() {
  case "$1" in
    ""|*[!A-Za-z0-9._-]*) return 1 ;;
    .*) return 1 ;;
  esac
  return 0
}

# Validated here, not inside resolve_id: a `fail` in a command substitution
# only kills the subshell, so the caller would report the wrong reason.
if [ -n "$THREAD_ID" ] && ! valid_id "$THREAD_ID"; then
  fail "invalid thread id: $THREAD_ID (use letters, digits, dot, dash, underscore)"
fi

resolve_id() {
  if [ -n "$THREAD_ID" ]; then
    printf '%s\n' "$THREAD_ID"
    return 0
  fi
  read_current
}

require_thread() {
  local id
  id="$(resolve_id)" ||
    fail "no thread selected: pass --id, or start one with 'agent-thread new'"
  [ -f "$(thread_file "$id")" ] ||
    fail "unknown thread: $id (agent-thread list)"
  printf '%s\n' "$id"
}

# The sequence number is read from the file and then written back, so two
# panel members answering in parallel would both read the same maximum and both
# claim it. Serialize the read-and-append the way every other shared writer in
# the harness does.
append_row() {
  local id="$1" role="$2" text_file="$3"
  local file
  file="$(thread_file "$id")"
  mkdir -p "$THREADS"
  agent_memory_ensure_oms_ignore "$STATE_ROOT" 2>/dev/null || true
  oms_with_file_lock "$file" append_row_unlocked "$id" "$role" "$text_file" "$file"
}

append_row_unlocked() {
  local id="$1" role="$2" text_file="$3" file="$4"
  OMS_TH_FILE="$file" OMS_TH_ID="$id" OMS_TH_ROLE="$role" \
  OMS_TH_TEXT_FILE="$text_file" OMS_TH_AGENT="$(oms_detect_agent)" \
  OMS_TH_PROVIDER="$PROVIDER" OMS_TH_MODEL="$MODEL" OMS_TH_ARTIFACT="$ARTIFACT" \
  OMS_TH_QUALITY="$QUALITY" \
  OMS_TH_MAX="${OMS_THREAD_TURN_BYTES:-4000}" \
  python3 - <<'PY'
import json, os, time

path = os.environ["OMS_TH_FILE"]
text = ""
tf = os.environ.get("OMS_TH_TEXT_FILE") or ""
if tf and os.path.isfile(tf):
    with open(tf, encoding="utf-8", errors="replace") as f:
        text = f.read()
try:
    budget = int(os.environ.get("OMS_TH_MAX", "4000"))
except ValueError:
    budget = 4000
text = text.strip()
if len(text.encode("utf-8")) > budget:
    # Keep the tail, cut on a character boundary: provider CLIs narrate tools
    # first and answer last, so the head of a long turn is the noise and the
    # tail is the content. The artifact keeps the full text.
    cut = text.encode("utf-8")[-budget:].decode("utf-8", "ignore")
    text = "[earlier output truncated: see artifact]\n" + cut

seq = 0
if os.path.isfile(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except Exception:
                continue
            if isinstance(row, dict) and isinstance(row.get("seq"), int):
                seq = max(seq, row["seq"])

row = {
    "schema": 1,
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "thread": os.environ["OMS_TH_ID"],
    "seq": seq + 1,
    "role": os.environ["OMS_TH_ROLE"],
    "agent": os.environ.get("OMS_TH_AGENT") or "agent",
    "text": text,
}
for key, env in (("provider", "OMS_TH_PROVIDER"), ("model", "OMS_TH_MODEL"),
                 ("artifact", "OMS_TH_ARTIFACT"), ("quality", "OMS_TH_QUALITY")):
    value = os.environ.get(env) or ""
    if value:
        row[key] = value
with open(path, "a", encoding="utf-8") as f:
    f.write(json.dumps(row, ensure_ascii=False) + "\n")
PY
}

cmd_new() {
  local id stamp
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  if [ -n "$THREAD_ID" ]; then
    valid_id "$THREAD_ID" || fail "invalid thread id: $THREAD_ID"
    id="$THREAD_ID"
  else
    id="th-$stamp-$$"
  fi
  if [ -f "$(thread_file "$id")" ]; then
    write_current "$id"
    echo "$id"
    echo "agent-thread: joined existing thread $id" >&2
    return 0
  fi
  mkdir -p "$THREADS"
  agent_memory_ensure_oms_ignore "$STATE_ROOT" 2>/dev/null || true
  : > "$(thread_file "$id")"
  if [ -n "$TOPIC" ]; then
    append_topic "$id"
  fi
  write_current "$id"
  echo "$id"
}

append_topic() {
  local id="$1" tmp
  tmp="$(agent_memory_mktemp)" || return 0
  printf '%s\n' "$TOPIC" > "$tmp"
  if agent_memory_file_has_sensitive_content "$tmp"; then
    rm -f "$tmp"
    fail "topic looks sensitive; rephrase without secrets, private paths, or cluster details"
  fi
  append_row "$id" topic "$tmp"
  rm -f "$tmp"
}

cmd_append() {
  local id tmp
  id="$(require_thread)"
  case "$ROLE" in
    question|answer|note|decision|summary|topic) ;;
    *) fail "unknown --role: $ROLE (question, answer, note, decision, summary)" ;;
  esac
  tmp="$(agent_memory_mktemp)" || fail "mktemp failed"
  if [ -n "$TEXT_FILE" ]; then
    [ -f "$TEXT_FILE" ] || { rm -f "$tmp"; fail "text file not found: $TEXT_FILE"; }
    cat "$TEXT_FILE" > "$tmp"
  elif [ -n "$TEXT" ]; then
    printf '%s\n' "$TEXT" > "$tmp"
  else
    rm -f "$tmp"
    fail "--text or --text-file is required"
  fi
  # Same gate as shared memory: a thread is read back into other providers'
  # prompts, so it must never carry secrets or private paths.
  if agent_memory_file_has_sensitive_content "$tmp"; then
    rm -f "$tmp"
    fail "turn looks sensitive; it would be replayed into other agents' prompts"
  fi
  append_row "$id" "$ROLE" "$tmp"
  rm -f "$tmp"
  echo "agent-thread: appended $ROLE to $id" >&2
}

cmd_context() {
  local id
  id="$(require_thread)"
  OMS_TH_FILE="$(thread_file "$id")" OMS_TH_ID="$id" \
  OMS_TH_BYTES="$MAX_BYTES" OMS_TH_TURNS="$MAX_TURNS" python3 - <<'PY'
import json, os

path = os.environ["OMS_TH_FILE"]
budget = int(os.environ["OMS_TH_BYTES"])
limit = int(os.environ["OMS_TH_TURNS"])
rows = []
with open(path, encoding="utf-8", errors="replace") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        if isinstance(row, dict) and row.get("role") != "closed":
            rows.append(row)
if not rows:
    raise SystemExit(0)

# Newest turns matter most, so fill the budget from the end and then print
# oldest-first for a readable transcript.
kept = []
used = 0
for row in reversed(rows[-limit:]):
    who = row.get("provider") or row.get("agent") or "agent"
    quality = row.get("quality")
    tag = "" if quality in (None, "ok") else " [%s answer]" % quality
    block = "### %s (%s, %s)%s\n%s\n" % (row.get("role", "note"), who,
                                          row.get("ts", "?"), tag,
                                          row.get("text", ""))
    size = len(block.encode("utf-8"))
    if kept and used + size > budget:
        break
    kept.append(block)
    used += size
omitted = len(rows) - len(kept)
print("## Conversation so far (thread %s)" % os.environ["OMS_TH_ID"])
if omitted > 0:
    print("[%d earlier turn(s) omitted]" % omitted)
print("")
for block in reversed(kept):
    print(block)
PY
}

cmd_show() {
  local id
  id="$(require_thread)"
  if [ "$AS_JSON" -eq 1 ]; then
    OMS_TH_FILE="$(thread_file "$id")" OMS_TH_ID="$id" python3 - <<'PY'
import json, os
rows = []
with open(os.environ["OMS_TH_FILE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except Exception:
            continue
print(json.dumps({"schema": 1, "thread": os.environ["OMS_TH_ID"],
                  "turns": rows}, ensure_ascii=False))
PY
    return 0
  fi
  echo "# thread $id"
  OMS_TH_FILE="$(thread_file "$id")" python3 - <<'PY'
import json, os
with open(os.environ["OMS_TH_FILE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        who = row.get("provider") or row.get("agent") or "agent"
        print("\n## %s %s (%s)" % (row.get("seq", "?"), row.get("role", "note"), who))
        if row.get("model"):
            print("model: %s" % row["model"])
        if row.get("quality") and row["quality"] != "ok":
            print("quality: %s (did not really answer)" % row["quality"])
        if row.get("artifact"):
            print("artifact: %s" % row["artifact"])
        print(row.get("text", ""))
PY
}

# One scan feeds both views: staleness, the provider set, and current status all
# come from the rows the listing already reads.
thread_view() {
  local current=""
  current="$(read_current || true)"
  OMS_TH_DIR="$THREADS" OMS_TH_ALL="$ALL" OMS_TH_JSON="$AS_JSON" \
  OMS_TH_CURRENT="$current" OMS_TH_MODE="$1" OMS_TH_STALE="$STALE" \
  OMS_TH_STALE_TTL="$STALE_TTL" python3 - <<'PY'
import calendar, glob, json, os, re, time

d = os.environ["OMS_TH_DIR"]
mode = os.environ["OMS_TH_MODE"]
as_json = os.environ["OMS_TH_JSON"] == "1"
only_stale = os.environ["OMS_TH_STALE"] == "1"
# Whether a thread ever got reused is a question closed threads answer too.
show_all = os.environ["OMS_TH_ALL"] == "1" or mode == "stats"
stale_ttl = int(os.environ["OMS_TH_STALE_TTL"])
current = os.environ.get("OMS_TH_CURRENT") or ""
now = time.time()
# Only an id the --id flag would accept can appear in a printed close command.
addressable = re.compile(r"\A[A-Za-z0-9_-][A-Za-z0-9._-]*\Z")


def last_epoch(rows):
    """Age a thread by its newest turn whose timestamp actually parses."""
    for row in reversed(rows):
        if not isinstance(row, dict):
            continue
        try:
            return calendar.timegm(
                time.strptime(row.get("ts") or "", "%Y-%m-%dT%H:%M:%SZ"))
        except Exception:
            continue
    return None


def human(seconds):
    if seconds is None:
        return "?"
    if seconds >= 86400:
        return "%dd" % (seconds // 86400)
    if seconds >= 3600:
        return "%dh" % (seconds // 3600)
    return "%dm" % (seconds // 60)


stats = {"threads": 0, "open": 0, "closed": 0, "stale_open": 0,
         "undatable_open": 0, "second_question": 0, "multi_provider": 0,
         "answers_rated": 0, "answers_ok": 0}
threads = []
for path in sorted(glob.glob(os.path.join(d, "*.jsonl"))):
    tid = os.path.basename(path)[:-len(".jsonl")]
    rows = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except Exception:
                continue
    closed = any(r.get("role") == "closed" for r in rows if isinstance(r, dict))
    if closed and not show_all:
        continue
    topic = ""
    for r in rows:
        if isinstance(r, dict) and r.get("role") == "topic":
            topic = r.get("text", "")
            break
    if not topic:
        for r in rows:
            if isinstance(r, dict) and r.get("role") == "question":
                topic = r.get("text", "")
                break
    providers = []
    questions = 0
    rated = 0
    rated_ok = 0
    for r in rows:
        if not isinstance(r, dict):
            continue
        p = r.get("provider")
        if p and p not in providers:
            providers.append(p)
        if r.get("role") == "question":
            questions += 1
        if r.get("role") == "answer" and r.get("quality"):
            rated += 1
            if r.get("quality") == "ok":
                rated_ok += 1
    epoch = last_epoch(rows)
    age = None if epoch is None else int(now - epoch)
    # Advisory classification only. The current thread is live by definition,
    # and a turn the harness cannot date must not be called abandoned.
    if closed:
        staleness = "closed"
    elif tid == current:
        staleness = "current"
    elif age is None:
        staleness = "unknown"
    elif age > stale_ttl:
        staleness = "stale"
    else:
        staleness = "fresh"
    stats["threads"] += 1
    stats["closed" if closed else "open"] += 1
    if staleness == "stale":
        stats["stale_open"] += 1
    elif staleness == "unknown":
        stats["undatable_open"] += 1
    if questions >= 2:
        stats["second_question"] += 1
    if len(providers) >= 2:
        stats["multi_provider"] += 1
    stats["answers_rated"] += rated
    stats["answers_ok"] += rated_ok
    entry = {
        "id": tid,
        "turns": len(rows),
        "closed": closed,
        "current": tid == current,
        "topic": " ".join(topic.split())[:80],
        "providers": providers,
        "last_ts": rows[-1].get("ts") if rows and isinstance(rows[-1], dict) else None,
        "staleness": staleness,
        "age_seconds": age,
    }
    if staleness == "stale" and addressable.match(tid):
        entry["close_command"] = 'oms thread close --id %s --summary "stale"' % tid
    threads.append(entry)
threads.sort(key=lambda t: t["last_ts"] or "", reverse=True)

if mode == "stats":
    if as_json:
        print(json.dumps({"schema": 1, "stale_ttl_seconds": stale_ttl,
                          "stats": stats}, ensure_ascii=False, sort_keys=True))
    else:
        print("# thread stats")
        print("  threads: %d (open %d, closed %d)" % (
            stats["threads"], stats["open"], stats["closed"]))
        print("  stale open: %d (idle over %s; oms thread list --stale)" % (
            stats["stale_open"], human(stale_ttl)))
        print("  undatable open: %d" % stats["undatable_open"])
        print("  with a follow-up question: %d" % stats["second_question"])
        print("  with multiple providers: %d" % stats["multi_provider"])
        print("  rated answers: %d (quality=ok %d)" % (
            stats["answers_rated"], stats["answers_ok"]))
    raise SystemExit(0)

if only_stale:
    threads = [t for t in threads if t["staleness"] == "stale"]
if as_json:
    print(json.dumps({"schema": 1, "threads": threads}, ensure_ascii=False))
elif only_stale and not threads:
    print("no stale open threads (none idle longer than %s)" % human(stale_ttl))
elif only_stale:
    print("%d stale open thread(s), idle over %s. Nothing is closed "
          "automatically:" % (len(threads), human(stale_ttl)))
    for t in threads:
        print("%-28s idle=%-5s turns=%-3d %s%s" % (
            t["id"], human(t["age_seconds"]), t["turns"],
            ",".join(t["providers"]) or "-",
            ("  " + t["topic"]) if t["topic"] else ""))
        if t.get("close_command"):
            print("  close: %s" % t["close_command"])
        else:
            print("  close: unavailable, id is not addressable by --id")
elif not threads:
    print("no threads")
else:
    for t in threads:
        print("%-28s %s turns=%-3d %s%s%s" % (
            t["id"], "closed" if t["closed"] else "open  ", t["turns"],
            ",".join(t["providers"]) or "-",
            "  *current" if t["current"] else "",
            ("  " + t["topic"]) if t["topic"] else ""))
PY
}

cmd_list() { thread_view list; }

cmd_stats() { thread_view stats; }

cmd_current() {
  local id
  id="$(read_current)" || { echo "no current thread" >&2; exit 1; }
  printf '%s\n' "$id"
}

cmd_close() {
  local id tmp
  id="$(require_thread)"
  tmp="$(agent_memory_mktemp)" || fail "mktemp failed"
  printf '%s\n' "${SUMMARY:-closed}" > "$tmp"
  if agent_memory_file_has_sensitive_content "$tmp"; then
    rm -f "$tmp"
    fail "summary looks sensitive; rephrase it"
  fi
  append_row "$id" closed "$tmp"
  rm -f "$tmp"
  # Only clear the pointer when it names this thread.
  if [ "$(read_current || true)" = "$id" ]; then
    rm -f "$CURRENT"
  fi
  echo "agent-thread: closed $id" >&2
}

case "$ACTION" in
  new) cmd_new ;;
  append) cmd_append ;;
  context) cmd_context ;;
  show) cmd_show ;;
  list) cmd_list ;;
  stats) cmd_stats ;;
  current) cmd_current ;;
  close) cmd_close ;;
esac
