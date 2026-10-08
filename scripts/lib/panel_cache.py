"""Disposable, repository-bound panel reads; room selection stays in the caller."""

import hashlib
import json
import math
import os
from pathlib import Path
import stat
import time

from panel_results import DURABLE, _lock

INTERVAL = 5
MAX_BYTES = 8 * 1024 * 1024
DIRECTORY = '.oms/hooks/panel-cache'
SKIPPED = {DIRECTORY, '.oms/hooks/panel-activity'}
ROOT = Path(__file__).resolve().parents[2]


def fingerprint(repo):
    """Stat inputs without reading logs or following symlinks into other trees."""
    entries = []

    def visit(path, label):
        try:
            info = path.lstat()
        except FileNotFoundError:
            entries.append((label, None))
            return
        if stat.S_ISDIR(info.st_mode):
            # Cache writes must not invalidate their own parent directory.
            entries.append((label, info.st_mode, info.st_dev, info.st_ino))
            for child in sorted(path.iterdir()):
                relative = label + '/' + child.name
                # Mains' activity heartbeats are read by each board directly, not through this cache.
                if relative in SKIPPED or child.name.endswith(('.lock', '.tmp')):
                    continue
                visit(child, relative)
        else:
            entries.append((label, info.st_mode, info.st_dev, info.st_ino,
                            info.st_size, info.st_mtime_ns, info.st_ctime_ns))

    visit(repo / '.oms', '.oms')
    for name in ('AGENTS.md', 'CLAUDE.md', 'PROJECT.md', '.gitignore', '.github/workflows'):
        visit(repo / name, name)
    # A worktree's gitfile is unchanged when its HEAD or index changes.
    git_dir = repo / '.git'
    if git_dir.is_file():
        visit(git_dir, '.git')
        with git_dir.open(encoding='utf-8') as handle:
            pointer = handle.read(4096).strip()
        if pointer.startswith('gitdir: '):
            git_dir = (repo / pointer[8:]).resolve()
    for part in ('HEAD', 'index', 'config', 'packed-refs', 'refs', 'commondir'):
        visit(git_dir / part, 'git/' + part)
    common = git_dir / 'commondir'
    if common.is_file():
        with common.open(encoding='utf-8') as handle:
            common_dir = (git_dir / handle.read(4096).strip()).resolve()
        for part in ('config', 'packed-refs', 'refs'):
            visit(common_dir / part, 'git-common/' + part)
    settings = {name: os.environ.get(name) for name in (
        'XDG_STATE_HOME', 'LOCALAPPDATA', 'OH_MY_SETTING_AUTO_UPDATE_STATE',
        'OMS_THREAD_ATTENTION', 'OMS_EXPERIMENT_CLAIM_TTL', 'OMS_RUN_CURRENT_TTL', 'OMS_GUARD_TTL',
        'OMS_THREAD_CURRENT_TTL', 'OMS_THREAD_STALE_TTL')}
    entries.append(('settings', settings))
    state_home = (os.environ.get('XDG_STATE_HOME') or
                  (os.environ.get('LOCALAPPDATA') if os.name == 'nt' else None))
    approvals = (Path(state_home).expanduser() if state_home else Path.home() / '.local/state')
    visit(approvals / 'oh-my-setting/approvals' / (hashlib.sha256(str(repo).encode()).hexdigest() + '.jsonl'),
          'approvals')
    visit(Path(os.environ.get('OH_MY_SETTING_AUTO_UPDATE_STATE') or ROOT / 'local/auto-update.status'),
          'auto-update')
    entries.append(('source', str(ROOT)))
    for name in ('terminal_panel.py', 'panel_results.py', 'panel_cache.py'):
        visit(Path(__file__).parent / name, 'source/' + name)
    return hashlib.sha256(json.dumps(entries, ensure_ascii=True).encode()).hexdigest()


def _body(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, allow_nan=False).encode()


def _load(repo, relative, identity, stamp):
    try:
        raw = DURABLE['read_no_follow'](str(repo), relative, 'panel cache',
                                       missing_ok=True, max_bytes=MAX_BYTES)
        data = json.loads(raw)
        if (data['schema'] == 1 and data['repo'] == identity and data['fingerprint'] == stamp
                and isinstance(data['created'], (int, float)) and math.isfinite(data['created'])
                and data['sha256'] == hashlib.sha256(_body(data['value'])).hexdigest()):
            return data
    except (OSError, ValueError, TypeError, KeyError, OverflowError, RecursionError, SystemExit):
        pass
    return None


def read_shared(repo, key, collect, cacheable=None):
    """Reuse fresh reads; a busy writer may replay only the same input generation.

    Expiry still refreshes clock-derived ages and process liveness. An input
    change during collection is discarded rather than labelled as current.
    Repositories without hook state keep the ordinary uncached read path.
    """
    repo = Path(repo).resolve()
    if not (repo / '.oms/hooks').is_dir():
        return collect()
    identity = hashlib.sha256(str(repo).encode()).hexdigest()
    relative = DIRECTORY + '/' + key + '.json'
    cached = None
    try:
        stamp = fingerprint(repo)
        cached = _load(repo, relative, identity, stamp)
        if cached and 0 <= time.time() - cached['created'] < INTERVAL and fingerprint(repo) == stamp:
            return cached['value']
        with _lock(repo, 'panel-cache-' + key, timeout=0):
            cached = _load(repo, relative, identity, stamp)
            if cached and 0 <= time.time() - cached['created'] < INTERVAL and fingerprint(repo) == stamp:
                return cached['value']
            value = collect()
            if fingerprint(repo) != stamp:
                # Mains write state all the time; a read that overlapped a write is still a valid read for this
                # board, it is just not stored for the others.
                return value
            if cacheable is not None and not cacheable(value):
                return value
            data = {'schema': 1, 'repo': identity, 'fingerprint': stamp,
                    'created': time.time(), 'value': value,
                    'sha256': hashlib.sha256(_body(value)).hexdigest()}
            try:
                path = DURABLE['canonical_repo_path'](str(repo), relative, 'panel cache', True)
                DURABLE['atomic_mutate'](path, lambda old: _body(data), str(repo),
                                        missing_ok=True, create_parent=True,
                                        max_input=MAX_BYTES, max_output=MAX_BYTES, label='panel cache')
            except (OSError, ValueError, SystemExit):
                # Read-only or unsafe cache storage must not break a successful collection.
                pass
            return value
    except ValueError as error:
        if str(error) == 'panel operation is busy; retry later':
            # Another board is collecting: show its last value while it works, or read directly without one.
            if cached:
                return cached['value']
            return collect()
        if str(error) == 'panel lock unavailable':
            return collect()
        raise
    except (OSError, SystemExit):
        return collect()
