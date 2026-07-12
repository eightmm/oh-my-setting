#!/usr/bin/env bash
set -euo pipefail

# Execute exactly one pre-authorized agent-plan task. This is deliberately not
# a daemon or an unbounded autonomy loop: it composes atomic claim, isolated
# delegation, bounded repair, review, and optional patch landing while the
# parent agent retains scope and release authority.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT/scripts/lib/oms-common.sh"

REPO="$PWD"
TO=""
TASK_ID=""
USE_NEXT=0
REPAIR=0
LAND=0
RETRY_KNOWN=0
DRY_RUN=0
EXECUTOR_ID=""
LEASE_ID=""
VERIFY=""
FAIL_CMD=""
KNOWN_FAILURE_FP=""
CHILD_PID=""
KEEP_CLAIM=0
CLAIMED=0

usage() {
  cat <<'EOF'
Usage: plan-run.sh --to PROVIDER (--id ID | --next) [options]

Execute one actionable agent-plan task through the existing isolated worker
and admission boundaries. Success stops in review by default; --land is an
explicit request to admit and apply the reviewed patch.

Options:
  --to PROVIDER   codex, claude, or antigravity.
  --id ID         Claim and execute this ready task.
  --next          Atomically claim the next actionable task.
  --repair N      Bounded worker repair rounds, 0-3 (default: 0).
  --land          Land through patch-land after successful delegation.
  --retry-known   Retry even when this exact task/base/provider/verify contract
                  is an unresolved known failure.
  --executor ID   Use a frozen task-scoped executor soul.
  --repo PATH     Target repo (default: current directory).
  --dry-run       Show the selected task and command without claiming/calling.
  -h, --help      Show help.

The selected task must declare allowed_paths and a mechanical verify command.
One invocation executes at most one task and never commits, pushes, publishes,
adds dependencies, or recursively delegates.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --to) [ "$#" -ge 2 ] || fail "--to requires provider"; TO="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires value"; TASK_ID="$2"; shift 2 ;;
    --next) USE_NEXT=1; shift ;;
    --repair) [ "$#" -ge 2 ] || fail "--repair requires N"; REPAIR="$2"; shift 2 ;;
    --land) LAND=1; shift ;;
    --retry-known) RETRY_KNOWN=1; shift ;;
    --executor) [ "$#" -ge 2 ] || fail "--executor requires ID"; EXECUTOR_ID="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$TO" ] || fail "--to is required"
TO="$(oms_normalize_provider "$TO")" || fail "unknown provider: $TO"
case "$REPAIR" in *[!0-9]*|"") fail "--repair must be 0-3" ;; esac
[ "$REPAIR" -le 3 ] || fail "--repair must be 0-3"
if [ "$USE_NEXT" -eq 1 ] && [ -n "$TASK_ID" ]; then fail "use exactly one of --id or --next"; fi
if [ "$USE_NEXT" -eq 0 ] && [ -z "$TASK_ID" ]; then fail "use exactly one of --id or --next"; fi
case "$TASK_ID$EXECUTOR_ID" in *[!A-Za-z0-9._-]* ) fail "task/executor ids must match [A-Za-z0-9._-]+" ;; esac

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"

release_claim() {
  local state current_lease
  [ "$CLAIMED" -eq 1 ] || return 0
  [ "$KEEP_CLAIM" -eq 0 ] || return 0
  task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID" 2>/dev/null || true)"
  [ -n "$task_json" ] || return 0
  state="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))' 2>/dev/null || true)"
  current_lease="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("lease_id", ""))' 2>/dev/null || true)"
  case "$state" in claimed|running|review|landing) ;; *) return 0 ;; esac
  [ "$current_lease" = "$LEASE_ID" ] || return 0
  "$ROOT/scripts/agent-plan.sh" --repo "$REPO" release --id "$TASK_ID" --lease-id "$LEASE_ID" >/dev/null 2>&1 || true
}

cleanup() {
  code="$?"
  grace="${OMS_PLAN_RUN_KILL_AFTER:-5}"
  elapsed=0
  trap - EXIT HUP INT TERM
  case "$grace" in *[!0-9]*|"") grace=5 ;; esac
  if [ -n "$CHILD_PID" ]; then
    kill -TERM "$CHILD_PID" 2>/dev/null || true
    while kill -0 "$CHILD_PID" 2>/dev/null && [ "$elapsed" -lt "$grace" ]; do
      sleep 1
      elapsed=$((elapsed + 1))
    done
    if kill -0 "$CHILD_PID" 2>/dev/null; then
      kill -KILL "$CHILD_PID" 2>/dev/null || true
    fi
    wait "$CHILD_PID" 2>/dev/null || true
    CHILD_PID=""
  fi
  release_claim
  exit "$code"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$USE_NEXT" -eq 1 ]; then
  if [ "$DRY_RUN" -eq 1 ]; then
    task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" next --json)" || exit $?
  else
    task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" next --claim --provider "$TO" --json)" || exit $?
    CLAIMED=1
  fi
  TASK_ID="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
