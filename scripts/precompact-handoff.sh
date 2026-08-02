#!/usr/bin/env bash
set -uo pipefail

# PreCompact hook: compaction is about to compress the transcript, so capture
# a session handoff digest while the detail is still lossless. Best-effort by
# contract: a hook that blocks or delays compaction costs more than a missing
# digest, so every failure path exits 0 silently.
# OMS_PRECOMPACT_HANDOFF=0 disables. Optional $1 names the agent whose session
# format to read (claude|codex|antigravity, default claude) — the Codex plugin
# dispatcher passes codex.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENT="${1:-claude}"

[ "${OMS_PRECOMPACT_HANDOFF:-1}" = "1" ] || exit 0
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
' 2>/dev/null)" || parsed=""
session="$(printf '%s\n' "$parsed" | sed -n 1p)"
cwd="$(printf '%s\n' "$parsed" | sed -n 2p)"
[ -n "$cwd" ] || cwd="$PWD"

# Only a harness-adopted repo keeps handoffs; a random directory must not grow
# a .oms tree because compaction happened to fire there.
[ -d "$cwd/.oms" ] || exit 0

args=(capture --agent "$AGENT" --cwd "$cwd" --note "auto: pre-compact snapshot")
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
    --cmd "session-handoff capture --agent $AGENT (pre-compact)" --exit 1 \
    --summary "${summary:-capture failed}" \
    --next "run manually: oms session-handoff capture --agent $AGENT" \
    >/dev/null 2>&1 || true
fi
rm -f "$err"
exit 0
