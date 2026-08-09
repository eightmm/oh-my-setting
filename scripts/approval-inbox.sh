#!/usr/bin/env bash
set -euo pipefail

# Request, decide, and consume durable approvals with compare-and-set fencing.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/lib/agent-events.py" approvals "$@"
