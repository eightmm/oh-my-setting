#!/usr/bin/env bash
set -euo pipefail

# Open the terminal control panel for Codex and Claude Code, using existing OMS state and peer workflows.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$ROOT/scripts/lib/terminal_panel.py" "$@"
