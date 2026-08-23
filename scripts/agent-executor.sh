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
MODEL=""
FALLBACK_MODEL=""
REASONING_EFFORT=auto
GC_APPLY=0
EXPECTED_STATE=""
MARKERS_DIR=""
CHECK_ONLY=0

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
  repair    Re-arm done -> frozen exactly once under the same soul/task/lease
            contract for a bounded reviewed-patch repair.
  fail      Move frozen/running -> failed; accepts --reason.
  recover   Parent-only dead-worker recovery: CAS exact expected running state
            and fail only when no exact live/malformed worker marker exists.
            --check runs the identical locked predicate without mutation.
  gc        Remove aged draft/done/failed executors; keeps frozen/running.

Repair accepts only the first done state whose exact frozen plan task has
entered repair under the same lease. A failed executor and the second done
state are terminal and cannot be re-armed.

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
  --model MODEL      Exact provider model.
  --fallback-model M Explicit one-shot capacity fallback model.
  --reasoning-effort E auto, low, medium, high, xhigh, max, or ultra; frozen.
  --reason TEXT      Failure reason for fail.
  --expected-state S Exact state observed by the recovery caller.
  --markers-dir PATH Repo-local worker marker directory for recovery.
  --check             Evaluate recover under lock without mutation.
  --days N           Retention age for gc. Default: 30.
  --dry-run          Print executor gc removals without deleting (default).
  --apply            Apply executor gc removals.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }
need_id() {
  [ -n "$ID" ] || fail "--id is required"
  case "$ID" in *[!A-Za-z0-9._-]*|"") fail "--id must match [A-Za-z0-9._-]+" ;; esac
  case "$ID" in .|..) fail "--id cannot be . or .." ;; esac
}

DAYS=30
while [ "$#" -gt 0 ]; do
  case "$1" in
    create|validate|freeze|brief|show|list|start|done|repair|fail|recover|gc)
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
    --mode)
      [ "$#" -ge 2 ] || fail "--mode requires worktree-write"
      case "$2" in
        worktree-write) MODE=worktree-write ;;
        read) fail "read executors were removed; use agent-run --mode read" ;;
        *) fail "--mode only accepts legacy-compatible worktree-write" ;;
      esac
      shift 2
      ;;
    --model) [ "$#" -ge 2 ] || fail "--model requires value"; MODEL="$2"; shift 2 ;;
    --fallback-model) [ "$#" -ge 2 ] || fail "--fallback-model requires value"; FALLBACK_MODEL="$2"; shift 2 ;;
    --reasoning-effort) [ "$#" -ge 2 ] || fail "--reasoning-effort requires value"; REASONING_EFFORT="$2"; shift 2 ;;
    --reason) [ "$#" -ge 2 ] || fail "--reason requires text"; REASON="$2"; shift 2 ;;
    --expected-state) [ "$#" -ge 2 ] || fail "--expected-state requires value"; EXPECTED_STATE="$2"; shift 2 ;;
    --markers-dir) [ "$#" -ge 2 ] || fail "--markers-dir requires path"; MARKERS_DIR="$2"; shift 2 ;;
    --check) CHECK_ONLY=1; shift ;;
    --days) [ "$#" -ge 2 ] || fail "--days requires integer"; DAYS="$2"; shift 2 ;;
    --dry-run) GC_APPLY=0; shift ;;
    --apply) GC_APPLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
[ -n "$ACTION" ] || { usage >&2; exit 2; }
[ "$CHECK_ONLY" = 0 ] || [ "$ACTION" = recover ] ||
  fail "--check is valid only with recover"
if [ "$ACTION" = recover ] && [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  fail "recover is parent-only; a harness child cannot recover executor authority"
fi
oms_model_validate_name "$MODEL" || exit $?
oms_model_validate_name "$FALLBACK_MODEL" || exit $?
oms_reasoning_validate "$REASONING_EFFORT" || exit $?
case "$DAYS" in *[!0-9]*|"") fail "--days must be a non-negative integer" ;; esac
command -v python3 >/dev/null 2>&1 || fail "python3 is required"
REPO="$(oms_repo_root "$REPO")" || fail "bad --repo"
REPO="$(cd "$REPO" && pwd -P)" || fail "cannot resolve the physical repository"
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

