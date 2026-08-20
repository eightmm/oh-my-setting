#!/usr/bin/env bash
set -euo pipefail

# Bounded coding autopilot over the existing plan/drive/landing contracts. It
# never invents authority: generated task tranches remain proposal files until
# the parent reviews and passes them back to `run --proposal`. Only one r1-
# remainder tranche is possible. Mechanical acceptance is authoritative;
# cross-family semantic review starts in shadow mode. The sole built-in remote
# write path is delegated to draft-pr's exact create-only publication intent.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/model-routing.sh
. "$ROOT/scripts/lib/model-routing.sh"

REPO="$PWD"
ACTION=""
ACTION_REASON=""
ACTION_SESSION=""
PROPOSAL=""
PROPOSAL_SHA=""
PLANNER="claude"
WORKER="codex"
REVIEWER=""
ALLOWED=""
BASE=""
REMOTE="origin"
MAX_CYCLES=10
INITIAL_TASKS=6
REPLAN_TASKS=2
AUTO_REPAIR=0
ALLOW_VERIFIER_CHANGE=0
REVIEW_MODE="shadow"
REVIEW_MODE_EXPLICIT=0
DRAFT_PR=0
RETRY_KNOWN=0
PROVIDER_TIMEOUT="5m"
PLANNER_TIMEOUT=""
WORKER_TIMEOUT=""
REVIEWER_TIMEOUT=""
PLANNER_MODEL=""
WORKER_MODEL=""
REVIEWER_MODEL=""
PLANNER_FALLBACK_MODEL=""
WORKER_FALLBACK_MODEL=""
REVIEWER_FALLBACK_MODEL=""
PLANNER_REASONING_EFFORT="auto"
WORKER_REASONING_EFFORT="auto"
REVIEWER_REASONING_EFFORT="auto"
OUTER_RECEIPT=""
OUTER_RECEIPT_READY=0
APPROVED_PROPOSAL_PATH=""
APPROVED_PROPOSAL_SHA=""
review_base_sha=""

PLAN_FROM_SPEC="${OMS_AUTOPILOT_PLAN_FROM_SPEC:-$ROOT/scripts/plan-from-spec.sh}"
GOAL_DRIVE="${OMS_AUTOPILOT_GOAL_DRIVE:-$ROOT/scripts/goal-drive.sh}"
PEER_REVIEW="${OMS_AUTOPILOT_PEER_REVIEW:-$ROOT/scripts/peer-review.sh}"
DRAFT_PR_TOOL="${OMS_AUTOPILOT_DRAFT_PR:-$ROOT/scripts/draft-pr.sh}"
RECEIPT_HELPER="$ROOT/scripts/lib/autopilot-receipt.py"

usage() {
  cat <<'EOF'
Usage: autopilot.sh [options] propose
       autopilot.sh [options] run [--proposal FILE --expected-proposal-sha256 SHA]
       autopilot.sh [--repo PATH] status
       autopilot.sh [--repo PATH] shadow [--session ID]

Coordinate a confirmed PROJECT.md through a reviewed plan, bounded goal-drive,
one optional remainder proposal, semantic review, and optionally a Draft PR.

  propose                 Generate an initial proposal, or one r1- remainder
                          proposal after every current task is done. Exits 4
                          because a parent must review the exact proposal.
  run [--proposal FILE --expected-proposal-sha256 SHA]
                          Atomically apply a reviewed proposal when supplied
                          (the digest of the reviewed bytes is required and
                          rechecked against a read-once snapshot), drive the
                          plan, and finish locally or publish a Draft PR. Task
                          exhaustion may emit the sole r1- proposal and exit 4.
                          Every other unsafe stop exits 3.
  status                  Show the current plan and latest drive terminal row.
  abandon --reason TEXT   Retire the live outer receipt append-only: the exact
                          bytes are preserved content-addressed, a typed
                          abandon record names the predecessor digest and
                          reason, and a fresh propose may bind a new contract.
                          For receipts whose frozen contract cannot finish
                          (e.g. a provider wall that kills every call).
  reenter [--reason TEXT] Re-enter the live run from a fresh session with no
                          re-explanation: validate the receipt through the
                          same digest gate every resume passes, append a
                          typed reentry record (refusals are records too),
                          and exec the receipt-derived continuation. Refuses
                          proposal-review (re-entry cannot supply the parent
                          approval), parked (an operator decision is open),
                          done, and any digest drift — abandon plus a fresh
                          contract is the recovery. No new authority: the
                          resumed run keeps the original contract's terms.
                          Superseding a dead claimant also requeues the
                          claimed/running leases that session still held
                          (review and later stay untouched), recorded as a
                          typed requeue row.
  shadow [--session ID]   Observe, never act: run the exact reenter decision
                          sequence read-only and append one typed
                          would-resume/would-refuse row to the shadow
                          evidence ledger (.oms/plan/autopilot-shadow.jsonl).
                          Raise-after-evidence protocol input: no claim is
                          written, nothing resumes, and without a live
                          receipt the action is a silent no-op.

Options:
  --repo PATH             Repository (default: current directory).
  --planner PROVIDER      Plan/replan provider (default: claude).
  --worker PROVIDER       Implementation provider (default: codex).
  --reviewer PROVIDER     Semantic reviewer; defaults to a provider different
                          from the worker.
  --allowed PATHS         Immutable comma-separated task path envelope.
                          Required whenever a proposal may be generated.
  --base BRANCH           Review/PR base branch. Required for propose and run.
  --remote NAME           Git remote for base resolution and Draft PR (origin).
  --max-cycles N          goal-drive cycle cap, 1..10 (default: 10).
  --initial-tasks N       Initial proposal cap, 1..12 (default: 6).
  --replan-tasks N        Sole r1- proposal cap, 1..2 (default: 2).
  --auto-repair           Allow goal-drive's one same-lease landing repair.
  --retry-known           Explicitly retry a matching unresolved worker failure.
  --allow-verifier-change Contract-level consent, bound at propose time: the
                          drive forwards it to plan-run so a patch touching its
                          own verifier can land (base floor still applies).
                          Mid-run discovery cannot add it — abandon the run and
                          propose a fresh contract carrying the flag.
  --provider-timeout DUR  Default per-provider wall clock (default: 5m; max 24h).
  --planner-timeout DUR   Planner override for --provider-timeout.
  --worker-timeout DUR    Worker override for --provider-timeout.
  --reviewer-timeout DUR  Reviewer override for --provider-timeout.
  --planner-model MODEL   Exact planner model.
  --worker-model MODEL    Exact implementation model.
  --reviewer-model MODEL  Exact semantic-review model.
  --planner-fallback-model MODEL
  --worker-fallback-model MODEL
  --reviewer-fallback-model MODEL
                          One-shot capacity fallback for that role.
  --planner-reasoning-effort E
  --worker-reasoning-effort E
  --reviewer-reasoning-effort E
                          auto, low, medium, high, xhigh, max, or ultra.
  --review-mode MODE      shadow (default), gate, or off. Shadow reports a
                          semantic finding. With --draft-pr, gate is the
                          default unless this option is explicitly supplied.
  --draft-pr              After acceptance/review, prepare and publish an exact
                          create-only Draft PR. Never merges or releases.

Generated proposals never apply themselves. `run --proposal FILE
--expected-proposal-sha256 SHA` is the parent's exact approval boundary: the
digest printed at propose time must come back, and the bytes are snapshotted
once from a regular non-symlink file of at most 1 MiB and verified before
anything reads them. Proposal bytes and the plan CAS
are checked inside one atomic plan update; PROJECT.md, HEAD, and the allowed
envelope are bound to that durable plan and rechecked on every resume. A run started on the
named base first switches to a deterministic local feature branch.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

park() {
  if [ "${OUTER_RECEIPT_READY:-0}" -eq 1 ]; then
    outer_receipt_write parked >/dev/null 2>&1 ||
      echo "autopilot: warning: outer receipt could not record the park" >&2
  fi
  echo "autopilot: parked reason=$1" >&2
  [ -z "${2:-}" ] || echo "autopilot: parent-agent next: $2" >&2
  exit 3
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --proposal) [ "$#" -ge 2 ] || fail "--proposal requires a file"; PROPOSAL="$2"; shift 2 ;;
    --expected-proposal-sha256)
      [ "$#" -ge 2 ] || fail "--expected-proposal-sha256 requires a digest"
      PROPOSAL_SHA="$2"; shift 2 ;;
    --planner) [ "$#" -ge 2 ] || fail "--planner requires a provider"; PLANNER="$2"; shift 2 ;;
    --worker) [ "$#" -ge 2 ] || fail "--worker requires a provider"; WORKER="$2"; shift 2 ;;
    --reviewer) [ "$#" -ge 2 ] || fail "--reviewer requires a provider"; REVIEWER="$2"; shift 2 ;;
    --allowed) [ "$#" -ge 2 ] || fail "--allowed requires paths"; ALLOWED="$2"; shift 2 ;;
    --base) [ "$#" -ge 2 ] || fail "--base requires a branch"; BASE="$2"; shift 2 ;;
    --remote) [ "$#" -ge 2 ] || fail "--remote requires a name"; REMOTE="$2"; shift 2 ;;
    --max-cycles)
      [ "$#" -ge 2 ] || fail "--max-cycles requires a count"
      MAX_CYCLES="$2"; shift 2 ;;
    --initial-tasks)
      [ "$#" -ge 2 ] || fail "--initial-tasks requires a count"
      INITIAL_TASKS="$2"; shift 2 ;;
    --replan-tasks)
      [ "$#" -ge 2 ] || fail "--replan-tasks requires a count"
      REPLAN_TASKS="$2"; shift 2 ;;
    --auto-repair) AUTO_REPAIR=1; shift ;;
    --retry-known) RETRY_KNOWN=1; shift ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    --provider-timeout)
      [ "$#" -ge 2 ] || fail "--provider-timeout requires a duration"
      PROVIDER_TIMEOUT="$2"; shift 2 ;;
    --planner-timeout)
      [ "$#" -ge 2 ] || fail "--planner-timeout requires a duration"
      PLANNER_TIMEOUT="$2"; shift 2 ;;
    --worker-timeout)
      [ "$#" -ge 2 ] || fail "--worker-timeout requires a duration"
      WORKER_TIMEOUT="$2"; shift 2 ;;
    --reviewer-timeout)
      [ "$#" -ge 2 ] || fail "--reviewer-timeout requires a duration"
      REVIEWER_TIMEOUT="$2"; shift 2 ;;
    --planner-model)
      [ "$#" -ge 2 ] || fail "--planner-model requires a model"
      PLANNER_MODEL="$2"; shift 2 ;;
    --worker-model)
      [ "$#" -ge 2 ] || fail "--worker-model requires a model"
      WORKER_MODEL="$2"; shift 2 ;;
    --reviewer-model)
      [ "$#" -ge 2 ] || fail "--reviewer-model requires a model"
      REVIEWER_MODEL="$2"; shift 2 ;;
    --planner-fallback-model)
      [ "$#" -ge 2 ] || fail "--planner-fallback-model requires a model"
      PLANNER_FALLBACK_MODEL="$2"; shift 2 ;;
    --worker-fallback-model)
      [ "$#" -ge 2 ] || fail "--worker-fallback-model requires a model"
      WORKER_FALLBACK_MODEL="$2"; shift 2 ;;
    --reviewer-fallback-model)
      [ "$#" -ge 2 ] || fail "--reviewer-fallback-model requires a model"
      REVIEWER_FALLBACK_MODEL="$2"; shift 2 ;;
    --planner-reasoning-effort)
      [ "$#" -ge 2 ] || fail "--planner-reasoning-effort requires a value"
      PLANNER_REASONING_EFFORT="$2"; shift 2 ;;
    --worker-reasoning-effort)
      [ "$#" -ge 2 ] || fail "--worker-reasoning-effort requires a value"
      WORKER_REASONING_EFFORT="$2"; shift 2 ;;
    --reviewer-reasoning-effort)
      [ "$#" -ge 2 ] || fail "--reviewer-reasoning-effort requires a value"
      REVIEWER_REASONING_EFFORT="$2"; shift 2 ;;
    --review-mode)
      [ "$#" -ge 2 ] || fail "--review-mode requires shadow, gate, or off"
      REVIEW_MODE="$2"; REVIEW_MODE_EXPLICIT=1; shift 2 ;;
    --draft-pr) DRAFT_PR=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --reason)
      [ "$#" -ge 2 ] || fail "--reason requires text"
      ACTION_REASON="$2"; shift 2 ;;
    --session)
      [ "$#" -ge 2 ] || fail "--session requires a value"
      ACTION_SESSION="$2"; shift 2 ;;
    propose|run|status|abandon|reenter|shadow)
      [ -z "$ACTION" ] || fail "multiple actions: $ACTION, $1"
      ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
