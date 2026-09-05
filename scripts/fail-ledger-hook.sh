#!/usr/bin/env bash
set -euo pipefail

# PostToolUseFailure + PostToolUse hook: give the agent's Bash commands the
# memory the rules always assumed, and take it back when they pass. fail-ledger
# has recorded, warned, and named `oms advise` at the repeat threshold since it
# existed — but only when something called it, which the primary agent
# mid-failure-loop never did. This hook is that call: when a Bash tool run
# fails, it first surfaces what the ledger already knows about this command,
# then records the failure so the second identical attempt crosses the advise
# threshold mechanically. When the same command later exits 0, the hook
# resolves its own row — the ledger's resolved-on-success contract, honored by
# the writer that produced the rows instead of by a human sweep. Fail-open by
# construction: it cannot change the tool result, and it says nothing unless
# the ledger has something to say. When it does, the speech goes to stderr
# with exit 2 — on PostToolUse/PostToolUseFailure that is the one channel
# Claude Code shows to the model without blocking anything; plain stdout on
# these events reaches only the debug log, which is where this hook's context
# went unread until a probe showed the second identical failure arriving with
# no ledger line attached. OMS_FAIL_LEDGER_HOOK=0 disables the whole script;
# OMS_FAIL_LEDGER_RESOLVE=0 disables only the success side.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${OMS_FAIL_LEDGER_HOOK:-1}" in
  0|false|FALSE|no|NO|off|OFF) exit 0 ;;
esac

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

# Adoption gating before the parse: PostToolUse fires on every successful Bash
# call, so an unadopted repo must cost a rev-parse rather than a python spawn.
# Only repos that already carry harness state get rows — a command in an
# unadopted repo must not seed .oms as a hook side effect.
repo="${OMS_STATE_REPO:-$PWD}"
root="$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$repo")"
[ -d "$root/.oms" ] || exit 0
[ -x "$ROOT/scripts/fail-ledger.sh" ] || exit 0

# One stat is the whole cost of the success side in a repo with no failure
# history: an empty ledger has nothing to resolve, and the scan below never
# opens it. Emptied here so the parse can bail before fingerprinting anything.
ledger="${OMS_FAIL_LEDGER:-$root/.oms/failures.jsonl}"
[ -s "$ledger" ] || ledger=""
case "${OMS_FAIL_LEDGER_RESOLVE:-1}" in
  0|false|FALSE|no|NO|off|OFF) ledger="" ;;
esac

# Content-free usage counter: rows carry a family name and a day, never the
# command. Optional diagnostic telemetry; enable with OMS_USAGE_TRACK=1.
usage_file="$root/.oms/usage.jsonl"
case "${OMS_USAGE_TRACK:-0}" in
  0|false|FALSE|no|NO|off|OFF) usage_file="" ;;
esac

# One python3 pass decides everything: which side of the ledger this event is,
# and — on the success side — whether the fingerprint has an open failure at
# all. Deliberately not a `check` call: resolving needs no git state
# fingerprint, so this never computes one.
parsed="$(OMS_FLH_PAYLOAD="$payload" OMS_FLH_LEDGER="$ledger" \
  OMS_FLH_USAGE="$usage_file" OMS_FLH_FAMILIES="$ROOT/scripts/lib/usage-families.json" \
  python3 - <<'PY' 2>/dev/null || true
import datetime
import hashlib
import json
import os
import re

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
command = command.replace("\n", " ")[:2000]

# Usage counter, before any branch can bail: every Bash call in an adopted
# repo is an observation. A read-only first token is a mention, not use —
# `grep rdkit` must not count toward the chem family. Guarded so counter
# trouble can never touch the resolve/record paths, and silent on stdout,
# which belongs to the positional parse protocol below.
try:
    usage_path = os.environ.get("OMS_FLH_USAGE") or ""
    if usage_path:
        first = os.path.basename(re.split(r"\s+", command.strip())[0])
        readers = {"grep", "egrep", "fgrep", "rg", "sed", "awk", "cat",
                   "head", "tail", "less", "wc", "find", "echo", "printf",
                   "ls", "stat", "file", "diff"}
        if first not in readers:
            with open(os.environ.get("OMS_FLH_FAMILIES") or "",
                      encoding="utf-8") as handle:
                families = json.load(handle).get("families", {})
            lowered = command.lower()
            day = datetime.date.today().isoformat()
            hits = [name for name, spec in families.items()
                    if isinstance(spec, dict) and spec.get("pattern")
                    and re.search(spec["pattern"], lowered)]
            if hits:
                with open(usage_path, "a", encoding="utf-8") as handle:
                    for name in hits:
                        handle.write(json.dumps(
                            {"schema": 1, "family": name, "day": day},
                            sort_keys=True) + "\n")
