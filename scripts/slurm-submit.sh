#!/usr/bin/env bash
set -euo pipefail

# Submit a Slurm job once per request key. An agent that retries, resumes
# after a crash or runs twice would otherwise submit the same job twice. The
# key is recorded BEFORE sbatch runs, so a repeat is detected and reported.
# Duplicate detection inside one repository's .oms, not exactly-once across
# machines or clones. OMS never submits on its own: this wraps the submission
# a person or agent explicitly asked for.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"

STATE_ROOT="$(oms_repo_root "$PWD")"
SUBMITS_FILE="${OMS_SLURM_SUBMITS_FILE:-$STATE_ROOT/.oms/slurm-submits.jsonl}"
HELPER="$ROOT_LIB/slurm_submit.py"

usage() {
  cat <<'EOF'
Usage: slurm-submit.sh [--key KEY] [--dry-run] [--force-resubmit --reason TEXT] -- sbatch ARGS... SCRIPT

Run `sbatch ARGS... SCRIPT` once per request key and record it in
.oms/slurm-submits.jsonl (append-only, under the repo file lock).

Default key: sha256 over the job script's content digest, the sbatch
arguments in order, the repo-relative working directory and the current
commit (HEAD). --key supplies your own (for example a sweep cell id);
SCRIPT is then optional (--wrap jobs).

Rows: `intent` (key, time, script path and digest, argument digest; raw
arguments only when they pass the sensitive scan) is written before sbatch
runs; then `submitted` (job id) or `failed` (exit code). A failed submission
does not block a retry.

A repeat of a submitted key prints the earlier job id and time and how to
check it (oms run-reconcile list), runs nothing, and exits 3. An intent with
no result (OMS died, or a concurrent submit is running) prints `unknown: an
earlier submission with this key may have gone through` and exits 3. Both can
be overridden with --force-resubmit --reason TEXT (recorded).

Unless a comment is already set (command line or #SBATCH --comment), sbatch
gets `--comment oms-key=<12 chars>` so an unknown attempt can be found with
squeue/sacct.

--dry-run prints the key and what would run; nothing is run or written.
oms slurm-lint runs first as advice; its warnings never block.

Duplicate detection inside this repository's .oms only: not exactly-once
across machines or clones.

Exit: 0 submitted (or dry run), 3 refused as duplicate/unknown, 2 usage,
otherwise sbatch's own exit status (3 is reported as 1).
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

command -v python3 >/dev/null 2>&1 || fail "python3 is required"

CUSTOM_KEY=""
DRY_RUN=0
FORCE=0
REASON=""
CMD=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --key) [ "$#" -ge 2 ] || fail "--key needs a value"; CUSTOM_KEY="$2"; shift 2 ;;
    --key=*) CUSTOM_KEY="${1#--key=}"; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --force-resubmit) FORCE=1; shift ;;
    --reason) [ "$#" -ge 2 ] || fail "--reason needs a value"; REASON="$2"; shift 2 ;;
    --reason=*) REASON="${1#--reason=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; CMD=("$@"); break ;;
    *) fail "unknown option: $1 (the command goes after --)" ;;
  esac
done

[ "${#CMD[@]}" -ge 1 ] || fail "usage: slurm-submit.sh [options] -- sbatch ARGS... SCRIPT"
[ "${CMD[0]##*/}" = sbatch ] || fail "the command must be sbatch (got ${CMD[0]})"
SBATCH_ARGS=("${CMD[@]:1}")
[ "${#SBATCH_ARGS[@]}" -ge 1 ] || fail "sbatch needs arguments and a script"
if [ "$FORCE" = 1 ] && [ -z "$REASON" ]; then
  fail "--force-resubmit requires --reason TEXT"
fi

SCRIPT=""
last="${SBATCH_ARGS[${#SBATCH_ARGS[@]}-1]}"
if [ -f "$last" ]; then
  SCRIPT="$last"
elif [ -z "$CUSTOM_KEY" ]; then
  fail "the last argument must be the job script (a readable file); use --key for a --wrap job"
fi

TMPS=()
cleanup() { [ "${#TMPS[@]}" -eq 0 ] || rm -f "${TMPS[@]}"; }
trap cleanup EXIT
new_tmp() {
  local f
  f="$(mktemp)" || fail "mktemp failed"
  TMPS+=("$f")
  printf '%s\n' "$f"
}

ARGS_FILE="$(new_tmp)"
if [ "${#SBATCH_ARGS[@]}" -gt 0 ]; then
  printf '%s\0' "${SBATCH_ARGS[@]}" > "$ARGS_FILE"
fi

# Free text recorded in a local state file passes the existing sensitive scan
# or is left out.
scan_clean() {
  local f
  f="$(new_tmp)"
  printf '%s\n' "$1" > "$f"
  ! agent_memory_file_has_sensitive_content "$f"
}

if [ -n "$CUSTOM_KEY" ]; then
  scan_clean "$CUSTOM_KEY" || fail "--key looks sensitive; use an opaque id"
fi
STORE_ARGS=()
if scan_clean "${SBATCH_ARGS[*]}"; then
  STORE_ARGS=(--store-args)
