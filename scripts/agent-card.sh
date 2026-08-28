#!/usr/bin/env bash
set -euo pipefail

# Print the public, read-only A2A v1 Agent Card for the optional local bridge.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec python3 "$ROOT/scripts/lib/a2a-readonly.py" card "$@"
