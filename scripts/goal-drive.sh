#!/usr/bin/env bash
set -euo pipefail

# Bounded goal driver: loop [acceptance -> one plan-run task -> exact commit]
# over an EXISTING human-approved plan until the plan's acceptance command
# passes, the cycle cap is hit, tasks run out, or anything looks unsafe. This
# is the mechanized form of the parent's re-orientation between tasks — the
# acceptance check, stuck check, and park reasons ARE the re-orientation —
# not an unbounded autonomy loop. Deliberately absent in v1, in line with the
# cross-family design review: no plan generation, no replanning, no automatic
# lease reclaim, no default repair. The driver only ever commits the exact
# files the admission gate applied; any surprise in the tree parks the run.
#
# Preconditions: clean tree (it refuses otherwise — run it in a dedicated
# worktree when other sessions share the checkout), a plan with tasks and an
# acceptance command (agent-plan init --goal ... --accept CMD). The
# acceptance command is snapshotted at start; if it changes mid-run the run
# parks rather than judge against a moved goalpost.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/model-routing.sh
. "$ROOT/scripts/lib/model-routing.sh"
# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"

REPO="$PWD"
PROVIDER="codex"
MAX_CYCLES=3
AUTO_REPAIR=0
RUN_ID_OVERRIDE=""
RETRY_KNOWN=0
ALLOW_VERIFIER_CHANGE=0
# Empty when not allowed; carries the literal flag when it is. Every landing
# entrance (plan-run and the direct patch-land retries) expands it the same
# way, so contract-level consent cannot cover one landing path and miss
# another.
AVC_FLAG=""
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
PROVIDER_TIMEOUT="${OMS_PEER_TIMEOUT:-5m}"
EXPECTED_REF=""

usage() {
  cat <<'EOF'
Usage: goal-drive.sh [--repo PATH] [--to PROVIDER] [--max-cycles N] [--run-id ID]
                     [--auto-repair] [--retry-known] [--model MODEL]
                     [--fallback-model MODEL] [--reasoning-effort E]
                     [--provider-timeout DUR] [--expected-ref REF]

Drive an existing agent-plan toward its acceptance command: each cycle runs
acceptance first (pass = done), otherwise executes exactly one plan task via
plan-run --next, freezes its reviewed patch, then lands and commits exactly
those bytes (and nothing else).

Stops by design: acceptance pass; --max-cycles (the hard terminator, default
3); no actionable task while acceptance still fails; identical tree + same
failing acceptance twice (stuck); acceptance command changed mid-run; any
mismatch between the admitted patch and the working tree. Every terminal
writes a reason row to .oms/plan/progress.jsonl and a fail-ledger entry when
it parked; a later run whose acceptance passes resolves those park rows.

  --repo PATH     Repository (default: current directory). Tree must be clean.
  --to PROVIDER   Worker for plan-run delegation (default: codex).
  --max-cycles N  Hard cycle cap (default 3, max 10).
  --run-id ID      Optional caller correlation receipt (1..64 safe characters).
  --auto-repair   Run one same-lease repair delegation after a failed landing,
                  then retry landing once before parking.
  --retry-known   Forward the explicit unchanged-failure retry to plan-run.
  --allow-verifier-change  Forward to plan-run and every landing retry: permit
                  a patch that touches its own verifier (base floor still
                  applies). Meant to arrive from the autopilot contract.
  --model MODEL   Exact worker model; disables implicit fallback.
  --fallback-model M  Explicit one-shot capacity fallback model.
  --reasoning-effort E  auto, low, medium, high, xhigh, max, or ultra.
  --provider-timeout DUR Worker wall-clock timeout (for example 15m).
  --expected-ref REF  Parent-bound full work ref (refs/heads/...). Goal-drive
                      refuses acceptance, delegation, and commit on another ref.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --to) [ "$#" -ge 2 ] || fail "--to requires a provider"; PROVIDER="$2"; shift 2 ;;
    --max-cycles)
      [ "$#" -ge 2 ] || fail "--max-cycles requires a count"
      case "$2" in *[!0-9]*|"") fail "--max-cycles requires a positive integer" ;; esac
      [ "$2" -ge 1 ] && [ "$2" -le 10 ] || fail "--max-cycles must be 1..10"
      MAX_CYCLES="$2"; shift 2 ;;
    --run-id)
      [ "$#" -ge 2 ] || fail "--run-id requires an id"
      [ -n "$2" ] || fail "--run-id requires a non-empty id"
      RUN_ID_OVERRIDE="$2"; shift 2 ;;
    --auto-repair) AUTO_REPAIR=1; shift ;;
    --retry-known) RETRY_KNOWN=1; shift ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; AVC_FLAG="--allow-verifier-change"; shift ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires a value"; MODEL="$2"; shift 2 ;;
    --fallback-model) [ "$#" -ge 2 ] || fail "--fallback-model requires a value"; FALLBACK_MODEL="$2"; shift 2 ;;
    --reasoning-effort) [ "$#" -ge 2 ] || fail "--reasoning-effort requires a value"; REASONING_EFFORT="$2"; shift 2 ;;
    --provider-timeout) [ "$#" -ge 2 ] || fail "--provider-timeout requires a duration"; PROVIDER_TIMEOUT="$2"; shift 2 ;;
    --expected-ref) [ "$#" -ge 2 ] || fail "--expected-ref requires a ref"; EXPECTED_REF="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
  fail "goal-drive is parent-only; a harness child cannot commit reviewed work"
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
PROVIDER_TIMEOUT_SECONDS="$(python3 - "$PROVIDER_TIMEOUT" <<'PY' | tr -d '\r'
import re, sys
match = re.fullmatch(r"([1-9][0-9]*)([smh]?)", sys.argv[1])
if not match:
    raise SystemExit(2)
seconds = int(match.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[match.group(2)]
if seconds > 24 * 60 * 60:
    raise SystemExit(2)
print(seconds)
PY
)" || fail "--provider-timeout must be a positive duration up to 24h (for example 15m)"

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
PROVIDER="$(oms_normalize_provider "$PROVIDER")" ||
  fail "unknown provider: use codex, claude, or antigravity (agy)"
