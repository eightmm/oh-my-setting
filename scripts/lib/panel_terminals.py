"""Owned interactive terminals; explicit reads only, never native AI transcripts.

Process start identities bind a private record to the exact tmux server and
launcher PID. A shell may exec another program without losing that identity.
The same private-file primitives are used by panel_jobs; no room events occur.
"""

import argparse
import contextlib
try:
    import fcntl
except ImportError:  # Core OMS remains importable on hosts without POSIX ownership.
    fcntl = None
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import time
import uuid

from work_journal import sanitize_multiline, sanitize_text

ID = re.compile(r"(?:term|job)-[0-9a-f]{32}\Z")
MAX_RECORD = 65536
MAX_ITEMS = 128
MAX_READ = 32768
_PROCESS_HELPERS = None


def parent_only():
    if (os.environ.get("OMS_HARNESS_CHILD") == "1"
            or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"):
        raise ValueError("workers cannot control owner terminals or jobs")


def supported():
    if os.name != "posix" or fcntl is None or not hasattr(os, "O_NOFOLLOW"):
        raise ValueError("managed terminals require POSIX tmux and no-follow private files")


def canonical(repo):
    path = Path(repo).resolve(strict=True)
    if not path.is_dir():
        raise ValueError("repository must be a directory")
    return path


def _process_helpers():
    global _PROCESS_HELPERS
    if _PROCESS_HELPERS is None:
        path = Path(__file__).with_name("attempt-runner.py")
        spec = importlib.util.spec_from_file_location("oms_panel_process_helpers", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _PROCESS_HELPERS = module
    return _PROCESS_HELPERS


def process(pid):
    """Reuse attempt-runner's boot/start identity, including its POSIX ps fallback."""
    if type(pid) is not int or pid <= 0:
        return None
    identity = _process_helpers().process_identity(pid)
    if not identity:
        return None
    try:
        if sys.platform == "linux":
            fields = Path("/proc/%s/stat" % pid).read_text().rsplit(")", 1)[1].split()
            state, parent, group = fields[0], int(fields[1]), int(fields[2])
        else:
            output = subprocess.run(["ps", "-o", "ppid=,pgid=,stat=", "-p", str(pid)],
                                    capture_output=True, text=True, timeout=3, check=False)
            fields = output.stdout.replace("\r", "").split()
            if output.returncode or len(fields) != 3:
                return None
            parent, group, state = int(fields[0]), int(fields[1]), fields[2][0]
        if _process_helpers().process_identity(pid) != identity:
            return None
        return {"pid": pid, "identity": identity, "state": state, "parent": parent, "group": group}
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        return None


def state_home():
    path = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state").expanduser()
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("XDG_STATE_HOME must be an absolute normalized path")
    return path


def state_directory(repo):
    """Private user-local path; never part of a repository or shared OMS state."""
    repo = canonical(repo)
    base = state_home()
    if base == repo or repo in base.parents:
        raise ValueError("private terminal/job state must be outside the repository")
    return base / "oh-my-setting" / "panel-control" / hashlib.sha256(str(repo).encode()).hexdigest()


def matches(pid, identity):
    current = process(pid)
    return bool(current and current["state"] not in {"Z", "X"} and current["identity"] == identity)


def process_gone(pid, identity):
    """Require positive PID disappearance or an observed different/dead identity."""
    if type(pid) is not int or pid <= 0 or not identity:
        return False
    current = process(pid)
    if current:
        return current["identity"] != identity or current["state"] in {"Z", "X"}
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        pass
    return False


def ordinary_environment():
    # These are execution identities, not the user's configuration or credentials.
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("OMS_", "CLAUDE_CODE_", "CODEX_THREAD_", "CODEX_SESSION_",
                                   "AGY_SESSION_", "ANTIGRAVITY_SESSION_"))
            and key not in {"CLAUDECODE", "CLAUDE_SESSION_ID", "CODEX_INTERNAL_ORIGINATOR_OVERRIDE"}}


