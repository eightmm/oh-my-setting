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
# shellcheck source=scripts/lib/model-routing.sh
. "$ROOT/scripts/lib/model-routing.sh"

REPO="$PWD"
TO=""
TASK_ID=""
USE_NEXT=0
REPAIR=0
LAND=0
AUTO_REPAIR=0
ALLOW_VERIFIER_CHANGE=0
RETRY_KNOWN=0
DRY_RUN=0
EXECUTOR_ID=""
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
LEASE_ID=""
VERIFY=""
FAIL_CMD=""
KNOWN_FAILURE_FP=""
CHILD_PID=""
KEEP_CLAIM=0
CLAIMED=0
EXECUTOR_PLAN_TASK=""
CONTINUE_REVIEW=0
POSSIBLE_REPAIR_RESUME=0
RESUME_REPAIR=0

usage() {
  cat <<'EOF'
Usage: plan-run.sh --to PROVIDER (--id ID | --next) [options]

Execute one actionable agent-plan task through the existing isolated worker
and admission boundaries. Success stops in review by default; --land is an
explicit request to admit and apply the reviewed patch.

Options:
  --to PROVIDER   Registered write-capable agent transport.
  --id ID         Claim and execute this ready task. With --land, a matching
                  reviewed task continues from its stored patch first.
  --next          Atomically claim the next actionable task.
  --repair N      Bounded worker repair rounds, 0-3 (default: 0).
  --land          Land through patch-land after successful delegation.
  --allow-verifier-change  Forward to patch-land/patch-admit: permit a patch
                  whose task scope includes its own verifier file.
  --auto-repair   With --land: when landing fails, run exactly ONE fenced repair
                  delegation with the failed gate's own output embedded in
                  the worker brief, retry landing once, then park the task
                  for an outside read (oms advise). The plan lease and any
                  frozen executor contract are reused, never widened. Never loops.
  --retry-known   Retry even when this exact task/base/provider/verify contract
                  is an unresolved known failure.
  --executor ID   Use a frozen task-scoped executor soul. A reviewed task that
                  carries an executor receipt requires this exact ID and soul.
  --model MODEL   Exact provider model; disables implicit fallback.
  --fallback-model M  Explicit one-shot capacity fallback model.
  --reasoning-effort E  auto, low, medium, high, xhigh, max, or ultra.
  --repo PATH     Target repo (default: current directory).
  --dry-run       Show the selected task and command without claiming/calling.
  -h, --help      Show help.

The selected task must declare allowed_paths and a mechanical verify command.
One invocation executes at most one task and never commits, pushes, publishes,
adds dependencies, or recursively delegates. Before claiming, a pre-flight
agent-plan reclaim frees claims whose heartbeat is past OMS_PLAN_CLAIM_TTL, so
a task abandoned by a dead worker is runnable again without a separate call.
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
    --allow-verifier-change) ALLOW_VERIFIER_CHANGE=1; shift ;;
    --auto-repair) AUTO_REPAIR=1; [ "$REPAIR" -ge 1 ] || REPAIR=1; shift ;;
    --retry-known) RETRY_KNOWN=1; shift ;;
    --executor) [ "$#" -ge 2 ] || fail "--executor requires ID"; EXECUTOR_ID="$2"; shift 2 ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires value"; MODEL="$2"; shift 2 ;;
    --fallback-model) [ "$#" -ge 2 ] || fail "--fallback-model requires value"; FALLBACK_MODEL="$2"; shift 2 ;;
    --reasoning-effort) [ "$#" -ge 2 ] || fail "--reasoning-effort requires value"; REASONING_EFFORT="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$TO" ] || fail "--to is required"
TO="$(oms_normalize_provider "$TO")" || fail "unknown provider: $TO"
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
case "$REPAIR" in *[!0-9]*|"") fail "--repair must be 0-3" ;; esac
[ "$REPAIR" -le 3 ] || fail "--repair must be 0-3"
[ "$AUTO_REPAIR" -eq 0 ] || [ "$LAND" -eq 1 ] || fail "--auto-repair requires --land"
if [ "$USE_NEXT" -eq 1 ] && [ -n "$TASK_ID" ]; then fail "use exactly one of --id or --next"; fi
if [ "$USE_NEXT" -eq 0 ] && [ -z "$TASK_ID" ]; then fail "use exactly one of --id or --next"; fi
case "$TASK_ID$EXECUTOR_ID" in *[!A-Za-z0-9._-]* ) fail "task/executor ids must match [A-Za-z0-9._-]+" ;; esac

REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || fail "not a git repo: $REPO"

if [ -n "$EXECUTOR_ID" ]; then
  executor_meta="$($ROOT/scripts/agent-executor.sh show --repo "$REPO" --id "$EXECUTOR_ID")" ||
    fail "cannot read executor $EXECUTOR_ID"
  EXECUTOR_PLAN_TASK="$(printf '%s' "$executor_meta" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("plan_task", ""))')"
  if [ -n "$EXECUTOR_PLAN_TASK" ]; then
    [ "$USE_NEXT" -eq 0 ] || fail "a plan-bound executor requires --id, not --next"
    [ "$TASK_ID" = "$EXECUTOR_PLAN_TASK" ] || fail "executor is bound to plan task $EXECUTOR_PLAN_TASK"
  fi
fi

release_claim() {
  local state current_lease
  [ "$CLAIMED" -eq 1 ] || return 0
  [ "$KEEP_CLAIM" -eq 0 ] || return 0
  task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID" 2>/dev/null || true)"
  [ -n "$task_json" ] || return 0
  state="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))' 2>/dev/null || true)"
  current_lease="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("lease_id", ""))' 2>/dev/null || true)"
  case "$state" in claimed|running) ;; *) return 0 ;; esac
  [ "$current_lease" = "$LEASE_ID" ] || return 0
  "$ROOT/scripts/agent-plan.sh" --repo "$REPO" release --id "$TASK_ID" --lease-id "$LEASE_ID" >/dev/null 2>&1 || true
}

terminalize_resumed_repair_failure() {
  local current values current_state current_lease current_review_lease current_repair
  local executor_state executor_repair

  [ "$RESUME_REPAIR" -eq 1 ] || return 0
  current="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$TASK_ID" 2>/dev/null)" || {
    echo "error: cannot read failed resumed repair task $TASK_ID" >&2
    return 1
  }
  values="$(printf '%s' "$current" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print("\t".join([d.get("state", ""), d.get("lease_id", ""),
                  d.get("review_lease_id", ""), str(d.get("repair_count", 0))]))
')" || {
    echo "error: cannot decode failed resumed repair task $TASK_ID" >&2
    return 1
  }
  current_state="$(printf '%s' "$values" | cut -f1)"
  current_lease="$(printf '%s' "$values" | cut -f2)"
  current_review_lease="$(printf '%s' "$values" | cut -f3)"
  current_repair="$(printf '%s' "$values" | cut -f4)"

  # The delegated repair owns terminalization and normally returns an already
  # blocked row. A failure before its terminal transition may leave the original
  # active lease, which is still safe to block here. Never block a ready row:
  # agent-plan intentionally does not fence ready -> blocked by lease, so that
  # post-release write would race a fresh sibling claim.
  if [ "$current_repair" != 1 ] || [ "$current_review_lease" != "$LEASE_ID" ]; then
    echo "error: failed resumed repair no longer has its exact one-shot receipt" >&2
    return 1
  fi
  case "$current_state" in
    blocked) ;;
    ready)
      echo "error: delegated resumed repair returned a ready task; refusing a racy post-release block" >&2
      return 1
      ;;
    claimed|running|review|landing)
      [ "$current_lease" = "$LEASE_ID" ] || {
        echo "error: failed resumed repair is now owned by a different lease; refusing to overwrite it" >&2
        return 1
      }
      "$ROOT/scripts/agent-plan.sh" --repo "$REPO" block --id "$TASK_ID" \
        --lease-id "$LEASE_ID" --reason "bounded landing repair failed" >/dev/null 2>&1 || {
        echo "error: could not block failed resumed repair under its exact lease" >&2
        return 1
      }
      ;;
    *)
      echo "error: failed resumed repair task $TASK_ID is $current_state, not terminalizable" >&2
      return 1
      ;;
  esac

  current="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" show --id "$TASK_ID" 2>/dev/null)" || return 1
  if ! printf '%s' "$current" | python3 -c '