if [ -n "$EXPECTED_REF" ]; then
  case "$EXPECTED_REF" in
    refs/heads/*) ;;
    *) fail "--expected-ref must be a full refs/heads/... ref" ;;
  esac
  expected_branch="${EXPECTED_REF#refs/heads/}"
  git check-ref-format --branch "$expected_branch" >/dev/null 2>&1 ||
    fail "--expected-ref is not a canonical branch ref"
  [ "refs/heads/$expected_branch" = "$EXPECTED_REF" ] ||
    fail "--expected-ref is not canonical"
fi
if [ -n "$RUN_ID_OVERRIDE" ]; then
  case "$RUN_ID_OVERRIDE" in
    *[!A-Za-z0-9._-]*) fail "--run-id has unsafe characters" ;;
  esac
  [ "${#RUN_ID_OVERRIDE}" -le 64 ] || fail "--run-id must be at most 64 characters"
fi

# Repository hooks are mutable code outside the reviewed task/verifier
# contract. Disable them for this process and every Git subprocess launched by
# plan delegation, admission, landing, and exact ref publication. Preserve any
# caller-supplied command-scope Git config entries and append the stronger
# hooksPath setting last.
GOAL_GIT_CONFIG_COUNT="${GIT_CONFIG_COUNT:-0}"
case "$GOAL_GIT_CONFIG_COUNT" in
  0|[1-9]|[1-9][0-9]*) ;;
  *) fail "GIT_CONFIG_COUNT must be a canonical non-negative integer" ;;
esac
[ "$GOAL_GIT_CONFIG_COUNT" -le 64 ] || fail "GIT_CONFIG_COUNT is too large"
GOAL_HOOK_KEY="GIT_CONFIG_KEY_$GOAL_GIT_CONFIG_COUNT"
GOAL_HOOK_VALUE="GIT_CONFIG_VALUE_$GOAL_GIT_CONFIG_COUNT"
printf -v "$GOAL_HOOK_KEY" '%s' core.hooksPath
printf -v "$GOAL_HOOK_VALUE" '%s' /dev/null
export "${GOAL_HOOK_KEY?}" "${GOAL_HOOK_VALUE?}"
GIT_CONFIG_COUNT=$((GOAL_GIT_CONFIG_COUNT + 1))
export GIT_CONFIG_COUNT

PLAN_FILE="$REPO/.oms/plan/tasks.json"
PROGRESS="$REPO/.oms/plan/progress.jsonl"
[ -f "$PLAN_FILE" ] || fail "no plan at $PLAN_FILE; create one first (agent-plan init/add)"
python3 "$ROOT/scripts/lib/durable-jsonl.py" check "$PROGRESS" ||
  fail "progress.jsonl must be a repo-local regular non-symlink file"

read_accept_cmd() {
  python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
print(d.get("accept", "") or "")
' "$PLAN_FILE" | tr -d '\r'
}

plan_has_unfinished_work() {  # prints 1 when any task is not done
  # Fails closed: a plan this cannot read is not evidence that the goal is met.
  python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
    tasks = list((d.get("tasks") or {}).values())
except Exception:
    print("1")
    sys.exit(0)
print("1" if any(t.get("state") != "done" for t in tasks) else "")
' "$PLAN_FILE" | tr -d '\r'
}

acceptance_ever_failed() {  # prints 1 when this contract failed at an ancestor
  # The one thing that makes a pass meaningful: the same acceptance command,
  # by digest, recorded a failure at a commit this HEAD descends from. Work
  # landed in between and turned it around. Without such a receipt the command
  # has never demonstrated it can fail at all, and passing proves nothing.
  local candidates row_sha
  candidates="$(OMS_GD_ACCEPT16="${ACCEPT_SNAPSHOT:0:16}" python3 -c '
import json, os, sys
want = os.environ["OMS_GD_ACCEPT16"]
seen = []
try:
    handle = open(sys.argv[1], encoding="utf-8")
except OSError:
    sys.exit(0)
with handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict) or row.get("kind") != "acceptance":
            continue
        if row.get("status") != "fail" or row.get("accept_sha256") != want:
            continue
        sha = row.get("base_sha")
        if isinstance(sha, str) and sha and sha not in seen:
            seen.append(sha)
print("\n".join(seen[-20:]))
' "$PROGRESS" | tr -d '\r')"
  [ -n "$candidates" ] || { printf ''; return 0; }
  local head
  head="$(git -C "$REPO" rev-parse HEAD 2>/dev/null | tr -d '\r')"
  while IFS= read -r row_sha; do
    [ -n "$row_sha" ] || continue
    [ "$row_sha" != "$head" ] || continue
    if git -C "$REPO" merge-base --is-ancestor "$row_sha" "$head" 2>/dev/null; then
      printf '1'
      return 0
    fi
  done <<EOF
$candidates
EOF
  printf ''
}

ACCEPT_CMD="$(read_accept_cmd)"
[ -n "$ACCEPT_CMD" ] ||
  fail "plan has no acceptance command; set one: agent-plan init --goal ... --accept CMD"
ACCEPT_SNAPSHOT="$(printf '%s' "$ACCEPT_CMD" | oms_sha256_stream 2>/dev/null || echo unhashed)"
RUN_ID="${RUN_ID_OVERRIDE:-gd-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
TERMINAL_RECEIPT="$(python3 -c 'import os; print(os.urandom(32).hex())' | tr -d '\r')" ||
  fail "cannot generate a private terminal receipt"
case "$TERMINAL_RECEIPT" in *[!0-9a-f]*|"") fail "invalid terminal receipt" ;; esac
[ "${#TERMINAL_RECEIPT}" -eq 64 ] || fail "invalid terminal receipt"
CYCLE=0
INTENT_ID=""
INTENT_TASK=""
INTENT_BASE=""
INTENT_REF=""
INTENT_PATCH=""
INTENT_PATCH_SHA=""
INTENT_PATHS="[]"
INTENT_TITLE=""
INTENT_LEASE=""
INTENT_VERIFY_SHA=""
INTENT_PROVIDER=""
INTENT_PHASE=""
INTENT_INDEX_PATH=""
INTENT_INDEX_LOCK=""
INTENT_INDEX_SHA=""
INTENT_INDEX_OWNED=0
INTENT_INDEX_ERROR=""
INTENT_NEW_COMMIT=""

TASK_RECEIPT_ID=""
TASK_RECEIPT_TITLE=""
TASK_RECEIPT_STATE=""
TASK_RECEIPT_PROVIDER=""
TASK_RECEIPT_LEASE=""
TASK_RECEIPT_REVIEW_LEASE=""
TASK_RECEIPT_VERIFY=""
TASK_RECEIPT_PATCH=""
TASK_RECEIPT_ARTIFACT=""
TASK_RECEIPT_EXECUTOR_ID=""
TASK_RECEIPT_EXECUTOR_SOUL=""
TASK_RECEIPT_REPAIR_COUNT=0
TASK_RECEIPT_SHA=""

progress_append() {
  python3 "$ROOT/scripts/lib/durable-jsonl.py" append "$PROGRESS"
}

terminal_row() {  # STATUS REASON
  [ "${OMS_GOAL_DRIVE_TEST_FAIL_TERMINAL_APPEND:-0}" != 1 ] || return 1
  OMS_GD_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_GD_RUN="$RUN_ID" \
    OMS_GD_RECEIPT="$TERMINAL_RECEIPT" \
    OMS_GD_CYCLE="$CYCLE" OMS_GD_STATUS="$1" OMS_GD_REASON="$2" \
    OMS_GD_SHA="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unborn)" \
    python3 -c '
import json, os
print(json.dumps({
    "schema": 1, "kind": "terminal", "ts": os.environ["OMS_GD_TS"],
    "run_id": os.environ["OMS_GD_RUN"], "cycle": int(os.environ["OMS_GD_CYCLE"]),
    "receipt": os.environ["OMS_GD_RECEIPT"],
    "base_sha": os.environ["OMS_GD_SHA"], "status": os.environ["OMS_GD_STATUS"],
    "reason": os.environ["OMS_GD_REASON"],
}, ensure_ascii=False))
' | progress_append 2>/dev/null
}

terminal_result() {  # STATUS REASON
  case "$1" in park|done) ;; *) return 1 ;; esac
  case "$2" in ""|*[!A-Za-z0-9._-]*) return 1 ;; esac
  [ "${#2}" -le 128 ] || return 1
  printf 'goal-drive: terminal-v1 run=%s receipt=%s status=%s reason=%s\n' \
    "$RUN_ID" "$TERMINAL_RECEIPT" "$1" "$2"
}

park() {  # REASON NEXT
  local recorded
  if ! terminal_row park "$1"; then
    echo "error: cannot durably append the goal-drive terminal row" >&2
    exit 2
  fi
  # The ledger fingerprints the command, and `goal-drive $RUN_ID` made every
  # park a singleton: its digit mask only collapses runs of 8+ digits, so the
  # RUN_ID's time-of-day and pid survive and the count per fingerprint can
  # never reach the advise threshold. The contract — this reason against this
  # acceptance command — is what repeats across runs, so that is the cmd;
  # lineage belongs in the summary.
  # The park reason is already a judgment; where it maps onto the canonical
  # taxonomy with certainty, say so explicitly instead of letting the
  # classifier guess from prose. Unmapped reasons classify from the summary.
  local park_code=""
  case "$1" in
    acceptance-*) park_code=contract_invalid ;;
    tasks-exhausted|cycles-exhausted|replan-budget-used) park_code=budget_exhausted ;;
    stuck) park_code=verifier_failed ;;
  esac
  recorded="$("$ROOT/scripts/fail-ledger.sh" record --repo "$REPO" --kind plan-run \
    --cmd "goal-drive park reason=$1 accept=$ACCEPT_SNAPSHOT" --exit 3 \
    --summary "parked: $1 ($RUN_ID)" --next "$2" \
    ${park_code:+--failure-code "$park_code"} 2>&1 >/dev/null || true)"
  echo "goal-drive: parked run=$RUN_ID cycle=$CYCLE reason=$1"
  echo "goal-drive: parent-agent next: $2"
  # record names `oms advise` once the same park repeats, and this used to
  # send that line to /dev/null — the threshold crossing has to be visible at
  # the one moment the run is ending on it.
  printf '%s\n' "$recorded" | grep -F 'oms advise' || true
  # A park is the day's judgment the journal never had: the reason is a
  # blocker and the parent's next step is a next action, both already bound
  # by this function's own argument validation. Observed before the canonical
  # line, and silenced on both streams -- the caller treats any trailing child
  # output as a safe failure, so a degraded journal must stay invisible here.
  work_journal_observe "$REPO" goal-drive "$PROGRESS" \
    --source-id "$RUN_ID:park" --outcome "Goal drive parked: $1" \
    --outcome-status "parked" --verification-status not_verified \
    --blocker "$1" --next-action "$2" >/dev/null 2>&1 || true
  # This canonical result is the one final non-empty output line. The caller
  # treats duplicates or trailing child output as a safe failure, and uses the
  # mutable progress row only to cross-check this bound status and reason.
  if ! terminal_result park "$1"; then
    echo "error: cannot emit the goal-drive terminal result" >&2
    exit 2
  fi
  exit 3
}

# The ledger's contract is resolved-on-success and nothing honored it for
# parks: a goal that finally passes left every earlier park OPEN in the resume
# hook, the inbox, and every advise prompt until a human swept it. One list
# call, bounded by the open park rows filed against this run's acceptance
# contract — parks of a different goal stay open, because they still are.
resolve_parks() {
  local fps fp
  fps="$("$ROOT/scripts/fail-ledger.sh" --repo "$REPO" list --unresolved --json 2>/dev/null |
    OMS_GD_ACCEPT="$ACCEPT_SNAPSHOT" python3 -c '
import json, os, sys
try:
    rows = json.load(sys.stdin).get("failures")
except Exception:
    raise SystemExit(0)
suffix = "accept=" + os.environ["OMS_GD_ACCEPT"]
for row in rows if isinstance(rows, list) else []:
    cmd = row.get("cmd") or ""
    fp = row.get("fingerprint") or ""
    if fp and cmd.startswith("goal-drive park ") and cmd.endswith(suffix):
        print(fp)
' 2>/dev/null | tr -d '\r' || true)"
  [ -n "$fps" ] || return 0
  while IFS= read -r fp; do
    [ -n "$fp" ] || continue
    "$ROOT/scripts/fail-ledger.sh" --repo "$REPO" resolve --fingerprint "$fp" \
      --how "acceptance passed in $RUN_ID" >/dev/null 2>&1 || true
  done <<EOF
$fps
EOF
}

# `status --porcelain` and ordinary diffs intentionally trust index hints.
# skip-worktree and assume-unchanged can therefore hide tracked bytes from the
# clean-tree, scope, and acceptance fences. Autonomous publication requires a
# plain index; reject the hint itself before inspecting or consuming work.
guard_plain_index() {
  if ! python3 - "$REPO" <<'PY'
import subprocess, sys

raw = subprocess.check_output(
    ["git", "-C", sys.argv[1], "ls-files", "-v", "-z"],
    stderr=subprocess.DEVNULL)
for entry in raw.split(b"\0"):
    if not entry:
        continue
    flag = entry[:1]
    if flag == b"S" or b"a" <= flag <= b"z":
        raise SystemExit(2)
PY
  then
    park "hidden-index-flags" "clear every skip-worktree/assume-unchanged bit, inspect the real tracked bytes, then retry"
  fi
}

guard_git_authority() {
  command -v oms_git_assert_safe_execution_config >/dev/null 2>&1 ||
    fail "installed Git execution guard is unavailable"
  command -v oms_git_assert_plain_index >/dev/null 2>&1 ||
    fail "installed Git index guard is unavailable"
  oms_git_assert_safe_execution_config "$REPO" ||
    park "unsafe-git-execution-config" "remove executable local/worktree/command-scope Git config, then retry"
  oms_git_assert_plain_index "$REPO" ||
    park "hidden-index-flags" "clear every skip-worktree/assume-unchanged bit, inspect the real tracked bytes, then retry"
  # Keep the local parser as defense in depth and as an explicit Bash 3.2 /
  # older-install compatibility assertion for this commit-capable boundary.
  guard_plain_index
}

guard_expected_ref() {
  local current_ref
  [ -z "$EXPECTED_REF" ] && return 0
  current_ref="$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')"
  [ "$current_ref" = "$EXPECTED_REF" ] ||
    park "expected-ref-moved" \
      "restore $EXPECTED_REF; goal-drive authority does not follow same-SHA branch switches"
}

git_status_porcelain() {
  git -c core.fsmonitor=false -C "$REPO" \
    status --porcelain --untracked-files=all
}

# plan-run and the optional repair are long-lived, synchronous provider
# phases. Run each in a fresh process group owned by a small supervisor; a
# signal to goal-drive is forwarded, given a bounded grace period, escalated,
# and reaped before the driver itself exits.
GOAL_PHASE_PID=""
GOAL_PHASE_OUT_TMP=""
GOAL_PHASE_OUT=""
goal_phase_forward_signal() {  # SIGNAL EXIT_CODE
  local signal_name="$1" exit_code="$2"
  trap - HUP INT TERM
  if [ -n "$GOAL_PHASE_PID" ]; then
    kill -s "$signal_name" "$GOAL_PHASE_PID" 2>/dev/null || true
    wait "$GOAL_PHASE_PID" 2>/dev/null || true
  fi
  [ -z "$GOAL_PHASE_OUT_TMP" ] || rm -f "$GOAL_PHASE_OUT_TMP"
  exit "$exit_code"
}

goal_phase_run() {  # COMMAND [ARGV...]; output -> GOAL_PHASE_OUT
  local supervisor_rc=0 phase_rc phase_wall
  GOAL_PHASE_OUT_TMP="$(agent_memory_mktemp)" || return 126
  trap 'goal_phase_forward_signal HUP 129' HUP
  trap 'goal_phase_forward_signal INT 130' INT
  trap 'goal_phase_forward_signal TERM 143' TERM
  set +e
  # The provider already has its own shorter timeout. The outer wall is a
  # fail-closed ceiling and, critically, uses one cross-platform supervisor
  # implementation with POSIX process groups and Windows taskkill /T /F.
  # Keep an explicit `bash script` in argv: native Windows Python cannot
  # CreateProcess a shebang-only .sh file, while Git Bash is documented core.
  # One routed delegation may consume primary + safeguard retry + explicit or
  # capacity fallback attempts. Preserve the inner per-attempt timeout rather
  # than cutting off a legitimate fallback sequence halfway through.
  phase_wall=$((PROVIDER_TIMEOUT_SECONDS * 5 + 60))
  python3 "$ROOT/scripts/lib/autopilot-receipt.py" supervise \
    --wall "$phase_wall" --kill-after 1 --label goal-provider -- \
    "$@" >"$GOAL_PHASE_OUT_TMP" 2>&1 &
  GOAL_PHASE_PID=$!
  wait "$GOAL_PHASE_PID" || supervisor_rc=$?
  GOAL_PHASE_PID=""
  trap - HUP INT TERM
  set -e
  GOAL_PHASE_OUT="$(cat "$GOAL_PHASE_OUT_TMP" 2>/dev/null || true)"
  phase_rc="$supervisor_rc"
  rm -f "$GOAL_PHASE_OUT_TMP"
  GOAL_PHASE_OUT_TMP=""
  return "$phase_rc"
}

# --- Durable land -> commit intent -----------------------------------------
# plan-run deliberately does not commit. The old driver learned the patch path
# only after plan-run had already landed it, leaving an unrecorded crash window
# before commit. Freeze the reviewed patch and append an exact intent BEFORE
# landing. On restart, prove whether those bytes are applicable, applied, or
# already committed; never call the provider again for an established intent.
intent_write() {  # PHASE REASON
  local phase="$1"
  local reason="${2:-}"

  INTENT_PHASE="$phase"

  OMS_GD_TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)" OMS_GD_RUN="$RUN_ID" \
    OMS_GD_CYCLE="$CYCLE" OMS_GD_INTENT="$INTENT_ID" \
    OMS_GD_PHASE="$phase" OMS_GD_REASON="$reason" \
    OMS_GD_TASK="$INTENT_TASK" OMS_GD_BASE="$INTENT_BASE" \
    OMS_GD_REF="$INTENT_REF" \
    OMS_GD_PATCH="$INTENT_PATCH" OMS_GD_PATCH_SHA="$INTENT_PATCH_SHA" \
    OMS_GD_PATHS="$INTENT_PATHS" OMS_GD_TITLE="$INTENT_TITLE" \
    OMS_GD_LEASE="$INTENT_LEASE" OMS_GD_VERIFY_SHA="$INTENT_VERIFY_SHA" \
    OMS_GD_PROVIDER="$INTENT_PROVIDER" \
    python3 - <<'PY' | progress_append
import json, os
paths = json.loads(os.environ["OMS_GD_PATHS"])
print(json.dumps({
    "schema": 1,
    "kind": "commit-intent",
    "ts": os.environ["OMS_GD_TS"],
    "run_id": os.environ["OMS_GD_RUN"],
    "cycle": int(os.environ["OMS_GD_CYCLE"]),
    "intent_id": os.environ["OMS_GD_INTENT"],
    "phase": os.environ["OMS_GD_PHASE"],
    "reason": os.environ["OMS_GD_REASON"],
    "task_id": os.environ["OMS_GD_TASK"],
    "base_sha": os.environ["OMS_GD_BASE"],
    "head_ref": os.environ["OMS_GD_REF"],
    "patch": os.environ["OMS_GD_PATCH"],
    "patch_sha256": os.environ["OMS_GD_PATCH_SHA"],
    "paths": paths,
    "title": os.environ["OMS_GD_TITLE"],
    "provider": os.environ["OMS_GD_PROVIDER"],
    "lease_id": os.environ["OMS_GD_LEASE"],
    "verify_sha256": os.environ["OMS_GD_VERIFY_SHA"],
}, ensure_ascii=False, sort_keys=True))
PY
}

latest_open_intent() {
  [ -f "$PROGRESS" ] || return 0
  python3 - "$REPO" "$PROGRESS" "$PLAN_FILE" <<'PY' | tr -d '\r'
import hashlib, json, os, pathlib, re, subprocess, sys, tempfile

repo = os.path.realpath(sys.argv[1])
progress = sys.argv[2]
plan_file = sys.argv[3]
ident = re.compile(r"^[A-Za-z0-9._-]+$")
sha = re.compile(r"^[0-9a-f]{64}$")
base_re = re.compile(r"^[0-9a-f]{40,64}$")
open_phases = ("prepared", "repairing", "landed")

try:
    plan = json.load(open(plan_file, encoding="utf-8"))
    tasks = plan.get("tasks", {})
    if not isinstance(tasks, dict):
        raise ValueError("tasks")
except Exception:
    raise SystemExit(6)

def text(row, name):
    value = row.get(name, "")
    if not isinstance(value, str) or "\x00" in value:
        raise ValueError(name)
    return value

def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()

def repo_file(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise ValueError("receipt path")
    return value if os.path.isabs(value) else os.path.join(repo, value)

def patch_paths(base, path):
    tree = expected_tree(base, path)
    raw = subprocess.check_output(
        ["git", "-C", repo, "diff-tree", "--no-commit-id", "--no-renames",
         "--name-only", "-r", "-z", base, tree],
        stderr=subprocess.DEVNULL)
    decoded = sorted({value.decode("utf-8", "surrogateescape")
                      for value in raw.split(b"\0") if value})
    if not decoded:
        raise ValueError("empty patch")
    return decoded, tree

def expected_tree(base, patch):
    with tempfile.TemporaryDirectory(prefix="oms-goal-candidate-") as directory:
        index = os.path.join(directory, "index")
        env = dict(os.environ, GIT_INDEX_FILE=index)
        subprocess.check_call(
            ["git", "-C", repo, "read-tree", base], env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.check_call(
            ["git", "-C", repo, "apply", "--cached", "--binary", patch], env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return subprocess.check_output(
            ["git", "-C", repo, "write-tree"], env=env,
            stderr=subprocess.DEVNULL, text=True).strip()

def validate(row):
    if row.get("schema") != 1 or isinstance(row.get("schema"), bool):
        raise ValueError("schema")
    if row.get("kind") != "commit-intent":
        raise ValueError("kind")
    intent_id = text(row, "intent_id")
    task_id = text(row, "task_id")
    phase = text(row, "phase")
    provider = text(row, "provider")
    lease = text(row, "lease_id")
    base = text(row, "base_sha")
    head_ref = row.get("head_ref")
    # A historical row does not prove which same-SHA branch owned publication
    # authority. Never infer it from whichever checkout happens to resume.
    head_ref_missing = head_ref is None
    if head_ref_missing:
        head_ref = ""
    elif not isinstance(head_ref, str) or "\x00" in head_ref:
        raise ValueError("head_ref")
    patch_sha = text(row, "patch_sha256")
    verify_sha = text(row, "verify_sha256")
    patch_rel = text(row, "patch")
    text(row, "title")
    if not ident.fullmatch(intent_id) or not ident.fullmatch(task_id):
        raise ValueError("id")
    if phase not in open_phases or provider not in ("codex", "claude", "antigravity"):
        raise ValueError("phase/provider")
    if (not ident.fullmatch(lease) or not base_re.fullmatch(base) or
            (not head_ref_missing and
             (not re.fullmatch(r"refs/heads/[A-Za-z0-9._/-]+", head_ref) or
              ".." in head_ref or head_ref.endswith("/") or "//" in head_ref))):
        raise ValueError("lease/base")
    if not sha.fullmatch(patch_sha) or not sha.fullmatch(verify_sha):
        raise ValueError("hash")

    relative = pathlib.PurePosixPath(patch_rel)
    if (relative.is_absolute() or relative.parts[:3] != (".oms", "plan", "commit-patches")
            or len(relative.parts) != 4
            or any(part in ("", ".", "..") for part in relative.parts)):
        raise ValueError("patch path")
    frozen_root = os.path.join(repo, ".oms", "plan", "commit-patches")
    if os.path.realpath(frozen_root) != frozen_root:
        raise ValueError("patch root")
    patch = os.path.join(repo, *relative.parts)
    if (os.path.islink(patch) or os.path.realpath(os.path.dirname(patch)) != frozen_root
            or not os.path.isfile(patch) or digest(patch) != patch_sha):
        raise ValueError("patch receipt")
    recorded_paths = row.get("paths")
    if (not isinstance(recorded_paths, list) or not recorded_paths
            or not all(isinstance(value, str) and value and "\x00" not in value
                       for value in recorded_paths)):
        raise ValueError("paths")
    normalized_paths = []
    for value in recorded_paths:
        parsed = pathlib.PurePosixPath(value)
        if parsed.is_absolute() or any(part in ("", ".", "..") for part in parsed.parts):
            raise ValueError("path")
        normalized_paths.append(value)
    normalized_paths = sorted(set(normalized_paths))
    derived_paths, tree = patch_paths(base, patch)
    if normalized_paths != recorded_paths or derived_paths != normalized_paths:
        raise ValueError("patch paths")

    task = tasks.get(task_id)
    if not isinstance(task, dict) or task.get("id") != task_id:
        raise ValueError("task")
    names = ("title", "state", "provider", "lease_id", "review_lease_id", "verify",
             "patch", "artifact", "executor_id", "executor_soul_sha256")
    if any(not isinstance(task.get(name, ""), str) or "\x00" in task.get(name, "")
           for name in names):
        raise ValueError("task fields")
    allowed = task.get("allowed_paths", [])
    if (not isinstance(allowed, list) or not allowed
            or not all(isinstance(value, str) and value and "\x00" not in value
                       for value in allowed)):
        raise ValueError("allowed paths")
    repair = task.get("repair_count", 0)
    if isinstance(repair, bool) or not isinstance(repair, int) or repair < 0 or repair > 1:
        raise ValueError("repair count")
    state = task["state"]
    if phase == "prepared" and state not in ("review", "landing", "done"):
        raise ValueError("prepared state")
    if phase == "landed" and state not in ("review", "landing", "done"):
        raise ValueError("landed state")
    if phase == "repairing" and state not in ("ready", "claimed", "running", "review", "blocked"):
        raise ValueError("repairing state")
    released_repair = phase == "repairing" and repair == 1 and state in ("ready", "blocked")
    if task["review_lease_id"] != lease:
        raise ValueError("review lease")
    if released_repair:
        if task["provider"] not in ("", provider) or task["lease_id"] not in ("", lease):
            raise ValueError("released repair authority")
    elif task["provider"] != provider or task["lease_id"] != lease:
        raise ValueError("task authority")
    verify = task["verify"].replace("\r", "")
    if not verify or hashlib.sha256(verify.encode("utf-8")).hexdigest() != verify_sha:
        raise ValueError("verify")
    task_patch = repo_file(task["patch"])
    artifact = repo_file(task["artifact"])
    if not os.path.isfile(task_patch) or not os.path.isfile(artifact):
        raise ValueError("task evidence")
    task_patch_sha = digest(task_patch)
    repaired_review = phase == "repairing" and state == "review" and repair == 1
    # A completed same-lease repair intentionally replaces task.patch before
    # goal-drive can freeze it. Keep the old outer row authoritative enough to
    # reach reconcile_commit_intent, which re-freezes the new exact receipt
    # under the same intent id. Every other phase/state must still match bytes.
    if not repaired_review and task_patch_sha != patch_sha:
        raise ValueError("task patch")
    if task["executor_id"] or task["executor_soul_sha256"]:
        raise ValueError("executor receipt")
    return (task_id, base, tree, patch_sha, lease, verify_sha, provider, head_ref)

receipt_fields = (
    "task_id", "base_sha", "head_ref", "patch", "patch_sha256", "paths", "title",
    "provider", "lease_id", "verify_sha256",
)

def same_receipt(left, right):
    return all(left.get(name) == right.get(name) for name in receipt_fields)

def same_repair_identity(left, right):
    fields = ("task_id", "base_sha", "title", "provider", "lease_id", "verify_sha256")
    return all(left.get(name) == right.get(name) for name in fields)

def open_transition_allowed(left, right):
    transition = (left.get("phase"), right.get("phase"))
    if same_receipt(left, right):
        return transition in {
            ("prepared", "prepared"), ("prepared", "repairing"),
            ("prepared", "landed"), ("repairing", "repairing"),
            ("repairing", "prepared"), ("landed", "landed"),
        }
    return transition == ("repairing", "prepared") and same_repair_identity(left, right)

def ref_resolves(ref):
    try:
        subprocess.check_output(
            ["git", "-C", repo, "rev-parse", "--verify", "--quiet",
             ref + "^{commit}"], stderr=subprocess.DEVNULL, text=True)
    except (OSError, subprocess.CalledProcessError):
        return False
    return True

def exact_commit_exists(base, tree, head_ref):
    # The frozen ref is the proof while it resolves, and a sibling branch never
    # inherits it. But deleting a merged work branch is ordinary hygiene, and a
    # name that no longer exists cannot be the only proof: this projection is
    # recomputed every run, so a closed intent would reopen forever and park
    # every later run on intent-ref-moved with no way back. Fall back to the
    # lineage the repository actually kept. Nothing about what counts as the
    # commit is widened -- one parent equal to the frozen base and the frozen
    # tree, both exact, and the caller still requires a committed terminal row.
    lineage = head_ref if ref_resolves(head_ref) else "HEAD"
    try:
        history = subprocess.check_output(
            ["git", "-C", repo, "rev-list", "--first-parent", lineage],
            stderr=subprocess.DEVNULL, text=True)
    except (OSError, subprocess.CalledProcessError):
        return False
    for candidate in history.splitlines():
        try:
            # Inspect the candidate without --first-parent: a merge commit is
            # never an exact autonomous publication even when its first parent
            # and tree happen to match the frozen receipt.
            parents = subprocess.check_output(
                ["git", "-C", repo, "rev-list", "--parents", "-n", "1", candidate],
                stderr=subprocess.DEVNULL, text=True).split()
            if len(parents) != 2 or parents[1] != base:
                continue
            candidate_tree = subprocess.check_output(
                ["git", "-C", repo, "rev-parse", candidate + "^{tree}"],
                stderr=subprocess.DEVNULL, text=True).strip()
        except (OSError, subprocess.CalledProcessError):
            continue
        if candidate_tree == tree:
            return True
    return False

def rebased_patch_on_current_lineage(active):
    relative = pathlib.PurePosixPath(active["patch"])
    patch = os.path.join(repo, *relative.parts)
    old_base = active["base_sha"]
    head_ref = active.get("head_ref", "")
    task = tasks.get(active.get("task_id"), {})
    state = task.get("state") if isinstance(task, dict) else ""
    try:
        symbolic = subprocess.check_output(
            ["git", "-C", repo, "symbolic-ref", "-q", "HEAD"],
            stderr=subprocess.DEVNULL, text=True).strip()
        if symbolic != head_ref:
            return False
        current = subprocess.check_output(
            ["git", "-C", repo, "rev-parse", head_ref],
            stderr=subprocess.DEVNULL, text=True).strip()
    except (OSError, subprocess.CalledProcessError):
        return False
    if current == old_base:
        return False
    if state == "review":
        try:
            dirty = subprocess.check_output(
                ["git", "-c", "core.fsmonitor=false", "-C", repo,
                 "status", "--porcelain", "--untracked-files=all"],
                stderr=subprocess.DEVNULL)
            if dirty:
                return False
            expected_tree(current, patch)
            return True
        except (OSError, subprocess.CalledProcessError):
            return False
    if state != "done":
        return False
    try:
        history = subprocess.check_output(
            ["git", "-C", repo, "rev-list", "--first-parent", head_ref, "--parents"],
            stderr=subprocess.DEVNULL, text=True)
    except (OSError, subprocess.CalledProcessError):
        return False
    for line in history.splitlines():
        values = line.split()
        if len(values) != 2 or values[1] == old_base:
            continue
        try:
            candidate_tree = expected_tree(values[1], patch)
            commit_tree = subprocess.check_output(
                ["git", "-C", repo, "rev-parse", values[0] + "^{tree}"],
                stderr=subprocess.DEVNULL, text=True).strip()
        except (OSError, subprocess.CalledProcessError):
            continue
        if candidate_tree == commit_tree:
            return True
    return False

def abandonment_allowed(active, terminal):
    reason = terminal.get("reason")
    if (active.get("phase") == "prepared"
            and reason == "frozen-base-receipt-mismatch"):
        return rebased_patch_on_current_lineage(active)
    if active.get("phase") != "repairing":
        return False
    task = tasks.get(active.get("task_id"), {})
    state = task.get("state") if isinstance(task, dict) else ""
    repair = task.get("repair_count", 0) if isinstance(task, dict) else 0
    if repair == 1 and not isinstance(repair, bool) and state in ("ready", "blocked"):
        return reason in {
            "repair-released-before-review", "repair-blocked-before-review",
            "review-authority-left-ready", "review-authority-left-blocked",
        }
    if repair == 1 and not isinstance(repair, bool) and state == "review":
        try:
            empty = os.path.getsize(repo_file(task.get("patch", ""))) == 0
        except (OSError, ValueError):
            empty = True
        return empty and reason in ("repair-returned-no-patch", "repaired-review-patch-missing")
    return False

def terminal_allowed(active, terminal, authority):
    if (terminal.get("schema") != 1 or isinstance(terminal.get("schema"), bool)
            or terminal.get("kind") != "commit-intent"
            or terminal.get("intent_id") != active.get("intent_id")
            or not same_receipt(active, terminal)):
        return False
    phase = terminal.get("phase")
    if phase == "committed" and active.get("phase") in ("prepared", "landed"):
        return exact_commit_exists(authority[1], authority[2], authority[7])
    if phase == "abandoned":
        return abandonment_allowed(active, terminal)
    return False

chains = {}
order = []
with open(progress, encoding="utf-8", errors="replace") as handle:
    for line in handle:
        if len(line) > 1024 * 1024:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        if not isinstance(row, dict) or row.get("kind") != "commit-intent":
            continue
        key = row.get("intent_id")
        if not isinstance(key, str) or not ident.fullmatch(key):
            continue
        if key not in chains:
            order.append(key)
            chains[key] = []
        chains[key].append(row)

valid = []
legacy_missing = False
seen = set()
for key in order:
    active = None
    authority = None
    closed = False
    for row in chains[key]:
        phase = row.get("phase")
        if phase in open_phases:
            if closed:
                continue
            try:
                candidate_authority = validate(row)
            except Exception as error:
                print("goal-drive: ignored invalid commit intent %s (%s)" %
                      (key, error), file=sys.stderr)
                continue
            if active is None or open_transition_allowed(active, row):
                active = row
                authority = candidate_authority
            else:
                print("goal-drive: ignored invalid commit-intent transition %s" % key,
                      file=sys.stderr)
        elif phase in ("committed", "abandoned"):
            if active is not None:
                if terminal_allowed(active, row, authority):
                    active = None
                    authority = None
                    closed = True
                else:
                    print("goal-drive: ignored unauthoritative commit-intent terminal %s" % key,
                          file=sys.stderr)
    if active is None:
        continue
    if active.get("head_ref") is None:
        legacy_missing = True
        continue
    if authority in seen:
        continue
    seen.add(authority)
    valid.append(active)
if legacy_missing:
    print(json.dumps({"error": "legacy-open-intent-missing-head-ref"}))
elif len(valid) > 1:
    print(json.dumps({"error": "multiple-open-intents", "count": len(valid)}))
elif valid:
    print(json.dumps(valid[0], ensure_ascii=False))
PY
}

load_intent() {  # JSON
  local values
  case "$1" in
    *'"error": "legacy-open-intent-missing-head-ref"'*)
      park "legacy-intent-missing-head-ref" "inspect and explicitly reconcile the historical commit intent; branch authority cannot be inferred safely"
      ;;
    *'"error": "multiple-open-intents"'*)
      park "multiple-commit-intents" "inspect .oms/plan/progress.jsonl and reconcile each frozen patch manually"
      ;;
  esac
  values="$(printf '%s' "$1" | python3 -c '
import json, pathlib, re, shlex, sys
d=json.load(sys.stdin)
ident=re.compile(r"^[A-Za-z0-9._-]+$")
sha=re.compile(r"^[0-9a-f]{64}$")
base=re.compile(r"^[0-9a-f]{40,64}$")
def text(name):
    value=d.get(name, "")
    if not isinstance(value, str) or "\x00" in value: raise ValueError(name)
    return value
intent_id=text("intent_id"); task_id=text("task_id")
base_sha=text("base_sha"); patch=text("patch")
head_ref=text("head_ref")
patch_sha=text("patch_sha256"); title=text("title")
lease=text("lease_id"); verify_sha=text("verify_sha256")
provider=text("provider"); phase=text("phase")
if not ident.fullmatch(intent_id) or not ident.fullmatch(task_id): raise ValueError("id")
if not base.fullmatch(base_sha) or not sha.fullmatch(patch_sha) or not sha.fullmatch(verify_sha):
    raise ValueError("hash")
if not ident.fullmatch(lease): raise ValueError("lease")
if (not re.fullmatch(r"refs/heads/[A-Za-z0-9._/-]+", head_ref) or
        ".." in head_ref or head_ref.endswith("/") or "//" in head_ref):
    raise ValueError("head_ref")
if provider not in ("codex", "claude", "antigravity"): raise ValueError("provider")
if phase not in ("prepared", "repairing", "landed"): raise ValueError("phase")
p=pathlib.PurePosixPath(patch)
if p.is_absolute() or p.parts[:3] != (".oms", "plan", "commit-patches") or len(p.parts) != 4:
    raise ValueError("patch")
if any(part in ("", ".", "..") for part in p.parts): raise ValueError("patch")
paths=d.get("paths")
if not isinstance(paths, list) or not paths: raise ValueError("paths")
normalized=[]
for value in paths:
    if not isinstance(value, str) or not value or "\x00" in value: raise ValueError("path")
    path_value=pathlib.PurePosixPath(value)
    if path_value.is_absolute() or any(part in ("", ".", "..") for part in path_value.parts):
        raise ValueError("path")
    normalized.append(value)
normalized=sorted(set(normalized))
assignments={
 "INTENT_ID":intent_id,"INTENT_TASK":task_id,"INTENT_BASE":base_sha,
 "INTENT_REF":head_ref,
 "INTENT_PATCH":patch,"INTENT_PATCH_SHA":patch_sha,
 "INTENT_PATHS":json.dumps(normalized,ensure_ascii=True,separators=(",",":")),
 "INTENT_TITLE":title,"INTENT_LEASE":lease,"INTENT_VERIFY_SHA":verify_sha,
 "INTENT_PROVIDER":provider,"INTENT_PHASE":phase,
}
for key,value in assignments.items(): print("%s=%s" % (key,shlex.quote(value)))
' | tr -d '\r')" || park "invalid-commit-intent" "inspect .oms/plan/progress.jsonl"
  eval "$values" || park "invalid-commit-intent" "inspect .oms/plan/progress.jsonl"
}

intent_patch_file() {
  python3 - "$REPO" "$INTENT_PATCH" <<'PY' | tr -d '\r'
import os, pathlib, sys
repo=os.path.realpath(sys.argv[1])
rel=pathlib.PurePosixPath(sys.argv[2])
if rel.is_absolute() or rel.parts[:3] != (".oms", "plan", "commit-patches") or len(rel.parts) != 4:
    raise SystemExit(1)
root=os.path.join(repo, ".oms", "plan", "commit-patches")
if os.path.realpath(root) != root:
    raise SystemExit(1)
target=os.path.join(repo, *rel.parts)
if os.path.islink(target) or os.path.realpath(os.path.dirname(target)) != root:
    raise SystemExit(1)
print(target)
PY
}

intent_patch_paths_json() {  # PATCH
  local expected_tree
  expected_tree="$(intent_expected_tree "$1")" || return 1
  python3 - "$REPO" "$INTENT_BASE" "$expected_tree" <<'PY' | tr -d '\r'
import json, subprocess, sys
raw = subprocess.check_output([
    "git", "-C", sys.argv[1], "diff-tree", "--no-commit-id", "--no-renames",
    "--name-only", "-r", "-z", sys.argv[2], sys.argv[3],
])
decoded = sorted({p.decode("utf-8", "surrogateescape")
                  for p in raw.split(b"\0") if p})
if not decoded:
    raise SystemExit(2)
print(json.dumps(decoded, ensure_ascii=True, separators=(",", ":")))
PY
}

load_task_receipt() {  # TASK_ID
  local task_json values
  task_json="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$1" 2>/dev/null)" || return 1
  values="$(printf '%s' "$task_json" | python3 -c '
import hashlib, json, shlex, sys
d=json.load(sys.stdin)
names=("id","title","state","provider","lease_id","review_lease_id","verify","patch",
       "artifact","executor_id","executor_soul_sha256")
for name in names:
    if not isinstance(d.get(name, ""), str) or "\x00" in d.get(name, ""): raise ValueError(name)
repair=d.get("repair_count", 0)
if isinstance(repair, bool) or not isinstance(repair, int) or repair < 0 or repair > 1:
    raise ValueError("repair_count")
allowed=d.get("allowed_paths", [])
if not isinstance(allowed, list) or not allowed or not all(
        isinstance(x, str) and x and "\x00" not in x for x in allowed):
    raise ValueError("allowed_paths")
assignments={
 "TASK_RECEIPT_ID":d.get("id", ""),"TASK_RECEIPT_TITLE":d.get("title", ""),
 "TASK_RECEIPT_STATE":d.get("state", ""),"TASK_RECEIPT_PROVIDER":d.get("provider", ""),
 "TASK_RECEIPT_LEASE":d.get("lease_id", ""),
 "TASK_RECEIPT_REVIEW_LEASE":d.get("review_lease_id", ""),
 "TASK_RECEIPT_VERIFY":d.get("verify", ""),"TASK_RECEIPT_PATCH":d.get("patch", ""),
 "TASK_RECEIPT_ARTIFACT":d.get("artifact", ""),
 "TASK_RECEIPT_EXECUTOR_ID":d.get("executor_id", ""),
 "TASK_RECEIPT_EXECUTOR_SOUL":d.get("executor_soul_sha256", ""),
 "TASK_RECEIPT_REPAIR_COUNT":str(repair),
}
stable={name:d.get(name, "") for name in names}
stable["repair_count"]=repair
stable["allowed_paths"]=allowed
assignments["TASK_RECEIPT_SHA"]=hashlib.sha256(json.dumps(
 stable,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()
for key,value in assignments.items(): print("%s=%s" % (key,shlex.quote(value)))
' | tr -d '\r')" || return 1
  eval "$values"
}

patch_leaf_replace() {  # SOURCE_BASENAME DEST_BASENAME
  python3 - "$1" "$2" <<'PY'
import os
import stat
import sys

try:
    os.replace(sys.argv[1], sys.argv[2])
    mode = os.lstat(sys.argv[2]).st_mode
except (OSError, ValueError):
    raise SystemExit(1)
if not stat.S_ISREG(mode):
    raise SystemExit(1)
PY
}

intent_prepare() {  # TASK PATCH BASE REF SUFFIX [REUSE_INTENT_ID]
  local task="$1"
  local source_patch="$2"
  local base="$3"
  local head_ref="$4"
  local suffix="${5:-}"
  local reuse_id="${6:-}"
  local frozen_dir frozen_abs frozen_name patch_rel paths_json task_patch_sha source_patch_sha
  local receipt_sha repo_physical plan_physical frozen_physical frozen_after
  local prior_task prior_base prior_lease
  local prior_verify_sha prior_provider prior_ref

  prior_task="$INTENT_TASK"
  prior_base="$INTENT_BASE"
  prior_lease="$INTENT_LEASE"
  prior_verify_sha="$INTENT_VERIFY_SHA"
  prior_provider="$INTENT_PROVIDER"
  prior_ref="$INTENT_REF"

  [ -s "$source_patch" ] || park "no-admitted-patch" "review the task artifact; its patch file is missing"
  load_task_receipt "$task" ||
    park "missing-plan-task" "the reviewed task disappeared; inspect plan state"
  [ "$TASK_RECEIPT_ID" = "$task" ] || park "invalid-review-receipt" "the reviewed task id changed"
  [ "$TASK_RECEIPT_STATE" = review ] ||
    park "invalid-review-receipt" "task $task is $TASK_RECEIPT_STATE, not review"
  [ "$TASK_RECEIPT_PROVIDER" = "$PROVIDER" ] ||
    park "invalid-review-receipt" "task $task was reviewed by $TASK_RECEIPT_PROVIDER, not $PROVIDER"
  [ -n "$TASK_RECEIPT_LEASE" ] && [ "$TASK_RECEIPT_LEASE" = "$TASK_RECEIPT_REVIEW_LEASE" ] ||
    park "invalid-review-receipt" "task $task has no exact review lease"
  [ -n "$TASK_RECEIPT_VERIFY" ] || park "missing-verify-contract" "the reviewed task has no verifier"
  [ -f "$TASK_RECEIPT_ARTIFACT" ] ||
    park "invalid-review-receipt" "the reviewed task artifact is missing"
  [ -s "$TASK_RECEIPT_PATCH" ] ||
    park "invalid-review-receipt" "the reviewed task patch is missing"
  [ -z "$TASK_RECEIPT_EXECUTOR_ID$TASK_RECEIPT_EXECUTOR_SOUL" ] ||
    park "review-requires-executor" "goal-drive cannot consume an executor-bound review"
  if [ -n "$reuse_id" ]; then
    [ "$reuse_id" = "$INTENT_ID" ] && [ "$task" = "$prior_task" ] &&
      [ "$base" = "$prior_base" ] && [ "$head_ref" = "$prior_ref" ] &&
      [ "$TASK_RECEIPT_LEASE" = "$prior_lease" ] &&
      [ "$PROVIDER" = "$prior_provider" ] &&
      [ "$(printf '%s' "$TASK_RECEIPT_VERIFY" | oms_sha256_stream)" = "$prior_verify_sha" ] ||
      park "repair-receipt-mismatch" "the repaired review changed its frozen task, base, lease, provider, or verifier"
  fi
  receipt_sha="$TASK_RECEIPT_SHA"
  task_patch_sha="$(oms_sha256_file "$TASK_RECEIPT_PATCH")"
  source_patch_sha="$(oms_sha256_file "$source_patch")"
  [ "$task_patch_sha" = "$source_patch_sha" ] ||
    park "invalid-review-receipt" "the supplied patch differs from the task review receipt"
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$base" ] ||
    park "head-moved" "HEAD moved before the reviewed patch could be frozen"
  [ "$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')" = "$head_ref" ] ||
    park "intent-ref-moved" "the checked-out branch changed before the reviewed patch could be frozen"
  git -C "$REPO" apply --check --binary "$source_patch" >/dev/null 2>&1 ||
    park "review-patch-base-mismatch" "the reviewed patch does not apply to its frozen base"
  INTENT_TASK="$task"
  INTENT_BASE="$base"
  INTENT_REF="$head_ref"
  INTENT_TITLE="$TASK_RECEIPT_TITLE"
  INTENT_LEASE="$TASK_RECEIPT_LEASE"
  INTENT_VERIFY_SHA="$(printf '%s' "$TASK_RECEIPT_VERIFY" | oms_sha256_stream)"
  INTENT_PROVIDER="$PROVIDER"
  INTENT_ID="${reuse_id:-$RUN_ID-$CYCLE-$task}"
  frozen_dir="$REPO/.oms/plan/commit-patches"
  repo_physical="$(cd "$REPO" && pwd -P)"
  plan_physical="$(cd "$REPO/.oms/plan" 2>/dev/null && pwd -P)" ||
    park "unsafe-patch-path" "repo-local .oms/plan must be a real directory"
  [ "$plan_physical" = "$repo_physical/.oms/plan" ] ||
    park "unsafe-patch-path" "the plan directory must not cross a symlink boundary"
  if [ -e "$frozen_dir" ] || [ -L "$frozen_dir" ]; then
    if [ ! -d "$frozen_dir" ] || [ -L "$frozen_dir" ]; then
      park "unsafe-patch-path" "the frozen patch directory must be a real directory"
    fi
  else
    # Anchor creation to the already-open plan directory. If another same-UID
    # process swaps the absolute path before `cd`, the physical identity check
    # fails; if it swaps after `cd`, relative mkdir still targets the original
    # directory handle. This is portable to the documented Git Bash path where
    # Python dir_fd/openat support is not uniform.
    (
      cd "$REPO/.oms/plan" 2>/dev/null || exit 1
      [ "$(pwd -P)" = "$plan_physical" ] || exit 1
      umask 077
      mkdir commit-patches
    ) ||
      park "freeze-patch-failed" "create a writable repo-local patch directory"
  fi
  frozen_physical="$(cd "$frozen_dir" && pwd -P)"
  [ "$frozen_physical" = "$repo_physical/.oms/plan/commit-patches" ] ||
    park "unsafe-patch-path" "the frozen patch directory must not cross a symlink boundary"
  frozen_name="$INTENT_ID${suffix:+-$suffix}.patch"
  case "$frozen_name" in ""|*/*|.|..) park "unsafe-patch-path" "invalid frozen patch name" ;; esac
  # Keep every write relative to a cwd whose physical identity was checked
  # after entry. os.replace uses rename semantics for the final component, so
  # a planted directory symlink is replaced as a leaf (or safely rejected by
  # the platform) instead of being followed like `mv temp symlink-to-directory`.
  if ! (
    cd "$frozen_dir" 2>/dev/null || exit 1
    [ "$(pwd -P)" = "$frozen_physical" ] || exit 1
    umask 077
    frozen_tmp="$(mktemp .goal-patch.XXXXXX)" || exit 1
    if cp "$source_patch" "$frozen_tmp" &&
       chmod 600 "$frozen_tmp" &&
       patch_leaf_replace "$frozen_tmp" "./$frozen_name"; then
      exit 0
    fi
    rm -f "$frozen_tmp" 2>/dev/null || true
    exit 1
  ); then
    park "freeze-patch-failed" "preserve the reviewed patch and retry on a writable local state directory"
  fi
  frozen_after="$(cd "$frozen_dir" 2>/dev/null && pwd -P)" ||
    park "unsafe-patch-path" "the frozen patch directory changed after the write"
  [ "$frozen_after" = "$frozen_physical" ] ||
    park "unsafe-patch-path" "the frozen patch directory changed after the write"
  frozen_abs="$frozen_dir/$frozen_name"
  [ -f "$frozen_abs" ] && [ ! -L "$frozen_abs" ] ||
    park "unsafe-patch-path" "the frozen patch must be a regular non-symlink file"
  patch_rel="${frozen_abs#"$REPO"/}"
  [ "$patch_rel" != "$frozen_abs" ] || park "unsafe-patch-path" "the frozen patch must stay inside repo-local .oms"
  INTENT_PATCH="$patch_rel"
  INTENT_PATCH_SHA="$(oms_sha256_file "$frozen_abs")"
  [ "$(oms_sha256_file "$TASK_RECEIPT_PATCH")" = "$INTENT_PATCH_SHA" ] ||
    park "review-patch-changed" "the reviewed patch changed while it was being frozen"
  git -C "$REPO" apply --check --binary "$frozen_abs" >/dev/null 2>&1 ||
    park "review-patch-base-mismatch" "the frozen patch does not apply to its recorded base"
  paths_json="$(intent_patch_paths_json "$frozen_abs")" ||
    park "no-admitted-patch" "the reviewed patch has no parseable paths"
  INTENT_PATHS="$paths_json"
  load_task_receipt "$task" ||
    park "invalid-review-receipt" "the task review changed while its patch was being frozen"
  [ "$TASK_RECEIPT_SHA" = "$receipt_sha" ] ||
    park "invalid-review-receipt" "the task review changed while its patch was being frozen"
  intent_write prepared "reviewed-patch-frozen"
}