def preview(raw, maximum=MAX_READ):
    if type(maximum) is not int or not 1 <= maximum <= MAX_READ:
        raise ValueError("read limit must be between 1 and 32768 bytes")
    source_truncated = len(raw) > MAX_RECORD
    if source_truncated:
        raw = raw[:MAX_RECORD].rsplit(b"\n", 1)[0] if b"\n" in raw[:MAX_RECORD] else b""
    text = raw.decode("utf-8", "replace")
    text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\|$)", "", text)
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]", "", text)
    text = "".join(c for c in text if c in "\n\t" or (ord(c) >= 32 and not 127 <= ord(c) <= 159))
    # Omit oversized complete lines before regex redaction: never expose a
    # partially cut credential, and bound the existing scrubber's regex cost.
    lines = text.splitlines()
    source_truncated = source_truncated or any(len(line) > 1024 for line in lines)
    text = "\n".join(line if len(line) <= 1024 else "[oversized line omitted]" for line in lines)
    safe = sanitize_multiline(text, max(len(text.encode()) * 2, MAX_READ), MAX_RECORD)
    return {"text": safe.encode()[:maximum].decode("utf-8", "ignore"),
            "truncated": source_truncated or len(safe.encode()) > maximum}


def label(value):
    return sanitize_text(preview(str(value).encode(), 256)["text"], 120) or "Terminal"


class Store:
    """Directory-fd anchored private records; unsafe objects are never repaired."""

    def __init__(self, repo, kind, ident=None, create=False, base=None):
        supported()
        self.repo = canonical(repo)
        self.state_home = Path(base) if base is not None else state_home()
        if (not self.state_home.is_absolute() or ".." in self.state_home.parts
                or self.state_home == self.repo or self.repo in self.state_home.parents):
            raise ValueError("private state must be absolute and outside the repository")
        if kind not in {"terminals", "jobs", "workspaces"} or ident is not None and not ID.fullmatch(ident):
            raise ValueError("invalid managed kind or ID")
        self.fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            namespace = hashlib.sha256(str(self.repo).encode()).hexdigest()
            components = self.state_home.parts[1:]
            names = components + ("oh-my-setting", "panel-control", namespace, kind) + ((ident,) if ident else ())
            for index, name in enumerate(names):
                if create:
                    try:
                        os.mkdir(name, 0o700, dir_fd=self.fd)
                    except FileExistsError:
                        pass
                child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.fd)
                info = os.fstat(child)
                private = index > len(components)
                checked = index >= len(components) - 1
                if checked and (info.st_uid != os.getuid() or info.st_mode & (0o077 if private else 0o022)):
                    os.close(child)
                    raise ValueError("unsafe private directory")
                os.close(self.fd)
                self.fd = child
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def __enter__(self):
        return self

    def __exit__(self, *unused):
        self.close()

    def file(self, name, flags=os.O_RDONLY, create=False):
        if not re.fullmatch(r"[a-z0-9.-]+", name):
            raise ValueError("invalid private filename")
        fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK | (os.O_CREAT if create else 0),
                     0o600, dir_fd=self.fd)
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != os.getuid() or info.st_mode & 0o077):
            os.close(fd)
            raise ValueError("unsafe private file")
        return fd

    def read(self, name="record"):
        fd = self.file(name)
        try:
            raw = os.read(fd, MAX_RECORD + 1)
            if len(raw) > MAX_RECORD:
                raise ValueError("oversized private record")
            value = json.loads(raw)
            if not isinstance(value, dict):
                raise ValueError("invalid private record")
            return value
        finally:
            os.close(fd)

    def write(self, value, name="record"):
        try:
            fd = self.file(name)
        except FileNotFoundError:
            pass
        else:
            os.close(fd)
        raw = json.dumps(value, ensure_ascii=True).encode()
        if len(raw) > MAX_RECORD:
            raise ValueError("oversized private record")
        temp = "tmp-" + uuid.uuid4().hex
        fd = self.file(temp, os.O_WRONLY | os.O_EXCL, create=True)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
            os.rename(temp, name, src_dir_fd=self.fd, dst_dir_fd=self.fd)
        finally:
            try:
                os.unlink(temp, dir_fd=self.fd)
            except FileNotFoundError:
                pass

    @contextlib.contextmanager
    def lock(self, create=False):
        fd = self.file("lock", os.O_RDWR, create=create)
        try:
            deadline = time.monotonic() + 3
            while True:
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise ValueError("managed record is busy")
                    time.sleep(.02)
            yield
        finally:
            os.close(fd)