import json,sys
d=json.load(sys.stdin)
repair=d.get("repair_count")
if (d.get("state") != "blocked" or isinstance(repair, bool) or repair != 1
        or d.get("review_lease_id") != sys.argv[1]):
    raise SystemExit(1)
' "$LEASE_ID"; then
    echo "error: failed resumed repair did not converge to the exact blocked receipt" >&2
    return 1
  fi
  KEEP_CLAIM=1

  if [ -n "$EXECUTOR_ID" ]; then
    executor_state="$("$ROOT/scripts/agent-executor.sh" show --repo "$REPO" --id "$EXECUTOR_ID" 2>/dev/null |
      python3 -c 'import json,sys;print(json.load(sys.stdin).get("state", ""))' 2>/dev/null || true)"
    case "$executor_state" in
      frozen|running)
        "$ROOT/scripts/agent-executor.sh" fail --repo "$REPO" --id "$EXECUTOR_ID" \
          --reason "bounded landing repair failed" >/dev/null 2>&1 || {
          echo "error: could not fail executor $EXECUTOR_ID after resumed repair failure" >&2
          return 1
        }
        ;;
      failed) ;;
      *)
        echo "error: resumed repair executor $EXECUTOR_ID is $executor_state, not failed" >&2
        return 1
        ;;
    esac
    values="$("$ROOT/scripts/agent-executor.sh" show --repo "$REPO" --id "$EXECUTOR_ID" 2>/dev/null |
      python3 -c 'import json,sys;d=json.load(sys.stdin);print("%s\t%s"%(d.get("state", ""),d.get("repair_count", 0)))' 2>/dev/null || true)"
    executor_state="$(printf '%s' "$values" | cut -f1)"
    executor_repair="$(printf '%s' "$values" | cut -f2)"
    if [ "$executor_state" != failed ] || [ "$executor_repair" != 1 ]; then
      echo "error: resumed repair executor $EXECUTOR_ID did not converge to failed" >&2
      return 1
    fi
  fi

  echo "plan-run: failed resumed bounded repair parked task=$TASK_ID state=blocked" >&2
  return 0
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

# Pre-flight: free claims whose heartbeat is past the TTL. Reads already
# present such a claim as expired, but the stored row still says "claimed", and
# the --id path below requires a ready task. A caller that only ever runs
# plan-run never invokes reclaim, so a dead worker would park its task forever.
# Guarded and tolerant: if this cannot run, the claim simply stays put. Skipped
# for a dry run (which must not write) and for a plan-bound executor, whose
# task is required to STAY claimed.
if [ "$DRY_RUN" -eq 0 ] && [ -z "$EXECUTOR_PLAN_TASK" ]; then
  preflight="$("$ROOT/scripts/agent-plan.sh" --repo "$REPO" reclaim 2>/dev/null || true)"
  case "$preflight" in
    ""|*"reclaimed 0 task"*) ;;
    *) echo "plan-run: pre-flight reclaim of expired claim(s)"
       printf '%s\n' "$preflight" ;;
  esac
fi

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
  if [ "$state" = claimed ] && [ "$LAND" -eq 1 ] && [ "$AUTO_REPAIR" -eq 1 ]; then
    POSSIBLE_REPAIR_RESUME="$(printf '%s' "$task_json" | python3 -c '
