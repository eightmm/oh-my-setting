#!/usr/bin/env bash
set -euo pipefail

# Land a delegated patch onto the main tree, safely. patch-admit is the trust
# boundary (does it apply, parse, not touch its own verifier, and verify?); this
# is the one mutating step that composes it: clean-tree check -> patch-admit
# ADMIT gate -> git apply -> record a land row in the artifact index -> optional
# agent-plan finish. Nothing lands unless admission passes and the tree is clean,
# and the land is recorded so "which patch was applied for task X" is answerable.
#
# Apply, lineage, and plan completion are three writes that must all happen. A
# crash between them left a modified tree with no lineage row and a task stuck
# in landing, indistinguishable from "nothing happened". Each land therefore
# writes an intent row to .oms/landings.jsonl before touching the tree and a
# completion row after the last write; `--recover` finishes or abandons an
# interrupted one, and `oms state` shows that one is outstanding.

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
PATCH_FROM_PLAN=0
VERIFY=""
VERIFY_EXPLICIT=0
ML=0
PLAN_TASK=""
EXECUTOR_ID=""
PLAN_ID=""
PLAN_LEASE_ID=""
PLAN_REVIEW_LEASE_ID=""
PLAN_STATE=""
PLAN_JSON=""
PLAN_REVIEW_PATCH=""
PLAN_REVIEW_PATCH_STORED=""
PLAN_REVIEW_PATCH_SHA=""
PLAN_REVIEW_RECEIPT_SHA=""
PLAN_DONE_RECEIPT_SHA=""
PLAN_REVIEW_EXECUTOR_ID=""
PLAN_REVIEW_EXECUTOR_SOUL_SHA=""
PLAN_VERIFY=""
PLAN_VERIFY_RAW=""
ALLOW_VERIFIER_CHANGE=0
ALLOW_TEST_REDUCTION=0
ALLOW_RESTRUCTURE=0
RECOVER=0
LANDING_ID=""
REQUEST_APPROVAL=0
APPROVAL_ID=""
APPROVAL_GRANT=""
APPROVAL_VERSION=""
APPROVAL_CONSUMING_VERSION=""
SOURCE_PATCH=""
FROZEN_PATCH=""
FROZEN_PATCH_PERSIST=0
LANDING_BASE=""
executor_soul_sha=""
intent_plan_receipt_sha=""
intent_plan_done_receipt_sha=""
intent_plan_id=""
intent_patch_sha=""
intent_base_sha=""
intent_receipt_sha=""
intent_has_complete=0
intent_has_abandoned=0

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
  --allow-test-reduction  Forward to patch-admit: permit a patch that
                   net-removes test assertions (normally rejected).
  --allow-restructure  Forward to patch-admit: permit an unscoped patch that
                   adds top-level files or moves files across directories
                   (normally rejected by the structural floor).
  --request-approval  Create an exact, expiring landing approval request and
                   apply nothing.
  --approval ID    Consume this approved patch-land request.
  --approval-token TOKEN  One-time grant from approval-inbox decide.
  --approval-version N    Approved request version used for compare-and-set.
  --recover        Finish or abandon landings interrupted by a crash: for each
                   outstanding intent, prove whether the patch is applied or
                   still cleanly applicable,
                   then record the missing lineage and plan completion (or mark
                   it abandoned). If neither direction is clean, leave the
                   ambiguous intent for manual recovery. Idempotent; applies
                   nothing new. A patch whose content changed since its intent
                   is also left untouched and makes the command exit nonzero.
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
    --verify)
      [ "$#" -ge 2 ] || fail "--verify requires a command"
      VERIFY="$2"; VERIFY_EXPLICIT=1; shift 2 ;;
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
    --allow-test-reduction) ALLOW_TEST_REDUCTION=1; shift ;;
    --allow-restructure) ALLOW_RESTRUCTURE=1; shift ;;
    --request-approval) REQUEST_APPROVAL=1; shift ;;
    --approval) [ "$#" -ge 2 ] || fail "--approval requires id"; APPROVAL_ID="$2"; shift 2 ;;
    --approval-token) [ "$#" -ge 2 ] || fail "--approval-token requires a token"; APPROVAL_GRANT="$2"; shift 2 ;;
    --approval-version) [ "$#" -ge 2 ] || fail "--approval-version requires a number"; APPROVAL_VERSION="$2"; shift 2 ;;
    --recover) RECOVER=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

# Landing is parent authority, like goal-drive and autopilot, and this is the
# only verb that writes the owner's tree. A delegated worker runs with
# OMS_HARNESS_CHILD=1 inside a throwaway worktree, but git metadata still names
# the primary checkout, so --repo was the only thing between that worker and
# this tree; the worker-authority guard notices such a write after the bytes
# are already applied. Refuse before them. patch-admit stays open to children:
# reading and judging a patch is not landing it.
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] ||
  fail "patch-land is parent-only; a harness child cannot land onto the owner's tree"

REPO="$(cd "$REPO" && pwd -P)" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"

LANDINGS="$REPO/.oms/landings.jsonl"

landing_append() {  # landing_append EVENT [KEY=VALUE...]
  local event="$1"
  shift
  mkdir -p "$(dirname "$LANDINGS")"
  agent_memory_ensure_oms_ignore "$REPO" 2>/dev/null || true
  OMS_LD_EVENT="$event" OMS_LD_ID="$LANDING_ID" OMS_LD_FILE="$LANDINGS" \
    OMS_LD_PATCH="$intent_patch" OMS_LD_PATCH_SHA="$intent_patch_sha" \
    OMS_LD_BASE_SHA="$intent_base_sha" OMS_LD_TASK="$intent_task" \
    OMS_LD_PLAN_ID="$intent_plan_id" \
    OMS_LD_LEASE="$intent_lease" OMS_LD_PLAN_RECEIPT_SHA="$intent_plan_receipt_sha" \
    OMS_LD_PLAN_DONE_RECEIPT_SHA="$intent_plan_done_receipt_sha" \
    OMS_LD_APPROVAL="$intent_approval" \
    OMS_LD_APPROVAL_VERSION="$intent_approval_version" \
    OMS_LD_RECEIPT_SHA="$intent_receipt_sha" \
    python3 - "$@" <<'PY'
import json, os, re, sys, time

row = {
    "schema": 1,
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "landing_id": os.environ["OMS_LD_ID"],
    "event": os.environ["OMS_LD_EVENT"],
    "patch": os.environ["OMS_LD_PATCH"],
    "patch_sha": os.environ["OMS_LD_PATCH_SHA"],
    "base_sha": os.environ["OMS_LD_BASE_SHA"],
    "task": os.environ["OMS_LD_TASK"],
    "lease": os.environ["OMS_LD_LEASE"],
    "plan_receipt_sha": os.environ["OMS_LD_PLAN_RECEIPT_SHA"],
    "plan_done_receipt_sha": os.environ["OMS_LD_PLAN_DONE_RECEIPT_SHA"],
    "approval": os.environ["OMS_LD_APPROVAL"],
    "approval_version": os.environ["OMS_LD_APPROVAL_VERSION"],
    "receipt_sha": os.environ["OMS_LD_RECEIPT_SHA"],
}
plan_id = os.environ.get("OMS_LD_PLAN_ID", "")
if plan_id:
    if not re.fullmatch(r"plan_[0-9a-f]{32}", plan_id):
        raise SystemExit("invalid landing plan lineage")
    row["plan_id"] = plan_id
for pair in sys.argv[1:]:
    key, _, value = pair.partition("=")
    if key and value:
        row[key] = value
path = os.environ["OMS_LD_FILE"]
with open(path, "a", encoding="utf-8") as f:
    f.write(json.dumps(row, ensure_ascii=False) + "\n")
    f.flush()
    os.fsync(f.fileno())
try:
    directory_fd = os.open(os.path.dirname(path), os.O_RDONLY)
except OSError:
    directory_fd = None
if directory_fd is not None:
    try:
        os.fsync(directory_fd)
    except OSError:
        pass
    finally:
        os.close(directory_fd)
PY
}

