# shellcheck shell=bash
# Cross-cutting primitives shared by the run and worktree tools. Sourced, not
# executed.

# A fresh worktree checks out committed content only, so agent-facing files kept
# local-only (project-private.sh) would be absent there and a worker would lose
# the project rules. Copy them in — but only while the shared .git/info/exclude
# still ignores them, since an unignored copy would be swept up by `git add -A`
# and pollute the patch.
# Locals are named wt_*/src_* on purpose: with `shellcheck -x`, a local named
# `worktree` here makes every sourcing script's `MODE=worktree-write` look like
# arithmetic (SC2100).
oms_seed_local_agent_files() {
  local src_repo="$1"
  local wt_dir="$2"
  local name

  [ -d "$src_repo" ] && [ -d "$wt_dir" ] || return 0
  for name in AGENTS.md CLAUDE.md GEMINI.md PROJECT.md; do
    [ -f "$src_repo/$name" ] || continue
    [ -e "$wt_dir/$name" ] && continue
    git -C "$wt_dir" check-ignore -q "$name" 2>/dev/null || continue
    cp "$src_repo/$name" "$wt_dir/$name" 2>/dev/null || true
  done
}

# Effective run id for auto-linking: explicit OMS_RUN_ID wins; otherwise the
# repo's .oms/runs/CURRENT pointer (written by oms-run.sh new) when it is
# fresh. A stale pointer must not misjoin unrelated later work, so it expires
# after OMS_RUN_CURRENT_TTL seconds (default 86400, same as board claims).
# Prints nothing and returns nonzero when neither applies.
oms_effective_run_id() {
  local state_root="$1"
  local current id minted now ttl

  if [ -n "${OMS_RUN_ID:-}" ]; then
    printf '%s\n' "$OMS_RUN_ID"
    return 0
  fi
  current="$state_root/.oms/runs/CURRENT"
  [ -f "$current" ] || return 1
  id="$(awk 'NR==1{print $1}' "$current")"
  minted="$(awk 'NR==1{print $2}' "$current")"
  [ -n "$id" ] || return 1
  case "$minted" in *[!0-9]*|"") return 1 ;; esac
  ttl="${OMS_RUN_CURRENT_TTL:-86400}"
  case "$ttl" in *[!0-9]*|"") ttl=86400 ;; esac
  now="$(date +%s)"
  [ $((now - minted)) -le "$ttl" ] || return 1
  printf '%s\n' "$id"
}

# Hash stdin / a file with whatever sha256 tool exists. Returns nonzero (no
# output) when none is available, so callers can compose without aborting.
oms_sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 | awk '{print $NF}'
  else
    return 1
  fi
}

oms_sha256_file() {
  [ -f "$1" ] || return 1
  oms_sha256_stream < "$1"
}

# Hash only git metadata and tracked diff bytes. The ledger never stores diff
# content, file paths, host paths, or secrets.
oms_git_state_fingerprint() {
  local root="$1"
  local head diff_hash untracked_hash

  if ! git -C "$root" rev-parse --git-dir >/dev/null 2>&1; then
    printf 'non-git\n'
    return 0
  fi
  head="$(git -C "$root" rev-parse HEAD 2>/dev/null || printf 'unborn')"
  diff_hash="$({
    git -C "$root" diff --binary HEAD -- 2>/dev/null || true
    git -C "$root" diff --cached --binary -- 2>/dev/null || true
  } | oms_sha256_stream)"
  untracked_hash="$(python3 - "$root" <<'PY'
import hashlib, os, subprocess, sys
root = sys.argv[1]
paths = subprocess.check_output(
    ["git", "-C", root, "ls-files", "-z", "--others", "--exclude-standard"])
h = hashlib.sha256()
for raw in sorted(x for x in paths.split(b"\0") if x):
    path = os.path.join(root, os.fsdecode(raw))
    h.update(hashlib.sha256(raw).digest())
    try:
        info = os.lstat(path)
        # Metadata makes creation/replacement/content writes visible without
        # reading an unbounded dataset/checkpoint. Paths are hashed above and
        # only this final aggregate digest reaches the ledger.
        h.update(("M:%o:%d:%d" % (
            info.st_mode, info.st_size,
            getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000)),
        )).encode())
    except OSError:
        h.update(b"MISSING")
print(h.hexdigest())
PY
)"
  printf '%s:%s:%s\n' "$head" "$diff_hash" "$untracked_hash"
}

# --- Worker authority detection ---------------------------------------------
# "A worker cannot widen its authority" was prose in the brief and a scope check
# on the returned patch. Neither notices a worker that reaches around the patch
# entirely: editing the primary worktree by absolute path, rewriting local git
# config, adding a remote, moving refs, or installing a hook. Those surfaces are
# not supposed to change while a worker runs, so they can simply be fingerprinted
# before and after. This is detection, not a sandbox: it says loudly that
# something moved and keeps the evidence, and it cannot prevent the write.
#
# .oms is not compared byte-for-byte: workers are given OMS_STATE_REPO so they
# can append shared harness state, and the harness itself writes there during a
# run. But "may append" is not "may rewrite" — the append-only JSONL families
# are checked for exactly that, and no state file may vanish.
#
# What this cannot see, by construction: a write that is undone before the
# worker exits, anything the worker reads (inherited tokens, ssh agents), and
# anything it does outside the repository (HOME, /tmp, a surviving background
# process). Those need process isolation, which a bash harness does not have.
#
# Each surface is captured on its own (`oms_worker_surface_snapshot`) rather
# than folded into one aggregate digest, so a violation can name what moved
# instead of reporting that some hash differs.

