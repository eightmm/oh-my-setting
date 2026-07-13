#!/usr/bin/env bash
set -euo pipefail

# Task-scoped executor souls. The generated SOUL.md controls behavior; the
# parent-owned meta.json is the only authority for provider, scope, task lease,
# verification, and lifecycle. Frozen souls are hash-checked before every use.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT/scripts/lib/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT/scripts/lib/oms-common.sh"
# shellcheck source=scripts/lib/file-lock.sh
. "$ROOT/scripts/lib/file-lock.sh"
# shellcheck source=scripts/lib/model-routing.sh
. "$ROOT/scripts/lib/model-routing.sh"

REPO="${OMS_STATE_REPO:-$PWD}"
ACTION=""
ID=""
PROVIDER=""
STRATEGY=""
STRATEGY_EXPLICIT=0
TASK_ID=""
PLAN_TASK=""
ALLOWED=""
FORBIDDEN=""
VERIFY=""
SOUL_FILE=""
REASON=""
MODE="worktree-write"
MODEL_CLASS=auto
MODEL=""
FALLBACK_MODEL=""
NO_MODEL_FALLBACK=0
REASONING_EFFORT=auto
GC_APPLY=0

usage() {
  cat <<'EOF'
Usage: agent-executor.sh <command> [options]

Commands:
  create    Create a draft executor from a generated --soul-file.
  validate  Validate draft content or a frozen soul hash.
  freeze    Freeze a valid draft; idempotent while the hash matches.
  brief     Print the frozen soul and machine-owned execution contract.
  show      Print meta.json.
  list      List executor id/state/provider/strategy.
  start     Move frozen -> running after rechecking hash and task lease.
  done      Move running -> done.
  fail      Move frozen/running -> failed; accepts --reason.
  gc        Remove aged draft/done/failed executors; keeps frozen/running.

Options:
  --repo PATH        State repository. Default: PWD or OMS_STATE_REPO.
  --id ID            Executor id ([A-Za-z0-9._-]+).
  --provider NAME    codex, claude, or antigravity.
  --strategy NAME    Base strategy resolved by agent-role.sh.
  --task-id ID       Lineage id without plan hydration.
  --plan-task ID     Hydrate task/lease/scope/verify/strategy from agent-plan.
  --allowed LIST     Comma/space-separated allowed project paths.
  --forbidden LIST   Comma/space-separated forbidden project paths.
  --verify CMD       Frozen verification command.
  --soul-file FILE   Model-generated behavioral specialization for create.
  --mode MODE        read or worktree-write. Default: worktree-write.
  --model-class C    auto, fast, balanced, or deep; frozen at create time.
  --model MODEL      Exact provider model.
  --fallback-model M Explicit one-shot capacity fallback model.
  --no-model-fallback Disable implicit class fallback.
  --reasoning-effort E auto, low, medium, or high; frozen at create time.
  --reason TEXT      Failure reason for fail.
  --days N           Retention age for gc. Default: 30.
  --dry-run          Print executor gc removals without deleting (default).
  --apply            Apply executor gc removals.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }
need_id() {
  [ -n "$ID" ] || fail "--id is required"
  case "$ID" in *[!A-Za-z0-9._-]*|"") fail "--id must match [A-Za-z0-9._-]+" ;; esac
}

