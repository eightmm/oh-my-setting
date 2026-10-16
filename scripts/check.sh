#!/usr/bin/env bash
set -euo pipefail

# Core repository gate shared with CI, which additionally runs the real install
# lifecycle on Linux (symlink and copy ownership), macOS, and Windows Git Bash,
# and parses every script with macOS's stock Bash 3.2. A missing tool is a HARD
# FAILURE, never a silent skip. Wire this as a pre-push hook with
# scripts/install-hooks.sh.
#
# A passing run reports one line per stage: this gate is read by an agent
# between edits, and a green run has nothing to say beyond "ok". OMS_VERBOSE=1
# prints everything, and a failing stage always prints its own output.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

# Git exports GIT_DIR (and friends) to hooks. From a linked worktree that
# path is absolute, so it overrides repository discovery for every git call
# in every test fixture — `cd fixture && git add` then operates on THIS
# checkout, not the fixture. A pre-push from a worktree proved the leak by
# rewriting the checkout with fixture commits. The gate must be hermetic to
# the invoking git context.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE \
  GIT_OBJECT_DIRECTORY GIT_QUARANTINE_PATH GIT_PREFIX
unset NVM_BIN NVM_DIR
# Suites supply fake providers explicitly through PATH. Neither peer discovery
# nor managed Python activation may restore the invoking host's tool paths.
export OMS_TEST_PROVIDER_PATH_ONLY=1

# The same hermeticity applies to the harness's own session identity. A
# harness child (council seat, delegated worker) inherits capability
# variables, and OMS_HARNESS_CHILD=1 suppresses auto-task creation by design
# — so autonomy fixtures fail from inside any harness-mediated call while
# passing from an operator shell: the gate lies to the harness itself
# (recorded 2026-08-10, a seat-run verify went red in 81s on a green tree).
# The invoking session is not part of any fixture; a suite that needs child
# semantics sets the variables explicitly.
unset OMS_HARNESS_CHILD OMS_HARNESS_ORIGIN OMS_HARNESS_PARENT_AGENT \
  OMS_HARNESS_CALL_ID OMS_STATE_REPO OMS_ATTEMPT_ID OMS_PLAN_LEASE_ID \
  OMS_LEASE_ID OMS_EXECUTOR_ID OMS_SOUL_SHA256 OMS_APPROVAL_ID \
  OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE OMS_HARNESS_DELEGATE_DEPTH \
  OMS_ROOM_ID OMS_ROOM_PARTICIPANT OMS_ROOM_REPO OMS_ROOM_ADMITTED_PARTICIPANT \
  OMS_HOOK_AGENT OMS_PANEL_ENTRYPOINT OMS_PANEL_SESSION OMS_PANEL_MAIN_ATTEMPT \
  OMS_PANEL_REPO OMS_PANEL_RESULTS OMS_PANEL_DISPATCH OMS_PANEL_ROLE \
  OMS_PANEL_WORKLOAD OMS_PANEL_PURPOSE OMS_PANEL_LABEL OMS_PANEL_HOST OMS_PANEL_VIEW \
  HERDR_ENV HERDR_SOCKET_PATH HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID

# Keep lock/capability caches out of the invoking user's HOME. Several focused
# suites intentionally create temporary repos but use shared lifecycle helpers;
# one check run should be hermetic without duplicating HOME setup in every file.
# The lock dir gets its own variable rather than riding on XDG_CACHE_HOME: locks
# must resolve identically from a desktop shell and from the auto-update timer,
# so the production path cannot key off an ambient XDG variable (file-lock.sh).
CHECK_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/oms-check-runtime.XXXXXX")"
# Nested fixtures must not append to the real job summary.
CHECK_SUMMARY="${GITHUB_STEP_SUMMARY:-}"
unset GITHUB_STEP_SUMMARY
report_check() {
  [ "${LIST_STAGES:-0}" != 1 ] || return 0
  [ "${PRINT_AFFECTED_MODE:-0}" != 1 ] || return 0
  [ -s "$CHECK_RUNTIME/timings" ] || [ "${RUN_AFFECTED:-0}" = 1 ] || return 0
  python3 - "$CHECK_RUNTIME/timings" "$CHECK_SUMMARY" "${AFFECTED_MODE:-${MODE:-full}}" \
    "${AFFECTED_REASONS:-explicit check mode}" "$1" <<'PY'
import html, pathlib, sys
path, summary, mode, reasons, rc = sys.argv[1:]
rows = []
try:
    for line in pathlib.Path(path).read_text().splitlines():
        seconds, kind, name = line.split('\t', 2)
        rows.append((int(seconds), kind, name))
except (OSError, ValueError):
    pass
reason = ' '.join(reasons.split())[:800]
lines = ['check-summary: mode=%s; exit=%s; reason=%s' % (mode, rc, reason)]
for kind in ('stage', 'test', 'operator-section', 'operator-shard'):
    for seconds, _, name in sorted((row for row in rows if row[1] == kind), reverse=True)[:5]:
        lines.append('slow-%s: %ss %s' % (kind, seconds, name))
print('\n'.join(lines))
if summary:
    try:
        with open(summary, 'a', encoding='utf-8') as handle:
            handle.write('\n### OMS verification selection and timing\n\n<pre>' + html.escape('\n'.join(lines)) + '</pre>\n')
    except OSError as exc:
        print('warning: could not append CI summary: %s' % exc, file=sys.stderr)
PY
}
CHECK_STATE_GUARD_ACTIVE=0
CHECK_ORIGINAL_LOCK_DIR="${OMS_LOCK_DIR:-}"
CHECK_STAGE_PID=""
cleanup_check_runtime() {
  local rc=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ -n "$CHECK_STAGE_PID" ]; then
    kill -TERM "$CHECK_STAGE_PID" 2>/dev/null || true
    wait "$CHECK_STAGE_PID" 2>/dev/null || true
  fi
  if [ "$CHECK_STATE_GUARD_ACTIVE" = 1 ] && ! verify_oms_state; then
    rc=1
  fi
  if [ -n "${OMS_CHECK_MAINTENANCE_MARKER:-}" ]; then
    oms_check_maintenance_end
  fi
  report_check "$rc" || true
  rm -rf "$CHECK_RUNTIME"
  exit "$rc"
}
trap cleanup_check_runtime EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
export XDG_CACHE_HOME="${OMS_CHECK_CACHE_HOME:-$CHECK_RUNTIME/cache}"
# Config gets the same treatment: the install receipt resolves through
# XDG_CONFIG_HOME before HOME, and GitHub runners export it globally — so a
# fixture's HOME override never hid the runner-global receipt an earlier
# suite had written, and uninstall fixtures failed the canonical-owner check
# only in CI. Redirecting it also keeps suites from reading or writing the
# invoking user's real config.
export XDG_CONFIG_HOME="${OMS_CHECK_CONFIG_HOME:-$CHECK_RUNTIME/config}"
export OMS_LOCK_DIR="${OMS_CHECK_LOCK_DIR:-$CHECK_RUNTIME/locks}"
# Fixture Stop hooks must never reach the developer's live Codex app.
export OMS_CODEX_NOTIFY=0
mkdir -p "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$OMS_LOCK_DIR"

# These tune only the outer scripts-smoke runner. Do not export them into the
# focused suites or into scripts-smoke's test bodies.
CHECK_SMOKE_TIMINGS="${OMS_SMOKE_TIMINGS:-0}"
CHECK_SMOKE_TIMING_LIMIT="${OMS_SMOKE_TIMING_LIMIT:-10}"
unset OMS_SMOKE_TIMINGS OMS_SMOKE_TIMING_LIMIT