fi
if [ -n "$REASON" ] && ! scan_clean "$REASON"; then
  REASON="[withheld: looks sensitive]"
fi

COMMIT="$(git -C "$STATE_ROOT" rev-parse HEAD 2>/dev/null || echo nogit)"
helper_common=(--root "$STATE_ROOT" --cwd "$PWD" --commit "$COMMIT" --args-file "$ARGS_FILE" --custom-key "$CUSTOM_KEY")
[ -z "$SCRIPT" ] || helper_common+=(--script "$SCRIPT")

key_json="$(python3 "$HELPER" key "${helper_common[@]}")" || fail "could not derive the key"
json_field() { printf '%s\n' "$1" | sed -n 's/.*"'"$2"'": "\([^"]*\)".*/\1/p'; }
KEY="$(json_field "$key_json" key)"
SHORT="$(json_field "$key_json" short)"
[ -n "$KEY" ] && [ -n "$SHORT" ] || fail "could not derive the key"

# Advisory only: the lint is static and cheap; its warnings never block.
if [ -n "$SCRIPT" ] && [ -x "$ROOT/scripts/slurm-lint.sh" ]; then
  bash "$ROOT/scripts/slurm-lint.sh" "$SCRIPT" >&2 || true
fi

TAG=no
if [ "${#SBATCH_ARGS[@]}" -gt 0 ]; then
  for a in "${SBATCH_ARGS[@]}"; do
    case "$a" in --comment|--comment=*) TAG=set ;; esac
  done
fi
if [ "$TAG" = no ] && [ -n "$SCRIPT" ] &&
  grep -Eq '^#SBATCH[[:space:]]+([^#]*[[:space:]])?--comment' "$SCRIPT" 2>/dev/null; then
  TAG=set
fi
RUN=("${CMD[0]}")
if [ "$TAG" = no ]; then
  TAG=yes
  RUN+=(--comment "oms-key=$SHORT")
fi
RUN+=("${SBATCH_ARGS[@]}")

if [ "$DRY_RUN" = 1 ]; then
  status_json="$(python3 "$HELPER" status "${helper_common[@]}" --file "$SUBMITS_FILE")"
  echo "key: $KEY"
  echo "earlier record: $(json_field "$status_json" state)"
  if [ "$TAG" = yes ]; then
    echo "would run: ${RUN[*]}"
  else
    echo "would run: ${RUN[*]}   (comment already set by the caller; the attempt is not tagged)"
  fi
  echo "dry run: nothing was run or written"
  exit 0
fi

agent_memory_ensure_oms_ignore_for_path "$SUBMITS_FILE" 2>/dev/null || true

begin_args=("${helper_common[@]}" --file "$SUBMITS_FILE" --tag "$TAG")
[ "${#STORE_ARGS[@]}" -eq 0 ] || begin_args+=("${STORE_ARGS[@]}")
[ "$FORCE" = 0 ] || begin_args+=(--force --reason "$REASON")

rc=0
begin_json="$(oms_with_file_lock "$SUBMITS_FILE" python3 "$HELPER" begin "${begin_args[@]}")" || rc=$?
if [ "$rc" -eq 3 ]; then
  exit 3
elif [ "$rc" -ne 0 ]; then
  fail "could not record the submission intent (status $rc)"
fi
ATTEMPT="$(json_field "$begin_json" attempt)"
[ -n "$ATTEMPT" ] || fail "could not record the submission intent"

out_file="$(new_tmp)"
set +e
"${RUN[@]}" | tee "$out_file"
sbatch_rc=${PIPESTATUS[0]}
set -e

if [ "$sbatch_rc" -ne 0 ]; then
  oms_with_file_lock "$SUBMITS_FILE" python3 "$HELPER" finish --file "$SUBMITS_FILE" \
    --key "$KEY" --attempt "$ATTEMPT" --exit-code "$sbatch_rc" ||
    echo "warning: sbatch failed and the failed row could not be written; the next attempt will report unknown" >&2
  echo "failed: sbatch exited $sbatch_rc; recorded, a retry with this key is allowed" >&2
  [ "$sbatch_rc" -ne 3 ] || sbatch_rc=1
  exit "$sbatch_rc"
fi

job_line="$(sed -n -e 's/^Submitted batch job \([0-9][0-9]*\)\(.* on cluster \(.*\)\)\{0,1\}$/\1;\3/p' \
  -e 's/^\([0-9][0-9]*\)\(;\(.*\)\)\{0,1\}$/\1;\3/p' "$out_file" | tail -n 1)"
job_id="${job_line%%;*}"
where_on="${job_line#*;}"
[ "$where_on" != "$job_line" ] || where_on=""
oms_with_file_lock "$SUBMITS_FILE" python3 "$HELPER" finish --file "$SUBMITS_FILE" \
  --key "$KEY" --attempt "$ATTEMPT" --job-id "$job_id" --cluster-name "$where_on" ||
  echo "warning: the job was submitted but its row could not be written; the next attempt will report unknown" >&2
if [ -n "$job_id" ]; then
  echo "recorded: job $job_id key $KEY (check with: oms run-reconcile list)" >&2
else
  echo "recorded: submitted, but no job id was found in sbatch's output (key $KEY)" >&2
fi