import json,sys
d=json.load(sys.stdin)
count=d.get("repair_count",0)
print(1 if count == 1 and not isinstance(count,bool) and d.get("lease_id") and d.get("review_lease_id") == d.get("lease_id") else 0)
')"
  fi
  if [ -n "$EXECUTOR_PLAN_TASK" ]; then
    if [ "$state" = review ] && [ "$LAND" -eq 1 ]; then
      CONTINUE_REVIEW=1
    else
      [ "$state" = claimed ] || fail "plan-bound executor task $TASK_ID is $state, not claimed"
    fi
    task_provider="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("provider", ""))')"
    [ "$task_provider" = "$TO" ] || fail "task $TASK_ID claim provider is $task_provider, not $TO"
  else
    if [ "$state" = review ] && [ "$LAND" -eq 1 ]; then
      CONTINUE_REVIEW=1
    elif [ "$POSSIBLE_REPAIR_RESUME" -eq 1 ]; then
      : # Exact repair marker is validated with the full receipt below.
    else
      [ "$state" = ready ] || fail "task $TASK_ID is $state, not ready; the audited exits: oms agent-plan release --id $TASK_ID requeues it (review evidence kept) for a fresh drive, review+--land continues from the stored patch, and --auto-repair owns the in-drive repair loop"
    fi
  fi
  if [ "$CONTINUE_REVIEW" -eq 1 ]; then
    task_provider="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("provider", ""))')"
    [ "$task_provider" = "$TO" ] ||
      fail "reviewed task $TASK_ID provider is $task_provider, not $TO"
  fi
  if [ "$POSSIBLE_REPAIR_RESUME" -eq 1 ]; then
    task_provider="$(printf '%s' "$task_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("provider", ""))')"
    [ "$task_provider" = "$TO" ] ||
      fail "interrupted repair task $TASK_ID provider is $task_provider, not $TO"
  fi
  if [ "$DRY_RUN" -eq 0 ] && [ -z "$EXECUTOR_PLAN_TASK" ] &&
    [ "$CONTINUE_REVIEW" -eq 0 ] && [ "$POSSIBLE_REPAIR_RESUME" -eq 0 ]; then
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" claim --id "$TASK_ID" --provider "$TO" >/dev/null
    CLAIMED=1
    task_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID")"
  fi
fi

task_values="$(printf '%s' "$task_json" | python3 -c 'import json,sys; d=json.load(sys.stdin); print("\t".join([d.get("state", ""), d.get("lease_id", ""), d.get("verify", ""), ",".join(d.get("allowed_paths", [])), d.get("title", ""), d.get("artifact", ""), d.get("patch", ""), d.get("executor_id", ""), d.get("executor_soul_sha256", ""), str(d.get("repair_count", 0)), d.get("review_lease_id", ""), d.get("repair_artifact", "")]))')"
state="$(printf '%s' "$task_values" | cut -f1)"
LEASE_ID="$(printf '%s' "$task_values" | cut -f2)"
VERIFY="$(printf '%s' "$task_values" | cut -f3)"
ALLOWED="$(printf '%s' "$task_values" | cut -f4)"
TITLE="$(printf '%s' "$task_values" | cut -f5)"
artifact="$(printf '%s' "$task_values" | cut -f6)"
patch="$(printf '%s' "$task_values" | cut -f7)"
review_executor_id="$(printf '%s' "$task_values" | cut -f8)"
review_executor_soul="$(printf '%s' "$task_values" | cut -f9)"
repair_count="$(printf '%s' "$task_values" | cut -f10)"
review_lease_id="$(printf '%s' "$task_values" | cut -f11)"
repair_artifact="$(printf '%s' "$task_values" | cut -f12)"
[ -n "$ALLOWED" ] || fail "task $TASK_ID must declare non-empty allowed_paths"
[ -n "$VERIFY" ] || fail "task $TASK_ID must declare a mechanical verify command"
# Admission runs the same verify command a second time against a worktree where
# the verification surface has been restored from the base. A verify that names
# a file only this task creates therefore cannot be admitted at all: the floor
# deletes the very command it then runs. That verdict costs a full worker run to
# reach, and the precondition is knowable here, before the provider is called.
verify_missing="$(python3 "$ROOT/scripts/lib/verify-base-precondition.py" "$REPO" "$VERIFY" "$ALLOWED" | tr -d '\r')"
[ -z "$verify_missing" ] ||
  fail "task $TASK_ID verify names $verify_missing, which does not exist at the base commit; admission restores the verification surface from base, so this task can never be admitted — name a check that exists at base (a committed suite arm) instead"
if [ "$state" = claimed ] && [ "$repair_count" = 1 ] &&
  [ -n "$LEASE_ID" ] && [ "$review_lease_id" = "$LEASE_ID" ]; then
  RESUME_REPAIR=1
  [ "$LAND" -eq 1 ] && [ "$AUTO_REPAIR" -eq 1 ] ||
    fail "interrupted bounded repair requires --land --auto-repair"
