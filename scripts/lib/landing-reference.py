#!/usr/bin/env python3
"""Actual local operations keep reference publication visible to capture GC.

These attempts describe this writer, never the calling model or its authority.
A crash or unsuccessful publication deliberately remains nonterminal.
"""
import os
import json
import secrets
import stat
import time
import re
import subprocess
import tempfile
from pathlib import Path
import runpy
import sys

HERE = Path(__file__).resolve().parent
E = runpy.run_path(str(HERE / 'agent-events.py'))
C = runpy.run_path(str(HERE / 'landing-capture.py'))


def transition(repo, attempt, state):
    E['append_lifecycle'](Path(repo), E['new_event'](
        attempt, 0, 'attempt.state_changed', from_state=None, to_state=state,
        reason_code='reference-publication',
        actor={'kind': 'system', 'name': 'landing-reference'}))


def begin(repo, tool, reference_input=""):
    if tool not in ('agent-task', 'patch-admit'):
        raise ValueError('unsupported reference writer')
    attempt = E['create_attempt'](Path(repo), provider='local', tool=tool,
                                  refs={'reference_writer': 'landing-reference-v1',
                                        'reference_input_sha256': C['digest'](os.path.abspath(reference_input).encode()) if reference_input else ''})
    # Emit the ID before transitions so the shell can retain it on failure.
    print(attempt, flush=True)
    transition(repo, attempt, 'starting')
    transition(repo, attempt, 'working')


class OwnerExited(ValueError):
    pass


def owner_identity(pid):
    """Positive native-process proof; uncertainty is never an alive result."""
    if type(pid) is not int or pid <= 0:
        raise ValueError('invalid reference owner')
    if os.name == 'nt':
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.WaitForSingleObject.restype = wintypes.DWORD
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x100000 | 0x1000, False, pid)
        if not handle:
            if ctypes.get_last_error() == 87:
                raise OwnerExited('reference owner exited')
            raise ValueError('reference owner unavailable')
        try:
            birth = E['process_start_token'](pid)
            waited = kernel.WaitForSingleObject(handle, 0)
            if waited == 0:
                raise OwnerExited('reference owner exited')
            if not birth or waited != 258:
                raise ValueError('reference owner unproven')
            return {'pid': pid, 'birth': birth, 'uid': None}
        finally:
            kernel.CloseHandle(handle)
    if sys.platform.startswith('linux'):
        try:
            fields = E['process_helpers']['proc_stat_fields'](pid)
            info = os.stat('/proc/' + str(pid))
        except FileNotFoundError as exc:
            raise OwnerExited('reference owner exited') from exc
        if fields[0] in ('Z', 'X'):
            raise OwnerExited('reference owner exited')
        if not fields[19].isdigit() or info.st_uid != os.getuid():
            raise ValueError('reference owner unproven or foreign')
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        if not boot:
            raise ValueError('reference owner generation unavailable')
        return {'pid': pid, 'birth': boot + ':' + fields[19], 'uid': info.st_uid}
    if sys.platform == 'darwin':
        import ctypes
        class BSDInfo(ctypes.Structure):
            _fields_ = [(name, ctypes.c_uint32) for name in
                        ('flags', 'status', 'xstatus', 'pid', 'ppid', 'uid', 'gid',
                         'ruid', 'rgid', 'svuid', 'svgid', 'rfu')]
            _fields_ += [('comm', ctypes.c_char * 16), ('name', ctypes.c_char * 32)]
            _fields_ += [(name, ctypes.c_uint32) for name in
                         ('nfiles', 'pgid', 'pjobc', 'e_tdev', 'e_tpgid')]
            _fields_ += [('nice', ctypes.c_int32), ('start_sec', ctypes.c_uint64),
                         ('start_usec', ctypes.c_uint64)]
        lib = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
        lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                    ctypes.c_void_p, ctypes.c_int]
        lib.proc_pidinfo.restype = ctypes.c_int
        info = BSDInfo()
        read = lib.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
        if (read <= 0 and ctypes.get_errno() == 3) or (read == ctypes.sizeof(info) and info.status == 5):
            raise OwnerExited('reference owner exited')
        if (read != ctypes.sizeof(info) or
                info.pid != pid or info.uid != os.getuid() or
                not info.start_sec or info.start_usec >= 1000000):
            raise ValueError('reference owner unavailable')
        return {'pid': pid, 'birth': 'darwin:%d:%d' % (info.start_sec, info.start_usec), 'uid': info.uid}
    raise ValueError('native reference owner identity unavailable on this host')