def identifiers(repo, kind):
    try:
        with Store(repo, kind) as store:
            values = os.listdir(store.fd)
            if len(values) > MAX_ITEMS + 1:
                raise ValueError("managed item limit exceeded")
            return sorted(value for value in values if ID.fullmatch(value))
    except FileNotFoundError:
        return []


def new_store(repo, kind, prefix):
    with Store(repo, kind, create=True) as root:
        with root.lock(create=True):
            if sum(bool(ID.fullmatch(name)) for name in os.listdir(root.fd)) >= MAX_ITEMS:
                raise ValueError("managed item limit reached")
            ident = prefix + "-" + uuid.uuid4().hex
            os.mkdir(ident, 0o700, dir_fd=root.fd)
    with Store(repo, kind, ident) as store:
        os.close(store.file("lock", os.O_RDWR | os.O_EXCL, create=True))
    return ident


def forget_record(repo, kind, ident, proof, files):
    """Remove one verified record under the allocator and record locks."""
    parent_only()
    with Store(repo, kind) as root, root.lock():
        with Store(repo, kind, ident) as store, store.lock():
            row = validate_record(store, ident, {"terminals": "shell", "jobs": "job",
                                                  "workspaces": "workspace"}[kind])
            proof(store, row)
            names = set(os.listdir(store.fd))
            if names != set(files) | {"lock"}:
                raise ValueError("unexpected private record contents")
            for name in names:
                os.close(store.file(name))
            for name in files:
                os.unlink(name, dir_fd=store.fd)
            os.unlink("lock", dir_fd=store.fd)
        os.rmdir(ident, dir_fd=root.fd)
    return {"id": ident, "kind": kind, "forgotten": True,
            "notice": "Private history and any retained log were permanently deleted."}


def tmux(*args):
    result = subprocess.run(["tmux", *args], stdin=subprocess.DEVNULL,
                            capture_output=True, timeout=4, check=False)
    if result.returncode:
        raise ValueError("tmux target unavailable")
    return result.stdout.decode("utf-8", "replace").replace("\r", "").strip()


def session_proof(repo, session):
    supported()
    repo = canonical(repo)
    if not isinstance(session, str) or not re.fullmatch(r"\$[0-9]+", session):
        raise ValueError("explicit tmux session ID required")
    fields = tmux("display-message", "-p", "-t", session,
                  "#{session_id}\t#{pid}\t#{socket_path}").split("\t")
    owner = tmux("show-options", "-v", "-t", session, "@oms_panel_repo")
    if len(fields) != 3 or fields[0] != session or owner != tmux_display(str(repo)):
        raise ValueError("session is not owned by this repository")
    server = process(int(fields[1]))
    if not server:
        raise ValueError("tmux server identity unavailable")
    return {"session": session, "server_pid": server["pid"], "server_identity": server["identity"],
            "socket": fields[2]}


def validate_record(store, ident, kind):
    row = store.read()
    if (row.get("schema") != 1 or row.get("id") != ident or row.get("kind") != kind
            or row.get("repo") != str(store.repo) or row.get("uid") != os.getuid()
            or row.get("state_home") != str(store.state_home)
            or not isinstance(row.get("label"), str)):
        raise ValueError("foreign or invalid managed record")
    return row


def _shell_command(repo, ident, base):
    return "exec " + shlex.join([sys.executable, str(Path(__file__).resolve()), "_shell", str(repo), ident, str(base)])


def _same_launcher(display, expected):
    try:
        return _same_tmux_tokens(shlex.split(display), [expected])
    except ValueError:
        return False


def tmux_display(text):
    """tmux 3.4 prints strings via utf8_strvis(VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH): a dollar gains a
    backslash only before an ASCII letter, underscore or brace. Controls are not faithfully
    representable, so they yield None and never match."""
    if re.search(r"[\x00-\x1f\x7f]", text):
        return None
    return re.sub(r"\$(?=[A-Za-z_{])", lambda _: "\\$", text)


def _same_tmux_tokens(actual, expected):
    shown = [tmux_display(token) for token in expected]
    return None not in shown and actual == shown


