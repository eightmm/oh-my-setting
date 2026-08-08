#!/usr/bin/env bash
set -uo pipefail

# PreCompact / SessionEnd hook: compaction is about to compress the transcript
# and session end drops it from reach entirely, so capture a session handoff
# digest while the detail is still lossless. Best-effort by contract: a hook
# that blocks or delays either event costs more than a missing digest, so every
# failure path exits 0 silently.
# OMS_PRECOMPACT_HANDOFF=0 disables the hook outright (the pre-existing kill
# switch, kept whole-script so an operator who set it stays opted out of the new
# trigger too); OMS_SESSIONEND_HANDOFF=0 disables only the session-end capture.
# Optional $1 names the agent whose session format to read
# (claude|codex|antigravity, default claude) — the Codex plugin dispatcher
# passes codex. The event itself comes from the payload, not from $1.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT="${1:-claude}"
# Minutes: a digest for this session younger than this means the pressure path
# or PreCompact just captured, and a second one would add nothing.
DEDUPE_MIN=15

[ "${OMS_PRECOMPACT_HANDOFF:-1}" = "1" ] || exit 0
# Peer children (peer-common.sh exports OMS_HARNESS_CHILD=1) run headless in the
# adopted repo's cwd, so without this their throwaway sessions would each end by
# writing a digest and taking over the resume hook's newest-handoff pointer.
[ "${OMS_HARNESS_CHILD:-0}" != 1 ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

parsed="$(printf '%s' "$payload" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
print(data.get("session_id", "") or "")
print(data.get("cwd", "") or "")
print(data.get("hook_event_name", "") or "")
' 2>/dev/null)" || parsed=""
session="$(printf '%s\n' "$parsed" | sed -n 1p)"
cwd="$(printf '%s\n' "$parsed" | sed -n 2p)"
event="$(printf '%s\n' "$parsed" | sed -n 3p)"
[ -n "$cwd" ] || cwd="$PWD"

# The payload's event names the capture; $1 stays the agent name. An absent or
# unknown event keeps the original pre-compact wording, which is what the Codex
# dispatcher and any older registration send.
case "$event" in
  SessionEnd)
    [ "${OMS_SESSIONEND_HANDOFF:-1}" = "1" ] || exit 0
    note="auto: session end"
    label="session-end"
    ;;
  *)
    note="auto: pre-compact snapshot"
    label="pre-compact"
    ;;
esac

# Only a harness-adopted repo keeps handoffs; a random directory must not grow
# a .oms tree because compaction happened to fire there.
[ -d "$cwd/.oms" ] || exit 0

# Recency, not existence: a later capture supersedes an earlier one — the tail
# after a mid-session compaction is exactly what the next session needs — so
# only a digest written minutes ago (pressure capture just fired, or compact
# then exit) makes this run redundant. gc sweeps the superseded ones.
# The name mirrors session-handoff.sh's slug_id (tr, 24 chars, no trailing -).
if [ -n "$session" ]; then
  slug="$(printf '%s' "$session" | tr -c 'A-Za-z0-9' '-' | cut -c1-24 | sed 's/-*$//')"
  if [ -n "$slug" ] && [ -d "$cwd/.oms/handoffs" ]; then
    recent="$(find "$cwd/.oms/handoffs" -maxdepth 1 -type f \
      -name "$AGENT-$slug-*.md" -mmin "-$DEDUPE_MIN" 2>/dev/null | head -n 1)"
    [ -z "$recent" ] || exit 0
  fi
fi

# missing-session-policy skip: a worker-style session id that never wrote a
# transcript is a structural no-op, not a failure — without this, every such
# SessionEnd minted a ledger row and the row's "run manually" hint failed
# for the same reason, minting more.
args=(capture --agent "$AGENT" --cwd "$cwd" --note "$note" --missing-session-policy skip)
# With an explicit session id, capture that session or nothing: guessing the
# newest session in this cwd could snapshot a concurrent session's work under
# this one's label. Newest-for-cwd runs only when the payload named no session.
if [ -n "$session" ]; then
  args+=(--session "$session")
fi
# Still fail-open, no longer fail-silent: a refused capture goes to the
# fail-ledger, whose existing surfaces (check, inbox, daily hint) show it.
# Months of empty .oms/handoffs with every hook reporting success is exactly
# the failure mode this closes.
err="$(mktemp 2>/dev/null)" || err=""
if [ -z "$err" ]; then
  "$ROOT/scripts/session-handoff.sh" "${args[@]}" >/dev/null 2>&1 || true
  exit 0
fi
if ! "$ROOT/scripts/session-handoff.sh" "${args[@]}" >/dev/null 2>"$err"; then
  summary="$(head -c 160 "$err" 2>/dev/null | tr '\n' ' ')"
  "$ROOT/scripts/fail-ledger.sh" record --repo "$cwd" --kind hook \
    --cmd "session-handoff capture --agent $AGENT ($label)" --exit 1 \
    --summary "${summary:-capture failed}" \
    --next "run manually: oms session-handoff capture --agent $AGENT" \
    >/dev/null 2>&1 || true
fi
rm -f "$err"
exit 0
