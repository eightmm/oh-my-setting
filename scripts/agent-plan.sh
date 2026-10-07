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
# shellcheck source=scripts/lib/project-state.sh
. "$ROOT/scripts/lib/project-state.sh"

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
LANDED_COMMIT=""
LAND_RECEIPTS_DIR=""
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
REUSE_PASS_RUN=""
ROLE=""
ASSIGNMENT="{}"
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
APPLY=0
DISPOSITION=""
RETIRE_PHASE="${OMS_PLAN_RETIRE_PHASE:-}"

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
  retire [--check]                   Inspect whether the exact active plan can
                                     be retired; default is read-only and prints
                                     its SHA-256 CAS token.
         --apply --expected-plan-sha256 SHA256
         --disposition completed-external|superseded --reason TEXT
                                     Preserve the exact plan bytes in a
                                     content-addressed archive and append a
                                     typed retirement receipt. Active worker
                                     authority vetoes every disposition.
                                     completed-external runs a fresh acceptance
                                     against one clean committed HEAD; the
                                     other dispositions never claim completion.
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
         [--assignment '{"provider":"claude","workload":"routine"}']
         [--verify CMD] [--role NAME]
  claim  --id ID --provider NAME [--ttl TEXT]   Claim a ready task for a worker.
  start  --id ID [--lease-id TOKEN]  Mark a claimed task running.
  touch  --id ID [--lease-id TOKEN]  Heartbeat a claimed/running task: refresh
                                     claimed_at so a live worker is not reclaimed.
  review --id ID [--lease-id TOKEN] [--artifact PATH] [--patch PATH]
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
         --id ID --landed-commit SHA  Complete reviewed work integrated by
                                     commit. Requires an exact-SHA oms land
                                     receipt with passed gate/push/CI and
                                     reachability from its local target ref.
                                     Typed patch-land remains the default
                                     for single patches.
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
  list   [--state STATE] [--json]    List tasks (optionally by state); --json
                                     emits every task's `show` view at once.
  ready                              Print ids actionable now (deps done).
  status [--json]                    Typed plan snapshot or human summary.
  accept [--reuse-pass RUN_ID]        Reuse a matching pass only when PROJECT.md
                                     declares Acceptance reuse: same-run; otherwise
                                     run fresh. Reuse expires after ten minutes.
                                     Run the stored acceptance command from the
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
    --landed-commit) [ "$#" -ge 2 ] && [ -n "$2" ] || fail "--landed-commit requires SHA"; LANDED_COMMIT="$2"; shift 2 ;;
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
    --assignment) [ "$#" -ge 2 ] || fail "--assignment requires JSON"; ASSIGNMENT="$2"; shift 2 ;;
    --depends) [ "$#" -ge 2 ] || fail "--depends requires list"; DEPENDS="$2"; shift 2 ;;
    --allowed) [ "$#" -ge 2 ] || fail "--allowed requires list"; ALLOWED="$2"; shift 2 ;;
    --role) [ "$#" -ge 2 ] || fail "--role requires a name"; ROLE="$2"; shift 2 ;;
    --forbidden) [ "$#" -ge 2 ] || fail "--forbidden requires list"; FORBIDDEN="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires command"; VERIFY="$2"; shift 2 ;;
    --reuse-pass) [ "$#" -ge 2 ] || fail "--reuse-pass requires a run id"; REUSE_PASS_RUN="$2"; shift 2 ;;
    --accept) [ "$#" -ge 2 ] || fail "--accept requires command"; ACCEPT="$2"; shift 2 ;;
    --accept-files) [ "$#" -ge 2 ] || fail "--accept-files requires a value"; ACCEPT_FILES="$2"; shift 2 ;;
    --owner-id) [ "$#" -ge 2 ] || fail "--owner-id requires a value"; OWNER_ID="$2"; shift 2 ;;
    --markers-dir) [ "$#" -ge 2 ] || fail "--markers-dir requires a path"; MARKERS_DIR="$2"; shift 2 ;;
    --expected-state) [ "$#" -ge 2 ] || fail "--expected-state requires a value"; EXPECTED_STATE="$2"; shift 2 ;;
    --check) CHECK_ONLY=1; shift ;;
    --apply) APPLY=1; shift ;;
    --disposition) [ "$#" -ge 2 ] || fail "--disposition requires a value"; DISPOSITION="$2"; shift 2 ;;
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
    init|ensure-lineage|retire|apply-proposal|add|claim|start|touch|review|repair|land|finish|block|release|recover-lease|recover-owner|reclaim|reopen|show|evidence-snapshot|list|ready|status|next|brief|accept|lint-verify)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"; ACTION="$1"; shift ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }
[ "$CHECK_ONLY" = 0 ] || [ "$ACTION" = recover-lease ] || [ "$ACTION" = retire ] ||
  fail "--check is valid only with recover-lease or retire"
[ "$APPLY" = 0 ] || [ "$ACTION" = retire ] || fail "--apply is valid only with retire"
[ "$APPLY" = 0 ] || [ "$CHECK_ONLY" = 0 ] || fail "retire --apply and --check are mutually exclusive"
[ -z "$REUSE_PASS_RUN" ] || [ "$ACTION" = accept ] || fail "--reuse-pass requires accept"
if [ -n "$LANDED_COMMIT" ]; then
  [ "$ACTION" = finish ] || fail "--landed-commit requires finish"
  [ "$EXPECTED_LANDING_RECEIPT_SHA256_SET" = 0 ] && [ "$REFREEZE_ACCEPTANCE" = 0 ] ||
    fail "--landed-commit cannot be combined with typed patch-land finish options"
fi
PLAN_READ_ONLY=0
case "$ACTION" in
  show|evidence-snapshot|list|ready|status|brief) PLAN_READ_ONLY=1 ;;
  next) [ "$CLAIM" = 1 ] || PLAN_READ_ONLY=1 ;;
esac
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
REPO="$(cd "$REPO" && pwd -P)" || fail "cannot resolve the physical repository"
PLAN_FILE="${PLAN_FILE:-$REPO/.oms/plan/tasks.json}"
PLAN_LOCK_FILE=""
if [ -n "$PROVIDER" ]; then
  PROVIDER="$(oms_normalize_provider "$PROVIDER")" ||
    fail "unknown provider: inspect 'oms models' for registered transports"
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
  [ -n "${PLAN_KERNEL_NAME:-}" ] || PLAN_KERNEL_NAME="$(uname -s 2>/dev/null || true)"
  case "$PLAN_KERNEL_NAME" in
    MINGW*|MSYS*|CYGWIN*)
      command -v cygpath >/dev/null 2>&1 || return 2
      value="$(cygpath -m "$value" | tr -d '\r')" || return $?
      ;;
  esac
  printf '%s\n' "$value"
}

