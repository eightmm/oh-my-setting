#!/usr/bin/env bash
set -euo pipefail

# Stop hook guard: fail-open unless a high-risk/task turn with repo changes
# omits any verification status in the final answer.

[ "${OMS_TURN_GUARD_OFF:-0}" = "1" ] && exit 0

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/lib/hook_state.py"

[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 "$HELPER" guard || exit 0
