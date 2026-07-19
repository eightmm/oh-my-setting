#!/usr/bin/env bash
set -euo pipefail

# Core repository gate shared with CI. CI additionally runs the real install
# lifecycle and macOS portability fixtures. A missing tool is a HARD FAILURE,
# never a silent skip. Wire this as a pre-push hook with scripts/install-hooks.sh.

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

echo "== shellcheck =="
# scripts/oms is named explicitly: the dispatcher has no .sh extension, so
# the glob alone would silently skip it.
"$SHELLCHECK" -x -S warning install.sh scripts/oms scripts/*.sh scripts/lib/*.sh \
  plugins/oh-my-setting/scripts/*.sh templates/*.sh tests/*.sh

echo "== bash 3.2 static compatibility =="
bash scripts/check-bash32.sh

echo "== smoke =="
bash tests/autonomy-hook-smoke.sh
bash tests/autonomy-verification-smoke.sh
bash tests/autonomy-failure-smoke.sh
bash tests/autonomy-plan-run-smoke.sh
bash tests/model-routing-smoke.sh
bash tests/model-doctor-smoke.sh
bash tests/update-v04-smoke.sh
bash tests/lifecycle-hardening-smoke.sh
bash tests/harness-enhancements-smoke.sh
bash tests/context-core-smoke.sh
bash tests/prompt-budget-smoke.sh
bash tests/source-distribution-smoke.sh
bash tests/run-smoke-shard.sh --jobs "${OMS_SMOKE_JOBS:-4}"

echo "check: ok"
