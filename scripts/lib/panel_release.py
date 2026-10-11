"""Explicit recovery of panel main attempts with proven absent terminal ownership."""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys

from dashboard_projection import clean
from panel_cache import read_shared
from panel_results import _lock
from panel_view import age_label
import panel_host
import room


ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,159}")
STATES = {"blocked", "working"}
REASON = "panel_main_released"


def refusal_hint(attempts):
    candidates = [row.get("attempt_id") for row in attempts if row.get("tool") == "panel-main"
                  and row.get("terminal") is not True and row.get("state") in STATES]
    attempt = candidates[0] if len(candidates) == 1 and isinstance(candidates[0], str) and ID.fullmatch(candidates[0]) else "ATTEMPT"
    return "; if that main crashed, release it with oms panel --release-main %s (see k)" % attempt


def events(repo, *args):
    result = subprocess.run(["bash", str(room.ENTRY), "agent-events", "--repo", str(repo)] + list(args),
                            capture_output=True, text=True, check=False, timeout=10, stdin=subprocess.DEVNULL)
    if result.returncode:
        raise ValueError("panel main lifecycle query/update failed; existing owners preserved")
    return result.stdout.replace("\r", "")


def tagged_attempts(windows=None, panes=True):
    """Reuse complete global window tags when supplied; session-scoped rows cannot prove absence."""
    from terminal_panel import tmux_command
    if not shutil.which("tmux"):
        return set(windows or ())
    found = set(windows or ())
    queries = (["list-panes"] if panes else []) + (["list-windows"] if windows is None else [])
    for query in queries:
        try:
            result = subprocess.run(tmux_command(query, "-a", "-F", "#{@oms_panel_main_attempt}"),
                                    capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError) as error:
            raise ValueError("main liveness unproven; tmux status unavailable") from error
        if result.returncode:
            if "no server running" in result.stderr.replace("\r", "").lower():
                continue
            raise ValueError("main liveness unproven; tmux status unavailable")
        if len(result.stdout) > 1048576:
            raise ValueError("main liveness unproven; tmux status exceeds the local limit")
        for value in result.stdout.replace("\r", "").splitlines():
            if value and not ID.fullmatch(value):
                raise ValueError("main liveness unproven; invalid tmux attempt tag")
            if value:
                found.add(value)
    return found


def prove_host_absent(refs):
    if not refs.get("panel_host") or refs["panel_host"] in {"tmux", "inline"}:
        return
    if refs["panel_host"] != "herdr":
        raise ValueError("main liveness unproven; unsupported terminal host")
    try:
        if not panel_host.herdr_active() or refs.get("panel_socket_digest") != panel_host.socket_digest():
            raise ValueError("host identity unavailable")
        panel_host.pane(refs.get("panel_pane_id"))
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ValueError("main liveness unproven; Herdr pane status unavailable") from error
    raise ValueError("main still has a live Herdr pane; existing owner preserved")


def main_attempt(repo, attempt):
    row = json.loads(events(repo, "show", "--attempt", attempt, "--json"))
    refs = row.get("refs") if isinstance(row, dict) else None
    if (not isinstance(refs, dict) or row.get("attempt_id") != attempt or row.get("tool") != "panel-main"
            or refs.get("panel_role") != "main" or row.get("state") not in STATES):
        raise ValueError("release requires a panel-main with panel_role=main in blocked or working state")
    return row


def release(repo, attempt):
    if not isinstance(attempt, str) or not ID.fullmatch(attempt):
        raise ValueError("invalid panel main attempt identifier")
    main_attempt(repo, attempt)
    with _lock(repo, "panel-main-start", timeout=3), _lock(repo, "panel-main-session", timeout=.1):
        row = main_attempt(repo, attempt)
        if attempt in tagged_attempts():
            raise ValueError("main still has a live tagged tmux window or pane; existing owner preserved")
        prove_host_absent(row["refs"])
        previous = row["state"]
        steps = ["--state", "blocked", "--then", "abandoned"] if previous == "working" else ["--state", "abandoned"]
        events(repo, "transition", "--attempt", attempt, *steps, "--actor", "panel-main-release", "--reason-code", REASON)
        final = json.loads(events(repo, "show", "--attempt", attempt, "--json"))
        if final.get("state") != "abandoned" or final.get("terminal") is not True:
            raise ValueError("panel main release could not be verified; inspect lifecycle state")
    return {"attempt": attempt, "previous_state": previous, "state": "abandoned", "reason": REASON}


def dead_mains(repo, windows=None):
    try:
        attempts = read_shared(repo, "attempts", lambda: json.loads(events(repo, "list", "--json")))
        if not isinstance(attempts, list) or any(not isinstance(row, dict) for row in attempts):
            return []
        candidates = [row for row in attempts if row.get("tool") == "panel-main" and row.get("terminal") is False
                      and row.get("state") in STATES and isinstance(row.get("refs"), dict) and row["refs"].get("panel_role") == "main"
                      and isinstance(row.get("attempt_id"), str) and ID.fullmatch(row["attempt_id"])]
        if not candidates:
            return []
        tagged = tagged_attempts(windows, panes=False)
        found = []
        for row in candidates:
            if row["attempt_id"] in tagged:
                continue
            try:
                prove_host_absent(row["refs"])
            except ValueError:
                continue
            found.append({"attempt": row["attempt_id"], "provider": clean(row.get("provider"), 64),
                          "state": clean(row.get("state"), 32), "age": age_label(row.get("updated_at")),
                          "label": clean(row["refs"].get("panel_label"), 160)})
        return found
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return []


def cli_main(arguments):
    from terminal_panel import repository
    parser = argparse.ArgumentParser(description="Release a panel main with proven absent terminal ownership.")
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--release-main", required=True, metavar="ATTEMPT")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(arguments)
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        parser.error("a worker cannot release owner sessions; return the need to its parent")
    try:
        result = release(repository(args.repo), args.release_main)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print("error: " + clean(str(error), 200), file=sys.stderr)
        return 1
    print(json.dumps(result) if args.json else "Released panel main %s: %s -> abandoned (%s)" % (
        result["attempt"], result["previous_state"], result["reason"]))
    return 0
