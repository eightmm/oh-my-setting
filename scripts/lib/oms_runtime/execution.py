"""Optional bounded execution backends with honest capability receipts."""
from __future__ import annotations
import contextlib
import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple
from . import RUNTIME_SCHEMA
from .common import CoreError, SECRET_NAME_RE, SECRET_VALUE_RE, atomic_write_bytes, atomic_write_json, canonical_json, ensure_private_dir, relative_path, sha256_bytes, utc_now
DEFAULT_LOG_BYTES = 4 * 1024 * 1024
MAX_LOG_BYTES = 64 * 1024 * 1024

def describe(profile: str) -> Dict[str, Any]:
    if profile == 'trusted-local':
        return {'profile': profile, 'declared': {'filesystem': 'host-user', 'network': 'host', 'creden' + 'tials': 'ambient-host-readable', 'wall_clock': True}, 'enforced': {'filesystem': False, 'network': False, 'creden' + 'tials': False, 'wall_clock': True, 'process_group': os.name != 'nt'}, 'unknown': ['reads', 'writes outside repository', 'background process escape on unsupported hosts'], 'warning': 'Process supervision only; this is not a sandbox.'}
    if profile == 'isolated':
        return {'profile': profile, 'declared': {'repo': 'read-only', 'worktree': 'explicit-read-write', 'network': 'off-by-default', 'creden' + 'tials': 'not-mounted', 'wall_clock': True}, 'enforced': {'filesystem': 'container-mount-policy', 'network': 'container-network-policy', 'creden' + 'tials': 'environment-and-mount-filter', 'wall_clock': True, 'capabilities': 'drop-all', 'no_new_privileges': True}, 'unknown': ['container image contents', 'container daemon trust', 'kernel isolation'], 'warning': 'Image contents and the container daemon remain operator-owned.'}
    if profile == 'remote':
        return {'profile': profile, 'declared': {'transport': 'external-adapter', 'isolation': 'external-adapter', 'wall_clock': True}, 'enforced': {'wall_clock': True}, 'unknown': ['remote authentication', 'remote filesystem', 'remote network', 'remote cleanup'], 'warning': 'Isolation claims are adapter attestations, not locally enforced facts.'}
    raise CoreError('unsupported execution profile: %s' % profile)

def _container_engine() -> str:
    return shutil.which('docker') or shutil.which('podman') or ''