# CI partitions this gate into lint, focused suites, and native scripts-smoke
# shards. The default remains the complete local gate. Flags are used instead
# of environment variables, which would leak into every descendant (including
# tests that invoke this gate themselves).
MODE=full
PARALLEL=0
SKIP_LINT=0
SMOKE_SHARD=""
QUICK_FROM=""
QUICK_TO=""
AFFECTED_MODE=""
AFFECTED_REASONS=""
AFFECTED_TESTS=()
AFFECTED_CASE_LANGS=()
AFFECTED_CASE_PATHS=()
AFFECTED_CASE_SELECTORS=()
FOCUSED_LANE=""
FOCUSED_LANE_I=0
FOCUSED_LANE_N=0
FOCUSED_STAGE_INDEX=0
LIST_STAGES=0
PRINT_AFFECTED_MODE=0
CI_OUTPUT=""
CI_PARITY=0
AFFECTED_CAPPED=0
usage() {
  cat <<'EOF'
usage: check.sh [MODE] [OPTIONS]

Runs the repository gate: lint (shellcheck, Bash 3.2, Python syntax, skill
manifest) then the test suites. With no arguments it runs both.

  --no-lint             Skip lint only when equivalent checks ran separately.
  --lint-only           Run only lint stages.
  --parallel            Run the complete gate as concurrent CI partitions,
                        preserving signal handling and failure propagation.
  --focused-only        Run only the focused test suites.
  --focused-lane I/N    Run only every Nth focused stage starting at the Ith
                        (with --focused-only). The partition is positional,
                        so a newly registered stage lands in a lane
                        automatically and the lane union is always the whole
                        focused list.
  --scripts-smoke-only  Run only tests/scripts-smoke.sh.
  invariants            Run the fast repository-wide pre-land invariant set.
  --shard I/N           Select one scripts-smoke shard (with the mode above).
  --list-stages         Print the stage names the selected mode and lane
                        would run, one per line, without running anything.
                        (The scripts-smoke shards run outside the stage list
                        and are not included.)
  --quick               Run a partial changed-file gate for protected pushes.
  --affected            Check ordinary docs directly; otherwise use the Project
                        Graph for affected tests. Critical or uncertain changes
                        fall back to the complete gate, including lint.
  --print-affected-mode With --affected, print affected or full after planning
                        and exit without running test stages.
  --ci-output FILE      With --affected, append mode=affected|full and
                        native=true|false to FILE.
                        Run affected checks here; defer full checks to CI lanes.
  --ci-parity           With --affected, the oms land default: a full fallback
                        runs lint and the quick lane instead (CI runs the full
                        selection), plus operator-tools when tier-routing
                        inputs change; test stages run with every registry
                        provider CLI hidden from PATH as on CI hosts, and
                        changed shell files are parsed by a real Bash 3.2
                        (docker image bash:3.2 on
                        Linux, /bin/bash on macOS; skipped with a reason when
                        neither is available).
  --changed-from REF    Base tree for --quick or --affected.
  --changed-to REF      Target tree for --quick or --affected.

OMS_CHECK_JOBS=N enables the constrained-host profile: at most N parallel
lanes, each with serial nested runners. Other modes cap their suite workers
at N. The default partitions are unchanged. OMS_CHECK_STAGE_TIMEOUT_SECONDS
overrides the generous 30-minute per-stage deadline (operator-tools: 60m).
EOF
}

select_mode() {
  [ "$MODE" = full ] || {
    echo "error: gate modes are mutually exclusive: $MODE and $1" >&2
    usage >&2
    exit 2
  }
  MODE="$1"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --parallel) PARALLEL=1; shift ;;
    --no-lint) SKIP_LINT=1; shift ;;
    --lint-only) select_mode lint; shift ;;
    --focused-only) select_mode focused; shift ;;
    --scripts-smoke-only) select_mode scripts-smoke; shift ;;
    invariants) select_mode invariants; shift ;;
    --shard)
      [ "$#" -ge 2 ] || { echo "error: --shard requires I/N" >&2; exit 2; }
      SMOKE_SHARD="$2"
      shift 2
      ;;
    --focused-lane)
      [ "$#" -ge 2 ] || { echo "error: --focused-lane requires I/N" >&2; exit 2; }
      FOCUSED_LANE="$2"
      shift 2
      ;;
    --list-stages) LIST_STAGES=1; shift ;;
    --quick) select_mode quick; shift ;;
    --affected) select_mode affected; shift ;;
    --print-affected-mode) PRINT_AFFECTED_MODE=1; shift ;;
    --ci-output)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { echo "error: --ci-output requires a file" >&2; exit 2; }
      CI_OUTPUT="$2"; shift 2 ;;
    --ci-parity) CI_PARITY=1; shift ;;
    --changed-from)
      [ "$#" -ge 2 ] || { echo "error: --changed-from requires a ref" >&2; exit 2; }
      QUICK_FROM="$2"
      shift 2
      ;;
    --changed-to)
      [ "$#" -ge 2 ] || { echo "error: --changed-to requires a ref" >&2; exit 2; }
      QUICK_TO="$2"
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -n "${OMS_CHECK_JOBS:-}" ]; then
  case "$OMS_CHECK_JOBS" in 0|*[!0-9]*) echo 'error: OMS_CHECK_JOBS must be a positive integer' >&2; exit 2 ;; esac
  [ "$OMS_CHECK_JOBS" -gt 0 ] || exit 2
  for check_jobs_var in OMS_LINT_JOBS_MAX OMS_SMOKE_JOBS OMS_OPERATOR_JOBS; do
    case "$check_jobs_var" in
      OMS_LINT_JOBS_MAX) check_jobs_value="${OMS_LINT_JOBS_MAX:-4}" ;;
      OMS_SMOKE_JOBS) check_jobs_value="${OMS_SMOKE_JOBS:-4}" ;;
      OMS_OPERATOR_JOBS) check_jobs_value="${OMS_OPERATOR_JOBS:-4}" ;;
    esac
    case "$check_jobs_value" in ''|0|*[!0-9]*) echo "error: invalid $check_jobs_var" >&2; exit 2 ;; esac
    [ "$check_jobs_value" -le "$OMS_CHECK_JOBS" ] || check_jobs_value="$OMS_CHECK_JOBS"
    export "$check_jobs_var=$check_jobs_value"
  done
fi

if [ "$MODE" = lint ] && [ "$SKIP_LINT" = 1 ]; then
  echo "error: --no-lint and --lint-only leave nothing to run" >&2
  exit 2
fi
if [ "$PRINT_AFFECTED_MODE" = 1 ] && { [ "$MODE" != affected ] || [ "$LIST_STAGES" = 1 ]; }; then
  echo "error: --print-affected-mode requires --affected and cannot combine with --list-stages" >&2
  exit 2
fi
if [ -n "$CI_OUTPUT" ] && { [ "$MODE" != affected ] || [ "$LIST_STAGES" = 1 ] || [ "$PRINT_AFFECTED_MODE" = 1 ]; }; then
  echo "error: --ci-output requires --affected without listing/print-only flags" >&2
  exit 2
fi
if [ "$CI_PARITY" = 1 ] && { [ "$MODE" != affected ] || [ -n "$CI_OUTPUT" ] || [ "$PRINT_AFFECTED_MODE" = 1 ]; }; then
  echo "error: --ci-parity requires --affected without --ci-output or --print-affected-mode" >&2
  exit 2
fi
if [ "$SKIP_LINT" = 1 ] && [ "$MODE" != full ] && [ "$MODE" != lint ] && [ "$MODE" != affected ]; then
  echo "error: --no-lint only modifies the complete or affected gate" >&2
  exit 2
fi
if [ -n "$SMOKE_SHARD" ] && [ "$MODE" != scripts-smoke ]; then
  echo "error: --shard requires --scripts-smoke-only" >&2
  exit 2
fi
if [ -n "$FOCUSED_LANE" ]; then
  if [ "$MODE" != focused ]; then
    echo "error: --focused-lane requires --focused-only" >&2
    exit 2
  fi
  case "$FOCUSED_LANE" in
    [1-9]/[1-9]|[1-9]/1[0-6]|1[0-6]/1[0-6]) ;;
    *)
      echo "error: --focused-lane must be I/N with 1 <= I <= N <= 16" >&2
      exit 2
      ;;
  esac
  FOCUSED_LANE_I="${FOCUSED_LANE%%/*}"
  FOCUSED_LANE_N="${FOCUSED_LANE##*/}"
  if [ "$FOCUSED_LANE_I" -gt "$FOCUSED_LANE_N" ]; then
    echo "error: --focused-lane must be I/N with 1 <= I <= N <= 16" >&2
    exit 2
  fi
fi
if [ "$MODE" = quick ] || [ "$MODE" = affected ]; then
  [ -n "$QUICK_FROM" ] && [ -n "$QUICK_TO" ] || {
    echo "error: --$MODE requires --changed-from and --changed-to" >&2
    exit 2
  }