intent_worktree_matches_head() {
  GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
    git -c core.fsmonitor=false -c diff.external= -C "$REPO" \
      diff --no-ext-diff --no-textconv --quiet HEAD -- || return 1
  [ -z "$(GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
    git -c core.fsmonitor=false -C "$REPO" \
      ls-files --others --exclude-standard)" ]
}

intent_expected_tree() {  # PATCH
  local patch_abs="$1"
  local compare_dir expected_index expected_tree rc=1

  compare_dir="$(mktemp -d "${TMPDIR:-/tmp}/oms-goal-expected.XXXXXX")" || return 1
  expected_index="$compare_dir/index"
  if GIT_INDEX_FILE="$expected_index" git -C "$REPO" read-tree "$INTENT_BASE" &&
    GIT_INDEX_FILE="$expected_index" git -C "$REPO" apply --cached --binary "$patch_abs" &&
    expected_tree="$(GIT_INDEX_FILE="$expected_index" git -C "$REPO" write-tree 2>/dev/null | tr -d '\r')" &&
    [ -n "$expected_tree" ]; then
    printf '%s\n' "$expected_tree"
    rc=0
  fi
  rm -rf "$compare_dir"
  return "$rc"
}

intent_tree_matches() {  # PATCH
  local patch_abs="$1"
  local compare_dir actual_index expected_tree actual_tree rc=1

  compare_dir="$(mktemp -d "${TMPDIR:-/tmp}/oms-goal-tree.XXXXXX")" || return 1
  actual_index="$compare_dir/actual.index"

  # The expected side is the base plus only the frozen patch. Stage the entire
  # current worktree in a second private index: a single full-tree comparison
  # then catches extra paths, rename endpoints, and same-path byte changes
  # without depending on Git's rename-reporting presentation.
  if expected_tree="$(intent_expected_tree "$patch_abs")" &&
    GIT_INDEX_FILE="$actual_index" git -C "$REPO" read-tree "$INTENT_BASE" &&
    OMS_GD_REPO="$REPO" GIT_INDEX_FILE="$actual_index" python3 <<'PY'
import os, subprocess
repo = os.environ["OMS_GD_REPO"]
subprocess.check_call([
    "git", "-c", "core.hooksPath=/dev/null", "-C", repo, "add", "-A", "--", ".",
])
PY
  then
    actual_tree="$(GIT_INDEX_FILE="$actual_index" git -C "$REPO" write-tree 2>/dev/null | tr -d '\r' || true)"
    if [ -n "$expected_tree" ] && [ "$expected_tree" = "$actual_tree" ]; then
      rc=0
    fi
  fi
  rm -rf "$compare_dir"
  return "$rc"
}