DAYS=30
while [ "$#" -gt 0 ]; do
  case "$1" in
    create|validate|freeze|brief|show|list|start|done|fail|gc)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"; ACTION="$1"; shift ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires path"; REPO="$2"; shift 2 ;;
    --id) [ "$#" -ge 2 ] || fail "--id requires value"; ID="$2"; shift 2 ;;
    --provider) [ "$#" -ge 2 ] || fail "--provider requires name"; PROVIDER="$2"; shift 2 ;;
    --strategy) [ "$#" -ge 2 ] || fail "--strategy requires name"; STRATEGY="$2"; STRATEGY_EXPLICIT=1; shift 2 ;;
    --task-id) [ "$#" -ge 2 ] || fail "--task-id requires id"; TASK_ID="$2"; shift 2 ;;
    --plan-task) [ "$#" -ge 2 ] || fail "--plan-task requires id"; PLAN_TASK="$2"; shift 2 ;;
    --allowed) [ "$#" -ge 2 ] || fail "--allowed requires paths"; ALLOWED="$2"; shift 2 ;;
    --forbidden) [ "$#" -ge 2 ] || fail "--forbidden requires paths"; FORBIDDEN="$2"; shift 2 ;;
    --verify) [ "$#" -ge 2 ] || fail "--verify requires command"; VERIFY="$2"; shift 2 ;;
    --soul-file) [ "$#" -ge 2 ] || fail "--soul-file requires file"; SOUL_FILE="$2"; shift 2 ;;
    --mode) [ "$#" -ge 2 ] || fail "--mode requires read|worktree-write"; MODE="$2"; shift 2 ;;
    --model-class) [ "$#" -ge 2 ] || fail "--model-class requires value"; MODEL_CLASS="$2"; shift 2 ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires value"; MODEL="$2"; shift 2 ;;
    --fallback-model) [ "$#" -ge 2 ] || fail "--fallback-model requires value"; FALLBACK_MODEL="$2"; shift 2 ;;
    --no-model-fallback) NO_MODEL_FALLBACK=1; shift ;;
    --reasoning-effort) [ "$#" -ge 2 ] || fail "--reasoning-effort requires value"; REASONING_EFFORT="$2"; shift 2 ;;
    --reason) [ "$#" -ge 2 ] || fail "--reason requires text"; REASON="$2"; shift 2 ;;
    --days) [ "$#" -ge 2 ] || fail "--days requires integer"; DAYS="$2"; shift 2 ;;
    --dry-run) GC_APPLY=0; shift ;;
    --apply) GC_APPLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
[ -n "$ACTION" ] || { usage >&2; exit 2; }
case "$MODE" in read|worktree-write) ;; *) fail "--mode must be read or worktree-write" ;; esac
oms_model_validate_class "$MODEL_CLASS" || exit $?
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
case "$DAYS" in *[!0-9]*|"") fail "--days must be a non-negative integer" ;; esac
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
STATE="$REPO/.oms/executors"

if [ "$ACTION" = "list" ]; then
  [ -d "$STATE" ] || exit 0
  python3 - "$STATE" <<'PY'
import glob, json, os, sys
for p in sorted(glob.glob(os.path.join(sys.argv[1], "*", "meta.json"))):
    try: d = json.load(open(p, encoding="utf-8"))
    except Exception: continue
    print("%s\t%s\t%s\t%s" % (d.get("executor_id", ""), d.get("state", ""), d.get("provider", ""), d.get("strategy", "")))
PY
  exit 0
fi

if [ "$ACTION" = "gc" ]; then
  [ -d "$STATE" ] || { echo "executor-gc: nothing to do"; exit 0; }
  OMS_EXECUTOR_DAYS="$DAYS" OMS_EXECUTOR_GC_APPLY="$GC_APPLY" python3 - "$STATE" <<'PY'
import json, os, shutil, sys, time
root = sys.argv[1]; cutoff = time.time() - int(os.environ["OMS_EXECUTOR_DAYS"]) * 86400
apply = os.environ["OMS_EXECUTOR_GC_APPLY"] == "1"
removed = 0
for name in sorted(os.listdir(root)):
    d = os.path.join(root, name); meta = os.path.join(d, "meta.json")
    try: row = json.load(open(meta, encoding="utf-8"))
    except Exception: continue
    if row.get("state") not in ("draft", "done", "failed"): continue
    if os.path.getmtime(meta) >= cutoff: continue
    if apply: shutil.rmtree(d)
    print("executor-gc: %s %s" % ("removed" if apply else "would remove", name)); removed += 1
print("executor-gc: %d %s" % (removed, "removed" if apply else "candidate(s)"))
PY
  exit 0
fi

