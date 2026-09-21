#!/usr/bin/env bash
set -euo pipefail

# Compress a training/Slurm log (or a Slurm job id) into a short markdown
# digest sized for agent context: error patterns, last traceback, tail.
# Stateless: prints to stdout, no daemon, no state.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "$ROOT/scripts/lib/poll.sh" ]; then
  # shellcheck source=scripts/lib/poll.sh
  . "$ROOT/scripts/lib/poll.sh"
else
  oms_poll_int_or_default() {
    case "$1" in *[!0-9]*|"") printf '%s\n' "$2" ;; *) printf '%s\n' "$1" ;; esac
  }
  oms_poll_log_next() { return 0; }
fi

TAIL_LINES=30
PATTERN_LINES=40
PATTERNS='Traceback|ERROR|Error:|error:|OOM|[Oo]ut of memory|CUDA|NCCL|NaN|nan loss|Killed|Segmentation fault|exitcode|srun: error'
WAIT=0
WAIT_TIMEOUT=""
MAX_BYTES=""
PENDING=0
POLL_SECONDS="$(oms_poll_int_or_default "${OMS_JOB_DIGEST_POLL:-30}" 30)"
[ "$POLL_SECONDS" -ge 1 ] || POLL_SECONDS=1

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
  --wait-timeout S
                With --wait: stop observing after S seconds, digest what
                exists, mark the job pending and exit 124. The job itself is
                never cancelled or resubmitted. Default: wait without limit.
  --max-bytes N Cap the digest at N bytes (>= 512) of whole lines and say what
                was omitted. Default: no cap.
  -h, --help    Show this help.

Leaving the queue is not success: confirm State/ExitCode and required outputs.
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
      case "$2" in *[!0-9]*|"") fail "--tail requires a positive integer" ;; esac
      TAIL_LINES="$2"
      shift 2
      ;;
    --patterns)
      [ "$#" -ge 2 ] || fail "--patterns requires count"
      case "$2" in *[!0-9]*|"") fail "--patterns requires a positive integer" ;; esac
      PATTERN_LINES="$2"
      shift 2
      ;;
    --wait)
      WAIT=1
      shift
      ;;
    --wait-timeout)
      [ "$#" -ge 2 ] || fail "--wait-timeout requires seconds"
      case "$2" in *[!0-9]*|"") fail "--wait-timeout requires a positive integer" ;; esac
      WAIT_TIMEOUT="$((10#$2))"
      [ "$WAIT_TIMEOUT" -ge 1 ] || fail "--wait-timeout requires a positive integer"
      shift 2
      ;;
    --max-bytes)
      [ "$#" -ge 2 ] || fail "--max-bytes requires count"
      case "$2" in *[!0-9]*|"") fail "--max-bytes requires a positive integer" ;; esac
      MAX_BYTES="$((10#$2))"
      [ "$MAX_BYTES" -ge 512 ] || fail "--max-bytes must be at least 512"
      shift 2
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
[ "${#ARGS[@]}" -le 2 ] || fail "too many arguments"

JOB_ID=""
LOG_FILE=""
if [ -f "${ARGS[0]}" ]; then
  [ "${#ARGS[@]}" -eq 1 ] || fail "too many arguments"
  LOG_FILE="${ARGS[0]}"
elif printf '%s' "${ARGS[0]}" | grep -Eq '^[0-9]+(_[0-9]+)?$'; then
  JOB_ID="${ARGS[0]}"
  LOG_FILE="${ARGS[1]:-}"
  [ -z "$LOG_FILE" ] || [ -f "$LOG_FILE" ] || fail "log file not found: $LOG_FILE"
else
  fail "not a log file or job id: ${ARGS[0]}"
fi

[ -z "$WAIT_TIMEOUT" ] || [ "$WAIT" = "1" ] || fail "--wait-timeout requires --wait"

now_s() { date +%s; }