REPO="${REPO//$'\r'/}"
REPO="$(cd "$REPO" && pwd -P)" || fail "cannot resolve the physical repository path"
REPO="${REPO//$'\r'/}"
PLAN_FILE="$REPO/.oms/plan/tasks.json"
PROGRESS_FILE="$REPO/.oms/plan/progress.jsonl"
SPEC="$REPO/PROJECT.md"
OUTER_RECEIPT="$REPO/.oms/plan/autopilot-run.json"

if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  case "$ACTION" in
    status|shadow) ;;
    *) fail "a harness child cannot mutate parent autopilot authority" ;;
  esac
fi

outer_receipt_abandon_locked() {  # REASON
  local reason="$1" expected

  if [ ! -e "$OUTER_RECEIPT" ] && [ ! -L "$OUTER_RECEIPT" ]; then
    fail "no live autopilot receipt to abandon"
  fi
  expected="$(python3 "$RECEIPT_HELPER" digest "$OUTER_RECEIPT")" ||
    return $?
  expected="${expected//$'\r'/}"
  python3 "$RECEIPT_HELPER" abandon "$OUTER_RECEIPT" --repo "$REPO" \
    --expected "$expected" --reason "$reason" \
    --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}

# abandon touches only the receipt, deliberately before the spec/tree gates:
# the receipt being unfinishable (a frozen contract whose own value killed
# the run) is exactly when the operator needs this exit, and a dirty tree or
# changed spec must not block retiring it.
if [ "$ACTION" = abandon ]; then
  [ -n "${ACTION_REASON:-}" ] || fail "abandon requires --reason"
  record="$(oms_with_file_lock "$OUTER_RECEIPT" \
    outer_receipt_abandon_locked "$ACTION_REASON")" || exit $?
  record="${record//$'\r'/}"
  echo "autopilot: receipt abandoned; the exact contract is preserved and the exit recorded at $record"
  echo "autopilot: a fresh propose may now bind a new contract"
  exit 0
fi

if [ "$ACTION" = shadow ]; then
  # Observation only, early like abandon: a session start must never be
  # blocked by the spec/tree gates a real resume would face — the judgment
  # itself reports those as would-refuse. No receipt, nothing to observe,
  # and never any state or noise in a repo without a live run.
  [ -f "$OUTER_RECEIPT" ] || exit 0
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "unsafe repository Git execution config"
  python3 "$RECEIPT_HELPER" shadow-judge "$OUTER_RECEIPT" --repo "$REPO" \
    --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    ${ACTION_SESSION:+--session "$ACTION_SESSION"} || exit $?
  exit 0
fi

validate_duration() {  # VALUE LABEL
  local value="$1" label="$2" seconds
  case "$value" in
    [1-9]|[1-9][0-9]*|[1-9]s|[1-9][0-9]*s|[1-9]m|[1-9][0-9]*m|[1-9]h|[1-9][0-9]*h) ;;
    *) fail "$label must be a positive integer duration (s, m, or h)" ;;
  esac
  seconds="$(duration_seconds "$value")" || fail "$label is invalid"
  seconds="${seconds//$'\r'/}"
  [ "$seconds" -le 86400 ] || fail "$label must be at most 24h"
}

duration_seconds() {  # VALUE
  python3 - "$1" <<'PY'
import re, sys
match = re.fullmatch(r"([1-9][0-9]*)([smh]?)", sys.argv[1])
if not match:
    raise SystemExit(2)
print(int(match.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[match.group(2)])
PY
}

case "$MAX_CYCLES" in *[!0-9]*|"") fail "--max-cycles must be 1..10" ;; esac
[ "$MAX_CYCLES" -ge 1 ] && [ "$MAX_CYCLES" -le 10 ] || fail "--max-cycles must be 1..10"
case "$INITIAL_TASKS" in *[!0-9]*|"") fail "--initial-tasks must be 1..12" ;; esac
[ "$INITIAL_TASKS" -ge 1 ] && [ "$INITIAL_TASKS" -le 12 ] || fail "--initial-tasks must be 1..12"
case "$REPLAN_TASKS" in *[!0-9]*|"") fail "--replan-tasks must be 1..2" ;; esac
[ "$REPLAN_TASKS" -ge 1 ] && [ "$REPLAN_TASKS" -le 2 ] || fail "--replan-tasks must be 1..2"
[ "$DRAFT_PR" -eq 0 ] || [ "$REVIEW_MODE_EXPLICIT" -eq 1 ] || REVIEW_MODE=gate
case "$REVIEW_MODE" in shadow|gate|off) ;; *) fail "--review-mode must be shadow, gate, or off" ;; esac
case "$REMOTE" in *[!A-Za-z0-9._-]*|"") fail "--remote has unsafe characters" ;; esac
validate_duration "$PROVIDER_TIMEOUT" "--provider-timeout"
[ -n "$PLANNER_TIMEOUT" ] || PLANNER_TIMEOUT="$PROVIDER_TIMEOUT"
[ -n "$WORKER_TIMEOUT" ] || WORKER_TIMEOUT="$PROVIDER_TIMEOUT"
[ -n "$REVIEWER_TIMEOUT" ] || REVIEWER_TIMEOUT="$PROVIDER_TIMEOUT"
validate_duration "$PLANNER_TIMEOUT" "--planner-timeout"
validate_duration "$WORKER_TIMEOUT" "--worker-timeout"
validate_duration "$REVIEWER_TIMEOUT" "--reviewer-timeout"
planner_seconds="$(duration_seconds "$PLANNER_TIMEOUT")"; planner_seconds="${planner_seconds//$'\r'/}"
worker_seconds="$(duration_seconds "$WORKER_TIMEOUT")"; worker_seconds="${worker_seconds//$'\r'/}"
reviewer_seconds="$(duration_seconds "$REVIEWER_TIMEOUT")"; reviewer_seconds="${reviewer_seconds//$'\r'/}"
# A routed model call may spend primary + two safeguard recoveries + one
# catalog alternate + one capacity fallback before returning.
PLANNER_PHASE_WALL=$((planner_seconds * 5 + 60))
# goal-drive may spend up to five bounded model attempts per cycle. It also performs one
# separately supervised repair attempt when enabled. These are ceilings only;
# every inner call retains the exact role timeout from the durable contract.
worker_phase_attempts=$((MAX_CYCLES * 5))
[ "$AUTO_REPAIR" -eq 0 ] || worker_phase_attempts=$((worker_phase_attempts + 5))
WORKER_PHASE_WALL=$((worker_seconds * worker_phase_attempts + 60))
REVIEWER_PHASE_WALL=$((reviewer_seconds * 5 + 60))
# draft-pr has its own bounded verifier/network workflow and may legitimately
# outlive one model-provider attempt. Keep the outer kill fence conservative
# but bounded independently from --provider-timeout so it cannot truncate the
# verifier's default 10 minute budget (or another documented bounded value).
PUBLISH_PHASE_WALL=86400
oms_model_validate_name "$PLANNER_MODEL" || exit $?
oms_model_validate_name "$WORKER_MODEL" || exit $?
oms_model_validate_name "$REVIEWER_MODEL" || exit $?
oms_model_validate_name "$PLANNER_FALLBACK_MODEL" || exit $?
oms_model_validate_name "$WORKER_FALLBACK_MODEL" || exit $?
oms_model_validate_name "$REVIEWER_FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$PLANNER_REASONING_EFFORT" || exit $?
oms_reasoning_validate "$WORKER_REASONING_EFFORT" || exit $?
oms_reasoning_validate "$REVIEWER_REASONING_EFFORT" || exit $?
if [ -n "$PROPOSAL_SHA" ]; then
  case "$PROPOSAL_SHA" in
    *[!0-9a-f]*|"") fail "--expected-proposal-sha256 must be a lowercase SHA-256" ;;
  esac
  [ "${#PROPOSAL_SHA}" -eq 64 ] ||
    fail "--expected-proposal-sha256 must be a lowercase SHA-256"
