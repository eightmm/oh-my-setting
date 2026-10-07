"""Optional local terminal host; OMS lifecycle records remain authoritative."""

import hashlib
import json
import os
from pathlib import Path
import re
import socket
import stat
import uuid


MAX_RESPONSE = 1048576


def herdr_active():
    return (os.name == "posix" and os.environ.get("HERDR_ENV") == "1"
            and bool(os.environ.get("HERDR_SOCKET_PATH"))
            and os.environ.get("OMS_PANEL_HOST") not in {"inline", "tmux"})


def request(method, params):
    """Never replay a mutation after an ambiguous connection failure."""
    if os.name != "posix" or not herdr_active():
        raise ValueError("Herdr host requires an existing local POSIX Herdr pane")
    path = Path(os.environ["HERDR_SOCKET_PATH"])
    info = path.lstat()
    if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError("Herdr socket is not an owned local socket")
    ident = uuid.uuid4().hex
    payload = json.dumps({"id": ident, "method": method, "params": params}).encode() + b"\n"
    if len(payload) > MAX_RESPONSE:
        raise ValueError("Herdr request exceeds the local host limit")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        connection.connect(str(path))
        connection.sendall(payload)
        data = bytearray()
        while b"\n" not in data:
            chunk = connection.recv(min(65536, MAX_RESPONSE + 1 - len(data)))
            if not chunk:
                raise ValueError("Herdr disconnected; host outcome is unknown")
            data.extend(chunk)
            if len(data) > MAX_RESPONSE:
                raise ValueError("Herdr response exceeds the local host limit")
    response = json.loads(bytes(data).split(b"\n", 1)[0])
    if not isinstance(response, dict) or response.get("id") != ident:
        raise ValueError("Herdr response identity is unavailable")
    if "error" in response:
        raise ValueError("Herdr host rejected " + method)
    result = response.get("result")
    if not isinstance(result, dict):
        raise ValueError("Herdr host returned an invalid result")
    return result


def socket_digest():
    return hashlib.sha256(str(Path(os.environ["HERDR_SOCKET_PATH"]).resolve()).encode()).hexdigest()


def pane(ident):
    if not isinstance(ident, str) or not re.fullmatch(r"[A-Za-z0-9_-]+:p[A-Za-z0-9_-]+", ident):
        raise ValueError("invalid Herdr pane identity")
    result = request("pane.get", {"pane_id": ident}).get("pane")
    if (not isinstance(result, dict) or result.get("pane_id") != ident
            or any(not isinstance(result.get(key), str) or not result[key] or len(result[key]) > 160
                   for key in ("terminal_id", "workspace_id", "tab_id"))):
        raise ValueError("Herdr pane identity is unavailable")
    return result


def current(repo):
    result = pane(os.environ.get("HERDR_PANE_ID"))
    cwd = result.get("foreground_cwd") or result.get("cwd")
    if not isinstance(cwd, str) or Path(cwd).resolve() != Path(repo).resolve():
        raise ValueError("Herdr pane is outside the selected repository")
    return result


def refs(repo):
    current_pane = current(repo)
    return {"panel_host": "herdr", "panel_socket_digest": socket_digest(),
            "panel_pane_id": current_pane["pane_id"],
            "panel_terminal_id": current_pane["terminal_id"]}


