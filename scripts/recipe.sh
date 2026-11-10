#!/usr/bin/env bash
set -euo pipefail

# Explicit local recipes, workspace actions, extension packs and requests.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$ROOT/scripts/lib/oms_recipes/__main__.py" "$@"