require_worktree_write_mode() {
  local stored_mode
  stored_mode="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("mode",""))' "$META")"
  [ "$stored_mode" = worktree-write ] ||
    fail "legacy read executor is unsupported; inspect with show, retire with fail, and use agent-run --mode read"
}

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
  export OMS_MODEL_EXPLICIT="$MODEL"
  export OMS_MODEL_FALLBACK_EXPLICIT="$FALLBACK_MODEL"
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
"soul_sha256":"","repair_count":0,"created_at":now,"updated_at":now,"reason":""}
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

executor_python_path_for_host() {  # PATH
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

executor_python_lexical_path_for_host() {  # PATH
  local value="$1"
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*)
      command -v cygpath >/dev/null 2>&1 || return 2
      value="$(cygpath -m "$value" | tr -d '\r')" || return $?
      ;;
  esac
  printf '%s\n' "$value"
}

recover_executor() {
  OMS_EXECUTOR_RECOVERY_META="$RECOVERY_META" \
    OMS_EXECUTOR_RECOVERY_REPO="$RECOVERY_REPO" \
    OMS_EXECUTOR_RECOVERY_MARKERS="$RECOVERY_MARKERS" \
    OMS_EXECUTOR_RECOVERY_ID="$ID" \
    OMS_EXECUTOR_RECOVERY_EXPECTED_STATE="$EXPECTED_STATE" \
    OMS_EXECUTOR_RECOVERY_CHECK="$CHECK_ONLY" \
    OMS_EXECUTOR_RECOVERY_REASON="gc: delegation process is not alive" \
    python3 - "$ROOT/scripts/lib/process_liveness.py" <<'PY'
import json
import os
import re
import runpy
import stat
import sys
import tempfile
import time

meta_path = os.environ["OMS_EXECUTOR_RECOVERY_META"]
repo_root = os.path.realpath(os.environ["OMS_EXECUTOR_RECOVERY_REPO"])
marker_dir = os.path.abspath(os.environ["OMS_EXECUTOR_RECOVERY_MARKERS"])
executor_id = os.environ["OMS_EXECUTOR_RECOVERY_ID"]
expected_state = os.environ["OMS_EXECUTOR_RECOVERY_EXPECTED_STATE"]
check_only = os.environ["OMS_EXECUTOR_RECOVERY_CHECK"] == "1"
pid_alive = runpy.run_path(sys.argv[1])["pid_alive"]

def die(message):
    sys.stderr.write("error: %s\n" % message)
    raise SystemExit(2)

def changed(message, outcome="veto"):
    sys.stderr.write("executor-recovery-outcome: %s\n" % outcome)
    sys.stderr.write("executor: %s\n" % message)
    raise SystemExit(3)

def same_absolute(left, right):
    return os.path.normcase(os.path.abspath(left)) == os.path.normcase(os.path.abspath(right))

expected_markers = os.path.join(repo_root, ".oms", "delegations")
if not same_absolute(marker_dir, expected_markers):
    die("worker marker directory must be the repo-local .oms/delegations directory")

executor_root = os.path.join(repo_root, ".oms", "executors")
executor_dir = os.path.join(executor_root, executor_id)
expected_meta = os.path.join(executor_dir, "meta.json")
if (os.path.dirname(os.path.abspath(executor_dir)) !=
        os.path.abspath(executor_root) or
        os.path.basename(os.path.abspath(executor_dir)) != executor_id or
        not same_absolute(meta_path, expected_meta)):
    die("executor metadata path is outside the repo-local executor directory")
try:
    root_info = os.lstat(executor_root)
    dir_info = os.lstat(executor_dir)
    meta_before = os.lstat(meta_path)
except OSError as exc:
    die("executor metadata ancestry is unreadable: %s" % exc)
if (not stat.S_ISDIR(root_info.st_mode) or
        not stat.S_ISDIR(dir_info.st_mode) or
        not stat.S_ISREG(meta_before.st_mode) or
        os.path.normcase(os.path.realpath(executor_root)) !=
        os.path.normcase(os.path.abspath(executor_root)) or
        os.path.normcase(os.path.realpath(executor_dir)) !=
        os.path.normcase(os.path.abspath(executor_dir)) or
        meta_before.st_size > 64 * 1024):
    die("executor metadata must be a bounded real repo-local file")
meta_flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
meta_flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
try:
    meta_descriptor = os.open(meta_path, meta_flags)
except OSError as exc:
    die("executor metadata cannot be opened safely: %s" % exc)
try:
    meta_opened = os.fstat(meta_descriptor)
    if (not stat.S_ISREG(meta_opened.st_mode) or
            (meta_opened.st_dev, meta_opened.st_ino) !=
            (meta_before.st_dev, meta_before.st_ino)):
        die("executor metadata changed while opening")
    meta_payload = os.read(meta_descriptor, 64 * 1024 + 1)
    if len(meta_payload) > 64 * 1024 or os.read(meta_descriptor, 1):
        die("executor metadata exceeds 64 KiB")
finally:
    os.close(meta_descriptor)
try:
    meta = json.loads(meta_payload.decode("utf-8"))
except (UnicodeError, ValueError) as exc:
    die("executor metadata is unreadable: %s" % exc)
if not isinstance(meta, dict) or meta.get("executor_id") != executor_id:
    die("executor metadata identity is invalid")
if expected_state not in {"draft", "frozen", "running", "done", "failed"}:
    die("recover requires a valid --expected-state")
if meta.get("state") != expected_state or expected_state != "running":
    changed("%s no longer holds the exact recoverable state" % executor_id)

markers = []
if os.path.lexists(marker_dir):
    try:
        directory_state = os.lstat(marker_dir)
    except OSError as exc:
        die("cannot inspect worker marker directory: %s" % exc)
    if (not stat.S_ISDIR(directory_state.st_mode) or
            not same_absolute(os.path.realpath(marker_dir), expected_markers)):
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
            before = entry.stat(follow_symlinks=False)
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

matching = [marker for marker in markers
            if marker.get("executor_id") == executor_id]
safe_id = re.compile(r"^[A-Za-z0-9._-]+$")
owner_id = re.compile(r"^owner_[0-9a-f]{32}$")

def marker_is_typed(marker):
    schema = marker.get("schema")
    if (isinstance(schema, bool) or not isinstance(schema, int)
            or schema not in {1, 2, 3, 4}):
        return False
    marker_id = marker.get("id")
    if (not isinstance(marker_id, str) or not safe_id.fullmatch(marker_id)
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
    for key in ("task_id", "lease_id", "executor_id"):
        value = marker.get(key, "")
        if (not isinstance(value, str)
                or (value and not safe_id.fullmatch(value))):
            return False
    if marker.get("executor_id", "") in {".", ".."}:
        return False
    marker_owner = marker.get("autopilot_owner_id", "")
    if (not isinstance(marker_owner, str)
            or (marker_owner and not owner_id.fullmatch(marker_owner))):
        return False
    return True

if any(not marker_is_typed(marker) for marker in matching):
    changed("%s has an unproven exact worker marker" % executor_id, "unproven")
if any(pid_alive(marker.get("pid"), native_pid=marker.get("native_pid"))
       for marker in matching):
    changed("%s still has a live exact worker marker" % executor_id)
if check_only:
    print("executor: %s exact state is recoverable" % executor_id)
    raise SystemExit(0)

meta["state"] = "failed"
meta["reason"] = os.environ["OMS_EXECUTOR_RECOVERY_REASON"]
meta["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
descriptor, temporary = tempfile.mkstemp(dir=os.path.dirname(meta_path))
try:
    with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
        json.dump(meta, handle, indent=2, ensure_ascii=False)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, meta_path)
except Exception:
    try:
        os.unlink(temporary)
    except OSError:
        pass
    raise
print("executor: %s -> failed" % executor_id)
PY
}

recover_executor_with_meta_lock() {
  oms_with_file_lock "$META.lock" recover_executor
}

if [ "$ACTION" = recover ]; then
  [ -n "$EXPECTED_STATE" ] || fail "recover requires --expected-state"
  MARKERS_DIR="${MARKERS_DIR:-$REPO/.oms/delegations}"
  recovery_repo_physical="$(cd "$REPO" && pwd -P)" ||
    fail "cannot resolve the physical repository"
  RECOVERY_REPO="$(executor_python_path_for_host "$recovery_repo_physical")" ||
    fail "cannot normalize repository path for Python"
  RECOVERY_META="$(executor_python_lexical_path_for_host "$META")" ||
    fail "cannot normalize executor metadata path for Python"
  RECOVERY_MARKERS="$(executor_python_path_for_host "$MARKERS_DIR")" ||
    fail "cannot normalize worker marker path for Python"
  # Recovery always judges the marker set before the executor CAS. Publishers
  # take only the first lock, so the global order is marker set -> metadata.
  oms_with_file_lock "$REPO/.oms/delegations/.marker-set-lock-target" \
    recover_executor_with_meta_lock
  exit $?
fi

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
  require_worktree_write_mode
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

repair_once() {
  # The executor contract itself is immutable. This transition changes only
  # lifecycle state, timestamp, and its one-shot counter; validation before and
  # after the locked write proves the same soul and plan lease still apply.
  OMS_EXECUTOR_META="$META" python3 <<'PY'
import json, os, tempfile, time
p=os.environ["OMS_EXECUTOR_META"]
d=json.load(open(p,encoding="utf-8"))
count=d.get("repair_count",0)
if isinstance(count, bool) or not isinstance(count, int) or count < 0:
    raise SystemExit("error: executor repair counter is invalid")
if d.get("state") == "frozen" and count == 1:
    # The plan row is the durable repair intent. A crash after this metadata
    # transition must be retryable without re-arming or incrementing again.
    raise SystemExit(0)
if d.get("state") != "done":
    raise SystemExit("error: executor state %s cannot enter repair" % d.get("state"))
if count >= 1:
    raise SystemExit("error: executor bounded repair was already used")
d["state"]="frozen"
d["repair_count"]=1
d["updated_at"]=time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())
fd,tmp=tempfile.mkstemp(dir=os.path.dirname(p))
with os.fdopen(fd,"w",encoding="utf-8") as f:
    json.dump(d,f,indent=2,ensure_ascii=False)
os.replace(tmp,p)
PY
}

