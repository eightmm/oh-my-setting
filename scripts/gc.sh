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

Swept (older than --days): orphaned delegation markers (dead pid), archived
task packets, run capsules of runs that are NOT open, resolved failure rows;
artifact index/files are delegated to artifact-index prune. Never touches open
runs, the active task, unresolved failures, or active experiment claims. The
append-only experiment board is left intact.
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
note_remove() {  # note_remove KIND PATH
  printf -- '- %s: %s\n' "$1" "$2"
  removed=$((removed + 1))
  if [ "$DRY_RUN" = 0 ]; then
    rm -rf "$2"
  fi
}

# 1) Orphaned delegation markers: a dead pid means a crashed worker. A live
#    pid is an in-flight delegation and is never swept (regardless of age).
if [ -d "$OMS/delegations" ]; then
  for f in "$OMS/delegations"/*.json; do
    [ -e "$f" ] || continue
    pid="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("pid",""))' "$f" 2>/dev/null || true)"
    alive=0
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then alive=1; fi
    if [ "$alive" = 0 ]; then
      note_remove "orphan-delegation" "$f"
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

# 3) Run capsules older than --days whose run is NOT open (open = no close event
#    on the spine). Never GC a capsule for a run still in flight.
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

# 5) Artifacts: delegate to the dedicated prune (keeps recent N rows + files).
if [ -f "$OMS/artifacts/index.jsonl" ] && [ "$DRY_RUN" = 0 ]; then
  ( cd "$STATE_ROOT" && "$ROOT/scripts/artifact-index.sh" prune 1000 --files >/dev/null 2>&1 ) || true
  echo "- artifacts: pruned via artifact-index (keep 1000)"
fi

if [ "$removed" -eq 0 ]; then
  echo "gc: nothing to reclaim"
elif [ "$DRY_RUN" = 1 ]; then
  echo "gc: $removed item(s) would be reclaimed"
  echo "gc: re-run with --apply to remove"
else
  echo "gc: $removed item(s) reclaimed"
fi
