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

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Git exports GIT_DIR (and friends) to hooks. From a linked worktree that
# path is absolute, so it overrides repository discovery for every git call
# in every test fixture — `cd fixture && git add` then operates on THIS
# checkout, not the fixture. A pre-push from a worktree proved the leak by
# rewriting the checkout with fixture commits. The gate must be hermetic to
# the invoking git context.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE \
  GIT_OBJECT_DIRECTORY GIT_QUARANTINE_PATH GIT_PREFIX

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
  OMS_LANDING_ID OMS_WORKER_AUTHORITY_EXCLUSIVE

# Keep lock/capability caches out of the invoking user's HOME. Several focused
# suites intentionally create temporary repos but use shared lifecycle helpers;
# one check run should be hermetic without duplicating HOME setup in every file.
# The lock dir gets its own variable rather than riding on XDG_CACHE_HOME: locks
# must resolve identically from a desktop shell and from the auto-update timer,
# so the production path cannot key off an ambient XDG variable (file-lock.sh).
CHECK_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/oms-check-runtime.XXXXXX")"
CHECK_STATE_GUARD_ACTIVE=0
cleanup_check_runtime() {
  local rc=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ "$CHECK_STATE_GUARD_ACTIVE" = 1 ] && ! verify_oms_state; then
    rc=1
  fi
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
SKIP_LINT=0
SMOKE_SHARD=""
QUICK_FROM=""
QUICK_TO=""
FOCUSED_LANE=""
FOCUSED_LANE_I=0
FOCUSED_LANE_N=0
FOCUSED_STAGE_INDEX=0
LIST_STAGES=0
usage() {
  cat <<'EOF'
usage: check.sh [MODE] [OPTIONS]

Runs the repository gate: lint (shellcheck, Bash 3.2, Python syntax, skill
manifest) then the test suites. With no arguments it runs both.

  --no-lint             Skip lint in the otherwise complete local gate.
  --lint-only           Run only lint stages.
  --focused-only        Run only the focused test suites.
  --focused-lane I/N    Run only every Nth focused stage starting at the Ith
                        (with --focused-only). The partition is positional,
                        so a newly registered stage lands in a lane
                        automatically and the lane union is always the whole
                        focused list.
  --scripts-smoke-only  Run only tests/scripts-smoke.sh.
  --shard I/N           Select one scripts-smoke shard (with the mode above).
  --list-stages         Print the stage names the selected mode and lane
                        would run, one per line, without running anything.
                        (The scripts-smoke shards run outside the stage list
                        and are not included.)
  --quick               Run a partial changed-file gate for protected pushes.
  --changed-from REF    Base tree for --quick.
  --changed-to REF      Target tree for --quick.
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
    --no-lint) SKIP_LINT=1; shift ;;
    --lint-only) select_mode lint; shift ;;
    --focused-only) select_mode focused; shift ;;
    --scripts-smoke-only) select_mode scripts-smoke; shift ;;
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

if [ "$MODE" = lint ] && [ "$SKIP_LINT" = 1 ]; then
  echo "error: --no-lint and --lint-only leave nothing to run" >&2
  exit 2
fi
if [ "$SKIP_LINT" = 1 ] && [ "$MODE" != full ] && [ "$MODE" != lint ]; then
  echo "error: --no-lint only modifies the default complete gate" >&2
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
if [ "$MODE" = quick ]; then
  [ -n "$QUICK_FROM" ] && [ -n "$QUICK_TO" ] || {
    echo "error: --quick requires --changed-from and --changed-to" >&2
    exit 2
  }
elif [ -n "$QUICK_FROM" ] || [ -n "$QUICK_TO" ]; then
  echo "error: --changed-from/--changed-to require --quick" >&2
  exit 2
fi

RUN_LINT=0
RUN_FOCUSED=0
RUN_SMOKE=0
RUN_QUICK=0
case "$MODE" in
  full)
    [ "$SKIP_LINT" = 1 ] || RUN_LINT=1
    RUN_FOCUSED=1
    RUN_SMOKE=1
    ;;
  lint) RUN_LINT=1 ;;
  focused) RUN_FOCUSED=1 ;;
  scripts-smoke) RUN_SMOKE=1 ;;
  quick) RUN_QUICK=1 ;;
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

