#!/usr/bin/env bash
set -euo pipefail

# A small, shared task graph for multi-agent work. Where agent-task.sh holds the
# single active handoff packet, agent-plan.sh holds a DAG of subtasks that can be
# split across Codex / Claude Code / Antigravity: each task has dependencies, a
# path scope, a verify command, and a state. "ready" computes which tasks are
# actionable now (state=ready and every dependency done). State lives in
# .oms/plan/tasks.json (git-ignored, agent-shared); writes are atomic.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"

# OMS_STATE_REPO: set by peer-delegate.sh for worktree workers so they
# read the primary repo's shared state instead of the throwaway checkout's.
REPO="${OMS_STATE_REPO:-$PWD}"
PLAN_FILE=""
ACTION=""
ID=""
TITLE=""
GOAL=""
PROVIDER=""
TTL=""
REASON=""
ARTIFACT=""
PATCH=""
EXECUTOR_ID=""
EXECUTOR_SOUL_SHA256=""
EXPECTED_REVIEW_PATCH=""
EXPECTED_REVIEW_PATCH_SHA256=""
EXPECTED_REVIEW_VERIFY=""
EXPECTED_REVIEW_EXECUTOR_ID=""
EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256=""
EXPECTED_REVIEW_LEASE_ID=""
EXPECTED_LANDING_RECEIPT_SHA256=""
EXPECTED_REVIEW_PATCH_SET=0
EXPECTED_REVIEW_PATCH_SHA256_SET=0
EXPECTED_REVIEW_VERIFY_SET=0
EXPECTED_REVIEW_EXECUTOR_ID_SET=0
EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256_SET=0
EXPECTED_REVIEW_LEASE_ID_SET=0
EXPECTED_LANDING_RECEIPT_SHA256_SET=0
DEPENDS=""
ALLOWED=""
FORBIDDEN=""
VERIFY=""
ACCEPT=""
ROLE=""
STATE_FILTER=""
CLAIM=0
REFREEZE_ACCEPTANCE=0
INCLUDE_RUNNING=0
INCLUDE_REVIEW=0
LEASE_ID="${OMS_PLAN_LEASE_ID:-}"
AS_JSON=0
PROPOSAL=""
EXPECTED_PROPOSAL_SHA256=""
EXPECTED_PLAN_SHA256=""
ALLOWED_ENVELOPE=""
MAX_TASKS=""
ACCEPT_FILES=""
OWNER_ID="${OMS_AUTOPILOT_OWNER_ID:-}"
MARKERS_DIR=""
EXPECTED_STATE=""
CHECK_ONLY=0

usage() {
  cat <<'EOF'
Usage: agent-plan.sh [--repo PATH] [--file PATH] <command> [options]

Commands:
  init   --goal TEXT [--accept CMD]  Create/replace the plan with a goal and an
                                     optional goal-level acceptance command —
                                     the executable definition of done.
  ensure-lineage                     Parent-only, idempotently mint the
                                     immutable plan_id for a legacy plan before
                                     plan-scoped evidence is produced.
  apply-proposal --proposal FILE --expected-proposal-sha256 SHA256
         --expected-plan-sha256 absent|SHA256
         [--goal TEXT --accept CMD] --allowed-envelope "p1,p2"
         [--accept-files "path1,path2"]
         [--max-tasks N]
                                     Atomically create/append every task in a
                                     reviewed proposal. The plan CAS, proposal
                                     digest, scope envelope, dependencies, and
                                     duplicate definitions are checked under
                                     the plan lock. Exact replay is idempotent.
  add    --id ID --title TEXT        Add a task (state: ready).
         [--depends a,b] [--allowed "p1,p2"] [--forbidden "p3"]
         [--verify CMD] [--role NAME]
  claim  --id ID --provider NAME [--ttl TEXT]   Claim a ready task for a worker.
  start  --id ID [--lease-id TOKEN]  Mark a claimed task running.
  touch  --id ID [--lease-id TOKEN]  Heartbeat a claimed/running task: refresh
                                     claimed_at so a live worker is not reclaimed.
  review --id ID [--lease-id TOKEN] [--artifact PATH] [--patch PATH]
         [--executor-id ID --executor-soul-sha256 SHA256]
                                     Move a claimed/running task to review.
  repair --id ID [--lease-id TOKEN] [--artifact PATH]
                                     Re-enter a reviewed task under the same
                                     lease for one explicitly bounded repair.
                                     Prior artifact/patch evidence is retained
                                     until a new review replaces it. --artifact
                                     stores the failed gate output for recovery.
  land   --id ID --lease-id TOKEN    Fence the exact admitted review receipt.
  finish --id ID --expected-landing-receipt-sha256 SHA [--refreeze-acceptance]
                                     Complete a landing-fenced task. With
                                     --refreeze-acceptance (patch-land
                                     forwards operator verifier-change
                                     consent), acceptance-manifest entries
                                     the fenced patch itself modified are
                                     recomputed from the landed tree; all
                                     other entries keep their frozen hashes,
                                     and each refreeze appends a typed row.
  lint-verify --verify CMD --allowed "p1,p2"
                                     Lint a verify/acceptance command against
                                     the admission floor: content reads of
                                     allowed paths print typed
                                     floor_incompatible_verifier lines and
                                     exit 2. The same module the admission
                                     gate loads — the two cannot drift.
         --expected-review-patch PATH --expected-review-patch-sha256 SHA256
         --expected-review-verify CMD --expected-review-lease-id TOKEN
         --expected-review-executor-id ID
         --expected-review-executor-soul-sha256 SHA256
  finish --id ID [--lease-id TOKEN] [--artifact PATH] [--patch PATH]
         --expected-landing-receipt-sha256 SHA256
                                     Mark the exact landed receipt done. The
                                     receipt is checked atomically under the
                                     plan lock before the transition.
  block  --id ID --reason TEXT       Mark a task blocked.
  release --id ID                    Requeue a claimed/running/review task to ready (worker died).
  recover-lease --id ID --lease-id TOKEN --expected-state STATE
                [--markers-dir PATH] [--check]
                                     Atomically requeue only the exact current
                                     claimed/running state+lease when no live
                                     exact worker marker exists. --check runs
                                     the same locked predicate without saving.
                                     Drift exits 3.
  recover-owner --owner-id ID [--markers-dir PATH] [--json]
                                     Requeue only claimed/running leases owned
                                     by one autopilot run. Exact live worker
                                     markers and unproven running tasks remain
                                     held; review/landing evidence is untouched.
  reclaim [--ttl SECONDS] [--include-running] [--include-review]
                                     Requeue claimed tasks whose TTL since
                                     claimed_at expired (dead-worker recovery).
                                     A numeric per-task ttl wins over --ttl
                                     (default 3600). running needs the opt-in
                                     flag. review holds a finished artifact
                                     awaiting a reviewer, so it is only
                                     reclaimed with --include-review, ages from
                                     its updated timestamp, and defaults to a
                                     longer TTL (86400) unless --ttl is given;
                                     its artifact/patch fields are kept.
  reopen --id ID                     Return a blocked task to ready.
  show   --id ID                     Print one task as JSON.
  evidence-snapshot --id ID          Print one task plus its immutable plan_id
                                     for a plan-scoped evidence producer.
  list   [--state STATE]             List tasks (optionally by state).
  ready                              Print ids actionable now (deps done).
  status                             Human-readable summary.
  accept                             Run the stored acceptance command from the
                                     repo root (outside the plan lock), append
                                     one row to .oms/plan/progress.jsonl, and
                                     exit 0 on pass / 3 on fail.
  brief  --id ID                     Print a paste-able work brief for a task.
  next   [--provider NAME] [--claim] [--ttl TEXT]
                                     Print the brief for the next actionable
                                     task; with --claim --provider, atomically
                                     claim it first (pull-work primitive).
         [--json]                    Emit the selected task as JSON for safe
                                     composition by another harness command.

State: ready -> claimed -> running -> review -> landing -> done. An explicit
repair moves review -> claimed without minting a lease. Any -> blocked (block);
blocked -> ready (reopen); claimed/running/review -> ready (release).
Tasks are stored in REPO/.oms/plan/tasks.json (override with --file).

A claim whose last heartbeat (claimed_at, refreshed by touch) is older than
OMS_PLAN_CLAIM_TTL seconds (default 3600; a numeric per-task --ttl wins) is a
dead worker's, and every read says so: list/status/brief tag it EXPIRED, show
adds claim_expired, and ready/next offer the task again — next --claim fences
the old worker by minting a new lease. Reads never write; reclaim (and
plan-run's pre-flight call to it) is what frees the stored row.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

# Parse: first non-option token is the command.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --file) [ "$#" -ge 2 ] || fail "--file requires path"; PLAN_FILE="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires value"; ID="$2"; shift 2 ;;
    --title) [ "$#" -ge 2 ] || fail "--title requires text"; TITLE="$2"; shift 2 ;;
    --goal) [ "$#" -ge 2 ] || fail "--goal requires text"; GOAL="$2"; shift 2 ;;
    --provider) [ "$#" -ge 2 ] || fail "--provider requires name"; PROVIDER="$2"; shift 2 ;;
    --ttl) [ "$#" -ge 2 ] || fail "--ttl requires text"; TTL="$2"; shift 2 ;;
    --reason) [ "$#" -ge 2 ] || fail "--reason requires text"; REASON="$2"; shift 2 ;;
    --artifact) [ "$#" -ge 2 ] || fail "--artifact requires path"; ARTIFACT="$2"; shift 2 ;;
    --patch) [ "$#" -ge 2 ] || fail "--patch requires path"; PATCH="$2"; shift 2 ;;
    --executor-id) [ "$#" -ge 2 ] || fail "--executor-id requires id"; EXECUTOR_ID="$2"; shift 2 ;;
    --executor-soul-sha256) [ "$#" -ge 2 ] || fail "--executor-soul-sha256 requires hash"; EXECUTOR_SOUL_SHA256="$2"; shift 2 ;;
    --expected-review-patch)
      [ "$#" -ge 2 ] || fail "--expected-review-patch requires path"
      EXPECTED_REVIEW_PATCH="$2"; EXPECTED_REVIEW_PATCH_SET=1; shift 2 ;;
    --expected-review-patch-sha256)
      [ "$#" -ge 2 ] || fail "--expected-review-patch-sha256 requires hash"
      EXPECTED_REVIEW_PATCH_SHA256="$2"; EXPECTED_REVIEW_PATCH_SHA256_SET=1; shift 2 ;;
    --expected-review-verify)
      [ "$#" -ge 2 ] || fail "--expected-review-verify requires command"
      EXPECTED_REVIEW_VERIFY="$2"; EXPECTED_REVIEW_VERIFY_SET=1; shift 2 ;;
    --expected-review-executor-id)
      [ "$#" -ge 2 ] || fail "--expected-review-executor-id requires value"
      EXPECTED_REVIEW_EXECUTOR_ID="$2"; EXPECTED_REVIEW_EXECUTOR_ID_SET=1; shift 2 ;;
    --expected-review-executor-soul-sha256)
      [ "$#" -ge 2 ] || fail "--expected-review-executor-soul-sha256 requires value"
      EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256="$2"; EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256_SET=1; shift 2 ;;
    --expected-review-lease-id)
      [ "$#" -ge 2 ] || fail "--expected-review-lease-id requires value"
      EXPECTED_REVIEW_LEASE_ID="$2"; EXPECTED_REVIEW_LEASE_ID_SET=1; shift 2 ;;
    --expected-landing-receipt-sha256)
      [ "$#" -ge 2 ] || fail "--expected-landing-receipt-sha256 requires hash"
      EXPECTED_LANDING_RECEIPT_SHA256="$2"
      EXPECTED_LANDING_RECEIPT_SHA256_SET=1
      shift 2
      ;;
    --depends) [ "$#" -ge 2 ] || fail "--depends requires list"; DEPENDS="$2"; shift 2 ;;
    --allowed) [ "$#" -ge 2 ] || fail "--allowed requires list"; ALLOWED="$2"; shift 2 ;;
    --role) [ "$#" -ge 2 ] || fail "--role requires a name"; ROLE="$2"; shift 2 ;;
    --forbidden) [ "$#" -ge 2 ] || fail "--forbidden requires list"; FORBIDDEN="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires command"; VERIFY="$2"; shift 2 ;;
    --accept) [ "$#" -ge 2 ] || fail "--accept requires command"; ACCEPT="$2"; shift 2 ;;
    --accept-files) [ "$#" -ge 2 ] || fail "--accept-files requires a value"; ACCEPT_FILES="$2"; shift 2 ;;
    --owner-id) [ "$#" -ge 2 ] || fail "--owner-id requires a value"; OWNER_ID="$2"; shift 2 ;;
    --markers-dir) [ "$#" -ge 2 ] || fail "--markers-dir requires a path"; MARKERS_DIR="$2"; shift 2 ;;
    --expected-state) [ "$#" -ge 2 ] || fail "--expected-state requires a value"; EXPECTED_STATE="$2"; shift 2 ;;
    --check) CHECK_ONLY=1; shift ;;
    --proposal) [ "$#" -ge 2 ] || fail "--proposal requires a file"; PROPOSAL="$2"; shift 2 ;;
    --expected-proposal-sha256)
      [ "$#" -ge 2 ] || fail "--expected-proposal-sha256 requires a value"
      EXPECTED_PROPOSAL_SHA256="$2"; shift 2 ;;
    --expected-plan-sha256)
      [ "$#" -ge 2 ] || fail "--expected-plan-sha256 requires a value"
      EXPECTED_PLAN_SHA256="$2"; shift 2 ;;
    --allowed-envelope)
      [ "$#" -ge 2 ] || fail "--allowed-envelope requires paths"
      ALLOWED_ENVELOPE="$2"; shift 2 ;;
    --max-tasks)
      [ "$#" -ge 2 ] || fail "--max-tasks requires a count"
      MAX_TASKS="$2"; shift 2 ;;
    --state) [ "$#" -ge 2 ] || fail "--state requires value"; STATE_FILTER="$2"; shift 2 ;;
    --lease-id) [ "$#" -ge 2 ] || fail "--lease-id requires value"; LEASE_ID="$2"; shift 2 ;;
    --claim) CLAIM=1; shift ;;
    --refreeze-acceptance) REFREEZE_ACCEPTANCE=1; shift ;;
    --include-running) INCLUDE_RUNNING=1; shift ;;
    --include-review) INCLUDE_REVIEW=1; shift ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    init|ensure-lineage|apply-proposal|add|claim|start|touch|review|repair|land|finish|block|release|recover-lease|recover-owner|reclaim|reopen|show|evidence-snapshot|list|ready|status|next|brief|accept|lint-verify)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"; ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