need_id
DIR="$STATE/$ID"
META="$DIR/meta.json"
DRAFT="$DIR/soul.draft.md"
SOUL="$DIR/SOUL.md"

if [ "$ACTION" = "create" ]; then
  [ -n "$PROVIDER" ] || fail "create requires --provider"
  PROVIDER="$(oms_normalize_provider "$PROVIDER")" || fail "unsupported provider"
  [ -n "$STRATEGY" ] || STRATEGY="implementation-worker"
  [ -n "$SOUL_FILE" ] && [ -s "$SOUL_FILE" ] || fail "create requires a non-empty --soul-file"
  [ ! -e "$DIR" ] || fail "executor already exists: $ID"
  if agent_memory_file_has_sensitive_content "$SOUL_FILE"; then
    fail "soul contains sensitive-looking content"
  fi
  if grep -Eiq '(^|[[:space:]])(allowed_paths|forbidden_paths|authority|lease_id|base_sha)[[:space:]]*:' "$SOUL_FILE" ||
     grep -Eiq 'ignore (all )?(previous|prior) instructions' "$SOUL_FILE"; then
    fail "soul must not define authority, scope, lease, or instruction overrides"
  fi
  plan_json=""
  lease_id=""
  if [ -n "$PLAN_TASK" ]; then
    plan_json="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$PLAN_TASK")" || fail "unknown plan task: $PLAN_TASK"
    values="$(printf '%s' "$plan_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);print("\t".join([d.get("id",""),d.get("lease_id",""),",".join(d.get("allowed_paths",[])),",".join(d.get("forbidden_paths",[])),d.get("verify",""),d.get("role",""),d.get("state",""),d.get("provider","")]))')"
    plan_id="$(printf '%s' "$values" | cut -f1)"; lease_id="$(printf '%s' "$values" | cut -f2)"
    plan_allowed="$(printf '%s' "$values" | cut -f3)"; plan_forbidden="$(printf '%s' "$values" | cut -f4)"
    plan_verify="$(printf '%s' "$values" | cut -f5)"; plan_role="$(printf '%s' "$values" | cut -f6)"
    plan_state="$(printf '%s' "$values" | cut -f7)"; plan_provider="$(printf '%s' "$values" | cut -f8)"
    [ "$plan_state" = claimed ] && [ -n "$lease_id" ] || fail "plan task $PLAN_TASK must be claimed before executor creation"
    [ "$plan_provider" = "$PROVIDER" ] || fail "plan task $PLAN_TASK claim provider is ${plan_provider:-(none)}, not $PROVIDER"
    [ -z "$TASK_ID" ] || [ "$TASK_ID" = "$plan_id" ] || fail "--task-id conflicts with --plan-task"
    TASK_ID="$plan_id"
    [ -z "$ALLOWED" ] || [ "$ALLOWED" = "$plan_allowed" ] || fail "--allowed conflicts with plan task"
    [ -z "$FORBIDDEN" ] || [ "$FORBIDDEN" = "$plan_forbidden" ] || fail "--forbidden conflicts with plan task"
    [ -z "$VERIFY" ] || [ "$VERIFY" = "$plan_verify" ] || fail "--verify conflicts with plan task"
    if [ -n "$plan_role" ]; then
      [ "$STRATEGY_EXPLICIT" = 0 ] || [ "$STRATEGY" = "$plan_role" ] || fail "--strategy conflicts with plan task"
      STRATEGY="$plan_role"
    fi
    ALLOWED="$plan_allowed"; FORBIDDEN="$plan_forbidden"; VERIFY="$plan_verify"
  fi
  if ! OMS_SCOPE_ALLOWED="$ALLOWED" OMS_SCOPE_FORBIDDEN="$FORBIDDEN" python3 <<'PY'
import os, re
for raw in (os.environ["OMS_SCOPE_ALLOWED"], os.environ["OMS_SCOPE_FORBIDDEN"]):
    for p in re.split(r"[,\s]+", raw):
        if not p: continue
        if p.startswith("/") or ".." in p.split("/") or "\\" in p:
            raise SystemExit(1)