if [ "$WAIT" = "1" ]; then
  [ -n "$JOB_ID" ] || fail "--wait requires a slurm job id"
  command -v squeue >/dev/null 2>&1 || fail "--wait needs squeue (Slurm)"
  echo "job-digest: waiting for job $JOB_ID to leave the queue (poll ${POLL_SECONDS}s${WAIT_TIMEOUT:+, budget ${WAIT_TIMEOUT}s})" >&2
  # A hung scheduler query would outlive the budget, so bound it where GNU
  # timeout exists. BSD/macOS ship none: there only the sleeps are bounded.
  query_guard=""
  if [ -n "$WAIT_TIMEOUT" ]; then
    if command -v timeout >/dev/null 2>&1 && timeout --version >/dev/null 2>&1; then
      query_guard=timeout
    elif command -v gtimeout >/dev/null 2>&1 && gtimeout --version >/dev/null 2>&1; then
      query_guard=gtimeout
    fi
  fi
  remaining=""
  q_failures=0
  q_last_failure=""
  wait_start="$(now_s)"
  # One captured query per poll. Done when the queue no longer knows the job:
  # either rc=0 with empty output (recent Slurm) or rc!=0 with an
  # invalid/unknown-job-id error (job purged from the queue). Any other rc!=0
  # is a transient controller failure -> keep waiting, never fake completion.
  q_err="$(mktemp)" || fail "mktemp failed"
  cleanup_done=0
  cleanup() {
    [ "$cleanup_done" = 0 ] || return 0
    cleanup_done=1
    rm -f "$q_err"
  }
  cleanup_signal() {
    local code="$1"
    trap - EXIT HUP INT TERM
    cleanup
    exit "$code"
  }
  trap cleanup EXIT
  trap 'cleanup_signal 129' HUP
  trap 'cleanup_signal 130' INT
  trap 'cleanup_signal 143' TERM
  while :; do
    set +e
    if [ -n "$query_guard" ]; then
      q_budget=$((WAIT_TIMEOUT - ($(now_s) - wait_start)))
      [ "$q_budget" -ge 1 ] || q_budget=1
      q_out="$("$query_guard" "$q_budget" squeue -h -j "$JOB_ID" 2>"$q_err")"
    else
      q_out="$(squeue -h -j "$JOB_ID" 2>"$q_err")"
    fi
    q_rc=$?
    set -e
    if [ "$q_rc" -eq 0 ]; then
      [ -z "$q_out" ] && break
    elif grep -qiE 'invalid job id|unknown job( id)?' "$q_err"; then
      break
    else
      # An hour of controller outage is one fact, not 120 identical lines of
      # agent context: report a failure kind once, then only the total.
      q_failures=$((q_failures + 1))
      q_failure="$q_rc:$(head -n 1 "$q_err" | LC_ALL=C tr -cd '\11\40-\176' | cut -c1-200)"
      if [ "$q_failure" != "$q_last_failure" ]; then
        echo "job-digest: squeue query failed transiently (rc=${q_failure%%:*}: ${q_failure#*:}); retrying every ${POLL_SECONDS}s" >&2
        q_last_failure="$q_failure"
      fi
    fi
    wait_now="$(now_s)"
    sleep_for="$POLL_SECONDS"
    if [ -n "$WAIT_TIMEOUT" ]; then
      remaining=$((WAIT_TIMEOUT - (wait_now - wait_start)))
      if [ "$remaining" -le 0 ]; then
        PENDING=1
        break
      fi
      [ "$sleep_for" -le "$remaining" ] || sleep_for="$remaining"
    fi
    oms_poll_log_next job-digest "$((wait_now - wait_start))" "$sleep_for" "$remaining"
    sleep "$sleep_for"
  done
  [ "$q_failures" -le 1 ] ||
    echo "job-digest: squeue query failed $q_failures times while waiting" >&2
  if [ "$PENDING" = "1" ]; then
    echo "job-digest: wait budget ${WAIT_TIMEOUT}s spent; job $JOB_ID still queued and untouched (exit 124, re-run to keep observing)" >&2
  else
    echo "job-digest: job $JOB_ID no longer queued; digesting" >&2
  fi
fi

emit_digest() {
  printf '# Job digest\n\n'
  printf -- '- generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  [ -n "$JOB_ID" ] && printf -- '- slurm job: %s\n' "$JOB_ID"
  [ "$PENDING" = "1" ] &&
    printf -- '- wait: pending, still queued after %ss; this is not completion\n' "$WAIT_TIMEOUT"
  [ -n "$LOG_FILE" ] && printf -- '- log: %s (%s lines)\n' "$LOG_FILE" "$(wc -l < "$LOG_FILE")"
  if git rev-parse --git-dir >/dev/null 2>&1; then
    printf -- '- git: %s, %s dirty files\n' \
      "$(git rev-parse --short HEAD 2>/dev/null || echo 'no commit')" \
      "$(git status --porcelain --untracked-files=no | wc -l)"
  fi

  if [ -n "$JOB_ID" ]; then
    printf '\n## Slurm accounting\n\n'
    if command -v sacct >/dev/null 2>&1; then
      # Captured, not piped bare: under pipefail a failing sacct (accounting
      # storage down) used to abort the digest before any log section.
      acct_rc=0
      acct="$(sacct -j "$JOB_ID" --format=JobID,JobName%20,State,ExitCode,Elapsed,MaxRSS,ReqMem,AllocTRES%40 2>&1 |
        head -20)" || acct_rc=$?
      printf '```\n'
      printf '%s\n' "$acct"
      printf '```\n'
      # Header and rule only means no record yet; that is unknown, not success.
      if [ "$acct_rc" -ne 0 ] || [ "$(printf '%s\n' "$acct" | wc -l)" -le 2 ]; then
        printf 'Accounting unavailable or not recorded yet: outcome unknown, not verified.\n'
      fi
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
      # Bounded range instead of sed|head: a log that keeps growing past the
      # traceback (retries, caught exceptions) left sed writing into a closed
      # pipe once head exited — SIGPIPE 141 under pipefail killed the digest
      # exactly on the logs big enough to need one.
      sed -n "${last_tb},$((last_tb + 39))p" "$LOG_FILE"
      printf '```\n'
    else
      printf 'No Python traceback found.\n'
    fi

    printf '\n## Tail\n\n```\n'
    tail -n "$TAIL_LINES" "$LOG_FILE"
    printf '```\n'
  fi
}

# Whole lines only: a byte cut can split a UTF-8 character, and one
# multi-megabyte log line must not spend the budget. Reads to EOF so the
# producer never writes into a closed pipe under pipefail.
clamp_bytes() {
  LC_ALL=C awk -v max="$1" '
    BEGIN { budget = max - 160 }
    {
      n = length($0) + 1
      if (used + n <= budget) { used += n; print; next }
      lines++; bytes += n
    }
    END {
      if (lines)
        printf "\n[job-digest: %d line(s), %.0f bytes omitted by --max-bytes %d; read the log for more]\n", lines, bytes, max
    }'
}

if [ -n "$MAX_BYTES" ]; then
  emit_digest | clamp_bytes "$MAX_BYTES"
else
  emit_digest
fi
[ "$PENDING" = "0" ] || exit 124
