#!/usr/bin/env bash
set -euo pipefail

# Run a command and append one JSONL row to the project's experiment ledger:
# timestamp, command, git SHA, dirty state, Slurm job id, exit code, duration.
# The ledger is git-tracked agent memory: read it before proposing experiments.

ROOT_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")/lib" && pwd)"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

LEDGER=""
NOTE=""
METRICS_FILE=""
GATE="${OMS_RUN_LEDGER_GATE:-1}"

usage() {
  cat <<'EOF'
Usage: run-ledger.sh [options] -- <command...>
       run-ledger.sh list [N]

Run a command and append a row to the experiment ledger
(default: docs/EXPERIMENTS.jsonl). Exit code mirrors the command.

Before launching, when an executable scripts/check.sh exists, the project
verification contract runs as a pre-flight gate (ml-smoke when implemented,
else fast) and a failing gate aborts the launch. Identical earlier runs
(same commit, same diff hash, same command) produce a warning.

Options:
  --note TEXT     Free-text note recorded with the run.
  --file PATH     Ledger path. Default: docs/EXPERIMENTS.jsonl.
  --metrics PATH  After the run, fold scalar fields from this JSON file into
                  the row's "metrics" so eval results are part of the record.
  --no-gate       Skip the pre-flight scripts/check.sh gate.
  -h, --help      Show this help.

list [N]        Show the last N ledger rows (default 10).

Environment:
  OMS_RUN_LEDGER_GATE=0   Same as --no-gate.
  OMS_RUN_LEDGER_DUP=0    Disable the duplicate-run warning.

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
    m = r.get("metrics") or {}
    def _clean(x):
        s = str(x).replace("\n", " ").replace("\r", " ").replace("\t", " ")
        return s[:60]
    metrics = ("  [" + " ".join("%s=%s" % (_clean(k), _clean(v)) for k, v in m.items()) + "]") if m else ""
    print("%s  exit=%d  %ds  sha=%s%s  %s%s%s" % (
        r["ts"], r["exit"], r["duration_s"], r["git_sha"], dirty,
        " ".join(r["cmd"]), metrics, note))
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
    --metrics)
      [ "$#" -ge 2 ] || fail "--metrics requires path"
      METRICS_FILE="$2"
      shift 2
      ;;
    --no-gate)
      GATE=0
      shift
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
    # HEAD-relative so staged-only changes get distinct hashes too; a repo
    # with no commits yet has no HEAD, so hash index + worktree instead.
    if git rev-parse --verify HEAD >/dev/null 2>&1; then
      dirty_hash="$(git diff HEAD | sha256sum | cut -c1-16)"
    else
      dirty_hash="$( (git diff --cached; git diff) | sha256sum | cut -c1-16)"
    fi
  fi
fi

# Duplicate-run warning: the ledger is agent memory, so surface "this exact
# experiment already ran" before spending compute on it again.
if [ "${OMS_RUN_LEDGER_DUP:-1}" = "1" ] && [ -s "$LEDGER" ]; then
  dup="$(python3 - "$LEDGER" "$git_sha" "$dirty_hash" "$@" <<'EOF'
import json, sys
ledger, sha, dhash = sys.argv[1], sys.argv[2], sys.argv[3]
cmd = list(sys.argv[4:])
last = None
with open(ledger) as f:
    for line in f:
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("cmd") == cmd and r.get("git_sha") == sha and (r.get("dirty_hash") or "") == dhash:
            last = r
if last:
    note = " note=%s" % last["note"] if last.get("note") else ""
    print("%s exit=%s %ss%s" % (last.get("ts"), last.get("exit"), last.get("duration_s"), note))
EOF
)" || dup=""
  if [ -n "$dup" ]; then
    echo "warning: identical run already in ledger (same commit, diff, command): $dup" >&2
  fi
fi

