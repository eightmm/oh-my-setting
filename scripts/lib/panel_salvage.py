"""Capture stopped delegate bytes for owner review, without trusting checkout Git config."""

import argparse
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

import panel_results
from room_repository import LIBRARY, state_repository

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
    if (kept != physical or physical.name != 'wt' or physical.parent.parent != root or
            not physical.parent.name.startswith('oh-my-setting-delegate.')):
        raise ValueError('kept path is not an OMS-managed delegate worktree')
    marker = _read(physical.parent, '.oh-my-setting-tmp', 8192).decode()
    fields = {}
    for line in marker.splitlines():
        key, equals, value = line.partition('=')
        if not equals or key in fields:
            raise ValueError('invalid kept worktree marker')
        fields[key] = value
    if (fields.get('kind') != 'oh-my-setting-temp' or fields.get('temporary') != '1' or
            _native_path(fields.get('worktree', '')).resolve() != physical or
            _common(_git_directory(_native_path(fields.get('repo', '')).resolve(strict=True))) !=
            _common(_git_directory(repo))):
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


def _latest(repo, task):
    attempts = panel_results._query(repo, 'agent-events', 'list')
    rows = panel_results._query(repo, 'artifact-index', 'list', '1000000').get('rows', [])
    matching = [row for row in rows if row.get('kind') == 'delegate' and row.get('task_id') == task]
    record = matching[-1] if matching else {}
    candidates = [a for a in attempts if a.get('task_id') == task and
                  (a.get('tool') == 'peer-delegate' or a.get('refs', {}).get('panel_access') == 'write')]
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
        text = _read(repo, record['artifact']).decode('utf-8')
    matches = re.findall(r'^(?:error: )?worktree kept(?: at|:) (.+)$|^- worktree: (/.+)$', text, re.M)
    paths = [a or b for a, b in matches]
    location = attempt.get('refs', {}).get('panel_location', '')
    if location.startswith('worktree:oh-my-setting-delegate.'):
        root = _worktree_root()
        paths.append(str(root / location[len('worktree:'):]))
    if not paths:
        raise ValueError('latest delegate has no recorded kept worktree path')
    paths = [str(_native_path(value)) for value in paths]
    if len(set(paths)) != 1:
        raise ValueError('conflicting kept worktree evidence')
    reason = attempt.get('reason_code') or ('delegate exit ' + str(record.get('exit', 'unknown')))
    return paths[-1], attempt.get('attempt_id') or record.get('attempt_id'), reason


def _skeleton(line):
    return ''.join(c for c in unicodedata.normalize('NFKD', line) if ord(c) < 128)


def encoding_warnings(path, before, after, diff):
    old_count = sum(byte >= 128 for byte in before)
    new_count = sum(byte >= 128 for byte in after)
    warnings = []
    if old_count and new_count * 2 < old_count:
        warnings.append({'file': path, 'kind': 'non_ascii_drop',
                         'before_non_ascii_bytes': old_count, 'after_non_ascii_bytes': new_count})
    warnings.extend(_transliteration_warnings(path, diff))
    return warnings


def _transliteration_warnings(path, diff):
    warnings = []
    removed, added, hunk = [], [], None

    def inspect():
        skeletons = {}
        for line in removed:
            count = sum(ord(c) >= 128 for c in line)
            skeleton = _skeleton(line)
            skeletons[skeleton] = max(count, skeletons.get(skeleton, 0))
        for line in added:
            if sum(ord(c) >= 128 for c in line) < skeletons.get(_skeleton(line), 0):
                warnings.append({'file': path, 'kind': 'transliteration_hunk', 'hunk': hunk})
                break

    for line in diff.decode('utf-8', errors='replace').splitlines():
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
        old_bytes = sum(sum(byte >= 128 for byte in line.encode('utf-8')) for _, old, _ in hunks for line in old)
        new_bytes = sum(sum(byte >= 128 for byte in line.encode('utf-8')) for _, _, new in hunks for line in new)
        if old_bytes and new_bytes * 2 < old_bytes:
            for heading, old, _ in hunks:
                if any(any(ord(char) >= 128 for char in line) for line in old):
                    warnings.append({'file': path, 'kind': 'non_ascii_drop', 'hunk': heading,
                                     'before_non_ascii_bytes': old_bytes,
                                     'after_non_ascii_bytes': new_bytes})
        for heading, old, new in hunks:
            fragment = (heading + '\n' + ''.join('-' + line + '\n' for line in old) +
                        ''.join('+' + line + '\n' for line in new)).encode('utf-8')
            warnings.extend(_transliteration_warnings(path, fragment))

    def finish_hunk():
        if hunk is not None:
            hunks.append((hunk, removed, added))

    for line in text.splitlines():
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
        elif hunk and line.startswith('-') and not line.startswith('---'):
            removed.append(line[1:])
        elif hunk and line.startswith('+') and not line.startswith('+++'):
            added.append(line[1:])
        elif hunk and line.startswith('\\ No newline'):
            continue
    finish_hunk()
    inspect_file()
    return warnings


def _capture(kept, common, base):
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
        files, warnings = [], []
        for name in sorted(set(entries) | {os.fsdecode(p) for p in others if p}):
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
                after, mode = b'', None
            elif stat.S_ISLNK(info.st_mode):
                after, mode = os.fsencode(os.readlink(path)), '120000'
            elif stat.S_ISREG(info.st_mode):
                after = _read(kept, name, 256 * 1024 * 1024)
                mode = '100755' if info.st_mode & 0o111 else '100644'
                if os.name == 'nt':
                    mode = entries.get(name, ('100644', ''))[0]
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


def salvage(repo, task):
    path, attempt, reason = _latest(repo, task)
    kept, common, base = _managed(repo, path)
    patch, files, warnings = _capture(kept, common, base)
    if _managed(repo, path) != (kept, common, base):
        raise ValueError('kept worktree identity moved during capture')
    if not patch:
        raise ValueError('kept worktree has no changes')
    metadata = {'schema': 1, 'kind': 'oms-salvaged-patch', 'base_sha': base,
                'salvaged_from': {'task': task, 'attempt': attempt, 'kept_path': str(kept), 'stop_reason': reason},
                'files': files, 'encoding_warnings': warnings,
                'payload_sha256': hashlib.sha256(_payload(patch)).hexdigest()}
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
    parser.add_argument('--salvage', required=True, metavar='TASK_ID')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(arguments)
    try:
        panel_results._owner()
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,159}', args.salvage):
            raise ValueError('invalid task ID')
        repo = state_repository(Path(args.repo or os.getcwd()).resolve(strict=True))
        result = salvage(repo, args.salvage)
        if args.json:
            print(json.dumps(result, ensure_ascii=False, indent=2))
        else:
            print('patch: ' + result['patch_path'])
            print('metadata: ' + result['metadata_path'])
            print('patch bytes: ' + str(result['patch_bytes']))
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
