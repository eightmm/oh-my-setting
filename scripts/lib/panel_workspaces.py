"""Ordinary-terminal workspace: a separate owned tmux session for shells, jobs and their controls.

The workspace is never a main: it has its own private 'workspaces' record, a session-local
key table (F4 controls, F5 origin) and no room or model involvement. Root keys and global
tmux options are untouched, so shell keys such as F6/F7 reach the shell.
"""

import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
import uuid

import panel_terminals as terminals

SESSION_PREFIX = "oms_ws_"
HELP = "F4 controls   F5 back to origin   (other keys, including F6/F7, go to the shell)"
KEYS = (("F4", "focus"), ("F5", "origin"))
ACTIONS = tuple(action for _, action in KEYS)


def _fields(target, fmt):
    return terminals.tmux("display-message", "-p", "-t", target, fmt).split("\t")


def _command(repo, ident, base):
    return "exec " + shlex.join([sys.executable, str(Path(__file__).resolve()), "_control",
                                 str(repo), ident, str(base)])


def _key_command(repo, ident, base, action):
    # '#' would start a tmux format; the trailing ids are expanded by tmux and validated by _key.
    fixed = shlex.join([sys.executable, str(Path(__file__).resolve()), "_key", str(repo), ident, str(base), action])
    return ["run-shell", "-b", fixed.replace("#", "##") + " '#{session_id}' '#{pane_id}'"]


def _server(proof):
    return {key: proof[key] for key in ("server_pid", "server_identity", "socket")}


def _caller(repo, origin_session):
    """The calling pane must live in the explicitly named, repository-owned session."""
    proof = terminals.session_proof(repo, origin_session)
    pane = os.environ.get("TMUX_PANE", "")
    if not re.fullmatch(r"%[0-9]+", pane):
        raise ValueError("run this from a pane of the origin session")
    fields = _fields(pane, "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}")
    if len(fields) != 5 or fields[0] != origin_session or fields[2] != pane or fields[4] != "0":
        raise ValueError("caller pane is not in the origin session")
    pid = int(fields[3])
    current = terminals.process(pid)
    if not current:
        raise ValueError("caller pane process identity unavailable")
    return dict(_server(proof), session=origin_session, window=fields[1], pane=pane, pid=pid,
                identity=current["identity"])


def origin_proof(repo, row):
    origin = row["origin"]
    proof = terminals.session_proof(repo, origin["session"])
    if _server(proof) != _server(origin):
        raise ValueError("origin server identity changed")
    fields = _fields(origin["pane"], "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}")
    if (fields != [origin["session"], origin["window"], origin["pane"], str(origin["pid"]), "0"]
            or not terminals.matches(origin["pid"], origin["identity"])):
        raise ValueError("origin pane changed")
    return {key: origin[key] for key in ("session", "window", "pane")}


def workspace_proof(repo, row):
    proof = terminals.session_proof(repo, row["session"])
    if _server(proof) != _server(row["origin"]):
        raise ValueError("workspace server identity changed")
    if terminals.tmux("show-options", "-v", "-t", row["session"], "key-table") != row["key_table"]:
        raise ValueError("workspace key table changed")
    base = row["state_home"]
    expected = {key: ["bind-key", "-T", row["key_table"], key, *_key_command(repo, row["id"], base, action)]
                for key, action in KEYS}
    keys = terminals.tmux("list-keys", "-T", row["key_table"]).splitlines()
    try:
        bound = sorted(shlex.split(line) for line in keys)
    except ValueError:
        bound = None
    expected_values = list(expected.values())
    remaining = list(expected_values)
    if bound is None or len(bound) != len(remaining):
        raise ValueError("workspace key bindings changed")
    for actual in bound:
        match = next((index for index, value in enumerate(remaining)
                      if terminals._same_tmux_tokens(actual, value)), None)
        if match is None:
            raise ValueError("workspace key bindings changed")
        remaining.pop(match)
    if remaining:
        raise ValueError("workspace key bindings changed")
    if not re.fullmatch(r"%[0-9]+", row.get("pane", "")):
        raise ValueError("workspace launch has not completed")
    fields = _fields(row["pane"], "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t"
                     "#{@oms_workspace_id}\t#{pane_start_command}")
    expected = [row["session"], row.get("window"), row["pane"], str(row.get("pid")), "0", row["id"]]
    if (len(fields) != 7 or fields[:6] != expected
            or not terminals._same_launcher(fields[6], _command(repo, row["id"], row["state_home"]))
            or not terminals.matches(row.get("pid"), row.get("identity"))):
        raise ValueError("workspace control pane or launcher identity changed")
    return {key: row[key] for key in ("session", "window", "pane")}