def caller_owner(binding):
    # MSYS can interpose a transient native launcher. The Bash side reads its
    # own /proc/self/stat with a builtin, then uses the existing winpid bridge.
    # POSIX has no such intermediate launcher or supplied PID authority.
    if os.name == 'nt':
        if binding.get('owner_source') != 'msys-proc-v1':
            raise ValueError('reference owner mapping unavailable')
        return owner_identity(binding['owner']['pid'])
    return owner_identity(os.getppid())


def scope_read(repo, channel, expected):
    raw, info = C['regular'](channel, maximum=4096)
    if C['digest'](raw) != expected:
        raise ValueError('reference scope changed')
    scope = json.loads(raw)
    if (set(scope) != {'binding', 'attempt'} or not isinstance(scope['binding'], dict) or
            scope['binding'].get('repo') != C['digest'](os.path.realpath(repo).encode()) or
            scope['binding'].get('helper') != C['digest'](Path(__file__).read_bytes())):
        raise ValueError('foreign reference scope')
    directory = os.lstat(Path(channel).parent)
    if (C['identity'](info) != scope['binding'].get('channel') or
            C['identity'](directory) != scope['binding'].get('directory') or
            not stat.S_ISDIR(directory.st_mode) or C['D']['is_reparse'](directory) or
            os.path.realpath(channel) != os.path.abspath(channel) or
            (hasattr(os, 'getuid') and (directory.st_uid != os.getuid() or directory.st_mode & 0o077))):
        raise ValueError('reference scope directory or file replaced')
    return scope, info


def begin_owned(repo, tool, reference_input, channel, native_owner="", owner_source=""):
    # This command runs in the foreground, not a $(...) subshell. Its actual
    # native parent identifies the POSIX owner; Git Bash uses its existing
    # /proc native-PID bridge instead of a transient MSYS launcher.
    raw, info = C['regular'](channel, maximum=0)
    if raw or os.path.realpath(channel) != os.path.abspath(channel):
        raise ValueError('unsafe reference return channel')
    if os.name == 'nt':
        if owner_source != 'msys-proc-v1' or not native_owner.isdigit():
            raise ValueError('reference owner lacks the MSYS native PID bridge')
        owner = owner_identity(int(native_owner))
    else:
        if native_owner or owner_source:
            raise ValueError('unexpected reference owner override')
        owner = owner_identity(os.getppid())
    # The fence precedes both monitor and waiter startup; its inode is bound
    # into the immutable scope before either child can observe publication.
    with open(channel + '.ready.pending', 'xb') as pending:
        pending_identity = C['identity'](os.fstat(pending.fileno()))
    binding = {'owner': owner, 'owner_source': owner_source or 'foreground-parent-v1', 'nonce': secrets.token_hex(32), 'tool': tool,
               'input': C['digest'](os.path.abspath(reference_input).encode()),
               'repo': C['digest'](os.path.realpath(repo).encode()),
               'channel': C['identity'](info), 'ready_pending': pending_identity,
               'directory': C['identity'](os.lstat(Path(channel).parent)),
               'helper': C['digest'](Path(__file__).read_bytes())}
    if tool not in ('agent-task', 'patch-admit'):
        raise ValueError('unsupported reference writer')
    attempt = E['create_attempt'](Path(repo), provider='local', tool=tool,
        refs={'reference_writer': 'landing-reference-v1',
              'reference_input_sha256': binding['input'],
              'reference_scope_sha256': C['digest'](C['encoded'](binding))})
    transition(repo, attempt, 'starting')
    transition(repo, attempt, 'working')
    if owner_identity(owner['pid']) != owner:
        raise ValueError('reference owner changed during registration')
    payload = C['encoded']({'binding': binding, 'attempt': attempt})
    flags = os.O_WRONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_BINARY', 0)
    fd = os.open(channel, flags)
    try:
        opened = os.fstat(fd)
        if C['identity'](opened) != C['identity'](info) or opened.st_size or opened.st_nlink != 1:
            raise ValueError('reference return channel replaced')
        with os.fdopen(fd, 'wb', closefd=False) as output:
            output.write(payload)
            output.flush()
            os.fsync(fd)
    finally:
        os.close(fd)
    print(attempt + ' ' + C['digest'](payload), flush=True)