fi

if [ "$ACTION" != status ]; then
  # The wrapper itself checks out the deterministic work branch. Disable
  # mutable repository hooks before that top-level Git effect, then let every
  # descendant inherit the same command-scope guard.
  AUTOPILOT_GIT_CONFIG_COUNT="${GIT_CONFIG_COUNT:-0}"
  case "$AUTOPILOT_GIT_CONFIG_COUNT" in
    0|[1-9]|[1-9][0-9]*) ;;
    *) fail "GIT_CONFIG_COUNT must be a canonical non-negative integer" ;;
  esac
  [ "$AUTOPILOT_GIT_CONFIG_COUNT" -lt 64 ] || fail "GIT_CONFIG_COUNT is too large"
  AUTOPILOT_HOOK_KEY="GIT_CONFIG_KEY_$AUTOPILOT_GIT_CONFIG_COUNT"
  AUTOPILOT_HOOK_VALUE="GIT_CONFIG_VALUE_$AUTOPILOT_GIT_CONFIG_COUNT"
  printf -v "$AUTOPILOT_HOOK_KEY" '%s' core.hooksPath
  printf -v "$AUTOPILOT_HOOK_VALUE" '%s' /dev/null
  export "${AUTOPILOT_HOOK_KEY?}" "${AUTOPILOT_HOOK_VALUE?}"
  GIT_CONFIG_COUNT=$((AUTOPILOT_GIT_CONFIG_COUNT + 1))
  export GIT_CONFIG_COUNT
fi

if [ "$ACTION" = status ]; then
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "unsafe repository Git execution config"
  oms_git_assert_plain_index "$REPO" ||
    fail "skip-worktree/assume-unchanged flags make autopilot status unsafe"
  if [ -f "$PLAN_FILE" ]; then
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" status
  else
    echo "autopilot: no plan"
  fi
  if [ -f "$PROGRESS_FILE" ]; then
    python3 - "$PROGRESS_FILE" <<'PY'
import json, sys
latest = None
with open(sys.argv[1], encoding="utf-8", errors="replace") as handle:
    for line in handle:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and row.get("kind") == "terminal":
            latest = row
if latest:
    print("latest drive: %s (%s)" % (latest.get("status", "?"), latest.get("reason", "?")))
PY
  fi
  python3 - "$REPO/.oms/plan" <<'PY'
import json, pathlib, sys
directory = pathlib.Path(sys.argv[1])
def sort_key(path):
    # A proposal can vanish or dangle between glob and stat; diagnostics must
    # never crash at exactly the moment an operator needs them.
    try:
        return (path.stat().st_mtime, path.name)
    except OSError:
        return (0, path.name)
proposals = sorted(directory.glob("proposal-*.json"), key=sort_key) if directory.is_dir() else []
if proposals:
    latest = proposals[-1]
    try:
        with latest.open(encoding="utf-8") as handle:
            row = json.load(handle)
        tasks = row.get("tasks") or []
        print("latest proposal: %s (%d task(s), prefix=%s)" % (
            latest, len(tasks), row.get("id_prefix") or "initial"))
    except (OSError, ValueError):
        print("latest proposal: %s (unreadable)" % latest)
PY
  if [ -e "$OUTER_RECEIPT" ] || [ -L "$OUTER_RECEIPT" ]; then
    if ! python3 "$RECEIPT_HELPER" inspect "$OUTER_RECEIPT" \
      --repo "$REPO" --continuation; then
      echo "outer run: invalid receipt (no continuation)"
    fi
  fi
  exit 0
fi

AUTOPILOT_TMPDIR=""
PHASE_SUPERVISOR_PID=""
autopilot_cleanup() {
  [ -z "${AUTOPILOT_TMPDIR:-}" ] || rm -rf -- "$AUTOPILOT_TMPDIR"
}
autopilot_stop_phase() {  # SIGNAL
  local signal_name="$1"
  [ -n "${PHASE_SUPERVISOR_PID:-}" ] || return 0
  kill -s "$signal_name" "$PHASE_SUPERVISOR_PID" 2>/dev/null || true
  wait "$PHASE_SUPERVISOR_PID" 2>/dev/null || true
  PHASE_SUPERVISOR_PID=""
}
autopilot_signal() {  # SIGNAL EXIT_CODE
  local signal_name="$1" exit_code="$2"
  trap - EXIT HUP INT TERM
  autopilot_stop_phase "$signal_name"
  autopilot_cleanup
  exit "$exit_code"
}
trap autopilot_cleanup EXIT
trap 'autopilot_signal HUP 129' HUP
trap 'autopilot_signal INT 130' INT
trap 'autopilot_signal TERM 143' TERM
AUTOPILOT_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/oms-autopilot.XXXXXX")" ||
  fail "cannot allocate private autopilot scratch"
case "$AUTOPILOT_TMPDIR" in ""|/|"${HOME:-/nonexistent}") fail "unsafe autopilot scratch path" ;; esac
chmod 700 "$AUTOPILOT_TMPDIR" 2>/dev/null || fail "cannot protect autopilot scratch"

autopilot_mktemp() {
  mktemp "$AUTOPILOT_TMPDIR/item.XXXXXX"
}

# reenter is the audited entrance mirroring abandon's exit: a fresh session
# re-enters the live run with zero re-explanation. The helper appends the
# typed record first (refusals included), validates the digests through the
# same gate every resume passes, and prints the receipt-derived continuation
# — which is then exec'd, so execution derives from typed contract fields,
# never from replayed prose. Runs after the child-authority guard: a harness
# child cannot re-enter parent autopilot authority.
if [ "$ACTION" = reenter ]; then
  [ -f "$OUTER_RECEIPT" ] ||
    fail "no live autopilot receipt to re-enter; propose starts a new run"
  reenter_out="$(autopilot_mktemp)" || fail "mktemp failed"
  # $$ survives the exec below, so the claim pid stays checkable for the
  # whole resumed run: liveness is the claim's expiry.
  oms_with_file_lock "$OUTER_RECEIPT" python3 "$RECEIPT_HELPER" reenter \
    "$OUTER_RECEIPT" --repo "$REPO" --reason "${ACTION_REASON:-}" \
    --pid "$$" \
    --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$reenter_out" || exit $?
  reenter_cont=()
  while IFS= read -r reenter_line; do
    reenter_line="${reenter_line//$'\r'/}"
    [ -n "$reenter_line" ] || continue
    reenter_cont+=("$reenter_line")
  done < "$reenter_out"
  [ "${#reenter_cont[@]}" -ge 3 ] || fail "reenter continuation is malformed"
  [ "${reenter_cont[0]}" = oms ] && [ "${reenter_cont[1]}" = autopilot ] ||
    fail "reenter continuation is malformed"
  echo "autopilot: re-entering the live run (typed record appended to .oms/plan/autopilot-reentries.jsonl)"
  # Superseding a dead claimant is proof the previous session died; leases it
  # still holds would otherwise park the resumed drive as tasks-exhausted
  # (field drill, 2026-08-19). Requeue claimed/running only: review holds a
  # frozen patch the landing path owns, and a receipt with no claim rows at
  # all gets no requeue — the operator release path stays for those.
  reenter_dead_pid="$(tail -n 1 "$REPO/.oms/plan/autopilot-reentries.jsonl" 2>/dev/null |
    python3 -c 'import json,sys
try:
    print(int(json.loads(sys.stdin.read()).get("superseded_stale_claim_pid") or 0))