else
  task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID")" || exit $?
  state="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))')"
  [ "$state" = ready ] || fail "task $TASK_ID is $state, not ready"
  if [ "$DRY_RUN" -eq 0 ]; then
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" claim --id "$TASK_ID" --provider "$TO" >/dev/null
    CLAIMED=1
    task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID")"
  fi
fi

task_values="$(printf '%s' "$task_json" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("\t".join([d.get("lease_id", ""), d.get("verify", ""), ",".join(d.get("allowed_paths", [])), d.get("title", "")]))')"
LEASE_ID="$(printf '%s' "$task_values" | cut -f1)"
VERIFY="$(printf '%s' "$task_values" | cut -f2)"
ALLOWED="$(printf '%s' "$task_values" | cut -f3)"
TITLE="$(printf '%s' "$task_values" | cut -f4)"
[ -n "$ALLOWED" ] || fail "task $TASK_ID must declare non-empty allowed_paths"
[ -n "$VERIFY" ] || fail "task $TASK_ID must declare a mechanical verify command"

base="$(git -C "$REPO" rev-parse HEAD)"
verify_hash="$(printf '%s' "$VERIFY" | oms_sha256_stream | cut -c1-16)"
FAIL_CMD="plan-run task=$TASK_ID base=$base provider=$TO verify=$verify_hash"
set +e
failure_check="$(cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" check --cmd "$FAIL_CMD" 2>&1)"
failure_check_rc=$?
set -e
if [ -n "$failure_check" ]; then
  printf '%s\n' "$failure_check" >&2
  KNOWN_FAILURE_FP="$(printf '%s\n' "$failure_check" | awk '/^fail-ledger: [0-9a-f]+ / {print $2; exit}')"
fi
if [ "$failure_check_rc" -ne 0 ] && [ "$failure_check_rc" -ne 3 ]; then
  fail "could not check failure ledger (exit $failure_check_rc)"
fi
if [ "$failure_check_rc" -ne 0 ] && [ "$RETRY_KNOWN" -eq 0 ]; then
  fail "known unchanged plan-run failure; change the contract or use --retry-known"
fi

echo "plan-run: task=$TASK_ID provider=$TO title=$TITLE"
echo "plan-run: scope=$ALLOWED verify=$VERIFY"
if [ "$DRY_RUN" -eq 1 ]; then
  echo "plan-run: dry-run; no claim or provider call"
  exit 0
fi

delegate_script="${OMS_PLAN_RUN_DELEGATE:-$ROOT/scripts/peer-delegate.sh}"
[ -x "$delegate_script" ] || fail "delegate command is not executable: $delegate_script"
delegate_cmd=("$delegate_script" --repo "$REPO" --to "$TO" --plan-task "$TASK_ID" --repair "$REPAIR")
[ -n "$EXECUTOR_ID" ] && delegate_cmd+=(--executor "$EXECUTOR_ID")
output_file="$(agent_memory_mktemp)" || exit 1
set +e
"${delegate_cmd[@]}" >"$output_file" 2>&1 &
CHILD_PID="$!"
wait "$CHILD_PID"
delegate_status=$?
CHILD_PID=""
set -e
cat "$output_file"

artifact="$(awk -F': ' '$1 == "artifact" {v=$2} END {print v}' "$output_file")"
patch="$(awk -F': ' '$1 == "patch" {v=$2} END {print v}' "$output_file")"
rm -f "$output_file"

if [ "$delegate_status" -ne 0 ]; then
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record --kind plan-run --cmd "$FAIL_CMD" --exit "$delegate_status" \
    --summary "plan-run failed: task $TASK_ID on $TO") || true
  echo "plan-run: failed task=$TASK_ID exit=$delegate_status next=inspect-failure" >&2
  exit "$delegate_status"
fi

# peer-delegate moves a successful task to review and stores its patch under
# the same lease. Preserve that review state unless explicit landing succeeds.
KEEP_CLAIM=1
if [ "$LAND" -eq 1 ]; then
  land_cmd=("$ROOT/scripts/patch-land.sh" --repo "$REPO" --plan-task "$TASK_ID" --verify "$VERIFY")
  [ -n "$EXECUTOR_ID" ] && land_cmd+=(--executor "$EXECUTOR_ID")
  "${land_cmd[@]}"
  next_action="continue-plan"
  state="done"
else
  next_action="review-or-land"
  state="review"
fi

if [ -n "$KNOWN_FAILURE_FP" ]; then
  (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" resolve --fingerprint "$KNOWN_FAILURE_FP") || true
fi

echo "plan-run: result task=$TASK_ID state=$state artifact=${artifact:--} patch=${patch:--} next=$next_action"
