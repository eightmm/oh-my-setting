#!/usr/bin/env bash
set -euo pipefail

# Retention sweep for repo-local .oms state (--dry-run by default).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

REPO="$PWD"
DAYS=30
DRY_RUN=1
DELETE_ORPHAN_FILES=0

usage() {
  cat <<'EOF'
Usage: gc.sh [--repo PATH] [--days N] [--dry-run|--apply]

Reclaim aged, transient .oms state. Default is --dry-run (prints only).

Options:
  --repo PATH   Repo to sweep (default: PWD, git-root anchored).
  --days N      Age threshold in days (default: 30).
  --dry-run     Print what would be removed (default).
  --apply       Actually remove.
  --delete-orphan-files
                Also delete artifact/patch files no index row references.
                Off by default: an orphaned artifact is the evidence itself,
                and its row was retired by an earlier prune, so the sweep that
                runs unattended must not be the one that removes it. doctor
                names the count and the command when they accumulate.
  -h, --help    Show help.

Attempt liveness is judged on its own clock, not --days: an attempt still in
starting/working/verifying with no lifecycle event for OMS_ATTEMPT_STALE_SECONDS
(default 86400) is moved to blocked/heartbeat_expired, where the inbox can name
it. OMS_ARTIFACT_INDEX_KEEP (default 1000) is the artifact index floor; raise it
to keep older evidence through a sweep (the compaction high-water follows it
unless OMS_ARTIFACT_INDEX_HIGH_WATER is set explicitly).

Swept (older than --days): orphaned delegation markers (dead pid; a coupled
claimed/running plan task is released back to ready), the event streams of
long-terminal attempts, archived task packets,
handoff digests, local tracked-state checkpoints, hook events/sessions,
terminal supervisor runtime records outside the repository (durable lifecycle
events and attempt specs remain),
stale open runs (no spine event in --days; a close event is appended), run
capsules of runs that are NOT open, abandoned change-guards (dead owner pid
or aged snapshot), terminal/draft executor souls, retired failure rows
(resolved, or automatic hook rows past OMS_HOOK_FAIL_TTL — the same predicate
fail-ledger reads with), closed conversation threads, closed work rooms and empty
rooms without joined participants (unread room mail is always retained),
unreferenced completion captures/prefixes/segments outside plan, receipt and
transaction-pin roots (pins include validated requirement evidence; an unsettled
lease without a pin or with a legacy worker marker defers collection);
stale transaction pins and unused lease indexes after --days, retaining pinned
objects for that sweep;
interrupted pin-write temporary files after --days (unknown entries defer collection);
dispatch-log/activity pairs with recorded exits, stopped processes and settled
terminal attempts, excluding the latest attempt of unfinished plan tasks.
Dispatch cleanup reports pair/file counts and bytes, and defers while delegation
evidence is live or unknown. Existing log/activity/stream/transcript files must
all be older than --days. A lone activity file can finish interrupted cleanup;
unknown evidence is kept. Missing lifecycle evidence is noted once in dry-run
and stays silent in apply mode. Unexpected helper failures fail the sweep.
Panel-result files are retained because
their index, room-thread and outcome references need a separate closure check;
artifact index rows are delegated to
artifact-index prune; orphaned artifact files are left alone unless
--delete-orphan-files says otherwise. Never touches live runs, the active task, unresolved
failures, active experiment claims, or plan tasks in review. The append-only
experiment board is left intact. Legacy/unknown/referenced landing evidence is
retained. New native capture generations are collected only with private anchor
ownership, a writer terminal seal, complete reference checks and quiescence.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --days) [ "$#" -ge 2 ] || fail "--days requires an integer"; DAYS="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --apply) DRY_RUN=0; shift ;;
    --delete-orphan-files) DELETE_ORPHAN_FILES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
case "$DAYS" in *[!0-9]*|"") fail "--days must be a non-negative integer" ;; esac
[ "$DAYS" -le 36500 ] || fail "--days must be at most 36500"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
STATE_ROOT="$(cd "$STATE_ROOT" && pwd -P)" || fail "cannot resolve the physical repository"
OMS="$STATE_ROOT/.oms"
[ -d "$OMS" ] || { echo "gc: no .oms state at $STATE_ROOT"; exit 0; }

mode="dry-run"; [ "$DRY_RUN" = 0 ] && mode="apply"
echo "gc: $STATE_ROOT (older than ${DAYS}d, $mode)"

removed=0
supervisor_gc_args=(--repo "$STATE_ROOT" gc --older-than-days "$DAYS")
[ "$DRY_RUN" = 1 ] || supervisor_gc_args+=(--apply)
supervisor_gc_out="$("$ROOT/scripts/agent-supervisor.sh" "${supervisor_gc_args[@]}")"
printf '%s\n' "$supervisor_gc_out"
supervisor_changes="$(printf '%s\n' "$supervisor_gc_out" |
  awk '/^would delete: att_/ || /^deleted: att_/ {n++} END {print n+0}')"
removed=$((removed + supervisor_changes))

note_remove() {  # note_remove KIND PATH
  printf -- '- %s: %s\n' "$1" "$2"
  removed=$((removed + 1))
  if [ "$DRY_RUN" = 0 ]; then
    rm -rf "$2"
  fi
}

completion_capture_gc_locked() {
  python3 - "$ROOT_LIB/plan-completion.py" "$STATE_ROOT" "$DAYS" "$DRY_RUN" <<'PY'
import runpy, sys, time
try:
    collect = runpy.run_path(sys.argv[1])["collect_abandoned_prefixes"]
    for path in collect(sys.argv[2], time.time() - int(sys.argv[3]) * 86400, sys.argv[4] == "1"):
        if path.startswith('.oms/plan/completion-pins/'):
            kind = 'completion-pin-temp' if path.rsplit('/', 1)[1].startswith('.') else 'completion-pin'
        elif path.startswith('.oms/plan/completion-pin-leases/'):
            kind = 'completion-pin-index'
        else:
            kind = 'completion-provenance'
        print('- ' + kind + ': ' + path)
except (OSError, ValueError, KeyError, TypeError, RecursionError, SystemExit) as exc:
    print("gc: completion capture collection skipped: %s" % exc, file=sys.stderr)
PY
}
completion_capture_gc_plan_lock() {
  oms_try_file_lock "$OMS/plan/tasks.json" completion_capture_gc_locked
}
completion_capture_gc_marker_lock() {
  oms_try_file_lock "$OMS/delegations/.marker-set-lock-target" completion_capture_gc_plan_lock
}
if [ -d "$OMS/plan/completion-captures" ] || [ -d "$OMS/plan/completion-prefixes" ] ||
   [ -d "$OMS/plan/completion-segments" ] || [ -d "$OMS/plan/completion-pins" ] ||
   [ -d "$OMS/plan/completion-pin-leases" ]; then
  completion_gc_out=""
  completion_gc_status=0
  completion_gc_out="$(oms_try_file_lock "$OMS/landings.jsonl" completion_capture_gc_marker_lock)" ||
    completion_gc_status=$?
  case "$completion_gc_status" in
    0)
      [ -z "$completion_gc_out" ] || printf '%s\n' "$completion_gc_out"
      while IFS= read -r completion_gc_line; do
        case "$completion_gc_line" in
          '- completion-provenance: '*|'- completion-pin: '*|'- completion-pin-temp: '*|'- completion-pin-index: '*)
            removed=$((removed + 1)) ;;
        esac
      done <<< "$completion_gc_out"
      ;;
    75) echo "- completion-provenance: skipped while a writer holds the file" ;;
    *) fail "completion capture collection failed" ;;
  esac