def public(repo, row):
    try:
        workspace_proof(repo, row)
        state = "running"
    except (ValueError, OSError, KeyError, subprocess.SubprocessError):
        state = "gone" if workspace_gone(repo, row) else "unknown"
    return {"id": row["id"], "label": terminals.label(row["label"]), "kind": "workspace", "state": state,
            "capabilities": {"focus": state == "running", "forget": state == "gone"}}


def list_workspaces(repo):
    terminals.parent_only()
    rows = []
    for ident in terminals.identifiers(repo, "workspaces"):
        with terminals.Store(repo, "workspaces", ident) as store:
            rows.append(public(repo, terminals.validate_record(store, ident, "workspace")))
    return rows


def _gone(row):
    """True only when tmux positively reports that the workspace session no longer exists."""
    result = subprocess.run(["tmux", "has-session", "-t", row["session"]], stdin=subprocess.DEVNULL,
                            capture_output=True, timeout=4, check=False)
    return result.returncode != 0 and b"can't find" in result.stderr


def workspace_gone(repo, row):
    """Require the original live server to reject the exact session ID."""
    origin = row.get("origin")
    if not isinstance(origin, dict) or not re.fullmatch(r"\$[0-9]+", row.get("session", "")):
        return False
    try:
        if not terminals.process_gone(row.get("pid"), row.get("identity")):
            return False
        if terminals.process_gone(origin.get("server_pid"), origin.get("server_identity")):
            return True
        proof = terminals.tmux("display-message", "-p", "#{pid}\t#{socket_path}").split("\t")
        if proof != [str(origin.get("server_pid")), origin.get("socket")] or not terminals.matches(
                origin.get("server_pid"), origin.get("server_identity")):
            return False
        return _gone(row)
    except (ValueError, OSError, KeyError, subprocess.SubprocessError):
        return False


def forget_workspace(repo, ident):
    def proof(store, row):
        if not workspace_gone(repo, row):
            raise ValueError("workspace disappearance is not proven")
    return terminals.forget_record(repo, "workspaces", ident, proof, ("record",))


def _navigate(repo, row, target, pane):
    from panel_chats import terminal_commands
    os.environ["TMUX_PANE"] = pane
    for command in terminal_commands(target):
        workspace_proof(repo, row)
        terminals.tmux(*command[1:])


def _related(repo, origin):
    """Records bound to this exact caller; live ones are reusable, unproven ones refuse."""
    live = []
    for ident in terminals.identifiers(repo, "workspaces"):
        with terminals.Store(repo, "workspaces", ident) as store:
            row = terminals.validate_record(store, ident, "workspace")
        if row.get("origin") != origin:
            continue
        try:
            workspace_proof(repo, row)
            live.append(row)
        except (ValueError, OSError, KeyError, subprocess.SubprocessError):
            if not row.get("session") or not _gone(row):
                raise ValueError("an existing workspace for this pane is unknown; nothing was changed")
    if len(live) > 1:
        raise ValueError("several workspaces match this pane; nothing was changed")
    return live


