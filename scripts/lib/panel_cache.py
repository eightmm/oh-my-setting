"""Disposable, repository-bound panel reads; room selection stays in the caller."""

import calendar
import copy
import datetime
import hashlib
import json
import math
import os
import runpy
import subprocess
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


PLAN_INPUT_LIMIT = 16384
PLAN_METADATA_LIMIT = 64 * 1024 * 1024


def _plan_input_stamp(path, directory=False):
    try:
        info = Path(path).lstat()
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
        raise ValueError('unsafe plan cache input')
    identity = [info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_gid]
    if directory and stat.S_ISDIR(info.st_mode):
        # Ancestors are identity guards, not enumerated namespaces. Cache and
        # lock publication must not invalidate these guards themselves.
        return identity
    return identity + [info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns]


def _plan_inputs_current(envelope):
    try:
        inputs = envelope['inputs']
        return (isinstance(inputs, list) and len(inputs) <= PLAN_INPUT_LIMIT
                and all(_plan_input_stamp(path, directory) == stamp
                        for path, directory, stamp in inputs))
    except (OSError, ValueError, TypeError, KeyError):
        return False


def _plan_inputs(repo):
    """Discover validation reads once, without traversing the .oms tree.

    Receipt objects already contain the complete frozen proof references,
    including captured streams and approval snapshots. The artifact index and
    patch-land log are the only live streams used by validate_done. Git-backed
    proofs additionally guard the bounded local Git namespace and object store.
    """
    inputs = {}
    landing_paths = set()
    total = 0

    def add(path, directory=False):
        path = Path(os.path.abspath(path))
        key = str(path)
        if key in inputs and (directory or not inputs[key][0]):
            return
        if len(inputs) >= PLAN_INPUT_LIMIT:
            raise ValueError('plan cache input limit exceeded')
        inputs[key] = (directory, _plan_input_stamp(path, directory))
        if path.parent != path:
            add(path.parent, directory=True)

    def relative(value):
        if not isinstance(value, str) or not value:
            raise ValueError('invalid plan cache reference')
        if os.path.isabs(value):
            # Legacy successor intents retain absolute repo-local patch paths.
            value = str(Path(value).relative_to(repo)).replace(os.sep, '/')
        if '\\' in value or ':' in value or any(part in ('', '.', '..') for part in value.split('/')):
            raise ValueError('invalid plan cache reference')
        add(repo / value)
        if value.startswith('.oms/landing-patches/') and value.count('/') == 2:
            landing_paths.add(value)
        return value

    def metadata(value):
        nonlocal total
        rel = relative(value)
        raw = DURABLE['read_no_follow'](str(repo), rel, 'plan cache dependency',
                                        max_bytes=4 * 1024 * 1024)
        total += len(raw)
        if total > PLAN_METADATA_LIMIT:
            raise ValueError('plan cache metadata limit exceeded')
        return json.loads(raw)

    def references(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key in ('path', 'artifact', 'patch', 'decision_artifact') and isinstance(item, str) and item:
                    relative(item)
                else:
                    references(item)
        elif isinstance(value, list):
            for item in value:
                references(item)

    def tree(path):
        # Only Git/proof namespaces are enumerated; never the whole .oms tree.
        add(path)
        if Path(path).is_dir():
            for child in Path(path).iterdir():
                tree(child)

    plan = metadata('.oms/plan/tasks.json')
    receipts = []
    for task in plan.get('tasks', {}).values():
        if task.get('cancellation'):
            relative(task['cancellation']['decision_artifact'])
        if task.get('completion_receipt'):
            receipt = metadata(task['completion_receipt']['path'])
            references(receipt)
            receipts.append(receipt)
    if landing_paths:
        # The designated patch reader also proves private generation ownership
        # through the journal, owner certificate and publication hardlink.
        relative('.oms/landings.jsonl')
        journal_stamp = inputs[str(repo / '.oms/landings.jsonl')][1]
        total += journal_stamp[-3] if journal_stamp else 0
        if total > PLAN_METADATA_LIMIT:
            raise ValueError('plan cache metadata limit exceeded')
        capture = runpy.run_path(str(ROOT / 'scripts/lib/landing-capture.py'))
        reservations = capture['reservations'](str(repo))
        names = {Path(path).name for path in landing_paths}
        for item in reservations.values():
            if item['landing_id'] + '.patch' in names:
                owner = Path(item['directory']) / 'owner.json'
                add(owner)
                owner_stamp = inputs[str(owner)][1]
                total += owner_stamp[-3] if owner_stamp else 0
                if total > PLAN_METADATA_LIMIT:
                    raise ValueError('plan cache metadata limit exceeded')
                certificate = capture['certificate'](item)
                publication = certificate['objects'].get('publication')
                if publication:
                    add(publication['anchor'])
                    add(publication['path'])
    if receipts:
        relative('.oms/artifacts/index.jsonl')
        # Resolve gitfiles/commondir once; subsequent ticks use only lstat.
        env = dict(os.environ, GIT_NO_LAZY_FETCH='1', GIT_ALLOW_PROTOCOL='', GIT_OPTIONAL_LOCKS='0')
        for name in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
                     'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE'):
            env.pop(name, None)
        query = subprocess.run(['git', '-C', str(repo), 'rev-parse', '--git-dir', '--git-common-dir'],
                               capture_output=True, text=True, timeout=5, check=False, env=env,
                               stdin=subprocess.DEVNULL)
        paths = query.stdout.replace('\r', '').splitlines()
        if query.returncode or len(paths) != 2:
            raise ValueError('plan cache Git namespace unavailable')
        add(repo / '.git')
        git_dirs = {Path(os.path.abspath(repo / name)) for name in paths}
        for folder in git_dirs:
            add(folder, directory=True)
            for name in ('commondir', 'config', 'HEAD', 'index', 'packed-refs', 'shallow', 'info/grafts'):
                add(folder / name)
        # Git may read global or included config outside the worktree. Record
        # active origins and missing include targets so creation also invalidates.
        for path in (Path.home() / '.gitconfig',
                     Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config') / 'git/config',
                     Path(os.environ.get('GIT_CONFIG_SYSTEM') or '/etc/gitconfig'),
                     Path(os.environ.get('GIT_CONFIG_GLOBAL') or Path.home() / '.gitconfig')):
            add(path)
        config = subprocess.run(['git', '-C', str(repo), 'config', '--null', '--show-origin', '--includes', '--list'],
                                capture_output=True, timeout=5, check=False, env=env, stdin=subprocess.DEVNULL)
        if config.returncode or len(config.stdout) > 4 * 1024 * 1024:
            raise ValueError('plan cache Git config unavailable')
        parts = config.stdout.decode('utf-8').rstrip('\0').split('\0') if config.stdout else []
        if len(parts) % 2:
            raise ValueError('invalid plan cache Git config origins')
        for origin, entry in zip(parts[::2], parts[1::2]):
            if origin.startswith('file:'):
                config_path = Path(os.path.abspath(repo / origin[5:]))
                add(config_path)
            key, _, value = entry.partition('\n')
            if key == 'include.path' or key.startswith('includeif.') and key.endswith('.path'):
                if not origin.startswith('file:') or value.startswith('%'):
                    raise ValueError('unresolved plan cache Git include')
                add(config_path.parent / Path(value).expanduser())
        git_proof = False
        for receipt in receipts:
            bundle = receipt['bundle']
            if receipt['completion_kind'] == 'satisfied-by':
                git_proof = True
                if bundle['successor']['product_kind'] == 'commit':
                    core = runpy.run_path(str(ROOT / 'scripts/lib/plan-completion.py'))
                    tree(core['canonical_land_directory'](str(repo)))
                else:
                    relative('.oms/landings.jsonl')
            elif bundle.get('legacy_adoption') and receipt['snapshot'].get('evidence', {}).get('index', {}).get('base_sha'):
                git_proof = True
        if git_proof:
            # Directory stamps catch additions/removals; file stamps also catch
            # in-place corruption of content-addressed objects and packed refs.
            for folder in git_dirs:
                tree(folder / 'refs')
                tree(folder / 'objects')
                alternate = folder / 'objects/info/alternates'
                if alternate.exists():
                    raise ValueError('alternate Git object stores are not cacheable')
    return [[path, directory, stamp] for path, (directory, stamp) in sorted(inputs.items())]


