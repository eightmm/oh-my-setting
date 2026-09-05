#!/usr/bin/env bash
set -euo pipefail

# Stop hook budgets plus the Work Journal finish boundary. Answer-format
# blocking is opt-in (OMS_TURN_GUARD_MAX_BLOCKS_PER_TURN=1). A
# blocked answer is not recorded as finished until the corrected
# Stop delivery arrives. Failing open is not the
# same as saying nothing: a turn that reached no verdict is announced, so an
# unguarded turn never passes for a guarded one.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="$ROOT/scripts/lib/hook_state.py"

[ -f "$HELPER" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat)"
guard_out=""
guard_rc=0
if [ "${OMS_TURN_GUARD_OFF:-0}" != "1" ]; then
  guard_out="$(printf '%s' "$payload" | python3 "$HELPER" guard 2>/dev/null)" || guard_rc=$?
fi

# The journal must not record a blocked answer as finished. Parse the guard
# verdict instead of substring-matching its serialization, so a formatting
# change in the emitter cannot silently turn blocked turns into finished ones;
# malformed output fails open, exactly like the guard itself. A parsed verdict
# is still forwarded as the helper's own bytes: the block protocol belongs to
# the emitter, not to this reader.
decision=""
unguarded=""
if [ "$guard_rc" -ne 0 ]; then
  unguarded="helper exit $guard_rc"
elif [ -n "$guard_out" ]; then
  verdict="$(printf '%s' "$guard_out" | python3 -c '
import json, sys
raw = sys.stdin.read().strip()
try:
    parsed = json.loads(raw)
except ValueError:
    parsed = None
if isinstance(parsed, dict):
    print("ok:" + str(parsed.get("decision", "")))
else:
    print("invalid")
' 2>/dev/null || true)"
  case "$verdict" in
  ok:*)
    decision="${verdict#ok:}"
    printf '%s\n' "$guard_out"
    ;;
  *) unguarded="unreadable verdict" ;;
  esac
fi

# Fail-open is deliberate, silence is not: a turn the guard never judged says
# so, on the one channel the Stop protocol reserves for notices. Exclusive with
# the verdict above, so stdout never carries two JSON documents.
[ -z "$unguarded" ] ||
  printf '{"systemMessage": "oh-my-setting turn guard: unavailable (%s); this turn was not checked."}\n' \
    "$unguarded"

[ "$decision" != "block" ] || exit 0

[ "${OMS_HARNESS_CHILD:-0}" != "1" ] || exit 0
repo="$(printf '%s' "$payload" | python3 "$HELPER" repo 2>/dev/null || true)"
[ -n "$repo" ] || exit 0
git -C "$repo" rev-parse --git-dir >/dev/null 2>&1 || exit 0

# shellcheck source=scripts/lib/work-journal.sh
. "$ROOT/scripts/lib/work-journal.sh"
work_journal_finish "$repo"
work_journal_defer_finish "$repo"

exit 0