PY
  then
    fail "unsafe scope path"
  fi
  role_file="$($ROOT/scripts/agent-role.sh --repo "$REPO" --name "$STRATEGY" resolve)" ||
    fail "unknown strategy: $STRATEGY"
  export OMS_MODEL_CLASS_REQUEST="$MODEL_CLASS" OMS_MODEL_EXPLICIT="$MODEL"
  export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL" OMS_MODEL_NO_FALLBACK="$NO_MODEL_FALLBACK"
  export OMS_REASONING_EFFORT_REQUEST="$REASONING_EFFORT"
  export OMS_MODEL_ROLE="$STRATEGY" OMS_MODEL_OPERATION=delegate
  oms_model_prepare "$PROVIDER" || exit $?
  base_sha="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
  mkdir -p "$STATE"
  agent_memory_ensure_oms_ignore_for_path "$STATE" 2>/dev/null || true
  tmp="$STATE/.${ID}.tmp.$$"
  mkdir "$tmp"
  {
    printf '# Executor Soul: %s\n\nEXECUTOR-SOUL\n\n' "$ID"
    printf '## Base Strategy\n\n'; cat "$role_file"
    printf '\n\n## Task Specialization\n\n'; cat "$SOUL_FILE"; printf '\n'
  } > "$tmp/soul.draft.md"
  OMS_EXECUTOR_META="$tmp/meta.json" OMS_EXECUTOR_ID="$ID" OMS_EXECUTOR_PROVIDER="$PROVIDER" \
    OMS_EXECUTOR_STRATEGY="$STRATEGY" OMS_EXECUTOR_MODE="$MODE" OMS_EXECUTOR_TASK="$TASK_ID" \
    OMS_EXECUTOR_PLAN="$PLAN_TASK" OMS_EXECUTOR_LEASE="$lease_id" OMS_EXECUTOR_BASE="$base_sha" \
    OMS_EXECUTOR_ALLOWED="$ALLOWED" OMS_EXECUTOR_FORBIDDEN="$FORBIDDEN" OMS_EXECUTOR_VERIFY="$VERIFY" \
    OMS_EXECUTOR_MODEL_CLASS="$OMS_MODEL_RESOLVED_CLASS" OMS_EXECUTOR_MODEL="$OMS_MODEL_PRIMARY" \
    OMS_EXECUTOR_FALLBACK_MODEL="$OMS_MODEL_FALLBACK" \
    OMS_EXECUTOR_REASONING_EFFORT="$OMS_REASONING_RESOLVED" \
    OMS_EXECUTOR_FALLBACK_REASONING_EFFORT="$OMS_REASONING_FALLBACK" \
    python3 <<'PY'
import json, os, re, time
def paths(raw):
    out=[]
    for p in re.split(r"[,\s]+", raw):
        p=p.strip()
        if not p: continue
        if p.startswith("/") or ".." in p.split("/") or "\\" in p:
            raise SystemExit("error: unsafe scope path: %s" % p)
        out.append(p)
    return out