fi

# 0.5) Validate legacy retention inputs without treating shared terminal rows as ownership.
landing_patch_gc_locked() {
  python3 - "$STATE_ROOT" "$OMS/landings.jsonl" "$OMS/artifacts/index.jsonl" \
    "$DAYS" "$DRY_RUN" <<'PY'
import json, os, stat, sys, time

repo, landings_path, index_path, days_raw, dry_raw = sys.argv[1:]
patch_root = os.path.realpath(os.path.join(repo, ".oms", "landing-patches"))
if not os.path.isdir(patch_root) or os.path.islink(os.path.join(repo, ".oms", "landing-patches")):
    raise SystemExit(0)


def inside(path, root):
    try:
        return os.path.commonpath([path, root]) == root
    except ValueError:
        return False


intents = {}
seen_intents = set()
terminal = set()
try:
    landing_lines = open(landings_path, encoding="utf-8", errors="replace")
except OSError as exc:
    raise SystemExit("cannot read landing retention input: %s" % exc)
with landing_lines:
    for line_number, line in enumerate(landing_lines, 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except (TypeError, ValueError):
            raise SystemExit("malformed landing row at line %d" % line_number)
        if not isinstance(row, dict):
            raise SystemExit("non-object landing row at line %d" % line_number)
        landing_id = row.get("landing_id")
        if not isinstance(landing_id, str) or not landing_id:
            raise SystemExit("landing row %d has no landing_id" % line_number)
        event = row.get("event")
        if event in ("capture-reserved", "capture-terminal", "capture-release-ready", "capture-released"):
            # These are validated against private ownership by the native
            # collector; they are never legacy landing terminal authority.
            continue
        if event == "intent":
            if not isinstance(row.get("patch"), str) or not row["patch"]:
                raise SystemExit("landing intent %s has no patch" % landing_id)
            seen_intents.add(landing_id)
            value = row["patch"]
            candidate = value if os.path.isabs(value) else os.path.join(repo, value)
            real = os.path.realpath(candidate)
            if inside(real, patch_root):
                previous = intents.get(landing_id)
                if previous is not None and previous != real:
                    raise SystemExit("landing %s has conflicting intent paths" % landing_id)
                intents[landing_id] = real
        elif event in ("complete", "abandoned"):
            if landing_id not in seen_intents:
                raise SystemExit("landing %s is terminal without an intent" % landing_id)
            terminal.add(landing_id)
        elif event in ("applied-pending-receipt", "not-applied-pending-receipt"):
            if landing_id not in seen_intents:
                raise SystemExit("landing %s has a receipt event without an intent" % landing_id)
        else:
            raise SystemExit("landing %s has unknown event %r" % (landing_id, event))

active_paths = {
    path for landing_id, path in intents.items() if landing_id not in terminal
}
terminal_paths = {
    path for landing_id, path in intents.items() if landing_id in terminal
}

referenced = set()
try:
    index_lines = open(index_path, encoding="utf-8", errors="replace")
except FileNotFoundError:
    index_lines = None
except OSError as exc:
    raise SystemExit("cannot read artifact retention input: %s" % exc)
if index_lines is not None:
    with index_lines:
        for line_number, line in enumerate(index_lines, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except (TypeError, ValueError):
                raise SystemExit("malformed artifact row at line %d" % line_number)
            if not isinstance(row, dict):
                raise SystemExit("non-object artifact row at line %d" % line_number)
            for key in ("artifact", "patch", "source"):
                value = row.get(key)
                if value in (None, ""):
                    continue
                if not isinstance(value, str):
                    raise SystemExit("artifact row %d has invalid %s path" % (line_number, key))
                candidate = value if os.path.isabs(value) else os.path.join(repo, value)
                real = os.path.realpath(candidate)
                if inside(real, patch_root):
                    referenced.add(real)

cutoff = time.time() - int(days_raw) * 86400
for path in sorted(terminal_paths - active_paths - referenced):
    name = os.path.basename(path)
    if not (name.startswith("land-") and name.endswith(".patch")):
        continue
    try:
        info = os.stat(path, follow_symlinks=False)
    except OSError:
        continue
    if not stat.S_ISREG(info.st_mode) or info.st_mtime > cutoff:
        continue
    print("- landing-patch: retained (ownership unproven): %s" % path)
PY
}

if [ -f "$OMS/landings.jsonl" ] && [ -d "$OMS/landing-patches" ]; then
  landing_gc_out=""
  landing_gc_status=0
  landing_gc_out="$(oms_try_file_lock "$OMS/landings.jsonl" landing_patch_gc_locked)" ||
    landing_gc_status=$?
  case "$landing_gc_status" in
    0)
      [ -z "$landing_gc_out" ] || printf '%s\n' "$landing_gc_out"
      ;;
    75) echo "- landing-patch: skipped while a landing or recovery is active" ;;
    *) echo "error: gc: frozen landing patch maintenance failed" >&2; exit "$landing_gc_status" ;;
  esac
fi

# 1) Orphaned delegation markers and managed landing captures.
if [ -f "$OMS/landings.jsonl" ]; then
  managed_gc_status=0
  managed_gc_out="$(python3 "$ROOT_LIB/landing-capture.py" gc "$STATE_ROOT" \
    "$DAYS" "$((1 - DRY_RUN))")" || managed_gc_status=$?
  case "$managed_gc_status" in
    0) [ -z "$managed_gc_out" ] || printf '%s\n' "$managed_gc_out" ;;
    75) echo "- managed-landing-capture: skipped while a landing or lifecycle writer lock is active" ;;
    *) echo "error: managed landing retention refused unknown state" >&2; exit "$managed_gc_status" ;;
  esac
fi

delegation_set_lock="$STATE_ROOT/.oms/delegations/.marker-set-lock-target"
delegation_dir_is_safe() {  # DIRECTORY
  python3 - "$1" "$STATE_ROOT" <<'PY'
import os, stat, sys
path = os.path.abspath(sys.argv[1])
repo = os.path.realpath(sys.argv[2])
expected = os.path.join(repo, ".oms", "delegations")
try:
    info = os.lstat(path)
except FileNotFoundError:
    raise SystemExit(1)
except OSError:
    raise SystemExit(2)
if (not stat.S_ISDIR(info.st_mode) or
        os.path.normcase(os.path.realpath(path)) !=
        os.path.normcase(os.path.abspath(expected))):
    raise SystemExit(2)
PY
}

delegation_marker_names() {  # DIRECTORY [STRICT]
  python3 - "$1" "$STATE_ROOT" "${2:-}" <<'PY'
import os, re, stat, sys
path = os.path.abspath(sys.argv[1])
repo = os.path.realpath(sys.argv[2])
strict = sys.argv[3] == "strict"
expected = os.path.join(repo, ".oms", "delegations")
safe_name = re.compile(r"^[A-Za-z0-9._-]+\.json$")
try:
    info = os.lstat(path)
except OSError:
    raise SystemExit(2)
if (not stat.S_ISDIR(info.st_mode) or
        os.path.normcase(os.path.realpath(path)) !=
        os.path.normcase(os.path.abspath(expected))):
    raise SystemExit(2)
entries = []
try:
    with os.scandir(path) as iterator:
        for entry in iterator:
            if len(entries) >= 4096:
                raise SystemExit(2)
            entries.append(entry)
except OSError:
    raise SystemExit(2)
for entry in sorted(entries, key=lambda item: item.name):
    if not entry.name.endswith(".json"):
        continue
    if not safe_name.fullmatch(entry.name):
        sys.stderr.write("warning: gc: unsafe delegation marker name kept\n")
        if strict:
            raise SystemExit(2)
        continue
    print(entry.name)
PY
}

