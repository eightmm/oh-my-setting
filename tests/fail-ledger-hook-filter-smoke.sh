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
  # $1 = command, $2 = exit code, optional $3 = cwd
  OMS_P_CMD="$1" OMS_P_EXIT="$2" OMS_P_CWD="${3:-}" python3 - <<'PY'
import json
import os
print(json.dumps({
    "tool_name": "Bash",
    "hook_event_name": "PostToolUseFailure",
    "cwd": os.environ.get("OMS_P_CWD") or None,
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

# Payload cwd selects the state repo even when the process cwd is another
# adopted checkout. Nested paths resolve to the physical Git root.
target="$TMP/target"
nested="$target/work/child"
other="$TMP/other"
mkdir -p "$target/.oms" "$nested" "$other/.oms"
git -C "$target" init -q
git -C "$other" init -q
payload_cwd="$nested"
if command -v cygpath >/dev/null 2>&1; then
  payload_cwd="$(cygpath -u "$nested")"
fi
payload "python3 -m payload_repo_only" 1 "$payload_cwd" |
  ( cd "$other" && env -u OMS_STATE_REPO bash "$ROOT/scripts/fail-ledger-hook.sh" )
grep -Fq payload_repo_only "$target/.oms/failures.jsonl" ||
  fail "the payload cwd should write into the adopted Git root"
[ ! -e "$other/.oms/failures.jsonl" ] ||
  fail "the process repo must not receive a payload repo failure"

OMS_P_CMD="python3 -m cwd_alias_only" OMS_P_EXIT=1 OMS_P_CWD="$nested" python3 - <<'PY' |
import json, os
print(json.dumps({
    "tool_name": "Bash", "hook_event_name": "PostToolUseFailure",
    "currentWorkingDirectory": os.environ["OMS_P_CWD"],
    "tool_input": {"command": os.environ["OMS_P_CMD"]},
    "tool_response": {"exit_code": int(os.environ["OMS_P_EXIT"])},
}))
PY
  ( cd "$other" && env -u OMS_STATE_REPO bash "$ROOT/scripts/fail-ledger-hook.sh" )
grep -Fq cwd_alias_only "$target/.oms/failures.jsonl" ||
  fail "currentWorkingDirectory should be accepted as the event cwd"

# Explicit repository override wins over payload cwd.
payload "python3 -m explicit_repo_only" 1 "$target" |
  ( cd "$other" && OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh" )
grep -Fq explicit_repo_only "$ledger" || fail "OMS_STATE_REPO should override payload cwd"
if grep -Fq explicit_repo_only "$target/.oms/failures.jsonl"; then
  fail "the overridden payload repo must not receive the failure"
fi

# Invalid or unadopted explicit payload cwd must not fall back to an adopted
# process directory. An absent cwd still uses the process directory.
invalid="$TMP/does-not-exist"
plain="$TMP/unadopted-payload"
mkdir -p "$plain"
touch "$other/.oms/failures.jsonl"
before="$(wc -l < "$other/.oms/failures.jsonl" | tr -d ' ')"
payload "python3 -m invalid_payload_only" 1 "$invalid" |
  ( cd "$other" && env -u OMS_STATE_REPO bash "$ROOT/scripts/fail-ledger-hook.sh" )
payload "python3 -m unadopted_payload_only" 1 "$plain" |
  ( cd "$other" && env -u OMS_STATE_REPO bash "$ROOT/scripts/fail-ledger-hook.sh" )
[ "$(wc -l < "$other/.oms/failures.jsonl" | tr -d ' ')" = "$before" ] ||
  fail "an invalid payload cwd must not redirect into the process repo"

payload "python3 -m invalid_override_only" 1 |
  ( cd "$other" && OMS_STATE_REPO="$invalid" bash "$ROOT/scripts/fail-ledger-hook.sh" )
[ "$(wc -l < "$other/.oms/failures.jsonl" | tr -d ' ')" = "$before" ] ||
  fail "an invalid explicit OMS_STATE_REPO must not fall back to process cwd"

payload "python3 -m absent_cwd_fallback" 1 |
  ( cd "$other" && env -u OMS_STATE_REPO bash "$ROOT/scripts/fail-ledger-hook.sh" )
grep -Fq absent_cwd_fallback "$other/.oms/failures.jsonl" ||
  fail "an absent payload cwd should fall back to process cwd"

echo "fail-ledger-hook-filter-smoke: ok"