landing_receipt_hash() {
  OMS_LD_ID="$LANDING_ID" OMS_LD_PATCH="$intent_patch" \
    OMS_LD_PATCH_SHA="$intent_patch_sha" OMS_LD_BASE_SHA="$intent_base_sha" \
    OMS_LD_TASK="$intent_task" OMS_LD_LEASE="$intent_lease" \
    OMS_LD_PLAN_RECEIPT_SHA="$intent_plan_receipt_sha" \
    OMS_LD_PLAN_DONE_RECEIPT_SHA="$intent_plan_done_receipt_sha" \
    OMS_LD_APPROVAL="$intent_approval" \
    OMS_LD_APPROVAL_VERSION="$intent_approval_version" python3 <<'PY' | tr -d '\r'
import hashlib, json, os
row = {
    "schema": 1,
    "landing_id": os.environ["OMS_LD_ID"],
    "patch": os.environ["OMS_LD_PATCH"],
    "patch_sha": os.environ["OMS_LD_PATCH_SHA"],
    "base_sha": os.environ["OMS_LD_BASE_SHA"],
    "task": os.environ["OMS_LD_TASK"],
    "lease": os.environ["OMS_LD_LEASE"],
    "plan_receipt_sha": os.environ["OMS_LD_PLAN_RECEIPT_SHA"],
    "plan_done_receipt_sha": os.environ["OMS_LD_PLAN_DONE_RECEIPT_SHA"],
    "approval": os.environ["OMS_LD_APPROVAL"],
    "approval_version": os.environ["OMS_LD_APPROVAL_VERSION"],
}
raw = json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
print(hashlib.sha256(raw.encode()).hexdigest())
PY
}

# Every first intent, annotated with terminal hints. Terminal rows are appendable
# worker-visible evidence, not authority: recovery still proves the patch/tree
# and external receipts before treating either outcome as closed. plan_id stays
# outside the legacy canonical hash for compatibility, but terminal matching
# requires its exact optional value and recovery carries it from the first intent.
landing_outstanding() {
  [ -f "$LANDINGS" ] || return 0
  OMS_LD_FILE="$LANDINGS" python3 <<'PY'
import hashlib, json, os

receipt_fields = (
    "landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
    "plan_receipt_sha", "plan_done_receipt_sha", "approval",
    "approval_version",
)

def canonical(row):
    values = {"schema": 1}
    for name in receipt_fields:
        value = row.get(name, "")
        if not isinstance(value, str):
            raise ValueError(name)
        values[name] = value
    raw = json.dumps(values, ensure_ascii=False, sort_keys=True,
                     separators=(",", ":"))
    return values, hashlib.sha256(raw.encode()).hexdigest()

intents = {}
order = []
terminals = {}
with open(os.environ["OMS_LD_FILE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except Exception:
            continue
        if not isinstance(row, dict):
            continue
        lid = row.get("landing_id")
        if not lid:
            continue
        if row.get("event") == "intent":
            if lid not in intents:
                order.append(lid)
                intents[lid] = row
        elif row.get("event") in ("complete", "abandoned"):
            terminals.setdefault(lid, []).append(row)
for lid in order:
    row = dict(intents[lid])
    try:
        expected, digest = canonical(row)
    except (TypeError, ValueError):
        expected, digest = None, ""
    exact = set()
    if expected is not None:
        for terminal in terminals.get(lid, []):
            if terminal.get("schema") != 1:
                continue
            try:
                terminal_values, terminal_digest = canonical(terminal)
            except (TypeError, ValueError):
                continue
            if (terminal_values == expected and terminal_digest == digest
                    and terminal.get("plan_id", "") == row.get("plan_id", "")
                    and terminal.get("receipt_sha") == digest):
                exact.add(terminal.get("event"))
    row["_receipt_sha"] = digest
    row["_has_complete"] = "complete" in exact
    row["_has_abandoned"] = "abandoned" in exact
    print(json.dumps(row, ensure_ascii=False))
PY
}

approval_recover_finish() {  # approval_recover_finish ID VERSION consumed|failed
  local approval="$1"
  local version="$2"
  local result="$3"
  local row state current_version
  [ -n "$approval" ] && [ -n "$version" ] || return 0
  row="$("$ROOT/scripts/approval-inbox.sh" --repo "$REPO" show --approval "$approval" --json 2>/dev/null)" || return 1
  state="$(printf '%s' "$row" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("state",""))')"
  current_version="$(printf '%s' "$row" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("version",""))')"
  [ "$state" != "$result" ] || return 0
  # A crash before begin-consume performed no effect and leaves an approved
  # grant reusable. Only a reserved grant needs a terminal recovery row.
  [ "$result:$state" != "failed:approved" ] || return 0
  [ "$state" = consuming ] && [ "$current_version" = "$version" ] || return 1
  "$ROOT/scripts/approval-inbox.sh" --repo "$REPO" finish-consume \
    --approval "$approval" --expected-version "$version" --result "$result" \
    --consumer patch-land-recovery >/dev/null
}

approval_terminal_converged() {  # approval_terminal_converged complete|abandoned
  local outcome="$1"
  local row
  [ -n "$intent_approval" ] || return 0
  [ -n "$intent_approval_version" ] || return 1
  row="$("$ROOT/scripts/approval-inbox.sh" --repo "$REPO" show \
    --approval "$intent_approval" --json 2>/dev/null)" || return 1
  OMS_LD_APPROVAL_ROW="$row" OMS_LD_OUTCOME="$outcome" \
    OMS_LD_APPROVAL_VERSION="$intent_approval_version" \
    OMS_LD_PATCH_SHA="$intent_patch_sha" OMS_LD_BASE_SHA="$intent_base_sha" \
    OMS_LD_TASK="$intent_task" OMS_LD_LEASE="$intent_lease" python3 <<'PY'
import json, os
try:
    row = json.loads(os.environ["OMS_LD_APPROVAL_ROW"])
    version = int(os.environ["OMS_LD_APPROVAL_VERSION"])
except (TypeError, ValueError):
    raise SystemExit(1)
if not isinstance(row, dict):
    raise SystemExit(1)
bound = (
    row.get("patch_sha", "") == os.environ["OMS_LD_PATCH_SHA"],
    row.get("base_sha", "") == os.environ["OMS_LD_BASE_SHA"],
    row.get("task_id", "") == os.environ["OMS_LD_TASK"],
    row.get("lease_id", "") == os.environ["OMS_LD_LEASE"],
)
if not all(bound):
    raise SystemExit(1)
if os.environ["OMS_LD_OUTCOME"] == "complete":
    ok = row.get("state") == "consumed" and row.get("version") == version + 1
else:
    ok = (
        row.get("state") == "failed" and row.get("version") == version + 1
    ) or (
        row.get("state") == "approved" and row.get("version") == version - 1
    ) or (
        # A rejected attempt leaves its approved grant reusable. A later exact
        # landing may consume it, or time may expire it; those later durable
        # states do not reopen the earlier abandoned landing receipt.
        row.get("state") == "consumed" and row.get("version") == version + 1
    ) or (
        row.get("state") == "expired" and row.get("version") == version
    )
raise SystemExit(0 if ok else 1)
PY
}

plan_receipt_values() {  # plan_receipt_values TASK
  local task_json
  task_json="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$1" 2>/dev/null)" || return 1
  printf '%s' "$task_json" | python3 -c '
import json,runpy,sys
d=json.load(sys.stdin)
state=d.get("state", "")
lease=d.get("lease_id", "")
digest=runpy.run_path(sys.argv[1])["digest"](d)
print(state + "\t" + lease + "\t" + digest)
' "$ROOT/scripts/lib/plan-receipt.py" | tr -d '\r'
}

plan_complete_receipt_converged() {
  local values state lease receipt_sha
  [ -n "$intent_task" ] || return 0
  [ -n "$intent_plan_done_receipt_sha" ] || return 1
  values="$(plan_receipt_values "$intent_task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  lease="$(printf '%s' "$values" | cut -f2)"
  receipt_sha="$(printf '%s' "$values" | cut -f3)"
  [ "$state" = "done" ] && [ "$lease" = "$intent_lease" ] &&
    [ "$receipt_sha" = "$intent_plan_done_receipt_sha" ]
}

plan_landing_receipt_converged() {
  local values state lease receipt_sha
  [ -n "$intent_task" ] || return 0
  [ -n "$intent_plan_receipt_sha" ] || return 1
  values="$(plan_receipt_values "$intent_task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  lease="$(printf '%s' "$values" | cut -f2)"
  receipt_sha="$(printf '%s' "$values" | cut -f3)"
  [ "$state" = "landing" ] && [ "$lease" = "$intent_lease" ] &&
    [ "$receipt_sha" = "$intent_plan_receipt_sha" ]
}

plan_superseding_landing_receipt_converged() {
  local values state lease receipt_sha
  [ -n "$intent_task" ] || return 1
  [ -n "$intent_plan_receipt_sha" ] || return 1
  values="$(plan_receipt_values "$intent_task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  lease="$(printf '%s' "$values" | cut -f2)"
  receipt_sha="$(printf '%s' "$values" | cut -f3)"
  # A same-lease replacement can already own `landing` when recovery reaches
  # an older intent. Result bytes are not identity: two reviews can produce the
  # same tree while carrying different artifact/patch receipts.
  [ "$state" = "landing" ] && [ "$lease" = "$intent_lease" ] &&
    [ "$receipt_sha" != "$intent_plan_receipt_sha" ]
}

plan_superseding_done_receipt_converged() {
  local values state lease receipt_sha
  [ -n "$intent_task" ] || return 1
  [ -n "$intent_plan_done_receipt_sha" ] || return 1
  values="$(plan_receipt_values "$intent_task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  lease="$(printf '%s' "$values" | cut -f2)"
  receipt_sha="$(printf '%s' "$values" | cut -f3)"
  # `done` alone is not authority for this landing. The same lease can publish
  # a replacement review and complete that different receipt while an older
  # intent is outstanding. Preserve that durable winner instead of borrowing
  # its state to create lineage for the stale intent.
  [ "$state" = "done" ] && [ "$lease" = "$intent_lease" ] &&
    [ "$receipt_sha" != "$intent_plan_done_receipt_sha" ]
}

plan_abandoned_receipt_converged() {
  local values state lease receipt_sha
  [ -n "$intent_task" ] || return 0
  [ -n "$intent_plan_receipt_sha" ] || return 1
  values="$(plan_receipt_values "$intent_task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  lease="$(printf '%s' "$values" | cut -f2)"
  receipt_sha="$(printf '%s' "$values" | cut -f3)"
  if [ "$state" = ready ] && [ -z "$lease" ]; then
    return 0
  fi
  case "$state" in review|landing|claimed|running|blocked)
    [ "$receipt_sha" != "$intent_plan_receipt_sha" ]
    return
    ;;
    done)
      [ -n "$intent_plan_done_receipt_sha" ] &&
        [ "$lease" = "$intent_lease" ] &&
        [ "$receipt_sha" != "$intent_plan_done_receipt_sha" ]
      return
      ;;
  esac
  return 1
}