except Exception:
    print(0)')"
  if [ "${reenter_dead_pid:-0}" -gt 0 ]; then
    reenter_requeued=""
    for reenter_state in claimed running; do
      while IFS= read -r reenter_tid; do
        [ -n "$reenter_tid" ] || continue
        if "$ROOT/scripts/agent-plan.sh" --repo "$REPO" release \
            --id "$reenter_tid" >/dev/null 2>&1; then
          reenter_requeued="${reenter_requeued:+$reenter_requeued,}$reenter_tid"
        fi
      done <<EOF_REENTER
$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" list --state "$reenter_state" 2>/dev/null | awk '{print $1}')
EOF_REENTER
    done
    if [ -n "$reenter_requeued" ]; then
      oms_with_file_lock "$OUTER_RECEIPT" python3 "$RECEIPT_HELPER" reenter-note \
        "$OUTER_RECEIPT" --repo "$REPO" --requeued "$reenter_requeued" \
        --dead-pid "$reenter_dead_pid" \
        --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >/dev/null ||
        fail "could not record the lease requeue"
      echo "autopilot: requeued task(s) $reenter_requeued held by dead session $reenter_dead_pid"
    fi
  fi
  trap - EXIT HUP INT TERM
  autopilot_cleanup
  exec bash "$0" "${reenter_cont[@]:2}"
fi

run_phase() {  # LABEL WALL_SECONDS COMMAND...
  local label="$1" wall="$2" phase_rc=0
  shift 2
  OMS_AUTOPILOT_PHASE_WALL="$wall" \
    python3 "$RECEIPT_HELPER" supervise --wall "$wall" --kill-after 1 \
    --label "$label" -- bash "$@" &
  PHASE_SUPERVISOR_PID=$!
  if wait "$PHASE_SUPERVISOR_PID"; then
    phase_rc=0
  else
    phase_rc=$?
  fi
  PHASE_SUPERVISOR_PID=""
  return "$phase_rc"
}

autopilot_shell_join() {  # ARGV...
  python3 - "$@" <<'PY' | tr -d '\r'
import shlex, sys
print(" ".join(shlex.quote(value) for value in sys.argv[1:]))
PY
}

resolve_review_base_sha() {
  local ref sha
  ref="refs/remotes/$REMOTE/$BASE"
  if ! git -C "$REPO" rev-parse --verify --quiet "$ref^{commit}" >/dev/null; then
    ref="refs/heads/$BASE"
  fi
  sha="$(git -C "$REPO" rev-parse --verify "$ref^{commit}" 2>/dev/null)" ||
    fail "cannot resolve --base from refs/remotes/$REMOTE/$BASE or refs/heads/$BASE"
  sha="${sha//$'\r'/}"
  case "$sha" in *[!0-9a-f]*|"") fail "resolved base commit is invalid" ;; esac
  case "${#sha}" in 40|64) ;; *) fail "resolved base commit has an unsupported object id" ;; esac
  printf '%s\n' "$sha"
}

outer_receipt_branch() {
  local current target
  current="$(git -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
    fail "autopilot requires a branch for its outer receipt"
  current="${current//$'\r'/}"
  target="oms/autopilot-$(printf '%.12s' "$BOUND_SPEC_SHA")"
  if [ "$current" = "$BASE" ]; then printf '%s\n' "$target"; else printf '%s\n' "$current"; fi
}

outer_receipt_write_locked() {  # STAGE
  local stage="$1" expected="absent" receipt_branch now
  local -a receipt_args
  if [ -e "$OUTER_RECEIPT" ] || [ -L "$OUTER_RECEIPT" ]; then
    expected="$(python3 "$RECEIPT_HELPER" digest "$OUTER_RECEIPT")" || return $?
    expected="${expected//$'\r'/}"
  fi
  receipt_branch="${work_branch:-}"
  [ -n "$receipt_branch" ] || receipt_branch="$(outer_receipt_branch)" || return $?
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  receipt_args=(write "$OUTER_RECEIPT" --expected "$expected" --stage "$stage" \
    --repo "$REPO" --spec-sha256 "$BOUND_SPEC_SHA" \
    --planner "$PLANNER" --worker "$WORKER" --reviewer "$REVIEWER" \
    --planner-model "$PLANNER_MODEL" --worker-model "$WORKER_MODEL" \
    --reviewer-model "$REVIEWER_MODEL" \
    --planner-fallback-model "$PLANNER_FALLBACK_MODEL" \
    --worker-fallback-model "$WORKER_FALLBACK_MODEL" \
    --reviewer-fallback-model "$REVIEWER_FALLBACK_MODEL" \
    --planner-reasoning-effort "$PLANNER_REASONING_EFFORT" \
    --worker-reasoning-effort "$WORKER_REASONING_EFFORT" \
    --reviewer-reasoning-effort "$REVIEWER_REASONING_EFFORT" \
    --provider-timeout "$PROVIDER_TIMEOUT" \
    --planner-timeout "$PLANNER_TIMEOUT" --worker-timeout "$WORKER_TIMEOUT" \
    --reviewer-timeout "$REVIEWER_TIMEOUT" --allowed "$ALLOWED" \
    --base "$BASE" --base-sha "$review_base_sha" --remote "$REMOTE" \
    --max-cycles "$MAX_CYCLES" --initial-tasks "$INITIAL_TASKS" \
    --replan-tasks "$REPLAN_TASKS" --review-mode "$REVIEW_MODE" \
    --proposal "$APPROVED_PROPOSAL_PATH" \
    --proposal-sha256 "$APPROVED_PROPOSAL_SHA" \
    --branch "$receipt_branch" --updated "$now")
  [ "$AUTO_REPAIR" -eq 0 ] || receipt_args+=(--auto-repair)
  [ "$RETRY_KNOWN" -eq 0 ] || receipt_args+=(--retry-known)
  [ "$ALLOW_VERIFIER_CHANGE" -eq 0 ] || receipt_args+=(--allow-verifier-change)
  [ "$DRAFT_PR" -eq 0 ] || receipt_args+=(--draft-pr)
  python3 "$RECEIPT_HELPER" "${receipt_args[@]}"
}

outer_receipt_write() {  # STAGE
  local value
  value="$(oms_with_file_lock "$OUTER_RECEIPT" outer_receipt_write_locked "$1")" || return $?
  value="${value//$'\r'/}"
  OUTER_RECEIPT_READY=1
  printf '%s\n' "$value"
}

outer_receipt_archive_done_locked() {
  local metadata stage receipt_sha
  metadata="$(python3 "$RECEIPT_HELPER" metadata "$OUTER_RECEIPT")" || return $?
  metadata="${metadata//$'\r'/}"
  stage="$(printf '%s\n' "$metadata" | awk -F '\t' '{print $1}')"
  receipt_sha="$(printf '%s\n' "$metadata" | awk -F '\t' '{print $3}')"
  [ "$stage" = "done" ] || return 4
  python3 "$RECEIPT_HELPER" archive-done "$OUTER_RECEIPT" \
    --repo "$REPO" --expected "$receipt_sha" >/dev/null
}

outer_receipt_prepare_new() {
  local metadata stage
  [ -e "$OUTER_RECEIPT" ] || [ -L "$OUTER_RECEIPT" ] || return 0
  metadata="$(python3 "$RECEIPT_HELPER" metadata "$OUTER_RECEIPT")" || return $?
  metadata="${metadata//$'\r'/}"
  stage="$(printf '%s\n' "$metadata" | awk -F '\t' '{print $1}')"
  [ "$stage" = "done" ] || return 0
  oms_with_file_lock "$OUTER_RECEIPT" outer_receipt_archive_done_locked
}

snapshot_regular_proposal() {  # SOURCE DEST
  python3 - "$1" "$2" <<'PY'
import os
import stat
import sys

source, destination = sys.argv[1:]
limit = 1024 * 1024
source_fd = None
destination_fd = None
destination_created = False

class ProposalError(Exception):
    pass

def identity(value):
    return (value.st_dev, value.st_ino)

try:
    before = os.lstat(source)
    if not stat.S_ISREG(before.st_mode):
        raise ProposalError("proposal must be a regular non-symlink file")
    if before.st_size > limit:
        raise ProposalError("proposal exceeds the 1 MiB snapshot limit")

    flags = os.O_RDONLY
    for name in ("O_BINARY", "O_CLOEXEC", "O_NOFOLLOW", "O_NONBLOCK"):
        flags |= getattr(os, name, 0)
    source_fd = os.open(source, flags)
    opened = os.fstat(source_fd)
    if not stat.S_ISREG(opened.st_mode) or identity(before) != identity(opened):
        raise ProposalError("proposal changed while its snapshot was opened")
    if opened.st_size > limit:
        raise ProposalError("proposal exceeds the 1 MiB snapshot limit")

    payload = bytearray()
    while len(payload) <= limit:
        chunk = os.read(source_fd, min(65536, limit + 1 - len(payload)))
        if not chunk:
            break
        payload.extend(chunk)
    if len(payload) > limit:
        raise ProposalError("proposal exceeds the 1 MiB snapshot limit")

    after = os.lstat(source)
    if (not stat.S_ISREG(after.st_mode) or
            identity(opened) != identity(after)):
        raise ProposalError("proposal changed while it was snapshotted")

    output_flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    output_flags |= getattr(os, "O_BINARY", 0) | getattr(os, "O_CLOEXEC", 0)
    destination_fd = os.open(destination, output_flags, 0o600)
    destination_created = True
    view = memoryview(payload)
    while view:
        written = os.write(destination_fd, view)
        if written <= 0:
            raise OSError("short snapshot write")
        view = view[written:]
except ProposalError as exc:
    print("error: %s" % exc, file=sys.stderr)
    if destination_created:
        try:
            os.unlink(destination)
        except OSError:
            pass
    raise SystemExit(2)
except (OSError, ValueError):
    print("error: cannot safely snapshot the proposal file", file=sys.stderr)
    if destination_created:
        try:
            os.unlink(destination)
        except OSError:
            pass
    raise SystemExit(2)
finally:
    for descriptor in (destination_fd, source_fd):
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
PY
}