elif [ -n "$QUICK_FROM" ] || [ -n "$QUICK_TO" ]; then
  echo "error: --changed-from/--changed-to require --quick or --affected" >&2
  exit 2
fi

if [ "$LIST_STAGES" != 1 ]; then
  # Use the caller's lock namespace, not the fixture-only OMS_LOCK_DIR above.
  # Each parallel lane registers independently; no inherited bypass flag.
  # shellcheck source=scripts/lib/check-maintenance.sh
  . "$ROOT/scripts/lib/check-maintenance.sh"
  OMS_LOCK_DIR="$CHECK_ORIGINAL_LOCK_DIR" oms_check_maintenance_begin "$ROOT"
fi

if [ "$PARALLEL" = 1 ]; then
  [ "$MODE" = full ] || { echo "error: --parallel requires the complete gate" >&2; exit 2; }
  if [ "$LIST_STAGES" != 1 ]; then
    # shellcheck source=scripts/lib/check-parallel.sh
    . "$ROOT/scripts/lib/check-parallel.sh"
    oms_check_parallel "$ROOT/scripts/check.sh" "$CHECK_RUNTIME/parallel" "$SKIP_LINT"
    # A skipped stage is not the complete gate; oms land installs only on the exact full markers.
    if [ "$SKIP_LINT" = 1 ]; then
      echo "check: ok (parallel gate without lint)"
    else
      echo "check: ok (parallel complete gate)"
    fi
    exit 0
  fi
fi

RUN_LINT=0
RUN_FOCUSED=0
RUN_SMOKE=0
RUN_INVARIANTS=0
RUN_QUICK=0
RUN_AFFECTED=0
case "$MODE" in
  full)
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_FOCUSED=1
    RUN_SMOKE=1
    ;;
  lint) RUN_LINT=1 ;;
  focused) RUN_FOCUSED=1 ;;
  scripts-smoke) RUN_SMOKE=1 ;;
  invariants) RUN_INVARIANTS=1 ;;
  quick) RUN_QUICK=1 ;;
  affected) RUN_AFFECTED=1 ;;
esac

# Binary name is overridable so tests can exercise the missing-tool path
# deterministically without PATH surgery.
SHELLCHECK="${OMS_SHELLCHECK_BIN:-shellcheck}"

# With -x, ShellCheck parses every sourced library again for each script that
# sources it, and the largest suite alone holds gigabytes of AST: one serial
# invocation over the tree spent 282s in CI and 160s on a workstation. Batches
# run in parallel, largest files first so the long pole starts at once. The
# worker count is bounded by memory rather than cores (the biggest file peaks
# near 6.5 GB), and xargs exits non-zero when any batch does, which the stage
# reads as a failure like any other.
lint_shell() {  # lint_shell FILE...
  local jobs
  jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 2)"
  case "$jobs" in ''|*[!0-9]*) jobs=2 ;; esac
  [ "$jobs" -le "${OMS_LINT_JOBS_MAX:-4}" ] || jobs="${OMS_LINT_JOBS_MAX:-4}"
  [ "$jobs" -ge 1 ] || jobs=1
  ls -S -- "$@" | tr '\n' '\0' |
    xargs -0 -P "$jobs" -n 2 "$SHELLCHECK" -x -S warning
}
# Stages run in a supervised Bash child; only lint uses a shell function.
export -f lint_shell
export SHELLCHECK
if [ "$RUN_LINT" = 1 ] && ! command -v "$SHELLCHECK" >/dev/null 2>&1; then
  echo "FATAL: shellcheck is not installed — CI enforces it, so passing here" >&2
  echo "would be false confidence. Install one of:" >&2
  echo "  apt-get install shellcheck   |   brew install shellcheck" >&2
  echo "  or a static binary from https://github.com/koalaman/shellcheck/releases" >&2
  exit 1
fi

# No suite may write into this checkout's own .oms state. A test that forgets
# --repo defaults to the working directory. The inventory and its ambient
# exclusions live in the library so they can be tested directly.
oms_state_fingerprint() {
  [ -d "$ROOT/.oms" ] || return 0
  python3 "$ROOT/scripts/lib/oms-state-inventory.py" "$ROOT/.oms" | tr -d '\r'
}

STATE_BEFORE="$CHECK_RUNTIME/oms-state.before"
STATE_AFTER="$CHECK_RUNTIME/oms-state.after"
verify_oms_state() {
  local fingerprint_rc=0
  [ "$CHECK_STATE_GUARD_ACTIVE" = 1 ] || return 0
  CHECK_STATE_GUARD_ACTIVE=0
  oms_state_fingerprint > "$STATE_AFTER" || fingerprint_rc=$?
  if [ "$fingerprint_rc" -ne 0 ]; then
    echo "check: could not inspect this checkout's .oms state" >&2
    return "$fingerprint_rc"
  fi
  if ! cmp -s "$STATE_BEFORE" "$STATE_AFTER"; then
    echo "check: a suite wrote into this checkout's .oms state" >&2
    echo "usually a test that omits --repo and defaults to the working directory;" >&2
    echo "give it a temporary repo instead. A live agent session driving this same" >&2
    echo "checkout can also be the writer — check the entry's own timestamp before" >&2
    echo "hunting a test. First changed inventory entries:" >&2
    diff -u "$STATE_BEFORE" "$STATE_AFTER" | sed -n '1,40p' >&2 || true
    return 1
  fi
  return 0
}
oms_state_fingerprint > "$STATE_BEFORE"
CHECK_STATE_GUARD_ACTIVE=1

# Own a stage's process tree independently of the caller's terminal. Parallel
# suites create additional process groups, so a deadline must also follow PPIDs.
run_stage_with_deadline() {  # SECONDS COMMAND...
  exec python3 - "$BASH" "$@" <<'PY'
import os, signal, subprocess, sys, time

bash, wall, *command = sys.argv[1:]
try:
    seconds = int(wall)
    if seconds <= 0:
        raise ValueError()
except ValueError:
    raise SystemExit('error: OMS_CHECK_STAGE_TIMEOUT_SECONDS must be a positive integer')
forwarded = tuple(getattr(signal, name) for name in ('SIGINT', 'SIGTERM', 'SIGHUP')
                  if hasattr(signal, name))
process = None
pending_signal = None

def records():
    rows = subprocess.check_output(['ps', '-eo', 'pid=,ppid=,lstart='], text=True, timeout=5)
    result = {}
    for row in rows.splitlines():
        pid, parent, stamp = row.strip().split(None, 2)
        result[int(pid)] = (int(parent), stamp)
    return result

def tree(snapshot):
    owned = {process.pid}
    while True:
        children = {pid for pid, (parent, _) in snapshot.items() if parent in owned}
        if children <= owned:
            break
        owned.update(children)
    return {pid: snapshot[pid] for pid in owned if pid in snapshot}

def stop():
    if os.name != 'posix':
        # Windows Python sees native PIDs; taskkill /T covers Git Bash children.
        subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], check=False, timeout=10)
        process.wait(timeout=10)
        return
    try:
        owned = tree(records())
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        print('warning: could not inventory stage descendants: %s' % exc, file=sys.stderr)
        owned = {}
    for pid in reversed(list(owned)):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    # Keep the leader unreaped until escalation so its group ID cannot be reused.
    time.sleep(5)
    try:
        snapshot = records()
        current = tree(snapshot)
    except (OSError, ValueError, subprocess.SubprocessError):
        snapshot = current = {}
    for pid, identity in owned.items():
        # A reparented child retains its start time; a recycled PID does not.
        try:
            if snapshot.get(pid, (None, None))[1] == identity[1]:
                os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for pid in current.keys() - owned.keys():
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=10)

def interrupted(signum, _frame):
    global pending_signal
    # Popen can receive a signal after spawning, before returning its handle.
    if process is None:
        if pending_signal is None:
            pending_signal = signum
        return
    for sig in forwarded:
        signal.signal(sig, signal.SIG_IGN)
    stop()
    raise SystemExit(128 + signum)

for sig in forwarded:
    signal.signal(sig, interrupted)
# Handled signals become default on exec, undoing Bash's ignored INT for the
# stage while keeping the supervisor protected throughout detached launch.
process = subprocess.Popen([bash, '-c', '"$@"', 'oms-check-stage', *command],
                           stdin=subprocess.DEVNULL, start_new_session=os.name == 'posix')