intent_index_lock_release() {
  if [ "$INTENT_INDEX_OWNED" -eq 1 ] && [ -n "$INTENT_INDEX_LOCK" ]; then
    rm -f "$INTENT_INDEX_LOCK" 2>/dev/null || true
  fi
  INTENT_INDEX_OWNED=0
  INTENT_INDEX_LOCK=""
  INTENT_INDEX_SHA=""
}

intent_index_lock_acquire() {
  local raw index_dir index_name physical lock_sha

  INTENT_INDEX_ERROR=""
  INTENT_INDEX_PATH=""
  INTENT_INDEX_LOCK=""
  INTENT_INDEX_SHA=""
  INTENT_INDEX_OWNED=0
  raw="$(git -C "$REPO" rev-parse --git-path index 2>/dev/null | tr -d '\r')" || {
    INTENT_INDEX_ERROR="index-path-invalid"
    return 1
  }
  [ -n "$raw" ] || {
    INTENT_INDEX_ERROR="index-path-invalid"
    return 1
  }
  case "$raw" in
    /*|[A-Za-z]:/*) index_dir="$(dirname "$raw")" ;;
    *) index_dir="$REPO/$(dirname "$raw")" ;;
  esac
  index_name="$(basename "$raw")"
  physical="$(cd "$index_dir" 2>/dev/null && pwd -P)" || {
    INTENT_INDEX_ERROR="index-path-invalid"
    return 1
  }
  INTENT_INDEX_PATH="$physical/$index_name"
  INTENT_INDEX_LOCK="$INTENT_INDEX_PATH.lock"
  [ -f "$INTENT_INDEX_PATH" ] || {
    INTENT_INDEX_ERROR="index-missing"
    return 1
  }
  if ! python3 - "$INTENT_INDEX_PATH" "$INTENT_INDEX_LOCK" <<'PY'
import os, sys

index, lock = sys.argv[1:]
try:
    descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
except FileExistsError:
    raise SystemExit(2)
except OSError:
    raise SystemExit(3)
try:
    with open(index, "rb") as source, os.fdopen(descriptor, "wb") as target:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            target.write(chunk)
except Exception:
    try:
        os.close(descriptor)
    except OSError:
        pass
    try:
        os.unlink(lock)
    except OSError:
        pass
    raise SystemExit(3)
PY
  then
    INTENT_INDEX_ERROR="index-locked"
    return 1
  fi
  INTENT_INDEX_OWNED=1
  INTENT_INDEX_SHA="$(oms_sha256_file "$INTENT_INDEX_PATH" 2>/dev/null || true)"
  lock_sha="$(oms_sha256_file "$INTENT_INDEX_LOCK" 2>/dev/null || true)"
  if [ -z "$INTENT_INDEX_SHA" ] || [ "$lock_sha" != "$INTENT_INDEX_SHA" ]; then
    INTENT_INDEX_ERROR="index-changed-during-lock"
    intent_index_lock_release
    return 1
  fi
}

intent_index_original_unchanged() {
  local current
  current="$(oms_sha256_file "$INTENT_INDEX_PATH" 2>/dev/null || true)"
  [ -n "$current" ] && [ "$current" = "$INTENT_INDEX_SHA" ]
}

intent_locked_index_matches() {  # TREEISH
  GIT_INDEX_FILE="$INTENT_INDEX_LOCK" \
    git -C "$REPO" diff-index --cached --quiet "$1" -- >/dev/null 2>&1
}

intent_locked_index_prepare() {  # EXPECTED_TREE
  OMS_GD_REPO="$REPO" OMS_GD_PATHS="$INTENT_PATHS" OMS_GD_TREE="$1" \
    GIT_INDEX_FILE="$INTENT_INDEX_LOCK" python3 <<'PY'
import json, os, subprocess

repo = os.environ["OMS_GD_REPO"]
paths = json.loads(os.environ["OMS_GD_PATHS"])
subprocess.check_call(
    ["git", "-c", "core.hooksPath=/dev/null", "-C", repo,
     "reset", "-q", os.environ["OMS_GD_TREE"], "--"] + paths)
PY
  intent_locked_index_matches "$1"
}

intent_index_lock_commit() {
  if python3 - "$INTENT_INDEX_PATH" "$INTENT_INDEX_LOCK" "$INTENT_INDEX_SHA" <<'PY'
import hashlib, os, sys

index, lock, expected = sys.argv[1:]
value = hashlib.sha256()
try:
    with open(index, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(chunk)
    if value.hexdigest() != expected:
        raise SystemExit(2)
    os.replace(lock, index)
except OSError:
    raise SystemExit(3)
PY
  then
    INTENT_INDEX_OWNED=0
    INTENT_INDEX_LOCK=""
    INTENT_INDEX_SHA=""
    return 0
  fi
  return 1
}

# Recovery after ref publication may align only the untouched recorded-base
# index (or accept an index already at the exact expected tree). Anything else
# is another session's staged authority and must remain byte-for-byte intact.
intent_align_index() {  # EXPECTED_TREE
  local expected_tree="$1"

  intent_index_lock_acquire || return 1
  if intent_locked_index_matches "$expected_tree"; then
    if intent_index_original_unchanged; then
      intent_index_lock_release
      return 0
    fi
    INTENT_INDEX_ERROR="index-changed-during-recovery"
    intent_index_lock_release
    return 1
  fi
  if ! intent_locked_index_matches "$INTENT_BASE"; then
    INTENT_INDEX_ERROR="index-does-not-match-base"
    intent_index_lock_release
    return 1
  fi
  if ! intent_index_original_unchanged ||
    ! intent_locked_index_prepare "$expected_tree" ||
    ! intent_index_original_unchanged ||
    ! intent_index_lock_commit; then
    INTENT_INDEX_ERROR="index-changed-during-recovery"
    intent_index_lock_release
    return 1
  fi
}

# Hold Git's standard index lock across the parent/tree checks, ref CAS, and
# index CAS. The commit object is immutable; the real index is replaced only
# when its byte digest still matches the snapshot that proved INTENT_BASE.
intent_publish_exact() {  # PATCH EXPECTED_TREE MESSAGE
  local patch_abs="$1"
  local expected_tree="$2"
  local message="$3"
  local published_parent published_tree current_ref published_ref

  INTENT_NEW_COMMIT=""
  intent_index_lock_acquire || return 1
  if ! intent_locked_index_matches "$INTENT_BASE"; then
    INTENT_INDEX_ERROR="index-does-not-match-base"
    intent_index_lock_release
    return 1
  fi
  if ! intent_index_original_unchanged; then
    INTENT_INDEX_ERROR="index-changed-before-publication"
    intent_index_lock_release
    return 1
  fi
  if [ "$(git -C "$REPO" rev-parse HEAD 2>/dev/null | tr -d '\r')" != "$INTENT_BASE" ]; then
    INTENT_INDEX_ERROR="head-moved-before-publication"
    intent_index_lock_release
    return 1
  fi
  current_ref="$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')"
  if [ "$current_ref" != "$INTENT_REF" ]; then
    INTENT_INDEX_ERROR="head-ref-moved-before-publication"
    intent_index_lock_release
    return 1
  fi
  if ! intent_tree_matches "$patch_abs"; then
    INTENT_INDEX_ERROR="worktree-changed-before-publication"
    intent_index_lock_release
    return 1
  fi
  INTENT_NEW_COMMIT="$(git -c core.hooksPath=/dev/null -C "$REPO" \
    commit-tree "$expected_tree" -p "$INTENT_BASE" \
    -m "$message" 2>/dev/null | tr -d '\r')" || {
    INTENT_INDEX_ERROR="commit-object-failed"
    intent_index_lock_release
    return 1
  }
  if [ -z "$INTENT_NEW_COMMIT" ] || ! intent_index_original_unchanged; then
    INTENT_INDEX_ERROR="index-changed-before-publication"
    intent_index_lock_release
    return 1
  fi
  if ! git -c core.hooksPath=/dev/null -C "$REPO" \
    update-ref -m "goal-drive: $INTENT_TASK" \
    "$INTENT_REF" "$INTENT_NEW_COMMIT" "$INTENT_BASE"; then
    INTENT_INDEX_ERROR="head-moved-before-publication"
    intent_index_lock_release
    return 1
  fi
  current_ref="$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')"
  published_ref="$(git -C "$REPO" rev-parse "$INTENT_REF" 2>/dev/null | tr -d '\r' || true)"
  published_parent="$(git -C "$REPO" rev-parse "$INTENT_NEW_COMMIT^" 2>/dev/null | tr -d '\r' || true)"
  published_tree="$(git -C "$REPO" rev-parse "$INTENT_NEW_COMMIT^{tree}" 2>/dev/null | tr -d '\r' || true)"
  if [ "$current_ref" != "$INTENT_REF" ] || [ "$published_ref" != "$INTENT_NEW_COMMIT" ] ||
    [ "$published_parent" != "$INTENT_BASE" ] || [ "$published_tree" != "$expected_tree" ]; then
    INTENT_INDEX_ERROR="published-commit-mismatch"
    intent_index_lock_release
    return 1
  fi
  if [ "${OMS_GOAL_DRIVE_TEST_STOP_AFTER_REF:-0}" = 1 ]; then
    # Test-only graceful interruption models process loss after the durable ref
    # CAS. A real hard stop can leave index.lock, which is Git's standard
    # fail-closed signal and must be inspected rather than stolen on restart.
    intent_index_lock_release
    INTENT_INDEX_ERROR="test-stop-after-ref"
    return 75
  fi
  if ! intent_index_original_unchanged ||
    ! intent_locked_index_prepare "$expected_tree" ||
    ! intent_index_original_unchanged ||
    ! intent_index_lock_commit; then
    INTENT_INDEX_ERROR="index-changed-after-publication"
    intent_index_lock_release
    return 1
  fi
}

intent_commit() {  # LABEL
  local patch_abs msg after_dirty task_patch_sha verify_sha expected_tree publish_rc=0
  guard_git_authority
  guard_expected_ref
  patch_abs="$(intent_patch_file)" || park "unsafe-patch-path" "inspect the commit intent"
  [ -f "$patch_abs" ] || park "missing-intent-patch" "restore the frozen patch or reconcile manually"
  [ "$(oms_sha256_file "$patch_abs")" = "$INTENT_PATCH_SHA" ] ||
    park "intent-patch-changed" "the frozen patch bytes changed; do not commit them"
  load_task_receipt "$INTENT_TASK" ||
    park "invalid-commit-receipt" "the landed task receipt could not be read"
  task_patch_sha="$(oms_sha256_file "$TASK_RECEIPT_PATCH" 2>/dev/null || true)"
  verify_sha="$(printf '%s' "$TASK_RECEIPT_VERIFY" | oms_sha256_stream)"
  [ "$TASK_RECEIPT_ID" = "$INTENT_TASK" ] && [ "$TASK_RECEIPT_STATE" = "done" ] &&
    [ "$TASK_RECEIPT_PROVIDER" = "$INTENT_PROVIDER" ] &&
    [ "$TASK_RECEIPT_LEASE" = "$INTENT_LEASE" ] &&
    [ "$TASK_RECEIPT_REVIEW_LEASE" = "$INTENT_LEASE" ] &&
    [ "$verify_sha" = "$INTENT_VERIFY_SHA" ] &&
    [ "$task_patch_sha" = "$INTENT_PATCH_SHA" ] &&
    [ -f "$TASK_RECEIPT_ARTIFACT" ] &&
    [ -z "$TASK_RECEIPT_EXECUTOR_ID$TASK_RECEIPT_EXECUTOR_SOUL" ] ||
    park "invalid-commit-receipt" "the landed task no longer exactly matches the frozen commit intent"
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$INTENT_BASE" ] ||
    park "intent-head-moved" "HEAD moved after the intent; inspect the current commit before recovery"
  [ "$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')" = "$INTENT_REF" ] ||
    park "intent-ref-moved" "the checked-out branch differs from the frozen commit intent"
  git -C "$REPO" apply --reverse --check --binary "$patch_abs" >/dev/null 2>&1 ||
    park "intent-tree-ambiguous" "the working tree is not the exact applied patch; inspect it manually"
  intent_tree_matches "$patch_abs" ||
    park "commit-mismatch" "working-tree paths or bytes differ from the frozen patch; resolve by hand"
  expected_tree="$(intent_expected_tree "$patch_abs")" ||
    park "intent-tree-invalid" "the frozen patch could not produce its exact expected tree"
  guard_expected_ref
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$INTENT_BASE" ] ||
    park "intent-head-moved" "HEAD moved before the exact commit could be published"
  [ "$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')" = "$INTENT_REF" ] ||
    park "intent-ref-moved" "the checked-out branch changed before exact publication"
  intent_tree_matches "$patch_abs" ||
    park "commit-mismatch" "working-tree bytes changed before the exact commit could be published"
  msg="$(printf '%s' "${INTENT_TITLE:-$INTENT_TASK}" | tr -d '\000-\037' | sed 's/^[-[:space:]]*//' | cut -c1-72)"
  # `git commit` runs repository hooks against the mutable real index and reads
  # HEAD again after our checks. A hook or sibling writer could therefore add
  # bytes or change the parent. Build the immutable reviewed tree directly and
  # publish it with an expected-old ref update. Autonomous commits deliberately
  # do not run commit hooks: admission plus the task verifier is their gate.
  intent_publish_exact "$patch_abs" "$expected_tree" \
    "${msg:-plan task $INTENT_TASK}" || publish_rc=$?
  case "$publish_rc:$INTENT_INDEX_ERROR" in
    0:*) ;;
    75:test-stop-after-ref)
      echo "goal-drive: injected stop after exact ref publication; rerun to reconcile $INTENT_ID" >&2
      exit 75
      ;;
    *:index-locked)
      park "intent-index-locked" "another Git index transaction is active; preserve it and retry after it finishes"
      ;;
    *:index-does-not-match-base|*:index-changed-before-publication)
      park "intent-index-moved" "the real index differs from the frozen base; staged bytes were preserved"
      ;;
    *:head-moved-before-publication)
      park "intent-head-moved" "HEAD moved while publishing the exact commit; inspect the current lineage"
      ;;
    *:head-ref-moved-before-publication)
      park "intent-ref-moved" "the checked-out branch changed while publishing; the frozen ref was not widened"
      ;;
    *:worktree-changed-before-publication)
      park "commit-mismatch" "working-tree bytes changed before the exact commit could be published"
      ;;
    *:published-commit-mismatch)
      park "intent-publish-mismatch" "the published commit does not match the frozen parent and tree"
      ;;
    *:index-changed-after-publication)
      park "intent-index-moved" "the exact commit is published, but concurrent staged bytes were preserved"
      ;;
    *)
      park "intent-commit-failed" "the exact commit/index transaction failed closed ($INTENT_INDEX_ERROR)"
      ;;
  esac
  after_dirty="$(git_status_porcelain)"
  [ -z "$after_dirty" ] ||
    park "commit-incomplete" "tracked or untracked changes remain after the task commit"
  intent_write committed "exact-patch-committed"
  echo "goal-drive: ${1:-committed} commit intent task=$INTENT_TASK $(git -C "$REPO" rev-parse --short HEAD)"
}