def scope_pulse(repo, channel, expected):
    scope_read(repo, channel, expected)
    path = E['event_path'](Path(repo))
    with E['file_lock'](path):
        scope, _ = scope_read(repo, channel, expected)
        binding = scope['binding']
        if owner_identity(binding['owner']['pid']) != binding['owner']:
            raise ValueError('reference owner generation changed')
        rows = E['read_rows'](path)
        current = E['project_attempts'](rows).get(scope['attempt'])
        created = [r for r in rows if r.get('attempt_id') == scope['attempt'] and r.get('event_type') == 'attempt.created']
        if (len(created) != 1 or not current or current.get('provider') != 'local' or
                current.get('tool') != binding['tool'] or current.get('parent_attempt_id') or
                created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
                created[0].get('refs', {}).get('reference_input_sha256') != binding['input'] or
                created[0].get('refs', {}).get('reference_scope_sha256') != C['digest'](C['encoded'](binding))):
            raise ValueError('reference scope is not the registered writer')
        if current.get('state') not in ('starting', 'working', 'verifying'):
            return False
        event = E['new_event'](scope['attempt'], current['sequence'] + 1, 'attempt.heartbeat',
                              actor={'kind': 'runner', 'name': 'landing-reference'})
        E['validate_event_row'](event)
        E['append_row'](path, event)
        return True


def monitor(repo, channel, expected):
    # The shell owns and reaps this exact child. Each lock attempt is bounded;
    # inability to prove the writer leaves its attempt conservatively open.
    os.environ['OMS_LOCK_TIMEOUT'] = '1'
    initial, _ = scope_read(repo, channel, expected)
    if caller_owner(initial['binding']) != initial['binding']['owner']:
        raise ValueError('reference monitor belongs to another writer')
    if not scope_pulse(repo, channel, expected):
        raise ValueError('reference operation ended before monitor startup')
    scope, _ = scope_read(repo, channel, expected)
    ready = channel + '.ready'
    pending = ready + '.pending'
    flags = os.O_WRONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_BINARY', 0)
    fd = os.open(pending, flags)
    try:
        info = os.fstat(fd)
        if (C['identity'](info) != scope['binding']['ready_pending'] or
                not stat.S_ISREG(info.st_mode) or C['D']['is_reparse'](info) or
                info.st_nlink != 1 or info.st_size):
            raise ValueError('reference publication fence changed')
        with os.fdopen(fd, 'wb', closefd=False) as output:
            output.write(C['encoded']({'scope': expected, 'monitor': owner_identity(os.getpid())}))
            output.flush()
    finally:
        os.close(fd)
    scope_read(repo, channel, expected)
    if C['identity'](os.lstat(pending)) != scope['binding']['ready_pending']:
        raise ValueError('reference publication fence replaced')
    os.link(pending, ready)
    if C['identity'](os.lstat(pending)) != scope['binding']['ready_pending']:
        raise ValueError('reference publication fence replaced before release')
    os.unlink(pending)
    next_pulse = time.monotonic() + 10.0
    next_owner = time.monotonic() + 1.0
    while True:
        try:
            current, _ = scope_read(repo, channel, expected)
        except (OSError, ValueError):
            return
        now = time.monotonic()
        if now >= next_owner:
            if owner_identity(current['binding']['owner']['pid']) != current['binding']['owner']:
                return
            try:
                with E['file_lock'](E['event_path'](Path(repo))):
                    _, projection = E['load_projection'](Path(repo))
                    if projection.get(current['attempt'], {}).get('state') not in ('starting', 'working', 'verifying'):
                        return
            except E['OpsError'] as exc:
                if not str(exc).startswith('could not acquire state lock for '):
                    raise
            next_owner = now + 1.0
        if now >= next_pulse:
            try:
                if not scope_pulse(repo, channel, expected):
                    return
            except E['OpsError'] as exc:
                # Contention is not an owner death. Retry a bounded lock wait;
                # only a committed heartbeat can affect stale reconciliation.
                if not str(exc).startswith('could not acquire state lock for '):
                    raise
            else:
                next_pulse = time.monotonic() + 10.0
        time.sleep(0.02)


