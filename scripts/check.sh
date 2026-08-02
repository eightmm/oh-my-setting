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

# Keep lock/capability caches out of the invoking user's HOME. Several focused
# suites intentionally create temporary repos but use shared lifecycle helpers;
# one check run should be hermetic without duplicating HOME setup in every file.
# The lock dir gets its own variable rather than riding on XDG_CACHE_HOME: locks
# must resolve identically from a desktop shell and from the auto-update timer,
# so the production path cannot key off an ambient XDG variable (file-lock.sh).
CHECK_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/oms-check-runtime.XXXXXX")"
trap 'rm -rf "$CHECK_RUNTIME"' EXIT HUP INT TERM
export XDG_CACHE_HOME="${OMS_CHECK_CACHE_HOME:-$CHECK_RUNTIME/cache}"
export OMS_LOCK_DIR="${OMS_CHECK_LOCK_DIR:-$CHECK_RUNTIME/locks}"
mkdir -p "$XDG_CACHE_HOME" "$OMS_LOCK_DIR"

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
usage() {
  cat <<'EOF'
usage: check.sh [MODE] [OPTIONS]

Runs the repository gate: lint (shellcheck, Bash 3.2, Python syntax, skill
manifest) then the test suites. With no arguments it runs both.

  --no-lint             Skip lint in the otherwise complete local gate.
  --lint-only           Run only lint stages.
  --focused-only        Run only the focused test suites.
  --scripts-smoke-only  Run only tests/scripts-smoke.sh.
  --shard I/N           Select one scripts-smoke shard (with the mode above).
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
if [ "$RUN_LINT" = 1 ] && ! command -v "$SHELLCHECK" >/dev/null 2>&1; then
  echo "FATAL: shellcheck is not installed — CI enforces it, so passing here" >&2
  echo "would be false confidence. Install one of:" >&2
  echo "  apt-get install shellcheck   |   brew install shellcheck" >&2
  echo "  or a static binary from https://github.com/koalaman/shellcheck/releases" >&2
  exit 1
fi

# One line per stage on success, full output on failure: the gate is read by an
# agent between edits, and a green run has nothing to say beyond "ok".
stage() {  # stage NAME COMMAND...
  local name="$1"
  local log
  local rc=0
  local started elapsed
  shift

  log="$(mktemp "${TMPDIR:-/tmp}/oms-check.XXXXXX")"
  started="$(date +%s)"
  "$@" > "$log" 2>&1 || rc=$?
  elapsed=$(( $(date +%s) - started ))
  if [ "$rc" -ne 0 ] || [ "${OMS_VERBOSE:-0}" = "1" ]; then
    cat "$log"
  fi
  rm -f "$log"
  if [ "$rc" -ne 0 ]; then
    echo "check: $name FAILED (${elapsed}s)" >&2
    exit "$rc"
  fi
  echo "ok: $name (${elapsed}s)"
}

if [ "$RUN_LINT" = 1 ]; then
  # scripts/oms is named explicitly: the dispatcher has no .sh extension, so
  # the glob alone would silently skip it.
  stage shellcheck "$SHELLCHECK" -x -S warning install.sh scripts/oms scripts/*.sh \
    scripts/lib/*.sh plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh

  stage bash-compat bash scripts/check-bash32.sh
  stage python-syntax bash scripts/check-python.sh

  stage skill-manifest bash scripts/install-skills.sh
fi

if [ "$MODE" = lint ]; then
  echo "check: ok (lint only)"
  exit 0
fi

# No suite may write into this checkout's own .oms state. A test that forgets to
# pass --repo defaults to the working directory, and one that did quietly filed
# three council artifacts here on every gate run for weeks. scripts-smoke has its
# own leak check, but it only covers that file — the offender was in a different
# suite, so the guard belongs at the gate.
#
# hooks/ is excluded: the live agent session running this gate appends its own
# events there mid-run, which is the session's business, not a suite's.
oms_state_fingerprint() {
  [ -d "$ROOT/.oms" ] || { printf 'absent\n'; return 0; }
  (cd "$ROOT/.oms" && find . -path ./hooks -prune -o -type f -print 2>/dev/null |
    LC_ALL=C sort | cksum)
}
STATE_BEFORE="$(oms_state_fingerprint)"

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
      config/models.json|scripts/agent-call.sh|scripts/peer-ask.sh|scripts/lib/model-routing.sh|scripts/lib/peer-common.sh|tests/model-routing-smoke.sh)
        quick_models=1
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
    stage shellcheck-changed "$SHELLCHECK" -x -S warning "${quick_shell_files[@]}"
    stage bash-compat-changed bash scripts/check-bash32.sh "${quick_shell_files[@]}"
  fi
  stage python-syntax bash scripts/check-python.sh
  [ "$quick_skills" = 0 ] || stage skill-manifest bash scripts/install-skills.sh
  stage source-distribution bash tests/source-distribution-smoke.sh
  stage platform-portability bash tests/platform-portability-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
  [ "$quick_harness" = 0 ] || stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  [ "$quick_models" = 0 ] || stage model-routing bash tests/model-routing-smoke.sh
fi

if [ "$RUN_FOCUSED" = 1 ]; then
  stage autonomy-hook bash tests/autonomy-hook-smoke.sh
  stage autonomy-verification bash tests/autonomy-verification-smoke.sh
  stage autonomy-failure bash tests/autonomy-failure-smoke.sh
  stage autonomy-plan-run bash tests/autonomy-plan-run-smoke.sh
  stage model-routing bash tests/model-routing-smoke.sh
  stage model-doctor bash tests/model-doctor-smoke.sh
  stage doctor-model-capability bash tests/doctor-model-capability-smoke.sh
  stage update-v04 bash tests/update-v04-smoke.sh
  stage state-surfaces bash tests/state-surfaces-smoke.sh
  stage lifecycle-hardening bash tests/lifecycle-hardening-smoke.sh
  stage atomic-state bash tests/atomic-state-smoke.sh
  stage advisor-routing bash tests/advisor-routing-smoke.sh
  stage harness-enhancements bash tests/harness-enhancements-smoke.sh
  stage work-journal bash tests/work-journal-smoke.sh
  stage context-core bash tests/context-core-smoke.sh
  stage prompt-budget bash tests/prompt-budget-smoke.sh
  stage source-distribution bash tests/source-distribution-smoke.sh
  stage platform-portability bash tests/platform-portability-smoke.sh
  stage bsd-portability bash tests/bsd-portability-smoke.sh
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

if [ "$(oms_state_fingerprint)" != "$STATE_BEFORE" ]; then
  echo "check: a suite wrote into this checkout's .oms state" >&2
  echo "a test that omits --repo defaults to the working directory; give it a" >&2
  echo "temporary repo instead. Inspect with: git status --short .oms" >&2
  exit 1
fi

case "$MODE" in
  focused) echo "check: ok (focused only)" ;;
  scripts-smoke) echo "check: ok (scripts-smoke only)" ;;
  quick) echo "check: ok (quick local checks; full CI gate required)" ;;
  *) echo "check: ok" ;;
esac
