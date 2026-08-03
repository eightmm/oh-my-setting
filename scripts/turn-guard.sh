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

# The journal must not mirror a blocked answer as finished. Parse the guard
# verdict instead of substring-matching its serialization, so a formatting
# change in the emitter cannot silently turn blocked turns into finished ones;
# malformed output fails open, exactly like the guard itself.
decision="$(printf '%s' "$guard_out" | python3 -c '
import json, sys
raw = sys.stdin.read().strip()
if raw:
    try:
        verdict = json.loads(raw)
    except ValueError:
        verdict = None
    if isinstance(verdict, dict):
        print(verdict.get("decision", ""))
' 2>/dev/null || true)"
[ "$decision" != "block" ] || exit 0

[ "${OMS_HARNESS_CHILD:-0}" != "1" ] || exit 0
repo="$(printf '%s' "$payload" | python3 "$HELPER" repo 2>/dev/null || true)"
[ -n "$repo" ] || exit 0
git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || exit 0

# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"
work_journal_finish "$repo"

# A push near the end of a turn may not have a result yet. One bounded tick
# records it if already available; otherwise the next prompt retries after the
# throttle window. Never delay or fail Stop beyond the tick's own small budget.
if [ "${OMS_CI_TICK:-1}" = 1 ] &&
  git -C "$repo" remote get-url origin 2>/dev/null | grep -Eq 'github\.com[:/]'; then
  (cd "$repo" && OMS_CI_TICK_QUIET=1 \
    bash "$ROOT/scripts/ci-status.sh" tick) >/dev/null 2>&1 || true
fi
exit 0