# Compare recorded shared state against the current tree. Unlike the other
# surfaces this is not an equality check: appending is allowed, so only a
# changed prefix, a shrunk file, or a disappearance counts.
oms_worker_state_violations() {
  local repo="$1"
  local before="$2"

  [ -f "$before" ] || return 0
  OMS_WG_REPO="$repo" OMS_WG_BEFORE="$before" python3 - <<'PY'
import hashlib, os

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
problems = []
with open(os.environ["OMS_WG_BEFORE"], encoding="utf-8", errors="replace") as f:
    for line in f:
        parts = line.rstrip("\n").split(" ")
        if len(parts) < 2:
            continue
        rel, kind = parts[0], parts[1]
        path = os.path.join(root, rel)
        if not os.path.exists(path):
            problems.append("%s was deleted" % rel)
            continue
        if kind != "APPEND" or len(parts) != 4:
            continue
        try:
            size = int(parts[2])
        except ValueError:
            continue
        expected = parts[3]
        try:
            now = os.path.getsize(path)
        except OSError:
            problems.append("%s became unreadable" % rel)
            continue
        if now < size:
            problems.append("%s was truncated (%d -> %d bytes)" % (rel, size, now))
            continue
        digest = hashlib.sha256()
        with open(path, "rb") as fh:
            remaining = size
            while remaining > 0:
                chunk = fh.read(min(1024 * 1024, remaining))
                if not chunk:
                    break
                remaining -= len(chunk)
                digest.update(chunk)
        if digest.hexdigest() != expected:
            problems.append("%s had existing rows rewritten" % rel)
for problem in problems[:5]:
    print(problem)
PY
}

# Freeze only the authority owned by this delegated operation. The surrounding
# plan/executor files are shared: a sibling may legitimately move another task
# while this provider runs, so comparing either whole file would turn normal
# parallel work into a violation. The selected task lease and executor soul are
# fencing identities; snapshot their complete JSON objects after the harness
# moves them to running, then compare those objects semantically before any
# review receipt or landing is published.
oms_worker_operation_snapshot() {  # REPO OUT PLAN_TASK LEASE EXECUTOR SOUL
  local repo="$1"
  local out="$2"
  local plan_task="${3:-}"
  local lease="${4:-}"
  local executor="${5:-}"
  local soul="${6:-}"
  local tmp="$out.tmp.$$"

  OMS_WG_REPO="$repo" OMS_WG_PLAN_TASK="$plan_task" OMS_WG_LEASE="$lease" \
    OMS_WG_EXECUTOR="$executor" OMS_WG_SOUL="$soul" \
    python3 - <<'PY' > "$tmp" || { rm -f "$tmp"; return 1; }
import base64, hashlib, json, os, sys

repo = os.environ["OMS_WG_REPO"]
task_id = os.environ["OMS_WG_PLAN_TASK"]
lease = os.environ["OMS_WG_LEASE"]
executor_id = os.environ["OMS_WG_EXECUTOR"]
soul = os.environ["OMS_WG_SOUL"]

def load_regular(path, label):
    if os.path.islink(path) or not os.path.isfile(path):
        raise SystemExit("error: %s authority is not a regular file" % label)
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, TypeError, ValueError) as exc:
        raise SystemExit("error: cannot read %s authority: %s" % (label, exc))
    if not isinstance(value, dict):
        raise SystemExit("error: %s authority is not an object" % label)
    return value

# Schema 2 adds soul_b64 so post-violation recovery can restore the frozen
# soul bytes, not merely detect that they changed.
snapshot = {"schema": 2, "plan": None, "executor": None}
if task_id:
    plan = load_regular(os.path.join(repo, ".oms", "plan", "tasks.json"), "plan")
    tasks = plan.get("tasks")
    task = tasks.get(task_id) if isinstance(tasks, dict) else None
    if not isinstance(task, dict):
        raise SystemExit("error: current plan task %s is missing" % task_id)
    if not lease or task.get("lease_id") != lease:
        raise SystemExit("error: current plan task %s lease changed before provider start" % task_id)
    snapshot["plan"] = {"task_id": task_id, "lease_id": lease, "task": task}
elif lease:
    raise SystemExit("error: current operation has a lease without a plan task")

if executor_id:
    path = os.path.join(repo, ".oms", "executors", executor_id, "meta.json")
    meta = load_regular(path, "executor %s" % executor_id)
    if meta.get("executor_id") != executor_id:
        raise SystemExit("error: current executor id changed before provider start")
    if not soul or meta.get("soul_sha256") != soul:
        raise SystemExit("error: current executor soul changed before provider start")
    soul_path = os.path.join(repo, ".oms", "executors", executor_id, "SOUL.md")
    if os.path.islink(soul_path) or not os.path.isfile(soul_path):
        raise SystemExit("error: current executor soul is not a regular file")
    try:
        with open(soul_path, "rb") as handle:
            soul_bytes = handle.read()
    except OSError as exc:
        raise SystemExit("error: cannot read current executor soul: %s" % exc)
    soul_file_sha = hashlib.sha256(soul_bytes).hexdigest()
    if soul_file_sha != soul:
        raise SystemExit("error: current executor soul file does not match metadata")
    snapshot["executor"] = {
        "executor_id": executor_id,
        "soul_sha256": soul,
        "soul_file_sha256": soul_file_sha,
        "soul_b64": base64.b64encode(soul_bytes).decode("ascii"),
        "meta": meta,
    }
elif soul:
    raise SystemExit("error: current operation has a soul without an executor")

json.dump(snapshot, sys.stdout, ensure_ascii=False, sort_keys=True,
          separators=(",", ":"))
sys.stdout.write("\n")
PY
  mv "$tmp" "$out"
}