now=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
d={"schema":1,"executor_id":os.environ["OMS_EXECUTOR_ID"],"state":"draft",
"provider":os.environ["OMS_EXECUTOR_PROVIDER"],"strategy":os.environ["OMS_EXECUTOR_STRATEGY"],
"mode":os.environ["OMS_EXECUTOR_MODE"],"task_id":os.environ["OMS_EXECUTOR_TASK"],
"plan_task":os.environ["OMS_EXECUTOR_PLAN"],"lease_id":os.environ["OMS_EXECUTOR_LEASE"],
"base_sha":os.environ["OMS_EXECUTOR_BASE"],"allowed_paths":paths(os.environ["OMS_EXECUTOR_ALLOWED"]),
"forbidden_paths":paths(os.environ["OMS_EXECUTOR_FORBIDDEN"]),"verify":os.environ["OMS_EXECUTOR_VERIFY"],
"model_class":os.environ["OMS_EXECUTOR_MODEL_CLASS"],"model":os.environ["OMS_EXECUTOR_MODEL"],
"fallback_model":os.environ["OMS_EXECUTOR_FALLBACK_MODEL"],
"reasoning_effort":os.environ["OMS_EXECUTOR_REASONING_EFFORT"],
"fallback_reasoning_effort":os.environ["OMS_EXECUTOR_FALLBACK_REASONING_EFFORT"],
"soul_sha256":"","created_at":now,"updated_at":now,"reason":""}
with open(os.environ["OMS_EXECUTOR_META"],"w",encoding="utf-8") as f: json.dump(d,f,indent=2,ensure_ascii=False)
PY
  if ! OMS_EXECUTOR_TMP="$tmp" OMS_EXECUTOR_DIR="$DIR" python3 <<'PY'
import os
os.rename(os.environ["OMS_EXECUTOR_TMP"], os.environ["OMS_EXECUTOR_DIR"])
PY
  then
    rm -rf "$tmp"
    fail "could not create executor (id already exists?)"
  fi
  echo "executor: created $ID (draft)"
  exit 0
fi

[ -f "$META" ] || fail "executor not found: $ID"

validate_soul_hash() {
  local expected actual state
  state="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("state",""))' "$META")"
  case "$state" in frozen|running|done|failed) ;; *) fail "executor $ID is not frozen" ;; esac
  expected="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("soul_sha256",""))' "$META")"
  [ -f "$SOUL" ] || fail "frozen soul missing: $ID"
  actual="$(oms_sha256_file "$SOUL")"
  [ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "soul hash mismatch for executor $ID"
}

validate_frozen() {
  local expected_base current_base
  validate_soul_hash
  expected_base="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("base_sha",""))' "$META")"
  current_base="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
  [ -z "$expected_base" ] || [ "$expected_base" = "$current_base" ] || fail "base sha mismatch for executor $ID"
}

validate_plan_lease() {
  local plan task lease values
  values="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print("\t".join([d.get("plan_task",""),d.get("task_id",""),d.get("lease_id","")]))' "$META")"
  plan="$(printf '%s' "$values" | cut -f1)"; task="$(printf '%s' "$values" | cut -f2)"; lease="$(printf '%s' "$values" | cut -f3)"
  [ -n "$plan" ] || return 0
  current="$($ROOT/scripts/agent-plan.sh --repo "$REPO" show --id "$plan")" || fail "executor plan task missing: $plan"
  current_values="$(printf '%s' "$current" | python3 -c 'import json,sys;d=json.load(sys.stdin);print("\t".join([d.get("id",""),d.get("lease_id","")]))')"
  [ "$(printf '%s' "$current_values" | cut -f1)" = "$task" ] || fail "executor task mismatch"
  [ "$(printf '%s' "$current_values" | cut -f2)" = "$lease" ] || fail "executor task lease mismatch"
}

if [ "$ACTION" = "show" ]; then cat "$META"; exit 0; fi
if [ "$ACTION" = "validate" ]; then
  state="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("state",""))' "$META")"
  if [ "$state" = "draft" ]; then
    [ -s "$DRAFT" ] || fail "executor draft soul is empty"
    [ "$(wc -c < "$DRAFT" | tr -d ' ')" -le 30000 ] || fail "executor soul exceeds 30000 bytes"
    agent_memory_file_has_sensitive_content "$DRAFT" && fail "soul contains sensitive-looking content"
  else
    validate_frozen
    validate_plan_lease
  fi
  echo "executor: valid $ID ($state)"; exit 0
fi