PY_REPO="$(python_path_for_host "$REPO")" || fail "cannot normalize repository path for Python"
if [ -n "$LANDED_COMMIT" ]; then
  # Match land.sh's shared-worktree state directory using shell path spelling;
  # native Windows Python paths must not change the cksum repository identity.
  common_dir="$(git -C "$REPO" rev-parse --git-common-dir | tr -d '\r')" || fail "cannot resolve land state directory"
  case "$common_dir" in
    /*|[A-Za-z]:/*) ;;
    *) common_dir="$REPO/$common_dir" ;;
  esac
  common_dir="$(cd "$common_dir" && pwd -P)" || fail "cannot resolve land common directory"
  common_root="$(cd "$common_dir/.." && pwd -P)" || fail "cannot resolve land common root"
  land_name="$(basename "$common_root")"
  land_name="$(printf '%s' "$land_name" | tr -c 'A-Za-z0-9._-' '_')"
  land_name="${land_name//../_}"
  case "$land_name" in ''|.|..) land_name=repo ;; esac
  land_digest="$(printf '%s' "$common_root" | cksum | awk '{print $1 "-" $2}')"
  case "$land_digest" in ''|*[!0-9-]*) fail "cannot resolve land state digest" ;; esac
  LAND_RECEIPTS_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/oh-my-setting/land/$land_name-$land_digest"
  LAND_RECEIPTS_DIR="$(python_path_for_host "$LAND_RECEIPTS_DIR")" || fail "cannot normalize land state path for Python"
fi
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
  init|retire|apply-proposal|add)
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

if [ "$ACTION" = retire ]; then
  [ -z "$PLAN_FILE" ] || [ "$PLAN_FILE" = "$REPO/.oms/plan/tasks.json" ] ||
    fail "retire is valid only for the canonical repo-local plan"
  case "$DISPOSITION" in
    ""|completed-external|superseded) ;;
    *) fail "--disposition must be completed-external or superseded" ;;
  esac
  if [ "$APPLY" = 1 ]; then
    case "$EXPECTED_PLAN_SHA256" in
      *[!0-9a-f]*|"") fail "retire --apply requires a lowercase --expected-plan-sha256" ;;
    esac
    [ "${#EXPECTED_PLAN_SHA256}" -eq 64 ] ||
      fail "--expected-plan-sha256 must be a lowercase SHA-256"
    [ -n "$DISPOSITION" ] || fail "retire --apply requires --disposition"
    [ -n "$REASON" ] || fail "retire --apply requires --reason"
  fi
  [ "${#REASON}" -le 500 ] || fail "retirement reason exceeds 500 characters"
  if [ -n "$REASON" ]; then
    scan="$(mktemp)" || fail "mktemp failed"
    printf '%s\n' "$REASON" > "$scan"
    if agent_memory_file_has_secret_content "$scan"; then
      rm -f "$scan"
      fail "retirement reason looks sensitive; keep credentials and machine paths out of shared state"
    fi
    rm -f "$scan"
  fi
  oms_git_assert_safe_execution_config "$REPO" ||
    fail "unsafe executable Git config is active"
  oms_git_assert_plain_index "$REPO" ||
    fail "hidden Git index flags are active"
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
  project_state="$(oms_project_state "$REPO/PROJECT.md")"
  case "$project_state" in
    confirmed|legacy-active) ;;
    *) fail "PROJECT.md State is $project_state; confirm one canonical State before plan topology is applied" ;;
  esac
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
  OMS_LANDED_COMMIT="$LANDED_COMMIT" OMS_LAND_RECEIPTS_DIR="$LAND_RECEIPTS_DIR" \
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
  OMS_CHECK_ONLY="$CHECK_ONLY" OMS_APPLY="$APPLY" \
  OMS_DISPOSITION="$DISPOSITION" OMS_RETIRE_PHASE="$RETIRE_PHASE"
export OMS_AUTOPILOT_OWNER_ID="$OWNER_ID" OMS_PLAN_MARKERS_DIR="$PY_MARKERS_DIR"
export OMS_TASK_ASSIGNMENT="$ASSIGNMENT"

plan_run() {
python3 "$ROOT/scripts/lib/agent-plan-engine.py" "$PROPOSAL" "$ROOT/scripts/lib/plan-receipt.py" \
  "$ROOT/scripts/lib/verify-floor-lint.py" \
  "$ROOT/scripts/lib/process_liveness.py" \
  "$ROOT/scripts/lib/plan-retire.py" \
  "$ROOT/scripts/lib/path_scope.py" \
  "$ROOT/scripts/lib/project-state.py" "$ROOT/scripts/lib/task-assignment.py" </dev/null
}

# Retirement judges the worker-marker set and plan bytes as one authority
# snapshot. Preserve the global lock order used by lease recovery so marker
# publication can never race between the liveness check and the plan CAS.
plan_run_with_plan_lock() {
  local parent base physical_parent
  if [ -z "$PLAN_LOCK_FILE" ]; then
    parent="$(dirname "$PLAN_FILE")"
    base="$(basename "$PLAN_FILE")"
    physical_parent="$(cd "$parent" 2>/dev/null && pwd -P)" ||
      fail "cannot resolve the physical plan lock parent"
    physical_parent="$(printf '%s' "$physical_parent" | tr -d '\r')"
    PLAN_LOCK_FILE="$physical_parent/$base"
  fi
  oms_with_file_lock "$PLAN_LOCK_FILE" plan_run
}
plan_run_with_marker_and_plan_locks() {
  oms_with_file_lock "$REPO/.oms/delegations/.marker-set-lock-target" \
    plan_run_with_plan_lock
}

if [ "$ACTION" = retire ]; then
  if [ "$APPLY" = 0 ]; then
    OMS_RETIRE_PHASE=check
    export OMS_RETIRE_PHASE
    plan_run_with_marker_and_plan_locks
    exit $?
  fi

  OMS_RETIRE_PHASE=preflight
  export OMS_RETIRE_PHASE
  retire_meta="$(plan_run_with_marker_and_plan_locks)" || exit $?
  retire_field() {  # JSON KEY
    printf '%s\n' "$retire_meta" | python3 -c '
import json, sys
row = json.load(sys.stdin)
value = row.get(sys.argv[1])
if sys.argv[1] == "plan_generation":
    print(json.dumps(value, sort_keys=True, separators=(",", ":")))
elif isinstance(value, bool):
    print("1" if value else "0")
elif value is not None:
    print(value)
' "$1" | tr -d '\r'
  }
  retire_status="$(retire_field status)" || fail "retirement preflight returned invalid JSON"
  if [ "$retire_status" = already-retired ]; then
    printf '%s\n' "$retire_meta"
    exit 0
  fi
  retire_head="$(retire_field head)" || fail "retirement preflight omitted HEAD"
  retire_ref="$(retire_field git_ref)" || fail "retirement preflight omitted ref"
  retire_generation="$(retire_field plan_generation)" ||
    fail "retirement preflight omitted plan generation"
  retire_proof="$(retire_field proof_id)" || fail "retirement preflight omitted proof token"
  retire_resume="$(retire_field resume)" || fail "retirement preflight omitted resume state"
  [ -n "$retire_head$retire_ref$retire_generation$retire_proof" ] ||
    fail "retirement preflight is incomplete"

  if [ "$DISPOSITION" = completed-external ] && [ "$retire_resume" != 1 ]; then
    OMS_PLAN_ACCEPT_PROOF_ID="$retire_proof" \
    OMS_PLAN_ACCEPT_EXPECTED_PLAN_SHA="$EXPECTED_PLAN_SHA256" \
    OMS_PLAN_ACCEPT_EXPECTED_HEAD="$retire_head" \
    OMS_PLAN_ACCEPT_EXPECTED_REF="$retire_ref" \
    OMS_PLAN_ACCEPT_GENERATION="$retire_generation" \
      "$ROOT/scripts/agent-plan.sh" --repo "$REPO" accept
  fi

  OMS_RETIRE_PHASE=finalize
  OMS_RETIRE_EXPECTED_HEAD="$retire_head"
  OMS_RETIRE_EXPECTED_REF="$retire_ref"
  OMS_RETIRE_EXPECTED_GENERATION="$retire_generation"
  OMS_RETIRE_PROOF_ID="$retire_proof"
  export OMS_RETIRE_PHASE OMS_RETIRE_EXPECTED_HEAD OMS_RETIRE_EXPECTED_REF \
    OMS_RETIRE_EXPECTED_GENERATION OMS_RETIRE_PROOF_ID
  plan_run_with_marker_and_plan_locks
  exit $?
fi

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
elif [ "$ACTION" != retire ] && [ "$PLAN_READ_ONLY" = 0 ]; then
  mkdir -p "$(dirname "$PLAN_FILE")"
  agent_memory_ensure_oms_ignore_for_path "$PLAN_FILE" 2>/dev/null || true
fi

# `accept` runs OUTSIDE the plan lock: the acceptance command is an arbitrary
# project check (often the full gate) and must not hold the task-graph lock
# for its whole runtime. It only reads the stored command, runs it from the
# repo root, and appends one receipt row — the executable answer to "is the
# goal actually met", which no per-task verify can give.
if [ "$ACTION" = "accept" ]; then
  # Hold the plan lock only for the residual-retirement guard. The arbitrary
  # acceptance command still runs outside it, and retirement's exact context
  # fence catches any plan change after this short admission check.
  plan_run_with_plan_lock >/dev/null
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
    current.append({"path": rel, "sha256": value, "mode": stat.S_IMODE(info.st_mode)})
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
git_env["GIT_OPTIONAL_LOCKS"] = "0"

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

head = subprocess.check_output(
    ["git", "-C", repo, "rev-parse", "HEAD"],
    stderr=subprocess.DEVNULL, env=git_env)
symbolic = subprocess.run(
    ["git", "-C", repo, "symbolic-ref", "-q", "HEAD"],
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=git_env,
    check=False)
if symbolic.returncode == 0:
    git_ref = symbolic.stdout.strip()
elif symbolic.returncode == 1:
    git_ref = b"DETACHED@" + head.strip()
else:
    raise SystemExit(2)
for value in (git_ref, head):
    total += len(value)
    if total > limit:
        raise SystemExit(2)
    digest.update(value)
    digest.update(b"\0")
command(["git", "-C", repo, "ls-files", "--stage", "-z"])
command(["git", "-c", "core.fsmonitor=false", "-c", "diff.external=",
         "-C", repo, "diff", "--no-ext-diff", "--no-textconv",
         "--binary", "HEAD", "--"])
# Git may ignore executable changes when core.filemode=false. A local
# acceptance can still observe them, including on manifest-bound files.
tracked = command(["git", "-C", repo, "ls-files", "-z"])
for raw in sorted(value for value in tracked.split(b"\0") if value):
    target = os.path.join(repo, os.fsdecode(raw))
    try:
        mode = os.lstat(target).st_mode
    except FileNotFoundError:
        mode = 0
    digest.update(raw + b"\0mode\0" + str(mode).encode() + b"\0")
untracked = command(["git", "-C", repo, "ls-files", "--others", "--exclude-standard", "-z"])
for raw in sorted(value for value in untracked.split(b"\0") if value):
    rel = os.fsdecode(raw)
    target = os.path.join(repo, rel)
    if os.path.commonpath((repo, os.path.realpath(target))) != repo:
        raise SystemExit(2)
    info = os.lstat(target)
    digest.update(raw + b"\0" + str(info.st_mode).encode() + b"\0")
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

  retire_accept_context_matches() {
    python3 "$ROOT/scripts/lib/plan-retire.py" accept-context \
      "$PY_REPO" "$PY_PLAN_FILE"
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
  retire_proof="${OMS_PLAN_ACCEPT_PROOF_ID:-}"
  if [ -n "$retire_proof" ]; then
    case "$retire_proof" in
      *[!0-9a-f]*|"")
        acceptance_integrity_error acceptance-retire-context-changed \
          "retirement acceptance proof token is malformed"
        ;;
    esac
    [ "${#retire_proof}" -eq 64 ] ||
      acceptance_integrity_error acceptance-retire-context-changed \
        "retirement acceptance proof token is malformed"
    retire_accept_context_matches ||
      acceptance_integrity_error acceptance-retire-context-changed \
        "plan, HEAD, ref, generation, or clean status changed before retirement acceptance"
  fi

  # Only a run-bound row can later be reused, and only a reuse request consumes
  # one; without either, the context proof is unusable, so skip its helpers.
  reuse_context=""
  reuse_bound=0
  [ -z "${OMS_GOAL_RUN_ID:-}$REUSE_PASS_RUN" ] || reuse_bound=1
  if [ "$reuse_bound" = 1 ]; then
    reuse_context="$(python3 "$ROOT/scripts/lib/acceptance-reuse.py" context "$REPO" "$0" 2>/dev/null || true)"
    reuse_context="${reuse_context//$'\r'/}"
  fi
  if [ -n "$REUSE_PASS_RUN" ] && [ -z "$retire_proof" ] && [ -n "$reuse_context" ]; then
    reuse_result="$(python3 "$ROOT/scripts/lib/acceptance-reuse.py" check "$progress" \
      "$REUSE_PASS_RUN" "$reuse_context" "$plan_before" "$repo_before" \
      "$manifest_before" "$accept_cmd" 2>/dev/null)" || reuse_result=""
    if [ -n "$reuse_result" ] &&
        [ "$(acceptance_repo_snapshot)" = "$repo_before" ] &&
        [ "$(oms_sha256_file "$PLAN_FILE")" = "$plan_before" ] &&
        [ "$(acceptance_manifest)" = "$manifest_before" ] &&
        [ "$(python3 "$ROOT/scripts/lib/acceptance-reuse.py" context "$REPO" "$0" | tr -d '\r')" = "$reuse_context" ]; then
      printf '%s\n' "${reuse_result//$'\r'/}"
      exit 0
    fi
  fi

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
  (
    # The retirement proof binds this accept process, not commands that the
    # repository's acceptance gate may invoke in independent temporary repos.
    # Keep the outer shell's copy for the post-run CAS, but do not leak it into
    # the supervised command tree.
    unset OMS_PLAN_ACCEPT_PROOF_ID OMS_PLAN_ACCEPT_EXPECTED_PLAN_SHA \
      OMS_PLAN_ACCEPT_EXPECTED_HEAD OMS_PLAN_ACCEPT_EXPECTED_REF \
      OMS_PLAN_ACCEPT_GENERATION
    exec python3 "$ROOT/scripts/lib/autopilot-receipt.py" supervise \
      --wall "$timeout_seconds" --kill-after 1 --label acceptance \
      --cwd "$REPO" --output "$out_tmp" --output-limit 1048576 \
      --metadata "$meta_tmp" -- bash -c "$accept_cmd"
  ) &
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
  elif [ -n "$retire_proof" ] && ! retire_accept_context_matches; then
    integrity_reason=acceptance-retire-context-changed
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
  git_ref="$(git -C "$REPO" symbolic-ref -q HEAD 2>/dev/null || true)"
  git_ref="${git_ref//$'\r'/}"
  [ -n "$git_ref" ] || git_ref="DETACHED@$base_sha"
  if [ "$reuse_bound" = 1 ]; then
    reuse_after="$(python3 "$ROOT/scripts/lib/acceptance-reuse.py" context "$REPO" "$0" 2>/dev/null || true)"
    reuse_after="${reuse_after//$'\r'/}"
    [ "$reuse_context" = "$reuse_after" ] || reuse_context=""
  fi
  OMS_PA_REUSE="$reuse_context" OMS_PA_MANIFEST="$manifest_before" \
  OMS_PA_TS="$ts" OMS_PA_SHA="$base_sha" OMS_PA_VERDICT="$verdict" \
    OMS_PA_EXIT="$accept_exit" OMS_PA_DIGEST="$out_digest" OMS_PA_DUR="$duration" \
    OMS_PA_ACCEPT="$accept_digest" OMS_PA_RUN="${OMS_GOAL_RUN_ID:-}" \
    OMS_PA_CYCLE="${OMS_GOAL_CYCLE:-}" OMS_PA_REASON="$integrity_reason" \
    OMS_PA_TIMEOUT="$accept_timed_out" OMS_PA_LIMITED="$accept_output_limited" \
    OMS_PA_PLAN="$plan_before" OMS_PA_REF="$git_ref" OMS_PA_REPO="$repo_before" \
    OMS_PA_RETIRE_PROOF="$retire_proof" \
    OMS_PA_RETIRE_GENERATION="${OMS_PLAN_ACCEPT_GENERATION:-}" \
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
    "plan_sha256": os.environ["OMS_PA_PLAN"],
    "git_ref": os.environ["OMS_PA_REF"],
    "accept_sha256_full": os.environ["OMS_PA_ACCEPT"],
    "output_sha256_full": os.environ["OMS_PA_DIGEST"],
    "repo_snapshot_sha256": os.environ["OMS_PA_REPO"],
    "reuse_context_sha256": os.environ["OMS_PA_REUSE"],
    "manifest_sha256": os.environ["OMS_PA_MANIFEST"],
}
if os.environ.get("OMS_PA_RUN"): row["run_id"] = os.environ["OMS_PA_RUN"]
if os.environ.get("OMS_PA_CYCLE"): row["cycle"] = int(os.environ["OMS_PA_CYCLE"])
if os.environ.get("OMS_PA_RETIRE_PROOF"):
    row["retire_proof_id"] = os.environ["OMS_PA_RETIRE_PROOF"]
    row["plan_generation"] = json.loads(os.environ["OMS_PA_RETIRE_GENERATION"])
    row["git_clean"] = True
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
case "$ACTION" in
  show|evidence-snapshot|list|ready|status|brief)
    plan_run
    ;;
  next)
    if [ "$CLAIM" = 1 ]; then
      plan_run_with_plan_lock
    else
      plan_run
    fi
    ;;
  recover-lease|recover-owner)
    plan_run_with_marker_and_plan_locks
    ;;
  *) plan_run_with_plan_lock ;;
esac
