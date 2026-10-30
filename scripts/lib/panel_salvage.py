"""Capture stopped delegate bytes for owner review, without trusting checkout Git config."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import stat
import subprocess
import sys
import tempfile
import unicodedata
import uuid

import dispatch_supervisor
import panel_results
from room_repository import LIBRARY, harness_residue_parent, state_repository, work_repository

PREFIX = b'# oms-salvage: '
MAX_EVIDENCE = 8 * 1024 * 1024


def _native_path(value):
    # Use the same trusted native-library bridge as room_repository; Git Bash
    # marker spellings are not native Python paths on Windows.
    if os.name != 'nt' and not os.environ.get('MSYSTEM'):
        return Path(value)
    query = subprocess.run(['bash', '-c', '. "${1%/*}/peer-common.sh" || exit $?; '
                            'ma_artifact_index_python_path "$2"',
                            'salvage-native-path', LIBRARY.as_posix(), str(value)],
                           env=dict(os.environ, MSYS2_ARG_CONV_EXCL='*'),
                           stdin=subprocess.DEVNULL, capture_output=True, text=True,
                           check=False, timeout=15)
    if query.returncode:
        raise ValueError('native salvage path conversion failed: ' + query.stderr[:160].strip())
    return Path(query.stdout.replace('\r', '').rstrip('\n'))


def _worktree_root():
    cache = os.environ.get('XDG_CACHE_HOME') or str(Path.home() / '.cache')
    return _native_path(os.environ.get('OMS_DELEGATE_WORKTREE_ROOT') or
                str(Path(cache) / 'oh-my-setting/worktrees')).resolve(strict=True)


def _read(root, relative, maximum=MAX_EVIDENCE, missing=False):
    return panel_results._read(root, relative, maximum, missing)


def _git_directory(repo):
    entry = repo / '.git'
    if entry.is_dir() and not entry.is_symlink():
        return entry.resolve(strict=True)
    value = _read(repo, '.git', 4096).decode().replace('\r', '').strip()
    if not value.startswith('gitdir: '):
        raise ValueError('invalid worktree gitfile')
    return (repo / _native_path(value[8:])).resolve(strict=True)


def _common(directory):
    value = _read(directory, 'commondir', 4096, True).decode().replace('\r', '').strip()
    return (directory / _native_path(value)).resolve(strict=True) if value else directory


def _managed(repo, path):
    kept = _native_path(path).expanduser()
    physical = kept.resolve(strict=True)
    root = _worktree_root()
    residue = harness_residue_parent(physical)
    if (kept != physical or residue is None or residue.parent != root or
            not residue.name.startswith('oh-my-setting-delegate.')):
        raise ValueError('kept path is not an OMS-managed delegate worktree')
    marker = _read(physical.parent, '.oh-my-setting-tmp', 8192).decode()
    fields = {}
    for line in marker.splitlines():
        key, equals, value = line.partition('=')
        if not equals or key in fields:
            raise ValueError('invalid kept worktree marker')
        fields[key] = value
    marker_repo = _native_path(fields.get('repo', '')).resolve(strict=True)
    if (fields.get('kind') != 'oh-my-setting-temp' or fields.get('temporary') != '1' or
            _native_path(fields.get('worktree', '')).resolve() != physical or
            _common(_git_directory(marker_repo)) != _common(_git_directory(repo))):
        raise ValueError('kept worktree marker does not belong to this repository')
    directory = _git_directory(physical)
    common = _common(_git_directory(repo))
    if (_common(directory) != common or directory.parent != common / 'worktrees' or
            _native_path(_read(directory, 'gitdir', 4096).decode().replace('\r', '').strip()).resolve() !=
            physical / '.git'):
        raise ValueError('kept worktree registration does not match this repository')
    head = _read(directory, 'HEAD', 4096).decode().strip()
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', head):
        raise ValueError('kept worktree must have a detached HEAD')
    return physical, common, head


def _latest(repo, task, attempt_id=None):
    attempts = panel_results._query(repo, 'agent-events', 'list')
    rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
    matching = [row for row in rows if row.get('kind') == 'delegate' and row.get('task_id') == task]
    candidates = [a for a in attempts if a.get('task_id') == task and
                  (a.get('tool') == 'peer-delegate' or a.get('refs', {}).get('panel_access') == 'write')]
    if attempt_id is not None:
        candidates = [a for a in candidates if a.get('attempt_id') == attempt_id]
        if len(candidates) != 1:
            raise ValueError('selected attempt does not uniquely belong to task ' + task)
        matching = [row for row in matching if row.get('attempt_id') == attempt_id]
    record = matching[-1] if matching else {}
    attempt = max(candidates, key=lambda a: a.get('created_at', ''), default={})
    if record.get('attempt_id') and attempt and record['attempt_id'] != attempt['attempt_id']:
        raise ValueError('latest delegate attempt has no matching artifact; refusing older result')
    if attempt and not attempt.get('terminal'):
        raise ValueError('latest delegate attempt is not stopped')
    if not attempt and (not record or record.get('exit') == 0):
        raise ValueError('no stopped delegate evidence for task ' + task)
    text = ''
    if record.get('artifact'):
        if not record['artifact'].startswith('.oms/artifacts/'):
            raise ValueError('delegate artifact is outside OMS evidence')
        text = panel_results._indexed(repo, record, MAX_EVIDENCE).decode('utf-8')
    matches = re.findall(r'^(?:error: )?worktree kept(?: at|:) (.+)$|^- worktree: (/.+)$', text, re.M)
    paths = [a or b for a, b in matches]
    location = attempt.get('refs', {}).get('panel_location', '')
    delegate_path = None
    if location.startswith('worktree:oh-my-setting-delegate.'):
        relative = location[len('worktree:'):]
        components = relative.split('/')
        if (components[0].startswith('oh-my-setting-delegate.') and len(components) > 1 and
                all(part not in {'', '.', '..'} and '\\' not in part and ':' not in part
                    for part in components)):
            root = _worktree_root().resolve(strict=True)
            candidate = Path(_native_path(root / relative)).resolve(strict=False)
            if candidate != root and root in candidate.parents:
                delegate_path = str(candidate)
                paths.append(delegate_path)
    if not paths:
        raise ValueError('latest delegate has no recorded kept worktree path')
    paths = [str(_native_path(value)) for value in paths]
    if len(set(paths)) != 1:
        raise ValueError('conflicting kept worktree evidence')
    reason = attempt.get('reason_code') or ('delegate exit ' + str(record.get('exit', 'unknown')))
    refs = attempt.get('refs', {})
    participant, room = refs.get('panel_room_participant'), refs.get('panel_room_id')
    if (not isinstance(participant, str) or not re.fullmatch(r'call-[0-9a-f]{16}', participant) or
            not isinstance(room, str)):
        raise ValueError('latest delegate has no recorded room execution owner')
    owned = [row for row in attempts if row.get('refs', {}).get('panel_room_participant') == participant and
             row.get('refs', {}).get('panel_room_id') == room]
    if (len(owned) != 1 or owned[0].get('attempt_id') != attempt.get('attempt_id') or
            owned[0].get('task_id') != task):
        raise ValueError('room call does not uniquely own the latest delegate attempt')
    anchor = dispatch_supervisor.read(repo, participant + '-launch.json')
    call = anchor.get('call') if isinstance(anchor, dict) else None
    execution = anchor.get('execution_repo') if isinstance(anchor, dict) else None
    if (not isinstance(call, dict) or call.get('participant') != participant or
            call.get('room') != room or
            call.get('task_id') != task or
            call.get('attempt_id', attempt.get('attempt_id')) != attempt.get('attempt_id') or
            not isinstance(execution, str) or not os.path.isabs(execution)):
        raise ValueError('latest delegate has no matching recorded execution repository')
    try:
        execution = work_repository(execution)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise ValueError('recorded execution repository is unavailable') from error
    return (paths[-1], attempt.get('attempt_id') or record.get('attempt_id'), reason,
            execution, record, bool(delegate_path and paths[-1] == delegate_path))


def _standalone(repo, attempt_id, artifact):
    """Recover an explicitly selected failed call using indexed, hashed evidence."""
    attempts = panel_results._query(repo, 'agent-events', 'list')
    owned = [a for a in attempts if a.get('attempt_id') == attempt_id]
    if len(owned) != 1 or owned[0].get('tool') != 'peer-delegate' or not owned[0].get('terminal'):
        raise ValueError('standalone recovery requires one stopped delegate attempt')
    attempt = owned[0]
    refs = attempt.get('refs', {})
    if refs.get('panel_room_participant') or refs.get('panel_room_id'):
        raise ValueError('room delegates require the recorded room execution owner')
    rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
    records = [r for r in rows if r.get('kind') == 'delegate' and
               r.get('attempt_id') == attempt_id and r.get('artifact') == artifact]
    if (len(records) != 1 or not isinstance(records[0].get('exit'), int) or
            records[0]['exit'] == 0 or
            records[0].get('task_id', '') != attempt.get('task_id', '')):
        raise ValueError('standalone recovery requires matching failed indexed artifact')
    record = records[0]
    if not isinstance(artifact, str) or not artifact.startswith('.oms/artifacts/'):
        raise ValueError('delegate artifact is outside OMS evidence')
    body = _read(repo, artifact)
    if hashlib.sha256(body).hexdigest() != record.get('artifact_sha256'):
        raise ValueError('standalone artifact digest does not match index')
    paths = [a or b for a, b in re.findall(
        r'^(?:error: )?worktree kept(?: at|:) (.+)$|^- worktree: (/.+)$',
        body.decode('utf-8'), re.M)]
    paths = [str(_native_path(value)) for value in paths]
    if not paths or len(set(paths)) != 1:
        raise ValueError('standalone artifact has no unique kept worktree path')
    base = record.get('base_sha', '')
    if not isinstance(base, str) or not re.fullmatch(r'[0-9a-f]{7,64}', base):
        raise ValueError('standalone artifact has no recorded base')
    execution = Path(repo).resolve(strict=True)
    if not _managed(execution, paths[0])[2].startswith(base):
        raise ValueError('standalone kept HEAD does not match recorded base')
    return (paths[0], attempt_id, attempt.get('reason_code') or
            'delegate exit ' + str(record['exit']), execution)


def _skeleton(line):
    return ''.join(c for c in unicodedata.normalize('NFKD', line) if ord(c) < 128)


def encoding_warnings(path, before, after, diff):
    # File totals cannot distinguish encoding loss from ordinary rewrites.
    return _transliteration_warnings(path, diff)


def _encoding_loss(before, after):
    if before.isascii():
        return False
    for run in re.findall(r'[^\x00-\x7f]+', before):
        for encoding in ('latin-1', 'cp1252'):
            try:
                mojibake = run.encode('utf-8').decode(encoding)
            except UnicodeDecodeError:
                continue
            if mojibake in after:
                return True
    changed = False
    for tag, i, j, k, end in difflib.SequenceMatcher(None, before, after, autojunk=False).get_opcodes():
        if tag == 'equal':
            continue
        removed, added = before[i:j], after[k:end]
        if tag not in ('replace', 'delete') or any(ord(char) < 128 for char in removed):
            return False
        if added and added != _skeleton(removed) and any(char not in '?\ufffd' for char in added):
            return False
        changed = True
    return changed


def _hunk_encoding_warnings(path, heading, removed, added):
    # Exact equal lines anchor the replacement blocks; extra lines stay unpaired.
    matcher = difflib.SequenceMatcher(None, removed, added)
    for tag, i, j, k, end in matcher.get_opcodes():
        if tag != 'replace':
            continue
        if any(_encoding_loss(old, new) for old, new in zip(removed[i:j], added[k:end])):
            warnings = [{'file': path, 'kind': 'transliteration_hunk', 'hunk': heading}]
            old_count = sum(sum(byte >= 128 for byte in line.encode('utf-8')) for line in removed)
            new_count = sum(sum(byte >= 128 for byte in line.encode('utf-8')) for line in added)
            if new_count < old_count:
                warnings.append({'file': path, 'kind': 'non_ascii_drop', 'hunk': heading,
                                 'before_non_ascii_bytes': old_count, 'after_non_ascii_bytes': new_count})
            return warnings
    return []


def _transliteration_warnings(path, diff):
    warnings = []
    removed, added, hunk = [], [], None

    def inspect():
        warnings.extend(_hunk_encoding_warnings(path, hunk, removed, added))

    for line in diff.decode('utf-8', errors='replace').split('\n'):
        if line.startswith('@@'):
            inspect()
            removed, added, hunk = [], [], line
        elif hunk and line.startswith('-'):
            removed.append(line[1:])
        elif hunk and line.startswith('+'):
            added.append(line[1:])
    inspect()
    return warnings


def patch_encoding_warnings(patch):
    """Find encoding loss from unified diff hunks, without salvage provenance."""
    import shlex

    text = patch.decode('utf-8', errors='replace') if isinstance(patch, bytes) else str(patch)
    warnings = []
    path = None
    hunk = None
    removed, added = [], []
    hunks = []

    def inspect_file():
        if path is None or not hunks:
            return
        for heading, old, new in hunks:
            warnings.extend(_hunk_encoding_warnings(path, heading, old, new))

    def finish_hunk():
        if hunk is not None:
            hunks.append((hunk, removed, added))

    for line in text.split('\n'):
        if line.startswith('diff --git '):
            finish_hunk()
            inspect_file()
            path = None
            try:
                fields = shlex.split(line)
                if len(fields) >= 4 and fields[3].startswith('b/'):
                    path = fields[3][2:]
            except ValueError:
                pass
            hunk, removed, added, hunks = None, [], [], []
        elif line.startswith(('GIT binary patch', 'Binary files ')):
            finish_hunk()
            hunk, removed, added, hunks = None, [], [], []
            path = None
        elif line.startswith('@@'):
            finish_hunk()
            hunk, removed, added = line, [], []
        elif hunk and line.startswith('-'):
            removed.append(line[1:])
        elif hunk and line.startswith('+'):
            added.append(line[1:])
        elif hunk and line.startswith('\\ No newline'):
            continue
    finish_hunk()
    inspect_file()
    return warnings


def _capture(kept, common, base, head=None):
    # No invocation opens the kept repository's config, index or attributes.
    # Raw blobs bypass even working-tree-encoding and text normalization.
    with tempfile.TemporaryDirectory(prefix='oms-salvage-') as scratch:
        scratch = Path(scratch)
        directory = scratch / 'git'
        directory.mkdir()
        (directory / 'objects').mkdir()
        (directory / 'refs').mkdir()
        (directory / 'HEAD').write_text(base + '\n')
        (directory / 'config').write_text('[core]\nrepositoryformatversion = %s\nbare = false\n' %
                                        (1 if len(base) == 64 else 0) +
                                        ('[extensions]\nobjectFormat = sha256\n' if len(base) == 64 else ''))
        (directory / 'info').mkdir()
        (directory / 'info/exclude').write_bytes(_read(common, 'info/exclude', MAX_EVIDENCE, True))
        env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        env.update(HOME=str(scratch), XDG_CONFIG_HOME=str(scratch), GIT_CONFIG_NOSYSTEM='1',
                   GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull, GIT_ATTR_NOSYSTEM='1',
                   GIT_ALTERNATE_OBJECT_DIRECTORIES=str(common / 'objects'), GIT_NO_LAZY_FETCH='1',
                   GIT_OPTIONAL_LOCKS='0')
        command = ['git', '--git-dir=' + str(directory), '--work-tree=' + str(kept),
                   '-c', 'core.hooksPath=' + os.devnull, '-c', 'core.fsmonitor=',
                   '-c', 'diff.external=', '-c', 'core.attributesFile=' + os.devnull]

        def git(*args, data=None):
            result = subprocess.run(command + list(args), input=data, capture_output=True, env=env,
                                    cwd=scratch, timeout=60, check=False)
            if result.returncode:
                raise ValueError('safe salvage Git failed: ' + result.stderr.decode(errors='replace')[:300])
            return result.stdout

        git('read-tree', base)
        tracked = git('ls-files', '--stage', '-z').split(b'\0')
        entries = {}
        for entry in filter(None, tracked):
            header, path = entry.split(b'\t', 1)
            mode, oid, stage = header.split()
            if mode == b'160000':
                raise ValueError('salvage of submodule worktrees requires owner inspection')
            entries[os.fsdecode(path)] = (mode.decode(), oid.decode())
        others = git('ls-files', '--others', '--exclude-standard', '-z').split(b'\0')
        # Paths the worker committed (even force-added ones matching ignore rules) are in neither set above.
        committed = {}
        if head and head != base:
            for entry in filter(None, git('ls-tree', '-r', '-z', head).split(b'\0')):
                header, path = entry.split(b'\t', 1)
                mode = header.split()[0]
                if mode == b'160000':
                    raise ValueError('salvage of submodule worktrees requires owner inspection')
                committed[path] = mode.decode()
        others = others + list(committed)
        # peer-common.sh creates this relay queue in the worktree and leaves it after an interrupted
        # run; only untracked top-level directories of that form are harness residue.
        relay = re.compile(r'\.oms-room-relay\.[A-Za-z0-9]{6}/')
        files, warnings = [], []
        for name in sorted(set(entries) | {os.fsdecode(p) for p in others
                                           if p and not relay.match(os.fsdecode(p))}):
            path = kept / name
            # Never follow a directory symlink into unrelated data.
            if path.parent.resolve() != path.parent:
                raise ValueError('salvage path has a symlinked parent: ' + name)
            before = git('cat-file', 'blob', entries[name][1]) if name in entries else b''
            try:
                info = path.lstat()
            except FileNotFoundError:
                info = None
            if info is None or stat.S_ISDIR(info.st_mode):
                if name not in entries:
                    continue
                after, mode = b'', None
            elif stat.S_ISLNK(info.st_mode):
                after, mode = os.fsencode(os.readlink(path)), '120000'
            elif stat.S_ISREG(info.st_mode):
                after = _read(kept, name, 256 * 1024 * 1024)
                mode = '100755' if info.st_mode & 0o111 else '100644'
                if os.name == 'nt':
                    mode = committed.get(os.fsencode(name), entries.get(name, ('100644', ''))[0])
            else:
                raise ValueError('salvage refuses special file: ' + name)
            if name in entries and before == after and entries[name][0] == mode:
                continue
            if mode is None:
                git('update-index', '--force-remove', '--', name)
            else:
                oid = git('hash-object', '-w', '--stdin', '--no-filters', data=after).decode().strip()
                git('update-index', '--add', '--cacheinfo', mode, oid, name)
            diff = git('diff', '--cached', '--binary', '--no-ext-diff', '--no-textconv', base, '--', name)
            files.append({'path': name, 'before_bytes': len(before), 'after_bytes': len(after)})
            warnings.extend(encoding_warnings(name, before, after, diff))
        patch = git('diff', '--cached', '--binary', '--no-ext-diff', '--no-textconv',
                    '--src-prefix=a/', '--dst-prefix=b/', base)
        return patch, files, warnings


def _payload(body):
    # Git ignores wrapper text before the first diff. Admission keys the diff
    # bytes, so editing display metadata or adding blank wrapper lines cannot
    # change the controller's provenance lookup.
    start = re.search(br'^diff --git ', body, re.M)
    return (body[start.start():] if start else body).rstrip(b'\n') + b'\n'


def _provenance_path(payload_sha):
    if not re.fullmatch(r'[0-9a-f]{64}', payload_sha):
        raise ValueError('invalid salvage payload digest')
    return '.oms/salvage-provenance/' + payload_sha + '.json'


def _provenance(raw, payload_sha):
    if not raw:
        return {'schema': 1, 'payload_sha256': payload_sha, 'records': []}
    value = json.loads(raw)
    if (not isinstance(value, dict) or value.get('schema') != 1 or
            value.get('payload_sha256') != payload_sha or not isinstance(value.get('records'), list) or
            not value['records'] or any(not isinstance(row, dict) or
                not isinstance(row.get('salvaged_from'), dict) or
                not isinstance(row.get('encoding_warnings'), list) for row in value['records'])):
        raise ValueError('invalid controller salvage provenance')
    return value


def record_provenance(repo, metadata):
    panel_results._owner()
    payload_sha = metadata['payload_sha256']
    relative = _provenance_path(payload_sha)
    record = {key: metadata[key] for key in ('salvaged_from', 'encoding_warnings', 'base_sha')}

    def append(old):
        value = _provenance(old, payload_sha)
        if record not in value['records']:
            value['records'].append(record)
        return (json.dumps(value, ensure_ascii=True, sort_keys=True) + '\n').encode()

    target = panel_results.DURABLE['canonical_repo_path'](str(repo), relative, 'salvage provenance', True)
    panel_results.DURABLE['atomic_mutate'](target, append, str(repo), missing_ok=True,
                                         create_parent=True, max_input=MAX_EVIDENCE,
                                         max_output=MAX_EVIDENCE, label='salvage provenance')
    return relative


def _empty_provenance_identity(task, attempt):
    identity = json.dumps([task, attempt], ensure_ascii=True, separators=(',', ':')).encode()
    return hashlib.sha256(identity).hexdigest()


def _empty_provenance_path(task, attempt):
    return '.oms/salvage-provenance/empty-' + _empty_provenance_identity(task, attempt) + '.json'


def _attempt_recovery_evidence(repo, task, attempt, indexed=None, attempt_row=None, execution=None):
    """Name any durable patch or registered checkout that prevents empty salvage."""
    repo = Path(repo)
    evidence = []
    indexed_rows = [indexed] if isinstance(indexed, dict) else []
    indexed_elsewhere_patch_paths = set()
    try:
        rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
        indexed_elsewhere_patch_paths = {
            row.get('patch') for row in rows
            if isinstance(row.get('patch'), str) and row.get('patch').startswith('.oms/artifacts/')
            and isinstance(row.get('attempt_id'), str) and row.get('attempt_id') != attempt
        }
        indexed_rows.extend(row for row in rows if row.get('task_id') == task
                            and row.get('attempt_id') == attempt)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        raise ValueError('cannot verify delegate recovery evidence; refusing empty salvage') from error
    indexed_patch_paths = set()
    for row in indexed_rows:
        patch = row.get('patch')
        if isinstance(patch, str) and patch.startswith('.oms/artifacts/'):
            indexed_patch_paths.add(patch)
            path = repo / patch
            if path.is_file() and not path.is_symlink() and path.stat().st_size:
                evidence.append(patch)

    if attempt_row is None:
        try:
            rows = panel_results._query(repo, 'agent-events', 'list')
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            raise ValueError('cannot verify delegate recovery evidence; refusing empty salvage') from error
        matches = [row for row in rows if row.get('task_id') == task and row.get('attempt_id') == attempt]
        attempt_row = matches[0] if len(matches) == 1 else {}
    refs = attempt_row.get('refs', {})
    provider = refs.get('panel_provider') or attempt_row.get('provider')
    slug = re.sub(r'[^a-z0-9]+', '-', str(task).lower()).strip('-')[:48]
    created = attempt_row.get('created_at', '')
    if not indexed_patch_paths and isinstance(provider, str) and re.fullmatch(r'[a-z0-9_-]+', provider) and slug:
        try:
            from datetime import datetime, timedelta
            started = datetime.strptime(created, '%Y-%m-%dT%H:%M:%SZ')
            low, high = started - timedelta(minutes=5), started + timedelta(minutes=5)
            artifact_dir = repo / '.oms/artifacts/delegate'
            for path in artifact_dir.glob('%s-%s-*.patch' % (provider, slug)):
                if path.is_symlink() or not path.is_file():
                    continue
                relative = path.relative_to(repo).as_posix()
                if relative in indexed_elsewhere_patch_paths:
                    continue
                match = re.fullmatch(r'%s-%s-(\d{8}T\d{6}Z)-(\d+)\.patch' %
                                     (re.escape(provider), re.escape(slug)), path.name)
                if match and path.stat().st_size:
                    stamp = datetime.strptime(match.group(1), '%Y%m%dT%H%M%SZ')
                    if low <= stamp <= high:
                        evidence.append(relative)
        except (TypeError, ValueError, OSError):
            pass

    if execution is not None:
        location = attempt_row.get('refs', {}).get('panel_location', '')
        parts = location[len('worktree:'):].split('/') if location.startswith('worktree:') else []
        expected_parent = parts[0] if len(parts) == 2 and parts[-1] in ('wt', 'wt-' + parts[0].rpartition('.')[2]) else None
        if expected_parent:
            env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
            env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                       GIT_CONFIG_SYSTEM=os.devnull, GIT_OPTIONAL_LOCKS='0')
            try:
                listed = subprocess.run(['git', '-C', str(execution), 'worktree', 'list', '--porcelain'],
                                        capture_output=True, text=True, env=env, timeout=15, check=False)
            except (OSError, subprocess.TimeoutExpired) as error:
                raise ValueError('cannot verify registered delegate worktrees; refusing empty salvage') from error
            if listed.returncode:
                raise ValueError('cannot verify registered delegate worktrees; refusing empty salvage')
            for block in listed.stdout.split('\n\n'):
                entry = next((line[9:] for line in block.splitlines() if line.startswith('worktree ')), None)
                if entry:
                    entry_path = _native_path(entry)
                    if (entry_path.is_dir() and entry_path.name == parts[-1]
                            and entry_path.parent.name == expected_parent):
                        evidence.append('registered worktree: ' + str(entry_path))

    provenance_dir = repo / '.oms/salvage-provenance'
    if provenance_dir.is_dir():
        for path in provenance_dir.glob('*.json'):
            if path.name.startswith('empty-') or path.is_symlink():
                continue
            relative = path.relative_to(repo).as_posix()
            try:
                raw = _read(repo, relative, MAX_EVIDENCE)
                parsed = json.loads(raw)
            except (OSError, ValueError, TypeError):
                # An unreadable file cannot be attributed safely to this attempt.
                continue
            records = parsed.get('records') if isinstance(parsed, dict) else None
            belongs = (isinstance(records, list) and any(
                isinstance(row, dict) and isinstance(row.get('salvaged_from'), dict)
                and row['salvaged_from'].get('task') == task
                and row['salvaged_from'].get('attempt') == attempt for row in records))
            if not belongs:
                continue
            try:
                payload_sha = path.stem
                if not re.fullmatch(r'[0-9a-f]{64}', payload_sha):
                    raise ValueError('invalid provenance filename')
                _provenance(raw, payload_sha)
            except (OSError, ValueError, TypeError):
                evidence.append('unreadable provenance: ' + relative)
            else:
                # The controller record itself ties this task and attempt; do not
                # scan and hash every salvage artifact just to rediscover it.
                evidence.append(relative)
    return list(dict.fromkeys(evidence))


def _launch_base(execution, indexed):
    """Full recorded launch base, or None when no delegate record exists to name one."""
    if not indexed:
        return None
    recorded = indexed.get('base_sha')
    if not isinstance(recorded, str) or not re.fullmatch(r'[0-9a-f]{7,64}', recorded):
        raise ValueError('delegate record has no valid recorded launch base; '
                         'refusing to diff the kept worktree against its current HEAD')
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
               GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')
    found = subprocess.run(['git', '-C', str(execution), 'rev-parse', '--verify', recorded + '^{commit}'],
                           capture_output=True, env=env, timeout=15, check=False)
    full = found.stdout.decode().replace('\r', '').strip()
    if found.returncode or not full.startswith(recorded):
        raise ValueError('recorded launch base is unavailable or does not match')
    return full


def _without_clean_worktrees(repo, task, attempt, execution, evidence):
    """Drop registered kept worktrees that still capture to an empty patch at the recorded base."""
    remaining = []
    prefix = 'registered worktree: '
    for item in evidence:
        if item.startswith(prefix):
            try:
                rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
                indexed = next((row for row in reversed(rows) if row.get('kind') == 'delegate' and
                                row.get('task_id') == task and row.get('attempt_id') == attempt), {})
                kept, common, head = _managed(execution, item[len(prefix):])
                base = _launch_base(execution, indexed)
                # Without a recorded base an empty diff against the kept HEAD proves nothing.
                patch = _capture(kept, common, base, head)[0] if base else b'x'
                if not patch and _managed(execution, kept) == (kept, common, head):
                    continue
            except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
                pass
        remaining.append(item)
    return remaining


def _unindexed_clean_worktree(repo, task, attempt, execution, item):
    """True for a registered kept worktree with no delegate record that is clean at its own HEAD."""
    try:
        rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
        if any(row.get('kind') == 'delegate' and row.get('task_id') == task and
               row.get('attempt_id') == attempt for row in rows):
            return False
        kept, common, head = _managed(execution, item[len('registered worktree: '):])
        return not _capture(kept, common, head, head)[0]
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return False


def _raise_recovery_evidence(evidence, message):
    unreadable = [item.partition(': ')[2] for item in evidence
                  if item.startswith('unreadable provenance: ')]
    if unreadable:
        raise ValueError('unreadable salvage provenance file: %s' % ', '.join(unreadable))
    raise ValueError(message)


def record_empty_salvage(repo, task, attempt, reason):
    """Persist a controller finding that a stopped attempt has no patch to recover."""
    panel_results._owner()
    reason = '%s (delegate worktree root: %s)' % (reason, _worktree_root().resolve(strict=True))
    payload_sha = _empty_provenance_identity(task, attempt)
    relative = _empty_provenance_path(task, attempt)
    record = {'salvaged_from': {'task': task, 'attempt': attempt,
                                'outcome': 'nothing_to_recover', 'reason': reason},
              'encoding_warnings': []}

    def append(old):
        value = _provenance(old, payload_sha)
        if record not in value['records']:
            value['records'].append(record)
        return (json.dumps(value, ensure_ascii=True, sort_keys=True) + '\n').encode()

    target = panel_results.DURABLE['canonical_repo_path'](str(repo), relative, 'salvage provenance', True)
    panel_results.DURABLE['atomic_mutate'](target, append, str(repo), missing_ok=True,
                                         create_parent=True, max_input=MAX_EVIDENCE,
                                         max_output=MAX_EVIDENCE, label='salvage provenance')
    return relative


def _empty_result(repo, task, attempt, reason):
    record_empty_salvage(repo, task, attempt, reason)
    return {'outcome': 'nothing_to_recover', 'task': task, 'attempt': attempt,
            'reason': reason, 'next_action': '--continue %s may now resume the session without a patch' % task}


def salvage(repo, task=None, selected_repo=None, attempt_id=None, artifact=None):
    if attempt_id is not None and task is None:
        path, attempt, reason, execution = _standalone(repo, attempt_id, artifact)
        indexed = None
        task = attempt_id
        delegate_worktree = False
    else:
        path, attempt, reason, execution, indexed, delegate_worktree = _latest(repo, task, attempt_id)
    if selected_repo is not None:
        selected = work_repository(selected_repo)
        if selected not in {Path(repo).resolve(strict=True), execution}:
            raise ValueError('selected repository is neither the room repository nor the recorded execution repository')
    try:
        Path(path).lstat()
    except FileNotFoundError as error:
        if not delegate_worktree:
            raise ValueError('kept worktree is missing and is not a recorded OMS delegate worktree') from error
        if indexed is None or not any(key in indexed for key in ('patch', 'patch_sha256', 'patch_external')):
            evidence = _attempt_recovery_evidence(repo, task, attempt, indexed, execution=execution)
            if evidence:
                _raise_recovery_evidence(evidence, 'recoverable delegate evidence prevents empty salvage: %s' %
                                         ', '.join(evidence))
            return _empty_result(repo, task, attempt, 'no kept worktree or indexed patch')
        base = indexed.get('base_sha', '')
        if not isinstance(base, str) or not re.fullmatch(r'[0-9a-f]{7,64}', base):
            raise ValueError('indexed patch has no valid recorded base')
        patch = panel_results.verified_patch(repo, indexed)
        env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                   GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')
        check_base = subprocess.run(['git', '-C', str(execution), 'rev-parse', '--verify',
                                     base + '^{commit}'], capture_output=True, env=env,
                                    timeout=15, check=False)
        if check_base.returncode or not check_base.stdout.decode().replace('\r', '').strip().startswith(base):
            raise ValueError('indexed patch recorded base is unavailable or does not match')
        if not patch:
            evidence = _attempt_recovery_evidence(repo, task, attempt, indexed, execution=execution)
            if evidence:
                _raise_recovery_evidence(evidence, 'recoverable delegate evidence prevents empty salvage: %s' %
                                         ', '.join(evidence))
            return _empty_result(repo, task, attempt, 'indexed patch is empty')
        paths = panel_results._patch_paths(patch)
        if not paths:
            raise ValueError('indexed patch has no file changes')
        files = [{'path': name, 'before_bytes': None, 'after_bytes': None} for name in paths]
        warnings = patch_encoding_warnings(patch)
        recovered = True
    else:
        kept, common, head = _managed(execution, path)
        # Commits a stopped worker made inside its worktree belong to the patch.
        recorded_base = _launch_base(execution, indexed)
        base = recorded_base or head
        patch, files, warnings = _capture(kept, common, base, head)
        if _managed(execution, path) != (kept, common, head):
            raise ValueError('kept worktree identity moved during capture')
        recovered = False
        if not patch and not recorded_base:
            raise ValueError('kept worktree matches its current HEAD but no delegate artifact-index record names '
                             'its launch base, so commits made inside it cannot be ruled out; no empty outcome '
                             'was recorded. Inspect %s by hand' % kept)
        if not patch:
            evidence = _without_clean_worktrees(
                repo, task, attempt, execution,
                _attempt_recovery_evidence(repo, task, attempt, indexed, execution=execution))
            if evidence:
                _raise_recovery_evidence(evidence, 'recoverable delegate evidence prevents empty salvage: %s' %
                                         ', '.join(evidence))
            return _empty_result(repo, task, attempt, 'kept worktree is clean at its recorded base')
    if not patch:
        raise ValueError('kept worktree has no changes')
    metadata = {'schema': 1, 'kind': 'oms-salvaged-patch', 'base_sha': base,
                'salvaged_from': {'task': task, 'attempt': attempt, 'kept_path': str(path), 'stop_reason': reason,
                                 **({'recovered_from_artifact': True} if recovered else {})},
                'files': files, 'encoding_warnings': warnings,
                'payload_sha256': hashlib.sha256(_payload(patch)).hexdigest()}
    if recovered:
        metadata['recovered_artifact'] = indexed.get('patch')
    # Publish provenance first: an artifact must never escape with only an
    # editable warning list. Repeated payloads retain every controller warning.
    metadata['provenance'] = record_provenance(repo, metadata)
    header = PREFIX + json.dumps(metadata, ensure_ascii=True, sort_keys=True).encode() + b'\n'
    body = header + patch
    relative = '.oms/artifacts/delegate/salvage-%s-%s.patch' % (task, uuid.uuid4().hex[:16])
    target = panel_results.DURABLE['canonical_repo_path'](str(repo), relative, 'salvage patch', True)
    panel_results.DURABLE['atomic_mutate'](target, lambda old: body, str(repo), missing_ok=True,
                                         create_parent=True, expected_exists=False,
                                         max_output=len(body), label='salvage patch')
    metadata.update(patch=relative, patch_sha256=hashlib.sha256(body).hexdigest())
    sidecar = panel_results.DURABLE['canonical_repo_path'](str(repo), relative + '.json', 'salvage metadata', True)
    encoded = (json.dumps(metadata, ensure_ascii=True, sort_keys=True) + '\n').encode()
    panel_results.DURABLE['atomic_mutate'](sidecar, lambda old: encoded, str(repo), missing_ok=True,
                                         create_parent=True, expected_exists=False,
                                         max_output=len(encoded), label='salvage metadata')
    return dict(metadata, patch_path=target, metadata_path=sidecar, patch_bytes=len(body))


def review_candidate(state_repo, source_repo, patch, digest, base, destination):
    """Materialize indexed patch bytes on their recorded base, without source Git execution config."""
    if not isinstance(base, str) or not re.fullmatch(r'[0-9a-f]{7,64}', base):
        raise ValueError('candidate requires a recorded base commit')
    body = panel_results.verified_patch(state_repo, {'patch': patch, 'patch_sha256': digest})
    paths = panel_results._patch_paths(body)
    if not paths or any(not path or path.startswith(('/', '\\')) or '\\' in path or ':' in path or
                        any(part in {'', '.', '..', '.git', '.oms'} for part in path.split('/'))
                        for path in paths):
        raise ValueError('candidate patch contains unsafe or control paths')
    source_repo = work_repository(source_repo)
    common = _common(_git_directory(source_repo))
    destination = Path(destination).resolve(strict=True)
    if any(destination.iterdir()):
        raise ValueError('candidate directory is not empty')
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
               GIT_ATTR_NOSYSTEM='1', GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')
    command = ['git', '-c', 'core.hooksPath=' + os.devnull, '-c', 'core.fsmonitor=false',
               '-c', 'core.attributesFile=' + os.devnull, '-C', str(destination)]

    def git(*args, data=None):
        result = subprocess.run(command + list(args), input=data, capture_output=True, env=env,
                                timeout=60, check=False)
        if result.returncode:
            raise ValueError('candidate Git failed: ' + result.stderr.decode(errors='replace')[:300])
        return result.stdout.decode().replace('\r', '').strip()

    # The alternate supplies immutable objects only. Repack copies them into
    # the private repository before verification; Git never loads source config/index/hooks.
    config = _read(common, 'config', MAX_EVIDENCE).decode('utf-8')
    format_flags = (['--object-format=sha256'] if
                    re.search(r'^\s*objectformat\s*=\s*sha256\s*$', config, re.I | re.M) else [])
    git('init', '--quiet', '--template=', *format_flags)
    env['GIT_ALTERNATE_OBJECT_DIRECTORIES'] = str(common / 'objects')
    resolved = git('rev-parse', '--verify', base + '^{commit}')
    git('checkout', '--quiet', '--detach', resolved)
    git('repack', '-a', '-d')
    del env['GIT_ALTERNATE_OBJECT_DIRECTORIES']
    git('apply', '--binary', '--index', '--whitespace=nowarn', '-', data=body)
    tree = git('write-tree')
    return {'base': resolved, 'tree': tree, 'patch_sha256': digest, 'root': str(destination)}


def admission(patch, accept_salvaged=False, accept_encoding_loss=False, source=None, state_repo=None, repo=None):
    patch = Path(patch)
    if state_repo is None:
        body = _read(patch.parent, patch.name, 256 * 1024 * 1024)
    else:
        capture = runpy.run_path(str(Path(__file__).with_name('landing-capture.py')))
        item = capture['find'](str(state_repo), capture=str(patch))
        obj = capture['certificate'](item)['objects']['capture']
        body = capture['object_bytes'](item, obj)
        if capture['identity'](patch.lstat()) != obj['identity']:
            raise ValueError('salvage admission input was replaced')
    registry = state_repo if state_repo is not None else repo
    if registry is None:
        raise ValueError('salvage admission requires repository provenance state')
    payload_sha = hashlib.sha256(_payload(body)).hexdigest()
    raw = _read(Path(registry), _provenance_path(payload_sha), MAX_EVIDENCE, True)
    records = _provenance(raw, payload_sha)['records']
    if records:
        if not accept_salvaged:
            raise ValueError('salvaged patch requires --accept-salvaged after owner review')
        if any(row['encoding_warnings'] for row in records) and not accept_encoding_loss:
            raise ValueError('salvaged patch has encoding loss warnings; requires --accept-encoding-loss')


def cli_main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo')
    parser.add_argument('--salvage', metavar='TASK_ID')
    parser.add_argument('--attempt', metavar='ATTEMPT_ID',
                        help='select a stopped task attempt, or a standalone attempt with --artifact')
    parser.add_argument('--artifact', help='indexed .oms/artifacts/ path for --attempt')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(arguments)
    try:
        panel_results._owner()
        if not (args.attempt or args.salvage) or any(
                not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,159}', value)
                for value in (args.attempt, args.salvage) if value is not None):
            raise ValueError('invalid task or attempt ID')
        if args.salvage and args.artifact:
            raise ValueError('--artifact is standalone-only')
        if not args.salvage and (not args.attempt or not args.artifact):
            raise ValueError('--attempt requires --artifact for standalone recovery')
        selected = Path(args.repo or os.getcwd()).resolve(strict=True)
        inherited = os.environ.get('OMS_ROOM_REPO') if os.environ.get('OMS_ROOM_ID') else None
        repo = (state_repository(Path(inherited).resolve(strict=True)) if inherited else
                state_repository(selected))
        result = salvage(repo, args.salvage, selected if args.repo else None, args.attempt, args.artifact)
        if args.json:
            print(json.dumps(result, ensure_ascii=False, indent=2))
        elif result.get('outcome') == 'nothing_to_recover':
            print('nothing to recover for %s: %s; %s' %
                  (result['attempt'], result['reason'], result['next_action']))
        else:
            print('patch: ' + result['patch_path'])
            print('metadata: ' + result['metadata_path'])
            print('patch bytes: ' + str(result['patch_bytes']))
            if result['salvaged_from'].get('recovered_from_artifact'):
                print('recovered from indexed patch %s (worktree missing)' % result['recovered_artifact'])
            else:
                for file in result['files']:
                    print('%s: %s -> %s bytes' % (file['path'], file['before_bytes'], file['after_bytes']))
            for warning in result['encoding_warnings']:
                print('WARNING: %s: %s %s' % (warning['file'], warning['kind'], warning.get('hunk', '')))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print('oms panel salvage: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(cli_main())