update_state() {
  local from="$1" to="$2"
  OMS_EXECUTOR_META="$META" OMS_EXECUTOR_FROM="$from" OMS_EXECUTOR_TO="$to" OMS_EXECUTOR_REASON="$REASON" \
    python3 <<'PY'
import json, os, tempfile, time
p=os.environ["OMS_EXECUTOR_META"]; d=json.load(open(p,encoding="utf-8"))
allowed=os.environ["OMS_EXECUTOR_FROM"].split(",")
if d.get("state") not in allowed:
    raise SystemExit("error: executor state %s cannot move to %s"%(d.get("state"),os.environ["OMS_EXECUTOR_TO"]))
d["state"]=os.environ["OMS_EXECUTOR_TO"]; d["updated_at"]=time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())
if os.environ["OMS_EXECUTOR_REASON"]: d["reason"]=os.environ["OMS_EXECUTOR_REASON"]
fd,tmp=tempfile.mkstemp(dir=os.path.dirname(p))
with os.fdopen(fd,"w",encoding="utf-8") as f: json.dump(d,f,indent=2,ensure_ascii=False)
os.replace(tmp,p)
PY
}

if [ "$ACTION" = "freeze" ]; then
  state="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("state",""))' "$META")"
  if [ "$state" != "draft" ]; then validate_frozen; echo "executor: frozen $ID"; exit 0; fi
  "$0" validate --repo "$REPO" --id "$ID" >/dev/null
  validate_plan_lease
  cp "$DRAFT" "$SOUL"
  soul_hash="$(oms_sha256_file "$SOUL")"
  OMS_EXECUTOR_META="$META" OMS_SOUL_HASH="$soul_hash" python3 <<'PY'
import json, os, tempfile, time
p=os.environ["OMS_EXECUTOR_META"]; d=json.load(open(p,encoding="utf-8"))
if d.get("state") != "draft": raise SystemExit("error: executor is not draft")
d["state"]="frozen"; d["soul_sha256"]=os.environ["OMS_SOUL_HASH"]
d["updated_at"]=time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())
fd,tmp=tempfile.mkstemp(dir=os.path.dirname(p))
with os.fdopen(fd,"w",encoding="utf-8") as f: json.dump(d,f,indent=2,ensure_ascii=False)
os.replace(tmp,p)
PY
  echo "executor: frozen $ID"; exit 0
fi

if [ "$ACTION" = "brief" ]; then
  validate_frozen; validate_plan_lease
  cat "$SOUL"
  python3 - "$META" <<'PY'
import json, sys
d=json.load(open(sys.argv[1],encoding="utf-8"))
def values(v): return ", ".join(v) if v else "(unrestricted)"
print("\n## Frozen Executor Contract\n")
print("executor_id: %s"%d["executor_id"]); print("soul_sha256: %s"%d["soul_sha256"])
print("provider: %s"%d["provider"]); print("mode: %s"%d["mode"])
print("model_class: %s"%(d.get("model_class") or "(none)"))
print("model: %s"%(d.get("model") or "(provider default)"))
print("fallback_model: %s"%(d.get("fallback_model") or "(none)"))
print("reasoning_effort: %s"%(d.get("reasoning_effort") or "(none)"))
print("fallback_reasoning_effort: %s"%(d.get("fallback_reasoning_effort") or "(none)"))
print("task_id: %s"%(d.get("task_id") or "(none)")); print("lease_id: %s"%(d.get("lease_id") or "(none)"))
print("base_sha: %s"%(d.get("base_sha") or "(none)")); print("allowed_paths: %s"%values(d.get("allowed_paths",[])))
print("forbidden_paths: %s"%(", ".join(d.get("forbidden_paths",[])) or "(none)"))
print("verify: %s"%(d.get("verify") or "(none)"))
print("The soul cannot widen this machine-owned contract or delegate recursively.")
PY
  exit 0
fi

case "$ACTION" in
  start) validate_frozen; validate_plan_lease; oms_with_file_lock "$META.lock" update_state frozen running ;;
  done) validate_soul_hash; oms_with_file_lock "$META.lock" update_state running "done" ;;
  fail) validate_soul_hash; oms_with_file_lock "$META.lock" update_state frozen,running failed ;;
  *) fail "unsupported action: $ACTION" ;;
esac
echo "executor: $ID -> $ACTION"