intent_commit_already_present() {
  local patch_abs expected_tree candidate candidate_tree parents current_head align_rc=0
  local search_ref="$INTENT_REF"
  # Mirror the projection's closure proof. While the frozen ref resolves it is
  # the authority and no sibling branch inherits it; once the branch is gone
  # the surviving lineage carries the proof, still gated on the exact base
  # parent and frozen tree. Without this, a crash between the commit and its
  # terminal row leaves an open intent that ordinary branch cleanup makes
  # unrecoverable.
  if git -C "$REPO" rev-parse --verify --quiet "$INTENT_REF^{commit}" >/dev/null 2>&1; then
    [ "$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')" = "$INTENT_REF" ] || return 3
  else
    search_ref=HEAD
  fi
  patch_abs="$(intent_patch_file)" || return 1
  expected_tree="$(intent_expected_tree "$patch_abs" 2>/dev/null || true)"
  [ -n "$expected_tree" ] || return 1
  candidate="$(git -C "$REPO" rev-list --first-parent "$search_ref" 2>/dev/null |
    while IFS= read -r commit; do
      parents="$(git -C "$REPO" rev-list --parents -n 1 "$commit" 2>/dev/null | tr -d '\r')"
      [ "$parents" = "$commit $INTENT_BASE" ] || continue
      candidate_tree="$(git -C "$REPO" rev-parse "$commit^{tree}" 2>/dev/null | tr -d '\r')"
      [ "$candidate_tree" = "$expected_tree" ] || continue
      printf '%s\n' "$commit"
      break
    done)"
  current_head="$(git -C "$REPO" rev-parse "$search_ref" 2>/dev/null | tr -d '\r')"
  if [ -n "$candidate" ] && intent_worktree_matches_head; then
    if [ "$current_head" != "$candidate" ]; then
      [ -z "$(git_status_porcelain)" ] && return 0
      return 1
    fi
    intent_align_index "$expected_tree" || align_rc=$?
    [ "$align_rc" -eq 0 ] || return 2
    [ -z "$(git_status_porcelain)" ] && return 0
  fi
  return 1
}

