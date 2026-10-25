"""Disposable, repository-bound panel reads; room selection stays in the caller."""

import calendar
import copy
import datetime
import hashlib
import json
import math
import os
import re
import runpy
import subprocess
from pathlib import Path
import stat
import time

from panel_results import DURABLE, _lock
from room_repository import harness_residue_parent

# Clock-only refreshes need multiple watch ticks to amortize the full collectors.
INTERVAL = 30
MAX_BYTES = 8 * 1024 * 1024
DIRECTORY = '.oms/hooks/panel-cache'
SKIPPED = {DIRECTORY, '.oms/hooks/panel-activity'}
ROOT = Path(__file__).resolve().parents[2]
LEDGER = '.oms/lifecycle/events.jsonl'
LEDGER_WINDOW = 256 * 1024
# The one place a cached read declares its .oms inputs. Only a read whose complete input set is known may be
# listed; every other key (the dashboard, which aggregates state.sh and the collectors it starts) keeps the
# whole-.oms walk. A new read_shared key must either list every .oms file it opens here or stay unlisted.
#   attempts       agent-events list --json: load_projection reads only lifecycle/events.jsonl
#                  (scripts/lib/agent-events.py event_list -> load_projection -> read_rows(event_path))
#   result-records _records (panel_results.py): that attempts list plus read_index of artifacts/index.jsonl
# result-records is derived from the attempts read, so both keys list the same inputs: a tick then captures one
# generation for the pair, and a ledger advance between the two reads fails the publication check.
_LEDGER_READS = ('lifecycle/events.jsonl', 'artifacts/index.jsonl')
KEY_INPUTS = {'attempts': _LEDGER_READS, 'result-records': _LEDGER_READS}
_ledger_source = []


def _ledger_token(path, size):
    """End offset of the last non-heartbeat row, so plain heartbeats keep the fingerprint.

    Any doubt returns the file size, which changes on every append.
    """
    try:
        if not _ledger_source:
            _ledger_source.append(runpy.run_path(str(Path(__file__).with_name('agent-events.py'))))
        with _ledger_source[0]['_ledger_fd'](Path(path)) as descriptor:
            end = os.fstat(descriptor).st_size
            start = max(0, end - LEDGER_WINDOW)
            os.lseek(descriptor, start, os.SEEK_SET)
            chunks, remaining = [], end - start
            while remaining:
                chunk = os.read(descriptor, min(65536, remaining))
                if not chunk:
                    return size
                chunks.append(chunk)
                remaining -= len(chunk)
        data = b''.join(chunks)
        if not data.endswith(b'\n'):
            return size
        lines = data.split(b'\n')[:-1]
        offsets, position = [], start
        for line in lines:
            position += len(line) + 1
            offsets.append(position)
        # A window that starts mid-row never parses its first fragment.
        first = 1 if start else 0
        for index in range(len(lines) - 1, first - 1, -1):
            row = json.loads(lines[index].decode('utf-8'))
            if (not isinstance(row, dict) or row.get('event_type') != 'attempt.heartbeat'
                    or 'refs' in row or 'usage' in row):
                return offsets[index]
    except Exception:
        pass
    return size


def _link(info):
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, 'st_file_attributes', 0) & 0x400)


def fingerprint(repo, inputs=None):
    """Stat inputs without reading logs or following symlinks into other trees.

    `inputs` names the .oms files a narrowed key depends on; None walks all of .oms.
    """
    entries = []

    def visit(path, label, info=None):
        try:
            if info is None:
                info = os.lstat(path)
        except FileNotFoundError:
            entries.append((label, None))
            return
        if stat.S_ISDIR(info.st_mode):
            # Cache writes must not invalidate their own parent directory.
            entries.append((label, info.st_mode, info.st_dev, info.st_ino))
            # Sort entry names rather than Path objects, and reuse no-follow
            # entry stats without constructing a Path for every retained file.
            with os.scandir(path) as children:
                ordered = sorted(children, key=lambda child: child.name)
            for child in ordered:
                relative = label + '/' + child.name
                # Mains' activity heartbeats are read by each board directly, not through this cache.
                if relative in SKIPPED or child.name.endswith(('.lock', '.tmp')):
                    continue
                try:
                    child_info = child.stat(follow_symlinks=False)
                except FileNotFoundError:
                    entries.append((relative, None))
                    continue
                visit(child.path, relative, child_info)
        elif label == LEDGER and stat.S_ISREG(info.st_mode):
            entries.append((label, info.st_mode, info.st_dev, info.st_ino,
                            _ledger_token(path, info.st_size)))
        else:
            entries.append((label, info.st_mode, info.st_dev, info.st_ino,
                            info.st_size, info.st_mtime_ns, info.st_ctime_ns))

    def declared(name):
        # Each ancestor must be a real directory, as the whole walk never descends through a link;
        # an unsafe or missing one is itself the input.
        label, path = '.oms', repo / '.oms'
        for part in [None] + name.split('/')[:-1]:
            if part is not None:
                label, path = label + '/' + part, path / part
            try:
                info = os.lstat(path)
            except FileNotFoundError:
                entries.append((label, None))
                return
            if _link(info) or not stat.S_ISDIR(info.st_mode):
                entries.append((label, info.st_mode, info.st_dev, info.st_ino,
                                info.st_size, info.st_mtime_ns, info.st_ctime_ns))
                return
            # Identity only: a child creation must not invalidate its ancestor.
            entries.append((label, info.st_mode, info.st_dev, info.st_ino))
        visit(repo / '.oms' / name, '.oms/' + name)

    if inputs is None:
        visit(repo / '.oms', '.oms')
    else:
        for name in inputs:
            declared(name)
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