complete_terminal_converged() {
  landing_lineage_exists "$intent_patch" "$intent_patch_sha" \
    "$intent_task" "$intent_plan_id" || return 1
  plan_complete_receipt_converged || return 1
  approval_terminal_converged complete
}

abandoned_terminal_converged() {
  plan_abandoned_receipt_converged || return 1
  approval_terminal_converged abandoned
}

plan_release_recover() {  # plan_release_recover TASK LEASE REVIEW_RECEIPT_SHA [PRESERVE]
  local task="$1"
  local lease="$2"
  local expected_receipt_sha="${3:-}"
  local preserve="${4:-0}"
  local release_cmd values state current_lease current_receipt_sha
  [ -n "$task" ] || return 0
  [ "$preserve" != 1 ] || return 0
  values="$(plan_receipt_values "$task")" || return 1
  state="$(printf '%s' "$values" | cut -f1)"
  current_lease="$(printf '%s' "$values" | cut -f2)"
  current_receipt_sha="$(printf '%s' "$values" | cut -f3)"

  case "$state" in
    ready)
      [ -z "$current_lease" ]
      return
      ;;
    review|landing)
      # Only the exact review object frozen into the intent may be released.
      # A same-lease repair can replace patch/artifact evidence without changing
      # the lease or even enter its own landing state; that newer receipt is
      # authoritative and must survive.
      if [ -z "$expected_receipt_sha" ] ||
        [ "$current_receipt_sha" != "$expected_receipt_sha" ]; then
        [ -n "$expected_receipt_sha" ] && return 0
        return 1
      fi
      ;;
    claimed|running|blocked)
      # A differing receipt is a later repair/terminal state. Preserve it. An
      # exact review receipt cannot legitimately have one of these states.
      [ -n "$expected_receipt_sha" ] || return 1
      [ "$current_receipt_sha" != "$expected_receipt_sha" ] || return 1
      return 0
      ;;
    *) return 1 ;;
  esac

  release_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" release --id "$task")
  [ -n "$lease" ] && release_cmd+=(--lease-id "$lease")
  "${release_cmd[@]}" >/dev/null 2>&1
}

finish_not_applied_receipts() {  # finish_not_applied_receipts REASON [PRESERVE_PLAN]
  local reason="$1"
  local preserve_plan="${2:-0}"
  local receipts_ok=1
  if ! approval_recover_finish "$intent_approval" "$intent_approval_version" failed; then
    receipts_ok=0
    echo "warning: patch-land: could not finalize unapplied approval ${intent_approval:-none}" >&2
  fi
  if [ "$preserve_plan" != 1 ]; then
    if ! plan_release_recover "$intent_task" "$intent_lease" \
      "$intent_plan_receipt_sha"; then
      receipts_ok=0
      echo "warning: patch-land: could not release unapplied plan task ${intent_task:-none}" >&2
    fi
  fi
  if [ "$receipts_ok" = 1 ]; then
    if [ "$intent_has_abandoned" != 1 ]; then
      landing_append abandoned "reason=$reason" || return $?
    fi
    return 0
  fi
  landing_append not-applied-pending-receipt "reason=$reason" || true
  return 1
}

finish_superseded_plan_receipts() {  # REASON
  local reason="$1"
  local receipts_ok=1
  # A replacement exact done receipt owns the plan task. Never release or
  # rewrite it. The same applies while that replacement still owns `landing`.
  # Only converge this intent's approval side, then bind a canonical abandoned
  # terminal to the stale receipt.
  if ! approval_terminal_converged abandoned; then
    if ! approval_recover_finish "$intent_approval" "$intent_approval_version" failed; then
      receipts_ok=0
      echo "warning: patch-land: could not finalize superseded approval ${intent_approval:-none}" >&2
    fi
  fi
  if ! approval_terminal_converged abandoned; then
    receipts_ok=0
  fi
  if [ "$receipts_ok" = 1 ]; then
    if [ "$intent_has_abandoned" != 1 ]; then
      landing_append abandoned "reason=$reason" || return $?
    fi
    return 0
  fi
  landing_append not-applied-pending-receipt "reason=$reason" || true
  return 1
}

landing_lineage_exists() {  # landing_lineage_exists PATCH SHA256 TASK PLAN_ID
  local patch="$1"
  local digest="$2"
  local task="${3:-}"
  local plan_id="${4:-}"
  local index="${OMS_ARTIFACT_INDEX:-$REPO/.oms/artifacts/index.jsonl}"
  [ -f "$index" ] && [ -n "$digest" ] && [ "$digest" != unknown ] || return 1
  python3 - "$REPO" "$index" "$patch" "$digest" "$task" "$plan_id" <<'PY'
import json, os, sys

repo = os.path.abspath(sys.argv[1])
index = os.path.abspath(sys.argv[2])
patch = os.path.abspath(sys.argv[3])
digest = sys.argv[4]
task_id = sys.argv[5]
plan_id = sys.argv[6]
try:
    real_repo = os.path.realpath(repo)
    real_patch = os.path.realpath(patch)
    if os.path.commonpath([real_repo, real_patch]) != real_repo:
        raise SystemExit(1)
    relative = os.path.relpath(patch, repo)
except (OSError, ValueError):
    raise SystemExit(1)

try:
    with open(index, encoding="utf-8", errors="replace") as handle:
        found = 0
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except (TypeError, ValueError):
                print("corrupt artifact index row %d" % line_number, file=sys.stderr)
                raise SystemExit(2)
            if not isinstance(row, dict):
                print("non-object artifact index row %d" % line_number, file=sys.stderr)
                raise SystemExit(2)
            for key in ("artifact", "patch", "source"):
                if key in row and row[key] not in (None, "") and not isinstance(row[key], str):
                    print("invalid artifact path at row %d" % line_number, file=sys.stderr)
                    raise SystemExit(2)
            exit_value = row.get("exit")
            if (
                isinstance(exit_value, int)
                and not isinstance(exit_value, bool)
                and exit_value == 0
                and row.get("kind") == "patch-land"
                and row.get("patch") == relative
                and row.get("patch_sha256") == digest
                and row.get("task_id", "") == task_id
                and row.get("plan_id", "") == plan_id
            ):
                found += 1
        if found > 1:
            print("duplicate exact patch-land lineage rows", file=sys.stderr)
            raise SystemExit(2)
        if found == 1:
            raise SystemExit(0)
except OSError as exc:
    print("cannot read artifact index: %s" % exc, file=sys.stderr)
    raise SystemExit(2)
raise SystemExit(1)
PY
}

