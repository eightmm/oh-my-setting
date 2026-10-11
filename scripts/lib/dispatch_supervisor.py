#!/usr/bin/env python3
"""Per-call Linux descendant ownership and private controller receipts.

This is lifetime tracking, not an access-control boundary against hostile code
running as the controller's UID. Repository artifacts alone never authorize release.
"""

import ctypes
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time


def encoded(value):
    return json.dumps(value, sort_keys=True, allow_nan=False).encode('utf-8')


def store(repo):
    # Deliberately outside the worker's repository and artifact namespaces.
    uid = str(os.getuid()) if hasattr(os, 'getuid') else hashlib.sha256(str(Path.home()).encode()).hexdigest()[:16]
    temporary_root = Path('/tmp') if os.name == 'posix' else Path(tempfile.gettempdir())
    root = temporary_root / ('oms-dispatch-' + uid)
    if Path(repo).resolve() == root.resolve() or Path(repo).resolve() in root.resolve().parents:
        raise ValueError('dispatch controller state must be outside the repository')
    digest = hashlib.sha256(str(Path(repo).resolve()).encode()).hexdigest()
    for directory in (root, root / digest):
        try:
            directory.mkdir(mode=0o700)
        except FileExistsError:
            pass
        info = directory.lstat()
        if (not stat.S_ISDIR(info.st_mode) or os.name == "posix" and info.st_mode & 0o077
                or hasattr(os, 'getuid') and info.st_uid != os.getuid()):
            raise ValueError('unsafe dispatch controller directory')
    return root / digest


def read(repo, name):
    path = store(repo) / name
    fd = os.open(str(path), os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or os.name == "posix" and info.st_mode & 0o077
                or hasattr(os, 'getuid') and info.st_uid != os.getuid()):
            raise ValueError('unsafe dispatch controller record')
        data = stream.read(65537)
    if len(data) > 65536:
        raise ValueError('dispatch controller record too large')
    return json.loads(data)


def sync_directory(directory):
    if os.name == 'posix':
        fd = os.open(str(directory), os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0) | getattr(os, 'O_NOFOLLOW', 0))
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def write(repo, name, value, exclusive=False):
    directory = store(repo)
    if exclusive:
        fd = os.open(str(directory / name), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(encoded(value))
            stream.flush()
            os.fsync(stream.fileno())
        sync_directory(directory)
        return
    fd, temporary = tempfile.mkstemp(dir=str(directory))
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(encoded(value))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, str(directory / name))
        sync_directory(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def generation(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or os.name == "posix" and info.st_mode & 0o077:
        raise ValueError('unsafe dispatch launch record')
    return [info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns]


def supervise(repo, member, digest, argv):
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform != 'linux' or libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError('Linux child subreaper unavailable')
    received = []
    def interrupted(number, frame):
        if not received:
            received.append((number, time.monotonic()))
    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(number, interrupted)
    receipt = dict(launch_sha256=digest, pid=os.getpid(), all_reaped=False, exit=None)
    write(repo, member + '-supervisor.json', receipt)
    child = subprocess.Popen(argv)
    status, forwarded, sent, published = None, False, set(), False
    while True:
        try:
            pid, result = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            # Only ECHILD after installing the subreaper before spawn proves exhaustion.
            if status is None:
                raise ValueError('provider outcome unavailable')
            receipt.update(all_reaped=True, exit=status)
            write(repo, member + '-supervisor.json', receipt)
            return status
        if pid == child.pid:
            status = os.waitstatus_to_exitcode(result) if hasattr(os, 'waitstatus_to_exitcode') else (
                os.WEXITSTATUS(result) if os.WIFEXITED(result) else -os.WTERMSIG(result))
            child.returncode = status
            if status < 0:
                status = 128 - status
            receipt['exit'] = status
        if received:
            number, started = received[0]
            escalation = time.monotonic() - started >= 20
            if not forwarded:
                if status is None:
                    try:
                        # An unreaped direct child's PID cannot be reused. Avoid
                        # Popen.send_signal(), whose poll could consume its wait status.
                        os.kill(child.pid, number)
                    except ProcessLookupError:
                        pass
                forwarded = True
            # Give the provider's trap its cleanup grace. Once it exits (or
            # exceeds that grace), stop survivors without killing the subreaper.
            if status is not None or escalation:
                from terminal_panel import _dispatch_descendants
                rows = _dispatch_descendants(os.getpid())
                if rows is not None:
                    import panel_terminals
                    for row in rows:
                        key = (row['pid'], row['identity'], escalation)
                        if key not in sent and panel_terminals.matches(row['pid'], row['identity']):
                            try:
                                os.kill(row['pid'], signal.SIGKILL if escalation else number)
                                sent.add(key)
                            except ProcessLookupError:
                                pass
                elif escalation and status is None and child.pid not in sent:
                    try:
                        os.kill(child.pid, signal.SIGKILL)
                        sent.add(child.pid)
                    except ProcessLookupError:
                        pass
        if pid == 0:
            if status is not None and not published:
                try:
                    write(repo, member + '-supervisor.json', receipt)
                    published = True
                except (OSError, ValueError):
                    # Persistence failure must not abandon adopted children.
                    pass
            time.sleep(.05)


if __name__ == '__main__':
    try:
        sys.exit(supervise(sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]))
    except (OSError, ValueError):
        # No exhaustion receipt on initialization, spawn, or observation failure.
        sys.exit(1)
