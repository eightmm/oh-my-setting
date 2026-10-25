#!/usr/bin/env bash
set -euo pipefail

# The fail-ledger hook must refuse to record commands that name a
# session-scoped scratch path (/tmp/claude-<uid>/...): no other session can
# ever recompute that fingerprint, so the row would be open forever by
# construction. Ordinary failing commands still record, and the deliberate
# `record` verb keeps accepting anything.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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
  # $1 = command, $2 = exit code, optional $3 = cwd, $4 = is_interrupt (1/0)
  OMS_P_CMD="$1" OMS_P_EXIT="$2" OMS_P_CWD="${3:-}" OMS_P_INT="${4:-0}" python3 - <<'PY'
import json
import os
print(json.dumps({
    "tool_name": "Bash",
    "hook_event_name": "PostToolUseFailure",
    "cwd": os.environ.get("OMS_P_CWD") or None,
    "tool_input": {"command": os.environ["OMS_P_CMD"]},
    "error": "Exit code %s\nboom" % os.environ["OMS_P_EXIT"],
    "is_interrupt": os.environ["OMS_P_INT"] == "1",
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

# Interrupts are not failures: is_interrupt, and a documented "Exit code 130".
payload "sleep 999" 1 "" 1 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
payload "sleep 998" 130 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
if [ -s "$ledger" ]; then
  fail "an interrupt must not reach the ledger: $(cat "$ledger")"
fi

# An ordinary failure still records, with the exit code the error text names.
payload "python3 -m nope_such_module" 1 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
[ -s "$ledger" ] || fail "an ordinary hook failure should record a row"
grep -Fq nope_such_module "$ledger" ||
  fail "the recorded row should carry the failing command"
payload "python3 -m code_two_only" 2 |
  OMS_STATE_REPO="$repo" bash "$ROOT/scripts/fail-ledger-hook.sh"
grep -F code_two_only "$ledger" | grep -E '"exit": ?2' > /dev/null ||
  fail "the row should carry the exit code parsed from the error text"

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
    "error": "Exit code %s\nboom" % os.environ["OMS_P_EXIT"],
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

# The success pass imports hook_state (about 20 ms) only for a panel main:
# record_panel_activity returns at once for every other process. A scratch
# lib whose hook_state leaves a marker on import shows which processes load it.
fake="$TMP/fake-root"
mkdir -p "$fake/scripts/lib"
cp "$ROOT/scripts/fail-ledger-hook.sh" "$fake/scripts/"
cp "$ROOT/scripts/lib/cached-main.py" "$ROOT/scripts/lib/hook_repo.py" \
  "$ROOT/scripts/lib/usage-families.json" "$fake/scripts/lib/"
cp "$ROOT/scripts/lib/fail_ledger_hook.py" "$fake/scripts/lib/" 2>/dev/null || true
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "${OMS_FLH_CALLS:-/dev/null}"\nexit 0\n' > "$fake/scripts/fail-ledger.sh"
chmod +x "$fake/scripts/fail-ledger.sh"
cat > "$fake/scripts/lib/hook_state.py" <<'PY'
import os
open(os.environ["OMS_FLH_MARK"], "w").close()


def record_panel_activity(payload):
    pass
PY
printf '{"event":"fail","fingerprint":"0000000000000000"}\n' > "$ledger"
success_payload="$(OMS_P_REPO="$repo" python3 -c '
import json, os
print(json.dumps({"tool_name": "Bash", "hook_event_name": "PostToolUse",
                  "cwd": os.environ["OMS_P_REPO"],
                  "tool_input": {"command": "true"}}))
')"
mark="$TMP/hook-state-imported"
run_success() {
  rm -f "$mark"
  printf '%s' "$success_payload" |
    env -u OMS_HARNESS_CHILD -u OMS_PANEL_MAIN_ATTEMPT -u OMS_PANEL_SESSION \
      -u OMS_STATE_REPO OMS_FLH_MARK="$mark" "$@" bash "$fake/scripts/fail-ledger-hook.sh"
}
run_success
[ ! -e "$mark" ] || fail "a non-panel success pass must not import hook_state"
run_success OMS_PANEL_MAIN_ATTEMPT=a1 OMS_PANEL_SESSION=oms-x
[ -e "$mark" ] || fail "a panel main success pass must still record panel activity"
run_success OMS_PANEL_MAIN_ATTEMPT=a1 OMS_PANEL_SESSION=oms-x OMS_HARNESS_CHILD=1
[ ! -e "$mark" ] || fail "a harness child must not import hook_state"

# The success scan streams the ledger: memory stays bounded by the longest row,
# so a ledger far larger than the address space still resolves a passed command.
big="$TMP/big-failures.jsonl"
OMS_P_FILE="$big" python3 - <<'PY'
import hashlib, json, os
fp = hashlib.sha256(b"true").hexdigest()[:16]
filler = json.dumps({"event": "fail", "fingerprint": "f" * 16, "cmd": "x" * 900})
with open(os.environ["OMS_P_FILE"], "w") as out:
    out.write(json.dumps({"event": "fail", "fingerprint": fp}) + "\n")
    for _ in range(45000):
        out.write(filler + "\n")
PY
calls="$TMP/big-calls"
big_limit_kb=98304
if ! ( ulimit -v "$big_limit_kb" 2>/dev/null &&
    ! python3 -c 'bytearray(150 * 1024 * 1024)' 2>/dev/null ); then
  echo "fail-ledger-hook-filter-smoke: skip large-ledger case (ulimit -v is not enforced here)"
else
  rm -f "$calls"
  printf '%s' "$success_payload" |
    ( ulimit -v "$big_limit_kb"
      env -u OMS_HARNESS_CHILD -u OMS_PANEL_MAIN_ATTEMPT -u OMS_PANEL_SESSION \
        -u OMS_STATE_REPO OMS_FAIL_LEDGER="$big" OMS_FLH_CALLS="$calls" \
        bash "$fake/scripts/fail-ledger-hook.sh" )
  grep -q 'resolve' "$calls" 2>/dev/null ||
    fail "a ledger larger than the address space must still resolve a passed command"
fi

echo "fail-ledger-hook-filter-smoke: ok"