def read_shared(repo, key, collect, cacheable=None, _tick=None):
    """Reuse fresh reads; a busy writer may replay only the same input generation.

    Expiry still refreshes clock-derived ages and process liveness. An input
    change during collection is discarded rather than labelled as current.
    A tick shares its starting generation across keys; each collection is
    checked again before publication.
    Repositories without hook state keep the ordinary uncached read path.
    """
    repo = Path(repo).resolve()
    if not (repo / '.oms/hooks').is_dir():
        return collect()
    identity = hashlib.sha256(str(repo).encode()).hexdigest()
    relative = DIRECTORY + '/' + key + '.json'
    cached = None
    try:
        inputs = KEY_INPUTS.get(key)
        # One walk per scope and tick: the whole-tree key is shared by every unlisted key.
        stamp = (fingerprint(repo, inputs) if _tick is None else
                 _tick._once('fingerprint:' + repr(inputs), lambda: fingerprint(repo, inputs)))
        cached = _load(repo, relative, identity, stamp)
        if cached and 0 <= time.time() - cached['created'] < INTERVAL:
            return cached['value']
        with _lock(repo, 'panel-cache-' + key, timeout=0):
            cached = _load(repo, relative, identity, stamp)
            if cached and 0 <= time.time() - cached['created'] < INTERVAL:
                return cached['value']
            value = collect()
            if fingerprint(repo, inputs) != stamp:
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


class TickReads:
    """Share evidence and failures only for one board collection, without caching actions."""

    def __init__(self, repo):
        self.repo = repo
        self.values = {}
        self.errors = {}

    def _once(self, key, collect):
        if key in self.errors:
            raise self.errors[key]
        if key not in self.values:
            try:
                self.values[key] = collect()
            except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError,
                    subprocess.SubprocessError) as error:
                self.errors[key] = error
                raise
        return self.values[key]

    def attempts(self):
        def collect():
            result = subprocess.run(['bash', str(ROOT / 'scripts/oms'), 'agent-events',
                                     '--repo', str(self.repo), 'list', '--json'],
                                    capture_output=True, text=True, check=False, timeout=30,
                                    stdin=subprocess.DEVNULL)
            if result.returncode:
                raise ValueError('attempt evidence unavailable')
            rows = json.loads(result.stdout)
            if not isinstance(rows, list):
                raise ValueError('invalid attempt evidence')
            return rows
        return self._once('attempts', lambda: read_shared(
            self.repo, 'attempts', lambda: self._once('agent-events', collect), _tick=self))

    def records(self):
        from panel_results import _records
        return self._once('result-records', lambda: read_shared(
            self.repo, 'result-records', lambda: self._once('records-query',
                lambda: _records(self.repo, _attempts=self.attempts())), _tick=self))


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


def _plan_record_current(path, directory, stamp):
    if isinstance(stamp, dict):
        resolved = os.path.realpath(path)
        return (resolved == stamp['resolved']
                and _plan_input_stamp(resolved, directory) == stamp['stamp'])
    return _plan_input_stamp(path, directory) == stamp


def _plan_claim_ttl(task):
    # Match agent-plan.sh's ASCII environment guard, then the engine's
    # per-task string parser (which also accepts Unicode decimal digits).
    raw = os.environ.get('OMS_PLAN_CLAIM_TTL') or '3600'
    default = int(raw) if re.fullmatch(r'[0-9]+', raw) else 3600
    value = task.get('ttl', '')
    return int(value) if isinstance(value, str) and value.isdigit() else default


