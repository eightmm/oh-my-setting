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
# shellcheck source=scripts/lib/critical-paths.sh
. "$ROOT_LIB/critical-paths.sh"

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
STALLS=0
AS_JSON=0
LIMIT=0
IGNORE_STATE=0
OUTBOUND_EVIDENCE=()
ACTION=""

usage() {
  cat <<'EOF'
Usage: fail-ledger.sh [--repo PATH] record --cmd CMD --exit N [--kind K] [--summary TEXT] [--next TEXT]
       fail-ledger.sh [--repo PATH] check  --cmd CMD [--ignore-state]
       fail-ledger.sh [--repo PATH] resolve (--fingerprint FP | --cmd CMD) [--how TEXT]
       fail-ledger.sh [--repo PATH] list   [--unresolved] [--json] [--limit N]
       fail-ledger.sh [--repo PATH] list   --stalls [--json]
       fail-ledger.sh [--repo PATH] show   --fingerprint FP [--json]

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
         thing is what happens when nothing says so. Two more mechanical
         one-line suggestions use the same channel and switch
         (lib/advise_triggers.py): an unresolved merge/rebase conflict in the
         tree, and a change touching a path on check.sh's critical-path list
         (lib/critical-paths.sh). Each prints once per fingerprint; the row
         records it in `advised`. Nothing here calls a model.
check   Exit 3 (and print prior context) if CMD's fingerprint is a known
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
         --stalls instead reports loops in the recent ledger and goal-drive
         progress events (same failure 3x, two commands alternating 6x,
         goal-drive runs ending the same way at the same HEAD 3x), advisory
         only; thresholds: OMS_STALL_WINDOW/_ERROR_REPEAT/_ALTERNATE/
         _NO_EVIDENCE/_MAX_AGE (0 disables a rule).
show     Print one fingerprint's full current row (state, count, exit, kind,
         last ts, cmd, summary, next, how); exit 1 for an unknown fingerprint.

kind=hook rows are filed automatically for every failed shell command, so
nothing ever resolves them by hand. They retire on a read-time TTL instead:
OMS_HOOK_FAIL_TTL seconds (default 86400) after a hook failure was recorded it
stops counting as open in check/record/list, and list prints it as EXPIRED
rather than dropping it. Deliberately recorded kinds never expire. Reads never
rewrite the ledger; gc.sh carries the same predicate, so retired rows are
compacted away on the normal retention schedule.

Never records sensitive-looking commands/summaries (blocked, like memory).
--next and --how get the same scan: a credential-looking note is refused
(exit 3, the error names the flag) and nothing is written. show withholds an
already-stored note that fails the scan; stored rows are never rewritten.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