def await_monitor(repo, channel, expected, native_monitor):
    deadline = time.monotonic() + 1.5
    while time.monotonic() < deadline:
        scope, _ = scope_read(repo, channel, expected)
        if caller_owner(scope['binding']) != scope['binding']['owner']:
            raise ValueError('reference readiness belongs to another writer')
        try:
            pending = os.lstat(channel + '.ready.pending')
        except FileNotFoundError:
            pass
        else:
            if (C['identity'](pending) != scope['binding']['ready_pending'] or
                    not stat.S_ISREG(pending.st_mode) or C['D']['is_reparse'](pending) or
                    pending.st_nlink not in (1, 2) or pending.st_size > 512 or
                    (hasattr(os, 'getuid') and pending.st_uid != os.getuid())):
                raise ValueError('reference publication fence changed while waiting')
            try:
                named = os.lstat(channel + '.ready')
            except FileNotFoundError:
                pass
            else:
                if C['identity'](named) != scope['binding']['ready_pending']:
                    raise ValueError('foreign reference readiness during publication')
            time.sleep(0.02)
            continue
        try:
            raw, _ = C['regular'](channel + '.ready', maximum=512)
        except FileNotFoundError:
            time.sleep(0.02)
            continue
        # Exclusive creation publishes the name before the buffered payload.
        # Only the empty in-progress file may wait within the original budget.
        if not raw:
            time.sleep(0.02)
            continue
        ready = json.loads(raw)
        if (set(ready) != {'scope', 'monitor'} or ready['scope'] != expected or
                str(ready['monitor']['pid']) != native_monitor or
                owner_identity(ready['monitor']['pid']) != ready['monitor']):
            raise ValueError('reference monitor readiness changed')
        if caller_owner(scope['binding']) != scope['binding']['owner']:
            raise ValueError('reference readiness belongs to another writer')
        print(C['digest'](raw), flush=True)
        return
    raise ValueError('reference monitor did not become ready')


def close_scope(repo, channel, expected):
    previous = os.environ.get('OMS_LOCK_TIMEOUT')
    os.environ['OMS_LOCK_TIMEOUT'] = '1'
    try:
        with E['file_lock'](E['event_path'](Path(repo))):
            close_scope_locked(repo, channel, expected)
    finally:
        if previous is None:
            os.environ.pop('OMS_LOCK_TIMEOUT', None)
        else:
            os.environ['OMS_LOCK_TIMEOUT'] = previous