fi
if [ "$CONTINUE_REVIEW" -eq 1 ] || [ "$RESUME_REPAIR" -eq 1 ]; then
  [ -n "$LEASE_ID" ] || fail "reviewed task $TASK_ID has no active lease"
  [ -n "$artifact" ] && [ -n "$patch" ] || fail "reviewed task $TASK_ID is missing artifact/patch evidence"
  if [ -n "$review_executor_id$review_executor_soul" ]; then
    [ -n "$review_executor_id" ] && [ -n "$review_executor_soul" ] ||
      fail "reviewed task $TASK_ID has an incomplete executor receipt"
  fi
  [ "$EXECUTOR_ID" = "$review_executor_id" ] || {
    if [ -n "$review_executor_id" ]; then
      fail "reviewed task $TASK_ID requires executor $review_executor_id"
    fi
    fail "reviewed task $TASK_ID was not produced by an executor"
  }
  if [ -n "$review_executor_id" ]; then
    current_executor_soul="$(printf '%s' "$executor_meta" |
      python3 -c 'import json,sys;print(json.load(sys.stdin).get("soul_sha256", ""))')"
    [ "$current_executor_soul" = "$review_executor_soul" ] ||
      fail "reviewed task $TASK_ID executor soul receipt does not match $EXECUTOR_ID"
  fi
fi

# The plan row is the durable repair intent. If a prior plan-run stopped after
# review -> claimed but before done -> frozen, reconcile the executor under the
# exact retained lease/scope/verifier/receipt. agent-executor repair is
# idempotent only for frozen+repair_count=1, so retries cannot re-arm twice.
if [ "$RESUME_REPAIR" -eq 1 ]; then
  echo "plan-run: resuming interrupted bounded repair task=$TASK_ID lease=$LEASE_ID"
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$EXECUTOR_ID" ]; then
    "$ROOT/scripts/agent-executor.sh" repair --repo "$REPO" --id "$EXECUTOR_ID" >/dev/null ||
      fail "could not reconcile interrupted executor repair $EXECUTOR_ID"
    executor_meta="$($ROOT/scripts/agent-executor.sh show --repo "$REPO" --id "$EXECUTOR_ID")" ||
      fail "cannot reread reconciled executor $EXECUTOR_ID"
    if [ "${OMS_PLAN_RUN_TEST_STOP_AFTER_EXECUTOR_REPAIR:-0}" = 1 ]; then
      echo "plan-run: injected stop after reconciled executor repair" >&2
      exit 76
    fi
  fi
fi