[ -f "$SPEC" ] || fail "PROJECT.md is required; complete the project spec first"
spec_state="$(sed -n 's/^- State:[[:space:]]*//p' "$SPEC" | sed -n 1p)"
spec_state="${spec_state//$'\r'/}"
case "$spec_state" in
  confirmed|active) ;;
  *) fail "PROJECT.md must be confirmed (legacy active is also accepted)" ;;
esac
oms_git_assert_safe_execution_config "$REPO" ||
  fail "unsafe repository Git execution config"
oms_git_assert_plain_index "$REPO" ||
  fail "skip-worktree/assume-unchanged flags are forbidden during autopilot"
dirty="$(git -c core.fsmonitor=false -C "$REPO" \
  status --porcelain --untracked-files=all)"
[ -z "$dirty" ] || fail "tree is dirty; autopilot requires a dedicated clean worktree"

PLANNER="$(oms_normalize_provider "$PLANNER")" || fail "unknown planner provider"
WORKER="$(oms_normalize_provider "$WORKER")" || fail "unknown worker provider"
if [ -z "$REVIEWER" ]; then
  if [ "$WORKER" = codex ]; then REVIEWER=claude; else REVIEWER=codex; fi
fi
REVIEWER="$(oms_normalize_provider "$REVIEWER")" || fail "unknown reviewer provider"
if [ "$REVIEW_MODE" != off ] && [ "$REVIEWER" = "$WORKER" ]; then
  fail "semantic reviewer must differ from the implementation provider"
fi

plan_view() {  # MODE: all-done|has-r1|accept
  python3 - "$PLAN_FILE" "$1" <<'PY' | tr -d '\r'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
tasks = data.get("tasks") or {}
mode = sys.argv[2]
if mode == "all-done":
    print("1" if tasks and all(t.get("state") == "done" for t in tasks.values()) else "0")
elif mode == "has-r1":
    print("1" if any(str(key).startswith("r1-") for key in tasks) else "0")
elif mode == "accept":
    print(data.get("accept") or "")
PY
}

normalize_allowed() {  # TEXT
  python3 - "$1" <<'PY' | tr -d '\r'
import re, sys

def clean(value):
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(part in ("", ".", "..") for part in value.split("/")))):
        raise SystemExit(2)
    return value

items = sorted(set(clean(item) for item in re.split(r"[,\s]+", sys.argv[1]) if item.strip()))
if not items:
    raise SystemExit(2)
print(",".join(items))
PY
}

BOUND_SPEC_SHA=""
bind_plan_contract() {
  local meta contract_spec contract_allowed requested current_spec
  meta="$(python3 - "$PLAN_FILE" <<'PY'
import json, re, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
contract = data.get("project_contract")
if not isinstance(contract, dict) or contract.get("schema") != 1:
    raise SystemExit(2)
spec = contract.get("spec_sha256")
allowed = contract.get("allowed_envelope")
if (not isinstance(spec, str) or len(spec) != 64 or
        any(ch not in "0123456789abcdef" for ch in spec) or
        not isinstance(allowed, list) or not allowed or
        any(not isinstance(item, str) for item in allowed)):
    raise SystemExit(2)
print(spec)
print(",".join(allowed))
PY
)" || fail "approved plan lacks a valid PROJECT.md/scope contract; create a reviewed proposal"
  meta="${meta//$'\r'/}"
  contract_spec="$(printf '%s\n' "$meta" | sed -n 1p)"
  contract_allowed="$(printf '%s\n' "$meta" | sed -n 2p)"
  if [ -n "$ALLOWED" ]; then
    requested="$(normalize_allowed "$ALLOWED")" || fail "--allowed is not a safe path envelope"
    [ "$requested" = "$contract_allowed" ] ||
      fail "--allowed cannot widen or replace the reviewed plan envelope ($contract_allowed)"
  fi
  ALLOWED="$contract_allowed"
  current_spec="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
  [ "$current_spec" = "$contract_spec" ] ||
    fail "PROJECT.md changed after the approved plan was bound"
  BOUND_SPEC_SHA="$contract_spec"
}

proposal_info() {  # FILE -> prefix + presence
  python3 - "$1" "$PLAN_FILE" <<'PY' | tr -d '\r'
import json, os, re, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    proposal = json.load(handle)
prefix = proposal.get("id_prefix")
tasks = proposal.get("tasks")
if not isinstance(prefix, str) or (prefix and not re.fullmatch(r"[A-Za-z0-9._-]+", prefix)):
    raise SystemExit(2)
if not isinstance(tasks, list) or not tasks:
    raise SystemExit(2)
ids = [item.get("id") for item in tasks if isinstance(item, dict)]
if len(ids) != len(tasks) or any(not isinstance(item, str) for item in ids):
    raise SystemExit(2)
existing = set()
if os.path.exists(sys.argv[2]):
    with open(sys.argv[2], encoding="utf-8") as handle:
        existing = set((json.load(handle).get("tasks") or {}).keys())
present = [item in existing for item in ids]
state = "all" if present and all(present) else "partial" if any(present) else "none"
print(prefix)
print(state)
PY
}

propose_tasks() {  # PREFIX MAX
  local prefix="$1"
  local max_tasks="$2"
  local args out rc proposal_path proposal_digest continuation continuation_line

  [ -n "$ALLOWED" ] || fail "--allowed is required to generate a proposal"
  ALLOWED="$(normalize_allowed "$ALLOWED")" || fail "--allowed is not a safe path envelope"
  [ -n "$BOUND_SPEC_SHA" ] || BOUND_SPEC_SHA="$(oms_sha256_file "$SPEC")" ||
    fail "cannot hash PROJECT.md"
  [ -n "$review_base_sha" ] || review_base_sha="$(resolve_review_base_sha)"
  mkdir -p "$REPO/.oms/plan" || fail "cannot create the private plan directory"
  APPROVED_PROPOSAL_PATH=""
  APPROVED_PROPOSAL_SHA=""
  outer_receipt_prepare_new || fail "cannot archive the completed outer receipt"
  outer_receipt_write proposing >/dev/null ||
    fail "cannot bind this planning call to the durable outer receipt"
  args=(--repo "$REPO" --to "$PLANNER" --max-tasks "$max_tasks" --allowed "$ALLOWED")
  [ -z "$prefix" ] || args+=(--id-prefix "$prefix")
  [ -z "$PLANNER_MODEL" ] || args+=(--model "$PLANNER_MODEL")
  [ -z "$PLANNER_FALLBACK_MODEL" ] || args+=(--fallback-model "$PLANNER_FALLBACK_MODEL")
  args+=(--reasoning-effort "$PLANNER_REASONING_EFFORT" \
    --provider-timeout "$PLANNER_TIMEOUT")
  out="$(autopilot_mktemp)" || fail "mktemp failed"
  rc=0
  OMS_AUTOPILOT=1 run_phase planner "$PLANNER_PHASE_WALL" \
    "$PLAN_FROM_SPEC" "${args[@]}" > "$out" 2>&1 || rc=$?
  cat "$out"
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "planner installed unsafe repository Git execution config"
  oms_git_assert_plain_index "$REPO" ||
    fail "planner installed hidden Git index flags"
  if [ "$rc" -ne 0 ]; then
    rm -f "$out"
    return "$rc"
  fi
  proposal_path="$(sed -n 's/^plan-from-spec: proposed .* -> //p' "$out" | tail -n 1)"
  proposal_digest="$(sed -n 's/^plan-from-spec: proposal sha256: //p' "$out" | tail -n 1)"
  rm -f "$out"
  proposal_path="${proposal_path//$'\r'/}"
  proposal_digest="${proposal_digest//$'\r'/}"
  [ -n "$proposal_path" ] && [ -n "$proposal_digest" ] ||
    fail "the proposal was generated without a path/digest receipt"
  proposal_path="$(python3 - "$REPO" "$proposal_path" <<'PY' | tr -d '\r'
import os, sys
value = sys.argv[2]
if not os.path.isabs(value):
    value = os.path.join(sys.argv[1], value)
print(os.path.realpath(value))
PY
)" || fail "cannot resolve the proposal path"
  APPROVED_PROPOSAL_PATH="$proposal_path"
  APPROVED_PROPOSAL_SHA="$proposal_digest"
  outer_receipt_write proposal-review >/dev/null ||
    fail "cannot persist the outer proposal-review receipt"
  continuation=(oms autopilot --repo "$REPO" --planner "$PLANNER" \
    --worker "$WORKER" --reviewer "$REVIEWER" --allowed "$ALLOWED" \
    --base "$BASE" --remote "$REMOTE" \
    --max-cycles "$MAX_CYCLES" --initial-tasks "$INITIAL_TASKS" \
    --replan-tasks "$REPLAN_TASKS" --provider-timeout "$PROVIDER_TIMEOUT" \
    --planner-timeout "$PLANNER_TIMEOUT" --worker-timeout "$WORKER_TIMEOUT" \
    --reviewer-timeout "$REVIEWER_TIMEOUT" \
    --planner-reasoning-effort "$PLANNER_REASONING_EFFORT" \
    --worker-reasoning-effort "$WORKER_REASONING_EFFORT" \
    --reviewer-reasoning-effort "$REVIEWER_REASONING_EFFORT" \
    --review-mode "$REVIEW_MODE")
  [ -z "$PLANNER_MODEL" ] || continuation+=(--planner-model "$PLANNER_MODEL")
  [ -z "$WORKER_MODEL" ] || continuation+=(--worker-model "$WORKER_MODEL")
  [ -z "$REVIEWER_MODEL" ] || continuation+=(--reviewer-model "$REVIEWER_MODEL")
  [ -z "$PLANNER_FALLBACK_MODEL" ] ||
    continuation+=(--planner-fallback-model "$PLANNER_FALLBACK_MODEL")
  [ -z "$WORKER_FALLBACK_MODEL" ] ||
    continuation+=(--worker-fallback-model "$WORKER_FALLBACK_MODEL")
  [ -z "$REVIEWER_FALLBACK_MODEL" ] ||
    continuation+=(--reviewer-fallback-model "$REVIEWER_FALLBACK_MODEL")
  [ "$AUTO_REPAIR" -eq 0 ] || continuation+=(--auto-repair)
  [ "$RETRY_KNOWN" -eq 0 ] || continuation+=(--retry-known)
  [ "$ALLOW_VERIFIER_CHANGE" -eq 0 ] || continuation+=(--allow-verifier-change)
  [ "$DRAFT_PR" -eq 0 ] || continuation+=(--draft-pr)
  continuation+=(run --proposal "$proposal_path" \
    --expected-proposal-sha256 "$proposal_digest")
  continuation_line="$(autopilot_shell_join "${continuation[@]}")" ||
    fail "cannot render the proposal continuation"
  continuation_line="${continuation_line//$'\r'/}"
  echo "autopilot: proposal awaits parent-agent review"
  echo "autopilot: parent-agent continuation after review:"
  printf '  %s\n' "$continuation_line"
  return 4
}

