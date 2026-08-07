#!/usr/bin/env bash
set -euo pipefail

# The fail-ledger hook must refuse to record commands that name a
# session-scoped scratch path (/tmp/claude-<uid>/...): no other session can
# ever recompute that fingerprint, so the row would be open forever by
# construction. Ordinary failing commands still record, and the deliberate
# `record` verb keeps accepting anything.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-flh-filter.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/home" "$TMP/locks"
export HOME="$TMP/home"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_LOCK_FORCE_MKDIR=1

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
mkdir -p "$repo/.oms"

payload() {
  # $1 = command, $2 = exit code
  OMS_P_CMD="$1" OMS_P_EXIT="$2" python3 - <<'PY'
import json
import os
print(json.dumps({
    "tool_name": "Bash",
    "hook_event_name": "PostToolUseFailure",
    "tool_input": {"command": os.environ["OMS_P_CMD"]},
    "tool_response": {"exit_code": int(os.environ["OMS_P_EXIT"])},
}))
PY
}

ledger="$repo/.oms/failures.jsonl"

# A failure naming a session scratch path must not be recorded.
payload "cat /tmp/claude-1000/-some-repo/0c25aee6/scratchpad/x.md" 1 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
if [ -s "$ledger" ]; then
  fail "session-scratch-path failure must not reach the ledger: $(cat "$ledger")"
fi

# An ordinary failure still records.
payload "python3 -m nope_such_module" 1 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
[ -s "$ledger" ] || fail "an ordinary hook failure should record a row"
grep -Fq nope_such_module "$ledger" ||
  fail "the recorded row should carry the failing command"

# The deliberate record verb is not gated by the hook filter.
"$ROOT/scripts/fail-ledger.sh" --repo "$repo" record \
  --cmd "review /tmp/claude-1000/session/artifact.md" --exit 1 --kind cmd \
  >/dev/null 2>&1 || fail "explicit record must accept scratch paths"
grep -Fq '"kind": "cmd"' "$ledger" || grep -Fq '"kind":"cmd"' "$ledger" ||
  fail "explicit record should have written its row"

echo "fail-ledger-hook-filter-smoke: ok"
