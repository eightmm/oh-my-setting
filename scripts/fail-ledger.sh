#!/usr/bin/env bash
set -euo pipefail

# Cross-session, cross-agent failure memory. The active-task packet warns on a
# repeated failure only within one task and forgets it on close; this records
# a durable fingerprint of a failed command/verify/patch so a DIFFERENT agent
# (or a later session) can ask "have we already tried this and it failed?"
# before burning another attempt. Append-only JSONL; the current view is a
# replay (fails since the last resolve per fingerprint), like the study board.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT_LIB/file-lock.sh"

REPO="${OMS_STATE_REPO:-$PWD}"
STATE_ROOT=""
LEDGER=""
SCHEMA=2

CMD=""
EXIT_CODE=""
KIND="cmd"
SUMMARY=""
NEXT=""
FAILURE_CODE=""
FINGERPRINT=""
HOW=""
UNRESOLVED_ONLY=0
AS_JSON=0
LIMIT=0
IGNORE_STATE=0
ACTION=""

usage() {
  cat <<'EOF'
Usage: fail-ledger.sh [--repo PATH] record --cmd CMD --exit N [--kind K] [--summary TEXT] [--next TEXT]
       fail-ledger.sh [--repo PATH] check  --cmd CMD [--ignore-state]
       fail-ledger.sh [--repo PATH] resolve (--fingerprint FP | --cmd CMD) [--how TEXT]
       fail-ledger.sh [--repo PATH] list   [--unresolved] [--json]

Durable failure memory shared by Codex, Claude Code, and Antigravity.
Fingerprint = short hash of the normalized command (whitespace collapsed,
long digit runs masked). Schema-2 failures also carry a content-free git state
fingerprint, so an unchanged retry is blocked while a retry after edits is
allowed with a warning. Legacy rows remain command-only and conservative.

record   Append a failure for CMD (exit N), optionally with a recommended next
         action. Bumps the fingerprint's count. At
         the second unresolved failure of the same command it names `oms advise`
         (OMS_ADVISE_AFTER_FAILURES sets the threshold, 0 disables): the rules
         ask for an outside read after repeated failures, and retrying the same
         thing is what happens when nothing says so.
check    Exit 3 (and print prior context) if CMD's fingerprint is a known
         UNRESOLVED failure; exit 0 otherwise. Gate a retry with this.
         --ignore-state keeps the schema-2 git-state clearing out of the
         verdict, for failures that no repo change can plausibly fix (a
         provider CLI that hangs does not recover because the repo gained a
         commit).
resolve  Mark a fingerprint fixed so it stops warning; --how records how it
         was fixed.
list     One line per fingerprint (count, last exit, resolved); --unresolved
         shows only still-failing ones, --json emits a schema-1 JSON object,
         --limit N keeps the N most recently active fingerprints and says
         how many older ones were omitted.

kind=hook rows are filed automatically for every failed shell command, so
nothing ever resolves them by hand. They retire on a read-time TTL instead:
OMS_HOOK_FAIL_TTL seconds (default 86400) after a hook failure was recorded it
stops counting as open in check/record/list, and list prints it as EXPIRED
rather than dropping it. Deliberately recorded kinds never expire. Reads never
rewrite the ledger; gc.sh carries the same predicate, so retired rows are
compacted away on the normal retention schedule.

Never records sensitive-looking commands/summaries (blocked, like memory).
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Print the fingerprint of a command string on stdout.
# Unresolved failures recorded for a fingerprint, counted the way `check` counts
# them: a resolve row zeroes the run. Used to decide when a repeat has become a
# pattern worth an outside read.
unresolved_fails_for() {
  OMS_FP="$1" python3 - "$LEDGER" <<'COUNT'
import calendar, json, os, sys, time
fp = os.environ["OMS_FP"]
ttl = int(os.environ.get("OMS_HOOK_TTL") or 86400)
now = time.time()

# Retirement predicate, textually identical in fail-ledger.sh (record's repeat
# count, check, list), gc.sh's failure compaction, state.sh (both its
# sites), and resume-hook.sh's failure line: read-time expiry and gc compose
# only while every site agrees on which rows are retired — including >= at
# the boundary, where one drifted site once disagreed by exactly one second.
def hook_expired(r):
    if r.get("kind") != "hook" or r.get("event") != "fail":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False   # an unreadable stamp is never grounds for retirement
    return (now - t) >= ttl

fails = 0
try:
    handle = open(sys.argv[1], encoding="utf-8", errors="replace")
except OSError:
    print(0)
    raise SystemExit(0)
for line in handle:
    try:
        row = json.loads(line)
    except Exception:
        continue
    if row.get("fingerprint") != fp:
        continue
    event = row.get("event")
    if event == "resolved":
        fails = 0
    elif event == "fail" and not hook_expired(row):
        fails += 1
print(fails)
COUNT
}

# The rules have always said to consult an advisor after repeated failures, and
# peer-delegate does it from the second repair round. The primary agent's own
# gate failures land here and escalated nowhere: it would file the row and try
# the same thing again. Naming the advisor at the threshold costs one line of
# stderr and no model call, so the decision to spend one stays with the caller.
advise_hint_if_repeated() {
  local fp="$1"
  local fails="$2"
  local threshold="${OMS_ADVISE_AFTER_FAILURES:-2}"

  case "$threshold" in ''|*[!0-9]*) threshold=2 ;; esac
  [ "$threshold" -gt 0 ] || return 0
  [ "$fails" -ge "$threshold" ] || return 0
  printf 'fail-ledger: %s has failed %dx unresolved; get an outside read before the next attempt (oms advise --prompt "...")\n' \
    "$fp" "$fails" >&2
}

