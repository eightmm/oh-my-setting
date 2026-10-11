"""Explicit recovery of panel main attempts with proven absent terminal ownership."""

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time

from dashboard_projection import clean
from panel_cache import read_shared
from panel_results import _lock
from panel_view import age_label
import panel_host
import room
import panel_terminals
from process_liveness import pid_alive


ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,159}")
STATES = {"blocked", "working"}
REASON = "panel_main_released"
MAX_TICK_PROBES = 16


class LiveTmuxMain(ValueError):
    pass


def refusal_hint(attempts):
    candidates = classify(attempts)["releasable"]
    if len(candidates) == 1:
        return "; if that main crashed, release it with oms panel --release-main %s (see k)" % candidates[0]["attempt"]
    return "; see capacity screen k for recovery evidence; candidate is unverifiable; release requires confirmed termination"


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
        raise ValueError("main liveness unproven; tmux is unavailable")
    found = set(windows or ())
    queries = (["list-panes"] if panes else []) + (["list-windows"] if windows is None else [])
    for query in queries:
        try:
            result = subprocess.run(tmux_command(query, "-a", "-F", "#{@oms_panel_main_attempt}"),
                                    capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError) as error:
            raise ValueError("main liveness unproven; tmux status unavailable") from error
        if result.returncode:
            raise ValueError("main liveness unproven; tmux status unavailable")
        if len(result.stdout) > 1048576:
            raise ValueError("main liveness unproven; tmux status exceeds the local limit")
        for value in result.stdout.replace("\r", "").splitlines():
            if value and not ID.fullmatch(value):
                raise ValueError("main liveness unproven; invalid tmux attempt tag")
            if value:
                found.add(value)
    return found


def process_refs(pid, kind):
    if type(pid) is not int or pid <= 0:
        return {}
    identity = panel_terminals._process_helpers().process_identity(pid)
    if not re.fullmatch(r"[0-9a-f]{64}", identity or ""):
        return {}
    return {"panel_" + kind + "_pid": str(pid), "panel_" + kind + "_identity": identity}


def identity_gone(refs, kind):
    """Python runner PIDs are native; Windows probes must never use os.kill(pid, 0)."""
    pid, identity = refs.get("panel_" + kind + "_pid"), refs.get("panel_" + kind + "_identity")
    if (not isinstance(pid, str) or not re.fullmatch(r"[1-9][0-9]{0,9}", pid)
            or int(pid) > 2147483647 or not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity)):
        return False
    if os.name != "nt":
        return panel_terminals.process_gone(int(pid), identity)
    current = panel_terminals._process_helpers().process_identity(int(pid))
    return current != identity if current else not pid_alive(int(pid), native_pid=int(pid))


def tmux_identity():
    from terminal_panel import tmux_command
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "#{pid}\t#{socket_path}"),
                                capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError("main liveness unproven; tmux server status unavailable") from error
    fields = result.stdout.replace("\r", "").strip().split("\t")
    if result.returncode or len(fields) != 2 or not re.fullmatch(r"[1-9][0-9]{0,9}", fields[0]) or not fields[1]:
        raise ValueError("main liveness unproven; tmux server identity unavailable")
    refs = process_refs(int(fields[0]), "tmux_server")
    if not refs:
        raise ValueError("main liveness unproven; tmux server start identity unavailable")
    refs["panel_socket_digest"] = hashlib.sha256(os.path.realpath(fields[1]).encode()).hexdigest()
    return refs


def start_refs(execution, managed):
    refs = process_refs(os.getpid(), "wrapper")
    refs.update(panel_heartbeat_interval="60", panel_heartbeat_ttl="300")
    if panel_host.herdr_active():
        refs.update(panel_host.refs(execution))
    elif managed:
        refs.update(panel_host="tmux", panel_pane_id=os.environ.get("TMUX_PANE", ""))
        try:
            refs.update(tmux_identity())
        except (OSError, ValueError, subprocess.SubprocessError):
            print("OMS: tmux ownership binding unavailable; this main may need separate recovery.", file=sys.stderr)
    else:
        refs["panel_host"] = "inline"
    return refs


