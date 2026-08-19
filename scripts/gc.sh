#!/usr/bin/env bash
set -euo pipefail

# Retention sweep for repo-local .oms state. Only artifact-index prune reclaims
# anything today; capsules, task archives, handoff digests, hook
# telemetry/session state, local checkpoints, supervised runtime logs, frozen
# landing snapshots, orphaned delegation markers, and resolved failure rows otherwise grow unbounded over a
# repo's lifetime. This sweeps the SAFE, clearly-transient families by age and
# never touches live state (open runs, active attempts, the active task,
# unresolved failures, active claims).
# --dry-run by default, mirroring cleanup.sh.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
ROOT="$(cd "$ROOT" && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

REPO="$PWD"
DAYS=30
DRY_RUN=1

usage() {
  cat <<'EOF'
Usage: gc.sh [--repo PATH] [--days N] [--dry-run|--apply]

Reclaim aged, transient .oms state. Default is --dry-run (prints only).

Options:
  --repo PATH   Repo to sweep (default: PWD, git-root anchored).
  --days N      Age threshold in days (default: 30).
  --dry-run     Print what would be removed (default).
  --apply       Actually remove.
  -h, --help    Show help.

Swept (older than --days): orphaned delegation markers (dead pid; a coupled
claimed/running plan task is released back to ready), archived task packets,
handoff digests, local tracked-state checkpoints, hook events/sessions,
terminal supervisor runtime records outside the repository (durable lifecycle
events and attempt specs remain),
terminal frozen landing patches no longer referenced by the artifact index,
stale open runs (no spine event in --days; a close event is appended), run
capsules of runs that are NOT open, abandoned change-guards (dead owner pid
or aged snapshot), terminal/draft executor souls, retired failure rows
(resolved, or automatic hook rows past OMS_HOOK_FAIL_TTL — the same predicate
fail-ledger reads with), closed conversation threads;
artifact index/files are delegated
to artifact-index prune. Never touches live runs, the active task, unresolved
failures, active experiment claims, or plan tasks in review. The append-only
experiment board is left intact.
EOF
}

fail() { echo "error: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --days) [ "$#" -ge 2 ] || fail "--days requires an integer"; DAYS="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --apply) DRY_RUN=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
case "$DAYS" in *[!0-9]*|"") fail "--days must be a non-negative integer" ;; esac
[ "$DAYS" -le 36500 ] || fail "--days must be at most 36500"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

STATE_ROOT="$(oms_repo_root "$REPO")" || fail "bad --repo"
OMS="$STATE_ROOT/.oms"
[ -d "$OMS" ] || { echo "gc: no .oms state at $STATE_ROOT"; exit 0; }

mode="dry-run"; [ "$DRY_RUN" = 0 ] && mode="apply"
echo "gc: $STATE_ROOT (older than ${DAYS}d, $mode)"

removed=0
executor_gc_args=(gc --repo "$STATE_ROOT" --days "$DAYS" --dry-run)
[ "$DRY_RUN" = 1 ] || executor_gc_args=(gc --repo "$STATE_ROOT" --days "$DAYS" --apply)
executor_gc_out="$("$ROOT/scripts/agent-executor.sh" "${executor_gc_args[@]}")"
printf '%s\n' "$executor_gc_out"
executor_changes="$(printf '%s\n' "$executor_gc_out" | awk '/^executor-gc: [0-9]+ (candidate|removed)/ {n=$2} END {print n+0}')"
removed=$((removed + executor_changes))

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

# 0.5) A landing freezes caller-owned patch bytes before admission so verifier
# code cannot swap the approved object. Keep that snapshot while recovery is
# outstanding and while the artifact index still cites it; once both durable
# records say it is terminal/unreferenced, it is transient storage like an old
# supervisor log. The same non-blocking landing lock prevents GC from racing a
# live apply or recovery. A busy landing is expected and simply defers cleanup.
landing_patch_gc_locked() {
  python3 - "$STATE_ROOT" "$OMS/landings.jsonl" "$OMS/artifacts/index.jsonl" \
    "$DAYS" "$DRY_RUN" <<'PY'
import json
import os
import stat
import sys
import time

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
dry_run = dry_raw == "1"
removed = False
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
    print("- landing-patch: %s" % path)
    if not dry_run:
        os.unlink(path)
        removed = True
if removed:
    try:
        directory = os.open(patch_root, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError:
        pass
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
      landing_gc_changes="$(printf '%s\n' "$landing_gc_out" |
        awk '/^- landing-patch: / {n++} END {print n+0}')"
      removed=$((removed + landing_gc_changes))
      ;;
    75) echo "- landing-patch: skipped while a landing or recovery is active" ;;
    *) echo "error: gc: frozen landing patch maintenance failed" >&2; exit "$landing_gc_status" ;;
  esac