def read_plan_list(repo, collect):
    """Share stat-bound list reads, refreshing ages and fencing TTL expiry.

    Expired claims require fresh worker/PID evidence. Completion dependencies
    are discovered only on a miss, then checked by stat before reuse.
    """
    repo = Path(repo).resolve()
    if not (repo / '.oms/hooks').is_dir():
        return collect()

    # agent-plan redirects managed scratch worktrees to their main checkout.
    # snapshot normally already receives that canonical state repository.
    if repo.name == 'wt' and repo.parent.name.startswith('oh-my-setting-scratch.'):
        return collect()
    if any(os.environ.get(name) for name in (
            'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY',
            'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE')):
        return collect()

    def stamp():
        entries = []
        for path in (repo / '.oms', repo / '.oms/plan', repo / '.oms/plan/tasks.json',
                     repo / '.oms/plan/completions', repo / 'PROJECT.md',
                     ROOT / 'scripts/agent-plan.sh', ROOT / 'scripts/lib/agent_plan_core.py',
                     ROOT / 'scripts/lib/agent-plan-engine.py',
                     ROOT / 'scripts/lib/task-assignment.py', ROOT / 'scripts/lib/project-state.py',
                     *(ROOT / 'scripts/lib' / name for name in (
                         'plan-completion.py', 'plan-receipt.py', 'durable-jsonl.py',
                         'path_scope.py', 'landing-capture.py', 'main_reviewer.py',
                         'process_liveness.py', 'verify-floor-lint.py', 'plan-retire.py',
                         'agent-memory-common.sh', 'oms-common.sh', 'provider-registry.sh',
                         'harness-residue.sh', 'file-lock.sh', 'project-state.sh')),
                     ROOT / 'scripts/oms', Path(__file__)):
            try:
                info = path.lstat()
            except FileNotFoundError:
                entries.append((str(path), None))
                continue
            # Leaf/ancestor symlinks can change their targets without changing lstat.
            if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
                return None
            entries.append((str(path), info.st_dev, info.st_ino, info.st_mode,
                            info.st_size, info.st_mtime_ns, info.st_ctime_ns))
        entries.append(('settings', {name: value for name, value in os.environ.items()
                                    if name.startswith('GIT_') or name in (
                                        'OMS_PLAN_CLAIM_TTL', 'HOME', 'XDG_CONFIG_HOME',
                                        'XDG_STATE_HOME', 'PATH')}))
        return hashlib.sha256(_body(entries)).hexdigest()

    def refreshed(value):
        value = copy.deepcopy(value)
        now = int(time.time())
        default = os.environ.get('OMS_PLAN_CLAIM_TTL', '3600')
        default = int(default) if default.isascii() and default.isdigit() else 3600
        for task in value['tasks']:
            state = task.get('state')
            if state not in ('claimed', 'running', 'review'):
                continue
            anchor = None
            fields = ('updated',) if state == 'review' else ('claimed_at', 'updated')
            for field in fields:
                try:
                    anchor = calendar.timegm(datetime.datetime.strptime(
                        task.get(field, ''), '%Y-%m-%dT%H:%M:%SZ').timetuple())
                    break
                except (ValueError, TypeError):
                    continue
            if anchor is None:
                continue
            age = now - anchor
            ttl = task.get('ttl', '')
            ttl = int(ttl) if isinstance(ttl, str) and ttl.isdigit() else default
            if state == 'claimed' and age >= ttl:
                return None
            task['claim_age_s'] = age
        return value

    identity = hashlib.sha256(str(repo).encode()).hexdigest()
    relative = DIRECTORY + '/plan-list.json'
    try:
        generation = stamp()
        if generation is None:
            return collect()
        cached = _load(repo, relative, identity, generation)
        if cached and _plan_inputs_current(cached['value']):
            value = refreshed(cached['value']['result'])
            if value is not None and stamp() == generation and _plan_inputs_current(cached['value']):
                return value
        with _lock(repo, 'panel-cache-plan-list', timeout=0):
            cached = _load(repo, relative, identity, generation)
            if cached and _plan_inputs_current(cached['value']):
                value = refreshed(cached['value']['result'])
                if value is not None and stamp() == generation and _plan_inputs_current(cached['value']):
                    return value
            try:
                inputs = _plan_inputs(repo)
            except (OSError, ValueError, TypeError, KeyError, RecursionError, subprocess.SubprocessError, SystemExit):
                return collect()
            value = collect()
            envelope = {'result': value, 'inputs': inputs}
            if (stamp() == generation and refreshed(value) is not None
                    and _plan_inputs_current(envelope)):
                data = {'schema': 1, 'repo': identity, 'fingerprint': generation,
                        'created': time.time(), 'value': envelope,
                        'sha256': hashlib.sha256(_body(envelope)).hexdigest()}
                try:
                    path = DURABLE['canonical_repo_path'](str(repo), relative, 'panel cache', True)
                    DURABLE['atomic_mutate'](path, lambda old: _body(data), str(repo),
                                            missing_ok=True, create_parent=True,
                                            max_input=MAX_BYTES, max_output=MAX_BYTES, label='panel cache')
                except (OSError, ValueError, SystemExit):
                    pass
            return value
    except ValueError as error:
        if str(error) in ('panel operation is busy; retry later', 'panel lock unavailable'):
            return collect()
        raise
    except (OSError, SystemExit):
        return collect()