fingerprint_of() {
  OMS_CMD="$1" python3 - <<'PY'
import os, re, hashlib
cmd = os.environ["OMS_CMD"]
norm = re.sub(r"\s+", " ", cmd).strip()
norm = re.sub(r"\d{8,}", "N", norm)   # timestamps / pids
print(hashlib.sha256(norm.encode("utf-8", "replace")).hexdigest()[:16])
PY
}

# Content-free fingerprint of the working tree, shared with agent-task so both
# use one definition of "did the repo change since then".
state_fingerprint() {
  oms_git_state_fingerprint "$STATE_ROOT"
}

ledger_append() {
  local ledger="$1"
  local row_file="$2"
  # A crash or full disk mid-append can leave a torn, unterminated line; the
  # next append must not glue onto it and destroy its own record too. Start
  # on a fresh line whenever the last byte is not a newline.
  if [ -s "$ledger" ] && [ -n "$(tail -c 1 "$ledger" 2>/dev/null)" ]; then
    printf '\n' >> "$ledger"
  fi
  cat "$row_file" >> "$ledger"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --cmd) [ "$#" -ge 2 ] || fail "--cmd requires text"; CMD="$2"; shift 2 ;;
    --exit) [ "$#" -ge 2 ] || fail "--exit requires an integer"; EXIT_CODE="$2"; shift 2 ;;
    --kind) [ "$#" -ge 2 ] || fail "--kind requires text"; KIND="$2"; shift 2 ;;
    --summary) [ "$#" -ge 2 ] || fail "--summary requires text"; SUMMARY="$2"; shift 2 ;;
    --next) [ "$#" -ge 2 ] || fail "--next requires text"; NEXT="$2"; shift 2 ;;
    --failure-code) [ "$#" -ge 2 ] || fail "--failure-code requires a value"; FAILURE_CODE="$2"; shift 2 ;;
    --fingerprint) [ "$#" -ge 2 ] || fail "--fingerprint requires a value"; FINGERPRINT="$2"; shift 2 ;;
    --how) [ "$#" -ge 2 ] || fail "--how requires text"; HOW="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --unresolved) UNRESOLVED_ONLY=1; shift ;;
    --json) AS_JSON=1; shift ;;
    --limit)
      [ "$#" -ge 2 ] || fail "--limit requires a count"
      case "$2" in ''|*[!0-9]*) fail "--limit must be a non-negative integer" ;; esac
      LIMIT="$2"; shift 2 ;;
    --ignore-state) IGNORE_STATE=1; shift ;;
    record|check|resolve|list) ACTION="$1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"
[ "$AS_JSON" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--json is only supported by list"
[ "$LIMIT" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--limit is only supported by list"
[ "$IGNORE_STATE" -eq 0 ] || [ "$ACTION" = "check" ] || fail "--ignore-state is only supported by check"
case "$KIND" in
  cmd|hook|verify|plan-run|delegate|patch-land) ;;
  *) fail "--kind must be one of: cmd, hook, verify, plan-run, delegate, patch-land" ;;
