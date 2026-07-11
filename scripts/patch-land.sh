#!/usr/bin/env bash
set -euo pipefail

# Land a delegated patch onto the main tree, safely. patch-admit is the trust
# boundary (does it apply, parse, not touch its own verifier, and verify?); this
# is the one mutating step that composes it: clean-tree check -> patch-admit
# ADMIT gate -> git apply -> record a land row in the artifact index -> optional
# agent-plan finish. Nothing lands unless admission passes and the tree is clean,
# and the land is recorded so "which patch was applied for task X" is answerable.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/peer-common.sh
. "$ROOT_LIB/peer-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

REPO="$PWD"
PATCH=""
VERIFY=""
ML=0
PLAN_TASK=""
EXECUTOR_ID=""
PLAN_LEASE_ID=""
PLAN_REVIEW_LEASE_ID=""
PLAN_STATE=""
PLAN_JSON=""
ALLOW_VERIFIER_CHANGE=0

usage() {
  cat <<'EOF'
Usage: patch-land.sh --patch FILE [options]
       patch-land.sh --plan-task ID [options]

Admit a delegated patch and, only if it passes, apply it to the main tree.

Options:
  --patch FILE     Patch file to land. May be omitted with --plan-task when
                   the plan task carries a stored patch path (delegate stamps
                   it on review/finish).
  --repo PATH      Target git repo (default: current directory).
  --verify CMD     Verification command for the admission gate (forwarded to
                   patch-admit.sh; default: scripts/check.sh when present).
  --ml             Prefer ml-smoke verification when auto-detecting.
  --plan-task ID   On a successful land, mark this agent-plan task done.
  --executor ID    Enforce a frozen/running executor soul and scope.
  --allow-verifier-change  Forward to patch-admit: permit a patch that touches
                   its own verifier (normally rejected).
  -h, --help       Show this help.

Sequence: main tree must be clean -> patch-admit returns ADMIT -> git apply
--binary -> land row appended to .oms/artifacts/index.jsonl. Exit is nonzero
(and nothing is applied) if the tree is dirty or admission fails. A rejection
is recorded in the shared fail-ledger (fingerprint = patch content hash) so a
later agent is warned before re-landing the same patch; a subsequent
successful land resolves it.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --patch) [ "$#" -ge 2 ] || fail "--patch requires a file"; PATCH="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires a command"; VERIFY="$2"; shift 2 ;;
    --ml) ML=1; shift ;;
    --plan-task)
      [ "$#" -ge 2 ] || fail "--plan-task requires id"
      case "$2" in
        *[!A-Za-z0-9._-]*|"") fail "--plan-task must match [A-Za-z0-9._-]+" ;;
      esac
      PLAN_TASK="$2"; shift 2 ;;
    --executor)
      [ "$#" -ge 2 ] || fail "--executor requires id"
      case "$2" in *[!A-Za-z0-9._-]*|"") fail "--executor must match [A-Za-z0-9._-]+" ;; esac
      EXECUTOR_ID="$2"; shift 2 ;;
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

REPO="$(cd "$REPO" && pwd)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"

if [ -n "$EXECUTOR_ID" ]; then
  "$ROOT/scripts/agent-executor.sh" validate --repo "$REPO" --id "$EXECUTOR_ID" >/dev/null ||
    fail "executor $EXECUTOR_ID failed frozen validation"
  executor_json="$($ROOT/scripts/agent-executor.sh show --repo "$REPO" --id "$EXECUTOR_ID")"
  executor_values="$(printf '%s' "$executor_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);print("\t".join([d.get("task_id",""),d.get("soul_sha256","")]))')"
  executor_task="$(printf '%s' "$executor_values" | cut -f1)"
  executor_soul_sha="$(printf '%s' "$executor_values" | cut -f2)"
  [ -z "$PLAN_TASK" ] || [ -z "$executor_task" ] || [ "$PLAN_TASK" = "$executor_task" ] ||
    fail "executor task conflicts with --plan-task"
  export OMS_EXECUTOR_ID="$EXECUTOR_ID" OMS_SOUL_SHA256="$executor_soul_sha"
fi

if [ -n "$PLAN_TASK" ]; then
  PLAN_JSON="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$PLAN_TASK" 2>/dev/null)" ||
    fail "cannot read plan task $PLAN_TASK"
  PLAN_LEASE_ID="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("lease_id", ""))')" ||
    fail "cannot read lease for plan task $PLAN_TASK"
  PLAN_REVIEW_LEASE_ID="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("review_lease_id", ""))')" ||
    fail "cannot read review lease for plan task $PLAN_TASK"
  PLAN_STATE="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))')" ||
    fail "cannot read state for plan task $PLAN_TASK"
fi

# --plan-task alone is enough when delegate already stamped the patch path on
# the task: read it back instead of making the reviewer copy it by hand.
if [ -z "$PATCH" ] && [ -n "$PLAN_TASK" ]; then
  PATCH="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys;print(json.load(sys.stdin).get("patch",""))' 2>/dev/null || true)"
  [ -n "$PATCH" ] || fail "--patch omitted and plan task $PLAN_TASK has no stored patch path"
  echo "patch-land: using patch from plan task $PLAN_TASK: $PATCH" >&2
fi
[ -n "$PATCH" ] || fail "--patch is required (or --plan-task with a stored patch)"
[ -z "$PLAN_TASK" ] || {
  [ "$PLAN_STATE" = "review" ] ||
    fail "plan task $PLAN_TASK is $PLAN_STATE, not review"
  [ -n "$PLAN_LEASE_ID" ] && [ "$PLAN_REVIEW_LEASE_ID" = "$PLAN_LEASE_ID" ] ||
    fail "plan task $PLAN_TASK has a stale review lease"
}
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"