record_landing_lineage_once() {  # record_landing_lineage_once PATCH SHA TASK PLAN_ID
  local patch="$1"
  local digest="$2"
  local task="$3"
  local plan_id="${4:-}"
  local status
  if landing_lineage_exists "$patch" "$digest" "$task" "$plan_id"; then
    return 0
  else
    status=$?
  fi
  [ "$status" = 1 ] || return "$status"
  OMS_TASK_ID="$task" OMS_INDEX_PLAN_ID="$plan_id" \
    ma_append_artifact_index "$REPO" patch-land "" 0 "" "$patch"
}

# Recovery is a bookkeeping pass, never a second apply: it decides from the
# tree whether the interrupted land happened, then writes the records that the
# crash skipped. Running it twice changes nothing the first run did not.
if [ "$RECOVER" = 1 ]; then
  # Recovery observes and mutates the same intent/tree/receipt transaction as
  # a live landing. It must never decide that a not-yet-applied live intent was
  # abandoned, and two recoveries must not write duplicate receipts.
  if ! oms_hold_file_lock "$REPO/.oms/landings.jsonl"; then
    fail "another landing or recovery is in progress in $REPO; retry when it finishes"
  fi
  trap 'oms_release_held_file_lock' EXIT
  recovered=0
  abandoned=0
  needs_manual=0
  while IFS= read -r intent_row; do
    [ -n "$intent_row" ] || continue
    LANDING_ID="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("landing_id",""))')"
    intent_patch="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("patch",""))')"
    intent_task="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("task",""))')"
    intent_plan_id="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("plan_id",""))' | tr -d '\r')"
    intent_lease="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("lease",""))')"
    intent_plan_receipt_sha="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("plan_receipt_sha",""))' | tr -d '\r')"
    intent_plan_done_receipt_sha="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("plan_done_receipt_sha",""))' | tr -d '\r')"
    intent_patch_sha="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("patch_sha",""))' | tr -d '\r')"
    intent_base_sha="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("base_sha",""))' | tr -d '\r')"
    intent_receipt_sha="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("_receipt_sha",""))' | tr -d '\r')"
    intent_has_complete="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(1 if json.load(sys.stdin).get("_has_complete") is True else 0)' | tr -d '\r')"
    intent_verifier_consent="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(1 if json.load(sys.stdin).get("verifier_change_consent") == "true" else 0)' | tr -d '\r')"
    intent_has_abandoned="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(1 if json.load(sys.stdin).get("_has_abandoned") is True else 0)' | tr -d '\r')"
    intent_approval="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("approval",""))')"
    intent_approval_version="$(printf '%s' "$intent_row" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("approval_version",""))')"
    if [ -z "$intent_patch" ] || [ ! -f "$intent_patch" ]; then
      echo "error: patch-land: landing $LANDING_ID has no readable frozen patch" >&2
      echo "error: patch-land: outcome cannot be proven; preserve the intent for manual recovery" >&2
      needs_manual=$((needs_manual + 1))
      continue
    fi
    # Recovery decides from the tree whether THIS patch went in. A patch file
    # that has changed since the intent cannot answer that question, and
    # guessing either way writes a false record: leave it for a human.
    current_patch_sha="$(oms_sha256_file "$intent_patch" 2>/dev/null || printf 'unknown')"
    if [ -n "$intent_patch_sha" ] && [ "$intent_patch_sha" != "unknown" ] &&
      [ "$intent_patch_sha" != "$current_patch_sha" ]; then
      echo "error: landing $LANDING_ID: the patch changed since the intent was recorded" >&2
      echo "error: recorded $intent_patch_sha, now $current_patch_sha; recover this one by hand" >&2
      needs_manual=$((needs_manual + 1))
      continue
    fi
    if [ "$intent_has_complete" = 1 ] && complete_terminal_converged; then
      echo "patch-land: $LANDING_ID already complete (durable receipts converged)" >&2
      continue
    fi
    if [ "$intent_has_abandoned" = 1 ] && abandoned_terminal_converged; then
      echo "patch-land: $LANDING_ID already abandoned (durable receipts converged)" >&2
      continue
    fi
    # Forward/reverse apply only describes the current tree. Once HEAD leaves
    # the intent's exact base, another commit can independently introduce (or
    # remove) identical bytes. Historical terminals above have durable external
    # receipts; an open intent has no such authority and must remain untouched
    # for manual recovery rather than claiming the newer commit as its landing.
    current_intent_base="$(git -C "$REPO" rev-parse HEAD 2>/dev/null | tr -d '\r' || printf 'unborn')"
    if [ -z "$intent_base_sha" ] || [ "$current_intent_base" != "$intent_base_sha" ]; then
      echo "error: landing $LANDING_ID: HEAD no longer matches its exact intent base" >&2
      echo "error: recorded ${intent_base_sha:-missing}, now $current_intent_base; preserve the open intent for manual recovery" >&2
      needs_manual=$((needs_manual + 1))
      continue
    fi
    if plan_superseding_done_receipt_converged; then
      if finish_superseded_plan_receipts superseded-done; then
        echo "patch-land: landing $LANDING_ID superseded by a different exact done receipt" >&2
        abandoned=$((abandoned + 1))
      else
        echo "error: landing $LANDING_ID has a superseding done receipt but approval convergence failed" >&2
        needs_manual=$((needs_manual + 1))
      fi
      continue
    fi
    if plan_superseding_landing_receipt_converged; then
      if finish_superseded_plan_receipts superseded-landing; then
        echo "patch-land: landing $LANDING_ID superseded by a different exact landing receipt" >&2
        abandoned=$((abandoned + 1))
      else
        echo "error: landing $LANDING_ID has a superseding landing receipt but approval convergence failed" >&2
        needs_manual=$((needs_manual + 1))
      fi
      continue
    fi
    # Applied content reverse-applies cleanly; unapplied content does not.
    if git -C "$REPO" apply --binary --reverse --check "$intent_patch" >/dev/null 2>&1; then
      rec_ok=1
      if [ -n "$intent_task" ]; then
        if plan_complete_receipt_converged; then
          : # The interrupted run already published this exact done receipt.
        elif plan_landing_receipt_converged; then
          finish_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" finish --id "$intent_task" --patch "$intent_patch")
          [ -n "$intent_lease" ] && finish_cmd+=(--lease-id "$intent_lease")
          finish_cmd+=(--expected-landing-receipt-sha256 "$intent_plan_receipt_sha")
          # Consent replays from the durable intent, not operator argv: a
          # --recover run carries the same refreeze the crashed run would.
          [ "${intent_verifier_consent:-0}" = 0 ] || finish_cmd+=(--refreeze-acceptance)
          if "${finish_cmd[@]}" >/dev/null 2>&1; then
            if ! plan_complete_receipt_converged; then
              rec_ok=0
              echo "warning: patch-land: recovered plan task $intent_task has an unexpected done receipt" >&2
            fi
          elif plan_complete_receipt_converged; then
            : # A concurrent recovery already finished this exact receipt.
          else
            rec_ok=0
            echo "warning: patch-land: recovery could not finish the exact plan receipt for $intent_task" >&2
          fi
        else
          rec_ok=0
          echo "warning: patch-land: recovery found a different plan landing receipt for $intent_task" >&2
        fi
      fi
      if [ "$rec_ok" = 1 ] &&
        ! record_landing_lineage_once "$intent_patch" "$current_patch_sha" \
          "$intent_task" "$intent_plan_id"; then
        rec_ok=0
        echo "warning: patch-land: recovery could not verify or record lineage for $LANDING_ID" >&2
      fi
      if [ "$rec_ok" = 1 ]; then
        if ! approval_recover_finish "$intent_approval" "$intent_approval_version" consumed; then
          rec_ok=0
          echo "warning: patch-land: recovery could not finalize approval $intent_approval" >&2
        fi
      fi
      if [ "$rec_ok" = 1 ]; then
        if [ "$intent_has_complete" = 1 ]; then
          echo "patch-land: recovered $LANDING_ID (patch was applied; receipts already complete)" >&2
          recovered=$((recovered + 1))
        elif landing_append complete recovered=1; then
          echo "patch-land: recovered $LANDING_ID (patch was applied)" >&2
          recovered=$((recovered + 1))
        else
          rec_ok=0
          echo "warning: patch-land: recovery could not record completion for $LANDING_ID" >&2
        fi
      fi
      if [ "$rec_ok" != 1 ]; then
        landing_append applied-pending-receipt reason=recovery-incomplete || true
        echo "patch-land: $LANDING_ID still incomplete; records could not be written" >&2
        needs_manual=$((needs_manual + 1))
      fi
    elif git -C "$REPO" apply --binary --check "$intent_patch" >/dev/null 2>&1; then
      # Forward apply proves the exact bytes are not present and are still
      # applicable to this tree. Only that proof is strong enough to call the
      # interrupted action "not applied"; a generic reverse-check failure can
      # also mean it was applied and then edited again.
      if finish_not_applied_receipts not-applied; then
        echo "patch-land: landing $LANDING_ID never applied; released and marked abandoned" >&2
        abandoned=$((abandoned + 1))
      else
        echo "error: landing $LANDING_ID is unapplied but its approval/plan receipts did not converge" >&2
        echo "error: preserve the intent and retry or perform manual recovery" >&2
        needs_manual=$((needs_manual + 1))
      fi
    else
      echo "error: landing $LANDING_ID: current tree proves neither applied nor unapplied" >&2
      echo "error: both forward and reverse patch checks failed; preserve the intent for manual recovery" >&2
      needs_manual=$((needs_manual + 1))
    fi
  done <<EOF
