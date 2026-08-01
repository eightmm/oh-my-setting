#!/usr/bin/env bash
set -euo pipefail

# Stop hook guard plus the Work Journal finish boundary. Both fail open; a
# blocked high-risk answer is not considered finished and therefore is not
# mirrored until the corrected Stop delivery arrives.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/lib/hook_state.py"

[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat)"
guard_out=""
if [ "${OMS_TURN_GUARD_OFF:-0}" != "1" ]; then
  guard_out="$(printf '%s' "$payload" | python3 "$HELPER" guard 2>/dev/null || true)"
  [ -z "$guard_out" ] || printf '%s\n' "$guard_out"
fi

case "$guard_out" in
  *'"decision": "block"'*) exit 0 ;;
esac

[ "${OMS_HARNESS_CHILD:-0}" != "1" ] || exit 0
repo="$(printf '%s' "$payload" | python3 "$HELPER" repo 2>/dev/null || true)"
[ -n "$repo" ] || exit 0
git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || exit 0

# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"
work_journal_finish "$repo"
exit 0
