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
FINGERPRINT=""
HOW=""
UNRESOLVED_ONLY=0
AS_JSON=0
ACTION=""

usage() {
  cat <<'EOF'
Usage: fail-ledger.sh [--repo PATH] record --cmd CMD --exit N [--kind K] [--summary TEXT] [--next TEXT]
       fail-ledger.sh [--repo PATH] check  --cmd CMD
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
resolve  Mark a fingerprint fixed so it stops warning; --how records how it
         was fixed.
list     One line per fingerprint (count, last exit, resolved); --unresolved
         shows only still-failing ones, --json emits a schema-1 JSON object.

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
import json, os, sys
fp = os.environ["OMS_FP"]
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
    elif event == "fail":
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
  cat "$row_file" >> "$ledger"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --cmd) [ "$#" -ge 2 ] || fail "--cmd requires text"; CMD="$2"; shift 2 ;;
    --exit) [ "$#" -ge 2 ] || fail "--exit requires an integer"; EXIT_CODE="$2"; shift 2 ;;
    --kind) [ "$#" -ge 2 ] || fail "--kind requires text"; KIND="$2"; shift 2 ;;
    --summary) [ "$#" -ge 2 ] || fail "--summary requires text"; SUMMARY="$2"; shift 2 ;;
    --next) [ "$#" -ge 2 ] || fail "--next requires text"; NEXT="$2"; shift 2 ;;
    --fingerprint) [ "$#" -ge 2 ] || fail "--fingerprint requires a value"; FINGERPRINT="$2"; shift 2 ;;
    --how) [ "$#" -ge 2 ] || fail "--how requires text"; HOW="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --unresolved) UNRESOLVED_ONLY=1; shift ;;
    --json) AS_JSON=1; shift ;;
    record|check|resolve|list) ACTION="$1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"
[ "$AS_JSON" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--json is only supported by list"
case "$KIND" in
  cmd|hook|verify|plan-run|delegate|patch-land) ;;
  *) fail "--kind must be one of: cmd, hook, verify, plan-run, delegate, patch-land" ;;
esac
STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
LEDGER="${OMS_FAIL_LEDGER:-$STATE_ROOT/.oms/failures.jsonl}"

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
      python3 - > "$row_tmp" <<'PY'
import json, os
row = {"schema": int(os.environ["OMS_SCHEMA"]), "event": "fail",
       "ts": os.environ["OMS_TS"], "agent": os.environ["OMS_AGENT_L"],
       "fingerprint": os.environ["OMS_FP"], "kind": os.environ["OMS_KIND"],
       "cmd": os.environ["OMS_CMD"], "exit": int(os.environ["OMS_EXIT"]),
       "state_fingerprint": os.environ["OMS_STATE_FP"]}
if os.environ.get("OMS_SUMMARY"):
    row["summary"] = os.environ["OMS_SUMMARY"]
if os.environ.get("OMS_NEXT"):
    row["next"] = os.environ["OMS_NEXT"]
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
    OMS_FP="$fp" OMS_STATE_FP="$(state_fingerprint)" python3 - "$LEDGER" <<'PY'
import json, os, sys
fp = os.environ["OMS_FP"]
current_state = os.environ["OMS_STATE_FP"]
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
        fails += 1
        last = r
if fails > 0 and last is not None:
    recorded_state = last.get("state_fingerprint")
    if recorded_state and recorded_state != current_state:
        sys.stderr.write("fail-ledger: %s failed before, but git state changed; retry allowed\n" % fp)
        sys.exit(0)
    sys.stderr.write("fail-ledger: %s already failed %dx (last exit %s): %s\n" % (
        fp, fails, last.get("exit"), (last.get("summary") or last.get("cmd", ""))[:160]))
    if last.get("next"):
        sys.stderr.write("fail-ledger: next: %s\n" % last["next"])
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
    if [ ! -f "$LEDGER" ]; then
      if [ "$AS_JSON" -eq 1 ]; then
        echo '{"schema": 1, "failures": []}'
      else
        echo "no failures"
      fi
      exit 0
    fi
    OMS_UNRESOLVED="$UNRESOLVED_ONLY" OMS_JSON="$AS_JSON" python3 - "$LEDGER" <<'PY'
import json, os, sys
unresolved_only = os.environ.get("OMS_UNRESOLVED") == "1"
as_json = os.environ.get("OMS_JSON") == "1"
agg = {}
order = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        r = json.loads(line)
    except Exception:
        continue
    fp = r.get("fingerprint")
    if not fp:
        continue
    if fp not in agg:
        agg[fp] = {"count": 0, "last": None, "resolved": False, "how": None}
        order.append(fp)
    ev = r.get("event")
    if ev == "resolved":
        agg[fp]["resolved"] = True
        agg[fp]["count"] = 0
        agg[fp]["how"] = r.get("how")
    elif ev == "fail":
        agg[fp]["count"] += 1
        agg[fp]["last"] = r
        agg[fp]["resolved"] = False
        agg[fp]["how"] = None
rows = []
for fp in order:
    d = agg[fp]
    if unresolved_only and (d["resolved"] or d["count"] == 0):
        continue
    last = d["last"] or {}
    row = {
        "fingerprint": fp,
        "resolved": d["resolved"],
        "count": d["count"],
        "exit": last.get("exit"),
        "ts": last.get("ts"),
        "kind": last.get("kind"),
        "cmd": last.get("cmd"),
        "summary": last.get("summary"),
    }
    if last.get("next"):
        row["next"] = last["next"]
    if d.get("how"):
        row["how"] = d["how"]
    rows.append(row)
if as_json:
    print(json.dumps({"schema": 1, "failures": rows}, ensure_ascii=False))
else:
    for r in rows:
        tag = "resolved" if r["resolved"] else "OPEN"
        print("%s  %-8s count=%d exit=%s  %s" % (
            r["fingerprint"], tag, r["count"],
            "-" if r["exit"] is None else r["exit"],
            (r["summary"] or r["cmd"] or "")[:80]))
        if r.get("next"):
            print("  next: %s" % r["next"])
        if r["resolved"] and r.get("how"):
            print("  fixed: %s" % r["how"])
PY
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