if pending_signal is not None:
    interrupted(pending_signal, None)
try:
    rc = process.wait(timeout=seconds)
except subprocess.TimeoutExpired:
    print('error: stage deadline exceeded after %ss: %s' % (wall, command[0]), file=sys.stderr)
    for sig in forwarded:
        signal.signal(sig, signal.SIG_IGN)
    stop()
    raise SystemExit(124)
raise SystemExit(128 - rc if rc < 0 else rc)
PY
}

# One line per stage on success, full output on failure: the gate is read by an
# agent between edits, and a green run has nothing to say beyond "ok".
stage() {  # stage NAME COMMAND...
  local name="$1"
  local log
  local rc=0
  local started elapsed deadline=1800
  shift

  # Lane partition is positional over the stage calls the selected mode
  # actually reaches (focused mode reaches only the focused block), so every
  # stage belongs to exactly one lane and a new registration cannot fall
  # through the partition.
  if [ -n "$FOCUSED_LANE" ]; then
    FOCUSED_STAGE_INDEX=$((FOCUSED_STAGE_INDEX + 1))
    if [ $(( (FOCUSED_STAGE_INDEX - 1) % FOCUSED_LANE_N )) -ne $((FOCUSED_LANE_I - 1)) ]; then
      return 0
    fi
  fi
  if [ "$LIST_STAGES" = 1 ]; then
    echo "$name"
    return 0
  fi

  log="$(mktemp "${TMPDIR:-/tmp}/oms-check.XXXXXX")"
  started="$(date +%s)"
  # Recorded operator stage: 1,179s. Leave room for slower shared hosts; CI
  # bounds the whole lane separately rather than tightening fixture assertions.
  [ "$name" != operator-tools ] || deadline=3600
  run_stage_with_deadline "${OMS_CHECK_STAGE_TIMEOUT_SECONDS:-$deadline}" "$@" > "$log" 2>&1 &
  CHECK_STAGE_PID=$!
  wait "$CHECK_STAGE_PID" || rc=$?
  CHECK_STAGE_PID=""
  elapsed=$(( $(date +%s) - started ))
  printf '%s\tstage\t%s\n' "$elapsed" "$name" >> "$CHECK_RUNTIME/timings"
  # Reuse the runner's bounded per-test timings before its successful log is
  # discarded. No extra test run or persistent timing database.
  awk '/^smoke-timing: [0-9]+s [A-Za-z0-9_]+$/ { sub(/s$/, "", $2); print $2 "\ttest\t" $3 }
    /^operator-section: [0-9]+s [A-Za-z0-9_.\/-]+$/ { sub(/s$/, "", $2); print $2 "\toperator-section\t" $3 }
    /^operator-shard: [0-9]+\/[0-9]+ [0-9]+s$/ { sub(/s$/, "", $3); print $3 "\toperator-shard\t" $2 }' \
    "$log" >> "$CHECK_RUNTIME/timings"
  if [ "${OMS_VERBOSE:-0}" = "1" ]; then
    cat "$log"
    [ "$rc" -eq 124 ] || rm -f "$log"
  elif [ "$rc" -ne 0 ]; then
    # Failure output is read by an agent between edits: bounded tail in the
    # session (the assertion that failed prints last), full log kept on disk
    # for oms job-digest. Unbounded, one failing smoke can dump hundreds of
    # kilobytes into the calling context.
    local log_cap log_bytes
    log_cap="${OMS_CHECK_LOG_BYTES:-65536}"
    case "$log_cap" in ''|0|*[!0-9]*) log_cap=65536 ;; esac
    log_bytes="$(wc -c < "$log" | tr -d ' ')"
    if [ "$log_bytes" -gt "$log_cap" ]; then
      echo "[check: output is ${log_bytes}B; showing the last ${log_cap}B — full log: $log]"
      tail -c "$log_cap" "$log"
    else
      cat "$log"
      [ "$rc" -eq 124 ] || rm -f "$log"
    fi
  else
    rm -f "$log"
  fi
  if [ "$rc" -ne 0 ]; then
    [ "$rc" -ne 124 ] || echo "check: deadline diagnostics retained: $log" >&2
    echo "check: $name FAILED (${elapsed}s)" >&2
    exit "$rc"
  fi
  echo "ok: $name (${elapsed}s)"
}

# CI hosts have no provider CLI, so a suite that quietly depends on one passes
# here and fails there (f698ac9: panel routing found no installed provider).
# The names come from the provider registry (every built-in transport, both
# OpenCode spellings) plus custom oms-agent-adapter-* executables, so a
# provider the registry would discover stays hidden. Each PATH directory
# holding one is replaced by a shadow of symlinks to its other entries;
# dropping the directory would also drop oms, python3 and the rest. Git Bash
# copies instead of linking, so Windows hosts skip with a reason.
is_provider_cli() {  # is_provider_cli BASENAME
  case " $CHECK_PROVIDER_CLIS " in *" $1 "*) return 0 ;; esac
  case "$1" in oms-agent-adapter-?*) return 0 ;; esac
  return 1
}
hide_provider_clis() {
  local rest="$PATH:" dir entry base hit shadow count=0 filtered=""
  case "$(uname -s 2>/dev/null)" in
    MINGW*|MSYS*|CYGWIN*)
      echo "skip: provider-less PATH (no native symlinks on this host)"
      return 0
      ;;
  esac
  CHECK_PROVIDER_CLIS="opencode opencode2 $(
    # shellcheck source=scripts/lib/provider-registry.sh
    . "$ROOT/scripts/lib/provider-registry.sh"
    for name in $(oms_provider_supported_names); do oms_provider_binary "$name"; done | tr '\n' ' '
  )" || { echo "FATAL: the provider registry did not list its CLIs" >&2; exit 1; }
  while [ -n "$rest" ]; do
    dir="${rest%%:*}"
    rest="${rest#*:}"
    hit=0
    if [ -n "$dir" ] && [ -d "$dir" ]; then
      for entry in "$dir"/*; do
        if is_provider_cli "${entry##*/}" && [ -e "$entry" ]; then hit=1; break; fi
      done
    fi
    if [ "$hit" = 1 ]; then
      count=$((count + 1))
      shadow="$CHECK_RUNTIME/provider-less/$count"
      mkdir -p "$shadow"
      dir="$(cd "$dir" && pwd -P)"
      for entry in "$dir"/*; do
        [ -e "$entry" ] || continue
        base="${entry##*/}"
        is_provider_cli "$base" || ln -s "$entry" "$shadow/$base"
      done
      dir="$shadow"
    fi
    [ -z "$dir" ] || filtered="${filtered:+$filtered:}$dir"
  done
  PATH="$filtered"
  export PATH
  hash -r
  echo "ci-parity: provider CLIs hidden from PATH ($CHECK_PROVIDER_CLIS oms-agent-adapter-*; shadowed PATH entries: $count)"
}

# The host bash -n accepts what Bash 3.2 cannot parse (f698ac9 shipped such a
# file to the macOS leg), so ci-parity parses changed shell files with a real
# 3.2. It never pulls an image: an absent docker, daemon or image is a printed
# skip, and CI's macOS leg stays the authority.
parse_with_bash32() {  # parse_with_bash32 FILE...
  local docker="${OMS_DOCKER_BIN:-docker}" probe=()
  if [ "$LIST_STAGES" = 1 ]; then
    stage bash32-parse true
    return 0
  fi
  case "$(uname -s 2>/dev/null)" in
    Darwin)
      stage bash32-parse env OMS_BASH32_BIN=/bin/bash bash scripts/check-bash32.sh "$@"
      return 0
      ;;
    Linux) ;;
    *)
      echo "skip: bash32-parse (no Bash 3.2 parser on $(uname -s 2>/dev/null))"
      return 0
      ;;
  esac
  if ! command -v "$docker" >/dev/null 2>&1; then
    echo "skip: bash32-parse (docker is not installed)"
    return 0
  fi
  ! command -v timeout >/dev/null 2>&1 || probe=(timeout 30)
  if ! ${probe[@]+"${probe[@]}"} "$docker" image inspect bash:3.2 >/dev/null 2>&1; then
    echo "skip: bash32-parse (docker image bash:3.2 is absent or docker is not running; docker pull bash:3.2 enables it)"
    return 0
  fi
  case "$ROOT" in
    *:*|*,*)
      echo "skip: bash32-parse (docker cannot mount a checkout path containing ':' or ',')"
      return 0
      ;;
  esac
  # shellcheck disable=SC2016 # $f and $@ expand inside the container.
  stage bash32-parse "$docker" run --rm --network none -v "$ROOT:/r:ro" bash:3.2 \
    bash -c 'for f in "$@"; do bash -n "/r/$f" || exit 1; done' bash32-parse "$@"
}