if [ "$ACTION" = propose ]; then
  [ -n "$BASE" ] || fail "propose requires --base so its continuation is executable"
  git check-ref-format --branch "$BASE" >/dev/null 2>&1 ||
    fail "--base is not a valid branch name"
  if [ ! -f "$PLAN_FILE" ]; then
    propose_tasks "" "$INITIAL_TASKS" || exit $?
  fi
  bind_plan_contract
  [ "$(plan_view all-done)" = 1 ] ||
    fail "current plan still has unfinished tasks; run it before proposing a remainder"
  if "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept >/dev/null 2>&1; then
    echo "autopilot: acceptance already passes; no remainder proposal is needed"
    exit 0
  fi
  [ "$(plan_view has-r1)" = 0 ] || park "replan-budget-used" "inspect the remaining acceptance failure"
  propose_tasks r1- "$REPLAN_TASKS" || exit $?
fi

[ -n "$BASE" ] || fail "run requires --base for whole-change review and Draft PR binding"
git check-ref-format --branch "$BASE" >/dev/null 2>&1 || fail "--base is not a valid branch name"

if [ -n "$PROPOSAL" ]; then
  reviewed_proposal_path="$(python3 - "$REPO" "$PROPOSAL" <<'PY' | tr -d '\r'
import os, sys
value = sys.argv[2]
if not os.path.isabs(value):
    value = os.path.join(sys.argv[1], value)
print(os.path.realpath(value))
PY
)" || fail "cannot resolve the reviewed proposal path"
  [ -n "$ALLOWED" ] || fail "run --proposal requires the reviewed --allowed envelope"
  [ -n "$PROPOSAL_SHA" ] ||
    fail "run --proposal requires --expected-proposal-sha256 from the reviewed proposal"
  # Read the reviewed bytes exactly once into private scratch. Every later
  # consumer (tranche metadata, validation, atomic apply) sees this snapshot,
  # so a swap of the operator-named file after review changes nothing.
  proposal_snapshot="$AUTOPILOT_TMPDIR/proposal.json"
  proposal_snapshot_rc=0
  snapshot_regular_proposal "$PROPOSAL" "$proposal_snapshot" ||
    proposal_snapshot_rc=$?
  [ "$proposal_snapshot_rc" -eq 0 ] || exit "$proposal_snapshot_rc"
  snapshot_sha="$(oms_sha256_file "$proposal_snapshot")" || fail "cannot hash the proposal"
  [ "$snapshot_sha" = "$PROPOSAL_SHA" ] ||
    fail "proposal bytes do not match the reviewed --expected-proposal-sha256"
  APPROVED_PROPOSAL_PATH="$reviewed_proposal_path"
  APPROVED_PROPOSAL_SHA="$PROPOSAL_SHA"
  ALLOWED="$(normalize_allowed "$ALLOWED")" ||
    fail "--allowed is not a safe path envelope"
  BOUND_SPEC_SHA="$(oms_sha256_file "$SPEC")" || fail "cannot hash PROJECT.md"
  review_base_sha="$(resolve_review_base_sha)"
  outer_receipt_prepare_new || fail "cannot prepare the durable proposal receipt"
  outer_receipt_write proposal-review >/dev/null ||
    fail "run options differ from the durable proposal-review receipt"
  PROPOSAL="$proposal_snapshot"
  proposal_meta="$(proposal_info "$PROPOSAL")" || fail "proposal metadata/tasks are invalid"
  proposal_meta="${proposal_meta//$'\r'/}"
  prefix="$(printf '%s\n' "$proposal_meta" | sed -n 1p)"
  proposal_presence="$(printf '%s\n' "$proposal_meta" | sed -n 2p)"
  [ "$proposal_presence" != partial ] || fail "proposal is partially present in the plan"
  if [ "$proposal_presence" = none ]; then
    if [ -f "$PLAN_FILE" ]; then
      [ "$prefix" = r1- ] || fail "only the single r1- remainder may append to an existing plan"
      [ "$(plan_view all-done)" = 1 ] ||
        fail "a remainder proposal requires every approved task to be done"
      [ "$(plan_view has-r1)" = 0 ] || fail "the one remainder tranche was already used"
    else
      [ -z "$prefix" ] || fail "an initial proposal cannot spend the r1- remainder tranche"
    fi
  fi
  apply_cap="$INITIAL_TASKS"
  apply_args=(--repo "$REPO" --apply "$PROPOSAL" \
    --expected-proposal-sha256 "$PROPOSAL_SHA" \
    --max-tasks "$apply_cap" --allowed "$ALLOWED")
  if [ "$prefix" = r1- ]; then
    apply_cap="$REPLAN_TASKS"
    apply_args=(--repo "$REPO" --apply "$PROPOSAL" \
      --expected-proposal-sha256 "$PROPOSAL_SHA" \
      --max-tasks "$apply_cap" --allowed "$ALLOWED" --id-prefix r1-)
  elif [ -n "$prefix" ]; then
    fail "unsupported proposal tranche prefix: $prefix"
  fi
  OMS_AUTOPILOT=1 run_phase proposal-apply "$PLANNER_PHASE_WALL" \
    "$PLAN_FROM_SPEC" "${apply_args[@]}"
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "proposal apply installed unsafe repository Git execution config"
  oms_git_assert_plain_index "$REPO" ||
    fail "proposal apply installed hidden Git index flags"
fi

[ -f "$PLAN_FILE" ] || fail "no approved plan; run autopilot propose first"
bind_plan_contract

# A branch name is mutable. Freeze the reviewed commit before goal-drive can
# advance the current branch, then use this exact object for semantic review
# and (when requested) Draft PR base binding.
review_base_sha="$(resolve_review_base_sha)"
git -C "$REPO" merge-base --is-ancestor "$review_base_sha" HEAD 2>/dev/null ||
  fail "the frozen base commit is not an ancestor of the implementation branch"