# --next and --how are stored as given and printed back by show, so they pass
# the scan the command and summary pass. Nothing is written for a refused note.
refuse_secret_note() {  # FLAG VALUE
  local flag="$1" value="$2" note_scan
  [ -n "$value" ] || return 0
  note_scan="$(mktemp)" || fail "mktemp failed"
  printf '%s\n' "$value" > "$note_scan"
  if agent_memory_file_has_secret_content "$note_scan"; then
    rm -f "$note_scan"
    echo "fail-ledger: refused; $flag looks like a credential; nothing was recorded" >&2
    exit 3
  fi
  rm -f "$note_scan"
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Print the fingerprint of a command string on stdout.
# Unresolved failures recorded for a fingerprint, counted the way `check` counts
# them: a resolve row zeroes the run. Used to decide when a repeat has become a
# pattern worth an outside read.
unresolved_fails_for() {
  OMS_FP="$1" python3 -X utf8 -I -B - "$LEDGER" <<'COUNT'
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

# A hint does not grant authority to call another model.
advise_hint_if_repeated() {
  local fp="$1"
  local fails="$2"
  local threshold="${OMS_ADVISE_AFTER_FAILURES:-2}"

  case "$threshold" in ''|*[!0-9]*) threshold=2 ;; esac
  [ "$threshold" -gt 0 ] || return 0
  [ "$fails" -ge "$threshold" ] || return 0
  printf 'fail-ledger: %s has failed %dx unresolved; reassess the evidence; optional outside read if authorized (oms advise --prompt "...")\n' \
    "$fp" "$fails" >&2
}

# Facts for the mechanical advisor triggers in lib/advise_triggers.py, one per
# line: "conflict" while the index holds unmerged paths (a merge, rebase,
# cherry-pick or stash pop stopped on a conflict), and "contract:<reason>" for
# the first changed path (vs the upstream merge-base, else HEAD; untracked
# included) that the gate's own critical-path list names. No second list.
# Repository config that these reads would run (fsmonitor, filter drivers,
# includes) yields no signals: the hints are advisory and must never execute it.
advise_signals() {
  local base path reason
  oms_git_assert_safe_execution_config "$STATE_ROOT" diff-read >/dev/null 2>&1 || return 0
  if git -C "$STATE_ROOT" ls-files -u 2>/dev/null | grep -q .; then
    echo conflict
  fi
  base="$(git -C "$STATE_ROOT" merge-base HEAD '@{upstream}' 2>/dev/null)" || base=HEAD
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    if reason="$(oms_critical_path_reason "$path")"; then
      echo "contract:$reason"
      break
    fi
  done < <(git -C "$STATE_ROOT" diff --name-only --no-renames "$base" -- 2>/dev/null
    git -C "$STATE_ROOT" ls-files --others --exclude-standard 2>/dev/null)
  return 0
}

fingerprint_of() {
  OMS_CMD="$1" python3 -X utf8 -I -B - <<'PY'
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
    --outbound-evidence) [ "$#" -ge 2 ] || fail "--outbound-evidence requires tier,pattern,source,line"; OUTBOUND_EVIDENCE+=("$2"); shift 2 ;;
    --fingerprint) [ "$#" -ge 2 ] || fail "--fingerprint requires a value"; FINGERPRINT="$2"; shift 2 ;;
    --how) [ "$#" -ge 2 ] || fail "--how requires text"; HOW="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --unresolved) UNRESOLVED_ONLY=1; shift ;;
    --stalls) STALLS=1; shift ;;
    --json) AS_JSON=1; shift ;;
    --limit)
      [ "$#" -ge 2 ] || fail "--limit requires a count"
      case "$2" in ''|*[!0-9]*) fail "--limit must be a non-negative integer" ;; esac
      LIMIT="$2"; shift 2 ;;
    --ignore-state) IGNORE_STATE=1; shift ;;
    record|check|resolve|list|show) ACTION="$1"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

ACTION="${ACTION:-list}"
[ "$AS_JSON" -eq 0 ] || [ "$ACTION" = "list" ] || [ "$ACTION" = "show" ] || fail "--json is only supported by list and show"
[ "$LIMIT" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--limit is only supported by list"
[ "$ACTION" != "show" ] || [ -n "$FINGERPRINT" ] || fail "show requires --fingerprint"
[ "$STALLS" -eq 0 ] || [ "$ACTION" = "list" ] || fail "--stalls is only supported by list"
[ "$STALLS" -eq 0 ] || { [ "$UNRESOLVED_ONLY" -eq 0 ] && [ "$LIMIT" -eq 0 ]; } || fail "--stalls does not combine with --unresolved or --limit"
[ "$IGNORE_STATE" -eq 0 ] || [ "$ACTION" = "check" ] || fail "--ignore-state is only supported by check"
case "$KIND" in
  cmd|hook|verify|plan-run|delegate|patch-land) ;;
  *) fail "--kind must be one of: cmd, hook, verify, plan-run, delegate, patch-land" ;;
esac
STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
# Rows describe STATE_ROOT's tree; a parent's scratch files them in the shared
# ledger so other mains see them and they outlive the scratch.
LEDGER_ROOT="$(oms_state_root "$STATE_ROOT")" || fail "bad --repo"
LEDGER="${OMS_FAIL_LEDGER:-$LEDGER_ROOT/.oms/failures.jsonl}"

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
    [[ "$CMD" = *[![:space:]]* ]] || fail "record requires nonblank --cmd"
    case "$EXIT_CODE" in *[!0-9]*|"") fail "record requires --exit N (non-negative integer)" ;; esac
    valid_outbound_evidence=()
    dropped_outbound_evidence=0
    for evidence in "${OUTBOUND_EVIDENCE[@]}"; do
      valid=1
      case "$evidence" in *$'\n'*|*$'\r'*) valid=0 ;; esac
      IFS=, read -r evidence_tier evidence_pattern evidence_source evidence_line <<EOF