delegation_marker_names_locked() {  # DIRECTORY [STRICT]
  local directory_rc=0
  delegation_dir_is_safe "$1" || directory_rc=$?
  case "$directory_rc" in
    0) ;;
    1) return 1 ;;
    *) return 20 ;;
  esac
  delegation_marker_names "$1" "${2:-}" || return 21
}

delegation_marker_snapshot() {  # MARKER
  python3 - "$1" "$STATE_ROOT" "$ROOT/scripts/lib/process_liveness.py" <<'PY'
import hashlib, json, os, re, runpy, stat, sys

path = os.path.abspath(sys.argv[1])
repo = os.path.realpath(sys.argv[2])
expected_dir = os.path.join(repo, ".oms", "delegations")
marker_dir = os.path.dirname(path)
process_liveness = runpy.run_path(sys.argv[3])
pid_alive = process_liveness["pid_alive"]
persisted_native_pid_is_proven = process_liveness[
    "persisted_native_pid_is_proven"
]
safe_id = re.compile(r"^[A-Za-z0-9._-]*$")

def unproven(message):
    print("unproven\t" + message.replace("\t", " ").replace("\n", " "))
    raise SystemExit(0)

if not os.path.lexists(path):
    print("gone")
    raise SystemExit(0)
try:
    directory_info = os.lstat(marker_dir)
except OSError as exc:
    unproven("marker directory is unreadable: %s" % exc)
if (not stat.S_ISDIR(directory_info.st_mode) or
        os.path.normcase(os.path.realpath(marker_dir)) !=
        os.path.normcase(os.path.abspath(expected_dir))):
    unproven("marker directory is not the real repo-local delegation directory")
try:
    before = os.lstat(path)
except OSError as exc:
    unproven("marker is unreadable: %s" % exc)
if not stat.S_ISREG(before.st_mode):
    unproven("marker is not a regular file")
if before.st_size > 64 * 1024:
    unproven("marker exceeds 64 KiB")
flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
try:
    descriptor = os.open(path, flags)
except OSError as exc:
    unproven("marker cannot be opened safely: %s" % exc)
try:
    opened = os.fstat(descriptor)
    if (not stat.S_ISREG(opened.st_mode) or
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)):
        unproven("marker changed while opening")
    payload = os.read(descriptor, 64 * 1024 + 1)
    if len(payload) > 64 * 1024 or os.read(descriptor, 1):
        unproven("marker exceeds 64 KiB")
finally:
    os.close(descriptor)
try:
    marker = json.loads(payload.decode("utf-8"))
except (UnicodeError, ValueError):
    unproven("marker is malformed JSON")
if not isinstance(marker, dict):
    unproven("marker is not an object")
schema = marker.get("schema")
pid = marker.get("pid")
if (isinstance(schema, bool) or not isinstance(schema, int)
        or schema not in {1, 2, 3, 4}):
    unproven("marker schema is invalid")
if (isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0
        or pid > 0x7FFFFFFF):
    unproven("marker pid is invalid")
native_pid = marker.get("native_pid")
if schema == 4 and "native_pid" not in marker:
    unproven("schema 4 marker native pid is missing")
if "native_pid" in marker and (
        isinstance(native_pid, bool) or not isinstance(native_pid, int)
        or native_pid <= 0 or native_pid > 0xFFFFFFFF):
    unproven("marker native pid is invalid")
if not persisted_native_pid_is_proven(
        native_pid, marker.get("native_pid_source")):
    unproven("marker native pid source is missing or unproven")
for key in ("id", "task_id", "lease_id", "executor_id"):
    value = marker.get(key, "")
    if not isinstance(value, str) or not safe_id.fullmatch(value):
        unproven("marker %s is invalid" % key)
if marker.get("id", "") in {".", ".."}:
    unproven("marker id is not canonical")
if marker.get("executor_id", "") in {".", ".."}:
    unproven("marker executor_id is not canonical")
if not marker.get("id"):
    unproven("marker id is missing")
marker_owner = marker.get("autopilot_owner_id", "")
if (not isinstance(marker_owner, str) or
        (marker_owner and
         not re.fullmatch(r"owner_[0-9a-f]{32}", marker_owner))):
    unproven("marker autopilot owner id is invalid")
alive = pid_alive(pid, native_pid=native_pid)
print("\t".join([
    "alive" if alive else "dead", str(pid),
    str(native_pid) if isinstance(native_pid, int) else "0",
    marker.get("task_id", "") or "~", marker.get("lease_id", "") or "~",
    marker.get("executor_id", "") or "~", hashlib.sha256(payload).hexdigest(),
    str(opened.st_dev), str(opened.st_ino), str(opened.st_size),
]))
PY
}

