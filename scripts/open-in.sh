#!/usr/bin/env bash
set -euo pipefail

# Open a repository file in a probed editor, or resume a Codex task; --dry-run/--json never launch it.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
exec python3 "$ROOT/scripts/lib/open-in.py" "$@"