# One line per stage on success, full output on failure: the gate is read by an
# agent between edits, and a green run has nothing to say beyond "ok".
stage() {  # stage NAME COMMAND...
  local name="$1"
  local log
  local rc=0
  local started elapsed
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
  "$@" > "$log" 2>&1 || rc=$?
  elapsed=$(( $(date +%s) - started ))
  if [ "${OMS_VERBOSE:-0}" = "1" ]; then
    cat "$log"
    rm -f "$log"
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
      rm -f "$log"
    fi
  else
    rm -f "$log"
  fi
  if [ "$rc" -ne 0 ]; then
    echo "check: $name FAILED (${elapsed}s)" >&2
    exit "$rc"
  fi
  echo "ok: $name (${elapsed}s)"
}

if [ "$RUN_LINT" = 1 ]; then
  # scripts/oms is named explicitly: the dispatcher has no .sh extension, so
  # the glob alone would silently skip it.
  stage shellcheck lint_shell install.sh scripts/oms scripts/*.sh \
    scripts/lib/*.sh plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh

  stage bash-compat bash scripts/check-bash32.sh
  stage python-syntax bash scripts/check-python.sh

  stage skill-manifest bash scripts/install-skills.sh
fi

if [ "$MODE" = lint ]; then
  verify_oms_state
  echo "check: ok (lint only)"
  exit 0
fi

if [ "$RUN_QUICK" = 1 ]; then
  git rev-parse --verify "$QUICK_FROM^{tree}" >/dev/null 2>&1 || {
    echo "error: --changed-from is not a tree: $QUICK_FROM" >&2
    exit 2
  }
  git rev-parse --verify "$QUICK_TO^{tree}" >/dev/null 2>&1 || {
    echo "error: --changed-to is not a tree: $QUICK_TO" >&2
    exit 2
  }

  changed_file="$CHECK_RUNTIME/changed-files"
  git diff --name-only --diff-filter=ACMR "$QUICK_FROM" "$QUICK_TO" -- > "$changed_file"
  quick_harness=0
  quick_models=0
  quick_seats=0
  quick_skills=0
  quick_shell_files=()
  while IFS= read -r path; do
    [ -n "$path" ] || continue
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
    case "$path" in
      custom-skills/*|plugins/*|skills.manifest.json|scripts/install-skills.sh|scripts/validate-skills.py)
        quick_skills=1
        ;;
    esac
  done < "$changed_file"

  if [ "${#quick_shell_files[@]}" -gt 0 ]; then
    command -v "$SHELLCHECK" >/dev/null 2>&1 || {
      echo "FATAL: shellcheck is required for changed shell files" >&2
      exit 1
    }
    stage shellcheck-changed lint_shell "${quick_shell_files[@]}"
    stage bash-compat-changed bash scripts/check-bash32.sh "${quick_shell_files[@]}"
  fi
  stage python-syntax bash scripts/check-python.sh
  stage codex-hud-config bash tests/codex-hud-config-smoke.sh
  [ "$quick_skills" = 0 ] || stage skill-manifest bash scripts/install-skills.sh
  stage source-distribution bash tests/source-distribution-smoke.sh
  stage platform-portability bash tests/platform-portability-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
  stage functional-evolution bash tests/functional-evolution-smoke.sh
  [ "$quick_harness" = 0 ] || stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  [ "$quick_models" = 0 ] || stage model-routing bash tests/model-routing-smoke.sh
  [ "$quick_models" = 0 ] || stage provider-registry bash tests/provider-registry-smoke.sh
  [ "$quick_models" = 0 ] || stage models-surface bash tests/models-smoke.sh
  [ "$quick_seats" = 0 ] || stage seat-reliability bash tests/seat-reliability-smoke.sh
fi

if [ "$RUN_FOCUSED" = 1 ]; then
  stage autonomy-hook bash tests/autonomy-hook-smoke.sh
  stage autonomy-verification bash tests/autonomy-verification-smoke.sh
  stage autonomy-failure bash tests/autonomy-failure-smoke.sh
  stage autonomy-plan-run bash tests/autonomy-plan-run-smoke.sh
  stage autopilot bash tests/autopilot-smoke.sh
  stage draft-pr bash tests/draft-pr-smoke.sh
  stage goal-drive-recovery bash tests/goal-drive-recovery-smoke.sh
  stage model-routing bash tests/model-routing-smoke.sh
  stage provider-registry bash tests/provider-registry-smoke.sh
  stage execution-profile bash tests/execution-profile-smoke.sh
  stage herdr-adapter bash tests/herdr-adapter-smoke.sh
  stage codex-hud-config bash tests/codex-hud-config-smoke.sh
  stage models-surface bash tests/models-smoke.sh
  stage model-doctor bash tests/model-doctor-smoke.sh
  stage doctor-model-capability bash tests/doctor-model-capability-smoke.sh
  stage update-v04 bash tests/update-v04-smoke.sh
  stage state-surfaces bash tests/state-surfaces-smoke.sh
  stage operator-tools bash tests/operator-tools-smoke.sh
  stage skill-lifecycle bash tests/skill-lifecycle-smoke.sh
  stage interoperability bash tests/interoperability-smoke.sh
  stage runtime-core bash tests/runtime-core-smoke.sh
  stage runtime-core-integration bash tests/runtime-core-integration-smoke.sh
  stage graph bash tests/graph-smoke.sh
  stage install-profile bash tests/install-profile-smoke.sh
  stage functional-evolution bash tests/functional-evolution-smoke.sh
  stage lifecycle-hardening bash tests/lifecycle-hardening-smoke.sh
  stage lifecycle-events bash tests/lifecycle-events-smoke.sh
  stage supervisor bash tests/supervisor-smoke.sh
  stage lifecycle-provider-integration bash tests/lifecycle-provider-integration-smoke.sh
  stage install-lifecycle-lock bash tests/install-lifecycle-lock-smoke.sh
  # The real install/update/uninstall lifecycle in a sandboxed HOME. It ran
  # CI-only until a rename shipped green locally and reddened all four
  # install-e2e legs on a stale catalog assertion: the one suite the local
  # gate could not see was the one that broke. 30s buys that back.
  stage install-lifecycle bash tests/install-lifecycle-smoke.sh
  stage file-lock-boundary bash tests/file-lock-boundary-smoke.sh
  stage harness-residue-boundary bash tests/harness-residue-boundary-smoke.sh
  stage tool-lock bash tests/tool-lock-smoke.sh
  stage provider-permissions-mcp-boundary bash tests/provider-permissions-mcp-boundary-smoke.sh
  stage atomic-state bash tests/atomic-state-smoke.sh
  stage advisor-routing bash tests/advisor-routing-smoke.sh
  stage advisor-session bash tests/advisor-session-smoke.sh
  stage debate-delta bash tests/debate-delta-smoke.sh
  stage failure-attention bash tests/failure-attention-smoke.sh
  stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  stage tsp-queue bash tests/tsp-queue-smoke.sh
  stage work-journal bash tests/work-journal-smoke.sh
  stage durable-writers bash tests/durable-writers-contract-smoke.sh
  stage windows-durable-writer bash tests/windows-durable-writer-smoke.sh
  stage context-core bash tests/context-core-smoke.sh
  stage prompt-budget bash tests/prompt-budget-smoke.sh
  stage source-distribution bash tests/source-distribution-smoke.sh
  stage platform-portability bash tests/platform-portability-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
  stage turn-guard-fuse bash tests/turn-guard-fuse-smoke.sh
  stage fail-ledger-hook-filter bash tests/fail-ledger-hook-filter-smoke.sh
  stage self-advice-disclosure bash tests/self-advice-disclosure-smoke.sh
  stage patch-admit-structural bash tests/patch-admit-structural-smoke.sh
  stage patch-admit-verifier-floor bash tests/patch-admit-verifier-floor-smoke.sh
  stage patch-land-approval bash tests/patch-land-approval-smoke.sh
  stage ci-status bash tests/ci-status-smoke.sh
  stage read-time-expiry bash tests/read-time-expiry-smoke.sh
  stage council-failure-symmetry bash tests/council-failure-symmetry-smoke.sh
  stage seat-reliability bash tests/seat-reliability-smoke.sh
  stage doctor-surfaces bash tests/doctor-surfaces-smoke.sh
  stage artifact-supersession bash tests/artifact-supersession-smoke.sh
fi

if [ "$LIST_STAGES" = 1 ]; then
  RUN_SMOKE=0
fi
if [ "$RUN_SMOKE" = 1 ]; then
  if [ -n "$SMOKE_SHARD" ]; then
    OMS_SMOKE_TIMINGS="$CHECK_SMOKE_TIMINGS" \
      OMS_SMOKE_TIMING_LIMIT="$CHECK_SMOKE_TIMING_LIMIT" \
      bash tests/run-smoke-shard.sh --shard "$SMOKE_SHARD"
  else
    OMS_SMOKE_TIMINGS="$CHECK_SMOKE_TIMINGS" \
      OMS_SMOKE_TIMING_LIMIT="$CHECK_SMOKE_TIMING_LIMIT" \
      bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"
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
  quick) echo "check: ok (quick local checks; full CI gate required)" ;;
  *) echo "check: ok" ;;
esac