oms_worker_operation_violations() {  # REPO SNAPSHOT
  local repo="$1"
  local snapshot="$2"

  [ -f "$snapshot" ] || return 0
  OMS_WG_REPO="$repo" OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY'
import hashlib, json, os

repo = os.environ["OMS_WG_REPO"]
snapshot_path = os.environ["OMS_WG_OPERATION_SNAPSHOT"]

def load_regular(path):
    if os.path.islink(path) or not os.path.isfile(path):
        raise OSError("not a regular file")
    with open(path, encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("not an object")
    return value

def changed_fields(before, after):
    if not isinstance(before, dict) or not isinstance(after, dict):
        return ["object-shape"]
    return sorted(key for key in set(before) | set(after)
                  if key not in before or key not in after or before[key] != after[key])

try:
    expected = load_regular(snapshot_path)
except (OSError, TypeError, ValueError):
    print("current operation snapshot became unreadable")
    raise SystemExit(0)

plan_expected = expected.get("plan")
if isinstance(plan_expected, dict):
    task_id = plan_expected.get("task_id", "")
    plan_path = os.path.join(repo, ".oms", "plan", "tasks.json")
    try:
        plan = load_regular(plan_path)
        tasks = plan.get("tasks")
        current = tasks.get(task_id) if isinstance(tasks, dict) else None
        if not isinstance(current, dict):
            raise KeyError(task_id)
    except (OSError, TypeError, ValueError, KeyError):
        print("plan task %s was deleted or became unreadable" % task_id)
    else:
        fields = changed_fields(plan_expected.get("task"), current)
        if fields:
            print("plan task %s changed: %s" % (task_id, ", ".join(fields)))
        elif current.get("lease_id") != plan_expected.get("lease_id"):
            print("plan task %s lease no longer matches its operation fence" % task_id)

executor_expected = expected.get("executor")
if isinstance(executor_expected, dict):
    executor_id = executor_expected.get("executor_id", "")
    meta_path = os.path.join(repo, ".oms", "executors", executor_id, "meta.json")
    try:
        current = load_regular(meta_path)
    except (OSError, TypeError, ValueError):
        print("executor %s was deleted or became unreadable" % executor_id)
    else:
        fields = changed_fields(executor_expected.get("meta"), current)
        if fields:
            print("executor %s changed: %s" % (executor_id, ", ".join(fields)))
        elif current.get("soul_sha256") != executor_expected.get("soul_sha256"):
            print("executor %s soul no longer matches its operation fence" % executor_id)
    soul_path = os.path.join(repo, ".oms", "executors", executor_id, "SOUL.md")
    try:
        if os.path.islink(soul_path) or not os.path.isfile(soul_path):
            raise OSError("not a regular file")
        with open(soul_path, "rb") as handle:
            current_soul_sha = hashlib.sha256(handle.read()).hexdigest()
    except OSError:
        print("executor %s soul file was deleted or became unreadable" % executor_id)
    else:
        if current_soul_sha != executor_expected.get("soul_file_sha256"):
            print("executor %s soul file changed" % executor_id)
PY
}

# Repair the operation's own frozen authority after a violation, using only
# the snapshot this process hashed before the provider started. The
# discrimination is field-scoped, not clock-scoped: the claim-cycle fields
# (state/provider/ttl/claimed_at/reason/lease_id/lease_epoch) are the only
# surface a legitimate writer can move mid-run — an operator block, a
# heartbeat, a release/reclaim handoff — so under a changed lease they are
# kept and named rather than guessed. Authority and evidence fields have no
# legitimate mid-run writer under any lease, and reclaim preserves them, so a
# forged claim row would otherwise carry a weakened verifier or scope into
# the next legitimate claim. Those always restore. The run still fails either
# way: this repairs owner state, it never admits the worker's output.
OMS_COMMON_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
oms_worker_operation_restore() {  # REPO SNAPSHOT EXPECTED_SHA
  local repo="$1"
  local snapshot="$2"
  local expected_sha="${3:-}"
  local actual_sha=""
  local plan_file="$repo/.oms/plan/tasks.json"
  local executor_id=""
  local meta_path
  local status=0

  # The snapshot sits in the worker-writable worktree parent. Bytes that no
  # longer match the pre-launch hash are the worker's; restoring them would
  # install attacker authority under the owner's own lock.
  actual_sha="$(oms_sha256_file "$snapshot" 2>/dev/null || true)"
  if [ -z "$expected_sha" ] || [ -z "$actual_sha" ] ||
    [ "$actual_sha" != "$expected_sha" ]; then
    echo "restore refused: current-operation snapshot is untrusted (changed or deleted); plan/executor state left for inspection"
    return 1
  fi
  if ! command -v oms_with_file_lock >/dev/null 2>&1; then
    # shellcheck source=scripts/lib/file-lock.sh
    . "$OMS_COMMON_LIB_DIR/file-lock.sh"
  fi
  # The writers' own lock keys, taken sequentially, never nested.
  oms_with_file_lock "$plan_file" oms_worker_operation_restore_plan \
    "$repo" "$snapshot" || status=1
  executor_id="$(OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY' | tr -d '\r'
import json, os
try:
    with open(os.environ["OMS_WG_OPERATION_SNAPSHOT"], encoding="utf-8") as fh:
        executor = json.load(fh).get("executor")
    print(executor.get("executor_id", "") if isinstance(executor, dict) else "")
except Exception:
    print("")
PY
)"
  if [ -n "$executor_id" ]; then
    case "$executor_id" in
      *[!A-Za-z0-9._-]*)
        echo "restore refused: snapshot executor id is malformed; executor state left for inspection"
        return 1
        ;;
    esac
    meta_path="$repo/.oms/executors/$executor_id/meta.json"
    oms_with_file_lock "$meta_path.lock" oms_worker_operation_restore_executor \
      "$repo" "$snapshot" || status=1
  fi
  return "$status"
}