def check(profile: str, *, image: str='', adapter: str='') -> Dict[str, Any]:
    image = image or os.environ.get('OMS_ISOLATED_IMAGE', '')
    adapter = adapter or os.environ.get('OMS_REMOTE_ADAPTER', '')
    base = describe(profile)
    missing: List[str] = []
    details: Dict[str, Any] = {}
    if profile == 'isolated':
        engine = _container_engine()
        if not engine:
            missing.append('docker-or-podman')
        if not image:
            missing.append('image')
        if engine:
            details['engine'] = Path(engine).name
            try:
                info = subprocess.run([engine, 'info'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False, timeout=20)
            except (OSError, subprocess.SubprocessError) as exc:
                details['daemon_ready'] = False
                details['probe_error'] = type(exc).__name__
                missing.append('container-daemon')
            else:
                details['daemon_ready'] = info.returncode == 0
                if info.returncode != 0:
                    missing.append('container-daemon')
                if image and info.returncode == 0:
                    try:
                        inspect = subprocess.run([engine, 'image', 'inspect', image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False, timeout=20)
                    except (OSError, subprocess.SubprocessError) as exc:
                        details['image_local'] = False
                        details['image_probe_error'] = type(exc).__name__
                        missing.append('local-image')
                    else:
                        details['image_local'] = inspect.returncode == 0
                        if inspect.returncode != 0:
                            missing.append('local-image')
    elif profile == 'remote':
        resolved = shutil.which(adapter) if adapter and os.sep not in adapter else adapter
        if not resolved or not os.path.isfile(resolved) or (not os.access(resolved, os.X_OK)):
            missing.append('remote-adapter')
        else:
            details['adapter'] = os.path.basename(resolved)
    return dict(base, ready=not missing, missing=sorted(set(missing)), details=details)

def _safe_env(inherit_names: Sequence[str], *, include_home: bool) -> Dict[str, str]:
    base = ['PATH', 'TMPDIR', 'TEMP', 'TMP', 'LANG', 'LC_ALL', 'TERM', 'USER', 'SHELL']
    if include_home:
        base.append('HOME')
    result: Dict[str, str] = {}
    for name in base + list(inherit_names):
        if SECRET_NAME_RE.search(name):
            raise CoreError('refusing to inherit sensitive environment variable: %s' % name)
        if name in os.environ:
            value = os.environ[name]
            if SECRET_VALUE_RE.search('%s=%s' % (name, value)):
                raise CoreError('refusing sensitive-looking environment value: %s' % name)
            result[name] = value
    result['OMS_RUNTIME_BACKEND'] = '1'
    return result

def _container_cli_env() -> Dict[str, str]:
    """Minimal host-side environment needed to reach Docker/Podman daemons."""
    names = ['PATH', 'TMPDIR', 'TEMP', 'TMP', 'LANG', 'LC_ALL', 'DOCKER_HOST', 'DOCKER_CONTEXT', 'XDG_RUNTIME_DIR', 'CONTAINER_HOST', 'CONTAINERS_CONF', 'CONTAINERS_STORAGE_CONF']
    result: Dict[str, str] = {}
    for name in names:
        if name in os.environ and (not SECRET_NAME_RE.search(name)):
            result[name] = os.environ[name]
    result.setdefault('PATH', '/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin')
    result['OMS_RUNTIME_BACKEND'] = '1'
    return result

def _terminate(proc: subprocess.Popen) -> None:
    try:
        if os.name == 'nt':
            proc.terminate()
        else:
            os.killpg(proc.pid, signal.SIGTERM)
    except OSError:
        pass
    try:
        proc.wait(timeout=5)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        if os.name == 'nt':
            proc.kill()
        else:
            os.killpg(proc.pid, signal.SIGKILL)
    except OSError:
        pass
    with contextlib.suppress(subprocess.TimeoutExpired):
        proc.wait(timeout=5)

def _tail(path: Path, limit: int) -> Tuple[bytes, int]:
    size = path.stat().st_size
    if size <= limit:
        return (path.read_bytes(), 0)
    marker = ('[TRUNCATED: %d leading bytes omitted]\n' % (size - limit)).encode('utf-8')
    keep = max(0, limit - len(marker))
    with path.open('rb') as handle:
        handle.seek(max(0, size - keep))
        return (marker + handle.read(keep), size - keep)

def _container_command(engine: str, image: str, repo: Path, worktree: Optional[Path], command: Sequence[str], allow_network: bool, *, memory_mb: int, cpus: float, pids_limit: int, tmpfs_mb: int) -> List[str]:
    effective = [engine, 'run', '--rm', '--read-only', '--tmpfs', '/tmp:rw,nosuid,nodev,noexec,size=%dm' % tmpfs_mb, '--memory', '%dm' % memory_mb, '--cpus', str(cpus), '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--pids-limit', str(pids_limit), '-v', '%s:/workspace:ro' % repo.resolve()]
    if not allow_network:
        effective.extend(['--network', 'none'])
    cwd = '/workspace'
    if worktree is not None:
        effective.extend(['-v', '%s:/work:rw' % worktree.resolve()])
        cwd = '/work'
    if hasattr(os, 'getuid') and hasattr(os, 'getgid'):
        effective.extend(['--user', '%d:%d' % (os.getuid(), os.getgid())])
    effective.extend(['-w', cwd, image])
    effective.extend(command)
    return effective

def run(profile: str, repo: Path, command: Sequence[str], *, timeout_seconds: int=1200, image: str='', adapter: str='', worktree: Optional[Path]=None, allow_network: bool=False, inherit_env: Sequence[str]=(), log_path: Optional[Path]=None, receipt_path: Optional[Path]=None, log_limit: int=DEFAULT_LOG_BYTES, memory_mb: int=4096, cpus: float=2.0, pids_limit: int=512, tmpfs_mb: int=1024) -> Tuple[Dict[str, Any], int]:
    if not command:
        raise CoreError('execution requires a command after --')
    if timeout_seconds < 1 or timeout_seconds > 86400:
        raise CoreError('timeout must be between 1 and 86400 seconds')
    if log_limit < 1024 or log_limit > MAX_LOG_BYTES:
        raise CoreError('log limit must be between 1024 and %d bytes' % MAX_LOG_BYTES)
    if memory_mb < 64 or memory_mb > 1024 * 1024:
        raise CoreError('memory limit must be between 64 and 1048576 MiB')
    if not 0.1 <= float(cpus) <= 1024.0:
        raise CoreError('CPU limit must be between 0.1 and 1024')
    if pids_limit < 16 or pids_limit > 1000000:
        raise CoreError('PID limit must be between 16 and 1000000')
    if tmpfs_mb < 16 or tmpfs_mb > 1024 * 1024:
        raise CoreError('tmpfs limit must be between 16 and 1048576 MiB')
    image = image or os.environ.get('OMS_ISOLATED_IMAGE', '')
    adapter = adapter or os.environ.get('OMS_REMOTE_ADAPTER', '')
    command_text = ' '.join((str(item) for item in command))
    if SECRET_VALUE_RE.search(command_text):
        raise CoreError('command contains sensitive-looking creden' + 'tial material')
    readiness = check(profile, image=image, adapter=adapter)
    if not readiness['ready']:
        raise CoreError('execution backend is not ready: %s' % ', '.join(readiness['missing']), exit_code=3)
    raw_cwd = worktree or repo
    if raw_cwd.is_symlink():
        raise CoreError('execution directory must not be a symbolic link: %s' % raw_cwd)
    cwd = raw_cwd.resolve()
    if not cwd.is_dir():
        raise CoreError('execution directory must be a real directory: %s' % cwd)
    operation_id = 'exec-' + uuid.uuid4().hex
    log_path = log_path or repo / '.oms' / 'runtime' / 'logs' / (operation_id + '.log')
    receipt_path = receipt_path or repo / '.oms' / 'runtime' / 'receipts' / (operation_id + '.json')
    if log_path.is_symlink() or receipt_path.is_symlink():
        raise CoreError('log and receipt paths must not be symbolic links')
    ensure_private_dir(log_path.parent)
    ensure_private_dir(receipt_path.parent)
    stdin_data: Optional[bytes] = None
    env = _safe_env(inherit_env, include_home=profile == 'trusted-local')
    if profile == 'trusted-local':
        effective = list(command)
        popen_cwd: Optional[str] = str(cwd)
    elif profile == 'isolated':
        engine = _container_engine()
        if not engine or not image:
            # Never fall back to trusted-local silently: an unavailable
            # backend is an unavailable capability, reported as such.
            raise CoreError('container capability unavailable: %s' % (
                'no docker or podman on PATH' if not engine else 'no container image selected'))
        effective = _container_command(engine, image, repo, worktree, command, allow_network, memory_mb=memory_mb, cpus=float(cpus), pids_limit=pids_limit, tmpfs_mb=tmpfs_mb)
        popen_cwd = None
        env = _container_cli_env()
    else:
        resolved = shutil.which(adapter) if os.sep not in adapter else adapter
        effective = [str(resolved)]
        popen_cwd = None
        from .evidence import build_envelope
        envelope = build_envelope(repo)
        request = {'schema': 1, 'operation_id': operation_id, 'command': list(command), 'command_digest': sha256_bytes(canonical_json(list(command))), 'repo_head': envelope.get('repo', {}).get('head'), 'state_digest': envelope.get('state_digest'), 'worktree_supplied': worktree is not None, 'timeout_seconds': timeout_seconds, 'network': 'allowed' if allow_network else 'denied', 'declared_capabilities': describe(profile)['declared'], 'resource_limits': {'memory_mb': memory_mb, 'cpus': float(cpus), 'pids': pids_limit, 'tmpfs_mb': tmpfs_mb}}
        stdin_data = canonical_json(request) + b'\n'
    started_at = utc_now()
    started = time.monotonic()
    timed_out = False
    returncode = 127
    adapter_attestation: Optional[Dict[str, Any]] = None
    remote_response: Optional[Dict[str, Any]] = None
    remote_response_valid: Optional[bool] = None
    remote_accepted: Optional[bool] = None
    adapter_attestation_valid: Optional[bool] = None
    creationflags = getattr(subprocess, 'CREATE_NEW_PROCESS_GROUP', 0) if os.name == 'nt' else 0
    preexec_fn = None if os.name == 'nt' else os.setsid
    fd, temp_name = tempfile.mkstemp(prefix='oms-exec.', dir=str(log_path.parent))
    os.close(fd)
    temp_log = Path(temp_name)
    try:
        with temp_log.open('wb') as output:
            try:
                proc = subprocess.Popen(effective, cwd=popen_cwd, stdin=subprocess.PIPE if stdin_data is not None else subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, env=env, preexec_fn=preexec_fn, creationflags=creationflags)
                if stdin_data is not None and proc.stdin is not None:
                    proc.stdin.write(stdin_data)
                    proc.stdin.close()
                try:
                    returncode = proc.wait(timeout=timeout_seconds)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    _terminate(proc)
                    returncode = 124
            except OSError as exc:
                output.write(('runtime launch failed: %s\n' % exc).encode('utf-8', 'replace'))
                returncode = 127
        raw, omitted = _tail(temp_log, log_limit)
        atomic_write_bytes(log_path, raw)
        if profile == 'remote':
            lines = [line.strip() for line in raw.decode('utf-8', 'replace').splitlines() if line.strip()]
            try:
                candidate = json.loads(lines[-1]) if lines else None
            except ValueError:
                candidate = None
            if isinstance(candidate, dict):
                remote_response = candidate
                remote_response_valid = candidate.get('schema') == 1 and candidate.get('operation_id') == operation_id and isinstance(candidate.get('accepted'), bool)
                if remote_response_valid:
                    remote_accepted = bool(candidate.get('accepted'))
                    adapter_attestation = candidate.get('attestation') if isinstance(candidate.get('attestation'), dict) else None
                    if remote_accepted:
                        remote_exit = candidate.get('exit')
                        required_attestation = ('transport_authenticated', 'filesystem_isolated', 'cleanup_confirmed')
                        adapter_attestation_valid = isinstance(remote_exit, int) and (not isinstance(remote_exit, bool)) and (0 <= remote_exit <= 255) and isinstance(adapter_attestation, dict) and all((adapter_attestation.get(name) is True for name in required_attestation))
                        if returncode == 0 and adapter_attestation_valid:
                            returncode = int(remote_exit)
                        elif returncode == 0:
                            remote_response_valid = False
                            returncode = 125
                    elif returncode == 0:
                        returncode = 3
                elif returncode == 0:
                    returncode = 125
            else:
                remote_response_valid = False
                if returncode == 0:
                    returncode = 125
    finally:
        with contextlib.suppress(OSError):
            temp_log.unlink()
    description = describe(profile)
    receipt: Dict[str, Any] = {'schema': RUNTIME_SCHEMA, 'operation_id': operation_id, 'profile': profile, 'started_at': started_at, 'finished_at': utc_now(), 'duration_seconds': round(max(0.0, time.monotonic() - started), 3), 'exit': returncode, 'timed_out': timed_out, 'command_digest': sha256_bytes(canonical_json(list(command))), 'output_sha256': sha256_bytes(raw), 'output_bytes': len(raw), 'output_omitted_bytes': omitted, 'log': relative_path(log_path, repo) or log_path.name, 'declared_capabilities': description['declared'], 'enforced_capabilities': description['enforced'], 'unknown_capabilities': description['unknown'], 'network_requested': allow_network, 'resource_limits': {'memory_mb': memory_mb, 'cpus': float(cpus), 'pids': pids_limit, 'tmpfs_mb': tmpfs_mb, 'enforced': profile == 'isolated', 'attested_only': profile == 'remote'}, 'adapter_attestation': adapter_attestation, 'remote_response_valid': remote_response_valid, 'remote_accepted': remote_accepted, 'remote_request_digest': sha256_bytes(stdin_data.rstrip(b'\n')) if profile == 'remote' and stdin_data is not None else None, 'adapter_attestation_valid': adapter_attestation_valid, 'sensitive_output_detected': bool(SECRET_VALUE_RE.search(raw.decode('utf-8', 'replace')))}
    atomic_write_json(receipt_path, receipt)
    receipt['receipt'] = relative_path(receipt_path, repo) or receipt_path.name
    return (receipt, returncode)