delegation_marker_delete_snapshot() {  # MARKER DIGEST DEV INO SIZE
  python3 - "$1" "$STATE_ROOT" "$2" "$3" "$4" "$5" <<'PY'
import hashlib, os, stat, sys
path = os.path.abspath(sys.argv[1])
repo = os.path.realpath(sys.argv[2])
expected_digest = sys.argv[3]
expected_identity = tuple(int(value) for value in sys.argv[4:7])
expected_dir = os.path.join(repo, ".oms", "delegations")
marker_dir = os.path.dirname(path)
if (os.path.normcase(os.path.realpath(marker_dir)) !=
        os.path.normcase(os.path.abspath(expected_dir))):
    raise SystemExit(3)
try:
    directory_info = os.lstat(marker_dir)
    before = os.lstat(path)
except OSError:
    raise SystemExit(3)
if not stat.S_ISDIR(directory_info.st_mode) or not stat.S_ISREG(before.st_mode):
    raise SystemExit(3)
if (before.st_dev, before.st_ino, before.st_size) != expected_identity:
    raise SystemExit(3)
flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
try:
    descriptor = os.open(path, flags)
except OSError:
    raise SystemExit(3)
try:
    opened = os.fstat(descriptor)
    if (not stat.S_ISREG(opened.st_mode) or
            (opened.st_dev, opened.st_ino, opened.st_size) != expected_identity):
        raise SystemExit(3)
    payload = os.read(descriptor, 64 * 1024 + 1)
    if len(payload) > 64 * 1024 or os.read(descriptor, 1):
        raise SystemExit(3)
finally:
    os.close(descriptor)
if hashlib.sha256(payload).hexdigest() != expected_digest:
    raise SystemExit(3)
try:
    final = os.lstat(path)
except OSError:
    raise SystemExit(3)
if (not stat.S_ISREG(final.st_mode) or
        (final.st_dev, final.st_ino, final.st_size) != expected_identity):
    raise SystemExit(3)
os.unlink(path)
try:
    directory = os.open(marker_dir, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
except OSError:
    pass
PY
}

delegation_marker_gc() {  # MARKER
  local marker="$1" snapshot snapshot_rc status pid _native_pid task_id
  local marker_lease executor_id marker_digest marker_dev marker_ino marker_size
  local task_info task_state task_lease delete_rc=0
  local plan_show_rc plan_recovery_rc plan_show_err plan_task_absent
  local plan_recovery_out
  local recovery_hard_failure=0
  local -a recovery_args

  snapshot_rc=0
  snapshot="$(oms_with_file_lock "$delegation_set_lock" \
    delegation_marker_snapshot "$marker")" || snapshot_rc=$?
  snapshot="${snapshot//$'\r'/}"
  if [ "$snapshot_rc" -ne 0 ]; then
    echo "warning: gc: unproven delegation marker kept after parser failure: $marker" >&2
    return 0
  fi
  [ "$snapshot" != gone ] || return 0
  IFS=$'\t' read -r status pid _native_pid task_id marker_lease executor_id \
    marker_digest marker_dev marker_ino marker_size <<EOF
$snapshot
EOF
  if [ "$status" = unproven ]; then
    echo "warning: gc: unproven delegation marker kept: $marker ($pid)" >&2
    return 0
  fi
  [ "$task_id" != '~' ] || task_id=""
  [ "$marker_lease" != '~' ] || marker_lease=""
  [ "$executor_id" != '~' ] || executor_id=""
  [ "$status" = dead ] || return 0

  if [ -n "$executor_id" ]; then
    echo "warning: gc: retired Soul executor $executor_id; keeping legacy marker and evidence" >&2
    return 0
  fi
  if [ -n "$task_id" ]; then
    plan_show_rc=0
    plan_show_err="$(mktemp "${TMPDIR:-/tmp}/oms-gc-plan.XXXXXX")" || plan_show_err=""
    task_info="$("$ROOT/scripts/agent-plan.sh" --repo "$STATE_ROOT" show --id "$task_id" 2>"${plan_show_err:-/dev/null}" |
      python3 -c 'import json,sys;d=json.load(sys.stdin);print("%s\t%s"%(d.get("state",""),d.get("lease_id","")))' 2>/dev/null)" ||
      plan_show_rc=$?
    # A readable plan that lacks the task (retired and replaced) has nothing to recover.
    plan_task_absent=0
    # The core reports an unsearchable plan directory the same way, so trust the
    # message only while the plan store itself is readable.
    if [ "$plan_show_rc" -ne 0 ] && [ -n "$plan_show_err" ] &&
        [ -r "$STATE_ROOT/.oms/plan/tasks.json" ] &&
        [ "$(tr -d '\r' < "$plan_show_err")" = "error: no such task: $task_id" ]; then
      plan_task_absent=1
    fi
    [ -z "$plan_show_err" ] || rm -f "$plan_show_err"
    task_info="${task_info//$'\r'/}"
    task_state="$(printf '%s' "$task_info" | cut -f1)"
    task_lease="$(printf '%s' "$task_info" | cut -f2)"
    if [ "$plan_task_absent" -eq 1 ]; then
      printf -- '- orphan-delegation-plan: task %s is absent from the current plan; nothing to recover\n' "$task_id"
    elif [ "$plan_show_rc" -ne 0 ]; then
      echo "warning: gc: plan task $task_id is unreadable; kept trigger evidence" >&2
      recovery_hard_failure=1
    else
      case "$task_state" in
        claimed|running)
          if [ -z "$marker_lease" ]; then
            printf -- '- orphan-delegation-plan: task %s marker has no lease; keep current claim\n' "$task_id"
          elif [ "$marker_lease" != "$task_lease" ]; then
            printf -- '- orphan-delegation-plan: task %s lease changed; keep current claim\n' "$task_id"
          else
            recovery_args=(--repo "$STATE_ROOT" recover-lease --id "$task_id" \
              --lease-id "$marker_lease" --expected-state "$task_state" \
              --markers-dir "$OMS/delegations")
            [ "$DRY_RUN" = 0 ] || recovery_args+=(--check)
            plan_recovery_rc=0
            plan_recovery_out="$(OMS_HARNESS_CHILD=1 "$ROOT/scripts/agent-plan.sh" \
              "${recovery_args[@]}" 2>&1)" || plan_recovery_rc=$?
            plan_recovery_out="${plan_recovery_out//$'\r'/}"
            if [ "$plan_recovery_rc" -eq 0 ]; then
              printf -- '- orphan-delegation-plan: task %s (%s) -> ready\n' "$task_id" "$task_state"
            else
              echo "warning: gc: plan task $task_id changed or has a live exact worker; kept it" >&2
              if [ "$plan_recovery_rc" -ne 3 ] ||
                  printf '%s\n' "$plan_recovery_out" |
                    grep -Fx 'plan-recovery-outcome: unproven' > /dev/null; then
                recovery_hard_failure=1
              fi
            fi
          fi
          ;;
        ready|review|landing|blocked|done|cancelled) ;;
        *)
          echo "warning: gc: plan task $task_id has an invalid state; kept trigger evidence" >&2
          recovery_hard_failure=1
          ;;
      esac
    fi
  fi

  if [ "$recovery_hard_failure" -ne 0 ]; then
    echo "warning: gc: delegation recovery was unproven; kept trigger marker: $marker" >&2
    return 0
  fi
  if [ "$DRY_RUN" = 0 ]; then
    oms_with_file_lock "$delegation_set_lock" \
      delegation_marker_delete_snapshot "$marker" "$marker_digest" \
        "$marker_dev" "$marker_ino" "$marker_size" || delete_rc=$?
    if [ "$delete_rc" -ne 0 ]; then
      echo "warning: gc: delegation marker generation changed; kept it: $marker" >&2
      return 0
    fi
  fi
  printf -- '- orphan-delegation: %s\n' "$marker"
}

delegation_dir="$OMS/delegations"
delegation_dir_rc=0
delegation_names="$(oms_with_file_lock "$delegation_set_lock" \
  delegation_marker_names_locked "$delegation_dir")" || delegation_dir_rc=$?
delegation_names="${delegation_names//$'\r'/}"
case "$delegation_dir_rc" in
  0)
    while IFS= read -r delegation_name; do
      [ -n "$delegation_name" ] || continue
      f="$delegation_dir/$delegation_name"
      delegation_marker_rc=0
      delegation_out="$(delegation_marker_gc "$f")" || delegation_marker_rc=$?
      if [ "$delegation_marker_rc" -ne 0 ]; then
        echo "warning: gc: could not safely inspect delegation marker (rc=$delegation_marker_rc): $f" >&2
        continue
      fi
      [ -z "$delegation_out" ] || printf '%s\n' "$delegation_out"
      delegation_changes="$(printf '%s\n' "$delegation_out" |
        awk '/^- orphan-delegation: / ||
             /^- orphan-delegation-executor: .* -> failed$/ ||
             /^- orphan-delegation-plan: .* -> ready$/ {n++}
             END {print n+0}')"
      removed=$((removed + delegation_changes))
    done <<EOF_DELEGATION_NAMES
$delegation_names
EOF_DELEGATION_NAMES
    ;;
  1) ;;
  20)
    echo "warning: gc: delegation directory is not a real repo-local directory; preserved it" >&2
    ;;
  21)
    echo "warning: gc: delegation directory exceeds bounds or changed; preserved it" >&2
    ;;
  *)
    echo "warning: gc: delegation marker-set lock failed (rc=$delegation_dir_rc); preserved it" >&2
    ;;
esac

# 1.5) Reconcile non-terminal attempts that stopped reporting.
lifecycle_events="$OMS/lifecycle/events.jsonl"
if [ -f "$lifecycle_events" ] && [ ! -L "$lifecycle_events" ]; then
  attempt_stale_seconds="${OMS_ATTEMPT_STALE_SECONDS:-86400}"
  case "$attempt_stale_seconds" in
    *[!0-9]*|"") echo "error: gc: OMS_ATTEMPT_STALE_SECONDS must be a non-negative integer" >&2; exit 2 ;;
  esac
  reconcile_args=(--repo "$STATE_ROOT" reconcile --stale-seconds "$attempt_stale_seconds")
  [ "$DRY_RUN" = 1 ] || reconcile_args+=(--apply)
  if ! reconcile_out="$("$ROOT/scripts/agent-events.sh" "${reconcile_args[@]}" 2>&1)"; then
    printf '%s\n' "$reconcile_out" >&2
    echo "error: gc: lifecycle reconcile failed" >&2
    exit 1
  fi
  while IFS= read -r reconcile_line; do
    case "$reconcile_line" in
      ""|"no stale attempts") continue ;;
    esac
    printf -- '- stale-attempt: %s\n' "$reconcile_line"
    removed=$((removed + 1))
  done <<EOF