esac
STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
LEDGER="${OMS_FAIL_LEDGER:-$STATE_ROOT/.oms/failures.jsonl}"

# Read-time retirement clock for automatic hook rows. The deployed baseline is
# not measured yet, so the default sits behind an override.
HOOK_FAIL_TTL="${OMS_HOOK_FAIL_TTL:-86400}"
case "$HOOK_FAIL_TTL" in *[!0-9]*|"") HOOK_FAIL_TTL=86400 ;; esac
export OMS_HOOK_TTL="$HOOK_FAIL_TTL"

# Failed commands routinely carry absolute paths; refusing them lost the
# failure memory this ledger exists for. Machine paths are normalized instead
# of blocked, on every action so fingerprints of the stored and looked-up text
# agree. Secrets still block at record time below.
[ -z "$CMD" ] || CMD="$(printf '%s' "$CMD" | agent_memory_normalize_machine_paths)"
[ -z "$SUMMARY" ] || SUMMARY="$(printf '%s' "$SUMMARY" | agent_memory_normalize_machine_paths)"
[ -z "$NEXT" ] || NEXT="$(printf '%s' "$NEXT" | agent_memory_normalize_machine_paths)"
[ -z "$HOW" ] || HOW="$(printf '%s' "$HOW" | agent_memory_normalize_machine_paths)"

case "$ACTION" in
  record)
    [ -n "$CMD" ] || fail "record requires --cmd"
    case "$EXIT_CODE" in *[!0-9]*|"") fail "record requires --exit N (non-negative integer)" ;; esac
    # Refuse secret-tier content; machine paths were already normalized above.
    scan="$(mktemp)" || fail "mktemp failed"
    printf '%s\n%s\n' "$CMD" "$SUMMARY" > "$scan"
    if agent_memory_file_has_secret_content "$scan"; then
      rm -f "$scan"
      echo "fail-ledger: refused; command/summary looks sensitive" >&2
      exit 3
    fi
    rm -f "$scan"
    fp="$(fingerprint_of "$CMD")"
    mkdir -p "$(dirname "$LEDGER")"
    agent_memory_ensure_oms_ignore_for_path "$LEDGER" 2>/dev/null || true
    row_tmp="$(mktemp)" || fail "mktemp failed"
    OMS_SCHEMA="$SCHEMA" OMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_AGENT_L="$(oms_detect_agent)" \
      OMS_FP="$fp" OMS_KIND="$KIND" OMS_CMD="$CMD" OMS_EXIT="$EXIT_CODE" OMS_SUMMARY="$SUMMARY" OMS_NEXT="$NEXT" \
      OMS_STATE_FP="$(state_fingerprint)" \
      OMS_RUNTIME_LIB="$ROOT/scripts/lib" OMS_FAILURE_CODE="${FAILURE_CODE:-}" \
      python3 - > "$row_tmp" <<'PY'
import json, os, sys
row = {"schema": int(os.environ["OMS_SCHEMA"]), "event": "fail",
       "ts": os.environ["OMS_TS"], "agent": os.environ["OMS_AGENT_L"],
       "fingerprint": os.environ["OMS_FP"], "kind": os.environ["OMS_KIND"],
       "cmd": os.environ["OMS_CMD"], "exit": int(os.environ["OMS_EXIT"]),
       "state_fingerprint": os.environ["OMS_STATE_FP"]}
if os.environ.get("OMS_SUMMARY"):
    row["summary"] = os.environ["OMS_SUMMARY"]
if os.environ.get("OMS_NEXT"):
    row["next"] = os.environ["OMS_NEXT"]
# Canonical taxonomy rides beside the free-form reason: every producer that
# records a failure funnels through here, so one annotation point covers the
# provider, supervisor, delegation, admission, plan-run, and goal-drive paths.
# Classification is enrichment — a ledger that cannot classify still records.
try:
    sys.path.insert(0, os.environ["OMS_RUNTIME_LIB"])
    from oms_runtime.failures import classify
    judged = classify(
        "%s %s" % (os.environ["OMS_CMD"], os.environ.get("OMS_SUMMARY", "")),
        exit_code=int(os.environ["OMS_EXIT"]),
        explicit=os.environ.get("OMS_FAILURE_CODE", ""),
    )
    row["failure_code"] = judged["code"]
    row["recovery"] = judged["recovery"]
except Exception:
    pass
