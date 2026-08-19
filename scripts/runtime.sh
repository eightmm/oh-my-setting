#!/usr/bin/env bash
set -euo pipefail

# Typed projections and optional execution services over the existing OMS plane.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/lib/oms_core.py" "$@"
