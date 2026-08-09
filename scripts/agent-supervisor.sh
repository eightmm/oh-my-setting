#!/usr/bin/env bash
set -euo pipefail

# Queue and supervise bounded attempts without landing, committing, or pushing.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/lib/attempt-runner.py" "$@"
