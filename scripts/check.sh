#!/usr/bin/env bash
set -euo pipefail

# Core repository gate shared with CI, which additionally runs the real install
# lifecycle and macOS portability fixtures. A missing tool is a HARD FAILURE,
# never a silent skip. Wire this as a pre-push hook with scripts/install-hooks.sh.
#
# A passing run reports one line per stage: this gate is read by an agent
# between edits, and a green run has nothing to say beyond "ok". OMS_VERBOSE=1
# prints everything, and a failing stage always prints its own output.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Binary name is overridable so tests can exercise the missing-tool path
# deterministically without PATH surgery.
SHELLCHECK="${OMS_SHELLCHECK_BIN:-shellcheck}"
if ! command -v "$SHELLCHECK" >/dev/null 2>&1; then
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

# scripts/oms is named explicitly: the dispatcher has no .sh extension, so
# the glob alone would silently skip it.
stage shellcheck "$SHELLCHECK" -x -S warning install.sh scripts/oms scripts/*.sh \
  scripts/lib/*.sh plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh

stage bash-3.2 bash scripts/check-bash32.sh

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
bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"

echo "check: ok"