$evidence
EOF
      case "$evidence_tier" in
        secret|machine) ;;
        *) valid=0 ;;
      esac
      case "$evidence_pattern" in
        credential-assignment|authorization|bearer|url-credentials|webhook-url|machine-login|token-assignment|github-token|npm-token|hf-token|gitlab-token|stripe-key|jwt-like|aws-key|google-key|private-key|token-like|home-path|user-path|scratch-path|lustre-path|gpfs-path|beegfs-path|ssh-dir|aws-dir|cluster-field|partition-field|nodelist-field|sbatch-partition|env-secret) ;;
        *) valid=0 ;;
      esac
      case "$evidence_source" in prompt|brief-file|context-bundle|diff) ;; *) valid=0 ;; esac
      case "$evidence_line" in ''|*[!0-9]*) valid=0 ;; *) [ "$evidence_line" -gt 0 ] || valid=0 ;; esac
      if [ "$valid" = 1 ]; then
        valid_outbound_evidence+=("$evidence")
      else
        dropped_outbound_evidence=$((dropped_outbound_evidence + 1))
      fi
    done
    [ "$dropped_outbound_evidence" -eq 0 ] ||
      echo "fail-ledger: dropped $dropped_outbound_evidence invalid outbound evidence entr$( [ "$dropped_outbound_evidence" -eq 1 ] && printf 'y' || printf 'ies')" >&2
    # Refuse secret-tier content; machine paths were already normalized above.
    scan="$(mktemp)" || fail "mktemp failed"
    printf '%s\n%s\n' "$CMD" "$SUMMARY" > "$scan"
    if agent_memory_file_has_secret_content "$scan"; then
      rm -f "$scan"
      echo "fail-ledger: refused; command/summary looks sensitive" >&2
      exit 3
    fi
    rm -f "$scan"
    refuse_secret_note --next "$NEXT"
    fp="$(fingerprint_of "$CMD")"
    mkdir -p "$(dirname "$LEDGER")"
    agent_memory_ensure_oms_ignore_for_path "$LEDGER" 2>/dev/null || true
    row_tmp="$(mktemp)" || fail "mktemp failed"
    outbound_evidence=""
    for evidence in "${valid_outbound_evidence[@]}"; do
      outbound_evidence="${outbound_evidence}${outbound_evidence:+
}${evidence}"
    done
    # Extra advisor triggers (same OMS_ADVISE_AFTER_FAILURES=0 off switch):
    # decided before the append so the row can carry which ones it printed.
    advised_ids=""
    advised_text=""
    if [ "${OMS_ADVISE_AFTER_FAILURES:-2}" != 0 ]; then
      trigger_out="$(advise_signals |
        python3 -X utf8 -I -B "$ROOT_LIB/advise_triggers.py" "$LEDGER" "$fp" 2>/dev/null)" || trigger_out=""
      while IFS=$'\t' read -r trigger_id trigger_text; do
        [ -n "$trigger_id" ] || continue
        advised_ids="${advised_ids}${advised_ids:+,}$trigger_id"
        advised_text="${advised_text}${trigger_text}
"
      done <<< "$trigger_out"
    fi
    FAILURE_CODE="$(OMS_ADVISED="$advised_ids" OMS_OUTBOUND_EVIDENCE="$outbound_evidence" OMS_SCHEMA="$SCHEMA" OMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_AGENT_L="$(oms_detect_agent)" \
      OMS_FP="$fp" OMS_KIND="$KIND" OMS_CMD="$CMD" OMS_EXIT="$EXIT_CODE" OMS_SUMMARY="$SUMMARY" OMS_NEXT="$NEXT" \
      OMS_STATE_FP="$(state_fingerprint)" \
      OMS_RUNTIME_LIB="$ROOT/scripts/lib" OMS_FAILURE_CODE="${FAILURE_CODE:-}" \
      python3 -X utf8 -I -B - "$row_tmp" <<'PY'
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
if os.environ.get("OMS_ADVISED"):
    row["advised"] = os.environ["OMS_ADVISED"].split(",")
