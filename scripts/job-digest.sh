#!/usr/bin/env bash
set -euo pipefail

# Compress a training/Slurm log (or a Slurm job id) into a short markdown
# digest sized for agent context: error patterns, last traceback, tail.
# Stateless: prints to stdout, no daemon, no state.

TAIL_LINES=30
PATTERN_LINES=40
PATTERNS='Traceback|ERROR|Error:|error:|OOM|[Oo]ut of memory|CUDA|NCCL|NaN|nan loss|Killed|Segmentation fault|exitcode|srun: error'
WAIT=0
POLL_SECONDS="${OMS_JOB_DIGEST_POLL:-30}"

usage() {
  cat <<'EOF'
Usage: job-digest.sh <logfile>
       job-digest.sh <slurm-job-id> [logfile]

Emit a compact markdown digest of a training/Slurm run: sacct summary (job id
mode), error-pattern hits, the last Python traceback, and the log tail.

Options:
  --tail N      Lines of raw tail to include. Default: 30.
  --patterns N  Max error-pattern lines to include. Default: 40.
  --wait        Job-id mode only: block until the job leaves the queue
                (squeue), then digest. Poll interval OMS_JOB_DIGEST_POLL=30s.
  -h, --help    Show this help.

The command does not run training; pair it with the run ledger or sbatch.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

ARGS=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --tail)
      [ "$#" -ge 2 ] || fail "--tail requires count"
      TAIL_LINES="$2"
      shift 2
      ;;
    --patterns)
      [ "$#" -ge 2 ] || fail "--patterns requires count"
      PATTERN_LINES="$2"
      shift 2
      ;;
    --wait)
      WAIT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      ARGS+=("$1")
      shift
      ;;
  esac
done

[ "${#ARGS[@]}" -ge 1 ] || {
  usage >&2
  exit 2
}

JOB_ID=""
LOG_FILE=""
if [ -f "${ARGS[0]}" ]; then
  LOG_FILE="${ARGS[0]}"
elif printf '%s' "${ARGS[0]}" | grep -Eq '^[0-9]+(_[0-9]+)?$'; then
  JOB_ID="${ARGS[0]}"
  LOG_FILE="${ARGS[1]:-}"
  [ -z "$LOG_FILE" ] || [ -f "$LOG_FILE" ] || fail "log file not found: $LOG_FILE"
else
  fail "not a log file or job id: ${ARGS[0]}"
fi

if [ "$WAIT" = "1" ]; then
  [ -n "$JOB_ID" ] || fail "--wait requires a slurm job id"
  command -v squeue >/dev/null 2>&1 || fail "--wait needs squeue (Slurm)"
  echo "job-digest: waiting for job $JOB_ID to leave the queue (poll ${POLL_SECONDS}s)" >&2
  while squeue -h -j "$JOB_ID" >/dev/null 2>&1 && [ -n "$(squeue -h -j "$JOB_ID" 2>/dev/null)" ]; do
    sleep "$POLL_SECONDS"
  done
  echo "job-digest: job $JOB_ID no longer queued; digesting" >&2
fi

printf '# Job digest\n\n'
printf -- '- generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
[ -n "$JOB_ID" ] && printf -- '- slurm job: %s\n' "$JOB_ID"
[ -n "$LOG_FILE" ] && printf -- '- log: %s (%s lines)\n' "$LOG_FILE" "$(wc -l < "$LOG_FILE")"
if git rev-parse --git-dir >/dev/null 2>&1; then
  printf -- '- git: %s, %s dirty files\n' \
    "$(git rev-parse --short HEAD 2>/dev/null || echo 'no commit')" \
    "$(git status --porcelain --untracked-files=no | wc -l)"
fi

if [ -n "$JOB_ID" ]; then
  printf '\n## Slurm accounting\n\n'
  if command -v sacct >/dev/null 2>&1; then
    printf '```\n'
    sacct -j "$JOB_ID" --format=JobID,JobName%20,State,ExitCode,Elapsed,MaxRSS,ReqMem,AllocTRES%40 2>&1 |
      head -20
    printf '```\n'
  else
    printf 'sacct not available on this machine.\n'
  fi
fi

if [ -n "$LOG_FILE" ]; then
  printf '\n## Error patterns\n\n'
  if grep -nE "$PATTERNS" "$LOG_FILE" >/dev/null 2>&1; then
    printf '```\n'
    grep -nE "$PATTERNS" "$LOG_FILE" | tail -n "$PATTERN_LINES"
    printf '```\n'
  else
    printf 'No error patterns matched.\n'
  fi

  printf '\n## Last traceback\n\n'
  last_tb="$(grep -n 'Traceback (most recent call last)' "$LOG_FILE" | tail -n 1 | cut -d: -f1 || true)"
  if [ -n "$last_tb" ]; then
    printf '```\n'
    sed -n "${last_tb},\$p" "$LOG_FILE" | head -40
    printf '```\n'
  else
    printf 'No Python traceback found.\n'
  fi

  printf '\n## Tail\n\n```\n'
  tail -n "$TAIL_LINES" "$LOG_FILE"
  printf '```\n'
fi