[ "$CHECK_ONLY" = 0 ] || [ "$ACTION" = recover-lease ] ||
  fail "--check is valid only with recover-lease"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
REPO="$(cd "$REPO" && pwd -P)" || fail "cannot resolve the physical repository"
PLAN_FILE="${PLAN_FILE:-$REPO/.oms/plan/tasks.json}"
if [ -n "$PROVIDER" ]; then
  PROVIDER="$(oms_normalize_provider "$PROVIDER")" ||
    fail "unknown provider: use codex, claude, or antigravity (agy)"
fi

# Git Bash paths are valid for the shell but environment variables are not
# rewritten when it launches native Windows Python. Resolve shell aliases
# first, then use the mixed drive spelling understood by both runtimes.
python_path_for_host() {  # PATH
  local value="$1" parent base physical_parent
  parent="$(dirname "$value")"
  base="$(basename "$value")"
  if physical_parent="$(cd "$parent" 2>/dev/null && pwd -P)"; then
    value="$physical_parent/$base"
  fi
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*)
      command -v cygpath >/dev/null 2>&1 || return 2
      value="$(cygpath -m "$value" | tr -d '\r')" || return $?
      ;;
  esac
  printf '%s\n' "$value"
}

PY_REPO="$(python_path_for_host "$REPO")" || fail "cannot normalize repository path for Python"
PY_PLAN_FILE="$(python_path_for_host "$PLAN_FILE")" || fail "cannot normalize plan path for Python"
PY_MARKERS_DIR=""
if [ -n "$MARKERS_DIR" ]; then
  PY_MARKERS_DIR="$(python_path_for_host "$MARKERS_DIR")" ||
    fail "cannot normalize worker marker path for Python"
fi

# Read-only diagnostics against the same module the admission gate loads, so
# a spec author can lint an acceptance or verify before a planner copies it
# into a task. Exit 2 with typed floor_incompatible_verifier lines on a hit.
if [ "$ACTION" = lint-verify ]; then
  [ -n "$VERIFY" ] || fail "lint-verify requires --verify CMD"
  [ -n "$ALLOWED" ] || fail "lint-verify requires --allowed \"p1,p2\""
  exec python3 "$ROOT/scripts/lib/verify-floor-lint.py" \
    --verify "$VERIFY" --allowed "$ALLOWED"
fi

case "$ACTION" in
  init|apply-proposal|add)
    [ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
      fail "$ACTION is parent-only; a harness child cannot change plan topology"
    ;;
  ensure-lineage)
    [ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
      fail "ensure-lineage is parent-only; a harness child cannot mint plan lineage"
    ;;
  recover-owner)
    [ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
      fail "recover-owner is parent-only; a harness child cannot recover autopilot authority"
    ;;
esac

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# One clock for both halves of claim expiry: the read paths present a claim
# past this TTL as expired, and reclaim's default frees exactly those rows.
# The deployed baseline is not measured yet, so it sits behind an override.
CLAIM_TTL="${OMS_PLAN_CLAIM_TTL:-3600}"
case "$CLAIM_TTL" in *[!0-9]*|"") CLAIM_TTL=3600 ;; esac

# The plan file is durable state; a credential in the goal or acceptance
# command would persist verbatim (same contract as fail-ledger's cmd field).
if [ -n "$GOAL$ACCEPT" ]; then
  scan="$(mktemp)" || fail "mktemp failed"
  printf '%s\n%s\n' "$GOAL" "$ACCEPT" > "$scan"
  if agent_memory_file_has_secret_content "$scan"; then
    rm -f "$scan"
    fail "goal/acceptance text looks sensitive; pass credentials via environment, not command text"
  fi
  rm -f "$scan"
fi

if [ "$ACTION" = apply-proposal ]; then
  [ -n "$PROPOSAL" ] || fail "apply-proposal requires --proposal"
  [ -f "$PROPOSAL" ] && [ ! -L "$PROPOSAL" ] ||
    fail "proposal must be a regular non-symlink file"
  case "$EXPECTED_PROPOSAL_SHA256" in
    *[!0-9a-f]*|"") fail "apply-proposal requires a lowercase --expected-proposal-sha256" ;;
  esac
  [ "${#EXPECTED_PROPOSAL_SHA256}" -eq 64 ] ||
    fail "--expected-proposal-sha256 must be a lowercase SHA-256"
  case "$EXPECTED_PLAN_SHA256" in
    absent) ;;
    *[!0-9a-f]*|"") fail "apply-proposal requires --expected-plan-sha256 absent|SHA256" ;;
    *)
      [ "${#EXPECTED_PLAN_SHA256}" -eq 64 ] ||
        fail "--expected-plan-sha256 must be absent or a lowercase SHA-256"
      ;;
  esac
  [ -n "$ALLOWED_ENVELOPE" ] || fail "apply-proposal requires --allowed-envelope"
  case "${MAX_TASKS:-12}" in
    *[!0-9]*|"") fail "--max-tasks must be an integer" ;;
  esac
  [ "${MAX_TASKS:-12}" -ge 1 ] && [ "${MAX_TASKS:-12}" -le 12 ] ||
    fail "--max-tasks must be 1..12"
  if agent_memory_file_has_secret_content "$PROPOSAL"; then
    fail "proposal looks sensitive; task contracts must not persist credentials"
  fi
fi

# All mutations and queries run in one python process: load -> act -> (write|print).
# The whole load/decide/save section runs under a file lock so concurrent
# `next --claim` from different agents cannot both win the same task (the write
# itself is atomic, but the read-decide-write critical section is not).
export OMS_PLAN_FILE="$PY_PLAN_FILE" OMS_ACTION="$ACTION" OMS_TS="$ts" \
  OMS_REPO="$PY_REPO" \
  OMS_ID="$ID" OMS_TITLE="$TITLE" OMS_GOAL="$GOAL" OMS_PROVIDER="$PROVIDER" \
  OMS_TTL="$TTL" OMS_REASON="$REASON" OMS_ARTIFACT="$ARTIFACT" OMS_PATCH="$PATCH" \
  OMS_REFREEZE_ACCEPTANCE="$REFREEZE_ACCEPTANCE" \
  OMS_EXECUTOR_ID="$EXECUTOR_ID" OMS_EXECUTOR_SOUL_SHA256="$EXECUTOR_SOUL_SHA256" \
  OMS_EXPECTED_REVIEW_PATCH="$EXPECTED_REVIEW_PATCH" \
  OMS_EXPECTED_REVIEW_PATCH_SHA256="$EXPECTED_REVIEW_PATCH_SHA256" \
  OMS_EXPECTED_REVIEW_VERIFY="$EXPECTED_REVIEW_VERIFY" \
  OMS_EXPECTED_REVIEW_EXECUTOR_ID="$EXPECTED_REVIEW_EXECUTOR_ID" \
  OMS_EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256="$EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256" \
  OMS_EXPECTED_REVIEW_LEASE_ID="$EXPECTED_REVIEW_LEASE_ID" \
  OMS_EXPECTED_LANDING_RECEIPT_SHA256="$EXPECTED_LANDING_RECEIPT_SHA256" \
  OMS_EXPECTED_REVIEW_PATCH_SET="$EXPECTED_REVIEW_PATCH_SET" \
  OMS_EXPECTED_REVIEW_PATCH_SHA256_SET="$EXPECTED_REVIEW_PATCH_SHA256_SET" \
  OMS_EXPECTED_REVIEW_VERIFY_SET="$EXPECTED_REVIEW_VERIFY_SET" \
  OMS_EXPECTED_REVIEW_EXECUTOR_ID_SET="$EXPECTED_REVIEW_EXECUTOR_ID_SET" \
  OMS_EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256_SET="$EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256_SET" \
  OMS_EXPECTED_REVIEW_LEASE_ID_SET="$EXPECTED_REVIEW_LEASE_ID_SET" \
  OMS_EXPECTED_LANDING_RECEIPT_SHA256_SET="$EXPECTED_LANDING_RECEIPT_SHA256_SET" \
  OMS_DEPENDS="$DEPENDS" OMS_ALLOWED="$ALLOWED" OMS_FORBIDDEN="$FORBIDDEN" \
  OMS_VERIFY="$VERIFY" OMS_ACCEPT="$ACCEPT" OMS_ROLE="$ROLE" OMS_STATE_FILTER="$STATE_FILTER" OMS_CLAIM="$CLAIM" \
  OMS_INCLUDE_RUNNING="$INCLUDE_RUNNING" OMS_INCLUDE_REVIEW="$INCLUDE_REVIEW" \
  OMS_LEASE_ID="$LEASE_ID" OMS_AS_JSON="$AS_JSON" OMS_CLAIM_TTL="$CLAIM_TTL" \
  OMS_EXPECTED_PROPOSAL_SHA256="$EXPECTED_PROPOSAL_SHA256" \
  OMS_EXPECTED_PLAN_SHA256="$EXPECTED_PLAN_SHA256" \
  OMS_ALLOWED_ENVELOPE="$ALLOWED_ENVELOPE" OMS_MAX_TASKS="${MAX_TASKS:-12}" \
  OMS_ACCEPT_FILES="$ACCEPT_FILES" OMS_EXPECTED_STATE="$EXPECTED_STATE" \
  OMS_CHECK_ONLY="$CHECK_ONLY"
