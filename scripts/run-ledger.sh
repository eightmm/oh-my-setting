#!/usr/bin/env bash
set -euo pipefail

# Run a command and append one JSONL row to the project's experiment ledger:
# timestamp, command, git SHA, dirty state, Slurm job id, exit code, duration.
# The ledger is git-tracked agent memory: read it before proposing experiments.

LEDGER=""
NOTE=""

usage() {
  cat <<'EOF'
Usage: run-ledger.sh [options] -- <command...>
       run-ledger.sh list [N]

Run a command and append a row to the experiment ledger
(default: docs/EXPERIMENTS.jsonl). Exit code mirrors the command.

Options:
  --note TEXT   Free-text note recorded with the run.
  --file PATH   Ledger path. Default: docs/EXPERIMENTS.jsonl.
  -h, --help    Show this help.

list [N]        Show the last N ledger rows (default 10).

The command line is recorded verbatim -- do not put secrets in arguments.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required for ledger rows"

MODE="run"
if [ "${1:-}" = "list" ]; then
  MODE="list"
  shift
fi

if [ "$MODE" = "list" ]; then
  N="${1:-10}"
  LEDGER="${LEDGER:-docs/EXPERIMENTS.jsonl}"
  [ -f "$LEDGER" ] || fail "no ledger at $LEDGER"
  tail -n "$N" "$LEDGER" |
    python3 -c '
import json, sys
for line in sys.stdin:
    r = json.loads(line)
    dirty = "+dirty" if r["dirty"] else ""
    note = ("  # " + r["note"]) if r.get("note") else ""
    print("%s  exit=%d  %ds  sha=%s%s  %s%s" % (
        r["ts"], r["exit"], r["duration_s"], r["git_sha"], dirty,
        " ".join(r["cmd"]), note))
'
  exit 0
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    --note)
      [ "$#" -ge 2 ] || fail "--note requires text"
      NOTE="$2"
      shift 2
      ;;
    --file)
      [ "$#" -ge 2 ] || fail "--file requires path"
      LEDGER="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      break
      ;;
    *)
      fail "unknown argument before --: $1"
      ;;
  esac
done

[ "$#" -gt 0 ] || {
  usage >&2
  exit 2
}

LEDGER="${LEDGER:-docs/EXPERIMENTS.jsonl}"
mkdir -p "$(dirname "$LEDGER")"

git_sha="none"
dirty=0
dirty_hash=""
if git rev-parse --git-dir >/dev/null 2>&1; then
  git_sha="$(git rev-parse --short HEAD 2>/dev/null || echo 'no-commit')"
  dirty="$(git status --porcelain --untracked-files=no | wc -l | tr -d ' ')"
  if [ "$dirty" -gt 0 ]; then
    dirty_hash="$(git diff | sha256sum | cut -c1-16)"
  fi
fi

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_s="$(date +%s)"

set +e
"$@"
status=$?
set -e

duration_s=$(( $(date +%s) - start_s ))

python3 - "$ts" "$git_sha" "$dirty" "$dirty_hash" "${SLURM_JOB_ID:-}" \
  "$status" "$duration_s" "$NOTE" "$@" <<'EOF' >> "$LEDGER"
import json, sys
a = sys.argv[1:]
row = {
    "ts": a[0],
    "git_sha": a[1],
    "dirty": int(a[2]),
    "dirty_hash": a[3],
    "slurm_job_id": a[4],
    "exit": int(a[5]),
    "duration_s": int(a[6]),
    "note": a[7],
    "cmd": a[8:],
}
print(json.dumps(row, ensure_ascii=False))
EOF

echo "ledger: appended to $LEDGER (exit $status, ${duration_s}s)" >&2
exit "$status"