print(json.dumps(row, ensure_ascii=False, allow_nan=False))
PY
    oms_with_file_lock "$LEDGER" ledger_append "$LEDGER" "$row_tmp"
    rm -f "$row_tmp"
    echo "fail-ledger: recorded $fp (exit $EXIT_CODE)" >&2
    # Counted after the append, so the row just filed is included: the second
    # unresolved failure of the same command is the one worth stopping on.
    advise_hint_if_repeated "$fp" "$(unresolved_fails_for "$fp")"
    ;;
  check)
    [ -n "$CMD" ] || fail "check requires --cmd"
    [ -f "$LEDGER" ] || exit 0
    fp="$(fingerprint_of "$CMD")"
    OMS_FP="$fp" OMS_STATE_FP="$(state_fingerprint)" OMS_IGNORE_STATE="$IGNORE_STATE" \
      python3 - "$LEDGER" <<'PY'
import calendar, json, os, sys, time
fp = os.environ["OMS_FP"]
current_state = os.environ["OMS_STATE_FP"]
ignore_state = os.environ.get("OMS_IGNORE_STATE") == "1"
ttl = int(os.environ.get("OMS_HOOK_TTL") or 86400)
now = time.time()

# Retirement predicate, textually identical in fail-ledger.sh (record's repeat
# count, check, list), gc.sh's failure compaction, state.sh (both its
# sites), and resume-hook.sh's failure line: read-time expiry and gc compose
# only while every site agrees on which rows are retired — including >= at
# the boundary, where one drifted site once disagreed by exactly one second.
def hook_expired(r):
    if r.get("kind") != "hook" or r.get("event") != "fail":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False   # an unreadable stamp is never grounds for retirement
    return (now - t) >= ttl

fails = 0
last = None
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    if r.get("fingerprint") != fp:
        continue
    ev = r.get("event")
    if ev == "resolved":
        fails = 0
        last = None
    elif ev == "fail":
        # A retired hook row blocks nothing and is not the failure a caller
        # would be shown; the row itself stays on disk for gc to reclaim.
        if hook_expired(r):
            continue
        fails += 1
        last = r
if fails > 0 and last is not None:
    recorded_state = last.get("state_fingerprint")
    if not ignore_state and recorded_state and recorded_state != current_state:
        sys.stderr.write("fail-ledger: %s failed before, but git state changed; retry allowed\n" % fp)
        sys.exit(0)
    sys.stderr.write("fail-ledger: %s already failed %dx (last exit %s): %s\n" % (
        fp, fails, last.get("exit"), (last.get("summary") or last.get("cmd", ""))[:160]))
    if last.get("next"):
        sys.stderr.write("fail-ledger: next: %s\n" % last["next"])
    # The canonical taxonomy is why the row exists: surface the recovery the
    # code binds so every refusal built on this check carries the policy, not
    # just the history. Older rows predate the fields and stay readable.
    if last.get("failure_code"):
        sys.stderr.write("fail-ledger: code: %s\n" % last["failure_code"])
    if last.get("recovery"):
        sys.stderr.write("fail-ledger: recovery: %s\n" % last["recovery"])
    threshold = os.environ.get("OMS_ADVISE_AFTER_FAILURES", "2")
    threshold = int(threshold) if threshold.isdigit() else 2
    if threshold > 0 and fails >= threshold:
        sys.stderr.write(
            "fail-ledger: %s has failed %dx unresolved; get an outside read before "
            "the next attempt (oms advise --prompt \"...\")\n" % (fp, fails))
    sys.exit(3)
sys.exit(0)
PY
    ;;
  resolve)
    # A caller that has just watched a command pass holds the command, not the
    # fingerprint it was filed under. Deriving it here is the same computation
    # `check --cmd` already does, and saves every caller from shelling out
    # twice to clear a failure it just fixed.
    if [ -z "$FINGERPRINT" ] && [ -n "$CMD" ]; then
      FINGERPRINT="$(fingerprint_of "$CMD")"
    fi
    [ -n "$FINGERPRINT" ] || fail "resolve requires --fingerprint or --cmd"
    [ -f "$LEDGER" ] || fail "no ledger at $LEDGER"
    mkdir -p "$(dirname "$LEDGER")"
    row_tmp="$(mktemp)" || fail "mktemp failed"
    OMS_SCHEMA="$SCHEMA" OMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_AGENT_L="$(oms_detect_agent)" \
      OMS_FP="$FINGERPRINT" OMS_HOW="$HOW" python3 - > "$row_tmp" <<'PY'
