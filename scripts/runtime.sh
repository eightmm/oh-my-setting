#!/usr/bin/env bash
set -euo pipefail

# Typed projections and optional execution services over the existing OMS plane.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/pycache-env.sh
. "$ROOT/scripts/lib/pycache-env.sh"
oms_pycache_env "$ROOT"
exec python3 "$ROOT/scripts/lib/oms_core.py" "$@"