def terminal_proof(repo, row):
    proof = session_proof(repo, row["session"])
    if any(proof[key] != row.get(key) for key in proof):
        raise ValueError("terminal server identity changed")
    if not re.fullmatch(r"%[0-9]+", row.get("pane", "")):
        raise ValueError("terminal launch has not completed")
    fields = tmux("display-message", "-p", "-t", row["pane"],
                  "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t"
                  "#{@oms_terminal_id}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_attempt}\t#{pane_start_command}").split("\t")
    expected = [row["session"], row.get("window"), row["pane"], str(row.get("pid")), "0",
                row["id"], "", ""]
    expected_command = _shell_command(repo, row["id"], row["state_home"])
    if (len(fields) != 9 or fields[:8] != expected
            or not _same_launcher(fields[8], expected_command)
            or not matches(row.get("pid"), row.get("identity"))):
        raise ValueError("terminal pane or launcher identity changed")
    return {"session": row["session"], "window": row["window"], "pane": row["pane"]}


def terminal_gone(repo, row):
    """Require the exact launcher gone and positive server/pane disappearance."""
    if not re.fullmatch(r"%[0-9]+", row.get("pane", "")):
        return False
    if not process_gone(row.get("pid"), row.get("identity")):
        return False
    if process_gone(row.get("server_pid"), row.get("server_identity")):
        return True
    try:
        proof = tmux("display-message", "-p", "#{pid}\t#{socket_path}").split("\t")
        if proof != [str(row.get("server_pid")), row.get("socket")] or not matches(
                row.get("server_pid"), row.get("server_identity")):
            return False
        panes = tmux("list-panes", "-a", "-F", "#{pane_id}").splitlines()
        return row["pane"] not in panes
    except (ValueError, OSError, KeyError, subprocess.SubprocessError):
        return False


def forget_terminal(repo, ident):
    def proof(store, row):
        if not terminal_gone(repo, row):
            raise ValueError("terminal disappearance is not proven")
    return forget_record(repo, "terminals", ident, proof, ("record",))


def public_terminal(repo, row):
    try:
        terminal_proof(repo, row)
        state = "running"
    except (ValueError, OSError, KeyError, subprocess.SubprocessError):
        state = "gone" if terminal_gone(repo, row) else "unknown"
    return {"id": row["id"], "label": label(row["label"]), "kind": "shell", "state": state,
            "capabilities": {"focus": state == "running", "read": state == "running",
                             "restart": False, "forget": state == "gone"}}


def open_terminal(repo, session, *, cwd=None, label="Terminal"):
    parent_only()
    repo = canonical(repo)
    binding = session_proof(repo, session)
    cwd = canonical(cwd if cwd is not None else repo)
    shell = os.environ.get("SHELL") or "/bin/sh"
    if not os.path.isabs(shell) or not os.access(shell, os.X_OK) or not Path(shell).is_file():
        raise ValueError("interactive shell unavailable")
    ident = new_store(repo, "terminals", "term")
    row = dict(binding, schema=1, id=ident, repo=str(repo), uid=os.getuid(), kind="shell",
               label=label, cwd=str(cwd), shell=shell, state_home=str(state_home()))
    with Store(repo, "terminals", ident) as store:
        store.write(row)
        command = _shell_command(repo, ident, row["state_home"])
        result = tmux("new-window", "-d", "-P", "-F", "#{window_id}\t#{pane_id}",
                      "-t", session + ":", "-c", str(cwd), "-n", "Terminal", command).split("\t")
        if len(result) != 2 or not re.fullmatch(r"@[0-9]+", result[0]) or not re.fullmatch(r"%[0-9]+", result[1]):
            raise ValueError("terminal launch binding unavailable")
        tmux("set-option", "-p", "-t", result[1], "@oms_terminal_id", ident)
        with store.lock():
            row.update(window=result[0], pane=result[1])
            store.write(row)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            current = validate_record(store, ident, "shell")
            if current.get("identity"):
                terminal_proof(repo, current)
                return public_terminal(repo, current)
            time.sleep(.03)
    raise ValueError("terminal launch unconfirmed; no existing pane was changed")


def list_terminals(repo):
    parent_only()
    supported()
    rows = []
    for ident in identifiers(repo, "terminals"):
        with Store(repo, "terminals", ident) as store:
            rows.append(public_terminal(repo, validate_record(store, ident, "shell")))
    return rows


def focus_terminal(repo, ident):
    parent_only()
    with Store(repo, "terminals", ident) as store:
        row = validate_record(store, ident, "shell")
        target = terminal_proof(repo, row)
        from panel_chats import terminal_commands
        commands = terminal_commands(target)
        for command in commands:
            terminal_proof(repo, row)
            tmux(*command[1:])
        return public_terminal(repo, row)