def heartbeat_fresh(row):
    refs = row.get("refs") or {}
    interval, ttl = refs.get("panel_heartbeat_interval", "60"), refs.get("panel_heartbeat_ttl", "300")
    if (not isinstance(interval, str) or not re.fullmatch(r"[1-9][0-9]{0,5}", interval)
            or not isinstance(ttl, str) or not re.fullmatch(r"[1-9][0-9]{0,5}", ttl) or int(ttl) < int(interval)):
        raise ValueError("main liveness unproven; heartbeat bounds unavailable")
    ages = []
    for key in ("created_at", "last_heartbeat_at"):
        try:
            stamp = dt.datetime.strptime(row[key], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
            ages.append(time.time() - stamp.timestamp())
        except (KeyError, TypeError, ValueError):
            raise ValueError("main liveness unproven; heartbeat evidence unavailable")
    return ages[0] < int(interval) or ages[1] < int(ttl)


def prove_host_absent(refs, attempt, tagged=None, server=None, _tmux=None):
    host = refs.get("panel_host")
    if host == "inline":
        if identity_gone(refs, "wrapper"):
            return
        raise ValueError("main liveness unproven; inline wrapper termination is unconfirmed")
    if host == "tmux":
        if (not isinstance(refs.get("panel_pane_id"), str) or not re.fullmatch(r"%[0-9]+", refs["panel_pane_id"])
                or not isinstance(refs.get("panel_socket_digest"), str)
                or not re.fullmatch(r"[0-9a-f]{64}", refs["panel_socket_digest"])):
            raise ValueError("main liveness unproven; tmux ownership binding unavailable")
        if identity_gone(refs, "tmux_server"):
            return
        if _tmux is not None:
            server, tagged = _tmux()
        server = tmux_identity() if server is None else server
        if any(refs.get(key) != value for key, value in server.items()):
            raise ValueError("main liveness unproven; tmux server ownership changed")
        tagged = tagged_attempts() if tagged is None else tagged
        if attempt in tagged:
            raise LiveTmuxMain("main liveness unproven; main still has a live tagged tmux window or pane")
        return
    if host != "herdr":
        raise ValueError("main liveness unproven; recorded host ownership unavailable")
    try:
        if not panel_host.herdr_active() or refs.get("panel_socket_digest") != panel_host.socket_digest():
            raise ValueError("host identity unavailable")
        panel_host.pane(refs.get("panel_pane_id"))
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ValueError("main liveness unproven; Herdr pane status unavailable") from error
    raise ValueError("main liveness unproven; main still has a live Herdr pane")


def prove_native_absent(row):
    refs = row["refs"]
    if any(key in refs for key in ("panel_wrapper_pid", "panel_wrapper_identity")) and not identity_gone(refs, "wrapper"):
        raise ValueError("main liveness unproven; wrapper termination is unconfirmed")
    if identity_gone(refs, "native"):
        return
    if (row.get("reason_code") != "native_shutdown_unconfirmed" and refs.get("panel_native_exit") == "confirmed"
            and re.fullmatch(r"-?[0-9]+", refs.get("panel_native_exit_code", ""))):
        return
    if refs.get("panel_harness") == "pi":
        raise ValueError("main liveness unproven; Pi child termination unconfirmed; separate recovery required")
    raise ValueError("main liveness unproven; native termination is unconfirmed")


def main_attempt(repo, attempt):
    row = json.loads(events(repo, "show", "--attempt", attempt, "--json"))
    refs = row.get("refs") if isinstance(row, dict) else None
    if (not isinstance(row, dict) or row.get("attempt_id") != attempt or row.get("tool") != "panel-main"
            or row.get("state") not in STATES):
        raise ValueError("release requires a panel-main with panel_role=main in blocked or working state")
    if not isinstance(refs, dict) or not refs.get("panel_role"):
        raise ValueError("main liveness unproven; panel main role binding unavailable")
    if refs["panel_role"] != "main":
        raise ValueError("release requires a panel-main with panel_role=main in blocked or working state")
    return row


def release(repo, attempt):
    if not isinstance(attempt, str) or not ID.fullmatch(attempt):
        raise ValueError("invalid panel main attempt identifier")
    row = main_attempt(repo, attempt)
    if heartbeat_fresh(row):
        raise ValueError("main liveness unproven; heartbeat is fresh or attempt is pending")
    with _lock(repo, "panel-main-start", timeout=3), _lock(repo, "panel-main-session", timeout=.1):
        row = main_attempt(repo, attempt)
        if heartbeat_fresh(row):
            raise ValueError("main liveness unproven; heartbeat is fresh or attempt is pending")
        prove_host_absent(row["refs"], attempt)
        prove_native_absent(row)
        previous = row["state"]
        steps = ["--state", "blocked", "--then", "abandoned"] if previous == "working" else ["--state", "abandoned"]
        events(repo, "transition", "--attempt", attempt, *steps, "--actor", "panel-main-release", "--reason-code", REASON)
        final = json.loads(events(repo, "show", "--attempt", attempt, "--json"))
        if final.get("state") != "abandoned" or final.get("terminal") is not True:
            raise ValueError("panel main release could not be verified; inspect lifecycle state")
    return {"attempt": attempt, "previous_state": previous, "state": "abandoned", "reason": REASON}


def classify(attempts, windows=None, max_probes=None):
    found = {"releasable": [], "unverifiable": []}
    if not isinstance(attempts, list) or any(not isinstance(row, dict) for row in attempts):
        return found
    tagged, server, tmux_error = windows, None, None
    probes = 0

    def tmux_status():
        nonlocal tagged, server, tmux_error
        if tmux_error:
            raise ValueError(tmux_error)
        if server is None:
            try:
                server = tmux_identity()
                tagged = tagged_attempts(windows)
            except (OSError, ValueError, subprocess.SubprocessError):
                tmux_error = "main liveness unproven; tmux status unavailable"
                raise ValueError(tmux_error)
        return server, tagged

    for row in attempts:
        refs, attempt = row.get("refs") or {}, row.get("attempt_id")
        if (row.get("tool") != "panel-main" or row.get("terminal") is not False or row.get("state") not in STATES
                or not isinstance(refs, dict) or refs.get("panel_role") not in {None, "main"}
                or not isinstance(attempt, str) or not ID.fullmatch(attempt)):
            continue
        item = {"attempt": attempt, "provider": clean(row.get("provider"), 64),
                "state": row["state"], "age": age_label(row.get("updated_at")),
                "label": clean(refs.get("panel_label") or "", 160)}
        try:
            if heartbeat_fresh(row):
                continue
            if refs.get("panel_role") != "main":
                raise ValueError("main liveness unproven; panel main role binding unavailable")
            if max_probes is not None and probes >= max_probes:
                raise ValueError("main liveness unproven; verification pending (refresh probe limit)")
            probes += 1
            prove_host_absent(refs, attempt, _tmux=tmux_status)
            prove_native_absent(row)
        except LiveTmuxMain:
            continue
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            item["reason"] = clean(str(error), 160)
            found["unverifiable"].append(item)
        else:
            found["releasable"].append(item)
    return found


def dead_mains(repo, windows=None, _tick=None):
    try:
        attempts = _tick.attempts() if _tick is not None else read_shared(
            repo, "attempts", lambda: json.loads(events(repo, "list", "--json")))
        return classify(attempts, windows, max_probes=MAX_TICK_PROBES)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return {"releasable": [], "unverifiable": []}


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
