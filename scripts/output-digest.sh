#!/usr/bin/env bash
set -euo pipefail

# Run a command and print a bounded digest of its output instead of the whole
# log. A store-time-redacted capture is kept locally; the digest names that
# file, what it shows, what it leaves out, the exit code and the
# failure lines it recognised. Extraction is mechanical: no model call, and a
# log no recogniser understands falls back to a bounded head and tail.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_LIB="$ROOT/scripts/lib"
# shellcheck source=scripts/lib/agent-memory-common.sh
. "$ROOT_LIB/agent-memory-common.sh"
# shellcheck source=scripts/lib/oms-common.sh
. "$ROOT_LIB/oms-common.sh"

KEEP=50          # newest raw captures kept in the store; older ones of this tool are removed
WRAPPER_ERROR=125  # this tool's own failure in run mode, kept apart from the command's exit codes

usage() {
  cat <<'EOF'
Usage: output-digest.sh [options] -- COMMAND [ARG...]
       output-digest.sh [options] --file PATH [--exit-code N]

Run COMMAND, keep its whole output (stdout and stderr, in order) in a local
file, and print a bounded digest: exit code, line and byte counts, the raw
file's path and sha256, the failure lines a recogniser matched, a head and a
tail, and how much was left out. The command's exit code is this tool's exit
code. With --file, digest an existing capture; nothing is run or stored.

The digest is a derived view for reading. It is not the run's record and not
verification evidence: a verifier's input and receipt stay the raw output.

Options:
  --max-bytes N   Budget for the shown lines. Default 4000 (256..1048576).
                  Fitting lines are shown; any transformation is marked.
  --head N        Leading lines to show when cutting. Default 10.
  --tail N        Trailing lines to show when cutting. Default 30.
  --strategy MODE auto (default): try complete JSON tables or exact log runs;
                  legacy: use the failure/head/tail view only. Both mark loss.
  --no-store      Do not keep the raw output (digest names no file).
  --json          Print the digest as JSON (schema 1, kind oms-output-digest).
  --file PATH     Digest this file instead of running a command.
  --exit-code N   With --file: the exit code to report. Default: unknown.

Captures live in .oms/artifacts/output-digest/ (mode 600); the newest 50
are kept. A harness child keeps none. Before the capture is kept, every line
the secret scan flags is replaced by [oms-redacted:CLASS] (CLASS names the
pattern class, never the value); the exit code and the sha256 describe that
stored file. If the scan cannot run the stored file is one marker line. If the
shown lines still look like they carry a secret, they are withheld and only the
counts, the exit code and the path are printed.
Retained run captures include an oms_output_read reference for bounded MCP
reads or literal searches. Missing/evicted captures never rerun the command.

Exit: the command's exit code; 125 when this tool itself failed in run mode;
with --file, 0 on success and 2 on a bad request.
EOF
}

fail_usage() {
  echo "output-digest: $1" >&2
  exit 2
}

MAX_BYTES=4000
HEAD=10
TAIL=30
STORE=1
JSON=0
FILE=""
EXIT_CODE=""
STRATEGY="auto"
CAPTURE_REPO=""
declare -a COMMAND=()

while [ "$#" -gt 0 ]; do
  case "$1" in
    --max-bytes|--head|--tail|--file|--exit-code|--strategy)
      [ "$#" -ge 2 ] || fail_usage "$1 needs a value"
      case "$1" in
        --max-bytes) MAX_BYTES="$2" ;;
        --head) HEAD="$2" ;;
        --tail) TAIL="$2" ;;
        --file) FILE="$2" ;;
        --exit-code) EXIT_CODE="$2" ;;
        --strategy) STRATEGY="$2" ;;
      esac
      shift 2
      ;;
    --no-store) STORE=0; shift ;;
    --json) JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; COMMAND=("$@"); break ;;
    *) fail_usage "unknown argument: $1 (put the command after --)" ;;
  esac
done

for value in "$MAX_BYTES" "$HEAD" "$TAIL"; do
  case "$value" in ''|*[!0-9]*) fail_usage "--max-bytes, --head and --tail take whole numbers" ;; esac
done
[ "$MAX_BYTES" -ge 256 ] && [ "$MAX_BYTES" -le 1048576 ] || fail_usage "--max-bytes is 256..1048576"
[ "$HEAD" -le 10000 ] && [ "$TAIL" -le 10000 ] || fail_usage "--head and --tail are 0..10000"
case "$EXIT_CODE" in *[!0-9]*) fail_usage "--exit-code takes a whole number" ;; esac
case "$STRATEGY" in auto|legacy) ;; *) fail_usage "--strategy must be auto or legacy" ;; esac

