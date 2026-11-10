#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
export PYTHONDONTWRITEBYTECODE=1
export PYTHONPATH="$ROOT/scripts/lib${PYTHONPATH:+:$PYTHONPATH}"
exec python3 -m oms_sessions.cli "$@"
