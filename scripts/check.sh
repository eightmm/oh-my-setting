#!/usr/bin/env bash
set -euo pipefail

# Repo verification gate — the SAME checks CI runs, in one command, so a local
# pass means CI will pass. Crucially, a missing tool is a HARD FAILURE, never a
# silent skip: a skipped shellcheck is exactly what let a whole session of red
# CI go unnoticed. Wire it as a pre-push hook with scripts/install-hooks.sh.

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
"$SHELLCHECK" -x -S warning install.sh scripts/oms scripts/*.sh tests/*.sh

echo "== smoke =="
bash tests/scripts-smoke.sh

echo "check: ok"
