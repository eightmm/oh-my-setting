#!/usr/bin/env python3
"""Per-call Linux descendant ownership and private controller receipts.

This is lifetime tracking, not an access-control boundary against hostile code
running as the controller's UID. Repository artifacts alone never authorize release.
"""

import ctypes
import hashlib
import json
import os
import re
import uuid
import panel_terminals
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time


def encoded(value):
    return json.dumps(value, sort_keys=True, allow_nan=False).encode('utf-8')


# Schema 1 wraps the unchanged caller payload. identity.json is immutable launch
# metadata; boot identity stays explicit even where process identities include it.
RECORD_SCHEMA = 1
_PINNED = {}
_MEMBERS = {}


def _valid_boot(value):
    return isinstance(value, str) and (value == 'unknown' or bool(re.fullmatch(
        r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', value)))


def boot_id():
    if sys.platform == 'linux':
        try:
            value = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
            if _valid_boot(value):
                return value
        except OSError:
            pass
    return 'unknown'


def _validate_state_ancestors(path):
    """Validate permissions on existing dispatch state ancestors."""
    for ancestor in (Path(path),) + tuple(Path(path).parents):
        try:
            info = ancestor.stat()
        except FileNotFoundError:
            continue
        # Writable ancestors are unsafe regardless of owner; only a root-owned
        # sticky directory prevents another group member renaming our child.
        root_uid = os.stat('/').st_uid
        if info.st_mode & 0o022:
            if not (info.st_mode & stat.S_ISVTX and info.st_uid == root_uid):
                name = ancestor.name or ancestor.anchor
                raise ValueError(
                    'unsafe writable dispatch state ancestor %r; set XDG_STATE_HOME to a private directory or chmod %r'
                    % (name, name))


def process_identity():
    pid = os.getpid()
    return {'pid': pid, 'identity': panel_terminals._process_helpers().process_identity(pid) or 'unknown'}


def _member(repo, name, value=None):
    if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9._-]+', name):
        raise ValueError('invalid dispatch controller filename')
    match = re.fullmatch(r'(call-[A-Za-z0-9][A-Za-z0-9._-]{0,159})-(?:launch|process|pending|supervisor|settled)\.json', name)
    if match:
        return match[1]
    if not re.fullmatch(r'settlement-[0-9a-f]{64}\.json', name):
        raise ValueError('invalid dispatch controller filename')
    key = (str(Path(repo).resolve()), name)
    member = value.get('member') if isinstance(value, dict) else _MEMBERS.get(key)
    if member is None:
        directory = _root(repo)
        try:
            with Directory(directory, create=False) as source:
                for candidate in os.listdir(source.fd):
                    if (re.fullmatch(r'call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}', candidate)
                            and name == 'settlement-' + hashlib.sha256(candidate.encode()).hexdigest() + '.json'):
                        member = candidate
                        break
        except FileNotFoundError:
            pass
    if member is None:
        try:
            with _legacy_directory(repo) as source:
                member = source.read(name).get('member')
        except FileNotFoundError:
            pass
    if (not isinstance(member, str) or not re.fullmatch(r'call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}', member)
            or name != 'settlement-' + hashlib.sha256(member.encode()).hexdigest() + '.json'):
        raise FileNotFoundError('dispatch settlement owner unavailable')
    _MEMBERS[key] = member
    return member


def _root(repo, member=None, execution_repo=None):
    repo = Path(repo).resolve()
    key = (str(repo), member)
    if member is not None and key in _PINNED:
        return _PINNED[key]
    # Only this controller's environment selects the namespace, never repository bytes.
    base = panel_terminals.state_home()
    # A controller may sanitize its environment between calls. Keep its already
    # resolved namespace when the explicit setting disappears; never switch to /tmp.
    if not os.environ.get('XDG_STATE_HOME'):
        pinned = _PINNED.get((str(repo), None))
        if pinned is not None:
            base = pinned.parents[3]
    root = base / 'oh-my-setting/panel-control' / hashlib.sha256(str(repo).encode()).hexdigest() / 'dispatch'
    resolved = root.resolve()
    _validate_state_ancestors(resolved)
    for forbidden in (repo, Path(execution_repo).resolve() if execution_repo else repo):
        if resolved == forbidden or forbidden in resolved.parents:
            raise ValueError('dispatch controller state must be outside the repository')
    # Recognize worktree indirection files and checkout/bare HEAD markers without
    # spawning another process during launch. An empty .git sandbox sentinel
    # alone is not a repository.
    for ancestor in (resolved,) + tuple(resolved.parents):
        marker = ancestor / '.git'
        if (marker.is_file() or marker.is_symlink() or (marker / 'HEAD').exists()
                or (ancestor / 'HEAD').is_file() and (ancestor / 'objects').is_dir()):
            raise ValueError('dispatch controller state must be outside every repository')
    if member is not None:
        _PINNED[key] = root
        _PINNED[(str(repo), None)] = root
    return root


class Directory(panel_terminals.Store):
    """Reuse the managed-control file checks and anchor every directory component."""

    def file(self, name, flags=os.O_RDONLY, create=False):
        # Dispatch member IDs use the bounded panel-call contract, including
        # uppercase letters, underscores, and dots.
        if not re.fullmatch(r'[A-Za-z0-9._-]+', name):
            raise ValueError('invalid private filename')
        fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK | (os.O_CREAT if create else 0),
                     0o600, dir_fd=self.fd)
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != os.getuid() or info.st_mode & 0o077):
            os.close(fd)
            raise ValueError('unsafe private file')
        return fd

    def __init__(self, path, create=False):
        panel_terminals.supported()
        path = Path(path)
        self.path = path
        if not path.is_absolute() or '..' in path.parts:
            raise ValueError('invalid dispatch controller root')
        self.fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            private = False
            component_path = Path('/')
            for name in path.parts[1:]:
                component_path = component_path / name
                if name == 'oh-my-setting':
                    info = os.fstat(self.fd)
                    if info.st_uid != os.getuid() or info.st_mode & 0o022:
                        raise ValueError('unsafe dispatch state home')
                private = private or name == 'oh-my-setting'
                created = False
                if create:
                    try:
                        os.mkdir(name, 0o700, dir_fd=self.fd)
                        os.fsync(self.fd)
                        created = True
                    except FileExistsError:
                        pass
                try:
                    child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=self.fd)
                except PermissionError as error:
                    if created:
                        os.rmdir(name, dir_fd=self.fd)
                        raise ValueError('restrictive umask prevented opening new dispatch directory %r; '
                                         'run with a umask that preserves owner rwx permissions'
                                         % component_path) from error
                    raise
                if created:
                    os.fchmod(child, 0o700)
                info = os.fstat(child)
                root_uid = os.stat('/').st_uid
                if info.st_mode & 0o022 and not (info.st_mode & stat.S_ISVTX and info.st_uid == root_uid):
                    os.close(child)
                    raise ValueError('unsafe writable dispatch state ancestor %r; set XDG_STATE_HOME to a private directory or chmod %r'
                                     % (name, name))
                # The state home and every OMS directory must belong to the invoking owner.
                check = private or name == path.parts[-1]
                if check and (info.st_uid != os.getuid() or info.st_mode & (0o077 if private else 0o022)):
                    os.close(child)
                    raise ValueError('unsafe dispatch controller directory')
                os.close(self.fd)
                self.fd = child
        except BaseException:
            self.close()
            raise


