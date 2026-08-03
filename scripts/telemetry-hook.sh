#!/usr/bin/env bash
set -euo pipefail

# Content-free lifecycle telemetry hook shared by Codex and Claude Code. The
# helper keeps only bounded identifiers, counters, timings, and usage; prompts,
# commands, tool arguments, and tool output never enter shared state. Fail-open:
# observability must not block an agent lifecycle event.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/lib/hook_state.py"

[ "${OMS_TELEMETRY_HOOK:-1}" = 1 ] || exit 0
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || exit 0
[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 "$HELPER" telemetry >/dev/null 2>&1 || true
exit 0