def open_workspace(repo, origin_session):
    terminals.parent_only()
    terminals.supported()
    repo = terminals.canonical(repo)
    origin = _caller(repo, origin_session)
    caller = os.environ["TMUX_PANE"]
    # One private root lock covers lookup, allocation and launch, so concurrent openers for one
    # pane cannot both create a workspace. Only record locks are taken inside it (never new_store's).
    with terminals.Store(repo, "workspaces", create=True) as root, root.lock(create=True):
        live = _related(repo, origin)
        if live:
            row = live[0]
            _navigate(repo, row, {key: row[key] for key in ("session", "window", "pane")}, caller)
            return public(repo, row)
        if sum(bool(terminals.ID.fullmatch(name)) for name in os.listdir(root.fd)) >= terminals.MAX_ITEMS:
            raise ValueError("managed item limit reached")
        ident = "term-" + uuid.uuid4().hex
        os.mkdir(ident, 0o700, dir_fd=root.fd)
        with terminals.Store(repo, "workspaces", ident) as store:
            os.close(store.file("lock", os.O_RDWR | os.O_EXCL, create=True))
            base = str(terminals.state_home())
            table = "oms-ws-" + ident[5:]
            row = dict(schema=1, id=ident, repo=str(repo), uid=os.getuid(), kind="workspace", label="Workspace",
                       state_home=base, origin=origin, key_table=table)
            store.write(row)
            made = terminals.tmux("new-session", "-d", "-P", "-F", "#{session_id}\t#{window_id}\t#{pane_id}",
                                  "-s", SESSION_PREFIX + ident[5:17], "-n", "Controls", "-c", str(repo),
                                  _command(repo, ident, base)).split("\t")
            if (len(made) != 3 or not re.fullmatch(r"\$[0-9]+", made[0]) or not re.fullmatch(r"@[0-9]+", made[1])
                    or not re.fullmatch(r"%[0-9]+", made[2])):
                raise ValueError("workspace launch binding unavailable")
            # The launcher waits for row["pane"], so every binding below precedes it.
            terminals.tmux("set-option", "-t", made[0], "@oms_panel_repo", str(repo))
            terminals.tmux("set-option", "-p", "-t", made[2], "@oms_workspace_id", ident)
            terminals.tmux("set-option", "-t", made[0], "key-table", table)
            for key, action in KEYS:
                terminals.tmux("bind-key", "-T", table, key, *_key_command(repo, ident, base, action))
            with store.lock():
                row.update(session=made[0], window=made[1], pane=made[2])
                store.write(row)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                current = terminals.validate_record(store, ident, "workspace")
                if current.get("identity"):
                    _navigate(repo, current, workspace_proof(repo, current), caller)
                    return public(repo, current)
                time.sleep(.03)
    raise ValueError("workspace launch unconfirmed; no existing pane was changed")


def _control(repo, ident, base):
    terminals.supported()
    deadline = time.monotonic() + 5
    with terminals.Store(repo, "workspaces", ident, base=base) as store:
        while time.monotonic() < deadline:
            with store.lock():
                row = terminals.validate_record(store, ident, "workspace")
                if row.get("pane"):
                    if os.environ.get("TMUX_PANE") != row["pane"] or row.get("identity"):
                        raise ValueError("launcher binding mismatch")
                    current = terminals.process(os.getpid())
                    row.update(pid=os.getpid(), identity=current["identity"])
                    workspace_proof(repo, row)
                    origin_proof(repo, row)
                    store.write(row)
                    break
            time.sleep(.03)
        else:
            raise ValueError("launcher binding timed out")
    clean = terminals.ordinary_environment()
    os.environ.clear()
    os.environ.update(clean)
    os.environ["XDG_STATE_HOME"] = row["state_home"]
    from panel_controls import browse_controls
    while True:
        size = shutil.get_terminal_size((80, 24))
        print(HELP)
        browse_controls(repo, session=row["session"], width=size.columns, height=max(size.lines - 2, 3))
        try:
            input("Controls closed (shells and jobs untouched). Enter reopens: ")
        except EOFError:
            return


def _key(repo, ident, base, action, session, pane):
    terminals.parent_only()
    if (action not in ACTIONS or not re.fullmatch(r"\$[0-9]+", session)
            or not re.fullmatch(r"%[0-9]+", pane)):
        raise ValueError("invalid key callback")
    with terminals.Store(repo, "workspaces", ident, base=base) as store:
        row = terminals.validate_record(store, ident, "workspace")
    if session != row["session"] or _fields(pane, "#{session_id}")[0] != row["session"]:
        raise ValueError("caller is not in this workspace")
    focus = workspace_proof(repo, row)
    _navigate(repo, row, focus if action == "focus" else origin_proof(repo, row), pane)


if __name__ == "__main__":
    try:
        if len(sys.argv) == 5 and sys.argv[1] == "_control":
            _control(sys.argv[2], sys.argv[3], sys.argv[4])
        elif len(sys.argv) == 8 and sys.argv[1] == "_key":
            _key(*sys.argv[2:])
        else:
            sys.exit(2)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print("workspace action refused: %s" % error, file=sys.stderr)
        sys.exit(1)
