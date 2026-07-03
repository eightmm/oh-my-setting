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

STATE_ROOT="$(oms_repo_root "$PWD")"
LEDGER="${OMS_FAIL_LEDGER:-$STATE_ROOT/.oms/failures.jsonl}"
SCHEMA=1

CMD=""
EXIT_CODE=""
KIND="cmd"
SUMMARY=""
FINGERPRINT=""
UNRESOLVED_ONLY=0
ACTION=""

usage() {
  cat <<'EOF'
Usage: fail-ledger.sh record --cmd CMD --exit N [--kind K] [--summary TEXT]
       fail-ledger.sh check  --cmd CMD
       fail-ledger.sh resolve --fingerprint FP
       fail-ledger.sh list   [--unresolved]

Durable failure memory shared by Codex, Claude Code, and Antigravity.
Fingerprint = short hash of the normalized command (whitespace collapsed,
long digit runs masked), so the same failing command dedupes across agents.

record   Append a failure for CMD (exit N). Bumps the fingerprint's count.
check    Exit 3 (and print prior context) if CMD's fingerprint is a known
         UNRESOLVED failure; exit 0 otherwise. Gate a retry with this.
resolve  Mark a fingerprint fixed so it stops warning.
list     One line per fingerprint (count, last exit, resolved); --unresolved
         shows only still-failing ones.

Never records sensitive-looking commands/summaries (blocked, like memory).
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Print the fingerprint of a command string on stdout.
fingerprint_of() {
  OMS_CMD="$1" python3 - <<'PY'
import os, re, hashlib
cmd = os.environ["OMS_CMD"]
norm = re.sub(r"\s+", " ", cmd).strip()
norm = re.sub(r"\d{8,}", "N", norm)   # timestamps / pids
print(hashlib.sha256(norm.encode("utf-8", "replace")).hexdigest()[:16])
PY
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
    --fingerprint) [ "$#" -ge 2 ] || fail "--fingerprint requires a value"; FINGERPRINT="$2"; shift 2 ;;
    --unresolved) UNRESOLVED_ONLY=1; shift ;;
    record|check|resolve|list) ACTION="$1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"

case "$ACTION" in
  record)
    [ -n "$CMD" ] || fail "record requires --cmd"
    case "$EXIT_CODE" in *[!0-9]*|"") fail "record requires --exit N (non-negative integer)" ;; esac
    # Refuse sensitive-looking content, mirroring the memory writer.
    scan="$(mktemp)" || fail "mktemp failed"
    printf '%s\n%s\n' "$CMD" "$SUMMARY" > "$scan"
    if agent_memory_file_has_sensitive_content "$scan"; then
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
      OMS_FP="$fp" OMS_KIND="$KIND" OMS_CMD="$CMD" OMS_EXIT="$EXIT_CODE" OMS_SUMMARY="$SUMMARY" \
      python3 - > "$row_tmp" <<'PY'
import json, os
row = {"schema": int(os.environ["OMS_SCHEMA"]), "event": "fail",
       "ts": os.environ["OMS_TS"], "agent": os.environ["OMS_AGENT_L"],
       "fingerprint": os.environ["OMS_FP"], "kind": os.environ["OMS_KIND"],
       "cmd": os.environ["OMS_CMD"], "exit": int(os.environ["OMS_EXIT"])}
if os.environ.get("OMS_SUMMARY"):
    row["summary"] = os.environ["OMS_SUMMARY"]
print(json.dumps(row, ensure_ascii=False, allow_nan=False))
PY
    oms_with_file_lock "$LEDGER" ledger_append "$LEDGER" "$row_tmp"
    rm -f "$row_tmp"
    echo "fail-ledger: recorded $fp (exit $EXIT_CODE)" >&2
    ;;
  check)
    [ -n "$CMD" ] || fail "check requires --cmd"
    [ -f "$LEDGER" ] || exit 0
    fp="$(fingerprint_of "$CMD")"
    OMS_FP="$fp" python3 - "$LEDGER" <<'PY'
import json, os, sys
fp = os.environ["OMS_FP"]
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
    sys.stderr.write("fail-ledger: %s already failed %dx (last exit %s): %s\n" % (
        fp, fails, last.get("exit"), (last.get("summary") or last.get("cmd", ""))[:160]))
    sys.exit(3)
sys.exit(0)
PY
    ;;
  resolve)
    [ -n "$FINGERPRINT" ] || fail "resolve requires --fingerprint"
    [ -f "$LEDGER" ] || fail "no ledger at $LEDGER"
    mkdir -p "$(dirname "$LEDGER")"
    row_tmp="$(mktemp)" || fail "mktemp failed"
    OMS_SCHEMA="$SCHEMA" OMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_AGENT_L="$(oms_detect_agent)" \
      OMS_FP="$FINGERPRINT" python3 - > "$row_tmp" <<'PY'
import json, os
print(json.dumps({"schema": int(os.environ["OMS_SCHEMA"]), "event": "resolved",
                  "ts": os.environ["OMS_TS"], "agent": os.environ["OMS_AGENT_L"],
                  "fingerprint": os.environ["OMS_FP"]}, ensure_ascii=False))
PY
    oms_with_file_lock "$LEDGER" ledger_append "$LEDGER" "$row_tmp"
    rm -f "$row_tmp"
    echo "fail-ledger: resolved $FINGERPRINT" >&2
    ;;
  list)
    [ -f "$LEDGER" ] || { echo "no failures"; exit 0; }
    OMS_UNRESOLVED="$UNRESOLVED_ONLY" python3 - "$LEDGER" <<'PY'
import json, os, sys
unresolved_only = os.environ.get("OMS_UNRESOLVED") == "1"
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
        agg[fp] = {"count": 0, "last": None, "resolved": False}
        order.append(fp)
    ev = r.get("event")
    if ev == "resolved":
        agg[fp]["resolved"] = True
        agg[fp]["count"] = 0
    elif ev == "fail":
        agg[fp]["count"] += 1
        agg[fp]["last"] = r
        agg[fp]["resolved"] = False
for fp in order:
    d = agg[fp]
    if unresolved_only and (d["resolved"] or d["count"] == 0):
        continue
    last = d["last"] or {}
    tag = "resolved" if d["resolved"] else "OPEN"
    print("%s  %-8s count=%d exit=%s  %s" % (
        fp, tag, d["count"], last.get("exit", "-"),
        (last.get("summary") or last.get("cmd", ""))[:80]))
PY
    ;;
  *)
    fail "unknown command: $ACTION"
    ;;
esac