compile_affected_plan() {  # compile_affected_plan PLAN_JSON
  python3 - "$1" "$ROOT" <<'PY'
import json
import pathlib
import re
import sys

plan_path, root = sys.argv[1:]
try:
    with open(plan_path, encoding="utf-8") as handle:
        plan = json.load(handle)
except (OSError, ValueError) as exc:
    raise SystemExit("invalid affected plan: %s" % exc)
if not isinstance(plan, dict) or plan.get("schema") != 1:
    raise SystemExit("invalid affected plan schema")
mode = plan.get("mode")
if mode not in ("affected", "full"):
    raise SystemExit("invalid affected plan mode")

def clean(value, label):
    if not isinstance(value, str) or any(char in value for char in "\0\r\n\t"):
        raise ValueError("invalid %s" % label)
    return value

def test_path(value):
    value = clean(value, "test path")
    pure = pathlib.PurePosixPath(value)
    if pure.is_absolute() or ".." in pure.parts or not value.startswith("tests/"):
        raise ValueError("unsafe test path:%s" % value)
    target = pathlib.Path(root, *pure.parts)
    if target.is_symlink() or not target.is_file():
        raise ValueError("missing test:%s" % value)
    if value.endswith(".sh"):
        return value, "shell"
    if pure.name.startswith("test_") and value.endswith(".py"):
        return value, "python"
    raise ValueError("unsupported test:%s" % value)

raw_reasons = plan.get("reasons", [])
raw_tests = plan.get("tests", [])
raw_cases = plan.get("test_cases", [])
if not isinstance(raw_reasons, list) or not isinstance(raw_tests, list) or not isinstance(raw_cases, list):
    raise SystemExit("invalid affected plan collections")
reasons = []
try:
    reasons.extend(clean(reason, "reason") for reason in raw_reasons)
except ValueError as exc:
    raise SystemExit(str(exc))

tests = []
languages = {}
if mode == "affected":
    try:
        for raw in raw_tests:
            path, language = test_path(raw)
            if path not in languages:
                tests.append(path)
                languages[path] = language
    except ValueError as exc:
        mode = "full"
        reasons.append(str(exc))

cases = []
if mode == "affected":
    try:
        for row in raw_cases:
            if not isinstance(row, dict):
                raise ValueError("invalid test case")
            path = clean(row.get("path"), "test case path")
            language = clean(row.get("language"), "test case language")
            ident = clean(row.get("id"), "test case id")
            name = clean(row.get("name"), "test case name")
            if path not in languages or language != languages[path]:
                raise ValueError("orphan test case:%s" % ident)
            if path in ("tests/scripts-smoke.sh", "tests/autonomy-plan-run-smoke.sh"):
                if language != "shell" or not re.fullmatch(r"test_[A-Za-z0-9_]+", name):
                    raise ValueError("unsupported test case:%s" % ident)
                cases.append((language, path, name))
            elif language == "python":
                prefix = "symbol:%s::" % path
                selector = ident[len(prefix):] if ident.startswith(prefix) else ""
                atom = r"[A-Za-z_][A-Za-z0-9_]*"
                if not re.fullmatch(atom + r"(?:\." + atom + r")+", selector):
                    raise ValueError("unsupported test case:%s" % ident)
                cases.append((language, path, selector))
    except ValueError as exc:
        mode = "full"
        reasons.append(str(exc))

print("mode\t%s" % mode)
for reason in sorted(set(reasons)):
    print("reason\t%s" % reason)
if mode == "affected":
    for path in tests:
        print("test\t%s" % path)
    for language, path, selector in sorted(set(cases)):
        print("case\t%s\t%s\t%s" % (language, path, selector))
PY
}