ensure_work_branch() {
  local current target current_head target_head recovery_branch recovery_suffix
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "unsafe repository Git execution config before branch selection"
  oms_git_assert_plain_index "$REPO" ||
    fail "hidden Git index flags before branch selection"
  current="$(git -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
    fail "autopilot requires a branch, not detached HEAD"
  current="${current//$'\r'/}"
  git check-ref-format --branch "$current" >/dev/null 2>&1 || fail "current branch is invalid"
  target="oms/autopilot-$(printf '%.12s' "$BOUND_SPEC_SHA")"
  if [ "$current" != "$BASE" ]; then
    # Only the deterministic work branch of THIS contract, or one of its
    # strict -rN recovery branches, may be driven. An arbitrary checked-out
    # branch must never silently receive implementation commits.
    [ "$current" != "$target" ] || return 0
    case "$current" in
      "$target"-r*)
        recovery_suffix="${current#"$target-r"}"
        case "$recovery_suffix" in
          ""|0*|*[!0-9]*) ;;
          *) return 0 ;;
        esac
        ;;
    esac
    park "foreign-work-branch" \
      "checkout $BASE, $target, or a $target-rN recovery branch, then rerun"
  fi
  current_head="$(git -C "$REPO" rev-parse HEAD)"
  recovery_branch="$(git -C "$REPO" for-each-ref --sort=refname \
    --format='%(refname:short)' "refs/heads/$target-r*" |
    while IFS= read -r candidate; do
      candidate="${candidate//$'\r'/}"
      recovery_suffix="${candidate#"$target-r"}"
      case "$recovery_suffix" in ""|0*|*[!0-9]*) continue ;; esac
      printf '%s\n' "$candidate"
      break
    done)" ||
    park "autopilot-branch-inspection-failed" "inspect the local work branches"
  recovery_branch="${recovery_branch//$'\r'/}"
  [ -z "$recovery_branch" ] ||
    park "autopilot-recovery-branch-exists" \
      "checkout $recovery_branch and rerun the same autopilot command"
  if git -C "$REPO" show-ref --verify --quiet "refs/heads/$target"; then
    target_head="$(git -C "$REPO" rev-parse "refs/heads/$target")"
    [ "$target_head" = "$current_head" ] ||
      park "autopilot-branch-has-work" "inspect and explicitly resume the existing $target branch"
    git -C "$REPO" checkout -q "$target" ||
      park "autopilot-branch-busy" "use a dedicated worktree for $target"
    echo "autopilot: resumed local branch $target"
  else
    git -C "$REPO" checkout -qb "$target" ||
      park "autopilot-branch-create-failed" "create a clean feature branch"
    echo "autopilot: created local branch $target"
  fi
  [ -z "$(git -c core.fsmonitor=false -C "$REPO" \
    status --porcelain --untracked-files=all)" ] ||
    park "dirty-after-branch" "inspect the branch checkout"
  bind_plan_contract
}

work_branch_scope_check() {
  python3 - "$REPO" "$review_base_sha" "$ALLOWED" <<'PY'
import os
import subprocess
import sys

repo, base, allowed_text = sys.argv[1:]
try:
    raw = subprocess.check_output([
        "git", "-c", "core.fsmonitor=false", "-c", "diff.external=",
        "-C", repo, "diff", "--no-ext-diff", "--no-textconv",
        "--name-only", "--no-renames", "-z", base, "HEAD", "--",
    ], stderr=subprocess.DEVNULL)
except (OSError, subprocess.CalledProcessError):
    raise SystemExit(2)

allowed = allowed_text.split(",")
for value in raw.split(b"\0"):
    if not value:
        continue
    # `git ... -z` always uses '/' as its tree separator. On POSIX a
    # backslash is a literal filename byte, so normalizing it would turn a
    # root file such as `src\\escape` into a false member of allowed `src`.
    path = os.fsdecode(value)
    if not any(root == "." or path == root or path.startswith(root + "/")
               for root in allowed):
        raise SystemExit(3)
PY
}

assert_work_branch_scope() {  # PARK_REASON HIDDEN_INDEX_PARK_REASON
  local scope_rc=0
  oms_git_assert_safe_execution_config "$REPO" ||
    park "unsafe-git-execution-config" "remove executable repository Git config"
  oms_git_assert_plain_index "$REPO" ||
    park "$2" "clear skip-worktree/assume-unchanged flags and inspect tracked files"
  work_branch_scope_check || scope_rc=$?
  case "$scope_rc" in
    0) return 0 ;;
    3) park "$1" "remove or separately review history outside $ALLOWED" ;;
    *) park "work-branch-scope-unreadable" \
         "inspect the complete branch diff from the reviewed base" ;;
  esac
}

# Keep the named base immutable across interrupted local runs as well as Draft
# PR runs. That makes the frozen whole-change review base durable by topology:
# goal-drive commits only on the deterministic feature branch.
ensure_work_branch
work_branch="$(git -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
  fail "autopilot requires a branch after work-branch selection"
work_branch="${work_branch//$'\r'/}"
outer_receipt_prepare_new || fail "cannot prepare the durable run receipt"
outer_receipt_write approved >/dev/null ||
  fail "cannot bind this run to the durable outer receipt"
outer_receipt_write driving >/dev/null ||
  park "outer-receipt-write-failed" "inspect oms autopilot --repo . status"
assert_work_branch_scope "work-branch-outside-scope" "hidden-index-flags"
spec_sha="$BOUND_SPEC_SHA"
accept_cmd="$(plan_view accept)"
accept_cmd="${accept_cmd//$'\r'/}"
[ -n "$accept_cmd" ] || fail "approved plan has no acceptance command"
accept_sha="$(printf '%s' "$accept_cmd" | oms_sha256_stream)" || fail "cannot hash acceptance command"

drive_run_id="ap-$(date -u +%Y%m%dT%H%M%SZ)-$$"
drive_args=(--repo "$REPO" --to "$WORKER" --max-cycles "$MAX_CYCLES" \
  --run-id "$drive_run_id" --provider-timeout "$WORKER_TIMEOUT" \
  --reasoning-effort "$WORKER_REASONING_EFFORT" \
  --expected-ref "refs/heads/$work_branch")
[ -z "$WORKER_MODEL" ] || drive_args+=(--model "$WORKER_MODEL")
[ -z "$WORKER_FALLBACK_MODEL" ] || drive_args+=(--fallback-model "$WORKER_FALLBACK_MODEL")
[ "$AUTO_REPAIR" -eq 0 ] || drive_args+=(--auto-repair)
[ "$RETRY_KNOWN" -eq 0 ] || drive_args+=(--retry-known)
[ "$ALLOW_VERIFIER_CHANGE" -eq 0 ] || drive_args+=(--allow-verifier-change)
# Record this drive's wrapper pid on the run lineage before any provider
# spends money: a later re-entry distinguishes a live-but-slow session from
# a dead one by exactly this claim (run-level single-flight).
oms_with_file_lock "$OUTER_RECEIPT" python3 "$RECEIPT_HELPER" drive-claim \
  "$OUTER_RECEIPT" --repo "$REPO" --pid "$$" \
  --updated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >/dev/null ||
  fail "could not record the drive claim"
drive_out="$(autopilot_mktemp)" || fail "mktemp failed"
drive_rc=0
OMS_WORKER_GUARD_STRICT=1 OMS_WORKER_GUARD_OFF=0 \
  OMS_PEER_TIMEOUT="$WORKER_TIMEOUT" \
  run_phase goal-drive "$WORKER_PHASE_WALL" \
    "$GOAL_DRIVE" "${drive_args[@]}" > "$drive_out" 2>&1 || drive_rc=$?
cat "$drive_out"
oms_git_assert_safe_execution_config "$REPO" ||
  park "unsafe-git-execution-config" "remove executable repository Git config"
oms_git_assert_plain_index "$REPO" ||
  park "hidden-index-flags-after-drive" \
    "clear skip-worktree/assume-unchanged flags and inspect tracked files"

current_work_branch="$(git -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
  park "branch-changed-after-drive" "restore $work_branch and inspect the drive"
current_work_branch="${current_work_branch//$'\r'/}"
[ "$current_work_branch" = "$work_branch" ] ||
  park "branch-changed-after-drive" "restore $work_branch and inspect the drive"

current_spec_sha="$(oms_sha256_file "$SPEC")" || park "spec-unreadable" "restore PROJECT.md"
[ "$current_spec_sha" = "$spec_sha" ] || park "spec-changed" "review the changed project contract"
current_accept="$(plan_view accept)"
current_accept="${current_accept//$'\r'/}"
current_accept_sha="$(printf '%s' "$current_accept" | oms_sha256_stream)" ||
  park "acceptance-unhashable" "repair the plan contract"
[ "$current_accept_sha" = "$accept_sha" ] ||
  park "acceptance-changed" "review the changed acceptance command"
assert_work_branch_scope "work-branch-outside-scope-after-drive" \
  "hidden-index-flags-after-drive"

# goal-drive binds its receipt, status, and reason in one canonical final line
# written by the parent after its durable row. Captured child output may contain
# arbitrary text, so a duplicate prefix or any later non-empty line invalidates
# the result. The mutable progress ledger only cross-checks the exact result;
# it never selects the reason that can spend the bounded replan tranche.
drive_terminal_result() {  # RUN_ID OUTPUT -> STATUS<TAB>REASON
  [ -f "$PROGRESS_FILE" ] || return 0
  python3 - "$1" "$2" "$PROGRESS_FILE" <<'PY' | tr -d '\r'
import json
import re
import sys

run_id, output_path, progress_path = sys.argv[1:]
prefix = "goal-drive: terminal-v1 "
pattern = re.compile(
    r"goal-drive: terminal-v1 run=([A-Za-z0-9._-]{1,64}) "
    r"receipt=([0-9a-f]{64}) status=(park|done) "
    r"reason=([A-Za-z0-9._-]{1,128})")
candidate = None
candidate_line = None
last_nonempty = None
prefix_count = 0
try:
    with open(output_path, encoding="utf-8", errors="replace") as handle:
        for raw_line in handle:
            line = raw_line.rstrip("\r\n")
            if line:
                last_nonempty = line
            if line.startswith(prefix):
                prefix_count += 1
                match = pattern.fullmatch(line)
                if match is not None:
                    candidate = match.groups()
                    candidate_line = line
except OSError:
    raise SystemExit(0)

if (prefix_count != 1 or candidate is None or
        candidate_line != last_nonempty):
    raise SystemExit(0)
result_run, receipt, status, reason = candidate
if result_run != run_id:
    raise SystemExit(0)

matched = False
try:
    with open(progress_path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if (isinstance(row, dict) and row.get("schema") == 1 and
                    row.get("kind") == "terminal" and
                    row.get("run_id") == result_run and
                    row.get("receipt") == receipt and
                    row.get("status") == status and
                    row.get("reason") == reason):
                matched = True
                break
except OSError:
    matched = False
if matched:
    print("%s\t%s" % (status, reason))
PY
}

drive_result="$(drive_terminal_result "$drive_run_id" "$drive_out")"
drive_result="${drive_result//$'\r'/}"
drive_status="$(printf '%s\n' "$drive_result" | awk -F '\t' '{print $1}')"
drive_reason="$(printf '%s\n' "$drive_result" | awk -F '\t' '{print $2}')"
if [ "$drive_rc" -ne 0 ]; then
  if [ "$drive_rc" -eq 3 ] &&
    [ "$drive_status" = park ] && [ "$drive_reason" = tasks-exhausted ]; then
    [ "$(plan_view all-done)" = 1 ] ||
      park "tasks-exhausted-with-unfinished-work" "inspect the plan state"
    [ "$(plan_view has-r1)" = 0 ] ||
      park "replan-budget-used" "inspect the remaining acceptance failure"
    [ -z "$(git -c core.fsmonitor=false -C "$REPO" \
      status --porcelain --untracked-files=all)" ] ||
      park "dirty-before-replan" "preserve or remove foreign work"
    rm -f "$drive_out"
    propose_tasks r1- "$REPLAN_TASKS" || exit $?
  fi
  rm -f "$drive_out"
  # Keep the driver's own terminal reason: flattening every park into one
  # generic reason hides exactly the diagnosis the ledger row exists to carry
  # (a vacuous acceptance and a dead provider read identically otherwise).
  case "$drive_reason" in
    "" | *[!a-z0-9-]*) park "goal-drive-failed" "inspect the drive output and failure ledger" ;;
    *) park "goal-drive-$drive_reason" "inspect the drive output and failure ledger" ;;
  esac