def _legacy(repo):
    uid = str(os.getuid())
    base = Path('/tmp') if os.name == 'posix' else Path(tempfile.gettempdir())
    return base / ('oms-dispatch-' + uid) / hashlib.sha256(str(Path(repo).resolve()).encode()).hexdigest()


def _legacy_directory(repo):
    # Check the private UID directory too; never create missing legacy state.
    path = _legacy(repo)
    with Directory(path.parent) as parent:
        info = os.fstat(parent.fd)
        if info.st_mode & 0o077 or info.st_uid != os.getuid():
            raise ValueError('unsafe legacy dispatch directory')
    return Directory(path)


def _record_exists(repo, path, name, legacy):
    try:
        with (_legacy_directory(repo) if legacy else Directory(path)) as directory:
            fd = directory.file(name)
            os.close(fd)
        return True
    except FileNotFoundError:
        return False


def _location(repo, member, name=None):
    path = _root(repo, member) / member
    if name is not None and _record_exists(repo, path, name, False):
        return path, False
    legacy_path = _legacy(repo)
    try:
        with _legacy_directory(repo):
            if name is None or _record_exists(repo, legacy_path, name, True):
                return legacy_path, True
    except FileNotFoundError:
        pass
    try:
        with Directory(path):
            return path, False
    except FileNotFoundError:
        return path, False


class RecordPaths:
    """Resolve historical flat logical names for private diagnostic callers."""

    def __init__(self, repo):
        self.repo = repo

    def __truediv__(self, name):
        member = _member(self.repo, name)
        path, unused = _location(self.repo, member, name)
        return path / name


def store(repo):
    return RecordPaths(repo)


def initialize(repo, member, call, execution_repo=None):
    root = _root(repo, member, execution_repo)
    identity = dict(schema=RECORD_SCHEMA, repo=str(Path(repo).resolve()), room=call['room'],
                    participant=member, parent=call['parent'], task_id=call.get('task_id'),
                    nonce=call.get('nonce'), boot_id=boot_id(), controller=process_identity(),
                    root=str(root / member))
    with Directory(root / member, create=True) as directory:
        _write(directory, 'identity.json', identity, exclusive=True)
    return identity


def orphan_members(repo):
    """Return member directories whose creation crashed before identity was written."""
    root = _root(repo)
    try:
        with Directory(root) as directory:
            names = os.listdir(directory.fd)
    except FileNotFoundError:
        return []
    result = []
    for member in names:
        if not re.fullmatch(r'call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}', member):
            continue
        try:
            with Directory(root / member) as directory:
                directory.read('identity.json')
        except FileNotFoundError:
            result.append(member)
    return result


def pinned_state_home(repo, member):
    return str(_root(repo, member).parents[3])