except Exception:
    pass

response = row.get("tool_response")
exit_code = None
if isinstance(response, dict):
    for key in ("exit_code", "exitCode", "code"):
        value = response.get(key)
        if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
            exit_code = value
            break
event = row.get("hook_event_name")
event = event if isinstance(event, str) else ""


def fingerprint(cmd):
    # Mirrors fail-ledger.sh: machine paths normalized the way
    # agent_memory_normalize_machine_paths does, whitespace collapsed, long
    # digit runs masked. Drift can only miss a resolve, never invent one — the
    # row is filed under the fingerprint `fail-ledger.sh resolve` recomputes.
    home = os.environ.get("HOME") or ""
    if home:
        cmd = cmd.replace(home, "~")
    # The [e]/[s] classes keep these regex literals out of the outbound
    # scrubber net, same spelling as agent_memory_normalize_machine_paths.
    cmd = re.sub(r"/hom[e]/[A-Za-z0-9._-]+", "~", cmd)
    cmd = re.sub(r"/User[s]/[A-Za-z0-9._-]+", "~", cmd)
    norm = re.sub(r"\s+", " ", cmd).strip()
    norm = re.sub(r"\d{8,}", "N", norm)
    return hashlib.sha256(norm.encode("utf-8", "replace")).hexdigest()[:16]


# The success event fires on every Bash call; only an explicit exit 0 is a
# pass, and a nonzero one belongs to the failure event, which records it —
# deciding here keeps one failed command from being filed twice. A payload
# with no event name and an explicit 0 is a success too: 0 is not a failure.
if event == "PostToolUse" or (not event and exit_code == 0):
    ledger = os.environ.get("OMS_FLH_LEDGER") or ""
    if exit_code != 0 or not ledger:
        raise SystemExit(0)
    fp = fingerprint(command)
    fails = 0
    try:
        handle = open(ledger, encoding="utf-8", errors="replace")
    except OSError:
        raise SystemExit(0)
    with handle:
        for line in handle:
            try:
                r = json.loads(line)
            except Exception:
                continue
            if r.get("fingerprint") != fp:
                continue
            if r.get("event") == "resolved":
                fails = 0
            elif r.get("event") == "fail":
                fails += 1
    # Two named semantic changes this branch buys, both intended:
    # (a) a flaky fail/pass/fail command resets its repeat count here, so the
    #     advise threshold now means "failed twice since the last pass";
    # (b) a command that is never rerun verbatim still accumulates — this
    #     shrinks the manual sweep burden, it does not end it.
    if fails <= 0:
        raise SystemExit(0)
    print("resolve")
    print(command)
    raise SystemExit(0)

if exit_code is None or exit_code == 0:
    # A failure event without a readable exit code is still a failure.
    exit_code = 1
if exit_code in (130, 141, 143):
    # Interrupts and closed pipes are not failures worth remembering.
    raise SystemExit(0)
if re.search(r"/tmp/claude-\d+/", command):
    # A command naming a session-scoped scratch path has a fingerprint no
    # other session can recompute: the row would be write-only from birth.
    # The deliberate `record` verb is unaffected — this gates only the hook.
    raise SystemExit(0)
print("record")
print(exit_code)
print(command)
PY
)"
[ -n "$parsed" ] || exit 0
mode="$(printf '%s\n' "$parsed" | sed -n 1p)"

if [ "$mode" = "resolve" ]; then
  cmd="$(printf '%s\n' "$parsed" | sed -n 2p)"
  [ -n "$cmd" ] || exit 0
  # Silent on purpose: the resolve receipt is bookkeeping, and a command that
  # just passed is the last thing the agent needs narrated back to it.
  "$ROOT/scripts/fail-ledger.sh" --repo "$root" resolve --cmd "$cmd" \
    --how "same command passed (exit 0)" >/dev/null 2>&1 || true
  exit 0
fi

exit_code="$(printf '%s\n' "$parsed" | sed -n 2p)"
cmd="$(printf '%s\n' "$parsed" | sed -n 3p)"
case "$exit_code" in ''|*[!0-9]*) exit 0 ;; esac
[ -n "$cmd" ] || exit 0

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
speech="$(printf '%s\n%s\n' "$known" "$advise" |
  grep '^fail-ledger:' | awk '!seen[$0]++' || true)"
[ -n "$speech" ] || exit 0
printf '%s\n' "$speech" >&2
exit 2