# Landing decisions are the most expensive dead ends (a full delegate + admit
# cycle), so they feed the shared failure memory: fingerprint by patch content
# so the same rejected patch warns any later agent before it re-lands.
land_fingerprint_cmd() {
  local sha
  sha="$(oms_sha256_file "$PATCH" 2>/dev/null || true)"
  [ -n "$sha" ] || sha="$(basename "$PATCH")"
  printf 'patch-land %s' "$sha"
}
known_reject_fp=""
check_out="$( (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" check --cmd "$(land_fingerprint_cmd)") 2>&1 || true)"
case "$check_out" in
  *"already failed"*)
    echo "warning: this exact patch was rejected before:" >&2
    echo "  $check_out" >&2
    known_reject_fp="$(printf '%s' "$check_out" | awk '{print $2; exit}')"
    ;;
esac

# Pre-flight: never apply onto a dirty tree — a half-applied patch on top of
# unrelated edits is the mess this whole gate exists to avoid.
if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=no)" ]; then
  fail "refusing to land: main tree has uncommitted changes"
fi

# --- Admission gate (the trust boundary) ------------------------------------
admit_cmd=("$ROOT/scripts/patch-admit.sh" --patch "$PATCH" --repo "$REPO")
[ -n "$VERIFY" ] && admit_cmd+=(--verify "$VERIFY")
[ "$ML" = 1 ] && admit_cmd+=(--ml)
[ -n "$PLAN_TASK" ] && admit_cmd+=(--plan-task "$PLAN_TASK")
[ -n "$EXECUTOR_ID" ] && admit_cmd+=(--executor "$EXECUTOR_ID")
[ "$ALLOW_VERIFIER_CHANGE" = 1 ] && admit_cmd+=(--allow-verifier-change)
if ! "${admit_cmd[@]}" >/dev/null; then
  echo "patch-land: REJECTED by admission gate; not applied" >&2
  # Durable record: without this a later agent re-runs the whole delegate +
  # admit cycle for a patch already known to fail.
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record \
    --kind patch-land --cmd "$(land_fingerprint_cmd)" --exit 1 \
    --summary "patch-land REJECT: $(basename "$PATCH")${PLAN_TASK:+ (plan task $PLAN_TASK)}") >&2 ||
    echo "warning: patch-land: could not record rejection in fail-ledger" >&2
  exit 1
fi

# Fence the reviewed claim immediately before the irreversible apply. A stale
# reviewer cannot land after reclaim/re-claim because its captured lease no
# longer matches, and landing tasks are not eligible for dead-worker reclaim.
if [ -n "$PLAN_TASK" ]; then
  land_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" land --id "$PLAN_TASK")
  [ -n "$PLAN_LEASE_ID" ] && land_cmd+=(--lease-id "$PLAN_LEASE_ID")
  if ! "${land_cmd[@]}" >/dev/null; then
    echo "patch-land: plan task lease/state changed; not applied" >&2
    exit 1
  fi
fi

# --- Apply ------------------------------------------------------------------
if ! git -C "$REPO" apply --binary "$PATCH"; then
  if [ -n "$PLAN_TASK" ]; then
    release_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" release --id "$PLAN_TASK")
    [ -n "$PLAN_LEASE_ID" ] && release_cmd+=(--lease-id "$PLAN_LEASE_ID")
    "${release_cmd[@]}" >/dev/null 2>&1 ||
      echo "warning: patch-land: apply failed and plan task could not be released" >&2
  fi
  fail "admission passed but git apply failed (base moved?); tree unchanged"
fi

changed_files="$(git -C "$REPO" apply --numstat "$PATCH" 2>/dev/null | awk -F '\t' '{print $3}' | tr '\n' ' ' | sed 's/ *$//')"
echo "patch-land: applied $PATCH" >&2
[ -n "$changed_files" ] && echo "patch-land: changed $changed_files" >&2

# Record the land so "what patch was applied for task X" is answerable. Stamp
# the plan/task id via OMS_TASK_ID so the row carries lineage. A failed record
# does not unwind the land, but it must be loud: a silent miss here makes the
# lineage unanswerable exactly when something already went wrong.
[ -n "$PLAN_TASK" ] && export OMS_TASK_ID="$PLAN_TASK"
if ! ma_append_artifact_index "$REPO" patch-land "" 0 "" "$PATCH"; then
  echo "warning: patch-land: patch applied but the land row could NOT be recorded" >&2
  echo "warning: patch-land: lineage for $PATCH is missing from .oms/artifacts/index.jsonl" >&2
fi

# The patch was rejected before but lands now: resolve the fingerprint so the
# shared failure memory stops warning about it.
if [ -n "$known_reject_fp" ]; then
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" resolve --fingerprint "$known_reject_fp") >&2 || true
fi

# --- Optional plan lifecycle ------------------------------------------------
if [ -n "$PLAN_TASK" ]; then
  finish_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" finish --id "$PLAN_TASK" --patch "$PATCH")
  [ -n "$PLAN_LEASE_ID" ] && finish_cmd+=(--lease-id "$PLAN_LEASE_ID")
  if "${finish_cmd[@]}" >/dev/null 2>&1; then
    echo "patch-land: plan task $PLAN_TASK -> done" >&2
  else
    echo "warning: could not finish plan task $PLAN_TASK (wrong state?)" >&2
  fi
fi

echo "LANDED"