def _plan_inputs_current(envelope):
    try:
        inputs = envelope['inputs']
        return (isinstance(inputs, list) and len(inputs) <= PLAN_INPUT_LIMIT
                and all(_plan_record_current(path, directory, stamp)
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
        try:
            inside = os.path.commonpath((str(repo), key)) == str(repo)
        except ValueError:  # Different Windows drives are external inputs.
            inside = False
        if inside:
            stamp = _plan_input_stamp(path, directory)
        else:
            # Preserve the spelling too: retargeted external symlinks must
            # invalidate, while /etc and HOME aliases remain cacheable.
            resolved = os.path.realpath(path)
            stamp = {'resolved': resolved, 'stamp': _plan_input_stamp(resolved, directory)}
        inputs[key] = (directory, stamp)
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
    tasks = plan.get('tasks', {})
    for task in tasks.values() if isinstance(tasks, dict) else tasks:
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
                owner_info = owner_stamp['stamp'] if isinstance(owner_stamp, dict) else owner_stamp
                total += owner_info[-3] if owner_info else 0
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
            for name in ('commondir', 'config', 'HEAD', 'packed-refs', 'shallow', 'info/grafts'):
                add(folder / name)
            if os.name == 'nt':
                add(folder / 'index')
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
        legacy_refs = False
        commits = set()
        for receipt in receipts:
            bundle = receipt['bundle']
            if receipt['completion_kind'] == 'satisfied-by':
                git_proof = True
                if bundle['successor']['product_kind'] == 'commit':
                    commits.add(bundle['successor']['commit_sha'])
                else:
                    relative('.oms/landings.jsonl')
            elif bundle.get('legacy_adoption') and receipt['snapshot'].get('evidence', {}).get('index', {}).get('base_sha'):
                git_proof = True
                legacy_refs = True
        if commits:
            core = runpy.run_path(str(ROOT / 'scripts/lib/plan-completion.py'))
            directory = Path(core['canonical_land_directory'](str(repo)))
            add(directory)
            names = list(directory.iterdir()) if directory.is_dir() else []
            if len(names) > 4096:
                raise ValueError('too many canonical commit receipts')
            for path in names:
                if not any(path.name.startswith(commit + '-') for commit in commits) or path.suffix != '.json':
                    continue
                add(path)
                raw = DURABLE['read_no_follow'](path.anchor, str(path.relative_to(path.anchor)),
                                               'plan cache land receipt', max_bytes=1024 * 1024)
                total += len(raw)
                if total > PLAN_METADATA_LIMIT:
                    raise ValueError('plan cache metadata limit exceeded')
                landed = json.loads(raw)
                ref = 'refs/remotes/%s/%s' % (landed.get('remote', ''), landed.get('target', ''))
                checked = subprocess.run(['git', 'check-ref-format', ref], env=env,
                                         capture_output=True, timeout=5, check=False)
                if checked.returncode:
                    continue
                # Packed refs are already guarded. Follow loose symbolic refs
                # once on a miss and stamp every spelling in that chain.
                for folder in git_dirs:
                    current = ref
                    for depth in range(16):
                        target = folder / current
                        add(target)
                        if not target.is_file():
                            break
                        raw_ref = DURABLE['read_no_follow'](str(folder), current,
                                                           'plan cache ref', max_bytes=4096).decode().strip()
                        if not raw_ref.startswith('ref: '):
                            break
                        current = raw_ref[5:]
                        if not current.startswith('refs/') or any(part in ('', '.', '..') for part in current.split('/')):
                            raise ValueError('unsafe symbolic Git ref')
                    else:
                        raise ValueError('too many symbolic Git refs')
        if git_proof:
            # Directory stamps catch additions/removals; file stamps also catch
            # in-place corruption of content-addressed objects and packed refs.
            for folder in git_dirs:
                if legacy_refs:
                    # Abbreviated legacy identities can resolve through refs.
                    tree(folder / 'refs')
                else:
                    tree(folder / 'refs/replace')
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
    residue = harness_residue_parent(repo)
    if residue is not None and residue.name.startswith('oh-my-setting-scratch.'):
        return collect()
    if any(os.environ.get(name) for name in (
            'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY',
            'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE')):
        return collect()

    def stamp():
        entries = [('plan-input-schema', 2)]
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
            ttl = _plan_claim_ttl(task)
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
            except (OSError, ValueError, TypeError, KeyError, AttributeError, RecursionError,
                    subprocess.SubprocessError, SystemExit):
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