pending_review() {
  OMS_GD_PROVIDER="$PROVIDER" python3 - "$PLAN_FILE" <<'PY' | tr -d '\r'
import json, os, re, sys
try:
    data=json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    raise SystemExit(6)
tasks=data.get("tasks", {})
if not isinstance(tasks, dict):
    raise SystemExit(6)
provider=os.environ["OMS_GD_PROVIDER"]
valid=[]
executor_bound=[]
invalid=[]
ident=re.compile(r"^[A-Za-z0-9._-]+$")
for key, task in tasks.items():
    if not isinstance(task, dict) or task.get("state") != "review" or task.get("provider") != provider:
        continue
    executor_id=task.get("executor_id", "")
    executor_soul=task.get("executor_soul_sha256", "")
    if executor_id or executor_soul:
        if isinstance(executor_id, str) and isinstance(executor_soul, str) and executor_id and executor_soul:
            executor_bound.append(key)
        else:
            invalid.append(key)
        continue
    lease=task.get("lease_id", "")
    allowed=task.get("allowed_paths", [])
    values=(key, task.get("id", ""), lease, task.get("review_lease_id", ""),
            task.get("verify", ""), task.get("patch", ""), task.get("artifact", ""))
    if (not all(isinstance(value, str) for value in values) or not ident.fullmatch(key) or
        values[1] != key or not lease or values[3] != lease or not all(values[4:]) or
        not isinstance(allowed, list) or not allowed or
        not all(isinstance(path, str) and path for path in allowed)):
        invalid.append(key)
        continue
    valid.append(key)
if invalid:
    print("invalid:" + ",".join(sorted(str(value) for value in invalid)))
    raise SystemExit(6)
if len(valid) > 1:
    print("multiple:" + ",".join(sorted(valid)))
    raise SystemExit(4)
if len(valid) == 1:
    print(valid[0])
    raise SystemExit(0)
if executor_bound:
    print("executor:" + ",".join(sorted(str(value) for value in executor_bound)))
    raise SystemExit(5)
PY
}

abandon_invalid_intent() {  # REASON
  intent_write abandoned "$1"
  echo "goal-drive: ignored invalid commit intent $INTENT_ID ($1)" >&2
}

recover_landing_receipts() {
  local recover_log recover_rc=0
  recover_log="$(agent_memory_mktemp)" || return 1
  "$ROOT/scripts/patch-land.sh" --repo "$REPO" --recover >"$recover_log" 2>&1 || recover_rc=$?
  if [ "$recover_rc" -ne 0 ]; then
    tail -n 8 "$recover_log" >&2
    rm -f "$recover_log"
    return "$recover_rc"
  fi
  tail -n 3 "$recover_log" >&2
  rm -f "$recover_log"
  return 0
}