evidence = []
for item in os.environ.get("OMS_OUTBOUND_EVIDENCE", "").splitlines():
    try:
        tier, pattern, source, line = item.split(",", 3)
        evidence.append({"tier": tier, "pattern": pattern, "source": source,
                         "line": int(line)})
    except Exception:
        continue
if evidence:
    row["outbound_evidence"] = evidence
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
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    print(json.dumps(row, ensure_ascii=False, allow_nan=False), file=handle)
print(row.get("failure_code", ""))
PY
)"
    oms_with_file_lock "$LEDGER" ledger_append "$LEDGER" "$row_tmp"
    rm -f "$row_tmp"
    echo "fail-ledger: recorded $fp (exit $EXIT_CODE)" >&2
    # Reuse the classified row and one unresolved count; presentation failure
    # must never turn a successful append into a failed record operation.
    fails="$(unresolved_fails_for "$fp")" || fails=0
    case "$FAILURE_CODE" in
      provider_capacity|provider_no_answer|read_guard_changed|state_conflict) ;;
      *)
        if [ "$fails" -gt 0 ] && { [ "$KIND" != hook ] || [ "$fails" -ge 2 ]; }; then
          echo "report: oms report bug --from-failure $fp" >&2
        fi
        ;;
    esac
    advise_hint_if_repeated "$fp" "$fails"
    [ -z "$advised_text" ] || printf '%s' "$advised_text" >&2
    ;;
  check)
    [ -n "$CMD" ] || fail "check requires --cmd"
    [ -f "$LEDGER" ] || exit 0
    fp="$(fingerprint_of "$CMD")"
    OMS_FP="$fp" OMS_STATE_FP="$(state_fingerprint)" OMS_IGNORE_STATE="$IGNORE_STATE" \
      python3 -X utf8 -I -B - "$LEDGER" <<'PY'
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
            "fail-ledger: %s has failed %dx unresolved; reassess the evidence; "
            "optional outside read if authorized (oms advise --prompt \"...\")\n" % (fp, fails))
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
    grep -Fq "\"fingerprint\": \"$FINGERPRINT\"" "$LEDGER" ||
      fail "unknown fingerprint: $FINGERPRINT (see: fail-ledger list)"
    refuse_secret_note --how "$HOW"
    mkdir -p "$(dirname "$LEDGER")"
    row_tmp="$(mktemp)" || fail "mktemp failed"
    OMS_SCHEMA="$SCHEMA" OMS_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_AGENT_L="$(oms_detect_agent)" \
      OMS_FP="$FINGERPRINT" OMS_HOW="$HOW" python3 -X utf8 -I -B - > "$row_tmp" <<'PY'
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
  list|show)
    if [ "$STALLS" -eq 1 ]; then
      # Read-only and advisory: always exit 0; the lib refuses symlinked files.
      stall_args=()
      [ "$AS_JSON" -eq 0 ] || stall_args+=(--json)
      python3 -I -B "$ROOT_LIB/stall_detect.py" --ledger "$LEDGER" \
        --progress "$LEDGER_ROOT/.oms/plan/progress.jsonl" \
        ${stall_args[@]+"${stall_args[@]}"}
      exit 0
    fi
    [ ! -L "$LEDGER" ] || fail "failure ledger must be a regular non-symlink file"
    if [ ! -f "$LEDGER" ]; then
      [ "$ACTION" != "show" ] || fail "unknown fingerprint: $FINGERPRINT (no ledger at $LEDGER)"
      if [ "$AS_JSON" -eq 1 ]; then
        echo '{"schema": 1, "failures": [], "invalid_rows": 0}'
      else
        echo "no failures"
      fi
      exit 0
    fi
    OMS_UNRESOLVED="$UNRESOLVED_ONLY" OMS_JSON="$AS_JSON" OMS_LIMIT="$LIMIT" \
      OMS_SHOW="$([ "$ACTION" = show ] && printf '%s' "$FINGERPRINT")" \
      OMS_MEMORY_COMMON="$ROOT_LIB/agent-memory-common.sh" \
      OMS_HEAD="$(git -c core.fsmonitor=false -C "$STATE_ROOT" rev-parse HEAD 2>/dev/null || printf unborn)" \
      python3 -X utf8 -I -B - "$LEDGER" <<'PY'