import json, os
row = {"schema": int(os.environ["OMS_SCHEMA"]), "event": "resolved",
       "ts": os.environ["OMS_TS"], "agent": os.environ["OMS_AGENT_L"],
       "fingerprint": os.environ["OMS_FP"]}
if os.environ.get("OMS_HOW"):
    row["how"] = os.environ["OMS_HOW"]
print(json.dumps(row, ensure_ascii=False))
PY
    oms_with_file_lock "$LEDGER" ledger_append "$LEDGER" "$row_tmp"
    rm -f "$row_tmp"
    echo "fail-ledger: resolved $FINGERPRINT" >&2
    ;;
  list)
    [ ! -L "$LEDGER" ] || fail "failure ledger must be a regular non-symlink file"
    if [ ! -f "$LEDGER" ]; then
      if [ "$AS_JSON" -eq 1 ]; then
        echo '{"schema": 1, "failures": [], "invalid_rows": 0}'
      else
        echo "no failures"
      fi
      exit 0
    fi
    OMS_UNRESOLVED="$UNRESOLVED_ONLY" OMS_JSON="$AS_JSON" OMS_LIMIT="$LIMIT" \
      OMS_HEAD="$(git -c core.fsmonitor=false -C "$STATE_ROOT" rev-parse HEAD 2>/dev/null || printf unborn)" \
      python3 - "$LEDGER" <<'PY'
import calendar, json, os, sys, time
unresolved_only = os.environ.get("OMS_UNRESOLVED") == "1"
as_json = os.environ.get("OMS_JSON") == "1"
ttl = int(os.environ.get("OMS_HOOK_TTL") or 86400)
now = time.time()
head = os.environ.get("OMS_HEAD") or ""

def stale_head(r):
    # One failure against another commit is evidence about a tree that is
    # gone; a recurrence across commits is the tree-independent kind.
    fp = str(r.get("state_fingerprint") or "")
    return bool(head and fp) and fp.split(":", 1)[0] != head

# Retirement predicate, textually identical in fail-ledger.sh (record's repeat
# count, check, list), gc.sh's failure compaction, state.sh (both its
# sites), and resume-hook.sh's failure line: read-time expiry and gc compose
# only while every site agrees on which rows are retired — including >= at
# the boundary, where one drifted site once disagreed by exactly one second.
def hook_expired(r):
    if r.get("kind") != "hook" or r.get("event") != "fail":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False   # an unreadable stamp is never grounds for retirement
    return (now - t) >= ttl

def nonempty_string(value):
    return isinstance(value, str) and bool(value.strip())

def valid_row(row):
    """Validate current rows strictly while retaining documented legacy rows.

    Schema-less rows predate schema 2 and are read as implicit schema 1.  They
    still need a typed event/fingerprint, and every optional legacy field that
    is present must have the type its replay consumes.  Schema 2 is writer-
    owned, so its event-specific required fields are enforced in full.
    """
    if not isinstance(row, dict):
        return False
    schema = row.get("schema", 1)
    if isinstance(schema, bool) or not isinstance(schema, int) or schema not in (1, 2):
        return False
    event = row.get("event")
    if event not in ("fail", "resolved") or not nonempty_string(row.get("fingerprint")):
        return False
    for key in ("ts", "agent", "kind", "cmd", "summary", "next", "how",
                "state_fingerprint", "failure_code", "recovery"):
        if key in row and not isinstance(row[key], str):
            return False
    for key in ("exit", "count"):
        if key in row and (isinstance(row[key], bool) or
                           not isinstance(row[key], int) or row[key] < 0):
            return False
    if "resolved" in row and not isinstance(row["resolved"], bool):
        return False
    if schema == 1:
        return True
    if not nonempty_string(row.get("ts")) or not nonempty_string(row.get("agent")):
        return False
    if event == "resolved":
        return True
    return (nonempty_string(row.get("kind")) and
            nonempty_string(row.get("cmd")) and
            isinstance(row.get("exit"), int) and
            not isinstance(row.get("exit"), bool) and row["exit"] >= 0)