fi

# 1) Orphaned delegation markers: a dead pid means a crashed worker. A live
#    pid is an in-flight delegation and is never swept (regardless of age).
#    A marker carrying a plan task_id is the only record joining the dead
#    worker to its still-claimed plan task, so release the task in the same
#    sweep — otherwise the claim lingers until the reclaim TTL.
if [ -d "$OMS/delegations" ]; then
  for f in "$OMS/delegations"/*.json; do
    [ -e "$f" ] || continue
    info="$(python3 -c 'import json,sys
d = json.load(open(sys.argv[1]))
print("%s\t%s\t%s\t%s" % (d.get("pid", ""), d.get("task_id", ""), d.get("lease_id", ""), d.get("executor_id", "")))' "$f" 2>/dev/null || true)"
    pid="$(printf '%s' "$info" | cut -f1)"
    task_id="$(printf '%s' "$info" | cut -f2)"
    marker_lease="$(printf '%s' "$info" | cut -f3)"
    executor_id="$(printf '%s' "$info" | cut -f4)"
    alive=0
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then alive=1; fi
    if [ "$alive" = 0 ]; then
      note_remove "orphan-delegation" "$f"
      if [ -n "$executor_id" ]; then
        executor_state="$("$ROOT/scripts/agent-executor.sh" show --repo "$STATE_ROOT" --id "$executor_id" 2>/dev/null |
          python3 -c 'import json,sys;print(json.load(sys.stdin).get("state",""))' 2>/dev/null || true)"
        if [ "$executor_state" = "running" ]; then
          printf -- '- orphan-delegation-executor: %s running -> failed\n' "$executor_id"
          removed=$((removed + 1))
          if [ "$DRY_RUN" = 0 ]; then
            "$ROOT/scripts/agent-executor.sh" fail --repo "$STATE_ROOT" --id "$executor_id" \
              --reason "gc: delegation process is not alive" >/dev/null 2>&1 ||
              echo "warning: gc: could not fail executor $executor_id" >&2
          fi
        fi
      fi
      if [ -n "$task_id" ]; then
        task_info="$("$ROOT/scripts/agent-plan.sh" --repo "$STATE_ROOT" show --id "$task_id" 2>/dev/null |
          python3 -c 'import json,sys;d=json.load(sys.stdin);print("%s\t%s"%(d.get("state",""),d.get("lease_id","")))' 2>/dev/null || true)"
        task_state="$(printf '%s' "$task_info" | cut -f1)"
        task_lease="$(printf '%s' "$task_info" | cut -f2)"
        # Only claimed/running are dead-worker states; review holds a finished
        # artifact awaiting a reviewer and must not be requeued here.
        case "$task_state" in
          claimed|running)
            if [ "$marker_lease" != "$task_lease" ]; then
              printf -- '- orphan-delegation-plan: task %s lease changed; keep current claim\n' "$task_id"
              continue
            fi
            printf -- '- orphan-delegation-plan: task %s (%s) -> ready\n' "$task_id" "$task_state"
            removed=$((removed + 1))
            if [ "$DRY_RUN" = 0 ]; then
              release_args=(--repo "$STATE_ROOT" release --id "$task_id")
              [ -z "$marker_lease" ] || release_args+=(--lease-id "$marker_lease")
              OMS_HARNESS_CHILD=1 "$ROOT/scripts/agent-plan.sh" "${release_args[@]}" >/dev/null 2>&1 ||
                echo "warning: gc: could not release plan task $task_id" >&2
            fi
            ;;
        esac
      fi
    fi
  done
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

# 2.1) Local tracked-state checkpoints are recovery aids, not durable project
# history. Their metadata carries a portable UTC creation time; malformed or
# symlinked entries are never guessed at or deleted.
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

# 2.2) Hook state is deliberately transient. Compact only parseable old rows;
# invalid/unparseable rows are preserved for diagnosis. Apply uses a
# same-directory atomic replace under the shared file lock so a live hook can
# never observe a truncated file.
hook_events="$OMS/hooks/events.jsonl"
compact_hook_events() {
  local path="$1"
  local cutoff="$2"
  local apply="$3"
  local -a compact_args
  compact_args=(compact-events --path "$path" --cutoff "$cutoff")
  [ "$apply" = 0 ] || compact_args+=(--apply)
  python3 "$ROOT_LIB/hook_state.py" "${compact_args[@]}"
}
if [ -f "$hook_events" ] && [ ! -L "$hook_events" ]; then
  hook_counts="$(compact_hook_events "$hook_events" "$cutoff_epoch" "$((1 - DRY_RUN))")"
  hook_before="$(printf '%s' "$hook_counts" | cut -f1)"
  hook_after="$(printf '%s' "$hook_counts" | cut -f2)"
  case "$hook_before" in *[!0-9]*|"") echo "error: gc: could not compact hook events" >&2; exit 1 ;; esac
  case "$hook_after" in *[!0-9]*|"") echo "error: gc: could not compact hook events" >&2; exit 1 ;; esac
  if [ "$hook_after" -lt "$hook_before" ]; then
    printf -- '- hook-events: compact %s -> %s rows\n' "$hook_before" "$hook_after"
    removed=$((removed + hook_before - hook_after))
  fi
fi

if [ -d "$OMS/hooks/sessions" ] && [ ! -L "$OMS/hooks/sessions" ]; then
  for hook_session in "$OMS/hooks/sessions"/*; do
    if [ ! -f "$hook_session" ] || [ -L "$hook_session" ]; then
      continue
    fi
    hook_mtime="$(python3 -c 'import os,sys; print(int(os.path.getmtime(sys.argv[1])))' \
      "$hook_session" 2>/dev/null || true)"
    case "$hook_mtime" in *[!0-9]*|"") continue ;; esac
    [ "$hook_mtime" -lt "$cutoff_epoch" ] || continue
    note_remove "hook-session" "$hook_session"
  done
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

# 2.5) Stale open runs: nothing in the harness calls `oms run close`
#    automatically, so without this an abandoned run stays open forever and
#    permanently protects its capsule from step 3. A run whose LAST spine
#    event is older than --days is over; append a terminal close event.
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
        # timegm, not mktime: the stamps are UTC, and reading them as local
        # time shifted the cutoff by the host UTC offset.
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

# 2.6) Crashed-writer replace scratch: shared-state writers stage
#      .oms-replace.* beside their target so the swap is a same-directory
#      rename; a scratch an hour old means its writer died between mktemp and
#      mv. Minutes, not --days — the file has no value to anyone the moment
#      its writer is gone.
while IFS= read -r f; do
  [ -n "$f" ] || continue
  note_remove "replace-scratch" "$f"
done <<EOF
$(find "$OMS" -type f -name '.oms-replace.*' -mmin +60 2>/dev/null)
EOF

# 3) Run capsules older than --days whose run is NOT open (open = no close event
#    on the spine). Never GC a capsule for a run still in flight. In apply mode
#    step 2.5 has already closed stale runs, so their capsules reclaim here;
#    a dry run reports the close and the capsule sweep of the NEXT gc.
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

# 4) Retired failure rows older than --days: compact failures.jsonl, keeping
#    every fingerprint that still reads as open and any row newer than the
#    threshold. Retirement is fail-ledger's read-time view, not a second
#    opinion: a fingerprint is retired when it was resolved, or when every
#    failure still standing is an automatic hook row past its TTL. Without
#    that second arm the hook rows that read-time expiry retires would sit in
#    the file forever, and the two mechanisms would not compose.
fail_ledger="$OMS/failures.jsonl"
if [ -f "$fail_ledger" ]; then
  hook_fail_ttl="${OMS_HOOK_FAIL_TTL:-86400}"
  case "$hook_fail_ttl" in *[!0-9]*|"") hook_fail_ttl=86400 ;; esac
  # Snapshot, compaction, and publish run under the writers' own lock: the
  # old unlocked truncate-publish permanently lost any append that landed
  # between snapshot and publish, stripped the terminal newline through
  # command substitution (fusing the next locked append onto the last row),
  # and silently erased malformed lines — exactly the evidence a refusal
  # must preserve. Python stages beside the ledger with its newline intact
  # and the rename is the only publish; JSONL never rides a shell variable.
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

# A ledger with unreadable lines is evidence in an unknown state: publishing
# any compaction would erase those lines forever. Refuse without writing.
if malformed:
    raise SystemExit(3)

# Retirement predicate, textually identical in fail-ledger.sh (record repeat
# count, check, list) and in the gc failure compaction: read-time expiry and
# gc compose only while all four agree on which rows are retired.
def hook_expired(r):
    if r.get("kind") != "hook" or r.get("event") != "fail":
        return False
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False   # an unreadable stamp is never grounds for retirement
    return (now - t) >= ttl

# Replay the ledger the way `fail-ledger check` does; a fingerprint whose open
# count lands on zero is retired.
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
    # timegm, not mktime: the stamps are UTC, and reading them as local time
    # shifted the retention cutoff by the host's UTC offset.
    try:
        t = calendar.timegm(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False
    return t < cutoff

with open(sys.argv[2], "w", encoding="utf-8") as out:
    for r in rows:
        fp = r.get("fingerprint")
        # Drop only retired-fingerprint rows that are older than the cutoff.
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

# 4.2) Lifecycle events of long-terminal attempts. Delegated to agent-events,
#      which owns the stream's schema and lock: whole attempt streams only,
#      survivors must still project, and an invalid stream refuses to
#      compact. Without this the append-only ledger grows forever and the
#      projection every MCP reader consumes outgrows any output budget.
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

# 4.4) Usage rows: the content-free family counter the fail-ledger hook
#      appends on matched Bash calls. The reader (skill-router's usage hint)
#      drops rows past OMS_USAGE_TTL at read time — this compaction uses the
#      SAME predicate (unreadable day reads as expired too) and collapses
#      same-day rows into count rows the reader already sums, so the file
#      stops growing without the two mechanisms disagreeing.
usage_file="$OMS/usage.jsonl"
if [ -f "$usage_file" ] && [ ! -L "$usage_file" ]; then
  usage_ttl="${OMS_USAGE_TTL:-2592000}"
  case "$usage_ttl" in *[!0-9]*|"") usage_ttl=2592000 ;; esac
  # Same publish discipline as the failure ledger above: the hook appends
  # under the writers' lock, so compaction snapshots and publishes under
  # that lock too, staged beside the file with its terminal newline. Rows
  # that fail this file's own read predicate (unreadable family/day) are
  # dropped, matching the reader's documented contract.
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

# 4.5) Abandoned change-guards: a guard whose opt-in owner pid is dead, or
#    whose snapshot is older than --days, is a corpse from a crashed session —
#    without this it reads as "Change-guard: ACTIVE" in oms state forever.
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
    # Pre-liveness snapshot format: fall back to file age.
    [ -n "$(find "$guard_file" -mtime +"$DAYS" 2>/dev/null)" ] && guard_dead=1
  fi
  if [ "$guard_dead" = 1 ]; then
    note_remove "stale-change-guard" "$guard_file"
  fi
fi

# 4b) Closed conversation threads older than --days. An open thread is never
#     swept regardless of age: it is the only record of what the agents agreed,
#     and the current pointer expires on its own TTL.
if [ -d "$OMS/threads" ]; then
  cutoff=$(( $(date +%s) - DAYS * 86400 ))
  for f in "$OMS/threads"/*.jsonl; do
    [ -e "$f" ] || continue
    if ! grep -q '"role": *"closed"' "$f" 2>/dev/null; then
      continue
    fi
    mtime="$(python3 -c 'import os,sys; print(int(os.path.getmtime(sys.argv[1])))' "$f" 2>/dev/null || echo 0)"
    [ "$mtime" -lt "$cutoff" ] || continue
    note_remove "closed-thread" "$f"
  done
fi

# 5) Artifacts: use the same planner in dry-run and apply mode. Provider
# writers can spend minutes on a final-named file before indexing it, so GC
# applies a grace period before treating an unindexed file as orphaned.
if [ -f "$OMS/artifacts/index.jsonl" ]; then
  artifact_keep="${OMS_ARTIFACT_INDEX_KEEP:-1000}"
  artifact_grace="${OMS_ARTIFACT_ORPHAN_GRACE:-86400}"
  artifact_args=(--repo "$STATE_ROOT" prune "$artifact_keep" --files)
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