def targets(repo, ident, state):
    """Reconcile fresh host facts with an active OMS owner, never a UI label."""
    if not herdr_active():
        return {}
    import room
    import subprocess
    try:
        evidence = subprocess.run(["bash", str(room.ENTRY), "agent-events", "--repo", str(repo),
                                   "list", "--active", "--json"], capture_output=True, text=True,
                                  check=True, timeout=5, stdin=subprocess.DEVNULL)
        attempts = json.loads(evidence.stdout)
        if not isinstance(attempts, list):
            return {}
        members = {m["participant"]: m for m in state["participants"] if m["role"] == "main"}
        found = {}
        uncertain = set()
        digest = socket_digest()
        for attempt in attempts:
            if not isinstance(attempt, dict):
                continue
            ref = attempt.get("refs", {})
            if not isinstance(ref, dict):
                continue
            who = ref.get("panel_room_participant", attempt.get("attempt_id"))
            if (who not in members or attempt.get("terminal") is not False
                    or attempt.get("tool") != "panel-main" or ref.get("panel_role") != "main"
                    or attempt.get("provider") != members[who]["provider"]
                    or ref.get("panel_room_id") != ident or ref.get("panel_host") != "herdr"
                    or ref.get("panel_socket_digest") != digest):
                continue
            try:
                observed = pane(ref.get("panel_pane_id"))
            except (ValueError, KeyError, TypeError):
                # A missing candidate cannot make its sibling uniquely owned.
                uncertain.add(who)
                continue
            if (observed["terminal_id"] != ref.get("panel_terminal_id")
                    or not observed.get("cwd") or Path(observed["cwd"]).resolve() != Path(repo).resolve()):
                continue
            native = observed.get("agent_session", {})
            if native and (not isinstance(native, dict) or native.get("agent") != attempt["provider"]
                           or hashlib.sha256(str(native.get("value", "")).encode()).hexdigest()[:32]
                           not in (members[who].get("consumer"), members[who].get("initial_consumer"))):
                continue
            found.setdefault(who, []).append({"host": "herdr", "pane": observed["pane_id"],
                "terminal": observed["terminal_id"], "socket": digest, "repo": str(Path(repo).resolve()),
                "room": ident, "participant": who, "attempt": attempt["attempt_id"]})
        return {who: candidates for who, candidates in found.items() if who not in uncertain}
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return {}


def verify(target):
    if target.get("socket") != socket_digest() or pane(target["pane"])["terminal_id"] != target.get("terminal"):
        raise ValueError("Herdr conversation changed; refresh before navigating")
    import room
    candidates = targets(target["repo"], target["room"], room.status(target["repo"], target["room"]))
    if candidates.get(target["participant"]) != [target]:
        raise ValueError("Herdr owner changed; refresh before navigating")


def focus(target):
    verify(target)
    request("pane.focus", {"pane_id": target["pane"]})


def read(target):
    verify(target)
    result = request("pane.read", {"pane_id": target["pane"], "source": "visible", "lines": 8})
    reading = result.get("read")
    if not isinstance(reading, dict) or reading.get("pane_id") != target["pane"]:
        return ""
    value = reading.get("text")
    return value if isinstance(value, str) and len(value) <= 16384 else ""


def open_native(repo, provider, command, watcher):
    current_pane = current(repo)
    env = {key: os.environ[key] for key in ("OMS_ROOM_ID", "OMS_ROOM_REPO", "OMS_PANEL_NO_ANIMATION", "OMS_PANEL_COLOR", "NO_COLOR")
           if key in os.environ}
    env["OMS_PANEL_HOST"] = "herdr"
    top = os.environ.get("OMS_PANEL_POSITION", "auto") in ("auto", "top", "left")
    env["OMS_PANEL_VIEW"] = "graph" if top else "summary"
    env["OMS_PANEL_POSITION"] = "top" if top else "side"
    # layout.apply creates a new tab; it never types into an existing CLI.
    leaf = lambda label, argv: {"type": "pane", "label": label, "cwd": str(repo), "command": argv, "env": env}
    request("layout.apply", {"workspace_id": current_pane["workspace_id"], "tab_label": "OMS / " + provider,
        "focus": True, "root": {"type": "split", "direction": "down" if top else "right", "ratio": .5 if top else .74,
        "first": leaf("OMS team", watcher) if top else leaf(provider, command),
        "second": leaf(provider, command) if top else leaf("OMS tasks", watcher)}})


def zoom(enabled):
    request("pane.zoom", {"pane_id": os.environ.get("HERDR_PANE_ID"), "mode": "on" if enabled else "off"})