# Pre-flight gate: never burn a run on a project whose own verification
# contract fails. Fails loudly on unfilled contracts by design.
if [ "$GATE" = "1" ] && [ -x scripts/check.sh ]; then
  gate_mode="fast"
  # Mode is implemented only when a case label exists; a comment or usage
  # mention must not select it. Labels may be quoted, parenthesized, or in
  # an alternation: ml-smoke), (ml-smoke), "ml-smoke"), fast|ml-smoke).
  if grep -Eq '(^|[[:space:]("|'\''])ml-smoke("|'\'')?\)' scripts/check.sh; then
    gate_mode="ml-smoke"
  fi
  echo "ledger: pre-flight gate: bash scripts/check.sh $gate_mode (skip with --no-gate)" >&2
  if ! bash scripts/check.sh "$gate_mode" >&2; then
    echo "error: pre-flight check failed; launch aborted (--no-gate to override)" >&2
    exit 3
  fi
fi

# The note and command line are written verbatim to the git-tracked ledger.
# Warn (do not block — absolute paths are common and legitimate locally) when
# they look sensitive, so secrets are not committed unnoticed.
ledger_scan="$(mktemp)" || fail "mktemp failed"
ms_tmp=""
trap 'rm -f "$ledger_scan" ${ms_tmp:+"$ms_tmp"}' EXIT
printf '%s\n%s\n' "$NOTE" "$*" > "$ledger_scan"
if agent_memory_file_has_sensitive_content "$ledger_scan"; then
  echo "warning: ledger note/command looks sensitive; it is recorded in git-tracked $LEDGER" >&2
fi

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
start_s="$(date +%s)"

set +e
"$@"
status=$?
set -e

duration_s=$(( $(date +%s) - start_s ))

# Sanitize metrics BEFORE building the row: finite scalars only, capped
# count/length, control chars stripped, RFC-valid JSON. The sanitized string
# is then secret-scrubbed (the ledger is git-tracked) and only attached if clean.
METRICS_JSON=""
if [ -n "$METRICS_FILE" ]; then
  metrics_max_bytes="${OMS_METRICS_MAX_BYTES:-65536}"
  if [ ! -f "$METRICS_FILE" ]; then
    echo "ledger: metrics file not found; row recorded without metrics" >&2
  elif [ "$(wc -c < "$METRICS_FILE" | tr -d ' ')" -gt "$metrics_max_bytes" ]; then
    echo "ledger: metrics file exceeds ${metrics_max_bytes}B; row recorded without metrics" >&2
  else
    METRICS_JSON="$(OMS_METRICS_FILE="$METRICS_FILE" python3 <<'EOF'
import json, math, os, sys
mf = os.environ["OMS_METRICS_FILE"]
try:
    with open(mf) as f:
        data = json.load(f)
except Exception as e:
    sys.stderr.write("ledger: metrics file unreadable (%s); row recorded without metrics\n" % e)
    sys.exit(0)
if not isinstance(data, dict):
    sys.stderr.write("ledger: metrics file is not a JSON object; ignored\n")
    sys.exit(0)
MAX_KEYS = int(os.environ.get("OMS_METRICS_MAX_KEYS", "50"))
MAX_STR = int(os.environ.get("OMS_METRICS_MAX_STR", "200"))
out = {}
for k, v in data.items():
    if len(out) >= MAX_KEYS:
        break
    if isinstance(v, bool):
        out[str(k)[:MAX_STR]] = v
    elif isinstance(v, int):
        out[str(k)[:MAX_STR]] = v
    elif isinstance(v, float):
        if math.isfinite(v):            # drop NaN/Inf -> RFC-valid JSON
            out[str(k)[:MAX_STR]] = v
    elif isinstance(v, str):
        s = "".join(c for c in v if c >= " " and c != "\x7f")  # strip control/newline
        out[str(k)[:MAX_STR]] = s[:MAX_STR]
    # non-scalars dropped
if out:
    sys.stdout.write(json.dumps(out, ensure_ascii=False, allow_nan=False))
EOF
)" || METRICS_JSON=""
    if [ -n "$METRICS_JSON" ]; then
      ms_tmp="$(mktemp)" || fail "mktemp failed"
      printf '%s' "$METRICS_JSON" > "$ms_tmp"
      if agent_memory_file_has_sensitive_content "$ms_tmp"; then
        echo "ledger: metrics omitted because they contain sensitive-looking content" >&2
        METRICS_JSON=""
      fi
      rm -f "$ms_tmp"
      ms_tmp=""
    fi
  fi
fi

OMS_METRICS_JSON="$METRICS_JSON" python3 - "$ts" "$git_sha" "$dirty" "$dirty_hash" "${SLURM_JOB_ID:-}" \
  "$status" "$duration_s" "$NOTE" "$@" <<'EOF' >> "$LEDGER"
import json, os, sys
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
mj = os.environ.get("OMS_METRICS_JSON", "")
if mj:
    row["metrics"] = json.loads(mj)
print(json.dumps(row, ensure_ascii=False, allow_nan=False))
EOF

echo "ledger: appended to $LEDGER (exit $status, ${duration_s}s)" >&2
exit "$status"