base="$(git -C "$REPO" rev-parse HEAD)"
verify_hash="$(printf '%s' "$VERIFY" | oms_sha256_stream | cut -c1-16)"
contract_hash="$(printf '%s' "$task_json" | python3 -c '
import hashlib,json,sys
d=json.load(sys.stdin)
keys=("id","title","brief","depends","allowed_paths","forbidden_paths","verify","role")
stable={k:d.get(k) for k in keys}
print(hashlib.sha256(json.dumps(stable,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()[:16])
')"
if [ -n "$EXECUTOR_ID" ]; then
  route_contract="$(printf '%s' "$executor_meta" | python3 -c '
import json,sys
d=json.load(sys.stdin)
keys=("executor_id","provider","strategy","mode","plan_task","task_id","base_sha","allowed_paths","forbidden_paths","verify","model_class","model","fallback_model","reasoning_effort","fallback_reasoning_effort","soul_sha256")
print("|".join(str(d.get(k,"")) for k in keys))
')|request=$MODEL:$FALLBACK_MODEL:$REASONING_EFFORT"
else
  task_role="$(printf '%s' "$task_json" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("role", ""))')"
  export OMS_MODEL_EXPLICIT="$MODEL"
  export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL"
  export OMS_REASONING_EFFORT_REQUEST="$REASONING_EFFORT" OMS_REASONING_FALLBACK_EXPLICIT=""
  export OMS_MODEL_ROLE="$task_role" OMS_MODEL_OPERATION=delegate
  oms_model_prepare "$TO" || exit $?
  route_contract="$OMS_MODEL_RESOLVED_CLASS:$OMS_MODEL_PRIMARY:$OMS_MODEL_FALLBACK:$OMS_REASONING_RESOLVED:$OMS_REASONING_FALLBACK"
fi
route_hash="$(printf '%s' "$route_contract" | oms_sha256_stream | cut -c1-16)"
FAIL_CMD="plan-run task=$TASK_ID base=$base provider=$TO verify=$verify_hash contract=$contract_hash route=$route_hash"
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
  if [ "$CONTINUE_REVIEW" -eq 1 ]; then
    echo "plan-run: dry-run; would continue the stored review without a provider call before landing"
  else
    echo "plan-run: dry-run; no claim or provider call"
  fi
  exit 0
fi

delegate_script="${OMS_PLAN_RUN_DELEGATE:-$ROOT/scripts/peer-delegate.sh}"
[ -x "$delegate_script" ] || fail "delegate command is not executable: $delegate_script"
delegate_repair="$REPAIR"
[ "$RESUME_REPAIR" -eq 0 ] || delegate_repair=0
delegate_common_cmd=("$delegate_script" --repo "$REPO" --to "$TO" --plan-task "$TASK_ID")
[ -n "$EXECUTOR_ID" ] && delegate_common_cmd+=(--executor "$EXECUTOR_ID")
[ -n "$MODEL" ] && delegate_common_cmd+=(--model "$MODEL")
[ -n "$FALLBACK_MODEL" ] && delegate_common_cmd+=(--fallback-model "$FALLBACK_MODEL")
[ -n "$REASONING_EFFORT" ] && [ "$REASONING_EFFORT" != auto ] &&
  delegate_common_cmd+=(--reasoning-effort "$REASONING_EFFORT")
delegate_cmd=("${delegate_common_cmd[@]}" --repair "$delegate_repair")
if [ "$RESUME_REPAIR" -eq 1 ]; then
  resume_review_artifact="$repair_artifact"
  [ -n "$resume_review_artifact" ] && [ -f "$resume_review_artifact" ] ||
    resume_review_artifact="$artifact"
  [ -n "$resume_review_artifact" ] && [ -f "$resume_review_artifact" ] ||
    fail "interrupted repair has no readable review artifact"
  delegate_cmd+=(--review-artifact "$resume_review_artifact" --terminalize-resumed-repair)
fi
if [ "$CONTINUE_REVIEW" -eq 1 ]; then
  echo "plan-run: continuing stored review; no provider call before landing"
else
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
    if [ "$RESUME_REPAIR" -eq 1 ] && ! terminalize_resumed_repair_failure; then
      (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record --kind plan-run --cmd "$FAIL_CMD" --exit "$delegate_status" \
        --summary "resumed repair failed without terminal convergence: task $TASK_ID on $TO") || true
      fail "resumed bounded repair failed and could not be terminalized safely"
    fi
    (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record --kind plan-run --cmd "$FAIL_CMD" --exit "$delegate_status" \
      --summary "plan-run failed: task $TASK_ID on $TO") || true
    echo "plan-run: failed task=$TASK_ID exit=$delegate_status next=inspect-failure" >&2
    exit "$delegate_status"
  fi
fi

# peer-delegate moves a successful task to review and stores its patch under
# the same lease. Preserve that review state unless explicit landing succeeds.
KEEP_CLAIM=1
if [ "$LAND" -eq 1 ]; then
  land_cmd=("$ROOT/scripts/patch-land.sh" --repo "$REPO" --plan-task "$TASK_ID" --verify "$VERIFY")
  # A task whose scope legitimately includes its own regression file cannot
  # land through this front door without the admission override; the flag is
  # forwarded verbatim, never implied (the live campaign hit exactly this).
  [ "$ALLOW_VERIFIER_CHANGE" -eq 0 ] || land_cmd+=(--allow-verifier-change)
  [ -n "$EXECUTOR_ID" ] && land_cmd+=(--executor "$EXECUTOR_ID")
  land_log="$(agent_memory_mktemp)" || exit 1
  set +e
  "${land_cmd[@]}" >"$land_log" 2>&1
  land_status=$?
  set -e
  cat "$land_log"
  if [ "$land_status" -ne 0 ] && [ "$AUTO_REPAIR" -eq 1 ] && [ "$RESUME_REPAIR" -eq 0 ]; then
    # One repair round, never a loop: the failed gate's own output is the
    # review finding, embedded in the repair brief the same way peer-review
    # findings are; a second failure parks the task for an outside read.
    echo "plan-run: landing failed; one auto-repair round with the gate output embedded"
    repair_status=0
    set +e
    "$ROOT/scripts/agent-plan.sh" --repo "$REPO" repair --id "$TASK_ID" \
      --lease-id "$LEASE_ID" --artifact "$land_log" >/dev/null
    repair_status=$?
    if [ "$repair_status" -eq 0 ] &&
      [ "${OMS_PLAN_RUN_TEST_STOP_AFTER_PLAN_REPAIR:-0}" = 1 ]; then
      echo "plan-run: injected stop after durable plan repair" >&2
      exit 75
    fi
    if [ "$repair_status" -eq 0 ] && [ -n "$EXECUTOR_ID" ]; then
      "$ROOT/scripts/agent-executor.sh" repair --repo "$REPO" --id "$EXECUTOR_ID" >/dev/null
      repair_status=$?
      if [ "$repair_status" -ne 0 ]; then
        # The task transition retained the prior review evidence. Put it back
        # in review under the same lease when the executor cannot be re-armed;
        # never continue by dropping the frozen executor contract.
        "$ROOT/scripts/agent-plan.sh" --repo "$REPO" review --id "$TASK_ID" \
          --lease-id "$LEASE_ID" --executor-id "$review_executor_id" \
          --executor-soul-sha256 "$review_executor_soul" >/dev/null 2>&1 || true
      fi
    fi
    if [ "$repair_status" -eq 0 ]; then
      repair_log="$(agent_memory_mktemp)"
      if [ -z "$repair_log" ]; then
        repair_status=1
      else
        repair_delegate_cmd=("${delegate_common_cmd[@]}" --repair 0 \
          --review-artifact "$land_log" --terminalize-resumed-repair)
        "${repair_delegate_cmd[@]}" >"$repair_log" 2>&1 &
        CHILD_PID="$!"
        wait "$CHILD_PID"
        repair_status=$?
        CHILD_PID=""
        cat "$repair_log"
        if [ "$repair_status" -eq 0 ]; then
          repaired_artifact="$(awk -F': ' '$1 == "artifact" {v=$2} END {print v}' "$repair_log")"
          repaired_patch="$(awk -F': ' '$1 == "patch" {v=$2} END {print v}' "$repair_log")"
          [ -z "$repaired_artifact" ] || artifact="$repaired_artifact"
          [ -z "$repaired_patch" ] || patch="$repaired_patch"
          "${land_cmd[@]}"
          land_status=$?
        fi
        rm -f "$repair_log"
      fi
    fi
    if [ "$repair_status" -ne 0 ]; then
      # A worker-side repair failure normally releases the plan task to ready.
      # Park it explicitly so a goal driver cannot claim it again and exceed
      # the one-shot repair contract. Artifact/patch fields remain available
      # for the outside read named below.
      "$ROOT/scripts/agent-plan.sh" --repo "$REPO" block --id "$TASK_ID" \
        --lease-id "$LEASE_ID" --reason "bounded landing repair failed" >/dev/null 2>&1 || true
      if [ -n "$EXECUTOR_ID" ]; then
        executor_state="$($ROOT/scripts/agent-executor.sh show --repo "$REPO" \
          --id "$EXECUTOR_ID" 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", ""))' 2>/dev/null || true)"
        case "$executor_state" in
          frozen|running)
            "$ROOT/scripts/agent-executor.sh" fail --repo "$REPO" --id "$EXECUTOR_ID" \
              --reason "bounded landing repair failed" >/dev/null 2>&1 || true
            ;;
        esac
      fi
    fi
    set -e
  fi
  rm -f "$land_log"
  if [ "$land_status" -ne 0 ]; then
    (cd "$REPO" && "$ROOT/scripts/fail-ledger.sh" record --kind plan-run --cmd "$FAIL_CMD" --exit "$land_status" \
      --summary "landing failed for task $TASK_ID after bounded repair" \
      --next "get an outside read: oms advise, then decide land vs abandon") || true
    parked_state="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$TASK_ID" 2>/dev/null |
      python3 -c 'import json,sys; print(json.load(sys.stdin).get("state", "unknown"))' 2>/dev/null || echo unknown)"
    echo "plan-run: parked task=$TASK_ID state=$parked_state next=advise" >&2
    exit "$land_status"
  fi
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