reconcile_commit_intent() {
  local open patch_abs actual_paths task_patch_sha verify_sha land_rc=0 old_intent
  local present_rc=0
  open="$(latest_open_intent)"
  [ -n "$open" ] || return 0
  load_intent "$open"
  [ "$INTENT_PROVIDER" = "$PROVIDER" ] ||
    park "intent-provider-mismatch" "resume with --to $INTENT_PROVIDER or inspect the recorded intent"
  patch_abs="$(intent_patch_file)" || {
    abandon_invalid_intent "unsafe-frozen-patch-path"
    return 0
  }
  [ -s "$patch_abs" ] || {
    abandon_invalid_intent "frozen-patch-missing"
    return 0
  }
  [ "$(oms_sha256_file "$patch_abs")" = "$INTENT_PATCH_SHA" ] || {
    abandon_invalid_intent "frozen-patch-hash-mismatch"
    return 0
  }
  actual_paths="$(intent_patch_paths_json "$patch_abs")" || {
    abandon_invalid_intent "unparseable-frozen-patch"
    return 0
  }
  if [ "$actual_paths" != "$INTENT_PATHS" ]; then
    abandon_invalid_intent "frozen-path-receipt-mismatch"
    return 0
  fi

  load_task_receipt "$INTENT_TASK" || {
    abandon_invalid_intent "task-receipt-missing"
    return 0
  }
  [ "$TASK_RECEIPT_ID" = "$INTENT_TASK" ] || {
    abandon_invalid_intent "task-id-receipt-mismatch"
    return 0
  }
  # patch-land owns a nested apply/receipt transaction. If it stopped after
  # fencing the task (or after applying), converge that journal before judging
  # the outer commit intent. An applied tree must never lose its outer recovery
  # handle merely because one lineage/plan receipt was incomplete.
  if [ "$TASK_RECEIPT_STATE" = landing ]; then
    recover_landing_receipts ||
      park "landing-recovery-failed" "run oms patch-land --recover; the exact outer commit intent remains open"
    load_task_receipt "$INTENT_TASK" ||
      park "landing-recovery-failed" "the recovered landing task receipt could not be read"
    case "$TASK_RECEIPT_STATE" in
      ready)
        intent_write abandoned "landing-recovered-unapplied"
        return 0
        ;;
      done) ;;
      *) park "landing-recovery-incomplete" "inspect the outstanding patch-land receipt; the outer intent remains open" ;;
    esac
  elif [ "$TASK_RECEIPT_STATE" = "done" ] &&
    git -C "$REPO" apply --reverse --check --binary "$patch_abs" >/dev/null 2>&1 &&
    intent_tree_matches "$patch_abs"; then
    recover_landing_receipts ||
      park "landing-recovery-failed" "run oms patch-land --recover; the exact outer commit intent remains open"
    load_task_receipt "$INTENT_TASK" ||
      park "landing-recovery-failed" "the recovered landing task receipt could not be read"
  fi
  if [ "$INTENT_PHASE" = repairing ]; then
    case "$TASK_RECEIPT_STATE:$TASK_RECEIPT_REPAIR_COUNT" in
      ready:1)
        verify_sha="$(printf '%s' "$TASK_RECEIPT_VERIFY" | oms_sha256_stream)"
        task_patch_sha="$(oms_sha256_file "$TASK_RECEIPT_PATCH" 2>/dev/null || true)"
        if [ "$TASK_RECEIPT_REVIEW_LEASE" != "$INTENT_LEASE" ] ||
          [ "$verify_sha" != "$INTENT_VERIFY_SHA" ] ||
          [ ! -f "$TASK_RECEIPT_ARTIFACT" ] ||
          [ "$task_patch_sha" != "$INTENT_PATCH_SHA" ] ||
          [ -n "$TASK_RECEIPT_EXECUTOR_ID$TASK_RECEIPT_EXECUTOR_SOUL" ]; then
          abandon_invalid_intent "released-repair-receipt-mismatch"
          return 0
        fi
        "$ROOT/scripts/agent-plan.sh" --repo "$REPO" block --id "$INTENT_TASK" \
          --reason "interrupted bounded landing repair" >/dev/null 2>&1 ||
          park "repair-terminalize-failed" "the interrupted repair could not be blocked"
        intent_write abandoned "repair-released-before-review"
        return 0
        ;;
      blocked:1)
        intent_write abandoned "repair-blocked-before-review"
        return 0
        ;;
    esac
  fi
  [ "$TASK_RECEIPT_PROVIDER" = "$PROVIDER" ] ||
    {
      abandon_invalid_intent "task-provider-receipt-mismatch"
      return 0
    }
  [ -n "$TASK_RECEIPT_LEASE" ] && [ "$TASK_RECEIPT_LEASE" = "$INTENT_LEASE" ] || {
    abandon_invalid_intent "task-lease-receipt-mismatch"
    return 0
  }
  [ "$TASK_RECEIPT_REVIEW_LEASE" = "$INTENT_LEASE" ] || {
    abandon_invalid_intent "task-review-lease-receipt-mismatch"
    return 0
  }
  [ -n "$TASK_RECEIPT_VERIFY" ] ||
    {
      abandon_invalid_intent "task-verify-receipt-missing"
      return 0
    }
  verify_sha="$(printf '%s' "$TASK_RECEIPT_VERIFY" | oms_sha256_stream)"
  [ "$verify_sha" = "$INTENT_VERIFY_SHA" ] || {
    abandon_invalid_intent "task-verify-receipt-mismatch"
    return 0
  }
  [ -f "$TASK_RECEIPT_ARTIFACT" ] || {
    abandon_invalid_intent "task-artifact-receipt-missing"
    return 0
  }
  [ -z "$TASK_RECEIPT_EXECUTOR_ID$TASK_RECEIPT_EXECUTOR_SOUL" ] || {
    abandon_invalid_intent "task-executor-receipt-mismatch"
    return 0
  }

  # A repair journal row is written before the state transition. It prevents a
  # restart from blindly making a second provider call. A completed same-lease
  # repaired review supersedes the old frozen patch under the same intent id;
  # an in-flight repair stays parked, and a released/blocked repair closes.
  if [ "$INTENT_PHASE" = repairing ]; then
    case "$TASK_RECEIPT_STATE:$TASK_RECEIPT_REPAIR_COUNT" in
      review:1)
        [ -s "$TASK_RECEIPT_PATCH" ] || {
          abandon_invalid_intent "repaired-review-patch-missing"
          return 0
        }
        old_intent="$INTENT_ID"
        intent_prepare "$INTENT_TASK" "$TASK_RECEIPT_PATCH" "$INTENT_BASE" "$INTENT_REF" repair "$old_intent"
        patch_abs="$(intent_patch_file)" || park "unsafe-patch-path" "inspect the repaired intent"
        ;;
      review:0)
        intent_write prepared "repair-transition-not-started"
        ;;
      claimed:1|running:1)
        park "repair-in-progress" "wait for the existing same-lease repair worker or inspect task $INTENT_TASK"
        ;;
      *)
        park "repair-state-ambiguous" "task $INTENT_TASK is $TASK_RECEIPT_STATE with repair_count=$TASK_RECEIPT_REPAIR_COUNT"
        ;;
    esac
  fi

  [ -s "$TASK_RECEIPT_PATCH" ] || {
    abandon_invalid_intent "task-patch-receipt-missing"
    return 0
  }
  task_patch_sha="$(oms_sha256_file "$TASK_RECEIPT_PATCH")"
  if [ "$task_patch_sha" != "$INTENT_PATCH_SHA" ]; then
    abandon_invalid_intent "task-patch-receipt-mismatch"
    return 0
  fi
  case "$INTENT_PHASE:$TASK_RECEIPT_STATE" in
    prepared:review|prepared:done|landed:done) ;;
    *)
      abandon_invalid_intent "task-state-receipt-mismatch"
      return 0
      ;;
  esac

  if [ "$(git -C "$REPO" rev-parse HEAD)" != "$INTENT_BASE" ]; then
    intent_commit_already_present || present_rc=$?
    if [ "$present_rc" -eq 0 ]; then
      intent_write committed "reconciled-existing-commit"
      echo "goal-drive: recovered commit intent task=$INTENT_TASK (commit already present)"
      return 0
    fi
    if [ "$present_rc" -eq 2 ]; then
      case "$INTENT_INDEX_ERROR" in
        index-locked)
          park "intent-index-locked" "the exact commit is present, but another Git index transaction is active"
          ;;
        *)
          park "intent-index-moved" "the exact commit is present, but staged bytes differ from the frozen base and were preserved"
          ;;
      esac
    fi
    if [ "$present_rc" -eq 3 ]; then
      park "intent-ref-moved" "the checked-out branch differs from the frozen commit intent"
    fi
    if [ "$TASK_RECEIPT_STATE" = review ] &&
      [ -z "$(git_status_porcelain)" ] &&
      git -C "$REPO" apply --check --binary "$patch_abs" >/dev/null 2>&1; then
      abandon_invalid_intent "frozen-base-receipt-mismatch"
      return 0
    fi
    park "intent-head-moved" "HEAD moved and does not exactly match the frozen intent"
  fi

  if git -C "$REPO" apply --reverse --check --binary "$patch_abs" >/dev/null 2>&1 &&
    intent_tree_matches "$patch_abs"; then
    intent_write landed "reconciled-applied-tree"
    intent_commit "recovered"
    return 0
  fi

  if [ -z "$(git_status_porcelain)" ] &&
    git -C "$REPO" apply --check --binary "$patch_abs" >/dev/null 2>&1; then
    [ "$TASK_RECEIPT_STATE" = review ] ||
      park "intent-task-moved" "the pending intent task is $TASK_RECEIPT_STATE, not review"
    "$ROOT/scripts/patch-land.sh" --repo "$REPO" --patch "$patch_abs" \
      ${AVC_FLAG:+"$AVC_FLAG"} \
      --plan-task "$INTENT_TASK" --verify "$TASK_RECEIPT_VERIFY" >/dev/null 2>&1 || land_rc=$?
    [ "$land_rc" -eq 0 ] || park "intent-reland-failed" "the frozen reviewed patch no longer admits; inspect its report"
    intent_write landed "reconciled-by-landing"
    intent_commit "recovered"
    return 0
  fi

  park "intent-tree-ambiguous" "the tree proves neither an unapplied nor exact applied frozen patch"
}

# Reconcile before acceptance and before the ordinary clean-tree refusal. A
# crash after landing is expected to be dirty; treating it as foreign work is
# precisely the liveness bug this journal closes.
guard_git_authority
guard_expected_ref
reconcile_commit_intent
guard_expected_ref

# A dirty tree now means another session's live work, not an OMS-owned intent.
# Include untracked files: a new-file-only landing was previously invisible.
dirty="$(git_status_porcelain)"
[ -z "$dirty" ] || fail "tree is dirty; commit or stash first, or drive from a dedicated worktree"

prev_tree=""
prev_accept_out=""

while :; do
  CYCLE=$((CYCLE + 1))
  guard_git_authority
  guard_expected_ref
  # Judge against the start-of-run contract only: a mid-run edit to the
  # acceptance command is a moved goalpost, not a pass. Checked in the main
  # shell so park() actually terminates the run.
  now="$(printf '%s' "$(read_accept_cmd)" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  [ "$now" = "$ACCEPT_SNAPSHOT" ] ||
    park "acceptance-command-changed" "re-run goal-drive after reviewing the new acceptance command"
  accept_rc=0
  accept_out="$(OMS_GOAL_RUN_ID="$RUN_ID" OMS_GOAL_CYCLE="$CYCLE" \
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept 2>&1)" || accept_rc=$?
  guard_expected_ref
  printf '%s\n' "$accept_out" | tail -n 3
  if [ "$accept_rc" -eq 0 ]; then
    # An acceptance command that already passes before this run did anything
    # cannot tell the goal state from the start state. Reporting done here is a
    # run that accomplished nothing while every caller downstream — a semantic
    # review of an empty diff, a parent reading the terminal row — takes it for
    # success. Two things redeem a cycle-1 pass: a recorded failure of this same
    # contract at an ancestor commit (work landed and turned it around), or a
    # plan with nothing left to do. Neither one, and the contract is broken.
    if [ "$CYCLE" -eq 1 ] && [ -z "$(acceptance_ever_failed)" ] &&
      [ "$(plan_has_unfinished_work)" = 1 ]; then
      park "acceptance-vacuous" "acceptance passes on the base tree while every task is still pending; tighten it (agent-plan init --accept) so it fails until the goal is actually met"
    fi
    if ! terminal_row "done" acceptance-pass; then
      echo "error: cannot durably append the goal-drive terminal row" >&2
      exit 2
    fi
    resolve_parks
    work_journal_observe "$REPO" goal-drive "$PROGRESS" \
      --source-id "$RUN_ID:done" --outcome "Goal drive reached acceptance" \
      --outcome-status "done" --verification-status passed \
      >/dev/null 2>&1 || true
    echo "goal-drive: done run=$RUN_ID cycles=$CYCLE (acceptance passed)"
    if ! terminal_result "done" acceptance-pass; then
      echo "error: cannot emit the goal-drive terminal result" >&2
      exit 2
    fi
    exit 0
  fi
  if [ "$accept_rc" -ne 3 ]; then
    accept_reason="$(printf '%s\n' "$accept_out" | python3 -c '
import re, sys
allowed = {
    "acceptance-command-changed", "acceptance-files-changed",
    "acceptance-mutated-repository", "acceptance-supervision-failed",
    "acceptance-timeout", "acceptance-output-limit",
}
found = ""
for line in sys.stdin:
    match = re.search(r"(?:^|\s)reason=([a-z0-9-]+)(?=$|\s)", line)
    if match and match.group(1) in allowed:
        found = match.group(1)
