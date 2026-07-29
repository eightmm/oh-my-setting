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

# CI splits the gate into two jobs so a lint failure cannot mask every test
# result: stage() exits on the first failure, so one shellcheck nit would
# otherwise suppress the entire smoke report. Both halves run by default, so a
# local run and the pre-push hook still execute the whole gate.
#
# Flags, not environment variables. An `OMS_CHECK_LINT=0 bash scripts/check.sh`
# prefix exports into every descendant, so the smoke suite inherited it and two
# tests that run this gate themselves changed behaviour: one asked for a gate
# that runs nothing and was refused, and the other skipped the missing-shellcheck
# check and recursed through every suite instead of failing fast. A flag reaches
# only the process it is passed to.
RUN_LINT=1
RUN_TESTS=1
usage() {
  cat <<'EOF'
usage: check.sh [--no-lint | --lint-only]

Runs the repository gate: lint (shellcheck, Bash 3.2, Python syntax, skill
manifest) then the test suites. With no arguments it runs both.

  --no-lint     Skip the lint stages (CI runs them as their own job).
  --lint-only   Run only the lint stages.
EOF
}
while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-lint) RUN_LINT=0 ;;
    --lint-only) RUN_TESTS=0 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done
if [ "$RUN_LINT" = 0 ] && [ "$RUN_TESTS" = 0 ]; then
  echo "error: --no-lint and --lint-only leave nothing to run" >&2
  exit 2
fi

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
  shift

  log="$(mktemp "${TMPDIR:-/tmp}/oms-check.XXXXXX")"
  "$@" > "$log" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ] || [ "${OMS_VERBOSE:-0}" = "1" ]; then
    cat "$log"
  fi
  rm -f "$log"
  if [ "$rc" -ne 0 ]; then
    echo "check: $name FAILED" >&2
    exit "$rc"
  fi
  echo "ok: $name"
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

if [ "$RUN_TESTS" != 1 ]; then
  echo "check: ok (lint only)"
  exit 0
fi

stage autonomy-hook bash tests/autonomy-hook-smoke.sh
stage autonomy-verification bash tests/autonomy-verification-smoke.sh
stage autonomy-failure bash tests/autonomy-failure-smoke.sh
stage autonomy-plan-run bash tests/autonomy-plan-run-smoke.sh
stage model-routing bash tests/model-routing-smoke.sh
stage model-doctor bash tests/model-doctor-smoke.sh
stage doctor-model-capability bash tests/doctor-model-capability-smoke.sh
stage update-v04 bash tests/update-v04-smoke.sh
stage lifecycle-hardening bash tests/lifecycle-hardening-smoke.sh
stage harness-enhancements bash tests/harness-enhancements-smoke.sh
stage context-core bash tests/context-core-smoke.sh
stage prompt-budget bash tests/prompt-budget-smoke.sh
stage source-distribution bash tests/source-distribution-smoke.sh
stage platform-portability bash tests/platform-portability-smoke.sh
stage bsd-portability bash tests/bsd-portability-smoke.sh
bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"

echo "check: ok"
