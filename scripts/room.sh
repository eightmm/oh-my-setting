#!/usr/bin/env bash
set -euo pipefail

# Shared work rooms and addressed messages on the existing OMS thread log.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || { echo 'error: python3 is required' >&2; exit 2; }
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$ROOT/scripts/lib/room.py" "$@"