fi
[ "$drive_status" = "done" ] && [ "$drive_reason" = acceptance-pass ] || {
  rm -f "$drive_out"
  park "goal-drive-result-invalid" \
    "inspect the missing or mismatched goal-drive terminal receipt"
}
rm -f "$drive_out"

# Re-run the exact acceptance receipt immediately before any semantic or remote
# effect. A provider's prose and goal-drive's terminal line are not proof here.
if ! "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept >/dev/null 2>&1; then
  park "final-acceptance-failed" "the repository changed after goal-drive completion"
fi

final_head="$(git -C "$REPO" rev-parse HEAD)"
final_tree="$(git -C "$REPO" rev-parse 'HEAD^{tree}')"
final_plan_sha="$(oms_sha256_file "$PLAN_FILE")" || fail "cannot hash the completed plan"
final_head="${final_head//$'\r'/}"
final_tree="${final_tree//$'\r'/}"
git -C "$REPO" merge-base --is-ancestor "$review_base_sha" "$final_head" 2>/dev/null ||
  park "base-not-ancestor-after-drive" "rebase onto the reviewed base and repeat the run"
assert_work_branch_scope "work-branch-outside-scope-before-review" \
  "hidden-index-flags-before-review"

assert_final_snapshot() {
  local current
  [ -z "$(git -c core.fsmonitor=false -C "$REPO" \
    status --porcelain --untracked-files=all)" ] ||
    park "tree-changed-after-acceptance" "inspect the foreign work"
  current="$(git -C "$REPO" symbolic-ref --quiet --short HEAD 2>/dev/null)" ||
    park "branch-changed-after-review" "restore $work_branch and repeat whole-change review"
  current="${current//$'\r'/}"
  [ "$current" = "$work_branch" ] ||
    park "branch-changed-after-review" "restore $work_branch and repeat whole-change review"
  current="$(git -C "$REPO" rev-parse HEAD)"; current="${current//$'\r'/}"
  [ "$current" = "$final_head" ] || park "head-changed-after-review" "repeat whole-change review"
  current="$(git -C "$REPO" rev-parse 'HEAD^{tree}')"; current="${current//$'\r'/}"
  [ "$current" = "$final_tree" ] || park "tree-changed-after-review" "repeat whole-change review"
  current="$(oms_sha256_file "$PLAN_FILE")" || park "plan-unreadable-after-review" "restore the plan"
  [ "$current" = "$final_plan_sha" ] || park "plan-changed-after-review" "repeat whole-change review"
  current="$(oms_sha256_file "$SPEC")" || park "spec-unreadable-after-review" "restore PROJECT.md"
  [ "$current" = "$spec_sha" ] || park "spec-changed-after-review" "repeat whole-change review"
}

review_rc=0
review_evidence="mode=off outcome=skipped reviewer=none"
outer_receipt_write reviewing >/dev/null ||
  park "outer-receipt-write-failed" "inspect oms autopilot --repo . status"
if [ "$REVIEW_MODE" != off ]; then
  review_out="$(autopilot_mktemp)" || fail "mktemp failed"
  review_args=(--repo "$REPO" --base "$review_base_sha" \
    --providers "$REVIEWER" --writer "$WORKER" --gate --verify "$accept_cmd")
  [ -z "$REVIEWER_MODEL" ] || review_args+=(--model "$REVIEWER_MODEL")
  [ -z "$REVIEWER_FALLBACK_MODEL" ] ||
    review_args+=(--fallback-model "$REVIEWER_FALLBACK_MODEL")
  review_args+=(--reasoning-effort "$REVIEWER_REASONING_EFFORT" \
    --prompt "Review whether the confirmed PROJECT.md goal and every explicit success criterion are satisfied by this whole change. Treat ambiguity as a finding; do not widen scope." \
  )
  OMS_PEER_TIMEOUT="$REVIEWER_TIMEOUT" \
    run_phase peer-review "$REVIEWER_PHASE_WALL" \
      "$PEER_REVIEW" "${review_args[@]}" > "$review_out" 2>&1 || review_rc=$?
  cat "$review_out"
  oms_git_assert_safe_execution_config "$REPO" ||
    park "unsafe-git-execution-config" "remove executable repository Git config"
  oms_git_assert_plain_index "$REPO" ||
    park "hidden-index-flags-after-review" \
      "clear skip-worktree/assume-unchanged flags and inspect tracked files"
  rm -f "$review_out"
  if [ "$review_rc" -eq 0 ]; then
    echo "autopilot: semantic review: pass"
    review_evidence="mode=$REVIEW_MODE outcome=pass reviewer=$REVIEWER"
  elif [ "$REVIEW_MODE" = shadow ]; then
    if [ "$review_rc" -eq 1 ]; then
      echo "autopilot: semantic review: advisory fail"
      review_evidence="mode=shadow outcome=advisory-fail reviewer=$REVIEWER"
    else
      echo "autopilot: semantic review: advisory incomplete"
      review_evidence="mode=shadow outcome=advisory-incomplete reviewer=$REVIEWER"
    fi
  else
    park "semantic-review-failed" "inspect the typed review outcome"
  fi
fi

assert_work_branch_scope "work-branch-outside-scope-after-review" \
  "hidden-index-flags-after-review"
assert_final_snapshot
# peer-review exit 1 can represent either semantic dissent or its own
# mechanical verifier failure. Shadow mode keeps dissent advisory, but never
# completion: the exact acceptance command must still pass on the frozen tree.
if ! "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept >/dev/null 2>&1; then
  park "post-review-acceptance-failed" "repair the mechanical gate before publication"
fi
assert_final_snapshot

if [ "$DRAFT_PR" -eq 1 ]; then
  outer_receipt_write publishing >/dev/null ||
    park "outer-receipt-write-failed" "inspect oms autopilot --repo . status"
  prepare_out="$(autopilot_mktemp)" || fail "mktemp failed"
  run_phase draft-prepare "$PUBLISH_PHASE_WALL" \
    "$DRAFT_PR_TOOL" prepare --repo "$REPO" --remote "$REMOTE" --base "$BASE" \
    --verify "$accept_cmd" --expected-head "$final_head" --expected-tree "$final_tree" \
    --expected-base-sha "$review_base_sha" \
    --expected-spec-sha256 "$spec_sha" \
    --review-evidence "$review_evidence" > "$prepare_out"
  cat "$prepare_out"
  oms_git_assert_safe_execution_config "$REPO" ||
    park "unsafe-git-execution-config" "remove executable repository Git config"
  oms_git_assert_plain_index "$REPO" ||
    park "hidden-index-flags-before-publication" \
      "clear skip-worktree/assume-unchanged flags and inspect tracked files"
  intent="$(sed -n 's/^intent: //p' "$prepare_out" | tail -n 1)"
  rm -f "$prepare_out"
  [ -n "$intent" ] || park "draft-intent-missing" "inspect draft-pr prepare output"
  assert_work_branch_scope "work-branch-outside-scope-before-publication" \
    "hidden-index-flags-before-publication"
  assert_final_snapshot
  run_phase draft-publish "$PUBLISH_PHASE_WALL" \
    "$DRAFT_PR_TOOL" publish --repo "$REPO" --intent "$intent"
  oms_git_assert_safe_execution_config "$REPO" ||
    park "unsafe-git-execution-config" "remove executable repository Git config"
  oms_git_assert_plain_index "$REPO" ||
    park "hidden-index-flags-after-publication" \
      "clear skip-worktree/assume-unchanged flags and inspect tracked files"
fi

outer_receipt_write "done" >/dev/null ||
  park "outer-receipt-write-failed" "inspect oms autopilot --repo . status"
echo "autopilot: done (acceptance passed)"
exit 0
