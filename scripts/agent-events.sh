#!/usr/bin/env bash
set -euo pipefail

# Append, follow, validate, and project the normalized agent lifecycle stream.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/lib/agent-events.py" events "$@"