# Print the digest of capture $1. $2 exit code or "", $3 ref or "", $4 duration or "".
digest() {
  local capture="$1" code="$2" ref="$3" duration="$4" withhold="" probe rc=0
  local -a base=(python3 "$ROOT_LIB/output_digest.py" "$capture" --max-bytes "$MAX_BYTES" --head "$HEAD" --tail "$TAIL" --strategy "$STRATEGY")
  [ -z "$code" ] || base+=(--exit-code "$code")
  [ -z "$ref" ] || base+=(--ref "$ref")
  [ -z "$duration" ] || base+=(--duration "$duration")
  [ -z "$CAPTURE_REPO" ] || base+=(--capture-repo "$CAPTURE_REPO")
  # What leaves this tool is the shown lines, so those are what is scanned.
  probe="$(mktemp)" || return 1
  if ! "${base[@]}" > "$probe" 2>/dev/null; then
    rm -f "$probe"
    "${base[@]}" >/dev/null   # print the reader's own error
    return 1
  fi
  agent_memory_file_secret_status "$probe" 2>/dev/null || rc=$?
  rm -f "$probe"
  case "$rc" in
    1) ;;
    0) withhold="the lines look like they carry a secret, so they are withheld; read the raw file yourself" ;;
    *) withhold="the secret scan could not run, so the lines are withheld; read the raw file yourself" ;;
  esac
  [ -z "$withhold" ] || base+=(--withhold "$withhold")
  [ "$JSON" -eq 0 ] || base+=(--json)
  "${base[@]}"
}

if [ -n "$FILE" ]; then
  [ "${#COMMAND[@]}" -eq 0 ] || fail_usage "--file and a command are two modes; give one"
  [ -f "$FILE" ] && [ -r "$FILE" ] || fail_usage "no readable file: $FILE"
  digest "$FILE" "$EXIT_CODE" "$FILE" "" || exit 2
  exit 0
fi

[ "${#COMMAND[@]}" -gt 0 ] || { usage >&2; exit 2; }
[ -z "$EXIT_CODE" ] || fail_usage "--exit-code goes with --file"

if [ "${OMS_HARNESS_CHILD:-0}" = 1 ]; then
  STORE=0   # a worker's .oms surface is guarded; it keeps no capture
fi

capture=""
ref=""
if [ "$STORE" -eq 1 ]; then
  state_root="$(oms_state_root "$PWD" 2>/dev/null || oms_repo_root "$PWD")"
  CAPTURE_REPO="$state_root"
  store="$state_root/.oms/artifacts/output-digest"
  if [ -L "$state_root/.oms" ] || [ -L "$state_root/.oms/artifacts" ] || [ -L "$store" ]; then
    echo "output-digest: refusing a symlinked capture store: $store" >&2
    exit "$WRAPPER_ERROR"
  fi
  if ! (umask 077 && mkdir -p "$store"); then
    echo "output-digest: cannot create the capture store: $store" >&2
    exit "$WRAPPER_ERROR"
  fi
  capture="$store/$(date -u +%Y%m%dT%H%M%SZ)-$$.log"
  ref="${capture#"$state_root"/}"
else
  capture="$(mktemp)" || exit "$WRAPPER_ERROR"
  trap 'rm -f "$capture"' EXIT
fi
if ! (umask 077 && : > "$capture"); then
  echo "output-digest: cannot write the capture: $capture" >&2
  exit "$WRAPPER_ERROR"
fi

start_s="$(date +%s)"
status=0
"${COMMAND[@]}" > "$capture" 2>&1 || status=$?
duration_s=$(( $(date +%s) - start_s ))

if [ "$STORE" -eq 1 ]; then
  # Store-time redaction: lines the shared secret scan flags are replaced by
  # [oms-redacted:CLASS] before anything stays under .oms. Fail closed when the
  # scan cannot run: the stored file becomes one marker line.
  raw_rc=0
  redacted_lines="$(agent_memory_redact_secret_lines "$capture" 2>/dev/null)" || raw_rc=$?
  case "$raw_rc" in
    0) echo "output-digest: note: $redacted_lines line(s) matching the secret scan were replaced by [oms-redacted:CLASS] markers in the stored capture" >&2 ;;
    1) ;;
    *)
      printf '[oms-redacted:scan-failed]\n' > "$capture"
      echo "output-digest: note: the secret scan could not run; the stored capture was replaced by a marker" >&2 ;;
  esac
  # Bounded retention: only this tool's own captures, oldest first.
  extra=0
  while IFS= read -r old; do
    [ -n "$old" ] || continue
    extra=$((extra + 1))
    [ "$extra" -le "$KEEP" ] || rm -f "$store/$old"
  done < <(cd "$store" && find . -maxdepth 1 -type f -name '20*Z-*.log' | sed 's|^\./||' | LC_ALL=C sort -r)
fi

if ! digest "$capture" "$status" "$ref" "$duration_s"; then
  echo "output-digest: the digest failed; the command exited $status${ref:+ and its output is in $ref}" >&2
fi
exit "$status"