def _valid_process(value):
    return (isinstance(value, dict) and set(value) == {'pid', 'identity'}
            and type(value['pid']) is int and value['pid'] > 0
            and isinstance(value['identity'], str) and bool(value['identity']))


def _identity(directory, repo, member):
    try:
        identity = directory.read('identity.json')
    except FileNotFoundError as error:
        raise ValueError('dispatch launch identity unavailable') from error
    if (set(identity) != {'schema', 'repo', 'room', 'participant', 'parent', 'task_id',
                         'nonce', 'boot_id', 'controller', 'root'}
            or type(identity['schema']) is not int or identity['schema'] != RECORD_SCHEMA
            or identity['repo'] != str(Path(repo).resolve()) or identity['participant'] != member
            or identity['root'] != str(directory.path)
            or not _valid_boot(identity['boot_id']) or not _valid_process(identity['controller'])
            or any(identity[key] is not None and not isinstance(identity[key], str)
                   for key in ('room', 'parent', 'task_id', 'nonce'))):
        raise ValueError('invalid dispatch launch identity')
    return identity


def read(repo, name):
    member = _member(repo, name)
    path, legacy = _location(repo, member, name)
    with (_legacy_directory(repo) if legacy else Directory(path)) as directory:
        value = directory.read(name)
        if legacy:
            return value
        if (set(value) != {'schema', 'repo', 'participant', 'launch', 'boot_id', 'writer', 'supervisor', 'record'}
                or type(value['schema']) is not int or value['schema'] != RECORD_SCHEMA
                or value['repo'] != str(Path(repo).resolve()) or value['participant'] != member
                or value['launch'] != _identity(directory, repo, member)
                or not _valid_boot(value['boot_id']) or not _valid_process(value['writer'])
                or value['supervisor'] is not None and not _valid_process(value['supervisor'])):
            raise ValueError('invalid dispatch controller envelope')
        return value['record']


def stale_boot(repo, name):
    """Report old-boot evidence; assumes state home is not shared across hosts."""
    current = boot_id()
    if current == 'unknown':
        # On non-Linux systems (or when Linux boot metadata is unreadable), keep
        # the existing PID and process-identity checks unchanged.
        return False
    member = _member(repo, name)
    path, legacy = _location(repo, member, name)
    if legacy:
        return False
    try:
        read(repo, name)
    except FileNotFoundError:
        return False
    with Directory(path) as directory:
        envelope = directory.read(name)
        identity = _identity(directory, repo, member)
        recorded = (envelope.get('boot_id'), identity.get('boot_id'))
        return any(value != 'unknown' and value != current for value in recorded)


def _write(directory, name, value, exclusive=False):
    raw = encoded(value)
    if len(raw) > 65536:
        raise ValueError('dispatch controller record too large')
    # Refuse existing unsafe files, including on replace; never heal links or
    # foreign ownership into trusted controller evidence.
    try:
        fd = directory.file(name)
    except FileNotFoundError:
        pass
    else:
        os.close(fd)
    if exclusive:
        fd = directory.file(name, os.O_WRONLY | os.O_EXCL, create=True)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.fsync(directory.fd)
        return
    temporary = 'tmp-' + uuid.uuid4().hex
    fd = directory.file(temporary, os.O_WRONLY | os.O_EXCL, create=True)
    os.fchmod(fd, 0o600)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, name, src_dir_fd=directory.fd, dst_dir_fd=directory.fd)
        os.fsync(directory.fd)
    finally:
        try:
            os.unlink(temporary, dir_fd=directory.fd)
        except FileNotFoundError:
            pass


def write(repo, name, value, exclusive=False):
    member = _member(repo, name, value)
    path = _root(repo, member) / member
    try:
        with Directory(path):
            pass
    except FileNotFoundError:
        # Legacy files stay read-only; initialize the durable envelope from the
        # original launch record before writing any new evidence.
        with _legacy_directory(repo) as legacy:
            launch = legacy.read(member + '-launch.json')
        call = launch.get('call') if isinstance(launch, dict) else None
        if not isinstance(call, dict):
            raise ValueError('legacy dispatch launch call unavailable')
        initialize(repo, member, call)
    with Directory(path) as directory:
        supervisor = None
        if name.endswith(('-process.json', '-supervisor.json')) and isinstance(value, dict):
            pid = value.get('pid')
            if type(pid) is int and pid > 0:
                # The parent already captured its child's start identity.
                # Re-observing a PID-only legacy binding could name a reuse.
                supervisor = dict(pid=pid, identity=value.get('identity') or (
                    process_identity()['identity'] if pid == os.getpid() else 'unknown'))
        value = dict(schema=RECORD_SCHEMA, repo=str(Path(repo).resolve()), participant=member,
                     launch=_identity(directory, repo, member), boot_id=boot_id(), writer=process_identity(),
                     supervisor=supervisor, record=value)
        _write(directory, name, value, exclusive)


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
                        if key not in sent and panel_terminals.signal_process(
                                row['pid'], row['identity'], signal.SIGKILL if escalation else number):
                            sent.add(key)
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