$(landing_outstanding)
EOF
  echo "patch-land: recovery finished ($recovered recovered, $abandoned abandoned, $needs_manual need manual recovery)"
  [ "$needs_manual" -eq 0 ] || exit 1
  exit 0
fi

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
  PLAN_JSON="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" \
    evidence-snapshot --id "$PLAN_TASK" 2>/dev/null)" ||
    fail "cannot read plan task $PLAN_TASK"
  PLAN_ID="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys;print(json.load(sys.stdin).get("plan_id", ""))' |
    tr -d '\r')"
  if [ -z "$PLAN_ID" ]; then
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" ensure-lineage >/dev/null ||
      fail "cannot establish plan lineage before landing task $PLAN_TASK"
    PLAN_JSON="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" \
      evidence-snapshot --id "$PLAN_TASK" 2>/dev/null)" ||
      fail "cannot reread plan task $PLAN_TASK after establishing lineage"
    PLAN_ID="$(printf '%s' "$PLAN_JSON" |
      python3 -c 'import json,sys;print(json.load(sys.stdin).get("plan_id", ""))' |
      tr -d '\r')"
  fi
  case "$PLAN_ID" in
    plan_*)
      case "${PLAN_ID#plan_}" in *[!0-9a-f]*) fail "plan task has malformed lineage" ;; esac
      [ "${#PLAN_ID}" -eq 37 ] || fail "plan task has malformed lineage"
      ;;
    *) fail "plan task has no immutable lineage" ;;
  esac
  export OMS_TASK_ID="$PLAN_TASK" OMS_INDEX_PLAN_ID="$PLAN_ID"
  # The receipt digest is over the stored task. plan_id is a top-level value
  # added only by evidence-snapshot, so strip it from this in-memory copy after
  # freezing it; do not perform a second, racy task read.
  PLAN_JSON="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys;d=json.load(sys.stdin);d.pop("plan_id",None);print(json.dumps(d,ensure_ascii=False))')" ||
    fail "cannot normalize plan task snapshot $PLAN_TASK"
  PLAN_LEASE_ID="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("lease_id", ""))' | tr -d '\r')" ||
    fail "cannot read lease for plan task $PLAN_TASK"
  PLAN_REVIEW_LEASE_ID="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("review_lease_id", ""))' | tr -d '\r')" ||
    fail "cannot read review lease for plan task $PLAN_TASK"
  PLAN_STATE="$(printf '%s' "$PLAN_JSON" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))' | tr -d '\r')" ||
    fail "cannot read state for plan task $PLAN_TASK"
  PLAN_REVIEW_PATCH="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,sys
value=json.load(sys.stdin).get("patch", "")
if not isinstance(value, str) or "\x00" in value or "\n" in value or "\r" in value:
    raise SystemExit(1)
print(value)
' | tr -d '\r')" || fail "cannot read reviewed patch for plan task $PLAN_TASK"
  PLAN_REVIEW_PATCH_STORED="$PLAN_REVIEW_PATCH"
  PLAN_REVIEW_RECEIPT_SHA="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,runpy,sys
print(runpy.run_path(sys.argv[1])["digest"](json.load(sys.stdin)))
' "$ROOT/scripts/lib/plan-receipt.py" | tr -d '\r')" ||
    fail "cannot hash review receipt for plan task $PLAN_TASK"
  PLAN_REVIEW_EXECUTOR_ID="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,re,sys
value=json.load(sys.stdin).get("executor_id", "")
if not isinstance(value, str) or (value and not re.fullmatch(r"[A-Za-z0-9._-]+", value)):
    raise SystemExit(1)
print(value)
' | tr -d '\r')" || fail "cannot read executor receipt for plan task $PLAN_TASK"
  PLAN_REVIEW_EXECUTOR_SOUL_SHA="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,re,sys
value=json.load(sys.stdin).get("executor_soul_sha256", "")
if not isinstance(value, str) or (value and not re.fullmatch(r"[0-9a-f]{64}", value)):
    raise SystemExit(1)
print(value)
' | tr -d '\r')" || fail "cannot read executor soul receipt for plan task $PLAN_TASK"
  PLAN_VERIFY_RAW="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,sys
value=json.load(sys.stdin).get("verify", "")
if not isinstance(value, str) or "\x00" in value:
    raise SystemExit(1)
sys.stdout.write(value + "__OMS_PLAN_VERIFY_END_9f31c8__")
' | tr -d '\r')" || fail "cannot read verify contract for plan task $PLAN_TASK"
  case "$PLAN_VERIFY_RAW" in
    *"__OMS_PLAN_VERIFY_END_9f31c8__") ;;
    *) fail "cannot decode verify contract for plan task $PLAN_TASK" ;;
  esac
  PLAN_VERIFY="${PLAN_VERIFY_RAW%__OMS_PLAN_VERIFY_END_9f31c8__}"
  [ -n "$PLAN_VERIFY" ] || fail "plan task $PLAN_TASK has no stored verify contract"
  if [ "$VERIFY_EXPLICIT" = 1 ]; then
    VERIFY="${VERIFY//$'\r'/}"
    [ "$VERIFY" = "$PLAN_VERIFY" ] ||
      fail "--verify does not match plan task $PLAN_TASK review contract"
  else
    VERIFY="$PLAN_VERIFY"
  fi
fi

# --plan-task alone is enough when delegate already stamped the patch path on
# the task: read it back instead of making the reviewer copy it by hand.
if [ -z "$PATCH" ] && [ -n "$PLAN_TASK" ]; then
  PATCH="$PLAN_REVIEW_PATCH"
  PATCH_FROM_PLAN=1
  [ -n "$PATCH" ] || fail "--patch omitted and plan task $PLAN_TASK has no stored patch path"
  echo "patch-land: using patch from plan task $PLAN_TASK: $PATCH" >&2