def close_scope_locked(repo, channel, expected):
    # Closing the scope is separate from finishing the attempt: crash/failure
    # cleanup must stop liveness without inventing successful publication.
    scope, info = scope_read(repo, channel, expected)
    if caller_owner(scope['binding']) != scope['binding']['owner']:
        raise ValueError('reference scope belongs to another writer')
    if C['identity'](os.lstat(channel)) != C['identity'](info):
        raise ValueError('reference scope replaced before close')
    os.unlink(channel)
    # Closing the owned scope first prevents future pulses even when an
    # auxiliary file has been replaced. Validate every auxiliary before
    # deleting any of them; foreign/unknown bytes remain for inspection.
    auxiliary = []
    ready_raw = None
    try:
        ready_raw, ready_info = C['regular'](channel + '.ready', maximum=512)
    except FileNotFoundError:
        pass
    else:
        ready = json.loads(ready_raw)
        if set(ready) != {'scope', 'monitor'} or ready['scope'] != expected:
            raise ValueError('reference readiness changed before close')
        auxiliary.append((channel + '.ready', ready_info))
    result_path = str(Path(channel).parent / 'result')
    raw, result_info = C['regular'](result_path, maximum=256)
    if raw.replace(b'\r\n', b'\n') != (scope['attempt'] + ' ' + expected + '\n').encode():
        raise ValueError('reference return file changed before close')
    auxiliary.append((result_path, result_info))
    proof_path = str(Path(channel).parent / 'proof')
    proof, proof_info = C['regular'](proof_path, maximum=66)
    expected_proof = (C['digest'](ready_raw) + '\n').encode() if ready_raw is not None else b''
    if proof.replace(b'\r\n', b'\n') != expected_proof:
        raise ValueError('reference readiness proof changed before close')
    auxiliary.append((proof_path, proof_info))
    for name, before in auxiliary:
        if C['identity'](os.lstat(name)) != C['identity'](before):
            raise ValueError('reference auxiliary replaced before cleanup')
        os.unlink(name)
    os.rmdir(Path(channel).parent)


def terminate_monitor(identity):
    """Signal only a generation held by a native handle, never a bare PID."""
    pid = identity['pid']
    if sys.platform.startswith('linux'):
        import signal
        if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
            return False
        fd = os.pidfd_open(pid)
        try:
            if owner_identity(pid) != identity:
                return False
            signal.pidfd_send_signal(fd, signal.SIGKILL)
            return True
        finally:
            os.close(fd)
    if os.name == 'nt':
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        kernel.TerminateProcess.restype = wintypes.BOOL
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x100000 | 0x1000 | 1, False, pid)
        if not handle:
            return False
        try:
            if owner_identity(pid) != identity:
                return False
            return bool(kernel.TerminateProcess(handle, 1))
        finally:
            kernel.CloseHandle(handle)

    return False


def stop_monitor(repo, channel, expected, ready_digest=""):
    scope, _ = scope_read(repo, channel, expected)
    if caller_owner(scope['binding']) != scope['binding']['owner']:
        raise ValueError('reference stop belongs to another writer')
    ready = None
    try:
        raw, _ = C['regular'](channel + '.ready', maximum=512)
        value = json.loads(raw)
        if (set(value) == {'scope', 'monitor'} and value['scope'] == expected and
                ready_digest and C['digest'](raw) == ready_digest):
            ready = value
    except (OSError, ValueError):
        pass
    cleanup_failed = False
    try:
        close_scope(repo, channel, expected)
    except (OSError, ValueError) as exc:
        cleanup_failed = True
        print('landing-reference: scope preserved: ' + str(exc), file=sys.stderr)
    if ready is None:
        raise ValueError('reference monitor identity unavailable; scope closed without signalling')
    identity = ready['monitor']
    # Include the monitor's one-second lifecycle lock wait and poll interval.
    # A cooperative close must not require forced termination on macOS.
    deadline = time.monotonic() + 1.5
    forced = False
    while True:
        try:
            current = owner_identity(identity['pid'])
        except OwnerExited:
            return 3 if cleanup_failed else 0
        if current != identity:
            return 3 if cleanup_failed else 0
        if time.monotonic() >= deadline:
            if forced or not terminate_monitor(identity):
                raise ValueError('reference monitor stop unproven; not waiting on an unknown child')
            forced = True
            deadline = time.monotonic() + 0.3
        time.sleep(0.02)