agg = {}
order = []
invalid = 0
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    if not line.strip():
        continue
    try:
        r = json.loads(line)
    except Exception:
        invalid += 1
        continue
    if not valid_row(r):
        invalid += 1
        continue
    fp = r.get("fingerprint")
    if fp not in agg:
        agg[fp] = {"count": 0, "last": None, "resolved": False, "how": None,
                   "expired": 0, "last_expired": None}
        order.append(fp)
    ev = r.get("event")
    if ev == "resolved":
        agg[fp]["resolved"] = True
        agg[fp]["count"] = 0
        agg[fp]["how"] = r.get("how")
        agg[fp]["expired"] = 0
        agg[fp]["last_expired"] = None
    elif ev == "fail":
        # A retired row is counted apart, never dropped: the fingerprint still
        # prints, tagged EXPIRED, with the failure that retired it.
        if hook_expired(r):
            agg[fp]["expired"] += 1
            agg[fp]["last_expired"] = r
        else:
            agg[fp]["count"] += 1
            agg[fp]["last"] = r
        agg[fp]["resolved"] = False
        agg[fp]["how"] = None
if invalid:
    # Quarantine, never brick: a torn or foreign line is skipped and counted,
    # the valid rows still project, and the count rides the payload so state
    # marks the ledger unhealthy and the inbox keeps its corruption item. An
    # exit 2 here killed the whole runtime envelope with no recovery path —
    # the remediation the inbox names is this very command.
    sys.stderr.write("warning: failure ledger contains %d invalid row(s); "
                     "valid rows still project\n" % invalid)
rows = []
for fp in order:
    d = agg[fp]
    # --unresolved answers "what is still failing", and a retired row is not.
    if unresolved_only and (d["resolved"] or d["count"] == 0):
        continue
    last = d["last"] or d["last_expired"] or {}
    row = {
        "fingerprint": fp,
        "resolved": d["resolved"],
        "expired": not d["resolved"] and d["count"] == 0 and d["expired"] > 0,
        "count": d["count"],
        "exit": last.get("exit"),
        "ts": last.get("ts"),
        "kind": last.get("kind"),
        "cmd": last.get("cmd"),
        "summary": last.get("summary"),
    }
    if row["resolved"] or row["expired"]:
        row["attention"] = "none"
        row["actionable"] = False
        row["retiring"] = False
    elif row["kind"] == "hook" and row["count"] < 2:
        row["attention"] = "retiring"
        row["actionable"] = False
        row["retiring"] = True
    elif row["count"] < 2 and stale_head(last):
        row["attention"] = "stale"
        row["actionable"] = False
        row["retiring"] = False
    else:
        row["attention"] = "actionable"
        row["actionable"] = True
        row["retiring"] = False
    if d["expired"]:
        row["expired_count"] = d["expired"]
    if last.get("next"):
        row["next"] = last["next"]
    if d.get("how"):
        row["how"] = d["how"]
    rows.append(row)
# Most recently active fingerprints first when a limit is set: the projection
# rides into agent context (MCP), and the whole aggregated history outgrows
# any output budget. The omission is stated, never silent; retirement
# semantics above are untouched — this trims the report, not the ledger.
omitted = 0
limit = int(os.environ.get("OMS_LIMIT") or 0)
if limit and len(rows) > limit:
    newest = sorted(rows, key=lambda r: r.get("ts") or "", reverse=True)[:limit]
    keep = {id(r) for r in newest}
    omitted = len(rows) - limit
    rows = [r for r in rows if id(r) in keep]
if as_json:
    doc = {"schema": 1, "failures": rows, "invalid_rows": invalid}
    if omitted:
        doc["omitted"] = omitted
    print(json.dumps(doc, ensure_ascii=False))
else:
    for r in rows:
        if r["resolved"]:
            tag = "resolved"
        elif r["expired"]:
            tag = "EXPIRED"
        elif r["attention"] == "stale":
            tag = "stale"
        else:
            tag = "OPEN"
        print("%s  %-8s count=%d exit=%s  %s" % (
            r["fingerprint"], tag, r["count"],
            "-" if r["exit"] is None else r["exit"],
            (r["summary"] or r["cmd"] or "")[:80]))
        if r.get("next"):
            print("  next: %s" % r["next"])
        if r.get("expired_count"):
            print("  retired: %d hook failure(s) older than %ds; blocks nothing"
                  % (r["expired_count"], ttl))
        if r["resolved"] and r.get("how"):
            print("  fixed: %s" % r["how"])
    if omitted:
        print("(%d older fingerprint(s) omitted by --limit; run without it for all)" % omitted)
PY
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