validate_plan_repair_contract() {
  # Re-arming a terminal executor is only meaningful for the exact plan lease
  # that just entered its one-shot repair state. An unbound executor would make
  # the caller, not frozen metadata, choose the repaired task and scope.
  python3 - "$META" "$REPO/.oms/plan/tasks.json" <<'PY'
import json, sys
meta=json.load(open(sys.argv[1],encoding="utf-8"))
plan=json.load(open(sys.argv[2],encoding="utf-8"))
task_id=meta.get("plan_task","")
task=plan.get("tasks",{}).get(task_id,{})
if not task_id or not task:
    raise SystemExit("error: executor repair requires its frozen plan task")
checks=(
    task.get("id") == meta.get("task_id"),
    task.get("state") == "claimed",
    task.get("lease_id") == meta.get("lease_id") and bool(meta.get("lease_id")),
    task.get("provider") == meta.get("provider"),
    task.get("verify","") == meta.get("verify",""),
    task.get("allowed_paths",[]) == meta.get("allowed_paths",[]),
    task.get("forbidden_paths",[]) == meta.get("forbidden_paths",[]),
    task.get("repair_count",0) == 1,
    task.get("executor_id","") == meta.get("executor_id","") and bool(meta.get("executor_id")),
    task.get("executor_soul_sha256","") == meta.get("soul_sha256","") and bool(meta.get("soul_sha256")),
)
if not all(checks):
    raise SystemExit("error: executor repair no longer matches the exact plan repair contract")
PY
}