def finish(repo, attempt, result):
    _, projection = E['load_projection'](Path(repo))
    current = projection.get(attempt)
    created = [row for row in E['read_rows'](E['event_path'](Path(repo)))
               if row.get('attempt_id') == attempt and row.get('event_type') == 'attempt.created']
    if (not current or len(created) != 1 or current.get('provider') != 'local' or
            current.get('tool') not in ('agent-task', 'patch-admit') or
            created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
            current.get('parent_attempt_id')):
        raise ValueError('not an owned local reference operation')
    # The packet validator already ends proven no-write refusals while locked.
    # Its outer wrapper must not convert that terminal refusal to blocked.
    if current.get('terminal') is True:
        return
    if result == 'refuse-no-write':
        states = ('failed',)
    elif result == '0':
        states = ('verifying', 'review', 'done')
    else:
        states = ('blocked',)
    for state in states:
        transition(repo, attempt, state)


def supported(repo, path):
    path = os.path.realpath(path)
    return any(os.path.commonpath([path, os.path.join(repo, '.oms', name)]) ==
               os.path.join(repo, '.oms', name)
               for name in ('task', 'tasks', 'plan', 'artifacts'))


def aliases(repo):
    for item in C['reservations'](repo).values():
        for path in (os.path.join(item['directory'], item['landing_id'] + '.patch'),
                     os.path.join(repo, '.oms', 'landing-patches', item['landing_id'] + '.patch')):
            yield path, {value.encode() for value in C['reference_spellings'](repo, path)}


def validate(repo, target, values):
    if values and values[0] == 'agent_task_append_deduplicated_unlocked':
        dedupe_state(target, values[4], values[5])
    raw = '\n'.join(values).encode()
    # Only designated section/note arguments are files. A metadata value
    # may itself be a managed path, whose hardlinks require indexed_bytes.
    content_positions = {'agent_task_replace_section_unlocked': 3,
                         'agent_task_append_bullet_unlocked': 4,
                         'agent_task_verify_finalize_unlocked': 4,
                         'agent_task_append_deduplicated_unlocked': 4}
    inputs = [target] if os.path.lexists(target) else []
    position = content_positions.get(values[0]) if values else None
    if position is not None:
        inputs.append(values[position])
    for value in inputs:
        content, _ = C['regular'](value)
        raw += b'\n' + content
    found = False
    for path, forms in aliases(repo):
        if any(form in raw for form in forms):
            found = True
            if C['indexed_bytes'](repo, path) is None:
                raise ValueError('managed reference ownership unavailable')
    if found and not supported(repo, target):
        raise ValueError('managed references require a scanned OMS evidence namespace')


def dedupe_state(target, note, key):
    if not re.fullmatch(r'[0-9a-f]{64}', key):
        raise ValueError('invalid task append dedupe key')
    raw, info = C['regular'](target) if os.path.lexists(target) else (b'', None)
    content, _ = C['regular'](note)
    note_sha = C['digest'](content)
    header = raw.split(b'\n## ', 1)[0]
    records = {}
    for line in header.splitlines():
        if line.startswith(b'- append_receipt_'):
            match = re.fullmatch(rb'- append_receipt_([0-9a-f]{64}): ([0-9a-f]{64})', line)
            if not match or match[1] in records:
                raise ValueError('invalid task append receipt metadata')
            records[match[1]] = match[2]
    if len(records) > 256:
        raise ValueError('task append receipt limit exceeded')
    previous = records.get(key.encode())
    if previous and previous != note_sha.encode():
        raise ValueError('task append dedupe key already binds another note')
    if not previous and len(records) >= 256:
        raise ValueError('task append receipt limit reached; rotate the task')
    return raw, info, content, note_sha, bool(previous)


