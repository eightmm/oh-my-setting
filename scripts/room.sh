#!/usr/bin/env bash
set -euo pipefail

# Shared work rooms and addressed messages on the existing OMS thread log.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v python3 >/dev/null 2>&1 || { echo 'error: python3 is required' >&2; exit 2; }
# shellcheck source=scripts/lib/pycache-env.sh
. "$ROOT/scripts/lib/pycache-env.sh"
oms_pycache_env "$ROOT"
exec python3 "$ROOT/scripts/lib/room.py" "$@"