oms_worker_operation_restore_plan() {  # REPO SNAPSHOT (under the plan file lock)
  local repo="$1"
  local snapshot="$2"

  OMS_WG_REPO="$repo" OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY'
import datetime, json, os, tempfile

repo = os.environ["OMS_WG_REPO"]
snapshot_path = os.environ["OMS_WG_OPERATION_SNAPSHOT"]
ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

# The fields a legitimate writer can move while this operation's worker runs.
# Everything outside this set is authority or evidence.
CLAIM_FIELDS = {"state", "provider", "ttl", "claimed_at", "reason",
                "lease_id", "lease_epoch", "updated"}

def refuse(msg):
    print(msg)
    raise SystemExit(1)

with open(snapshot_path, encoding="utf-8") as fh:
    expected = json.load(fh)
plan_expected = expected.get("plan")
if not isinstance(plan_expected, dict):
    raise SystemExit(0)
task_id = plan_expected.get("task_id", "")
expected_task = plan_expected.get("task")
if not task_id or not isinstance(expected_task, dict):
    refuse("restore refused: snapshot plan entry is malformed; plan state left for inspection")

plan_file = os.path.join(repo, ".oms", "plan", "tasks.json")
try:
    if os.path.islink(plan_file) or not os.path.isfile(plan_file):
        raise OSError("not a regular file")
    with open(plan_file, encoding="utf-8") as fh:
        plan = json.load(fh)
    tasks = plan.get("tasks") if isinstance(plan, dict) else None
    if not isinstance(tasks, dict):
        raise ValueError("not an object")
except (OSError, TypeError, ValueError):
    # Only this task's object is snapshotted; a destroyed plan file cannot be
    # rebuilt from it without fabricating the rest of the plan.
    refuse("restore refused: plan file unreadable; plan task %s left for inspection" % task_id)

def parses_ts(value):
    try:
        datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
        return True
    except Exception:
        return False

MISSING = object()
current = tasks.get(task_id)
restored = []
if not isinstance(current, dict):
    # No verb deletes a task; absence is always the worker.
    tasks[task_id] = dict(expected_task)
    restored = ["all fields (task was deleted)"]
else:
    same_lease = current.get("lease_id", "") == expected_task.get("lease_id", "")
    for key in sorted(set(expected_task) | set(current)):
        if key in CLAIM_FIELDS:
            continue
        if current.get(key, MISSING) == expected_task.get(key, MISSING):
            continue
        if key in expected_task:
            current[key] = expected_task[key]
        else:
            current.pop(key, None)
        restored.append(key)
    if same_lease:
        blocked = current.get("state") == "blocked"
        kept_heartbeat = False
        for key in sorted(CLAIM_FIELDS - {"updated"}):
            if current.get(key, MISSING) == expected_task.get(key, MISSING):
                continue
            if blocked and key in ("state", "reason"):
                continue
            if key == "claimed_at" and isinstance(current.get(key), str) \
                    and parses_ts(current[key]):
                kept_heartbeat = True
                continue
            if key in expected_task:
                current[key] = expected_task[key]
            else:
                current.pop(key, None)
            restored.append(key)
        if blocked and (current.get("state", MISSING) != expected_task.get("state", MISSING)
                        or current.get("reason", MISSING) != expected_task.get("reason", MISSING)):
            print("kept: plan task %s operator block (state, reason)" % task_id)
        if kept_heartbeat:
            print("kept: plan task %s claimed_at heartbeat" % task_id)
    else:
        moved = sorted(key for key in CLAIM_FIELDS - {"updated"}
                       if current.get(key, MISSING) != expected_task.get(key, MISSING))
        if moved:
            print("kept: plan task %s claim fields under a changed lease: %s "
                  "(a legitimate new owner may exist; inspect plan state)"
                  % (task_id, ", ".join(moved)))

if restored:
    row = tasks[task_id]
    row["updated"] = ts
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(plan_file))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(plan, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, plan_file)
    except Exception:
        os.unlink(tmp)
        raise
    print("restored: plan task %s fields: %s" % (task_id, ", ".join(restored)))
PY
}

oms_worker_operation_restore_executor() {  # REPO SNAPSHOT (under the meta lock)
  local repo="$1"
  local snapshot="$2"

  OMS_WG_REPO="$repo" OMS_WG_OPERATION_SNAPSHOT="$snapshot" python3 - <<'PY'
import base64, hashlib, json, os, tempfile

repo = os.environ["OMS_WG_REPO"]
snapshot_path = os.environ["OMS_WG_OPERATION_SNAPSHOT"]
status = 0

with open(snapshot_path, encoding="utf-8") as fh:
    expected = json.load(fh)
executor = expected.get("executor")
if not isinstance(executor, dict):
    raise SystemExit(0)
executor_id = executor.get("executor_id", "")
expected_meta = executor.get("meta")
if not executor_id or not isinstance(expected_meta, dict):
    print("restore refused: snapshot executor entry is malformed; executor state left for inspection")
    raise SystemExit(1)

exec_dir = os.path.join(repo, ".oms", "executors", executor_id)
meta_path = os.path.join(exec_dir, "meta.json")
try:
    if os.path.islink(meta_path) or not os.path.isfile(meta_path):
        raise OSError("not a regular file")
    with open(meta_path, encoding="utf-8") as fh:
        current_meta = json.load(fh)
    if not isinstance(current_meta, dict):
        raise ValueError("not an object")
except (OSError, TypeError, ValueError):
    current_meta = None

if current_meta != expected_meta:
    os.makedirs(exec_dir, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=exec_dir)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(expected_meta, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, meta_path)
    except Exception:
        os.unlink(tmp)
        raise
    if current_meta is None:
        print("restored: executor %s meta (was deleted or unreadable)" % executor_id)
    else:
        MISSING = object()
        fields = sorted(k for k in set(current_meta) | set(expected_meta)
                        if current_meta.get(k, MISSING) != expected_meta.get(k, MISSING))
        print("restored: executor %s meta fields: %s" % (executor_id, ", ".join(fields)))

soul_path = os.path.join(exec_dir, "SOUL.md")
expected_soul_sha = executor.get("soul_file_sha256", "")
soul_b64 = executor.get("soul_b64")
try:
    if os.path.islink(soul_path) or not os.path.isfile(soul_path):
        raise OSError("not a regular file")
    with open(soul_path, "rb") as fh:
        current_soul_sha = hashlib.sha256(fh.read()).hexdigest()
except OSError:
    current_soul_sha = ""
if expected_soul_sha and current_soul_sha != expected_soul_sha:
    soul_bytes = None
    if isinstance(soul_b64, str):
        try:
            soul_bytes = base64.b64decode(soul_b64.encode("ascii"), validate=True)
        except Exception:
            soul_bytes = None
    # Bytes must round-trip to the hash frozen before launch; a schema-1
    # snapshot has no bytes, so the tampered soul stays for inspection.
    if soul_bytes is None or hashlib.sha256(soul_bytes).hexdigest() != expected_soul_sha:
        print("restore refused: soul bytes unavailable in this snapshot; executor %s soul left for inspection" % executor_id)
        status = 1
    else:
        os.makedirs(exec_dir, exist_ok=True)
        if os.path.islink(soul_path):
            os.unlink(soul_path)
        fd, tmp = tempfile.mkstemp(dir=exec_dir)
        try:
            with os.fdopen(fd, "wb") as fh:
                fh.write(soul_bytes)
            os.replace(tmp, soul_path)
        except Exception:
            os.unlink(tmp)
            raise
        print("restored: executor %s soul file" % executor_id)
raise SystemExit(status)
PY
}