import calendar, json, os, sys, time
unresolved_only = os.environ.get("OMS_UNRESOLVED") == "1"
as_json = os.environ.get("OMS_JSON") == "1"
ttl = int(os.environ.get("OMS_HOOK_TTL") or 86400)
now = time.time()
# An unborn repo reads "HEAD\nunborn" here and in each fingerprint (git echoes HEAD).
head = (os.environ.get("OMS_HEAD") or "").rsplit("\n", 1)[-1]
show_fp = os.environ.get("OMS_SHOW") or ""

def head_of(r):
    # "" when the row names no commit: legacy rows and non-git roots
    # ("non-git") have nothing to compare, so each stays its own head.
    h = str(r.get("state_fingerprint") or "").split(":", 1)[0].rsplit("\n", 1)[-1]
    ok = h == "unborn" or (bool(h) and all(c in "0123456789abcdefABCDEF" for c in h))
    return h if ok else ""

def stale_head(r):
    # Failures against another commit are evidence about a tree that is
    # gone; a recurrence across distinct commits is the tree-independent kind.
    # A retry on the same old commit is one observation, however often repeated.
    h = head_of(r)
    return bool(head and h) and h != head

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
                   "expired": 0, "last_expired": None, "heads": set()}
        order.append(fp)
    ev = r.get("event")
    if ev == "resolved":
        agg[fp]["resolved"] = True
        agg[fp]["count"] = 0
        agg[fp]["heads"] = set()
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
            # A row without a state fingerprint is its own head, so legacy
            # rows keep counting as separate recurrences.
            agg[fp]["heads"].add(head_of(r) or "row:%d" % agg[fp]["count"])
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
    if show_fp and fp != show_fp:
        continue
    if unresolved_only and not show_fp and (d["resolved"] or d["count"] == 0):
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
        "failure_code": last.get("failure_code"),
    }
    if row["resolved"] or row["expired"]:
        row["attention"] = "none"
        row["actionable"] = False
        row["retiring"] = False
    elif row["kind"] == "hook" and row["count"] < 2:
        row["attention"] = "retiring"
        row["actionable"] = False
        row["retiring"] = True
    elif len(d["heads"]) < 2 and stale_head(last):
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
def tag_of(r):
    if r["resolved"]:
        return "resolved"
    if r["expired"]:
        return "EXPIRED"
    if r["attention"] == "stale":
        return "stale"
    return "OPEN"

if show_fp:
    if not rows:
        sys.stderr.write("fail-ledger: unknown fingerprint: %s (see: oms fail-ledger list --unresolved)\n" % show_fp)
        sys.exit(1)
    r = rows[0]
    # Rows stored before the write path scanned --next/--how may hold a
    # credential; show withholds a note the shared scanner flags or cannot judge.
    import subprocess, tempfile
    def note_flagged(value):
        try:
            with tempfile.NamedTemporaryFile("w", encoding="utf-8") as handle:
                handle.write(value + "\n")
                handle.flush()
                done = subprocess.run(
                    ["bash", "-c", '. "$1" && agent_memory_file_has_secret_content "$2"',
                     "scan", os.environ["OMS_MEMORY_COMMON"], handle.name],
                    capture_output=True, check=False, timeout=30, stdin=subprocess.DEVNULL)
            return done.returncode == 0
        except Exception:
            return True
    for key in ("next", "how"):
        if isinstance(r.get(key), str) and r[key] and note_flagged(r[key]):
            r[key] = "(withheld: looks like a credential)"
    if as_json:
        print(json.dumps({"schema": 1, "failure": r, "invalid_rows": invalid}, ensure_ascii=False))
    else:
        for k, v in (("fingerprint", r["fingerprint"]), ("state", tag_of(r)),
                     ("attention", r.get("attention")),
                     ("count", r["count"]), ("exit", r["exit"]), ("kind", r["kind"]),
                     ("last", r["ts"]), ("cmd", r["cmd"]), ("summary", r["summary"]),
                     ("next", r.get("next")), ("how", r.get("how"))):
            if v is not None:
                print("%-11s %s" % (k + ":", v))
    sys.exit(0)
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
        tag = tag_of(r)
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
