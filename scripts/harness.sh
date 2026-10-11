#!/usr/bin/env bash
set -euo pipefail

# Run Codex models with the Codex, Pi, or Claude Code harness and existing ChatGPT login.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$ROOT/scripts/lib/codex_harness.py" "$@"