oms_worker_surface_diff() {
  local repo="$1"
  local before_dir="$2"
  local operation_snapshot_sha="${3:-}"
  local current_worktree_physical="${4:-}"
  local changed=""
  local name

  for name in config remotes refs tracked files gitmeta hooks; do
    [ -f "$before_dir/$name" ] || continue
    if ! oms_worker_surface_capture_one "$repo" "$name" \
      "$current_worktree_physical" | cmp -s - "$before_dir/$name"; then
      changed="${changed:+$changed, }$name"
    fi
  done
  # Shared state is compared by contract, not by equality: see above. The detail
  # goes to a file because this function runs in a command substitution, where
  # an exported variable would die with the subshell.
  oms_worker_state_violations "$repo" "$before_dir/omsstate" > "$before_dir/state-detail"
  [ ! -s "$before_dir/state-detail" ] || changed="${changed:+$changed, }shared-state"
  if [ -n "$operation_snapshot_sha" ]; then
    if [ ! -f "$before_dir/current-operation.json" ] ||
      [ "$(oms_sha256_file "$before_dir/current-operation.json" 2>/dev/null || true)" != "$operation_snapshot_sha" ]; then
      printf 'current operation snapshot was changed or deleted\n' \
        > "$before_dir/current-operation-detail"
    else
      oms_worker_operation_violations "$repo" "$before_dir/current-operation.json" \
        > "$before_dir/current-operation-detail"
    fi
    [ ! -s "$before_dir/current-operation-detail" ] ||
      changed="${changed:+$changed, }current-operation"
  fi
  printf '%s\n' "$changed"
}

# A parallel delegate owns a temporary linked worktree in the same repository.
# Its add/remove is harness lifecycle, not a provider reaching into Git
# metadata. Ignore only registrations backed by a live private marker whose
# physical repo/worktree pair passes the residue validator and whose metadata
# directory is that worktree's real Git backpointer. The current worker's own
# registration is never ignored: a commit there moves its detached HEAD and
# otherwise collapses into an empty successful patch.
oms_worker_gitmeta_is_live_managed_worktree() {  # REPO ENTRY CURRENT_WORKTREE
  local repo="$1"
  local entry="$2"
  local current_worktree_physical="${3:-}"
  local metadata_dir
  local metadata_physical=""
  local gitdir_file
  local gitfile=""
  local wt_path=""
  local wt_physical=""
  local worktree_git_dir=""
  local residue_dir=""
  local marker=""
  local marker_kind=""
  local marker_pid=""
  local marker_repo=""
  local marker_worktree=""
  local marker_temporary=""
  local repo_physical=""
  local base=""

  command -v oms_harness_safe_residue_worktree >/dev/null 2>&1 || return 1
  command -v oms_harness_temp_bases >/dev/null 2>&1 || return 1
  metadata_dir="$(dirname "$entry")"
  gitdir_file="$metadata_dir/gitdir"
  [ -f "$gitdir_file" ] && [ ! -L "$gitdir_file" ] || return 1
  gitfile="$(sed -n '1p' "$gitdir_file" 2>/dev/null || true)"
  gitfile="${gitfile//$'\r'/}"
  case "$gitfile" in
    */.git) wt_path="${gitfile%/.git}" ;;
    *) return 1 ;;
  esac
  wt_physical="$(oms_harness_physical_dir "$wt_path" 2>/dev/null || true)"
  [ -n "$wt_physical" ] || return 1
  [ -z "$current_worktree_physical" ] ||
    [ "$wt_physical" != "$current_worktree_physical" ] || return 1
  [ -f "$wt_physical/.git" ] && [ ! -L "$wt_physical/.git" ] || return 1
  metadata_physical="$(oms_harness_physical_dir "$metadata_dir" 2>/dev/null || true)"
  [ -n "$metadata_physical" ] || return 1
  worktree_git_dir="$(
    oms_harness_git_path_physical "$wt_physical" --git-dir 2>/dev/null || true
  )"
  [ -n "$worktree_git_dir" ] &&
    [ "$metadata_physical" = "$worktree_git_dir" ] || return 1
  residue_dir="$(dirname "$wt_physical")"
  marker="$residue_dir/.oh-my-setting-tmp"
  [ -f "$marker" ] && [ ! -L "$marker" ] || return 1
  marker_kind="$(oms_harness_read_marker_value "$marker" kind)"
  marker_pid="$(oms_harness_read_marker_value "$marker" pid)"
  marker_repo="$(oms_harness_read_marker_value "$marker" repo)"
  marker_worktree="$(oms_harness_read_marker_value "$marker" worktree)"
  marker_temporary="$(oms_harness_read_marker_value "$marker" temporary)"
  [ "$marker_kind" = oh-my-setting-temp ] && [ "$marker_temporary" = 1 ] || return 1
  case "$marker_pid" in *[!0-9]*|"") return 1 ;; esac
  kill -0 "$marker_pid" 2>/dev/null || return 1
  repo_physical="$(oms_harness_physical_dir "$repo" 2>/dev/null || true)"
  [ -n "$repo_physical" ] || return 1

  while IFS= read -r base; do
    [ -n "$base" ] || continue
    if oms_harness_safe_residue_worktree "$base" "$residue_dir" \
        "$marker_repo" "$marker_worktree" &&
      [ "$OMS_HARNESS_SAFE_RESIDUE_REPO" = "$repo_physical" ] &&
      [ "$OMS_HARNESS_SAFE_RESIDUE_WORKTREE" = "$wt_physical" ]; then
      return 0
    fi
  done <<EOF
$(oms_harness_temp_bases)
EOF
  return 1
}

# Capture and later re-check the delegated checkout's identity. A provider owns
# the files inside this directory, but it must not be able to rename the
# directory and replace the old pathname with a symlink to the primary checkout:
# every later `git -C "$worktree"` would otherwise follow that new target. The
# real directory and gitdir device/inode identities, common-dir, the regular
# `.git` file, and Git's regular backpointer must all stay bound to their
# pre-provider receipt.
# Results are globals so callers do not need to serialize pathnames through a
# delimiter (a repository pathname may legally contain tabs or newlines).
OMS_WORKER_IDENTITY_PHYSICAL=""
OMS_WORKER_IDENTITY_GIT_DIR=""
OMS_WORKER_IDENTITY_COMMON_DIR=""
OMS_WORKER_IDENTITY_GITFILE_SHA=""
OMS_WORKER_IDENTITY_BACKPOINTER_SHA=""
OMS_WORKER_IDENTITY_WORKTREE_STAT=""
OMS_WORKER_IDENTITY_GITDIR_STAT=""
OMS_WORKER_IDENTITY_DETAIL=""