print(found)
' | tr -d '\r')"
    if [ -n "$accept_reason" ]; then
      park "$accept_reason" "inspect the acceptance receipt and restore the frozen plan, files, and repository state before retrying"
    fi
    park "acceptance-command-error" "the acceptance command itself failed to run; fix the spec (agent-plan init --accept)"
  fi

  # Stuck: nothing changed since the last failing cycle — more cycles would
  # replay the same failure, not fix it.
  tree="$(git -C "$REPO" rev-parse 'HEAD^{tree}' 2>/dev/null || echo none)"
  accept_digest="$(printf '%s' "$accept_out" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  if [ "$tree" = "$prev_tree" ] && [ "$accept_digest" = "$prev_accept_out" ]; then
    park "stuck" "same tree and same failing acceptance twice; get an outside read: oms advise"
  fi
  prev_tree="$tree"; prev_accept_out="$accept_digest"

  if [ "$CYCLE" -gt "$MAX_CYCLES" ]; then
    park "cycles-exhausted" "acceptance still failing after $MAX_CYCLES cycle(s); review progress.jsonl, then extend the plan or raise --max-cycles"
  fi

  head_before="$(git -C "$REPO" rev-parse HEAD)"
  head_ref_before="$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')"
  [ -n "$head_ref_before" ] ||
    park "detached-head" "check out the reviewed work branch before goal-drive can commit"
  # First obtain a reviewed patch without mutating the main tree. The durable
  # intent below is the handoff between delegation authority and landing/
  # commit authority; plan-run no longer gets to cross both in one opaque
  # call. A unique exact review may predate this process (provider completed,
  # then goal-drive crashed), so adopt that receipt before claiming new work.
  pending_rc=0
  task_id="$(pending_review 2>&1)" || pending_rc=$?
  provider_called=0
  case "$pending_rc" in
    0)
      if [ -n "$task_id" ]; then
        load_task_receipt "$task_id" ||
          park "invalid-review-receipt" "the pending reviewed task could not be read"
        patch_file="$TASK_RECEIPT_PATCH"
        echo "goal-drive: continuing reviewed task=$task_id without a provider call"
      else
        provider_called=1
      fi
      ;;
    4) park "multiple-reviewed-tasks" "choose one reviewed task explicitly before resuming goal-drive: $task_id" ;;
    5) park "review-requires-executor" "goal-drive cannot consume the pending executor-bound review: $task_id" ;;
    *) park "invalid-review-receipt" "repair the malformed reviewed task before goal-drive continues: $task_id" ;;
  esac

  if [ "$provider_called" = 1 ]; then
    run_args=(--repo "$REPO" --to "$PROVIDER" --next)
    [ "$RETRY_KNOWN" -eq 0 ] || run_args+=(--retry-known)
    [ "$ALLOW_VERIFIER_CHANGE" -eq 0 ] || run_args+=(--allow-verifier-change)
    [ -z "$MODEL" ] || run_args+=(--model "$MODEL")
    [ -z "$FALLBACK_MODEL" ] || run_args+=(--fallback-model "$FALLBACK_MODEL")
    [ "$REASONING_EFFORT" = auto ] || run_args+=(--reasoning-effort "$REASONING_EFFORT")
    run_rc=0
    goal_phase_run env "OMS_PEER_TIMEOUT=$PROVIDER_TIMEOUT" bash \
      "$ROOT/scripts/plan-run.sh" "${run_args[@]}" || run_rc=$?
    run_out="$GOAL_PHASE_OUT"
    guard_git_authority
    guard_expected_ref
    printf '%s\n' "$run_out" | tail -n 6
    if [ "$run_rc" -ne 0 ]; then
      if printf '%s\n' "$run_out" | grep -q 'no actionable task'; then
        park "tasks-exhausted" "acceptance fails and the plan has no actionable task; add tasks (agent-plan add) or decompose the remaining gap"
      fi
      park "task-failed" "plan-run failed (see its ledger row); review the task, then re-run goal-drive"
    fi

    task_id="$(printf '%s\n' "$run_out" | sed -n 's/^plan-run: result task=\([^ ]*\).*/\1/p' | tail -n 1)"
    patch_file="$(printf '%s\n' "$run_out" | sed -n 's/.* patch=\([^ ]*\).*/\1/p' | tail -n 1)"
  fi

  guard_git_authority
  guard_expected_ref
  [ "$(git -C "$REPO" rev-parse HEAD)" = "$head_before" ] ||
    park "head-moved" "another writer advanced HEAD mid-cycle; inspect git log, then re-run on a quiet tree"
  [ "$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null | tr -d '\r')" = "$head_ref_before" ] ||
    park "intent-ref-moved" "the checked-out branch changed mid-cycle; return to the reviewed branch"
  [ -n "$task_id" ] || park "missing-plan-task" "plan-run returned no task id"
  if [ -z "$patch_file" ] || [ ! -f "$patch_file" ]; then
    park "no-admitted-patch" "plan-run returned no reviewed patch"
  fi
  if [ "$provider_called" = 1 ] && [ "${OMS_GOAL_DRIVE_TEST_STOP_AFTER_REVIEW:-0}" = 1 ]; then
    echo "goal-drive: injected stop after provider review; rerun to adopt task $task_id" >&2
    exit 75
  fi

  intent_prepare "$task_id" "$patch_file" "$head_before" "$head_ref_before" ""
  guard_expected_ref
  patch_abs="$(intent_patch_file)" || park "unsafe-patch-path" "inspect the commit intent"
  verify="$TASK_RECEIPT_VERIFY"
  land_log="$(agent_memory_mktemp)" || park "internal" "could not allocate landing output"
  land_rc=0
  repair_worker_failed=0
  "$ROOT/scripts/patch-land.sh" --repo "$REPO" --patch "$patch_abs" \
    ${AVC_FLAG:+"$AVC_FLAG"} \
    --plan-task "$task_id" --verify "$verify" >"$land_log" 2>&1 || land_rc=$?

  if [ "$land_rc" -ne 0 ] &&
    git -C "$REPO" apply --reverse --check --binary "$patch_abs" >/dev/null 2>&1 &&
    intent_tree_matches "$patch_abs"; then
    recover_landing_receipts || {
      printf '%s\n' "$(tail -n 8 "$land_log")" >&2
      rm -f "$land_log"
      park "landing-recovery-failed" "run oms patch-land --recover; the exact outer commit intent remains open"
    }
    load_task_receipt "$task_id" ||
      park "landing-recovery-failed" "the recovered landing task receipt could not be read"
    [ "$TASK_RECEIPT_STATE" = "done" ] ||
      park "landing-recovery-incomplete" "the applied patch is exact but its task receipt is $TASK_RECEIPT_STATE; keep the outer intent and recover it"
    land_rc=0
  elif [ "$land_rc" -ne 0 ] && [ -n "$(git_status_porcelain)" ]; then
    printf '%s\n' "$(tail -n 8 "$land_log")" >&2
    rm -f "$land_log"
    park "landing-tree-ambiguous" "landing failed with a dirty non-exact tree; the outer intent remains open for manual recovery"
  fi

  if [ "$land_rc" -ne 0 ] && [ "$AUTO_REPAIR" -eq 1 ]; then
    echo "goal-drive: landing failed; one fenced repair of the reviewed patch"
    repair_lease="$INTENT_LEASE"
    repair_rc=0
    intent_write repairing "landing-repair-started"
    if "$ROOT/scripts/agent-plan.sh" --repo "$REPO" repair --id "$task_id" \
      --lease-id "$repair_lease" --artifact "$land_log" >/dev/null 2>&1; then
      repair_args=(--repo "$REPO" --to "$PROVIDER" --plan-task "$task_id" \
        --repair 0 --review-artifact "$land_log" --terminalize-resumed-repair)
      [ -z "$MODEL" ] || repair_args+=(--model "$MODEL")
      [ -z "$FALLBACK_MODEL" ] || repair_args+=(--fallback-model "$FALLBACK_MODEL")
      [ "$REASONING_EFFORT" = auto ] || repair_args+=(--reasoning-effort "$REASONING_EFFORT")
      goal_phase_run env "OMS_PEER_TIMEOUT=$PROVIDER_TIMEOUT" bash \
        "$ROOT/scripts/peer-delegate.sh" "${repair_args[@]}" || repair_rc=$?
      repair_out="$GOAL_PHASE_OUT"
      guard_git_authority
      guard_expected_ref
      repair_rc="${repair_rc:-0}"
      [ "$repair_rc" -eq 0 ] || repair_worker_failed=1
      printf '%s\n' "$repair_out" | tail -n 6
      if [ "$repair_rc" -eq 0 ]; then
        repaired_patch="$(printf '%s\n' "$repair_out" | sed -n 's/^patch: //p' | tail -n 1)"
        if [ -z "$repaired_patch" ] || [ ! -s "$repaired_patch" ]; then
          # peer-delegate has already replaced the task's review evidence. The
          # original frozen patch therefore cannot be recovered under this
          # review receipt, even though the repair process itself exited zero.
          intent_write abandoned "repair-returned-no-patch"
          park "repair-missing-patch" "the bounded repair returned no patch"
        fi
        if [ "${OMS_GOAL_DRIVE_TEST_STOP_AFTER_REPAIR_REVIEW:-0}" = 1 ]; then
          echo "goal-drive: injected stop after repaired review; rerun to reconcile $INTENT_ID" >&2
          exit 75
        fi
        old_intent="$INTENT_ID"
        intent_prepare "$task_id" "$repaired_patch" "$head_before" "$head_ref_before" repair "$old_intent"
        guard_expected_ref
        patch_abs="$(intent_patch_file)" || park "unsafe-patch-path" "inspect the repaired intent"
        verify="$TASK_RECEIPT_VERIFY"
        : > "$land_log"
        land_rc=0
        "$ROOT/scripts/patch-land.sh" --repo "$REPO" --patch "$patch_abs" \
          ${AVC_FLAG:+"$AVC_FLAG"} \
          --plan-task "$task_id" --verify "$verify" >"$land_log" 2>&1 || land_rc=$?
      fi
    else
      repair_rc=1
      intent_write prepared "repair-transition-failed"
    fi
  fi
  if [ "$land_rc" -ne 0 ] &&
    git -C "$REPO" apply --reverse --check --binary "$patch_abs" >/dev/null 2>&1 &&
    intent_tree_matches "$patch_abs"; then
    recover_landing_receipts || {
      printf '%s\n' "$(tail -n 8 "$land_log")" >&2
      rm -f "$land_log"
      park "landing-recovery-failed" "run oms patch-land --recover; the repaired outer commit intent remains open"
    }
    load_task_receipt "$task_id" ||
      park "landing-recovery-failed" "the recovered repaired-task receipt could not be read"
    [ "$TASK_RECEIPT_STATE" = "done" ] ||
      park "landing-recovery-incomplete" "the repaired patch is exact but its task receipt is $TASK_RECEIPT_STATE; keep the outer intent and recover it"
    land_rc=0
  elif [ "$land_rc" -ne 0 ] && [ -n "$(git_status_porcelain)" ]; then
    printf '%s\n' "$(tail -n 8 "$land_log")" >&2
    rm -f "$land_log"
    park "landing-tree-ambiguous" "repaired landing failed with a dirty non-exact tree; the outer intent remains open"
  fi
  if [ "$land_rc" -ne 0 ]; then
    # A failed repair worker releases its claim, and recovery may also
    # terminalize a failed landing fence. In either case the frozen patch no
    # longer has the exact review lease needed to land. Close the intent now;
    # otherwise every later goal-drive run would park behind authority that can
    # never become valid again. Ordinary admission/approval failures remain in
    # review and therefore keep their prepared intent resumable.
    load_task_receipt "$task_id" ||
      park "invalid-review-receipt" "the failed landing task receipt could not be read"
    current_task_state="$TASK_RECEIPT_STATE"
    current_task_lease="$TASK_RECEIPT_LEASE"
    if [ "$repair_worker_failed" = 1 ] && [ "$current_task_state" != blocked ]; then
      block_args=(--repo "$REPO" block --id "$task_id" --reason "bounded landing repair failed")
      case "$current_task_state" in
        claimed|running|review|landing)
          [ -z "$current_task_lease" ] || block_args+=(--lease-id "$current_task_lease")
          ;;
      esac
      "$ROOT/scripts/agent-plan.sh" "${block_args[@]}" >/dev/null 2>&1 ||
        park "repair-terminalize-failed" "the failed bounded repair could not be blocked; inspect task $task_id"
      load_task_receipt "$task_id" ||
        park "repair-terminalize-failed" "the blocked repair task receipt could not be read"
      current_task_state="$TASK_RECEIPT_STATE"
      current_task_lease="$TASK_RECEIPT_LEASE"
    fi
    landing_next="the frozen intent is resumable after fixing the gate or approval"
    if [ "$current_task_state" != review ] || [ "$current_task_lease" != "$INTENT_LEASE" ]; then
      intent_write abandoned "review-authority-left-${current_task_state:-missing}"
      landing_next="the review lease ended; inspect the failed repair, then re-open or replace the plan task"
    fi
    printf '%s\n' "$(tail -n 8 "$land_log")" >&2
    rm -f "$land_log"
    park "task-failed" "landing failed; $landing_next"
  fi
  rm -f "$land_log"
  guard_git_authority
  guard_expected_ref
  intent_write landed "patch-land-complete"
  if [ "${OMS_GOAL_DRIVE_TEST_STOP_AFTER_LAND:-0}" = 1 ]; then
    echo "goal-drive: injected stop after landing; rerun to reconcile $INTENT_ID" >&2
    exit 75
  fi
  intent_commit "committed"
done