$reconcile_out
EOF

fi

# 2) Archived task packets older than --days.
if [ -d "$OMS/task/archive" ]; then
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    note_remove "task-archive" "$f"
  done <<EOF
$(find "$OMS/task/archive" -maxdepth 1 -type f -name '*.md' -mtime +"$DAYS" 2>/dev/null)
EOF
fi

# 2.1) Local tracked-state checkpoints older than --days.
cutoff_epoch=$(( $(date +%s) - DAYS * 86400 ))
if [ -d "$OMS/checkpoints" ] && [ ! -L "$OMS/checkpoints" ]; then
  for checkpoint_dir in "$OMS/checkpoints"/cp-*; do
    if [ ! -d "$checkpoint_dir" ] || [ -L "$checkpoint_dir" ]; then
      continue
    fi
    checkpoint_created="$(python3 - "$checkpoint_dir/meta.json" <<'PY' 2>/dev/null || true
import datetime, json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    value = json.load(handle).get("created_at", "")
if value.endswith("Z"):
    value = value[:-1] + "+00:00"
stamp = datetime.datetime.fromisoformat(value)
if stamp.tzinfo is None:
    stamp = stamp.replace(tzinfo=datetime.timezone.utc)
print(int(stamp.timestamp()))
PY
)"
    case "$checkpoint_created" in *[!0-9]*|"") continue ;; esac
    [ "$checkpoint_created" -lt "$cutoff_epoch" ] || continue
    note_remove "checkpoint" "$checkpoint_dir"
  done
fi

# 2.2) Hook state compaction under the marker-set lock.
hook_events="$OMS/hooks/events.jsonl"
hook_sessions="$OMS/hooks/sessions"
compact_hook_events() {
  local -a compact_args
  compact_args=(compact-events --path "$1" --cutoff "$2")
  [ "$3" = 0 ] || compact_args+=(--apply)
  python3 "$ROOT_LIB/cached-main.py" "$ROOT_LIB/hook_state.py" "${compact_args[@]}"
}
delegations_quiescent() {
  local marker_names marker_names_rc marker_name marker_snapshot marker_rc marker_state

  marker_names_rc=0
  marker_names="$(delegation_marker_names_locked "$delegation_dir" strict)" || marker_names_rc=$?
  marker_names="${marker_names//$'\r'/}"
  case "$marker_names_rc" in
    0) ;;
    1) marker_names="" ;;
    *) return 20 ;;
  esac
  while IFS= read -r marker_name; do
    [ -n "$marker_name" ] || continue
    marker_rc=0
    marker_snapshot="$(delegation_marker_snapshot "$delegation_dir/$marker_name")" || marker_rc=$?
    marker_snapshot="${marker_snapshot//$'\r'/}"
    [ "$marker_rc" -eq 0 ] || return 21
    marker_state="${marker_snapshot%%$'\t'*}"
    [ "$marker_state" = dead ] || return 22
  done <<EOF_HOOK_MARKERS
$marker_names
EOF_HOOK_MARKERS
}

