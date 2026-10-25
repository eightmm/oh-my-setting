import datetime
import hashlib
import json
import os
import re

from hook_repo import resolve

OFF = {"0", "false", "FALSE", "no", "NO", "off", "OFF"}

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

# This Bash surface bypasses the activity hook of the other tools, even when there
# is no failure history to resolve after the person approves the command.
# hook_state costs ~20 ms to import and acts only for a panel main that is not
# a harness child (hook_state.is_panel_main / is_harness_child), so every other
# process skips the import.
if (os.environ.get("OMS_PANEL_MAIN_ATTEMPT")
        and os.environ.get("OMS_PANEL_SESSION", "").startswith("oms-")
        and os.environ.get("OMS_HARNESS_CHILD") != "1"):
    try:
        from hook_state import record_panel_activity
        record_panel_activity(row)
    except Exception:
        pass

# Only repos that already carry harness state get ledger rows: an unadopted
# repo must not seed .oms as a ledger side effect.
repo = resolve(row)
if repo is None or not (repo / ".oms").is_dir():
    raise SystemExit(0)

# One stat is the whole cost of the success side in a repo with no failure
# history: an empty ledger has nothing to resolve, and the scan below never
# opens it.
ledger = os.environ.get("OMS_FAIL_LEDGER") or str(repo / ".oms" / "failures.jsonl")
try:
    if not os.stat(ledger).st_size or (os.environ.get("OMS_FAIL_LEDGER_RESOLVE") or "1") in OFF:
        ledger = ""
except OSError:
    ledger = ""

# Content-free usage counter: rows carry a family name and a day, never the
# command. Optional diagnostic telemetry; enable with OMS_USAGE_TRACK=1.
usage_path = ""
if (os.environ.get("OMS_USAGE_TRACK") or "0") not in OFF:
    usage_path = str(repo / ".oms" / "usage.jsonl")

# Usage counter, before any branch can bail: every Bash call in an adopted
# repo is an observation. A read-only first token is a mention, not use —
# `grep rdkit` must not count toward the chem family. Guarded so counter
# trouble can never touch the resolve/record paths, and silent on stdout,
# which belongs to the positional parse protocol below.
try:
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

event = row.get("hook_event_name")
event = event if isinstance(event, str) else ""

# Claude Code sends no Bash exit code. PostToolUse fires only on success;
# PostToolUseFailure carries `error` ("Exit code N\n...") and is_interrupt.
exit_code = None
response = row.get("tool_response")
if event == "PostToolUse":
    exit_code = 0
    # An interrupted or backgrounded command has not passed; Claude Code can
    # also background a running command mid-flight, which only the response
    # (backgroundTaskId) reveals.
    if (isinstance(response, dict) and (
            response.get("interrupted") is True or response.get("backgroundTaskId"))) or (
            isinstance(tool_input, dict) and tool_input.get("run_in_background") is True):
        raise SystemExit(0)
elif event == "PostToolUseFailure":
    if row.get("is_interrupt") is True:
        exit_code = 0
    else:
        error = row.get("error")
        match = re.match(r"Exit code (\d+)", error.split("\n", 1)[0]) if isinstance(error, str) else None
        exit_code = int(match.group(1)) if match else None


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


# The success event fires on every Bash call and means the command passed;
# failures arrive on their own event, which records them.
if event == "PostToolUse":
    if not ledger:
        raise SystemExit(0)
    fp = fingerprint(command)
    fails = 0
    try:
        handle = open(ledger, encoding="utf-8", errors="replace")
    except OSError:
        raise SystemExit(0)
    # Streamed, so memory is bounded by the longest row however long the
    # ledger grows. A row can only matter if it spells the fingerprint, so the
    # common case (this command never failed) costs a substring search per
    # line, not a JSON parse.
    with handle:
        for line in handle:
            if fp not in line:
                continue
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
    print(repo.as_posix())
    print("resolve")
    print(command)
    raise SystemExit(0)

if exit_code == 0:
    raise SystemExit(0)  # is_interrupt: not a failure worth remembering
if exit_code is None:
    # A failure event without a parseable exit code is still a failure.
    exit_code = 1
if exit_code in (130, 141, 143):
    # Interrupts and closed pipes are not failures worth remembering.
    raise SystemExit(0)
if re.search(r"/tmp/claude-\d+/", command):
    # A command naming a session-scoped scratch path has a fingerprint no
    # other session can recompute: the row would be write-only from birth.
    # The deliberate `record` verb is unaffected — this gates only the hook.
    raise SystemExit(0)
print(repo.as_posix())
print("record")
print(exit_code)
print(command)
