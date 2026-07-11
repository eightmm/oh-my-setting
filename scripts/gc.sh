#!/usr/bin/env bash
set -euo pipefail

# Retention sweep for repo-local .oms state. Only artifact-index prune reclaims
# anything today; capsules, task archives, orphaned delegation markers, and
# resolved failure rows otherwise grow unbounded over a repo's lifetime. This
# sweeps the SAFE, clearly-transient families by age and never touches live
# state (open runs, the active task, unresolved failures, active claims).
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
stale open runs (no spine event in --days; a close event is appended), run
capsules of runs that are NOT open, abandoned change-guards (dead owner pid
or aged snapshot), terminal/draft executor souls, resolved failure rows;
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

note_remove() {  # note_remove KIND PATH
  printf -- '- %s: %s\n' "$1" "$2"
  removed=$((removed + 1))
  if [ "$DRY_RUN" = 0 ]; then
    rm -rf "$2"
  fi
}

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

# 2.5) Stale open runs: nothing in the harness calls `oms-run close`
#    automatically, so without this an abandoned run stays open forever and
#    permanently protects its capsule from step 3. A run whose LAST spine
#    event is older than --days is over; append a terminal close event.
if [ -f "$OMS/runs/spine.jsonl" ]; then
  stale_open="$(OMS_DAYS="$DAYS" python3 - "$OMS/runs/spine.jsonl" <<'PY'
import json, os, sys, time
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
        ts = time.mktime(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
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
        "$ROOT/scripts/oms-run.sh" close --run-id "$rid" --note "gc: no event in ${DAYS}d" >/dev/null 2>&1 ||
        echo "warning: gc: could not close run $rid" >&2
    fi
  done
fi

# 3) Run capsules older than --days whose run is NOT open (open = no close event
#    on the spine). Never GC a capsule for a run still in flight. In apply mode
#    step 2.5 has already closed stale runs, so their capsules reclaim here;
#    a dry run reports the close and the capsule sweep of the NEXT gc.
open_ids=""
if [ -f "$OMS/runs/spine.jsonl" ]; then
  open_ids="$(OMS_RUN_INDEX="$OMS/runs/spine.jsonl" "$ROOT/scripts/oms-run.sh" ls --open 2>/dev/null | awk '{print $1}')"
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

# 4) Resolved failure rows older than --days: compact failures.jsonl, keeping
#    every unresolved fingerprint and any row newer than the threshold.
fail_ledger="$OMS/failures.jsonl"
if [ -f "$fail_ledger" ]; then
  before="$(wc -l < "$fail_ledger" | tr -d ' ')"
  compacted="$(OMS_DAYS="$DAYS" python3 - "$fail_ledger" <<'PY'
import json, os, sys, time
days = int(os.environ["OMS_DAYS"])
cutoff = time.time() - days * 86400
rows = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    line = line.strip()
    if line:
        try:
            rows.append(json.loads(line))
        except Exception:
            rows.append(None)
# Resolved fingerprints (final state resolved).
state = {}
for r in rows:
    if not isinstance(r, dict):
        continue
    fp = r.get("fingerprint")
    if not fp:
        continue
    ev = r.get("event")
    if ev == "resolved":
        state[fp] = True
    elif ev == "fail":
        state[fp] = False

def old(r):
    try:
        t = time.mktime(time.strptime(r.get("ts", ""), "%Y-%m-%dT%H:%M:%SZ"))
    except Exception:
        return False
    return t < cutoff

kept = []
for r in rows:
    if not isinstance(r, dict):
        kept.append(r)
        continue
    fp = r.get("fingerprint")
    # Drop only resolved-fingerprint rows that are older than the cutoff.
    if fp and state.get(fp) and old(r):
        continue
    kept.append(r)
sys.stdout.write("\n".join(json.dumps(r, ensure_ascii=False) for r in kept if isinstance(r, dict)))
if kept:
    sys.stdout.write("\n")
PY
)"
  after="$(printf '%s' "$compacted" | grep -c '' 2>/dev/null || echo 0)"
  if [ "$after" -lt "$before" ]; then
    printf -- '- failures: compact %s -> %s rows\n' "$before" "$after"
    removed=$((removed + 1))
    if [ "$DRY_RUN" = 0 ]; then
      printf '%s' "$compacted" > "$fail_ledger"
    fi
  fi
fi

# 4.5) Abandoned change-guards: a guard whose opt-in owner pid is dead, or
#    whose snapshot is older than --days, is a corpse from a crashed session —
#    without this it reads as "Change-guard: ACTIVE" in repo-state forever.
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