prepare_affected_gate() {
  local plan_json="$CHECK_RUNTIME/affected-plan.json"
  local plan_rows="$CHECK_RUNTIME/affected-plan.rows"
  local graph_err="$CHECK_RUNTIME/affected-graph.err"
  local selector_changes="$CHECK_RUNTIME/affected-selector-changes"
  local kind first second third path docs_only=1 scoped_only=1 models=0

  AFFECTED_NATIVE=false
  # Read the complete path set separately: the mode selector can stop at its
  # first full boundary, but a later path can still require native coverage.
  git diff --name-only --no-renames "$QUICK_FROM" "$QUICK_TO" -- > "$selector_changes"
  while IFS= read -r path; do
    case "$path" in
      .github/workflows/*|install.sh|scripts/*.sh|scripts/*.py|scripts/oms|\
      plugins/*|rules/*|config/*|templates/*.sh|tests/*.py|tests/scripts-smoke.sh|\
      tests/*install*|tests/*update*|tests/*portability*|\
      tests/*windows*|tests/*python*|tests/*runtime*|tests/*hook*|tests/*bash32*|\
      \"*|*\\*)
        AFFECTED_NATIVE=true ;;
    esac
  done < "$selector_changes"

  # Include both sides of renames: moving executable code into docs is not a
  # documentation-only change. Unknown/quoted paths take the conservative path.
  while IFS= read -r path; do
    case "$path" in
      README.md|README.ko.md|docs/*.md) ;;
      *) docs_only=0 ;;
    esac
    case "$path" in
      README.md|README.ko.md|docs/*.md) ;;
      custom-skills/*.md|templates/*.md) ;;
      config/models.json) models=1 ;;
      *) scoped_only=0 ;;
    esac
    case "$path" in
      .github/workflows/*|scripts/check.sh|scripts/graph.sh|scripts/lib/oms_graph/*|tests/graph-smoke.sh|tests/run-smoke-shard.sh)
        AFFECTED_MODE=full
        AFFECTED_REASONS="selector-boundary:$path"
        break
        ;;
      AGENTS.md|*/AGENTS.md|CLAUDE.md|*/CLAUDE.md|GEMINI.md|*/GEMINI.md|PROJECT.md|SECURITY.md)
        AFFECTED_MODE=full
        AFFECTED_REASONS="contract-boundary:$path"
        break
        ;;
      custom-skills/*.md|templates/*.md|config/models.json) ;;
      install.sh|scripts/install*|scripts/uninstall*|scripts/update*|scripts/doctor.sh|\
      scripts/python-runtime.sh|scripts/land.sh|scripts/patch-admit.sh|scripts/pre-push-check.sh|\
      scripts/lib/oms-common.sh|\
      scripts/lib/install*|scripts/lib/python-runtime.sh|scripts/lib/*lock*|scripts/lib/*state*|\
      scripts/lib/*artifact*|scripts/lib/oms_runtime/common.py|scripts/lib/oms_runtime/child_policy.py|\
      scripts/lib/durable-jsonl.py|rules/*|config/*|templates/*|custom-skills/*|plugins/*)
        AFFECTED_MODE=full
        AFFECTED_REASONS="contract-boundary:$path"
        break
        ;;
    esac
  done < "$selector_changes"

  if ! git diff --quiet "$QUICK_TO" -- ||
      [ "$(git rev-parse 'HEAD^{tree}')" != "$(git rev-parse "$QUICK_TO^{tree}")" ] ||
      [ -n "$(git ls-files --others --exclude-standard)" ]; then
    AFFECTED_MODE=full
    AFFECTED_REASONS=workspace-differs-from-head
    AFFECTED_NATIVE=true
  fi
  if [ "$AFFECTED_MODE" = full ]; then
    : # The selector cannot be the sole judge of changes to its own boundary.
  elif [ "$docs_only" = 1 ] && [ -s "$selector_changes" ]; then
    AFFECTED_MODE=affected
    AFFECTED_REASONS=documentation-only
  elif [ "$scoped_only" = 1 ] && [ -s "$selector_changes" ]; then
    AFFECTED_MODE=affected
    AFFECTED_REASONS=scoped-contracts
    # Prose is checked directly by skill-manifest and documentation references.
    # Runtime prompt/GC/lifecycle suites do not validate the changed guidance.
    if [ "$models" = 1 ]; then
      AFFECTED_TESTS+=(tests/model-routing-smoke.sh tests/provider-registry-smoke.sh tests/models-smoke.sh)
    fi
  elif ! bash scripts/graph.sh project affected \
      --base "$QUICK_FROM" --head "$QUICK_TO" \
      --guard 'scripts/graph.sh' --guard 'scripts/lib/oms_graph/**' \
      --guard 'tests/graph-smoke.sh' --json > "$plan_json" 2> "$graph_err"; then
    AFFECTED_MODE=full
    AFFECTED_REASONS=graph-error
    echo "affected: Project Graph failed; running the complete test gate" >&2
    tail -20 "$graph_err" >&2 || true
  elif ! compile_affected_plan "$plan_json" | tr -d '\r' > "$plan_rows"; then
    AFFECTED_MODE=full
    AFFECTED_REASONS=invalid-plan
    echo "affected: invalid Project Graph plan; running the complete test gate" >&2
  else
    while IFS="$(printf '\t')" read -r kind first second third; do
      case "$kind" in
        mode) AFFECTED_MODE="$first" ;;
        reason) AFFECTED_REASONS="${AFFECTED_REASONS:+$AFFECTED_REASONS, }$first" ;;
        test) AFFECTED_TESTS[${#AFFECTED_TESTS[@]}]="$first" ;;
        case)
          AFFECTED_CASE_LANGS[${#AFFECTED_CASE_LANGS[@]}]="$first"
          AFFECTED_CASE_PATHS[${#AFFECTED_CASE_PATHS[@]}]="$second"
          AFFECTED_CASE_SELECTORS[${#AFFECTED_CASE_SELECTORS[@]}]="$third"
          ;;
      esac
    done < "$plan_rows"
  fi

  if [ "$AFFECTED_MODE" = affected ]; then
    AFFECTED_REASONS="${AFFECTED_REASONS:-positive graph evidence}"
    RUN_QUICK=1
    echo "affected: ${AFFECTED_REASONS:-positive graph evidence}; ${#AFFECTED_TESTS[@]} selected test file(s)" >&2
  elif [ "$CI_PARITY" = 1 ]; then
    # CI's own selector reaches the same verdict for the pushed range and runs
    # the full lanes on separate runners; repeating them here cost 50 minutes.
    AFFECTED_MODE=full
    AFFECTED_CAPPED=1
    AFFECTED_REASONS="${AFFECTED_REASONS:-uncertain}; capped by --ci-parity"
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_QUICK=1
    echo "affected: full selection ($AFFECTED_REASONS) runs lint and the quick lane; CI runs the full lanes" >&2
  else
    AFFECTED_MODE=full
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_FOCUSED=1
    RUN_SMOKE=1
    echo "affected: full test fallback (${AFFECTED_REASONS:-uncertain})" >&2
  fi
}

if [ "$RUN_QUICK" = 1 ] || [ "$RUN_AFFECTED" = 1 ]; then
  git rev-parse --verify "$QUICK_FROM^{tree}" >/dev/null 2>&1 || {
    echo "error: --changed-from is not a tree: $QUICK_FROM" >&2
    exit 2
  }
  git rev-parse --verify "$QUICK_TO^{tree}" >/dev/null 2>&1 || {
    echo "error: --changed-to is not a tree: $QUICK_TO" >&2
    exit 2
  }
  # Informational; a project copy without the helper still checks.
  if [ -f "$ROOT/scripts/lib/test-growth.sh" ]; then
    # shellcheck source=scripts/lib/test-growth.sh
    . "$ROOT/scripts/lib/test-growth.sh"
    echo "changed: $(oms_test_growth "$QUICK_FROM" "$QUICK_TO")" >&2
  fi
fi
if [ "$CI_PARITY" = 1 ] && [ "$LIST_STAGES" != 1 ]; then
  hide_provider_clis
fi
if [ "$RUN_AFFECTED" = 1 ]; then
  prepare_affected_gate
  if [ -n "$CI_OUTPUT" ]; then
    printf 'mode=%s\nnative=%s\n' "$AFFECTED_MODE" "$AFFECTED_NATIVE" >> "$CI_OUTPUT"
    if [ "$AFFECTED_MODE" = full ]; then
      verify_oms_state
      exit 0
    fi
  fi
  if [ "$PRINT_AFFECTED_MODE" = 1 ]; then
    printf '%s\n' "$AFFECTED_MODE"
    verify_oms_state
    exit 0
  fi
fi

if [ "$RUN_LINT" = 1 ]; then
  # scripts/oms is named explicitly: the dispatcher has no .sh extension, so
  # the glob alone would silently skip it.
  stage shellcheck lint_shell install.sh scripts/oms scripts/*.sh \
    scripts/lib/*.sh plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh

  stage bash-compat bash scripts/check-bash32.sh
  stage python-syntax bash scripts/check-python.sh

  stage skill-manifest bash scripts/install-skills.sh
fi

if [ "$RUN_INVARIANTS" = 1 ]; then
  stage invariants-smoke-tests bash tests/run-smoke-shard.sh \
    --only test_oms_allowlist_stays_in_sync_with_scripts \
    --only test_peer_review_warns_untracked_files \
    --only test_peer_review_gate_binds_complete_diff_and_refuses_truncation
  stage invariants-provider-registry bash -c '
    set -e
    . scripts/lib/provider-registry.sh
    for provider in $(oms_provider_supported_names); do oms_provider_is_builtin "$provider"; done
    for alias in agy cursor-agent grok-build gemini-cli qwen-code opencode2 dsh deepseek-harness mistral-vibe github-copilot factory-droid; do
      normalized="$(oms_provider_normalize "$alias")"
      oms_provider_is_builtin "$normalized"
    done
  '
  stage invariants-skill-manifest bash scripts/install-skills.sh
  stage invariants-doc-references bash tests/source-distribution-smoke.sh --docs-only
  stage invariants-changed-syntax bash -c '
    set -e
    if [ -n "${OMS_CHECK_INVARIANTS_BASE:-}" ]; then
      base="$OMS_CHECK_INVARIANTS_BASE"
    else
      base="$(git symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null || true)"
      if [ -z "$base" ]; then
        for candidate in refs/remotes/origin/main refs/remotes/origin/master; do
          if git show-ref --verify --quiet "$candidate"; then base="$candidate"; break; fi
        done
      fi
    fi
    [ -n "$base" ] || { echo "error: no remote target ref; set OMS_CHECK_INVARIANTS_BASE" >&2; exit 2; }
    git rev-parse --verify "$base^{tree}" >/dev/null 2>&1 || {
      echo "error: invariant base is not a tree: $base" >&2; exit 2;
    }
    changed="$(mktemp "${TMPDIR:-/tmp}/oms-invariants-files.XXXXXX")"
    trap "rm -f \"\$changed\"" EXIT HUP INT TERM
    {
      git diff --name-only --no-renames -z "$base" HEAD --
      git diff --name-only --no-renames --cached -z --
      git diff --name-only --no-renames -z --
      git ls-files --others --exclude-standard -z
    } > "$changed"
    shell_files=()
    python_files=()
    while IFS= read -r -d "" file; do
      [ -f "$file" ] || continue
      case "$file" in
        *.sh) shell_files[${#shell_files[@]}]="$file" ;;
        *.py) python_files[${#python_files[@]}]="$file" ;;
      esac
    done < "$changed"
    for file in "${shell_files[@]}"; do bash -n "$file"; done
    if [ "${#python_files[@]}" -gt 0 ]; then
      bash scripts/check-python.sh "${python_files[@]}"
    fi
  '
fi

if [ "$MODE" = lint ]; then
  verify_oms_state
  echo "check: ok (lint only)"
  exit 0
fi

if [ "$RUN_QUICK" = 1 ]; then
  changed_file="$CHECK_RUNTIME/changed-files"
  git -c core.quotePath=false diff -z --name-only --no-renames "$QUICK_FROM" "$QUICK_TO" -- > "$changed_file"
  quick_harness=0
  quick_models=0
  quick_seats=0
  quick_routing=0
  quick_skills=0
  quick_python=0
  quick_docs=1
  quick_shell_files=()
  quick_python_files=()
  while IFS= read -r -d '' path; do
    [ -n "$path" ] || continue
    case "$path" in
      README.md|README.ko.md|docs/*.md|custom-skills/*.md|templates/*.md) ;;
      *) quick_docs=0 ;;
    esac
    case "$path" in
      *.py)
        if [ -f "$path" ]; then
          quick_python=1
          quick_python_files[${#quick_python_files[@]}]="$path"
        fi
        ;;
    esac
    case "$path" in
      *.sh|scripts/oms)
        [ ! -f "$path" ] || quick_shell_files[${#quick_shell_files[@]}]="$path"
        ;;
    esac
    case "$path" in
      .github/workflows/*|scripts/check.sh|scripts/install-hooks.sh|scripts/pre-push-check.sh|tests/run-smoke-shard.sh|tests/harness-enhancements-smoke.sh|tests/source-distribution-smoke.sh)
        quick_harness=1
        ;;
    esac
    case "$path" in
      config/models.json|scripts/agent-call.sh|scripts/peer-ask.sh|scripts/lib/model-routing.sh|scripts/lib/provider-registry.sh|scripts/lib/peer-common.sh|tests/model-routing-smoke.sh|tests/provider-registry-smoke.sh)
        quick_models=1
        ;;
    esac
    case "$path" in
      scripts/lib/peer-common.sh|scripts/fail-ledger.sh|tests/seat-reliability-smoke.sh)
        quick_seats=1
        ;;
    esac
    # Tier routing picks among installed providers, so these are the inputs
    # that let a fixture pass only where a provider CLI exists (f698ac9).
    case "$path" in
      config/panel-routing.json|config/models.json|scripts/lib/provider-registry.sh|scripts/lib/panel_routing.py|scripts/lib/panel_view.py|scripts/lib/terminal_panel.py|tests/operator-tools-smoke.sh)
        quick_routing=1
        ;;
    esac
    case "$path" in
      custom-skills/*|templates/*.md|plugins/*|skills.manifest.json|scripts/install-skills.sh|scripts/validate-skills.py)
        quick_skills=1
        ;;
    esac
  done < "$changed_file"

  # A capped full selection already ran the whole lint above.
  quick_lint=0
  [ "$SKIP_LINT" = 1 ] || [ "$RUN_LINT" = 1 ] || quick_lint=1
  if [ "${#quick_shell_files[@]}" -gt 0 ] && [ "$quick_lint" = 1 ]; then
    command -v "$SHELLCHECK" >/dev/null 2>&1 || {
      echo "FATAL: shellcheck is required for changed shell files" >&2
      exit 1
    }
    stage shellcheck-changed lint_shell "${quick_shell_files[@]}"
    stage bash-compat-changed bash scripts/check-bash32.sh "${quick_shell_files[@]}"
  fi
  if [ "${#quick_shell_files[@]}" -gt 0 ] && [ "$SKIP_LINT" = 0 ] && [ "$CI_PARITY" = 1 ]; then
    parse_with_bash32 "${quick_shell_files[@]}"
  fi
  [ "$quick_lint" = 0 ] || [ "$quick_python" = 0 ] || stage python-syntax bash scripts/check-python.sh "${quick_python_files[@]}"
  [ "$quick_lint" = 0 ] || [ "$quick_skills" = 0 ] || stage skill-manifest bash scripts/install-skills.sh
  if [ "$quick_docs" = 1 ] || [ "$quick_harness" = 0 ]; then
    stage source-distribution bash tests/source-distribution-smoke.sh --docs-only
  else
    stage source-distribution bash tests/source-distribution-smoke.sh
  fi
  [ "$quick_harness" = 0 ] || stage codex-hud-config bash tests/codex-hud-config-smoke.sh
  [ "$quick_harness" = 0 ] || stage platform-portability bash tests/platform-portability-smoke.sh
  [ "$quick_harness" = 0 ] || stage bsd-portability bash tests/bsd-portability-smoke.sh
  [ "$quick_harness" = 0 ] || stage functional-evolution bash tests/functional-evolution-smoke.sh
  [ "$quick_harness" = 0 ] || stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  [ "$quick_models" = 0 ] || stage model-routing bash tests/model-routing-smoke.sh
  [ "$quick_models" = 0 ] || stage provider-registry bash tests/provider-registry-smoke.sh
  [ "$quick_models" = 0 ] || stage models-surface bash tests/models-smoke.sh
  [ "$quick_seats" = 0 ] || stage seat-reliability bash tests/seat-reliability-smoke.sh
  # Only a CI-parity gate hides provider CLIs; a capped full selection has no
  # graph selection to reach this suite otherwise.
  # Sharded like CI and the other two call sites; the single-process run is not a CI shape.
  [ "$CI_PARITY" = 0 ] || [ "$quick_routing" = 0 ] || stage operator-tools bash tests/operator-tools-smoke.sh --jobs "${OMS_OPERATOR_JOBS:-4}"
fi

affected_test_was_run_by_quick() {  # affected_test_was_run_by_quick PATH
  case "$1" in
    tests/source-distribution-smoke.sh) return 0 ;;
    tests/codex-hud-config-smoke.sh|tests/platform-portability-smoke.sh|tests/bsd-portability-smoke.sh|tests/functional-evolution-smoke.sh|tests/harness-enhancements-smoke.sh)
      [ "$quick_harness" = 1 ]; return ;;
    tests/model-routing-smoke.sh|tests/provider-registry-smoke.sh|tests/models-smoke.sh)
      [ "$quick_models" = 1 ]; return
      ;;
    tests/seat-reliability-smoke.sh) [ "$quick_seats" = 1 ]; return ;;
    tests/operator-tools-smoke.sh) [ "$CI_PARITY" = 1 ] && [ "$quick_routing" = 1 ]; return ;;
  esac
  return 1
}

if [ "$RUN_AFFECTED" = 1 ] && [ "$AFFECTED_MODE" = affected ]; then
  for affected_test in ${AFFECTED_TESTS[@]+"${AFFECTED_TESTS[@]}"}; do
    affected_test_was_run_by_quick "$affected_test" && continue
    if [ "$affected_test" = tests/scripts-smoke.sh ] || [ "$affected_test" = tests/autonomy-plan-run-smoke.sh ]; then
      affected_args=()
      affected_index=0
      while [ "$affected_index" -lt "${#AFFECTED_CASE_PATHS[@]}" ]; do
        if [ "${AFFECTED_CASE_LANGS[$affected_index]}" = shell ] && \
            [ "${AFFECTED_CASE_PATHS[$affected_index]}" = "$affected_test" ]; then
          affected_args[${#affected_args[@]}]=--only
          affected_args[${#affected_args[@]}]="${AFFECTED_CASE_SELECTORS[$affected_index]}"
        fi
        affected_index=$((affected_index + 1))
      done
      if [ "${#affected_args[@]}" -gt 0 ]; then
        stage affected-scripts-smoke env OMS_SMOKE_SUITE="$ROOT/$affected_test" bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}" "${affected_args[@]}"
      else
        stage affected-scripts-smoke env OMS_SMOKE_SUITE="$ROOT/$affected_test" bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"
      fi
    elif [ "$affected_test" = tests/operator-tools-smoke.sh ]; then
      stage operator-tools bash "$ROOT/tests/operator-tools-smoke.sh" --jobs "${OMS_OPERATOR_JOBS:-4}"
    elif [ "${affected_test%.py}" != "$affected_test" ]; then
      affected_args=()
      affected_index=0
      while [ "$affected_index" -lt "${#AFFECTED_CASE_PATHS[@]}" ]; do
        if [ "${AFFECTED_CASE_LANGS[$affected_index]}" = python ] && \
            [ "${AFFECTED_CASE_PATHS[$affected_index]}" = "$affected_test" ]; then
          affected_args[${#affected_args[@]}]=-k
          affected_args[${#affected_args[@]}]="${AFFECTED_CASE_SELECTORS[$affected_index]}"
        fi
        affected_index=$((affected_index + 1))
      done
      stage "affected:$affected_test" env \
        "PYTHONPATH=$ROOT/scripts/lib:$ROOT/tests${PYTHONPATH:+:$PYTHONPATH}" \
        python3 -m unittest "${affected_args[@]}" "$affected_test"
    else
      stage "affected:$affected_test" bash "$affected_test"
    fi
  done
fi

if [ "$RUN_FOCUSED" = 1 ]; then
  # Registration order decides the lanes. --focused-lane I/N takes every Nth
  # stage from here, so the order below is not thematic. It is a cardinality-
  # constrained longest-processing-time assignment over eight lanes (the local
  # parallel gate) whose lane pairs k and k+4 also balance CI's four lanes,
  # from the mean stage costs of five full CI runs on 2026-10-10..12
  # (operator-tools 447s, goal-drive-recovery-1 209s, autopilot 188s, draft-pr
  # 157s, goal-drive-recovery-2 147s, autonomy-plan-run 125s, graph 119s, the
  # rest under 65s): each CI lane carries about 572s (it was 410-839s) and the
  # longest local lane is operator-tools' own. Re-derive the order when a heavy
  # suite is added or one changes by minutes: assign by cost to the lightest
  # lane that still has a slot, swap to balance the k/k+4 pairs, then list
  # lane 1..8, 1..8, ... The Linux-only stages close the list, so the other
  # hosts keep the same lanes for every stage before them.
  stage graph bash tests/graph-smoke.sh
  stage supervisor bash tests/supervisor-smoke.sh
  stage draft-pr bash tests/draft-pr-smoke.sh
  stage autonomy-plan-run bash tests/autonomy-plan-run-smoke.sh
  stage autopilot bash tests/autopilot-smoke.sh
  stage operator-tools bash tests/operator-tools-smoke.sh --jobs "${OMS_OPERATOR_JOBS:-4}"
  stage goal-drive-recovery-2 env OMS_GDR_PART=2 bash tests/goal-drive-recovery-smoke.sh
  stage goal-drive-recovery-1 env OMS_GDR_PART=1 bash tests/goal-drive-recovery-smoke.sh
  stage atomic-state bash tests/atomic-state-smoke.sh
  stage provider-registry bash tests/provider-registry-smoke.sh
  stage patch-land-approval bash tests/patch-land-approval-smoke.sh
  stage runtime-core bash tests/runtime-core-smoke.sh
  stage land bash tests/land-smoke.sh
  stage artifact-supersession bash tests/artifact-supersession-smoke.sh
  stage autonomy-hook bash tests/autonomy-hook-smoke.sh
  stage work-journal bash tests/work-journal-smoke.sh
  stage tick bash tests/tick-smoke.sh
  stage patch-admit-verifier-floor bash tests/patch-admit-verifier-floor-smoke.sh
  stage state-surfaces bash tests/state-surfaces-smoke.sh
  stage model-routing bash tests/model-routing-smoke.sh
  stage durable-writers bash tests/durable-writers-contract-smoke.sh
  stage source-distribution bash tests/source-distribution-smoke.sh
  # The real install/update/uninstall lifecycle in a sandboxed HOME. It ran
  # CI-only until a rename shipped green locally and reddened all four
  # install-e2e legs on a stale catalog assertion: the one suite the local
  # gate could not see was the one that broke. 30s buys that back.
  stage install-lifecycle bash tests/install-lifecycle-smoke.sh
  stage debate-delta bash tests/debate-delta-smoke.sh
  stage lifecycle-events bash tests/lifecycle-events-smoke.sh
  stage doctor-model-capability bash tests/doctor-model-capability-smoke.sh
  stage update-v04 bash tests/update-v04-smoke.sh
  stage lifecycle-provider-integration bash tests/lifecycle-provider-integration-smoke.sh
  stage council-failure-symmetry bash tests/council-failure-symmetry-smoke.sh
  stage install-profile bash tests/install-profile-smoke.sh
  stage functional-evolution bash tests/functional-evolution-smoke.sh
  stage autonomy-verification bash tests/autonomy-verification-smoke.sh
  stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  stage failure-attention bash tests/failure-attention-smoke.sh
  stage model-doctor bash tests/model-doctor-smoke.sh
  stage read-time-expiry bash tests/read-time-expiry-smoke.sh
  stage lifecycle-hardening bash tests/lifecycle-hardening-smoke.sh
  stage codex-hud-config bash tests/codex-hud-config-smoke.sh
  stage ci-status bash tests/ci-status-smoke.sh
  stage platform-portability bash tests/platform-portability-smoke.sh
  stage doctor-surfaces bash tests/doctor-surfaces-smoke.sh
  stage self-advice-disclosure bash tests/self-advice-disclosure-smoke.sh
  stage prompt-budget bash tests/prompt-budget-smoke.sh
  stage fail-ledger-hook-filter bash tests/fail-ledger-hook-filter-smoke.sh
  stage skill-lifecycle bash tests/skill-lifecycle-smoke.sh
  stage context-core bash tests/context-core-smoke.sh
  stage advisor-session bash tests/advisor-session-smoke.sh
  stage runtime-core-integration bash tests/runtime-core-integration-smoke.sh
  stage install-lifecycle-lock bash tests/install-lifecycle-lock-smoke.sh
  stage patch-admit-structural bash tests/patch-admit-structural-smoke.sh
  stage turn-guard-fuse bash tests/turn-guard-fuse-smoke.sh
  stage autonomy-failure bash tests/autonomy-failure-smoke.sh
  stage provider-permissions-mcp-boundary bash tests/provider-permissions-mcp-boundary-smoke.sh
  stage panel-controls bash tests/panel-controls-smoke.sh
  stage windows-durable-writer bash tests/windows-durable-writer-smoke.sh
  stage advisor-routing bash tests/advisor-routing-smoke.sh
  stage file-lock-boundary bash tests/file-lock-boundary-smoke.sh
  stage tool-lock bash tests/tool-lock-smoke.sh
  stage harness-residue-boundary bash tests/harness-residue-boundary-smoke.sh
  stage models-surface bash tests/models-smoke.sh
  stage seat-reliability bash tests/seat-reliability-smoke.sh
  stage tsp-queue bash tests/tsp-queue-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
  # panel jobs need Linux subreaper+pidfd and a POSIX pty; other hosts keep the
  # core lifecycle without them rather than counting a missing native proof.
  if [ "$(uname -s 2>/dev/null)" = Linux ]; then
    stage terminal-control bash tests/terminal-control-smoke.sh
    stage panel-workspaces bash tests/panel-workspaces-smoke.sh
  fi
fi

if [ "$LIST_STAGES" = 1 ]; then
  RUN_SMOKE=0
fi
if [ "$RUN_SMOKE" = 1 ]; then
  if [ -n "$SMOKE_SHARD" ]; then
    OMS_SMOKE_TIMINGS="$CHECK_SMOKE_TIMINGS" \
      OMS_SMOKE_TIMING_LIMIT="$CHECK_SMOKE_TIMING_LIMIT" \
      stage scripts-smoke bash tests/run-smoke-shard.sh --shard "$SMOKE_SHARD"
  else
    OMS_SMOKE_TIMINGS="$CHECK_SMOKE_TIMINGS" \
      OMS_SMOKE_TIMING_LIMIT="$CHECK_SMOKE_TIMING_LIMIT" \
      stage scripts-smoke bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"
  fi
fi

if [ "$LIST_STAGES" = 1 ]; then
  # The listing IS the output; no verification ran, so no verdict line.
  exit 0
fi

verify_oms_state

case "$MODE" in
  focused) echo "check: ok (focused only)" ;;
  scripts-smoke) echo "check: ok (scripts-smoke only)" ;;
  invariants) echo "check: ok (invariants only; pre-land)" ;;
  quick) echo "check: ok (quick local checks; full CI gate required)" ;;
  affected)
    check_suffix=""
    [ "$SKIP_LINT" = 0 ] || check_suffix="; without lint"
    [ "$CI_PARITY" = 0 ] || check_suffix="$check_suffix; ci-parity"
    # oms land reads "mode=full" alone as install proof; a capped run is not.
    if [ "$AFFECTED_CAPPED" = 1 ]; then
      echo "check: ok (affected tests; mode=full capped to lint and quick lane$check_suffix)"
    else
      echo "check: ok (affected tests; mode=$AFFECTED_MODE$check_suffix)"
    fi ;;
  *) if [ "$SKIP_LINT" = 1 ]; then echo "check: ok (without lint)"; else echo "check: ok"; fi ;;
esac
