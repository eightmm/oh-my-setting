#!/usr/bin/env bash
set -euo pipefail

# Summarize current OMS lifecycle, review, approval, and telemetry metadata without mutating the repository.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required" >&2
  exit 2
}
exec python3 "$ROOT/scripts/lib/ops-cockpit.py" "$@"