def read_terminal(repo, ident, *, maximum=MAX_READ, lines=200):
    parent_only()
    if type(lines) is not int or not 1 <= lines <= 2000:
        raise ValueError("line limit must be between 1 and 2000")
    with Store(repo, "terminals", ident) as store:
        row = validate_record(store, ident, "shell")
        target = terminal_proof(repo, row)
        import selectors
        child = subprocess.Popen(["tmux", "capture-pane", "-p", "-t", target["pane"],
                                  "-S", "-" + str(lines)], stdout=subprocess.PIPE,
                                 stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
        raw = bytearray()
        size = 0
        selector = selectors.DefaultSelector()
        try:
            os.set_blocking(child.stdout.fileno(), False)
            selector.register(child.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + 4
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise ValueError("terminal capture timed out")
                for key, unused in selector.select(.05):
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                    size += len(data)
                    raw.extend(data[:max(0, MAX_RECORD - len(raw))])
            if child.wait(timeout=1):
                raise ValueError("terminal capture unavailable")
        finally:
            selector.close()
            child.stdout.close()
            if child.poll() is None:
                child.kill()  # Only this short-lived capture client, never its pane.
                child.wait()
        raw = bytes(raw)
        if size > len(raw):
            # Discard a partially captured final line before credential scrubbing.
            raw = raw.rsplit(b"\n", 1)[0] if b"\n" in raw else b""
        terminal_proof(repo, row)
        result = preview(raw, maximum)
        history = tmux("display-message", "-p", "-t", target["pane"], "#{history_size}")
        result.update(id=ident, kind="shell", truncated=result["truncated"] or size > len(raw) or int(history) > lines,
                      retention="tmux history; explicit bounded capture")
        return result


def _shell(repo, ident, base):
    supported()
    deadline = time.monotonic() + 5
    with Store(repo, "terminals", ident, base=base) as store:
        while time.monotonic() < deadline:
            with store.lock():
                row = validate_record(store, ident, "shell")
                if row.get("pane"):
                    if os.environ.get("TMUX_PANE") != row["pane"] or row.get("identity"):
                        raise ValueError("launcher binding mismatch")
                    proof = session_proof(repo, row["session"])
                    if any(row.get(key) != value for key, value in proof.items()):
                        raise ValueError("launcher session changed")
                    current = process(os.getpid())
                    row.update(pid=os.getpid(), identity=current["identity"])
                    terminal_proof(repo, row)
                    store.write(row)
                    os.chdir(row["cwd"])
                    os.execve(row["shell"], [row["shell"], "-i"], ordinary_environment())
            time.sleep(.03)
    raise ValueError("launcher binding timed out")


def cli_main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("open", "list", "focus", "read", "controls", "forget", "workspaces", "workspace-forget"))
    parser.add_argument("id", nargs="?")
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--session")
    parser.add_argument("--cwd", type=Path)
    parser.add_argument("--label", default="Terminal")
    parser.add_argument("--maximum", type=int, default=MAX_READ)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.action == "open":
            result = open_terminal(args.repo, args.session, cwd=args.cwd, label=args.label)
        elif args.action == "list":
            result = list_terminals(args.repo)
        elif args.action == "controls":
            import panel_workspaces
            result = panel_workspaces.open_workspace(args.repo, args.session)
        elif args.action == "workspace-forget":
            import panel_workspaces
            result = panel_workspaces.forget_workspace(args.repo, args.id)
        elif args.action == "workspaces":
            import panel_workspaces
            result = panel_workspaces.list_workspaces(args.repo)
        elif args.action == "forget":
            result = forget_terminal(args.repo, args.id)
        elif args.action == "focus":
            result = focus_terminal(args.repo, args.id)
        else:
            result = read_terminal(args.repo, args.id, maximum=args.maximum)
        print(json.dumps(result, ensure_ascii=True))
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("managed terminal action refused: ownership or target unavailable", file=sys.stderr)
        return 1


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "_shell":
        try:
            _shell(sys.argv[2], sys.argv[3], sys.argv[4])
        except (OSError, ValueError, KeyError, subprocess.SubprocessError):
            sys.exit(1)
    else:
        sys.exit(cli_main())