if [ "$ACTION" = "freeze" ]; then
  require_worktree_write_mode
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
  require_worktree_write_mode
  validate_frozen; validate_plan_lease
  cat "$SOUL"
  python3 - "$META" <<'PY'
import json, sys
d=json.load(open(sys.argv[1],encoding="utf-8"))
def values(v): return ", ".join(v) if v else "(unrestricted)"
print("\n## Frozen Executor Contract\n")
print("executor_id: %s"%d["executor_id"])
print("allowed_paths: %s"%values(d.get("allowed_paths",[])))
print("forbidden_paths: %s"%(", ".join(d.get("forbidden_paths",[])) or "(none)"))
print("verify: %s"%(d.get("verify") or "(none)"))
print("The soul cannot widen this contract or delegate recursively.")
PY
  exit 0
fi

case "$ACTION" in
  start) require_worktree_write_mode; validate_frozen; validate_plan_lease; oms_with_file_lock "$META.lock" update_state frozen running ;;
  done) validate_soul_hash; oms_with_file_lock "$META.lock" update_state running "done" ;;
  repair)
    require_worktree_write_mode
    validate_frozen
    validate_plan_lease
    validate_plan_repair_contract
    oms_with_file_lock "$META.lock" repair_once
    validate_frozen
    validate_plan_lease
    validate_plan_repair_contract
    ;;
  fail) validate_soul_hash; oms_with_file_lock "$META.lock" update_state frozen,running failed ;;
  *) fail "unsupported action: $ACTION" ;;
esac
echo "executor: $ID -> $ACTION"