export OMS_AUTOPILOT_OWNER_ID="$OWNER_ID" OMS_PLAN_MARKERS_DIR="$PY_MARKERS_DIR"

plan_run() {
python3 - "$PROPOSAL" "$ROOT/scripts/lib/plan-receipt.py" \
  "$ROOT/scripts/lib/verify-floor-lint.py" \
  "$ROOT/scripts/lib/process_liveness.py" <<'PY'
import datetime, hashlib, json, os, re, runpy, secrets, stat, subprocess, sys, tempfile, unicodedata

SCHEMA = 3
path = os.environ["OMS_PLAN_FILE"]
act = os.environ["OMS_ACTION"]
ts = os.environ["OMS_TS"]
proposal_path = sys.argv[1]
landing_receipt_digest = runpy.run_path(sys.argv[2])["digest"]
process_liveness = runpy.run_path(sys.argv[4])
process_pid_alive = process_liveness["pid_alive"]
persisted_native_pid_is_proven = process_liveness[
    "persisted_native_pid_is_proven"
]
def env(k): return os.environ.get(k, "")

STATES = {"ready", "claimed", "running", "review", "landing", "blocked", "done"}
ID_RE = re.compile(r"^[A-Za-z0-9._-]+$")
PLAN_ID_RE = re.compile(r"^plan_[0-9a-f]{32}$")
OWNER_RE = re.compile(r"^owner_[0-9a-f]{32}$")

def die(msg):
    sys.stderr.write("error: %s\n" % msg); sys.exit(2)

def load():
    if not os.path.exists(path):
        return {"schema": SCHEMA, "goal": "", "accept": "", "tasks": {}}
    with open(path, encoding="utf-8") as fh:
        d = json.load(fh)
    d.setdefault("tasks", {})
    d.setdefault("accept", "")   # schema 2 plans predate the acceptance contract
    d["schema"] = SCHEMA
    if "plan_id" in d and not PLAN_ID_RE.fullmatch(str(d.get("plan_id", ""))):
        die("plan_id is malformed; refusing to replace immutable lineage")
    for task in d["tasks"].values():
        task.setdefault("lease_epoch", 0)
        task.setdefault("lease_id", "")
        task.setdefault("repair_count", 0)
        task.setdefault("repair_artifact", "")
        task.setdefault("executor_id", "")
        task.setdefault("executor_soul_sha256", "")
        task.setdefault("autopilot_owner_id", "")
    return d

def ensure_plan_id(d):
    value = d.get("plan_id")
    if value is None:
        value = "plan_" + secrets.token_hex(16)
        d["plan_id"] = value
    if not isinstance(value, str) or not PLAN_ID_RE.fullmatch(value):
        die("plan_id is malformed; refusing to replace immutable lineage")
    return value

def save(d):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(d, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, path)   # atomic
    except Exception:
        os.unlink(tmp); raise

def split_list(s):
    return [x.strip() for x in re.split(r"[,\s]+", s) if x.strip()]

def read_regular_bytes(filename, label, maximum):
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    flags |= getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(filename, flags)
    except OSError as exc:
        die("cannot open %s: %s" % (label, exc))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            die("%s must be a regular file" % label)
        if info.st_size > maximum:
            die("%s exceeds %d bytes" % (label, maximum))
        chunks = []
        total = 0
        while True:
            chunk = os.read(fd, min(1024 * 1024, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                die("%s exceeds %d bytes" % (label, maximum))
        return b"".join(chunks)
    finally:
        os.close(fd)

def reject_controls(value, label):
    if any(unicodedata.category(ch) in ("Cc", "Cf", "Cs") for ch in value):
        die("%s contains a control or format character" % label)

# Single source of truth for floor-incompatible content reads: the same
# module `agent-plan lint-verify` runs standalone, loaded here the way the
# landing receipt already is, so the admission gate and the lint front door
# can never drift apart.
floor_incompatible_reads = runpy.run_path(sys.argv[3])["floor_incompatible_reads"]

def clean_rel(value, label):
    if not isinstance(value, str):
        die("%s must be a string" % label)
    reject_controls(value, label)
    value = value.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    value = value.rstrip("/") or "."
    if (value.startswith("/") or re.match(r"^[A-Za-z]:", value) or
            (value != "." and any(part in ("", ".", "..") for part in value.split("/")))):
        die("%s must be a normalized repo-relative path" % label)
    return value

def require_id():
    i = env("OMS_ID")
    if not i: die("--id is required for %s" % act)
    if not ID_RE.fullmatch(i): die("--id must match [A-Za-z0-9._-]+")
    return i

def deps_done(d, t):
    return all(d["tasks"].get(x, {}).get("state") == "done" for x in t.get("depends", []))

def issue_lease(t):
    t["lease_epoch"] = int(t.get("lease_epoch", 0)) + 1
    t["lease_id"] = "lease_" + secrets.token_hex(16)

def require_current_lease(t):
    supplied = env("OMS_LEASE_ID")
    current = t.get("lease_id", "")
    if supplied and supplied != current:
        die("task %s lease mismatch; worker is stale" % t["id"])
    if env("OMS_HARNESS_CHILD") == "1" and current and not supplied:
        die("task %s requires --lease-id for harness child mutation" % t["id"])

def owner_id():
    value = env("OMS_AUTOPILOT_OWNER_ID")
    if value and not OWNER_RE.fullmatch(value):
        die("autopilot owner id is invalid")
    return value

def clear_claim(t):
    t.update(state="ready", provider="", ttl="", claimed_at="", reason="",
             lease_id="", autopilot_owner_id="")

def same_absolute_path(left, right):
    return os.path.normcase(os.path.abspath(left)) == os.path.normcase(os.path.abspath(right))

def worker_marker_dir():
    repo_root = os.path.realpath(env("OMS_REPO"))
    expected = os.path.join(repo_root, ".oms", "delegations")
    supplied = env("OMS_PLAN_MARKERS_DIR") or expected
    marker_dir = os.path.abspath(supplied)
    if not same_absolute_path(marker_dir, expected):
        die("worker marker directory must be the repo-local .oms/delegations directory")
    return marker_dir, expected

def load_worker_markers():
    marker_dir, expected = worker_marker_dir()
    markers = []
    if not os.path.lexists(marker_dir):
        return markers
    try:
        marker_dir_info = os.lstat(marker_dir)
    except OSError as exc:
        die("cannot inspect worker marker directory: %s" % exc)
    if (not stat.S_ISDIR(marker_dir_info.st_mode) or
            not same_absolute_path(os.path.realpath(marker_dir), expected)):
        die("worker marker directory must be a real repo-local directory")
    entries = []
    with os.scandir(marker_dir) as iterator:
        for entry in iterator:
            if len(entries) >= 4096:
                die("worker marker directory exceeds 4096 entries")
            entries.append(entry)
    for entry in entries:
        if not entry.name.endswith(".json"):
            continue
        try:
            # Windows scandir metadata can carry a directory-enumeration inode
            # that is not comparable with the file-id returned by fstat().
            # A pathname lstat and the opened handle share the stable identity
            # contract used by the other bounded readers.
            before = os.lstat(entry.path)
        except OSError as exc:
            die("cannot inspect worker marker %s: %s" % (entry.name, exc))
        if not stat.S_ISREG(before.st_mode) or before.st_size > 64 * 1024:
            die("worker marker %s is not a bounded regular file" % entry.name)
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
        try:
            descriptor = os.open(entry.path, flags)
        except OSError as exc:
            die("cannot open worker marker %s safely: %s" % (entry.name, exc))
        try:
            opened = os.fstat(descriptor)
            if (not stat.S_ISREG(opened.st_mode) or
                    (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
                die("worker marker %s changed while opening" % entry.name)
            payload = os.read(descriptor, 64 * 1024 + 1)
            if len(payload) > 64 * 1024 or os.read(descriptor, 1):
                die("worker marker %s exceeds 64 KiB" % entry.name)
        finally:
            os.close(descriptor)
        try:
            marker = json.loads(payload.decode("utf-8"))
        except (UnicodeError, ValueError):
            die("worker marker %s is malformed" % entry.name)
        if not isinstance(marker, dict):
            die("worker marker %s is malformed" % entry.name)
        markers.append(marker)
    return markers

def marker_pid_alive(marker):
    if not persisted_native_pid_is_proven(
        marker.get("native_pid"), marker.get("native_pid_source")
    ):
        # An unproven persisted Win32 pid cannot authorize recovery. Typed
        # recovery paths report it as unproven before reaching this fallback.
        return True
    return process_pid_alive(
        marker.get("pid"), native_pid=marker.get("native_pid")
    )

def exact_lease_markers(markers, task_id, lease):
    return [marker for marker in markers
            if marker.get("task_id") == task_id and marker.get("lease_id") == lease]

def worker_marker_is_typed(marker):
    schema = marker.get("schema")
    if (isinstance(schema, bool) or not isinstance(schema, int)
            or schema not in {1, 2, 3, 4}):
        return False
    marker_id = marker.get("id")
    if (not isinstance(marker_id, str) or not ID_RE.fullmatch(marker_id)
            or marker_id in {".", ".."}):
        return False
    pid = marker.get("pid")
    if (isinstance(pid, bool) or not isinstance(pid, int)
            or pid <= 0 or pid > 0x7FFFFFFF):
        return False
    native_pid = marker.get("native_pid")
    if schema == 4 and "native_pid" not in marker:
        return False
    if "native_pid" in marker and (
            isinstance(native_pid, bool) or not isinstance(native_pid, int)
            or native_pid <= 0 or native_pid > 0xFFFFFFFF):
        return False
    if not persisted_native_pid_is_proven(
            native_pid, marker.get("native_pid_source")):
        return False
    for key in ("task_id", "lease_id", "executor_id"):
        value = marker.get(key, "")
        if (not isinstance(value, str)
                or (value and not ID_RE.fullmatch(value))):
            return False
    if marker.get("executor_id", "") in {".", ".."}:
        return False
    marker_owner = marker.get("autopilot_owner_id", "")
    if (not isinstance(marker_owner, str)
            or (marker_owner and not OWNER_RE.fullmatch(marker_owner))):
        return False
    return True

CLAIM_TTL = int(os.environ.get("OMS_CLAIM_TTL") or 3600)

def parse_ts(s):
    try:
        return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
    except Exception:
        return None

now_dt = parse_ts(ts)

def claim_anchor(t):
    """When this task's TTL clock last restarted. touch refreshes claimed_at,
    so the clock runs from the last heartbeat, not from the claim. review ages
    from when it entered review instead: it waits on a reviewer, not a worker."""
    if t.get("state") == "review":
        return parse_ts(t.get("updated", ""))
    return parse_ts(t.get("claimed_at", "")) or parse_ts(t.get("updated", ""))

def claim_ttl_for(t, default_ttl=CLAIM_TTL):
    v = t.get("ttl", "")
    if isinstance(v, str) and v.isdigit():
        return int(v)
    return default_ttl

def claim_age(t):
    anchor = claim_anchor(t)
    if anchor is None or now_dt is None:
        return None
    return int((now_dt - anchor).total_seconds())

def claim_expired(t):
    """Read-time view of a claim: past its TTL it belongs to a worker nobody
    has heard from, so it is not a live hold on the task. Reads present that
    and change nothing (the same way agent-thread treats an expired CURRENT
    pointer); reclaim is what rewrites the row."""
    if t.get("state") != "claimed":
        return False
    age = claim_age(t)
    if age is None:
        return False
    return age >= claim_ttl_for(t)

def actionable(d, t):
    """Claimable right now: ready, or held by an expired claim."""
    return deps_done(d, t) and (t["state"] == "ready" or claim_expired(t))

def expiry_note(t):
    return "claim EXPIRED (age %ss >= ttl %ss, was @%s)" % (
        claim_age(t), claim_ttl_for(t), t.get("provider", "") or "?")

def brief_text(t):
    state = t["state"]
    if claim_expired(t):
        state = "%s [%s; claimable]" % (state, expiry_note(t))
    lines = ["# Task %s: %s" % (t["id"], t["title"]), "state: %s" % state]
    lines.append("depends: %s" % (", ".join(t.get("depends", [])) or "(none)"))
    lines.append("allowed_paths: %s" % (", ".join(t.get("allowed_paths", [])) or "(unrestricted)"))
    if t.get("forbidden_paths"):
        lines.append("forbidden_paths: %s" % ", ".join(t["forbidden_paths"]))
    lines.append("verify: %s" % (t.get("verify") or "(none)"))
    if t.get("role"):
        lines.append("role: %s" % t["role"])
    return "\n".join(lines)

d = load()
tasks = d["tasks"]

if act == "init":
    d = {"schema": SCHEMA, "plan_id": "plan_" + secrets.token_hex(16),
         "goal": env("OMS_GOAL"), "accept": env("OMS_ACCEPT"), "tasks": {}}
    save(d); print("plan: initialized (%s)" % path); sys.exit(0)

if act == "ensure-lineage":
    if not os.path.exists(path):
        die("no plan at %s; initialize it before ensuring lineage" % path)
    before = d.get("plan_id")
    value = ensure_plan_id(d)
    if before is None:
        save(d)
        print("plan: lineage initialized (%s)" % value)
    else:
        print("plan: lineage already initialized (%s)" % value)
    sys.exit(0)

if act == "apply-proposal":
    def sha256_file(filename):
        digest = hashlib.sha256()
        with open(filename, "rb") as handle:
            while True:
                chunk = handle.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        return digest.hexdigest()

    envelope = [clean_rel(item, "allowed envelope")
                for item in split_list(env("OMS_ALLOWED_ENVELOPE"))]
    if not envelope:
        die("allowed envelope is empty")

    def inside_envelope(value):
        candidate = clean_rel(value, "proposal allowed path")
        return any(root == "." or candidate == root or candidate.startswith(root + "/")
                   for root in envelope)

    proposal_bytes = read_regular_bytes(proposal_path, "proposal", 1024 * 1024)
    expected_proposal = env("OMS_EXPECTED_PROPOSAL_SHA256")
    if hashlib.sha256(proposal_bytes).hexdigest() != expected_proposal:
        die("proposal bytes changed after review")
    try:
        proposal = json.loads(proposal_bytes.decode("utf-8"))
    except (UnicodeError, ValueError) as exc:
        die("cannot read proposal: %s" % exc)
    if not isinstance(proposal, dict) or proposal.get("schema") != 1:
        die("proposal has an unsupported schema")
    if proposal.get("kind") != "agent-plan-proposal":
        die("proposal kind is not agent-plan-proposal")
    top_keys = {
        "schema", "kind", "spec_sha256", "plan_sha256", "base_sha",
        "id_prefix", "allowed_envelope", "acceptance_files", "tasks",
    }
    if set(proposal) != top_keys:
        die("proposal fields do not match the exact reviewed schema")

    proposal_spec = proposal.get("spec_sha256")
    proposal_plan = proposal.get("plan_sha256")
    proposal_base = proposal.get("base_sha")
    proposal_prefix = proposal.get("id_prefix")
    proposal_envelope = proposal.get("allowed_envelope")
    proposal_acceptance_files = proposal.get("acceptance_files")
    if (not isinstance(proposal_spec, str) or len(proposal_spec) != 64 or
            any(ch not in "0123456789abcdef" for ch in proposal_spec)):
        die("proposal spec_sha256 is invalid")
    if (not isinstance(proposal_base, str) or len(proposal_base) not in (40, 64) or
            any(ch not in "0123456789abcdef" for ch in proposal_base)):
        die("proposal base_sha is invalid")
    if proposal_plan != env("OMS_EXPECTED_PLAN_SHA256"):
        die("proposal plan digest does not match the apply CAS")
    if not isinstance(proposal_prefix, str) or (proposal_prefix and not ID_RE.fullmatch(proposal_prefix)):
        die("proposal id prefix is invalid")
    if not isinstance(proposal_envelope, list) or any(not isinstance(x, str) for x in proposal_envelope):
        die("proposal allowed envelope is invalid")
    reviewed_envelope = sorted(set(
        clean_rel(item, "proposal allowed envelope") for item in proposal_envelope
    )) or ["."]
    if reviewed_envelope != sorted(set(envelope)):
        die("proposal allowed envelope does not match the apply boundary")

    if (not isinstance(proposal_acceptance_files, list) or
            any(not isinstance(item, str) for item in proposal_acceptance_files)):
        die("proposal acceptance_files must be a list")
    if len(proposal_acceptance_files) > 64:
        die("proposal acceptance_files must contain at most 64 paths")
    if any(len(item.encode("utf-8")) > 240 for item in proposal_acceptance_files):
        die("proposal acceptance file paths must be at most 240 UTF-8 bytes")
    reviewed_acceptance_files = sorted(set(
        clean_rel(item, "proposal acceptance file")
        for item in proposal_acceptance_files
    ))
    supplied_acceptance_files = sorted(set(
        clean_rel(item, "--accept-files")
        for item in env("OMS_ACCEPT_FILES").split(",") if item.strip()
    ))
    if proposal_acceptance_files != reviewed_acceptance_files:
        die("proposal acceptance_files must be sorted and unique")
    if supplied_acceptance_files != reviewed_acceptance_files:
        die("proposal acceptance_files do not match --accept-files")

    repo_root = os.path.realpath(env("OMS_REPO"))
    acceptance_manifest = []
    for item in reviewed_acceptance_files:
        if item == ".":
            die("acceptance file must name a regular file, not the repository root")
        filename = os.path.join(repo_root, *item.split("/"))
        if os.path.realpath(filename) != filename:
            die("acceptance file %s must not cross a symlink boundary" % item)
        try:
            info = os.lstat(filename)
        except OSError as exc:
            die("cannot inspect acceptance file %s: %s" % (item, exc))
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            die("acceptance file %s must be a regular non-symlink file" % item)
        value = hashlib.sha256(read_regular_bytes(
            filename, "acceptance file %s" % item, 8 * 1024 * 1024
        )).hexdigest()
        acceptance_manifest.append({"path": item, "sha256": value})

    spec_path = os.path.join(env("OMS_REPO"), "PROJECT.md")
    spec_bytes = read_regular_bytes(spec_path, "PROJECT.md", 1024 * 1024)
    if hashlib.sha256(spec_bytes).hexdigest() != proposal_spec:
        die("PROJECT.md changed after proposal review")
    try:
        spec_text = spec_bytes.decode("utf-8")
    except UnicodeError:
        die("PROJECT.md must be UTF-8")
    state_match = re.search(r"(?m)^- State:[ \t]*([^\r\n]+?)[ \t]*\r?$", spec_text)
    if not state_match or state_match.group(1) not in ("confirmed", "active"):
        die("PROJECT.md must be confirmed before plan topology is applied")
    raw_tasks = proposal.get("tasks")
    max_tasks = int(env("OMS_MAX_TASKS") or "12")
    if not isinstance(raw_tasks, list) or not raw_tasks or len(raw_tasks) > max_tasks:
        die("proposal tasks must contain 1..%d entries" % max_tasks)

    prepared = []
    seen = []
    for index, raw in enumerate(raw_tasks):
        if not isinstance(raw, dict):
            die("proposal task %d must be an object" % index)
        if set(raw) != {"id", "title", "allowed", "verify", "depends"}:
            die("proposal task %d fields do not match the exact reviewed schema" % index)
        task_id = raw.get("id")
        title = raw.get("title")
        verify = raw.get("verify")
        if not isinstance(task_id, str) or not ID_RE.fullmatch(task_id):
            die("proposal task %d has an invalid id" % index)
        if task_id in seen:
            die("proposal task ids must be unique")
        if not isinstance(title, str) or not title.strip():
            die("proposal task %s has no title" % task_id)
        reject_controls(title, "proposal task %s title" % task_id)
        if not isinstance(verify, str) or not verify.strip():
            die("proposal task %s has no verify command" % task_id)
        reject_controls(verify, "proposal task %s verify" % task_id)
        if proposal_prefix and not task_id.startswith(proposal_prefix):
            die("proposal task %s does not match id prefix %s" % (task_id, proposal_prefix))
        allowed = raw.get("allowed")
        if not isinstance(allowed, list) or not allowed:
            die("proposal task %s has no allowed paths" % task_id)
        cleaned_allowed = [clean_rel(item, "proposal task %s allowed path" % task_id)
                           for item in allowed]
        if not all(inside_envelope(item) for item in cleaned_allowed):
            die("proposal task %s widens the allowed path envelope" % task_id)
        floor_hits = floor_incompatible_reads(verify, cleaned_allowed)
        if floor_hits:
            die("floor_incompatible_verifier: proposal task %s verify reads %s"
                " (via %s), a file the task itself modifies — the base floor"
                " restores that file from HEAD, so this check can never pass;"
                " verify by executing the restored file (run the suite), not"
                " by reading its content" % (task_id, floor_hits[0][0], floor_hits[0][1]))
        forbidden = raw.get("forbidden") or []
        if not isinstance(forbidden, list):
            die("proposal task %s forbidden paths must be a list" % task_id)
        cleaned_forbidden = [clean_rel(item, "proposal task %s forbidden path" % task_id)
                             for item in forbidden]
        depends = raw.get("depends") or []
        if (not isinstance(depends, list) or any(not isinstance(dep, str) for dep in depends)
                or any(not ID_RE.fullmatch(dep) for dep in depends)
                or len(depends) != len(set(depends))):
            die("proposal task %s dependencies must be ids" % task_id)
        for dep in depends:
            if dep in seen:
                continue
            if dep not in tasks:
                die("proposal task %s depends on unknown/later task %s" % (task_id, dep))
            if tasks[dep].get("state") != "done":
                die("proposal task %s depends on unfinished existing task %s" % (task_id, dep))
        role = raw.get("role") or ""
        if not isinstance(role, str):
            die("proposal task %s role must be a string" % task_id)
        reject_controls(role, "proposal task %s role" % task_id)
        prepared.append({
            "id": task_id, "title": title.strip(), "depends": list(depends),
            "allowed_paths": cleaned_allowed, "forbidden_paths": cleaned_forbidden,
            "verify": verify, "role": role,
        })
        seen.append(task_id)

    contract = d.get("project_contract")
    if contract is not None:
        if (not isinstance(contract, dict) or contract.get("schema") != 1 or
                contract.get("spec_sha256") != proposal_spec or
                contract.get("allowed_envelope") != reviewed_envelope or
                contract.get("acceptance_files", []) != reviewed_acceptance_files or
                contract.get("acceptance_manifest", []) != acceptance_manifest):
            die("existing plan project contract does not match the reviewed proposal")

    immutable = ("id", "title", "depends", "allowed_paths", "forbidden_paths", "verify", "role")
    already = [item["id"] in tasks for item in prepared]
    if any(already):
        if not all(already):
            die("proposal is partially present; refusing a non-atomic recovery")
        for item in prepared:
            current = tasks[item["id"]]
            if any(current.get(name, "" if name in ("verify", "role") else []) != item[name]
                   for name in immutable):
                die("existing task %s does not match the reviewed proposal" % item["id"])
        if env("OMS_GOAL") and d.get("goal", "") != env("OMS_GOAL"):
            die("existing plan goal does not match proposal replay")
        if env("OMS_ACCEPT") and d.get("accept", "") != env("OMS_ACCEPT"):
            die("existing plan acceptance does not match proposal replay")
        # A HEAD-relaxed replay is read-only and is allowed only after the
        # reviewed project contract was already persisted by the first apply.
        if contract is not None:
            print("plan: proposal already applied (%s)" % ",".join(seen))
            sys.exit(0)

    try:
        current_head = subprocess.check_output(
            ["git", "-C", env("OMS_REPO"), "rev-parse", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode("ascii").strip()
    except (OSError, subprocess.CalledProcessError, UnicodeError):
        die("cannot resolve the repository HEAD")
    if current_head != proposal_base:
        die("repository HEAD changed after proposal review")

    expected_plan = env("OMS_EXPECTED_PLAN_SHA256")
    if os.path.exists(path):
        actual_plan = sha256_file(path)
        if expected_plan != actual_plan:
            die("plan changed after proposal review")
        if env("OMS_GOAL") and d.get("goal", "") != env("OMS_GOAL"):
            die("existing plan goal changed after proposal review")
        if env("OMS_ACCEPT") and d.get("accept", "") != env("OMS_ACCEPT"):
            die("existing plan acceptance changed after proposal review")
    else:
        if expected_plan != "absent":
            die("proposal expected an existing plan")
        if not env("OMS_GOAL") or not env("OMS_ACCEPT"):
            die("initial proposal apply requires --goal and --accept")
        d = {
            "schema": SCHEMA,
            "plan_id": "plan_" + secrets.token_hex(16),
            "goal": env("OMS_GOAL"),
            "accept": env("OMS_ACCEPT"),
            "project_contract": {
                "schema": 1,
                "spec_sha256": proposal_spec,
                "allowed_envelope": reviewed_envelope,
                "acceptance_files": reviewed_acceptance_files,
                "acceptance_manifest": acceptance_manifest,
            },
            "tasks": {},
        }
        tasks = d["tasks"]

    if contract is None:
        # Adoption of an older manual plan is a first apply, not a replay: it
        # stays behind the proposal's HEAD + plan CAS and admits no extra task.
        if tasks:
            proposed_ids = set(item["id"] for item in prepared)
            extra_ids = sorted(set(tasks) - proposed_ids)
            if extra_ids:
                die("legacy plan has task(s) absent from the reviewed proposal: %s" %
                    ", ".join(extra_ids))
            for current_id, current_task in tasks.items():
                current_allowed = current_task.get("allowed_paths") or []
                if not current_allowed or not all(inside_envelope(item) for item in current_allowed):
                    die("existing task %s is outside the reviewed project contract" % current_id)
        d["project_contract"] = {
            "schema": 1,
            "spec_sha256": proposal_spec,
            "allowed_envelope": reviewed_envelope,
            "acceptance_files": reviewed_acceptance_files,
            "acceptance_manifest": acceptance_manifest,
        }
        ensure_plan_id(d)
        if prepared and all(item["id"] in tasks for item in prepared):
            save(d)
            print("plan: proposal already applied (%s)" % ",".join(seen))
            sys.exit(0)

    for item in prepared:
        task_id = item["id"]
        tasks[task_id] = {
            "id": task_id, "title": item["title"], "state": "ready",
            "depends": item["depends"], "allowed_paths": item["allowed_paths"],
            "forbidden_paths": item["forbidden_paths"], "verify": item["verify"],
            "role": item["role"], "provider": "", "ttl": "", "artifact": "",
            "patch": "", "reason": "", "executor_id": "",
            "executor_soul_sha256": "", "lease_epoch": 0, "lease_id": "",
            "autopilot_owner_id": "",
            "review_lease_id": "", "repair_count": 0, "repair_artifact": "",
            "created": ts, "updated": ts,
        }
    save(d)
    print("plan: proposal applied %s" % ",".join(seen))
    sys.exit(0)

if act == "add":
    i = require_id(); title = env("OMS_TITLE")
    if not title: die("--title is required for add")
    reject_controls(title, "task title")
    reject_controls(env("OMS_VERIFY"), "task verify")
    reject_controls(env("OMS_ROLE"), "task role")
    if d.get("project_contract") is not None:
        die("contract-bound plans accept new tasks only through a reviewed proposal")
    if i in tasks: die("task already exists: %s" % i)
    depends = split_list(env("OMS_DEPENDS"))
    unknown = [x for x in depends if x not in tasks]
    if unknown: die("unknown dependency id(s): %s" % ", ".join(unknown))
    tasks[i] = {
        "id": i, "title": title, "state": "ready",
        "depends": depends,
        "allowed_paths": split_list(env("OMS_ALLOWED")),
        "forbidden_paths": split_list(env("OMS_FORBIDDEN")),
        "verify": env("OMS_VERIFY"),
        "role": env("OMS_ROLE"),
        "provider": "", "ttl": "", "artifact": "", "patch": "", "reason": "",
        "executor_id": "", "executor_soul_sha256": "",
        "autopilot_owner_id": "",
        "lease_epoch": 0, "lease_id": "", "review_lease_id": "", "repair_count": 0,
        "repair_artifact": "",
        "created": ts, "updated": ts,
    }
    save(d); print("plan: added %s (%s)" % (i, title)); sys.exit(0)

def get_task(i):
    t = tasks.get(i)
    if not t: die("no such task: %s" % i)
    return t

if act in ("claim", "start", "finish", "review", "repair", "land", "block", "release", "recover-lease", "reopen", "show", "evidence-snapshot", "touch"):
    i = require_id(); t = get_task(i)
    if act == "touch":
        # Heartbeat: a live worker refreshes claimed_at so reclaim's TTL clock
        # restarts and it is not mistaken for a dead worker mid-run.
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can be touched" % (i, t["state"]))
        require_current_lease(t)
        t["claimed_at"] = ts
    elif act == "claim":
        prov = env("OMS_PROVIDER")
        if not prov: die("--provider is required for claim")
        # Only a ready task can be claimed; a blocked task must be reopened first.
        if t["state"] != "ready":
            die("task %s is %s; only a ready task can be claimed (reopen blocked first)" % (i, t["state"]))
        if not deps_done(d, t):
            pending = [x for x in t["depends"] if tasks.get(x, {}).get("state") != "done"]
            die("task %s has unfinished dependencies: %s" % (i, ", ".join(pending)))
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="", repair_artifact="",
                 autopilot_owner_id=owner_id())
    elif act == "start":
        if t["state"] != "claimed": die("task %s is %s; claim it first" % (i, t["state"]))
        require_current_lease(t)
        t["state"] = "running"
    elif act == "review":
        if t["state"] not in ("claimed", "running"):
            die("task %s is %s; only a claimed/running task can go to review" % (i, t["state"]))
        require_current_lease(t)
        executor_id = env("OMS_EXECUTOR_ID")
        executor_soul = env("OMS_EXECUTOR_SOUL_SHA256")
        if bool(executor_id) != bool(executor_soul):
            die("task %s review executor receipt requires both id and soul hash" % i)
        if executor_id and not ID_RE.fullmatch(executor_id):
            die("--executor-id must match [A-Za-z0-9._-]+")
        if executor_soul and not re.match(r"^[0-9a-f]{64}$", executor_soul):
            die("--executor-soul-sha256 must be a lowercase SHA-256")
        # A repair is the same review lease re-entering claimed state. It may
        # replace the patch evidence, but it cannot shed or swap the executor
        # that produced the prior review. A fresh lease may establish a new
        # receipt after an ordinary release/reclaim.
        same_review_lease = bool(t.get("lease_id")) and t.get("review_lease_id", "") == t.get("lease_id", "")
        if t.get("repair_count", 0) and same_review_lease:
            if executor_id != t.get("executor_id", "") or executor_soul != t.get("executor_soul_sha256", ""):
                die("task %s repaired review must keep the exact executor receipt" % i)
        t.update(state="review", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""),
                 review_lease_id=t.get("lease_id", ""), executor_id=executor_id,
                 executor_soul_sha256=executor_soul, repair_artifact="")
    elif act == "repair":
        # A landing gate can reject already-reviewed work and ask for one
        # bounded repair. Reuse the exact lease that produced the review so no
        # stale worker or wider claim is created. Keep the prior evidence until
        # the repaired worker publishes a replacement review; if it fails, the
        # caller can still inspect the patch that triggered the repair.
        if t["state"] != "review":
            die("task %s is %s; only reviewed work can enter repair" % (i, t["state"]))
        if not env("OMS_LEASE_ID"):
            die("task %s repair requires the exact review --lease-id" % i)
        require_current_lease(t)
        if not t.get("lease_id") or t.get("review_lease_id", "") != t.get("lease_id", ""):
            die("task %s review patch lease mismatch; repair is stale" % i)
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        repair_count = t.get("repair_count", 0)
        if isinstance(repair_count, bool) or not isinstance(repair_count, int) or repair_count < 0:
            die("task %s has an invalid repair counter" % i)
        if repair_count >= 1:
            die("task %s bounded review repair was already used" % i)
        t.update(state="claimed", claimed_at=ts, reason="", repair_count=1,
                 repair_artifact=env("OMS_ARTIFACT") or t.get("repair_artifact", ""))
    elif act == "land":
        if t["state"] != "review":
            die("task %s is %s; only reviewed work can enter landing" % (i, t["state"]))
        expected_fields = (
            "PATCH", "PATCH_SHA256", "VERIFY", "EXECUTOR_ID",
            "EXECUTOR_SOUL_SHA256", "LEASE_ID",
        )
        missing = [name.lower().replace("_", "-") for name in expected_fields
                   if env("OMS_EXPECTED_REVIEW_%s_SET" % name) != "1"]
        if missing:
            die("task %s land requires the complete expected review receipt: %s" %
                (i, ", ".join(missing)))
        if not env("OMS_LEASE_ID"):
            die("task %s land requires the current --lease-id" % i)
        require_current_lease(t)
        expected_lease = env("OMS_EXPECTED_REVIEW_LEASE_ID")
        if (not expected_lease or expected_lease != env("OMS_LEASE_ID") or
                t.get("review_lease_id", "") != expected_lease or
                t.get("lease_id", "") != expected_lease):
            die("task %s review patch lease mismatch; patch is stale" % i)
        if not t.get("artifact") or not t.get("patch"):
            die("task %s review is missing artifact/patch evidence" % i)
        expected_patch = env("OMS_EXPECTED_REVIEW_PATCH")
        stored_patch = t.get("patch", "")
        if not isinstance(stored_patch, str) or stored_patch != expected_patch:
            die("task %s reviewed patch path changed during admission" % i)
        expected_sha = env("OMS_EXPECTED_REVIEW_PATCH_SHA256")
        if not re.match(r"^[0-9a-f]{64}$", expected_sha):
            die("--expected-review-patch-sha256 must be a lowercase SHA-256")
        patch_path = stored_patch
        if not os.path.isabs(patch_path):
            patch_path = os.path.join(env("OMS_REPO"), patch_path)
        digest = hashlib.sha256()
        try:
            with open(patch_path, "rb") as handle:
                while True:
                    chunk = handle.read(1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
        except (OSError, TypeError) as exc:
            die("task %s reviewed patch cannot be hashed: %s" % (i, exc))
        if digest.hexdigest() != expected_sha:
            die("task %s reviewed patch bytes changed during admission" % i)
        stored_verify = t.get("verify", "")
        if (not isinstance(stored_verify, str) or
                stored_verify.replace("\r", "") != env("OMS_EXPECTED_REVIEW_VERIFY").replace("\r", "")):
            die("task %s verify contract changed during admission" % i)
        expected_executor = env("OMS_EXPECTED_REVIEW_EXECUTOR_ID")
        expected_soul = env("OMS_EXPECTED_REVIEW_EXECUTOR_SOUL_SHA256")
        if bool(expected_executor) != bool(expected_soul):
            die("expected review executor receipt requires both id and soul hash")
        if expected_executor and not ID_RE.fullmatch(expected_executor):
            die("--expected-review-executor-id must match [A-Za-z0-9._-]+")
        if expected_soul and not re.match(r"^[0-9a-f]{64}$", expected_soul):
            die("--expected-review-executor-soul-sha256 must be a lowercase SHA-256")
        if (t.get("executor_id", "") != expected_executor or
                t.get("executor_soul_sha256", "") != expected_soul):
            die("task %s executor review receipt changed during admission" % i)
        t["state"] = "landing"
    elif act == "finish":
        # Done is a landing receipt, not a worker self-report. patch-land owns
        # the review -> landing fence after mechanical admission succeeds.
        if t["state"] != "landing":
            die("task %s is %s; finish only after reviewed work enters landing" % (i, t["state"]))
        require_current_lease(t)
        if env("OMS_EXPECTED_LANDING_RECEIPT_SHA256_SET") != "1":
            die("task %s finish requires --expected-landing-receipt-sha256" % i)
        expected_receipt = env("OMS_EXPECTED_LANDING_RECEIPT_SHA256")
        if not re.fullmatch(r"[0-9a-f]{64}", expected_receipt):
            die("--expected-landing-receipt-sha256 must be a lowercase SHA-256")
        if landing_receipt_digest(t) != expected_receipt:
            die("task %s landing receipt changed; stale finish rejected" % i)
        if env("OMS_REFREEZE_ACCEPTANCE") == "1":
            # Consent-aware manifest refreeze (field finding: a consented,
            # admitted, floor-verified landing that touches acceptance-listed
            # files parked at the next accept because the frozen manifest
            # predates it). Only entries the FENCED patch itself modified are
            # recomputed from the landed tree; every other entry keeps its
            # frozen hash, so out-of-band edits — to other acceptance files,
            # or to these files after this landing — still park. The patch's
            # file list comes from the patch bytes the review fence pinned,
            # never from caller argv. Recompute-from-tree is idempotent, so a
            # --recover replay of this finish converges.
            contract = d.get("project_contract")
            patch_rel = env("OMS_PATCH") or t.get("patch", "")
            repo_root = os.path.realpath(env("OMS_REPO") or os.path.dirname(
                os.path.dirname(os.path.dirname(os.path.abspath(path)))))
            if isinstance(contract, dict) and patch_rel:
                patch_abs = os.path.join(repo_root, *patch_rel.split("/")) \
                    if not os.path.isabs(patch_rel) else patch_rel
                touched = set()
                try:
                    with open(patch_abs, encoding="utf-8", errors="replace") as ph:
                        for line in ph:
                            if line.startswith("+++ b/"):
                                touched.add(line[6:].strip())
                except OSError:
                    touched = set()
                files = contract.get("acceptance_files") or []
                manifest = contract.get("acceptance_manifest") or []
                refrozen = []
                for index_m, rel in enumerate(files):
                    if rel not in touched or index_m >= len(manifest):
                        continue
                    target = os.path.join(repo_root, *rel.split("/"))
                    digest_new = hashlib.sha256()
                    try:
                        with open(target, "rb") as th:
                            total = 0
                            while True:
                                chunk = th.read(1024 * 1024)
                                if not chunk:
                                    break
                                total += len(chunk)
                                if total > 8 * 1024 * 1024:
                                    die("acceptance file %s exceeds the manifest size cap" % rel)
                                digest_new.update(chunk)
                    except OSError:
                        die("cannot refreeze acceptance file %s" % rel)
                    old_hash = manifest[index_m].get("sha256", "")
                    new_hash = digest_new.hexdigest()
                    if old_hash != new_hash:
                        manifest[index_m] = {"path": rel, "sha256": new_hash}
                        refrozen.append({"path": rel, "old": old_hash, "new": new_hash})
                if refrozen:
                    ledger_path = os.path.join(
                        os.path.dirname(path), "manifest-refreeze.jsonl")
                    row_out = {
                        "schema": 1, "kind": "manifest-refreeze", "task": i,
                        "landing_receipt_sha256": expected_receipt,
                        "entries": refrozen, "ts": ts,
                    }
                    ledger_fd = os.open(
                        ledger_path,
                        os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
                    try:
                        os.write(ledger_fd, (json.dumps(
                            row_out, sort_keys=True,
                            separators=(",", ":")) + "\n").encode("utf-8"))
                        os.fsync(ledger_fd)
                    finally:
                        os.close(ledger_fd)
        t.update(state="done", artifact=env("OMS_ARTIFACT") or t.get("artifact", ""),
                 patch=env("OMS_PATCH") or t.get("patch", ""))
    elif act == "block":
        r = env("OMS_REASON")
        if not r: die("--reason is required for block")
        if t["state"] in ("claimed", "running", "review", "landing"):
            require_current_lease(t)
        t.update(state="blocked", reason=r)
    elif act == "release":
        # Requeue a claimed/running task (e.g. the worker died) back to ready.
        if t["state"] not in ("claimed", "running", "review", "landing"):
            die("task %s is %s; only a claimed/running/review/landing task can be released" % (i, t["state"]))
        require_current_lease(t)
        clear_claim(t)
    elif act == "recover-lease":
        supplied = env("OMS_LEASE_ID")
        expected_state = env("OMS_EXPECTED_STATE")
        if not expected_state or expected_state not in STATES:
            die("recover-lease requires a valid --expected-state")
        if (not supplied or t.get("lease_id", "") != supplied or
                t.get("state") != expected_state or
                expected_state not in ("claimed", "running")):
            sys.stderr.write("plan: task %s no longer holds that exact claimed/running state+lease\n" % i)
            sys.exit(3)
        matching = exact_lease_markers(load_worker_markers(), i, supplied)
        # A malformed pid is not proof of death. Any exact live marker is a
        # retry/worker veto even when an older dead marker triggered cleanup.
        if any(not worker_marker_is_typed(marker) for marker in matching):
            sys.stderr.write("plan-recovery-outcome: unproven\n")
            sys.stderr.write("plan: task %s has an unproven exact worker marker\n" % i)
            sys.exit(3)
        if any(marker_pid_alive(marker) for marker in matching):
            sys.stderr.write("plan-recovery-outcome: veto\n")
            sys.stderr.write("plan: task %s still has a live exact worker marker\n" % i)
            sys.exit(3)
        if env("OMS_CHECK_ONLY") == "1":
            print("plan: task %s exact state+lease is recoverable" % i)
            sys.exit(0)
        clear_claim(t)
    elif act == "reopen":
        if t["state"] != "blocked":
            die("task %s is %s; only a blocked task can be reopened" % (i, t["state"]))
        clear_claim(t)
    elif act in ("show", "evidence-snapshot"):
        # Computed on a copy: the stored task keeps exactly the fields its
        # writers put there, and a later save() cannot persist a read's view.
        view = dict(t)
        if isinstance(d.get("project_contract"), dict):
            # Contract metadata is a read-only view for admission consumers. It
            # is deliberately not copied into each stored task.
            view["project_contract"] = d["project_contract"]
        if act == "evidence-snapshot":
            view["plan_id"] = d.get("plan_id", "")
        view["claim_expired"] = claim_expired(t)
        if t.get("state") in ("claimed", "running", "review"):
            age = claim_age(t)
            if age is not None:
                view["claim_age_s"] = age
        print(json.dumps(view, ensure_ascii=False, indent=2)); sys.exit(0)
    t["updated"] = ts
    save(d); print("plan: %s -> %s" % (i, t["state"])); sys.exit(0)

# Read-only queries.
ordered = sorted(tasks.values(), key=lambda t: t.get("created", ""))

if act == "recover-owner":
    recovery_owner = owner_id()
    if not recovery_owner:
        die("recover-owner requires --owner-id")
    markers = load_worker_markers()

    recovered = []
    preserved = []
    for task in ordered:
        if task.get("autopilot_owner_id", "") != recovery_owner:
            continue
        state = task.get("state")
        if state in ("review", "landing"):
            preserved.append(task["id"])
            continue
        if state not in ("claimed", "running"):
            continue
        lease = task.get("lease_id", "")
        if not lease:
            preserved.append(task["id"])
            continue
        matching = exact_lease_markers(markers, task["id"], lease)
        owned = [marker for marker in matching
                 if marker.get("autopilot_owner_id") == recovery_owner]
        if any(not worker_marker_is_typed(marker) for marker in matching):
            preserved.append(task["id"])
            continue
        if any(marker_pid_alive(marker) for marker in matching):
            preserved.append(task["id"])
            continue
        if matching and len(owned) != len(matching):
            preserved.append(task["id"])
            continue
        if state == "running" and not owned:
            preserved.append(task["id"])
            continue
        clear_claim(task)
        task["updated"] = ts
        recovered.append(task["id"])
    if recovered:
        save(d)
    result = {"owner_id": recovery_owner, "recovered": recovered,
              "preserved": preserved}
    if env("OMS_AS_JSON") == "1":
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    else:
        print("plan: recovered %s" % (",".join(recovered) if recovered else "(none)"))
        if preserved:
            print("plan: preserved %s" % ",".join(preserved))
    sys.exit(0)

if act == "reclaim":
    # Dead-worker recovery: claim/next store provider+ttl+claimed_at, and this
    # is the consumer. Only ages out claimed (and, opted in, running) tasks by
    # default. review holds a finished artifact awaiting a reviewer, so TTL
    # expiry there means "waiting on reviewer", not "dead worker" — reclaiming
    # it is a separate opt-in with its own clock (updated = when it entered
    # review) and a longer default TTL, and keeps artifact/patch so the
    # finished work is not lost. The claimed/running default is the same
    # OMS_PLAN_CLAIM_TTL the read paths present expiry with, so this frees
    # exactly the claims that already read as expired.
    raw_ttl = env("OMS_TTL")
    if raw_ttl and not raw_ttl.isdigit():
        die("reclaim --ttl must be an integer number of seconds")
    default_ttl = int(raw_ttl) if raw_ttl else CLAIM_TTL
    review_ttl = int(raw_ttl) if raw_ttl else 86400
    states = {"claimed"}
    if env("OMS_INCLUDE_RUNNING") == "1":
        states.add("running")
    if env("OMS_INCLUDE_REVIEW") == "1":
        states.add("review")
    reclaimed = 0
    for t in ordered:
        if t["state"] not in states:
            continue
        anchor = claim_anchor(t)
        if anchor is None or now_dt is None:
            continue
        if t["state"] == "review":
            ttl_s = review_ttl
        else:
            ttl_s = claim_ttl_for(t, default_ttl)
        age = int((now_dt - anchor).total_seconds())
        if age < ttl_s:
            continue
        prov = t.get("provider", "") or "?"
        was = t["state"]
        clear_claim(t)
        t["updated"] = ts
        reclaimed += 1
        print("plan: reclaimed %s from %s (age %ss > ttl %ss, was @%s)" % (t["id"], was, age, ttl_s, prov))
    if reclaimed:
        save(d)
    print("plan: reclaimed %d task(s)" % reclaimed)
    sys.exit(0)

if act == "brief":
    i = require_id()
    print(brief_text(get_task(i)))
    sys.exit(0)

if act == "next":
    candidates = [t for t in ordered if actionable(d, t)]
    if not candidates:
        sys.stderr.write("plan: no actionable task\n")
        sys.exit(3)
    t = candidates[0]
    if claim_expired(t):
        sys.stderr.write("plan: %s %s; %s\n" % (
            t["id"], expiry_note(t),
            "re-claiming under a new lease" if env("OMS_CLAIM") == "1"
            else "offered as claimable"))
    if env("OMS_CLAIM") == "1":
        prov = env("OMS_PROVIDER")
        if not prov:
            die("--claim requires --provider")
        # A fresh lease is the fence: whatever the previous holder does next
        # fails the lease check instead of racing this worker.
        issue_lease(t)
        t.update(state="claimed", provider=prov, ttl=env("OMS_TTL"),
                 claimed_at=ts, reason="", autopilot_owner_id=owner_id())
        t["updated"] = ts
        save(d)
    if env("OMS_AS_JSON") == "1":
        view = dict(t)
        view["claim_expired"] = claim_expired(t)
        print(json.dumps(view, ensure_ascii=False, indent=2))
    else:
        print(brief_text(t))
    sys.exit(0)

if act == "ready":
    # Ids only: this output is consumed. The expiry note goes to stderr.
    for t in ordered:
        if not actionable(d, t):
            continue
        if claim_expired(t):
            sys.stderr.write("plan: %s %s; counted as ready\n" % (t["id"], expiry_note(t)))
        print(t["id"])
    sys.exit(0)

if act == "list":
    sf = env("OMS_STATE_FILTER")
    if sf and sf not in STATES: die("unknown --state: %s" % sf)
    for t in ordered:
        if sf and t["state"] != sf: continue
        dep = (" depends=%s" % ",".join(t["depends"])) if t["depends"] else ""
        prov = (" @%s" % t["provider"]) if t.get("provider") else ""
        # The state column keeps saying what is stored; the tag says how that
        # stored state reads now.
        tag = (" EXPIRED(%s)" % expiry_note(t)) if claim_expired(t) else ""
        print("%-10s %-9s %s%s%s%s" % (t["id"], t["state"], t["title"], prov, dep, tag))
    sys.exit(0)

if act == "status":
    if d.get("goal"): print("goal: %s" % d["goal"])
    contract = d.get("project_contract")
    if isinstance(contract, dict) and contract.get("spec_sha256"):
        print("contract: PROJECT.md %s scope=%s" % (
            str(contract["spec_sha256"])[:12],
            ",".join(contract.get("allowed_envelope") or []) or "?",
        ))
    if d.get("accept"):
        print("accept: %s" % d["accept"])
        progress_path = os.path.join(os.path.dirname(path), "progress.jsonl")
        last = None
        try:
            with open(progress_path, encoding="utf-8") as fh:
                for line in fh:
                    try:
                        last = json.loads(line)
                    except Exception:
                        continue
        except OSError:
            pass
        if last:
            print("acceptance: %s at %s (base %s)" % (
                last.get("status", "?"), last.get("ts", "?"),
                str(last.get("base_sha", "?"))[:12]))
    by = {}
    for t in tasks.values(): by[t["state"]] = by.get(t["state"], 0) + 1
    order = ["ready", "claimed", "running", "review", "landing", "blocked", "done"]
    print("tasks: %d  [%s]" % (len(tasks),
        " ".join("%s=%d" % (s, by[s]) for s in order if by.get(s))))
    claimable = [t["id"] for t in ordered if actionable(d, t)]
    print("ready now: %s" % (" ".join(claimable) if claimable else "(none)"))
    for t in ordered:
        if claim_expired(t):
            print("expired claim %s: %s" % (t["id"], expiry_note(t)))
    blocked = [t for t in ordered if t["state"] == "blocked"]
    for t in blocked:
        print("blocked %s: %s" % (t["id"], t.get("reason", "")))
    waiting = [t["id"] for t in ordered
               if t["state"] == "ready" and not deps_done(d, t)]
    if waiting: print("waiting on deps: %s" % " ".join(waiting))
    sys.exit(0)

die("unhandled action: %s" % act)
PY
}

# Keep the plan dir out of git like the rest of .oms state. Acceptance is
# read-only until its final receipt, so validate its existing authority files
# before any mkdir/ignore helper could follow a planted state-directory link.
if [ "$ACTION" = accept ]; then
  python3 - "$REPO" "$PLAN_FILE" <<'PY' ||
import os, stat, sys
repo = os.path.realpath(sys.argv[1])
target = os.path.abspath(sys.argv[2])
parent = os.path.dirname(target)
expected = os.path.join(repo, ".oms", "plan")
try:
    parent_info = os.lstat(parent)
    target_info = os.lstat(target)
except OSError as exc:
    print("error: cannot inspect acceptance plan authority: %s" % exc, file=sys.stderr)
    raise SystemExit(2)
if (parent != expected or os.path.realpath(parent) != expected or
        stat.S_ISLNK(parent_info.st_mode) or not stat.S_ISDIR(parent_info.st_mode)):
    print("error: acceptance plan parent must be the real repo-local .oms/plan directory", file=sys.stderr)
    raise SystemExit(2)
if stat.S_ISLNK(target_info.st_mode) or not stat.S_ISREG(target_info.st_mode):
    print("error: acceptance plan must be a regular non-symlink file", file=sys.stderr)
    raise SystemExit(2)
PY
    fail "acceptance requires a safe repo-local plan file"
else
  mkdir -p "$(dirname "$PLAN_FILE")"
  agent_memory_ensure_oms_ignore_for_path "$PLAN_FILE" 2>/dev/null || true
fi

# `accept` runs OUTSIDE the plan lock: the acceptance command is an arbitrary
# project check (often the full gate) and must not hold the task-graph lock
# for its whole runtime. It only reads the stored command, runs it from the
# repo root, and appends one receipt row — the executable answer to "is the
# goal actually met", which no per-task verify can give.
if [ "$ACTION" = "accept" ]; then
  [ -f "$PLAN_FILE" ] || fail "no plan at $PLAN_FILE; run: agent-plan init --goal ... --accept CMD"
  progress="$(dirname "$PLAN_FILE")/progress.jsonl"
  python3 "$ROOT/scripts/lib/durable-jsonl.py" --label progress.jsonl check "$progress" ||
    fail "progress.jsonl must be a repo-local regular non-symlink file"
  accept_cmd="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
print(d.get("accept", "") or "")
' "$PLAN_FILE")"
  [ -n "$accept_cmd" ] ||
    fail "plan has no acceptance command; set one with: agent-plan init --goal ... --accept CMD"
  # The command and the digest the verdict is filed against have to describe
  # the same plan. accept deliberately runs outside the plan lock, and the
  # freeze below happens several reads later -- the contract manifest and the
  # repository snapshot come first -- so a second session replacing the plan in
  # between left this run executing the previous acceptance command while the
  # post-check compared the NEW plan against itself. The old contract's pass
  # was then recorded as the new contract's.
  accept_plan_sha="$(oms_sha256_file "$PLAN_FILE")" ||
    fail "cannot freeze the plan alongside its acceptance command"
  # Test-only: replace the plan inside the window this check covers. Inert
  # unless set, like the other OMS_*_TEST_* hooks in this tree.
  if [ -n "${OMS_PLAN_ACCEPT_TEST_REWRITE:-}" ] && [ -f "${OMS_PLAN_ACCEPT_TEST_REWRITE}" ]; then
    cat "$OMS_PLAN_ACCEPT_TEST_REWRITE" > "$PLAN_FILE"
  fi

  # Contract-bound acceptance names every verifier/input file whose bytes were
  # reviewed. Re-open each leaf without following symlinks and compare it with
  # the stored digest before and after execution. Legacy manual plans have no
  # project_contract and therefore use the empty manifest, but still receive
  # the repository and plan mutation fences below.
  acceptance_manifest() {
    python3 - "$REPO" "$PLAN_FILE" <<'PY' | tr -d '\r'
import hashlib, json, os, stat, sys

repo = os.path.realpath(sys.argv[1])
with open(sys.argv[2], encoding="utf-8") as handle:
    plan = json.load(handle)
contract = plan.get("project_contract")
if contract is None:
    print("legacy")
    raise SystemExit(0)
if not isinstance(contract, dict) or contract.get("schema") != 1:
    raise SystemExit(2)
files = contract.get("acceptance_files", [])
manifest = contract.get("acceptance_manifest", [])
if (not isinstance(files, list) or not isinstance(manifest, list) or
        len(files) > 64 or len(files) != len(manifest)):
    raise SystemExit(2)
current = []
for index, rel in enumerate(files):
    if (not isinstance(rel, str) or not rel or len(rel.encode("utf-8")) > 240 or
            rel.startswith("/") or "\\" in rel or
            any(part in ("", ".", "..") for part in rel.split("/"))):
        raise SystemExit(2)
    expected = manifest[index]
    if (not isinstance(expected, dict) or expected.get("path") != rel or
            not isinstance(expected.get("sha256"), str)):
        raise SystemExit(2)
    target = os.path.join(repo, *rel.split("/"))
    if os.path.realpath(target) != target:
        raise SystemExit(3)
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(target, flags)
    except OSError:
        raise SystemExit(3)
    digest = hashlib.sha256()
    total = 0
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise SystemExit(3)
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, 8 * 1024 * 1024 + 1 - total))
            if not chunk:
                break
            total += len(chunk)
            if total > 8 * 1024 * 1024:
                raise SystemExit(3)
            digest.update(chunk)
    finally:
        os.close(descriptor)
    value = digest.hexdigest()
    if value != expected.get("sha256"):
        raise SystemExit(3)
    current.append({"path": rel, "sha256": value})
if files != sorted(set(files)):
    raise SystemExit(2)
print(hashlib.sha256(json.dumps(
    current, sort_keys=True, separators=(",", ":")
).encode()).hexdigest())
PY
  }

  # Exact repository state, including symbolic ref, HEAD, index entries,
  # tracked working bytes, and every untracked leaf. The 64 MiB ceiling keeps
  # an already-pathological dirty checkout from turning this fence into an
  # unbounded read; goal-drive normally supplies a clean dedicated worktree.
  acceptance_repo_snapshot() {
    python3 - "$REPO" <<'PY' | tr -d '\r'
import hashlib, os, stat, subprocess, sys

repo = os.path.realpath(sys.argv[1])
limit = 64 * 1024 * 1024
total = 0
digest = hashlib.sha256()
git_env = os.environ.copy()
git_env["GIT_CONFIG_GLOBAL"] = os.devnull
git_env["GIT_CONFIG_SYSTEM"] = os.devnull

def command(argv):
    value = subprocess.check_output(
        argv, stderr=subprocess.DEVNULL, env=git_env)
    global total
    total += len(value)
    if total > limit:
        raise SystemExit(2)
    digest.update(value)
    digest.update(b"\0")
    return value

command(["git", "-C", repo, "symbolic-ref", "-q", "HEAD"])
command(["git", "-C", repo, "rev-parse", "HEAD"])
command(["git", "-C", repo, "ls-files", "--stage", "-z"])
command(["git", "-c", "core.fsmonitor=false", "-c", "diff.external=",
         "-C", repo, "diff", "--no-ext-diff", "--no-textconv",
         "--binary", "HEAD", "--"])
untracked = command(["git", "-C", repo, "ls-files", "--others", "--exclude-standard", "-z"])
for raw in sorted(value for value in untracked.split(b"\0") if value):
    rel = os.fsdecode(raw)
    target = os.path.join(repo, rel)
    if os.path.commonpath((repo, os.path.realpath(target))) != repo:
        raise SystemExit(2)
    info = os.lstat(target)
    digest.update(raw + b"\0" + str(stat.S_IFMT(info.st_mode)).encode() + b"\0")
    if stat.S_ISLNK(info.st_mode):
        value = os.fsencode(os.readlink(target))
        total += len(value)
        digest.update(value)
    elif stat.S_ISREG(info.st_mode):
        with open(target, "rb") as handle:
            while True:
                chunk = handle.read(min(1024 * 1024, limit + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                if total > limit:
                    raise SystemExit(2)
                digest.update(chunk)
    else:
        raise SystemExit(2)
print(digest.hexdigest())
PY
  }

  acceptance_integrity_error() {  # REASON MESSAGE [BASE]
    local reason="$1" message="$2" base="${3:-unborn}"
    echo "plan-accept: error (exit 2, 0s) base=$base reason=$reason"
    echo "error: $message" >&2
    exit 2
  }

  command -v oms_git_assert_safe_execution_config >/dev/null 2>&1 ||
    fail "installed Git execution guard is unavailable"
  command -v oms_git_assert_plain_index >/dev/null 2>&1 ||
    fail "installed Git index guard is unavailable"
  oms_git_assert_safe_execution_config "$REPO" ||
    acceptance_integrity_error acceptance-mutated-repository \
      "unsafe executable Git config is active" unsafe
  oms_git_assert_plain_index "$REPO" ||
    acceptance_integrity_error acceptance-mutated-repository \
      "hidden Git index flags are active"
  manifest_before="$(acceptance_manifest)" ||
    acceptance_integrity_error acceptance-files-changed \
      "acceptance files changed or their stored manifest is invalid"
  repo_before="$(acceptance_repo_snapshot)" ||
    acceptance_integrity_error acceptance-supervision-failed \
      "cannot freeze repository state before acceptance"
  plan_before="$(oms_sha256_file "$PLAN_FILE")" ||
    acceptance_integrity_error acceptance-supervision-failed \
      "cannot freeze the plan before acceptance"
  [ "$plan_before" = "$accept_plan_sha" ] ||
    acceptance_integrity_error acceptance-command-changed \
      "the plan changed between reading its acceptance command and freezing it"

  timeout_value="${OMS_PLAN_ACCEPT_TIMEOUT:-10m}"
  timeout_seconds="$(python3 - "$timeout_value" <<'PY' | tr -d '\r'
import re, sys
match = re.fullmatch(r"([1-9][0-9]*)([smh]?)", sys.argv[1])
if not match:
    raise SystemExit(2)
seconds = int(match.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[match.group(2)]
if seconds > 24 * 60 * 60:
    raise SystemExit(2)
print(seconds)
PY
)" || fail "OMS_PLAN_ACCEPT_TIMEOUT must be a positive duration up to 24h"
  out_tmp="$(mktemp)" || fail "mktemp failed"
  meta_tmp="$(mktemp)" || { rm -f "$out_tmp"; fail "mktemp failed"; }
  start_s="$(date +%s)"
  accept_supervisor_pid=""
  acceptance_forward_signal() {  # SIGNAL EXIT_CODE
    local signal_name="$1" exit_code="$2"
    trap - HUP INT TERM
    if [ -n "$accept_supervisor_pid" ]; then
      kill -s "$signal_name" "$accept_supervisor_pid" 2>/dev/null || true
      wait "$accept_supervisor_pid" 2>/dev/null || true
    fi
    rm -f "$out_tmp" "$meta_tmp"
    exit "$exit_code"
  }
  trap 'acceptance_forward_signal HUP 129' HUP
  trap 'acceptance_forward_signal INT 130' INT
  trap 'acceptance_forward_signal TERM 143' TERM
  set +e
  # Use the same whole-tree supervisor as provider phases. In addition to the
  # original group, it retains nested process groups/sessions, periodically
  # refreshes descendants, and adopts daemonized Linux grandchildren. The
  # capture options preserve acceptance's exact 1 MiB output and typed receipt.
  python3 "$ROOT/scripts/lib/autopilot-receipt.py" supervise \
    --wall "$timeout_seconds" --kill-after 1 --label acceptance \
    --cwd "$REPO" --output "$out_tmp" --output-limit 1048576 \
    --metadata "$meta_tmp" -- bash -c "$accept_cmd" &
  accept_supervisor_pid=$!
  wait "$accept_supervisor_pid"
  runner_exit=$?
  accept_supervisor_pid=""
  trap - HUP INT TERM
  set -e
  meta="$(python3 - "$meta_tmp" <<'PY' | tr -d '\r'
import json, sys
try:
    value = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    raise SystemExit(2)
expected = {"exit", "timed_out", "output_limited", "launch_error", "supervision_error"}
if set(value) != expected:
    raise SystemExit(2)
exit_code = value.get("exit")
flags = [value.get(name) for name in (
    "timed_out", "output_limited", "launch_error", "supervision_error"
)]
if (not isinstance(exit_code, int) or isinstance(exit_code, bool) or
        not 0 <= exit_code <= 255 or any(not isinstance(item, bool) for item in flags)):
    raise SystemExit(2)
print("%s\t%s\t%s\t%s\t%s" % (
    exit_code, *(int(item) for item in flags)
))
PY
)" || { rm -f "$out_tmp" "$meta_tmp"; fail "acceptance supervisor did not return a receipt"; }
  accept_exit="$(printf '%s' "$meta" | cut -f1)"
  accept_timed_out="$(printf '%s' "$meta" | cut -f2)"
  accept_output_limited="$(printf '%s' "$meta" | cut -f3)"
  accept_launch_error="$(printf '%s' "$meta" | cut -f4)"
  accept_supervision_error="$(printf '%s' "$meta" | cut -f5)"
  rm -f "$meta_tmp"
  duration=$(( $(date +%s) - start_s ))
  post_git_safe=1
  oms_git_assert_safe_execution_config "$REPO" >/dev/null 2>&1 || post_git_safe=0
  oms_git_assert_plain_index "$REPO" >/dev/null 2>&1 || post_git_safe=0
  if [ "$post_git_safe" = 1 ]; then
    base_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unborn)"
  else
    base_sha=unsafe
  fi
  out_digest="$(oms_sha256_stream < "$out_tmp" 2>/dev/null || echo unhashed)"
  verdict=pass
  [ "$accept_exit" -eq 0 ] || verdict=fail
  integrity_reason=""
  plan_after="$(oms_sha256_file "$PLAN_FILE" 2>/dev/null || true)"
  manifest_after="$(acceptance_manifest 2>/dev/null || true)"
  if [ "$post_git_safe" = 1 ]; then
    repo_after="$(acceptance_repo_snapshot 2>/dev/null || true)"
  else
    repo_after=""
  fi
  # Integrity outranks the command's own result: a failing check that mutates
  # inputs or repository state is not an ordinary acceptance failure.
  if [ "$post_git_safe" != 1 ]; then
    integrity_reason=acceptance-mutated-repository
  elif [ -z "$plan_after" ] || [ "$plan_after" != "$plan_before" ]; then
    integrity_reason=acceptance-command-changed
  elif [ -z "$manifest_after" ] || [ "$manifest_after" != "$manifest_before" ]; then
    integrity_reason=acceptance-files-changed
  elif [ -z "$repo_after" ] || [ "$repo_after" != "$repo_before" ]; then
    integrity_reason=acceptance-mutated-repository
  elif [ "$runner_exit" -ne "$accept_exit" ] || \
      [ "$accept_launch_error" = 1 ] || [ "$accept_supervision_error" = 1 ]; then
    integrity_reason=acceptance-supervision-failed
  elif [ "$accept_timed_out" = 1 ]; then
    integrity_reason=acceptance-timeout
  elif [ "$accept_output_limited" = 1 ]; then
    integrity_reason=acceptance-output-limit
  fi
  [ -z "$integrity_reason" ] || verdict=error
  accept_digest="$(printf '%s' "$accept_cmd" | oms_sha256_stream 2>/dev/null || echo unhashed)"
  # run_id/cycle are set by goal-drive so one run's rows correlate; manual
  # invocations leave them empty. The row is the goal-run protocol record:
  # enough to answer which command, on which tree, in which cycle, and why.
  OMS_PA_TS="$ts" OMS_PA_SHA="$base_sha" OMS_PA_VERDICT="$verdict" \
    OMS_PA_EXIT="$accept_exit" OMS_PA_DIGEST="$out_digest" OMS_PA_DUR="$duration" \
    OMS_PA_ACCEPT="$accept_digest" OMS_PA_RUN="${OMS_GOAL_RUN_ID:-}" \
    OMS_PA_CYCLE="${OMS_GOAL_CYCLE:-}" OMS_PA_REASON="$integrity_reason" \
    OMS_PA_TIMEOUT="$accept_timed_out" OMS_PA_LIMITED="$accept_output_limited" \
    python3 -c '
import json, os
row = {
    "schema": 1, "kind": "acceptance",
    "ts": os.environ["OMS_PA_TS"], "base_sha": os.environ["OMS_PA_SHA"],
    "status": os.environ["OMS_PA_VERDICT"], "exit": int(os.environ["OMS_PA_EXIT"]),
    "accept_sha256": os.environ["OMS_PA_ACCEPT"][:16],
    "output_sha256": os.environ["OMS_PA_DIGEST"][:16],
    "duration_s": int(os.environ["OMS_PA_DUR"]),
    "reason": os.environ.get("OMS_PA_REASON", ""),
    "timed_out": os.environ.get("OMS_PA_TIMEOUT") == "1",
    "output_limited": os.environ.get("OMS_PA_LIMITED") == "1",
}
if os.environ.get("OMS_PA_RUN"): row["run_id"] = os.environ["OMS_PA_RUN"]
if os.environ.get("OMS_PA_CYCLE"): row["cycle"] = int(os.environ["OMS_PA_CYCLE"])
print(json.dumps(row, ensure_ascii=False))
' | python3 "$ROOT/scripts/lib/durable-jsonl.py" --label progress.jsonl append "$progress" || {
    rm -f "$out_tmp"
    fail "cannot durably append the acceptance receipt to progress.jsonl"
  }
  # A non-pass verdict's output used to survive only as a stderr tail and the
  # row's digest: diagnosing a parked run meant re-running the acceptance and
  # hoping the environment had not moved. Persist the body under the digest the
  # row already carries. Diagnostics, not evidence: the row's output_sha256
  # stays the digest of the raw output, while the stored body is machine-path
  # normalized (repo-local git-ignored sink contract) and replaced by a marker
  # when it matches the sensitive-content guard.
  accept_log=""
  if [ "$verdict" != "pass" ] && [ "$out_digest" != unhashed ]; then
    accept_log_dir="$REPO/.oms/plan/acceptance"
    accept_log_key="$(printf '%s' "$out_digest" | cut -c1-16)"
    mkdir -p "$accept_log_dir" 2>/dev/null || true
    body_tmp="$(mktemp 2>/dev/null || true)"
    if [ -n "$body_tmp" ]; then
      if agent_memory_file_has_secret_content "$out_tmp"; then
        printf 'redacted: acceptance output matched the sensitive-content guard\n' > "$body_tmp"
      else
        agent_memory_normalize_machine_paths < "$out_tmp" > "$body_tmp" 2>/dev/null || : > "$body_tmp"
      fi
    fi
    if [ -n "$body_tmp" ] && [ -s "$body_tmp" ] &&
      python3 "$ROOT/scripts/lib/durable-jsonl.py" write \
        "$accept_log_dir/$accept_log_key.log" < "$body_tmp" 2>/dev/null; then
      accept_log=".oms/plan/acceptance/$accept_log_key.log"
      # Keep the newest 20 bodies. The prune runs only after a durable write
      # proved the directory real, and removes by name inside it.
      # shellcheck disable=SC2010,SC2012
      ls -1t "$accept_log_dir" 2>/dev/null | grep '\.log$' | tail -n +21 |
        while IFS= read -r stale_log; do
          rm -f -- "$accept_log_dir/$stale_log" 2>/dev/null || true
        done || true
    else
      echo "plan-accept: warning: acceptance output body was not persisted" >&2
    fi
    [ -z "$body_tmp" ] || rm -f "$body_tmp"
  fi
  echo "plan-accept: $verdict (exit $accept_exit, ${duration}s) base=$base_sha${integrity_reason:+ reason=$integrity_reason}${accept_log:+ output-log=$accept_log}"
  if [ "$verdict" != "pass" ]; then
    echo "--- acceptance output (last 20 lines) ---" >&2
    tail -n 20 "$out_tmp" >&2
    [ -z "$accept_log" ] || echo "full output: $accept_log" >&2
    rm -f "$out_tmp"
    [ "$verdict" = fail ] && exit 3
    exit 2
  fi
  rm -f "$out_tmp"
  exit 0
fi

# Serialize the read-decide-write section against other agents. Recovery also
# freezes worker-marker publication while it judges liveness, in the global
# marker set -> plan order. Ordinary plan transitions never nest these locks.
plan_run_with_plan_lock() {
  oms_with_file_lock "$PLAN_FILE" plan_run
}
case "$ACTION" in
  recover-lease|recover-owner)
    oms_with_file_lock "$REPO/.oms/delegations/.marker-set-lock-target" \
      plan_run_with_plan_lock
    ;;
  *) plan_run_with_plan_lock ;;
esac