hook_state_cleanup_locked() {
  local hook_counts hook_before hook_after hook_session hook_mtime removed_before
  delegations_quiescent || return $?

  removed_before="$removed"
  if [ -f "$hook_events" ] && [ ! -L "$hook_events" ]; then
    hook_counts="$(compact_hook_events "$hook_events" "$cutoff_epoch" "$((1 - DRY_RUN))")" || return 30
    hook_counts="${hook_counts//$'\r'/}"
    hook_before="$(printf '%s' "$hook_counts" | cut -f1)"
    hook_after="$(printf '%s' "$hook_counts" | cut -f2)"
    case "$hook_before" in *[!0-9]*|"") return 31 ;; esac
    case "$hook_after" in *[!0-9]*|"") return 31 ;; esac
    if [ "$hook_after" -lt "$hook_before" ]; then
      printf -- '- hook-events: compact %s -> %s rows\n' "$hook_before" "$hook_after"
      removed=$((removed + hook_before - hook_after))
    fi
  fi

  if [ -d "$hook_sessions" ] && [ ! -L "$hook_sessions" ]; then
    for hook_session in "$hook_sessions"/*; do
      if [ ! -f "$hook_session" ] || [ -L "$hook_session" ]; then
        continue
      fi
      hook_mtime="$(python3 -c 'import os,sys; print(int(os.path.getmtime(sys.argv[1])))' \
        "$hook_session" 2>/dev/null || true)"
      hook_mtime="${hook_mtime//$'\r'/}"
      case "$hook_mtime" in *[!0-9]*|"") continue ;; esac
      [ "$hook_mtime" -lt "$cutoff_epoch" ] || continue
      note_remove "hook-session" "$hook_session"
    done
  fi
  printf '__OMS_GC_HOOK_REMOVED__%s\n' "$((removed - removed_before))"
}

if { [ -f "$hook_events" ] && [ ! -L "$hook_events" ]; } ||
   { [ -d "$hook_sessions" ] && [ ! -L "$hook_sessions" ]; }; then
  hook_cleanup_status=0
  hook_cleanup_out="$(oms_with_file_lock "$delegation_set_lock" hook_state_cleanup_locked)" || \
    hook_cleanup_status=$?
  case "$hook_cleanup_status" in
    0)
      hook_cleanup_changes="$(printf '%s\n' "$hook_cleanup_out" |
        sed -n 's/^__OMS_GC_HOOK_REMOVED__\([0-9][0-9]*\)$/\1/p' | tail -n 1)"
      case "$hook_cleanup_changes" in
        *[!0-9]*|"") echo "error: gc: could not account for hook cleanup" >&2; exit 1 ;;
      esac
      hook_cleanup_out="$(printf '%s\n' "$hook_cleanup_out" |
        sed '/^__OMS_GC_HOOK_REMOVED__[0-9][0-9]*$/d')"
      [ -z "$hook_cleanup_out" ] || printf '%s\n' "$hook_cleanup_out"
      removed=$((removed + hook_cleanup_changes))
      ;;
    20|21|22|75)
      echo "warning: gc: hook cleanup skipped while a delegation is live or marker evidence/lock is unverified (rc=$hook_cleanup_status)" >&2
      ;;
    30|31)
      echo "error: gc: could not compact hook events; hook sessions were kept" >&2
      exit 1
      ;;
    *)
      echo "error: gc: could not safely complete hook cleanup (rc=$hook_cleanup_status)" >&2
      exit 1
      ;;
  esac
fi

# Dispatch logs cleanup under marker-set -> plan locks.
dispatch_logs_gc_locked() {
  delegations_quiescent || return 24
  python3 - "$ROOT_LIB" "$STATE_ROOT" "$DAYS" "$DRY_RUN" <<'PY_DISPATCH_LOGS' || return 1
import json, os, re, runpy, stat, subprocess, sys, time
from pathlib import Path

sys.path.insert(0, sys.argv[1])
import terminal_panel as panel
from process_liveness import pid_alive

repo = Path(sys.argv[2])
cutoff = time.time() - int(sys.argv[3]) * 86400
dry = sys.argv[4] == "1"
pairs = files = size = 0
failed = False
ops = runpy.run_path(str(Path(sys.argv[1]) / "agent-events.py"))
errors = (OSError, ValueError, KeyError, TypeError, AttributeError,
          RecursionError, subprocess.SubprocessError, ops["OpsError"])

def generation(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("not a single-link regular file")
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)

def optional_generation(path):
    try:
        return generation(path)
    except FileNotFoundError:
        return None

try:
    directory = repo / ".oms/panel-state/dispatch-logs"
    ancestry = panel.DURABLE["component_snapshot"](str(repo), str(directory / "probe"), "dispatch logs")
    ledger = repo / ".oms/lifecycle/events.jsonl"
    try:
        ledger_generation = generation(ledger)
    except FileNotFoundError as error:
        if dry:
            print("warning: gc: dispatch log evidence unavailable: %s" % error, file=sys.stderr)
            print("- dispatch-logs: 0 pair(s), 0 file(s), 0 bytes")
        raise SystemExit(0)
    attempts = list(ops["project_attempts"](ops["read_rows"](ledger)).values())
    # A present but unreadable plan is different from a repository with no plan.
    raw_plan = panel._read(repo, ".oms/plan/tasks.json", 16 * 1024 * 1024, missing=True)
    tasks = {}
    if raw_plan:
        plan = json.loads(raw_plan)
        if (not isinstance(plan, dict) or type(plan.get("schema")) is not int
                or plan["schema"] not in {1, 2, 3} or not isinstance(plan.get("tasks"), dict)):
            raise ValueError("unknown plan shape")
        for ident, task in plan["tasks"].items():
            if (not isinstance(task, dict) or task.get("id") != ident
                    or task.get("state") not in {"ready", "claimed", "running", "review", "landing", "blocked", "done", "cancelled"}):
                raise ValueError("unknown plan task")
            tasks[ident] = task
    elif os.path.lexists(repo / ".oms/plan/tasks.json"):
        raise ValueError("empty plan")
    by_call, by_task = {}, {}
    for attempt in attempts:
        by_call.setdefault(attempt["refs"].get("panel_room_participant"), []).append(attempt)
        by_task.setdefault(attempt["task_id"], []).append(attempt)
    for activity in sorted(directory.glob("call-*.activity.json")):
        member = activity.name[:-len(".activity.json")]
        if not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", member):
            continue
        # Keep activity last: any interruption or failed earlier unlink leaves
        # the next sweep an anchor, including after the output log is gone.
        paths = [directory / (member + suffix) for suffix in (".log", ".stream", ".transcript")] + [activity]
        try:
            before = {path: optional_generation(path) for path in paths}
            if before[activity] is None or any(item[3] >= cutoff * 1000000000 for item in before.values() if item):
                continue
            linked = by_call.get(member, [])
            if not linked or any(a["terminal"] is not True for a in linked):
                continue
            # Review/approval/blocked attempts for this task can still need salvage.
            if any(not a["terminal"] for own in linked for a in by_task[own["task_id"]]):
                continue
            protected = False
            for own in linked:
                task = tasks.get(own["task_id"])
                if task is None:
                    # A loaded plan that omits this task cannot prove its disposition.
                    protected = bool(raw_plan)
                elif task["state"] not in {"done", "cancelled"}:
                    latest = max(ops["parse_ts"](a["created_at"]) for a in by_task[own["task_id"]])
                    protected = (task["state"] in {"claimed", "running", "review", "landing"}
                                 or ops["parse_ts"](own["created_at"]) >= latest)
                if protected:
                    break
            if protected:
                continue
            launch = json.loads(panel._read(repo, panel._dispatch_call_path(member), 4096))
            call = panel._dispatch_call_record(repo, launch["room"], launch)
            if (not call or any(a["refs"].get("panel_room_id") != call["room"] for a in linked)
                    or call.get("task_id") and any(a["task_id"] != call["task_id"] for a in linked)):
                continue
            exited = json.loads(panel._read(repo, panel._dispatch_call_path(member, True), 8192))
            if (type(exited.get("exit")) is not int
                    or exited != panel._dispatch_exit_record(call, exited["exit"])
                    or not panel._dispatch_call_finished(repo, call["room"], call)
                    or not panel._dispatch_released(repo, call)):
                continue
            pending = panel._dispatch_pending(repo, call)
            released = panel.dispatch_store.read(repo, member + "-settled.json")
            snapshot = panel._dispatch_supervisor_snapshot(repo, call, allow_stale=True)
            if (not snapshot or snapshot[0]["all_reaped"] is not True
                    or snapshot[0]["exit"] != exited["exit"] or pid_alive(snapshot[1]["pid"])
                    or pending["exit"] != exited["exit"] or released["exit"] != exited["exit"]
                    or any(a["task_id"] != pending["task_id"] for a in linked)):
                continue
            claim = panel.dispatch_store.read(repo, panel._settlement_claim_path(member))
            if (claim.get("schema") != 1 or claim.get("member") != member
                    or claim.get("complete") is not True or claim.get("active") is not False
                    or type(claim.get("exit")) is not int or claim["exit"] != exited["exit"]
                    or any(claim.get("steps", {}).get(step) is not True
                           for step in ("settlement", "room_result", "structured_event", "auto_review"))):
                continue
            panel.DURABLE["components_unchanged"](ancestry, "dispatch logs")
            if generation(ledger) != ledger_generation:
                break
            if before != {path: optional_generation(path) for path in paths}:
                continue
            group_failed = False
            before_files = files
            for path in paths:
                if before[path] is None or path == activity and group_failed:
                    continue
                if not dry:
                    try:
                        path.unlink()
                    except FileNotFoundError:
                        continue  # A concurrent transcript prune already removed it.
                    except OSError as error:
                        group_failed = failed = True
                        print("warning: gc: dispatch log unlink failed: %s" % error, file=sys.stderr)
                        continue
                files += 1
                size += before[path][2]
            if files > before_files:
                pairs += 1
        except errors:
            continue  # Unknown call evidence cannot authorize either unlink.
except errors as error:
    print("warning: gc: dispatch log evidence unavailable: %s" % error, file=sys.stderr)
print("- dispatch-logs: %d pair(s), %d file(s), %d bytes" % (pairs, files, size))
if failed:
    raise SystemExit(1)
PY_DISPATCH_LOGS
}
dispatch_logs_gc_plan_lock() {
  oms_try_file_lock "$OMS/plan/tasks.json" dispatch_logs_gc_locked
}
if [ -d "$OMS/panel-state/dispatch-logs" ]; then
  dispatch_gc_status=0
  dispatch_gc_out="$(oms_try_file_lock "$delegation_set_lock" dispatch_logs_gc_plan_lock)" || dispatch_gc_status=$?
  dispatch_gc_out="${dispatch_gc_out//$'\r'/}"
  case "$dispatch_gc_status" in
    0)
      if [ -n "$dispatch_gc_out" ]; then
        printf '%s\n' "$dispatch_gc_out"
        dispatch_gc_count="$(printf '%s\n' "$dispatch_gc_out" | awk '/^- dispatch-logs:/ {print $5}')"
        case "$dispatch_gc_count" in
          *[!0-9]*|"") fail "could not account for dispatch log cleanup" ;;
        esac
        removed=$((removed + dispatch_gc_count))
      fi
      ;;
    24|75)
      echo "warning: gc: dispatch log cleanup skipped while a delegation is live or evidence/lock is unverified (rc=$dispatch_gc_status)" >&2
      ;;
    *)
      [ -z "$dispatch_gc_out" ] || printf '%s\n' "$dispatch_gc_out"
      echo "error: gc: dispatch log cleanup failed (rc=$dispatch_gc_status)" >&2
      exit 1
      ;;
  esac
fi

# 2.3) Handoff digests older than --days.
if [ -d "$OMS/handoffs" ]; then
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    note_remove "handoff" "$f"
  done <<EOF
$(find "$OMS/handoffs" -maxdepth 1 -type f -name '*.md' -mtime +"$DAYS" 2>/dev/null)
EOF
fi

# 2.5) Stale open runs whose last spine event is older than --days.
if [ -f "$OMS/runs/spine.jsonl" ]; then
  stale_open="$(OMS_DAYS="$DAYS" python3 - "$OMS/runs/spine.jsonl" <<'PY'
import calendar, json, os, sys, time
cutoff = time.time() - int(os.environ["OMS_DAYS"]) * 86400
last, closed, order = {}, set(), []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line:
        continue
    try:
        r = json.loads(line)
    except Exception:
        continue
    rid = r.get("run_id")
    if not rid:
        continue
    if rid not in last:
        order.append(rid)
    try:
        ts = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        ts = None
    if ts is not None and ts > last.get(rid, 0):
        last[rid] = ts
    if r.get("tool") == "oms-run" and r.get("event") == "close":
        closed.add(rid)
for rid in order:
    if rid not in closed and last.get(rid) and last[rid] < cutoff:
        print(rid)
PY
)"
  for rid in $stale_open; do
    printf -- '- stale-run-close: %s\n' "$rid"
    removed=$((removed + 1))
    if [ "$DRY_RUN" = 0 ]; then
      OMS_RUN_INDEX="$OMS/runs/spine.jsonl" \
        "$ROOT/scripts/run.sh" close --run-id "$rid" --note "gc: no event in ${DAYS}d" >/dev/null 2>&1 ||
        echo "warning: gc: could not close run $rid" >&2
    fi
  done
fi

# 2.6) Crashed-writer replace scratch older than 60 minutes.
while IFS= read -r f; do
  [ -n "$f" ] || continue
  note_remove "replace-scratch" "$f"
done <<EOF
$(find "$OMS" -type f -name '.oms-replace.*' -mmin +60 2>/dev/null)
EOF

# 3) Run capsules older than --days whose run is not open.
open_ids=""
if [ -f "$OMS/runs/spine.jsonl" ]; then
  open_ids="$(OMS_RUN_INDEX="$OMS/runs/spine.jsonl" "$ROOT/scripts/run.sh" ls --open 2>/dev/null | awk '{print $1}')"
fi
if [ -d "$OMS/runs" ]; then
  while IFS= read -r d; do
    [ -n "$d" ] || continue
    id="$(basename "$d")"
    skip=0
    for oid in $open_ids; do
      [ "$id" = "$oid" ] && { skip=1; break; }
    done
    [ "$skip" = 1 ] && continue
    note_remove "capsule" "$d"
  done <<EOF
$(find "$OMS/runs" -mindepth 1 -maxdepth 1 -type d -mtime +"$DAYS" 2>/dev/null)
EOF
fi

# 4) Retired failure rows older than --days.
fail_ledger="$OMS/failures.jsonl"
if [ -f "$fail_ledger" ]; then
  hook_fail_ttl="${OMS_HOOK_FAIL_TTL:-86400}"
  case "$hook_fail_ttl" in *[!0-9]*|"") hook_fail_ttl=86400 ;; esac
  failures_gc_locked() {
    local before after stage py_status=0
    before="$(wc -l < "$fail_ledger" | tr -d ' ')"
    stage="$(mktemp "$(dirname "$fail_ledger")/.oms-replace.XXXXXX")" || return 1
    OMS_DAYS="$DAYS" OMS_HOOK_TTL="$hook_fail_ttl" \
      python3 - "$fail_ledger" "$stage" <<'PY' || py_status=$?
import calendar, json, os, sys, time
days = int(os.environ["OMS_DAYS"])
ttl = int(os.environ.get("OMS_HOOK_TTL") or 86400)
cutoff = time.time() - days * 86400
now = time.time()
rows = []
malformed = False
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line:
        continue
    try:
        row = json.loads(line)
    except Exception:
        malformed = True
        continue
    if not isinstance(row, dict):
        malformed = True
        continue
    rows.append(row)

if malformed:
    raise SystemExit(3)

def hook_expired(r):
    if r.get("kind") != "hook" or r.get("event") != "fail":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False
    return (now - t) >= ttl

open_fails = {}
for r in rows:
    fp = r.get("fingerprint")
    if not fp:
        continue
    ev = r.get("event")
    if ev == "resolved":
        open_fails[fp] = 0
    elif ev == "fail":
        open_fails.setdefault(fp, 0)
        if not hook_expired(r):
            open_fails[fp] += 1
retired = {fp: count == 0 for fp, count in open_fails.items()}

def old(r):
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False
    return t < cutoff

with open(sys.argv[2], "w", encoding="utf-8") as out:
    for r in rows:
        fp = r.get("fingerprint")
        if fp and retired.get(fp) and old(r):
            continue
        out.write(json.dumps(r, ensure_ascii=False) + "\n")
PY
    case "$py_status" in
      0)
        after="$(wc -l < "$stage" | tr -d ' ')"
        if [ "$after" -lt "$before" ]; then
          printf -- '- failures: compact %s -> %s rows\n' "$before" "$after"
          if [ "$DRY_RUN" = 0 ]; then
            mv "$stage" "$fail_ledger"
          else
            rm -f "$stage"
          fi
        else
          rm -f "$stage"
        fi
        ;;
      3)
        rm -f "$stage"
        printf -- '- failures: compaction refused (malformed rows preserved for inspection)\n'
        ;;
      *)
        rm -f "$stage"
        return "$py_status"
        ;;
    esac
  }
  failures_gc_status=0
  failures_gc_out="$(oms_try_file_lock "$fail_ledger" failures_gc_locked)" ||
    failures_gc_status=$?
  case "$failures_gc_status" in
    0)
      if [ -n "$failures_gc_out" ]; then
        printf '%s\n' "$failures_gc_out"
        case "$failures_gc_out" in
          *'failures: compact '*) removed=$((removed + 1)) ;;
        esac
      fi
      ;;
    75) echo "- failures: skipped while another writer holds the ledger" ;;
    *) echo "error: gc: failure-ledger compaction failed" >&2; exit "$failures_gc_status" ;;
  esac
fi

# 4.2) Lifecycle events of long-terminal attempts.
if [ -f "$OMS/lifecycle/events.jsonl" ]; then
  lifecycle_gc_args=(--repo "$STATE_ROOT" compact --days "$DAYS")
  [ "$DRY_RUN" = 1 ] || lifecycle_gc_args+=(--apply)
  if lifecycle_gc_out="$("$ROOT/scripts/agent-events.sh" "${lifecycle_gc_args[@]}" 2>&1)"; then
    printf -- '- %s\n' "$lifecycle_gc_out"
    case "$lifecycle_gc_out" in
      *'dropped '*) removed=$((removed + 1)) ;;
    esac
  else
    echo "- lifecycle: compaction skipped ($lifecycle_gc_out)"
  fi
fi

# 4.4) Retain opt-in and historical usage under OMS_USAGE_TTL.
usage_file="$OMS/usage.jsonl"
if [ -f "$usage_file" ] && [ ! -L "$usage_file" ]; then
  usage_ttl="${OMS_USAGE_TTL:-2592000}"
  case "$usage_ttl" in *[!0-9]*|"") usage_ttl=2592000 ;; esac
  usage_gc_locked() {
    local before after stage py_status=0
    before="$(wc -l < "$usage_file" | tr -d ' ')"
    stage="$(mktemp "$(dirname "$usage_file")/.oms-replace.XXXXXX")" || return 1
    OMS_TTL="$usage_ttl" python3 - "$usage_file" "$stage" <<'PY' || py_status=$?
import datetime, json, os, sys
ttl = int(os.environ.get("OMS_TTL") or 2592000)
today = datetime.date.today()
counts = {}
order = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line:
        continue
    try:
        row = json.loads(line)
    except Exception:
        continue
    fam, day = row.get("family"), row.get("day")
    if not fam or not day:
        continue
    try:
        age = (today - datetime.date.fromisoformat(day)).days * 86400
    except (TypeError, ValueError):
        continue
    if age > ttl or age < 0:
        continue
    key = (fam, day)
    if key not in counts:
        order.append(key)
    try:
        counts[key] = counts.get(key, 0) + int(row.get("count") or 1)
    except (TypeError, ValueError):
        counts[key] = counts.get(key, 0) + 1
out = open(sys.argv[2], "w", encoding="utf-8")
for fam, day in order:
    out.write(json.dumps({"schema": 1, "family": fam, "day": day,
                      "count": counts[(fam, day)]}, sort_keys=True) + "\n")
PY
    if [ "$py_status" -ne 0 ]; then
      rm -f "$stage"
      return "$py_status"
    fi
    after="$(wc -l < "$stage" | tr -d ' ')"
    if [ "$after" -lt "$before" ]; then
      printf -- '- usage: compact %s -> %s rows\n' "$before" "$after"
      if [ "$DRY_RUN" = 0 ]; then
        mv "$stage" "$usage_file"
      else
        rm -f "$stage"
      fi
    else
      rm -f "$stage"
    fi
  }
  usage_gc_status=0
  usage_gc_out="$(oms_try_file_lock "$usage_file" usage_gc_locked)" ||
    usage_gc_status=$?
  case "$usage_gc_status" in
    0)
      if [ -n "$usage_gc_out" ]; then
        printf '%s\n' "$usage_gc_out"
        case "$usage_gc_out" in
          *'usage: compact '*) removed=$((removed + 1)) ;;
        esac
      fi
      ;;
    75) echo "- usage: skipped while another writer holds the file" ;;
    *) echo "error: gc: usage compaction failed" >&2; exit "$usage_gc_status" ;;
  esac
fi

# 4.5) Abandoned change-guards (dead owner pid or aged snapshot).
guard_file="$OMS/guards/change-guard.tsv"
if [ -f "$guard_file" ]; then
  guard_pid="$(awk -F'\t' '$1=="pid"{print $2; exit}' "$guard_file")"
  guard_started="$(awk -F'\t' '$1=="started"{print $2; exit}' "$guard_file")"
  case "$guard_started" in *[!0-9]*) guard_started="" ;; esac
  guard_dead=0
  if [ -n "$guard_pid" ]; then
    kill -0 "$guard_pid" 2>/dev/null || guard_dead=1
  elif [ -n "$guard_started" ]; then
    now_s="$(date +%s)"
    [ $((now_s - guard_started)) -gt $((DAYS * 86400)) ] && guard_dead=1
  else
    [ -n "$(find "$guard_file" -mtime +"$DAYS" 2>/dev/null)" ] && guard_dead=1
  fi
  if [ "$guard_dead" = 1 ]; then
    note_remove "stale-change-guard" "$guard_file"
  fi
fi

# 4b) Closed threads and work rooms older than --days.
gc_thread_unlocked() {
  python3 - "$ROOT_LIB" "$STATE_ROOT" "$1" "$cutoff" "$DRY_RUN" <<'PY'
import os, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import room
repo, filename, cutoff, dry_run = Path(sys.argv[2]), Path(sys.argv[3]), int(sys.argv[4]), sys.argv[5] == "1"
try:
    ident = room.identifier(filename.stem, "thread")
    with room.thread_live.open_thread(repo, ident) as handle:
        before = os.fstat(handle.fileno())
        if before.st_mtime >= cutoff:
            raise ValueError("recent thread")
        data = handle.read(room.thread_live.MAX_FILE + 1)
        rows = room.decode_records(data, ident)
    if any(row.get("room_event") for row in rows):
        state = room.project(rows)
        if any(message["pending_for"] for message in state["messages"]):
            raise ValueError("unread room mail")
        if any(member["joined"] for member in state["participants"]):
            raise ValueError("joined room")
        if state["closed"]:
            kind = "closed-room"
        elif not state["messages"]:
            kind = "empty-room"
        else:
            raise ValueError("active room")
    elif any(row.get("role") == "closed" for row in rows):
        kind = "closed-thread"
    else:
        raise ValueError("open thread")
    current = filename.lstat()
    generation = lambda info: (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    if generation(before) != generation(current):
        raise ValueError("thread changed")
    if not dry_run:
        filename.unlink()
    print("- %s: %s" % (kind, filename))
except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError):
    pass
PY
}
if [ -d "$OMS/threads" ]; then
  cutoff=$(( $(date +%s) - DAYS * 86400 ))
  for f in "$OMS/threads"/*.jsonl; do
    [ -f "$f" ] && [ ! -L "$f" ] || continue
    thread_gc_out="$(oms_with_file_lock "$f" gc_thread_unlocked "$f")" || fail "thread cleanup failed"
    if [ -n "$thread_gc_out" ]; then
      printf '%s\n' "$thread_gc_out"
      removed=$((removed + 1))
    fi
  done
fi

# 5) Artifacts retention via artifact-index prune.
if [ -f "$OMS/artifacts/index.jsonl" ]; then
  artifact_keep="${OMS_ARTIFACT_INDEX_KEEP:-1000}"
  artifact_grace="${OMS_ARTIFACT_ORPHAN_GRACE:-86400}"
  artifact_args=(--repo "$STATE_ROOT" prune "$artifact_keep")
  [ "$DELETE_ORPHAN_FILES" != 1 ] || artifact_args+=(--files)
  [ "$DRY_RUN" = 0 ] || artifact_args+=(--dry-run)
  artifact_out=""
  if ! artifact_out="$(OMS_ARTIFACT_ORPHAN_GRACE="$artifact_grace" \
      "$ROOT/scripts/artifact-index.sh" "${artifact_args[@]}" 2>&1)"; then
    printf '%s\n' "$artifact_out" >&2
    echo "error: gc: artifact maintenance failed" >&2
    exit 1
  fi
  while IFS= read -r artifact_line; do
    [ -z "$artifact_line" ] || printf -- '- artifacts: %s\n' "$artifact_line"
  done <<< "$artifact_out"
  artifact_changes="$(printf '%s\n' "$artifact_out" | python3 -c 'import re,sys
s=sys.stdin.read(); n=0
for a,b in re.findall(r"(?:would prune|pruned) (\d+) -> (\d+)",s): n += max(0,int(a)-int(b))
for x in re.findall(r"(?:would delete|deleted) (\d+) orphan file",s): n += int(x)
print(n)')"
  removed=$((removed + artifact_changes))
fi

if [ "$removed" -eq 0 ]; then
  echo "gc: nothing to reclaim"
elif [ "$DRY_RUN" = 1 ]; then
  echo "gc: $removed item(s) would be reclaimed"
  echo "gc: re-run with --apply to remove"
else
  echo "gc: $removed item(s) reclaimed"
fi
