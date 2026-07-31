#!/usr/bin/env bash
set -euo pipefail

# PostToolUseFailure hook: give the agent's failed commands the memory the
# rules always assumed. fail-ledger has recorded, warned, and named `oms
# advise` at the repeat threshold since it existed — but only when something
# called it, which the primary agent mid-failure-loop never did. This hook is
# that call: when a Bash tool run fails, it first surfaces what the ledger
# already knows about this command (hook stdout becomes agent context), then
# records the failure so the second identical attempt crosses the advise
# threshold mechanically. Fail-open by construction: it cannot change the tool
# result, it never exits nonzero, and it prints nothing unless the ledger has
# something to say. OMS_FAIL_LEDGER_HOOK=0 disables it.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${OMS_FAIL_LEDGER_HOOK:-1}" in
  0|false|FALSE|no|NO|off|OFF) exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

parsed="$(OMS_FLH_PAYLOAD="$payload" python3 - <<'PY' 2>/dev/null || true
import json
import os

try:
    row = json.loads(os.environ["OMS_FLH_PAYLOAD"])
except (ValueError, KeyError):
    raise SystemExit(0)
if not isinstance(row, dict) or row.get("tool_name") != "Bash":
    raise SystemExit(0)
tool_input = row.get("tool_input")
command = tool_input.get("command") if isinstance(tool_input, dict) else None
if not isinstance(command, str) or not command.strip():
    raise SystemExit(0)
response = row.get("tool_response")
exit_code = None
if isinstance(response, dict):
    for key in ("exit_code", "exitCode", "code"):
        value = response.get(key)
        if isinstance(value, int) and not isinstance(value, bool) and value > 0:
            exit_code = value
            break
if exit_code is None:
    # A failure event without a readable exit code is still a failure.
    exit_code = 1
if exit_code in (130, 141, 143):
    # Interrupts and closed pipes are not failures worth remembering.
    raise SystemExit(0)
print(exit_code)
print(command.replace("\n", " ")[:2000])
PY
)"
[ -n "$parsed" ] || exit 0
exit_code="$(printf '%s\n' "$parsed" | sed -n 1p)"
cmd="$(printf '%s\n' "$parsed" | sed -n 2p)"
case "$exit_code" in ''|*[!0-9]*) exit 0 ;; esac
[ -n "$cmd" ] || exit 0

# Only repos that already carry harness state get rows: a failed command in an
# unadopted repo must not seed .oms as a hook side effect.
repo="${OMS_STATE_REPO:-$PWD}"
root="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$repo")"
[ -d "$root/.oms" ] || exit 0
[ -x "$ROOT/scripts/fail-ledger.sh" ] || exit 0

# Known-failure context first: the ledger speaks on stderr, and check exits 3
# exactly when this fingerprint is an unresolved failure in an unchanged repo.
known=""
if ! known="$("$ROOT/scripts/fail-ledger.sh" --repo "$root" check --cmd "$cmd" 2>&1)"; then
  :
else
  known=""
fi

recorded="$("$ROOT/scripts/fail-ledger.sh" --repo "$root" record \
  --cmd "$cmd" --exit "$exit_code" --kind hook 2>&1 || true)"
advise="$(printf '%s\n' "$recorded" | grep -F 'oms advise' || true)"

# Emit only ledger speech, once per line: prior context and the advise-at-
# threshold hint. The routine "recorded <fp>" receipt is bookkeeping, not
# something the agent should read after every failed command.
printf '%s\n%s\n' "$known" "$advise" |
  grep '^fail-ledger:' | awk '!seen[$0]++' || true
exit 0