def append_deduplicated(repo, attempt, target, section, agent, note, key):
    # This is an internal transaction leg, not an unregistered reference writer.
    _, projection = E['load_projection'](Path(repo))
    current = projection.get(attempt, {})
    created = [row for row in E['read_rows'](E['event_path'](Path(repo)))
               if row.get('attempt_id') == attempt and row.get('event_type') == 'attempt.created']
    if (len(created) != 1 or current.get('provider') != 'local' or current.get('tool') != 'agent-task' or
            current.get('terminal') is not False or current.get('parent_attempt_id') or
            created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
            created[0].get('refs', {}).get('reference_input_sha256') != C['digest'](os.path.abspath(target).encode())):
        raise ValueError('task append requires its actual local reference operation')
    raw, info, content, note_sha, replay = dedupe_state(target, note, key)
    if replay:
        return
    parent = os.path.dirname(os.path.abspath(target))
    os.makedirs(parent, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.oms-replace.', dir=parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(raw)
        # Reuse the exact existing normalization, byte cap and pruning behavior.
        subprocess.run(['bash', '-c', 'set -e; . "$1"; if [ ! -s "$2" ]; then rm -f "$2"; fi; agent_task_append_bullet_unlocked "$2" "$3" "$4" "$5"',
                        'task-append', str(HERE / 'agent-task-common.sh'), temporary, section, agent, note], check=True)
        candidate, _ = C['regular'](temporary)
        # The raw input digest binds retries even when the existing display
        # normalization truncates or later pruning retires the rendered bullet.
        marker = ('- append_receipt_' + key + ': ' + note_sha + '\n').encode()
        offset = candidate.find(b'\n## ')
        if offset < 0:
            raise ValueError('task append produced no section boundary')
        candidate = candidate[:offset] + b'\n' + marker + candidate[offset:]
        if C['regular'](note)[0] != content:
            raise ValueError('task note changed during append')
        before, current_info = C['regular'](target) if os.path.lexists(target) else (b'', None)
        if before != raw or ((info is None) != (current_info is None)) or (info is not None and
                (info.st_dev, info.st_ino) != (current_info.st_dev, current_info.st_ino)):
            raise ValueError('task packet changed during append')
        _, temp_info = C['regular'](temporary)
        flags = os.O_WRONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0) | getattr(os, 'O_BINARY', 0)
        with os.fdopen(os.open(temporary, flags), 'wb') as handle:
            if C['identity'](os.fstat(handle.fileno())) != C['identity'](temp_info):
                raise ValueError('task append scratch changed')
            handle.truncate(0)
            handle.write(candidate)
            handle.flush()
            os.fsync(handle.fileno())
        # Validate proposed bytes under the real target namespace while the
        # outer operation and packet lock still exclude managed capture GC.
        validate(repo, target, ['agent_task_append_bullet_unlocked', target, section, agent, temporary])
        C['regular'](temporary)
        os.replace(temporary, target)
        C['D']['_fsync_directory_path'](parent, 'task append receipt')
    finally:
        if os.path.lexists(temporary):
            os.unlink(temporary)


def main():
    action, repo = sys.argv[1:3]
    repo = os.path.realpath(repo)
    args = sys.argv[3:]
    if action == 'begin-owned':
        begin_owned(repo, *args)
    elif action == 'monitor':
        monitor(repo, *args)
    elif action == 'await-monitor':
        await_monitor(repo, *args)
    elif action == 'stop-monitor':
        raise SystemExit(stop_monitor(repo, *args))
    elif action == 'close-scope':
        close_scope(repo, *args)
    elif action == 'begin':
        begin(repo, *args)
    elif action == 'finish':
        attempt, result = args
        finish(repo, attempt, result)
    elif action == 'task-append':
        append_deduplicated(repo, *args)
    elif action == 'admission':
        C['record_admission'](repo, *args)
    elif action == 'task':
        validate(repo, args[0], args[1:])
    elif action == 'admit':
        patch, report = args
        paths = {path for path, _ in aliases(repo)}
        known = os.path.abspath(patch) in paths or os.path.realpath(patch) in paths
        if known:
            if os.path.abspath(patch) != os.path.realpath(patch):
                raise ValueError('redirected managed admission input')
            if C['indexed_bytes'](repo, patch) is None:
                raise ValueError('managed admission input ownership unavailable')
            if not supported(repo, report):
                raise ValueError('managed admission requires an internal report')
        print('1' if known else '0')
    else:
        raise ValueError('unsupported reference operation')


if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        print('landing-reference: ' + str(exc), file=sys.stderr)
        sys.exit(2)