fi
[ -n "$PATCH" ] || fail "--patch is required (or --plan-task with a stored patch)"
[ -z "$PLAN_TASK" ] || {
  [ "$PLAN_STATE" = "review" ] ||
    fail "plan task $PLAN_TASK is $PLAN_STATE, not review"
  [ -n "$PLAN_LEASE_ID" ] && [ "$PLAN_REVIEW_LEASE_ID" = "$PLAN_LEASE_ID" ] ||
    fail "plan task $PLAN_TASK has a stale review lease"
  [ -n "$PLAN_REVIEW_PATCH" ] ||
    fail "plan task $PLAN_TASK review has no stored patch evidence"
  case "$PLAN_REVIEW_PATCH" in
    /*) ;;
    *) PLAN_REVIEW_PATCH="$REPO/$PLAN_REVIEW_PATCH" ;;
  esac
  PLAN_REVIEW_PATCH="$(cd "$(dirname "$PLAN_REVIEW_PATCH")" && pwd -P)/$(basename "$PLAN_REVIEW_PATCH")" ||
    fail "cannot resolve stored review patch for plan task $PLAN_TASK"
  [ "$PATCH_FROM_PLAN" = 0 ] || PATCH="$PLAN_REVIEW_PATCH"
  if [ ! -f "$PLAN_REVIEW_PATCH" ] || [ ! -r "$PLAN_REVIEW_PATCH" ]; then
    fail "plan task $PLAN_TASK stored patch is missing or unreadable"
  fi
  if [ -z "$PLAN_REVIEW_EXECUTOR_ID$PLAN_REVIEW_EXECUTOR_SOUL_SHA" ]; then
    [ -z "$EXECUTOR_ID" ] ||
      fail "plan task $PLAN_TASK review has no executor receipt for --executor $EXECUTOR_ID"
  else
    if [ -z "$PLAN_REVIEW_EXECUTOR_ID" ] || [ -z "$PLAN_REVIEW_EXECUTOR_SOUL_SHA" ]; then
      fail "plan task $PLAN_TASK has an incomplete executor review receipt"
    fi
    [ -n "$EXECUTOR_ID" ] ||
      fail "plan task $PLAN_TASK review requires --executor $PLAN_REVIEW_EXECUTOR_ID"
    [ "$EXECUTOR_ID" = "$PLAN_REVIEW_EXECUTOR_ID" ] ||
      fail "executor $EXECUTOR_ID does not match plan review executor $PLAN_REVIEW_EXECUTOR_ID"
    if [ -z "$executor_soul_sha" ] || [ "$executor_soul_sha" != "$PLAN_REVIEW_EXECUTOR_SOUL_SHA" ]; then
      fail "executor $EXECUTOR_ID soul does not match the plan review receipt"
    fi
  fi
}
[ -f "$PATCH" ] || fail "patch not found: $PATCH"
PATCH="$(cd "$(dirname "$PATCH")" && pwd -P)/$(basename "$PATCH")"
SOURCE_PATCH="$PATCH"
PATCH_SHA="$(oms_sha256_file "$PATCH" 2>/dev/null || true)"
[ -n "$PATCH_SHA" ] || fail "cannot hash patch: $PATCH"
if [ -n "$PLAN_TASK" ]; then
  PLAN_REVIEW_PATCH_SHA="$(oms_sha256_file "$PLAN_REVIEW_PATCH" 2>/dev/null || true)"
  [ -n "$PLAN_REVIEW_PATCH_SHA" ] ||
    fail "cannot hash stored review patch for plan task $PLAN_TASK"
  [ "$PATCH_SHA" = "$PLAN_REVIEW_PATCH_SHA" ] ||
    fail "patch bytes do not match plan task $PLAN_TASK review"
fi
APPROVAL_PROFILE="${OMS_EXECUTION_PROFILE:-trusted-local}"
case "$APPROVAL_PROFILE" in
  trusted-local|isolated|remote) ;;
  *) fail "invalid OMS_EXECUTION_PROFILE: $APPROVAL_PROFILE" ;;
esac
approval_allow_restructure=false
approval_allow_test_reduction=false
approval_allow_verifier_change=false
approval_ml=false
approval_verify_explicit=false
[ "$ALLOW_RESTRUCTURE" = 0 ] || approval_allow_restructure=true
[ "$ALLOW_TEST_REDUCTION" = 0 ] || approval_allow_test_reduction=true
[ "$ALLOW_VERIFIER_CHANGE" = 0 ] || approval_allow_verifier_change=true
[ "$ML" = 0 ] || approval_ml=true
[ -z "$VERIFY" ] || approval_verify_explicit=true
approval_verify_sha="$(printf '%s' "$VERIFY" | oms_sha256_stream 2>/dev/null || true)"
[ -n "$approval_verify_sha" ] || fail "cannot hash the verification contract"
APPROVAL_PARAMETERS="$( \
  OMS_APPROVAL_RESTRUCTURE="$approval_allow_restructure" \
  OMS_APPROVAL_TEST_REDUCTION="$approval_allow_test_reduction" \
  OMS_APPROVAL_VERIFIER_CHANGE="$approval_allow_verifier_change" \
  OMS_APPROVAL_ML="$approval_ml" \
  OMS_APPROVAL_VERIFY_EXPLICIT="$approval_verify_explicit" \
  OMS_APPROVAL_VERIFY_SHA="$approval_verify_sha" \
  OMS_APPROVAL_EXECUTOR_ID="$EXECUTOR_ID" \
  OMS_APPROVAL_EXECUTOR_SOUL="$executor_soul_sha" \
  python3 <<'PY'
import json
import os

def flag(name):
    return os.environ[name] == "true"

print(json.dumps({
    "admission_contract_schema": 1,
    "allow_restructure": flag("OMS_APPROVAL_RESTRUCTURE"),
    "allow_test_reduction": flag("OMS_APPROVAL_TEST_REDUCTION"),
    "allow_verifier_change": flag("OMS_APPROVAL_VERIFIER_CHANGE"),
    "ml": flag("OMS_APPROVAL_ML"),
    "verify_explicit": flag("OMS_APPROVAL_VERIFY_EXPLICIT"),
    "verify_sha256": os.environ["OMS_APPROVAL_VERIFY_SHA"],
    "executor_id": os.environ["OMS_APPROVAL_EXECUTOR_ID"] or None,
    "executor_soul_sha256": os.environ["OMS_APPROVAL_EXECUTOR_SOUL"] or None,
}, sort_keys=True, separators=(",", ":")))
PY
)" || fail "cannot encode the approval admission contract"

# Approval requests bind the exact base, patch, task lease, execution profile,
# verifier hash/mode, ML selection, frozen executor identity, and admission
# exceptions. The verifier text itself is not stored. Creating a request never
# runs the verifier or mutates Git; an operator decision produces the one-time
# grant.
if [ "$REQUEST_APPROVAL" = 1 ]; then
  [ -z "$APPROVAL_ID$APPROVAL_GRANT$APPROVAL_VERSION" ] ||
    fail "--request-approval cannot be combined with approval consumption options"
  approval_request=("$ROOT/scripts/approval-inbox.sh" --repo "$REPO" request \
    --action patch-land --object-id "patch:$PATCH_SHA" --summary "Land reviewed patch" \
    --base-sha "$(git -C "$REPO" rev-parse HEAD)" --patch-sha "$PATCH_SHA" \
    --profile "$APPROVAL_PROFILE" --parameters-json "$APPROVAL_PARAMETERS")
  [ -z "${OMS_ATTEMPT_ID:-}" ] || approval_request+=(--attempt "$OMS_ATTEMPT_ID")
  [ -z "$PLAN_TASK" ] || approval_request+=(--task-id "$PLAN_TASK")
  [ -z "$PLAN_LEASE_ID" ] || approval_request+=(--lease-id "$PLAN_LEASE_ID")
  "${approval_request[@]}"
  exit 0
fi

# One landing at a time per repository. Acquire before validating an approval
# or reading the base, then freeze the caller-owned patch into private durable
# state. Admission, intent, apply, lineage, and recovery all use these exact
# bytes; verifier code may mutate the original pathname without changing the
# approved object.
if ! oms_hold_file_lock "$REPO/.oms/landings.jsonl"; then
  fail "another landing or recovery is in progress in $REPO; retry when it finishes"
fi
patch_land_cleanup() {
  if [ -n "$FROZEN_PATCH" ] && [ "$FROZEN_PATCH_PERSIST" = 0 ]; then
    rm -f "$FROZEN_PATCH" "$FROZEN_PATCH.tmp" 2>/dev/null || true
  fi
  oms_release_held_file_lock
}
trap 'patch_land_cleanup' EXIT

LANDING_ID="land-$(date -u +%Y%m%dT%H%M%SZ)-$$-${RANDOM:-0}"
agent_memory_ensure_oms_ignore "$REPO" || fail "cannot initialize private landing state"
FROZEN_DIR="$REPO/.oms/landing-patches"
mkdir -p "$FROZEN_DIR" || fail "cannot create private landing patch directory"
chmod 700 "$FROZEN_DIR" 2>/dev/null || true
FROZEN_PATCH="$FROZEN_DIR/$LANDING_ID.patch"
if ! (umask 077; cp "$SOURCE_PATCH" "$FROZEN_PATCH.tmp"); then
  fail "cannot freeze patch bytes before admission"
fi
if ! mv "$FROZEN_PATCH.tmp" "$FROZEN_PATCH"; then
  rm -f "$FROZEN_PATCH.tmp" 2>/dev/null || true
  fail "cannot publish frozen patch bytes before admission"
fi
chmod 600 "$FROZEN_PATCH" 2>/dev/null || true
PATCH="$FROZEN_PATCH"
PATCH_SHA="$(oms_sha256_file "$PATCH" 2>/dev/null || true)"
[ -n "$PATCH_SHA" ] || fail "cannot hash frozen patch: $PATCH"
frozen_patch_matches() {
  local current_sha
  current_sha="$(oms_sha256_file "$PATCH" 2>/dev/null || true)"
  [ -n "$current_sha" ] && [ "$current_sha" = "$PATCH_SHA" ]
}
if [ -n "$PLAN_TASK" ]; then
  PLAN_REVIEW_PATCH_SHA="$(oms_sha256_file "$PLAN_REVIEW_PATCH" 2>/dev/null || true)"
  [ -n "$PLAN_REVIEW_PATCH_SHA" ] ||
    fail "cannot rehash stored review patch for plan task $PLAN_TASK"
  [ "$PATCH_SHA" = "$PLAN_REVIEW_PATCH_SHA" ] ||
    fail "frozen patch bytes no longer match plan task $PLAN_TASK review"
  PLAN_DONE_RECEIPT_SHA="$(printf '%s' "$PLAN_JSON" | python3 -c '
import json,runpy,sys
d=json.load(sys.stdin)
d["patch"]=sys.argv[2]
print(runpy.run_path(sys.argv[1])["digest"](d))
' "$ROOT/scripts/lib/plan-receipt.py" "$FROZEN_PATCH" | tr -d '\r')" ||
    fail "cannot hash expected done receipt for plan task $PLAN_TASK"
fi
LANDING_BASE="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || printf 'unborn')"

if [ -n "$APPROVAL_VERSION" ]; then
  case "$APPROVAL_VERSION" in *[!0-9]*|0) fail "--approval-version must be positive" ;; esac
fi
if [ -n "$APPROVAL_ID$APPROVAL_GRANT$APPROVAL_VERSION" ]; then
  [ -n "$APPROVAL_ID" ] && [ -n "$APPROVAL_GRANT" ] && [ -n "$APPROVAL_VERSION" ] ||
    fail "--approval, --approval-token, and --approval-version must be supplied together"
  approval_json="$("$ROOT/scripts/approval-inbox.sh" --repo "$REPO" show --approval "$APPROVAL_ID" --json)" ||
    fail "cannot read approval $APPROVAL_ID"
  OMS_LAND_APPROVAL_JSON="$approval_json" OMS_LAND_APPROVAL_ID="$APPROVAL_ID" \
  OMS_LAND_APPROVAL_VERSION="$APPROVAL_VERSION" OMS_LAND_PATCH_SHA="$PATCH_SHA" \
  OMS_LAND_BASE_SHA="$LANDING_BASE" OMS_LAND_TASK="$PLAN_TASK" \
  OMS_LAND_LEASE="$PLAN_LEASE_ID" OMS_LAND_PROFILE="$APPROVAL_PROFILE" \
  OMS_LAND_PARAMETERS="$APPROVAL_PARAMETERS" OMS_LAND_ATTEMPT="${OMS_ATTEMPT_ID:-}" \
    python3 <<'PY' || fail "approval does not match this exact landing action"
import json, os
try:
    row = json.loads(os.environ["OMS_LAND_APPROVAL_JSON"])
    expected_version = int(os.environ["OMS_LAND_APPROVAL_VERSION"])
    expected_parameters = json.loads(os.environ["OMS_LAND_PARAMETERS"])
except (KeyError, TypeError, ValueError, json.JSONDecodeError):
    raise SystemExit(1)
if not isinstance(row, dict):
    raise SystemExit(1)
expected_attempt = os.environ["OMS_LAND_ATTEMPT"]
stored_attempt = row.get("attempt_id", "")
matches = (
    row.get("approval_id") == os.environ["OMS_LAND_APPROVAL_ID"],
    row.get("version") == expected_version,
    row.get("state") == "approved",
    row.get("action") == "patch-land",
    row.get("object_id") == "patch:" + os.environ["OMS_LAND_PATCH_SHA"],
    row.get("patch_sha") == os.environ["OMS_LAND_PATCH_SHA"],
    row.get("base_sha") == os.environ["OMS_LAND_BASE_SHA"],
    row.get("task_id", "") == os.environ["OMS_LAND_TASK"],
    row.get("lease_id", "") == os.environ["OMS_LAND_LEASE"],
    row.get("profile") == os.environ["OMS_LAND_PROFILE"],
    row.get("parameters", {}) == expected_parameters,
    not stored_attempt or (bool(expected_attempt) and stored_attempt == expected_attempt),
)
if not all(matches):
    raise SystemExit(1)
PY
else
  case "${OMS_REQUIRE_LANDING_APPROVAL:-0}:$APPROVAL_PROFILE" in
    1:*|true:*|yes:*|on:*|*:isolated|*:remote)
      fail "this execution policy requires --approval, --approval-token, and --approval-version"
      ;;
  esac
fi

# Landing decisions are the most expensive dead ends (a full delegate + admit
# cycle), so they feed the shared failure memory: fingerprint by patch content
# so the same rejected patch warns any later agent before it re-lands.
land_fingerprint_cmd() {
  local sha
  sha="$(oms_sha256_file "$PATCH" 2>/dev/null || true)"
  [ -n "$sha" ] || sha="$(basename "$SOURCE_PATCH")"
  printf 'patch-land %s' "$sha"
}
known_reject_fp=""
check_out="$( (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" check --cmd "$(land_fingerprint_cmd)") 2>&1 || true)"
known_reject_fp="$(printf '%s\n' "$check_out" | awk '$1 == "fail-ledger:" && $2 ~ /^[0-9a-f]+$/ {print $2; exit}')"
case "$check_out" in
  *"already failed"*)
    echo "warning: this exact patch was rejected before:" >&2
    echo "  $check_out" >&2
    ;;
  *"failed before, but git state changed"*)
    echo "warning: this exact patch was rejected before under a different git state; retrying:" >&2
    echo "  $check_out" >&2
    ;;
esac

# Pre-flight: never apply onto a dirty tree — a half-applied patch on top of
# unrelated edits is the mess this whole gate exists to avoid.
# Admission verifies a throwaway worktree built from this base. If the main
# tree moves between that check and the apply below, what lands was never the
# combination that passed, so the base is captured here and rechecked.
PRE_ADMIT_BASE="$LANDING_BASE"
current_pre_admit_base="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || printf 'unborn')"
if [ "$current_pre_admit_base" != "$PRE_ADMIT_BASE" ]; then
  fail "tree moved before admission (base $PRE_ADMIT_BASE -> $current_pre_admit_base); retry"
fi
if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=all)" ]; then
  fail "refusing to land: main tree has tracked or untracked changes"
fi

# --- Admission gate (the trust boundary) ------------------------------------
# Candidate verification runs project code with the caller's host permissions.
# Even though the landing copy is private, a same-UID verifier can still find
# and rewrite it. Bind admission and apply to the digest captured above: check
# immediately before handing it to the gate and again when the gate returns.
frozen_patch_matches ||
  fail "frozen patch changed before admission; refusing to verify moved bytes"
admit_cmd=("$ROOT/scripts/patch-admit.sh" --patch "$PATCH" --repo "$REPO")
[ -n "$VERIFY" ] && admit_cmd+=(--verify "$VERIFY")
[ "$ML" = 1 ] && admit_cmd+=(--ml)
[ -n "$PLAN_TASK" ] && admit_cmd+=(--plan-task "$PLAN_TASK")
[ -n "$EXECUTOR_ID" ] && admit_cmd+=(--executor "$EXECUTOR_ID")
[ "$ALLOW_VERIFIER_CHANGE" = 1 ] && admit_cmd+=(--allow-verifier-change)
[ "$ALLOW_TEST_REDUCTION" = 1 ] && admit_cmd+=(--allow-test-reduction)
[ "$ALLOW_RESTRUCTURE" = 1 ] && admit_cmd+=(--allow-restructure)
if ! "${admit_cmd[@]}" >/dev/null; then
  echo "patch-land: REJECTED by admission gate; not applied" >&2
  # Durable record: without this a later agent re-runs the whole delegate +
  # admit cycle for a patch already known to fail.
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record \
    --kind patch-land --cmd "$(land_fingerprint_cmd)" --exit 1 \
    --summary "patch-land REJECT: $(basename "$SOURCE_PATCH")${PLAN_TASK:+ (plan task $PLAN_TASK)}") >&2 ||
    echo "warning: patch-land: could not record rejection in fail-ledger" >&2
  exit 1
fi
frozen_patch_matches ||
  fail "frozen patch changed during admission; verified bytes were not applied"

# --- Apply ------------------------------------------------------------------
# Recheck the reviewed tree, then persist the intent before changing the plan
# task to `landing`. A crash must never leave an unexplained landing task.
post_admit_base="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || printf 'unborn')"
if [ "$post_admit_base" != "$PRE_ADMIT_BASE" ]; then
  fail "tree moved during admission (base $PRE_ADMIT_BASE -> $post_admit_base); re-admit"
fi
if [ -n "$(git -C "$REPO" status --porcelain --untracked-files=all)" ]; then
  fail "tree became dirty during admission; re-admit against the current tree"
fi

[ -z "$APPROVAL_ID" ] || APPROVAL_CONSUMING_VERSION="$((APPROVAL_VERSION + 1))"
intent_patch="$PATCH"
intent_patch_sha="$PATCH_SHA"
intent_base_sha="$LANDING_BASE"
intent_task="$PLAN_TASK"
intent_plan_id="$PLAN_ID"
intent_lease="$PLAN_LEASE_ID"
intent_plan_receipt_sha="$PLAN_REVIEW_RECEIPT_SHA"
intent_plan_done_receipt_sha="$PLAN_DONE_RECEIPT_SHA"
intent_approval="$APPROVAL_ID"
intent_approval_version="$APPROVAL_CONSUMING_VERSION"
intent_receipt_sha="$(landing_receipt_hash)" ||
  fail "cannot hash the canonical landing intent receipt"
# Fail closed: an unrecordable intent means a crash after the apply would be
# indistinguishable from nothing having happened, which is the exact failure
# the transaction exists to prevent. Refuse rather than apply blind.
FROZEN_PATCH_PERSIST=1
# Operator verifier-change consent rides the durable intent so a --recover
# replay of the finish carries it too; the extra key stays outside the
# canonical receipt hash, so pre-field intents remain hash-compatible.
intent_consent_extra=""
[ "$ALLOW_VERIFIER_CHANGE" = 0 ] || intent_consent_extra="verifier_change_consent=true"
if ! landing_append intent ${intent_consent_extra:+"$intent_consent_extra"}; then
  FROZEN_PATCH_PERSIST=0
  fail "cannot record the landing intent in $LANDINGS; refusing to apply"
fi

# Fence the reviewed claim only after the recovery record is durable. A stale
# reviewer still cannot land after reclaim/re-claim, and a failed fence now
# converges through the same not-applied receipt path as crash recovery.
if [ -n "$PLAN_TASK" ]; then
  land_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" land --id "$PLAN_TASK" \
    --lease-id "$PLAN_LEASE_ID" \
    --expected-review-patch "$PLAN_REVIEW_PATCH_STORED" \
    --expected-review-patch-sha256 "$PLAN_REVIEW_PATCH_SHA" \
    --expected-review-verify "$PLAN_VERIFY" \
    --expected-review-executor-id "$PLAN_REVIEW_EXECUTOR_ID" \
    --expected-review-executor-soul-sha256 "$PLAN_REVIEW_EXECUTOR_SOUL_SHA" \
    --expected-review-lease-id "$PLAN_REVIEW_LEASE_ID")
  if ! "${land_cmd[@]}" >/dev/null; then
    # agent-plan's compare-and-set is atomic: a rejected fence never entered
    # landing. In particular, do not release a same-lease repair review that
    # superseded the admitted receipt while its verifier was running.
    if ! finish_not_applied_receipts plan-fence-rejected 1; then
      echo "warning: patch-land: plan fence failed and recovery receipts remain incomplete" >&2
    fi
    echo "patch-land: plan task lease/state changed; not applied" >&2
    exit 1
  fi
fi

if [ -n "$APPROVAL_ID" ]; then
  if ! "$ROOT/scripts/approval-inbox.sh" --repo "$REPO" begin-consume \
    --approval "$APPROVAL_ID" --token "$APPROVAL_GRANT" \
    --expected-version "$APPROVAL_VERSION" --consumer patch-land >/dev/null; then
    if ! finish_not_applied_receipts approval-rejected; then
      echo "warning: patch-land: approval rejection left plan/approval receipts incomplete" >&2
    fi
    fail "approval token was rejected; patch was not applied"
  fi
fi

# Keep the last authority-changing steps (plan fence and approval reservation)
# from reopening the same race. Recovery can safely converge both receipts
# because the intent is already durable and the tree is still untouched.
if ! frozen_patch_matches; then
  if ! finish_not_applied_receipts patch-changed-before-apply; then
    echo "warning: patch-land: changed patch left plan/approval receipts incomplete" >&2
  fi
  fail "frozen patch changed after admission; verified bytes were not applied"
fi

if ! git -C "$REPO" apply --binary "$PATCH"; then
  if ! finish_not_applied_receipts apply-failed; then
    echo "warning: patch-land: apply failed and recovery receipts remain incomplete" >&2
  fi
  fail "admission passed but git apply failed (base moved?); tree unchanged"
fi

approval_receipt_ok=1
if [ -n "$APPROVAL_CONSUMING_VERSION" ]; then
  if ! "$ROOT/scripts/approval-inbox.sh" --repo "$REPO" finish-consume \
    --approval "$APPROVAL_ID" --expected-version "$APPROVAL_CONSUMING_VERSION" \
    --result consumed --consumer patch-land >/dev/null; then
    approval_receipt_ok=0
    echo "warning: patch-land: patch applied but approval consumption could not be finalized" >&2
  fi
fi

changed_files="$(git -C "$REPO" apply --numstat "$PATCH" 2>/dev/null | awk -F '\t' '{print $3}' | tr '\n' ' ' | sed 's/ *$//')"
echo "patch-land: applied $SOURCE_PATCH" >&2
[ -n "$changed_files" ] && echo "patch-land: changed $changed_files" >&2

# Record the land so "what patch was applied for task X" is answerable. Stamp
# the plan/task id via OMS_TASK_ID so the row carries lineage. A failed record
# does not unwind the land, but it must be loud: a silent miss here makes the
# lineage unanswerable exactly when something already went wrong.
receipts_ok=1
[ "$approval_receipt_ok" = 1 ] || receipts_ok=0
if ! record_landing_lineage_once "$PATCH" "$PATCH_SHA" "$PLAN_TASK" "$PLAN_ID"; then
  receipts_ok=0
  echo "warning: patch-land: patch applied but the land row could NOT be recorded" >&2
  echo "warning: patch-land: lineage for $SOURCE_PATCH is missing from .oms/artifacts/index.jsonl" >&2
fi

# The patch was rejected before but lands now: resolve the fingerprint so the
# shared failure memory stops warning about it.
if [ -n "$known_reject_fp" ]; then
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" resolve --fingerprint "$known_reject_fp") >&2 || true
fi

# --- Optional plan lifecycle ------------------------------------------------
if [ -n "$PLAN_TASK" ]; then
  finish_cmd=("$ROOT/scripts/agent-plan.sh" --repo "$REPO" finish --id "$PLAN_TASK" --patch "$PATCH")
  [ "$ALLOW_VERIFIER_CHANGE" = 0 ] || finish_cmd+=(--refreeze-acceptance)
  [ -n "$PLAN_LEASE_ID" ] && finish_cmd+=(--lease-id "$PLAN_LEASE_ID")
  finish_cmd+=(--expected-landing-receipt-sha256 "$PLAN_REVIEW_RECEIPT_SHA")
  if "${finish_cmd[@]}" >/dev/null 2>&1; then
    echo "patch-land: plan task $PLAN_TASK -> done" >&2
  else
    receipts_ok=0
    echo "warning: could not finish plan task $PLAN_TASK (wrong state?)" >&2
  fi
fi

# Completion means every write landed, not just the apply. A missing lineage
# row or an unfinished plan task keeps the transaction outstanding so
# --recover retries it; otherwise the tree carries a change that state cannot
# explain and nothing says so.
if [ "$receipts_ok" = 1 ]; then
  if ! landing_append complete; then
    echo "patch-land: applied, but completion could not be recorded; run: oms patch-land --recover" >&2
    exit 1
  fi
else
  landing_append applied-pending-receipt reason=receipt-write-failed || true
  echo "patch-land: applied, but its records are incomplete; run: oms patch-land --recover" >&2
  exit 1
fi

echo "LANDED"