oms_worker_worktree_identity_capture() {  # REPO WORKTREE
  local repo="$1"
  local worker_tree="$2"
  local physical=""
  local git_dir=""
  local common_dir=""
  local repo_common=""
  local gitfile_line=""
  local gitfile_target=""
  local gitfile_target_physical=""
  local backpointer_file=""
  local backpointer=""
  local backpointer_parent=""
  local backpointer_physical=""
  local gitfile_sha=""
  local backpointer_sha=""
  local identity_stats=""
  local identity_tab=""
  local worktree_stat=""
  local gitdir_stat=""

  OMS_WORKER_IDENTITY_PHYSICAL=""
  OMS_WORKER_IDENTITY_GIT_DIR=""
  OMS_WORKER_IDENTITY_COMMON_DIR=""
  OMS_WORKER_IDENTITY_GITFILE_SHA=""
  OMS_WORKER_IDENTITY_BACKPOINTER_SHA=""
  OMS_WORKER_IDENTITY_WORKTREE_STAT=""
  OMS_WORKER_IDENTITY_GITDIR_STAT=""
  OMS_WORKER_IDENTITY_DETAIL=""

  physical="$(oms_harness_physical_dir "$worker_tree" 2>/dev/null || true)"
  if [ -z "$physical" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree path is missing, moved, or symbolic"
    return 1
  fi
  if [ ! -f "$worker_tree/.git" ] || [ -L "$worker_tree/.git" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git is not a regular backpointer file"
    return 1
  fi

  gitfile_line="$(sed -n '1p' "$worker_tree/.git" 2>/dev/null || true)"
  gitfile_line="${gitfile_line//$'\r'/}"
  case "$gitfile_line" in
    "gitdir: "*) gitfile_target="${gitfile_line#gitdir: }" ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git backpointer is malformed"
      return 1
      ;;
  esac
  case "$gitfile_target" in
    /*|[A-Za-z]:/*) ;;
    *) gitfile_target="$worker_tree/$gitfile_target" ;;
  esac
  gitfile_target_physical="$(
    oms_harness_physical_dir "$gitfile_target" 2>/dev/null || true
  )"
  git_dir="$(oms_harness_git_path_physical "$worker_tree" --git-dir 2>/dev/null || true)"
  common_dir="$(
    oms_harness_git_path_physical "$worker_tree" --git-common-dir 2>/dev/null || true
  )"
  repo_common="$(oms_harness_git_path_physical "$repo" --git-common-dir 2>/dev/null || true)"
  if [ -z "$git_dir" ] || [ "$gitfile_target_physical" != "$git_dir" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree .git no longer points to its actual gitdir"
    return 1
  fi
  if [ -z "$common_dir" ] || [ -z "$repo_common" ] ||
    [ "$common_dir" != "$repo_common" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree no longer belongs to the primary common gitdir"
    return 1
  fi
  case "$git_dir" in
    "$common_dir"/worktrees/*)
      case "${git_dir#"$common_dir"/worktrees/}" in
        ""|*/*)
          OMS_WORKER_IDENTITY_DETAIL="delegated worktree gitdir has an invalid registration path"
          return 1
          ;;
      esac
      ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree gitdir is outside the common worktree registry"
      return 1
      ;;
  esac

  backpointer_file="$git_dir/gitdir"
  if [ ! -f "$backpointer_file" ] || [ -L "$backpointer_file" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry gitdir is not a regular backpointer file"
    return 1
  fi
  backpointer="$(sed -n '1p' "$backpointer_file" 2>/dev/null || true)"
  backpointer="${backpointer//$'\r'/}"
  case "$backpointer" in
    */.git) backpointer_parent="${backpointer%/.git}" ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry backpointer is malformed"
      return 1
      ;;
  esac
  case "$backpointer_parent" in
    /*|[A-Za-z]:/*) ;;
    *) backpointer_parent="$git_dir/$backpointer_parent" ;;
  esac
  backpointer_physical="$(
    oms_harness_physical_dir "$backpointer_parent" 2>/dev/null || true
  )"
  if [ "$backpointer_physical" != "$physical" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree registry does not point back to the current checkout"
    return 1
  fi

  gitfile_sha="$(oms_sha256_file "$worker_tree/.git" 2>/dev/null || true)"
  backpointer_sha="$(oms_sha256_file "$backpointer_file" 2>/dev/null || true)"
  if [ -z "$gitfile_sha" ] || [ -z "$backpointer_sha" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree backpointer could not be hashed"
    return 1
  fi
  if ! identity_stats="$(python3 - "$physical" "$git_dir" <<'PY'
import os
import stat
import sys

values = []
for path in sys.argv[1:]:
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode):
        raise SystemExit(2)
    values.append("%s:%s" % (info.st_dev, info.st_ino))
print("\t".join(values))
PY
  )"; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity could not be read"
    return 1
  fi
  # Native Windows Python writes CRLF, and the same worktree can have more than
  # one path spelling. Paths were canonicalized above; strip CR only from this
  # compact device/inode receipt before comparing it across provider phases.
  identity_stats="$(printf '%s' "$identity_stats" | tr -d '\r')"
  identity_tab="$(printf '\t')"
  case "$identity_stats" in
    *"$identity_tab"*) ;;
    *)
      OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity is malformed"
      return 1
      ;;
  esac
  worktree_stat="${identity_stats%%"$identity_tab"*}"
  gitdir_stat="${identity_stats#*"$identity_tab"}"
  if [ -z "$worktree_stat" ] || [ -z "$gitdir_stat" ]; then
    OMS_WORKER_IDENTITY_DETAIL="delegated worktree inode identity is incomplete"
    return 1
  fi

  OMS_WORKER_IDENTITY_PHYSICAL="$physical"
  OMS_WORKER_IDENTITY_GIT_DIR="$git_dir"
  OMS_WORKER_IDENTITY_COMMON_DIR="$common_dir"
  OMS_WORKER_IDENTITY_GITFILE_SHA="$gitfile_sha"
  OMS_WORKER_IDENTITY_BACKPOINTER_SHA="$backpointer_sha"
  OMS_WORKER_IDENTITY_WORKTREE_STAT="$worktree_stat"
  OMS_WORKER_IDENTITY_GITDIR_STAT="$gitdir_stat"
  return 0
}

oms_worker_worktree_identity_check() {  # REPO WORKTREE PHYSICAL GITDIR COMMON GITFILE_SHA BACKPOINTER_SHA WORKTREE_STAT GITDIR_STAT
  local repo="$1"
  local worker_tree="$2"
  local expected_physical="$3"
  local expected_git_dir="$4"
  local expected_common_dir="$5"
  local expected_gitfile_sha="$6"
  local expected_backpointer_sha="$7"
  local expected_worktree_stat="$8"
  local expected_gitdir_stat="$9"

  if ! oms_worker_worktree_identity_capture "$repo" "$worker_tree"; then
    printf '%s\n' "$OMS_WORKER_IDENTITY_DETAIL"
    return 1
  fi
  if [ "$OMS_WORKER_IDENTITY_PHYSICAL" != "$expected_physical" ] ||
    [ "$OMS_WORKER_IDENTITY_GIT_DIR" != "$expected_git_dir" ] ||
    [ "$OMS_WORKER_IDENTITY_COMMON_DIR" != "$expected_common_dir" ] ||
    [ "$OMS_WORKER_IDENTITY_GITFILE_SHA" != "$expected_gitfile_sha" ] ||
    [ "$OMS_WORKER_IDENTITY_BACKPOINTER_SHA" != "$expected_backpointer_sha" ] ||
    [ "$OMS_WORKER_IDENTITY_WORKTREE_STAT" != "$expected_worktree_stat" ] ||
    [ "$OMS_WORKER_IDENTITY_GITDIR_STAT" != "$expected_gitdir_stat" ]; then
    printf 'delegated worktree physical path, inode, or Git backpointer changed\n'
    return 1
  fi
  return 0
}

oms_worker_surface_capture_one() {
  local repo="$1"
  local name="$2"
  local current_worktree_physical="${3:-}"
  local git_dir

  git_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null || printf '')"
  case "$git_dir" in
    "") git_dir="$(git -C "$repo" rev-parse --git-dir 2>/dev/null || printf '')" ;;
    /*) ;;
    *) git_dir="$repo/$git_dir" ;;
  esac
  case "$name" in
    config) git -C "$repo" config --local --list 2>/dev/null | LC_ALL=C sort ;;
    remotes) git -C "$repo" remote -v 2>/dev/null | LC_ALL=C sort ;;
    refs)
      git -C "$repo" show-ref 2>/dev/null | LC_ALL=C sort
      git -C "$repo" rev-parse HEAD 2>/dev/null || printf 'unborn\n'
      ;;
    tracked)
      # Status categories are not content: a file already marked ` M` stays
      # ` M` however much a worker rewrites it, so an in-flight edit of the
      # user's own uncommitted work would pass unnoticed. Hash the diff bytes.
      git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null | LC_ALL=C sort
      printf 'worktree-diff %s\n' "$({
        git -C "$repo" diff --binary HEAD -- 2>/dev/null || true
      } | oms_sha256_stream)"
      printf 'index-diff %s\n' "$({
        git -C "$repo" diff --cached --binary -- 2>/dev/null || true
      } | oms_sha256_stream)"
      ;;
    files)
      # Untracked AND ignored content, by stat rather than by reading bytes.
      # `git status --ignored` collapses a fully ignored directory to one entry,
      # so replacing .venv/bin/python or planting sitecustomize.py inside an
      # ignored tree is invisible to it. Enumerating with stat catches the
      # replacement without reading a single dataset byte. The walk is bounded:
      # a truncated scan says so instead of quietly covering less.
      # Untracked-but-not-ignored paths are scanned first and ignored ones
      # after, because the budget is spent in path order: a real ML repo whose
      # ignored data/ and runs/ trees hold tens of thousands of files would
      # otherwise eat the whole budget alphabetically and leave the handful of
      # genuinely new source files — the ones a stray worker write would land
      # in — unscanned. Truncation now only ever drops ignored churn.
      OMS_WG_REPO="$repo" OMS_WG_MAX="${OMS_WORKER_GUARD_MAX_FILES:-20000}" \
      OMS_WG_EXCLUDE="${OMS_WORKER_GUARD_EXCLUDE:-}" python3 - <<'PY'
import hashlib, os, subprocess, sys

root = os.environ["OMS_WG_REPO"]
try:
    budget = int(os.environ.get("OMS_WG_MAX", "20000"))
except ValueError:
    budget = 20000
def listing(args):
    try:
        raw = subprocess.check_output(
            ["git", "-C", root, "ls-files", "-z"] + args,
            stderr=subprocess.DEVNULL)
    except Exception:
        return []
    return sorted(p for p in raw.split(b"\0") if p)


untracked = listing(["--others", "--exclude-standard"])
seen_untracked = set(untracked)
ignored = [p for p in listing(["--others", "--ignored", "--exclude-standard"])
           if p not in seen_untracked]
paths = untracked + ignored
first_ignored = len(untracked)
count = 0
# The harness writes this run's own artifacts and patch somewhere; when that is
# inside the repo it is the harness moving, not the worker.
excluded = [".oms"]
for extra in (os.environ.get("OMS_WG_EXCLUDE") or "").splitlines():
    extra = extra.strip().strip("/")
    if extra:
        excluded.append(extra)
for index, rel in enumerate(paths):
    name = os.fsdecode(rel)
    if any(name == prefix or name.startswith(prefix + "/") for prefix in excluded):
        continue
    count += 1
    if count > budget:
        # Say which class was cut. "ignored" means every untracked source file
        # was still covered; "untracked" means the budget is too small for this
        # repository and the guard is genuinely partial.
        klass = "ignored" if index >= first_ignored else "untracked"
        print("TRUNCATED after %d entries (%s)" % (budget, klass))
        break
    full = os.path.join(root, name)
    try:
        info = os.lstat(full)
        print("%s %o %d %d" % (
            hashlib.sha256(rel).hexdigest(), info.st_mode, info.st_size,
            getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000))))
    except OSError:
        print("%s MISSING" % hashlib.sha256(rel).hexdigest())
PY
      ;;
    omsstate)
      # Shared state is append-only by contract: every .oms JSONL family is
      # written with >>. A worker that rewrites history there — dropping the
      # failure that blocks its retry, forging a landing, editing a thread turn
      # another agent will read as evidence — leaves a file whose existing bytes
      # changed. Record each file's length and the hash of exactly those bytes;
      # growth is legitimate, mutation of the prefix is not. Deletion of any
      # state file is a violation regardless of family.
      OMS_WG_REPO="$repo" python3 - <<'PY'
import hashlib, os, sys

root = os.path.join(os.environ["OMS_WG_REPO"], ".oms")
if not os.path.isdir(root):
    raise SystemExit(0)
rows = []
for base, dirs, files in os.walk(root):
    # Delegation markers are ephemeral owner liveness. A sibling legitimately
    # creates/removes them while this worker runs; the exclusive provider-window
    # guard covers their boundary when a caller explicitly guarantees quiescence.
    dirs[:] = sorted(d for d in dirs
                     if not os.path.islink(os.path.join(base, d))
                     and not (base == root and d == "delegations"))
    for name in sorted(files):
        path = os.path.join(base, name)
        rel = os.path.relpath(path, root).replace(os.sep, "/")
        if os.path.islink(path):
            rows.append("%s LINK" % rel)
            continue
        # Append-only by contract: the JSONL families, plus the memory log and
        # its pins. summary.md is derived from shared.md and regenerated, and
        # plan/executor/run state is rewritten by the harness by design, so
        # those are tracked by presence alone.
        append_only = rel.endswith(".jsonl") or rel in (
            "memory/shared.md", "memory/pins.md")
        if not append_only:
            rows.append("%s EXISTS" % rel)
            continue
        try:
            size = os.path.getsize(path)
            digest = hashlib.sha256()
            with open(path, "rb") as f:
                remaining = size
                while remaining > 0:
                    chunk = f.read(min(1024 * 1024, remaining))
                    if not chunk:
                        break
                    remaining -= len(chunk)
                    digest.update(chunk)
            rows.append("%s APPEND %d %s" % (rel, size, digest.hexdigest()))
        except OSError:
            rows.append("%s UNREADABLE" % rel)
for row in rows:
    print(row)
PY
      ;;
    gitmeta)
      # Object-store wiring and repository metadata outside config/refs/hooks:
      # an added alternate silently widens where objects may come from, a graft
      # or shallow edit rewrites history's shape, a new linked worktree is
      # another checkout of this repo, and a submodule keeps its own config,
      # refs, and hooks under .git/modules.
      if [ -n "$git_dir" ] && [ -d "$git_dir" ]; then
        local meta
        for meta in objects/info/alternates objects/info/http-alternates \
          info/exclude info/grafts info/attributes shallow; do
          if [ -f "$git_dir/$meta" ]; then
            printf '%s %s\n' "$meta" \
              "$(oms_sha256_file "$git_dir/$meta" 2>/dev/null || printf 'unreadable')"
          fi
        done
        if [ -d "$git_dir/worktrees" ]; then
          # Walk registry entries themselves rather than only regular HEAD and
          # gitdir leaves. `find -type f` silently skipped a symlink directory,
          # even though Git follows it as a duplicate worktree registration.
          # A live sibling is exempt only after its real gitdir/backpointer and
          # private liveness marker pass the validator above; every symlink and
          # every other registry entry remains part of the hard gitmeta surface.
          find "$git_dir/worktrees" -mindepth 1 -maxdepth 1 -print 2>/dev/null |
            LC_ALL=C sort |
            while IFS= read -r wt; do
              if [ -L "$wt" ]; then
                printf 'worktree-link %s %s\n' "${wt#"$git_dir"/}" \
                  "$(readlink "$wt" 2>/dev/null | oms_sha256_stream || printf 'unreadable')"
                continue
              fi
              if [ ! -d "$wt" ]; then
                printf 'worktree-entry %s non-directory\n' "${wt#"$git_dir"/}"
                continue
              fi
              if oms_worker_gitmeta_is_live_managed_worktree "$repo" "$wt/gitdir" \
                "$current_worktree_physical"; then
                continue
              fi
              printf 'worktree-dir %s\n' "${wt#"$git_dir"/}"
              for meta in "$wt/gitdir" "$wt/HEAD"; do
                if [ -L "$meta" ]; then
                  printf 'worktree-link %s %s\n' "${meta#"$git_dir"/}" \
                    "$(readlink "$meta" 2>/dev/null | oms_sha256_stream || printf 'unreadable')"
                elif [ -f "$meta" ]; then
                  printf 'worktree %s %s\n' "${meta#"$git_dir"/}" \
                    "$(oms_sha256_file "$meta" 2>/dev/null || printf 'unreadable')"
                elif [ -e "$meta" ]; then
                  printf 'worktree-entry %s non-regular\n' "${meta#"$git_dir"/}"
                fi
              done
            done
        fi
        if [ -d "$git_dir/modules" ]; then
          find "$git_dir/modules" -maxdepth 3 \( -name config -o -name HEAD \) -type f \
            -print 2>/dev/null | LC_ALL=C sort |
            while IFS= read -r mod; do
              printf 'module %s %s\n' "${mod#"$git_dir"/}" \
                "$(oms_sha256_file "$mod" 2>/dev/null || printf 'unreadable')"
            done
        fi
      fi
      ;;
    hooks)
      if [ -n "$git_dir" ] && [ -d "$git_dir/hooks" ]; then
        find "$git_dir/hooks" -maxdepth 1 -type f ! -name '*.sample' -print 2>/dev/null |
          LC_ALL=C sort |
          while IFS= read -r hook; do
            printf '%s %s\n' "$(basename "$hook")" "$(oms_sha256_file "$hook" 2>/dev/null || printf 'unreadable')"
          done
      fi
      ;;
  esac
}

# Capture each surface separately so a later comparison can name what moved.
oms_worker_surface_snapshot() {
  local repo="$1"
  local dir="$2"
  local current_worktree_physical="${3:-}"
  local name

  mkdir -p "$dir" || return 1
  for name in config remotes refs tracked files gitmeta hooks omsstate; do
    oms_worker_surface_capture_one "$repo" "$name" \
      "$current_worktree_physical" > "$dir/$name" 2>/dev/null || true
  done
}
