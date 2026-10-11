"""Owner result artifacts and delivery receipts, indexed by existing OMS state."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import time
import uuid

import codex_app_notify
from dashboard_projection import clean
from peer_artifacts import artifact_sections_stream, debate_sections
from work_journal import sanitize_multiline
from path_scope import classify
import room

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / 'scripts/oms'
DURABLE = runpy.run_path(str(ROOT / 'scripts/lib/durable-jsonl.py'))
ID = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]{0,159}\Z')
REVISION = re.compile(r'[0-9a-f]{24}\Z')
MAX_SUMMARY, MAX_RESULT = 8192, 32768


def _owner():
    if os.environ.get('OMS_HARNESS_CHILD') == '1' or os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') != '0':
        raise ValueError('a worker must return its result to the owner')


def _query(repo, verb, *args):
    command = ['bash', str(ENTRY), verb, '--repo', str(repo)]
    command += [*args, '--json'] if verb == 'agent-events' else ['--json', *args]
    result = subprocess.run(command, capture_output=True, text=True, timeout=30,
                            stdin=subprocess.DEVNULL, check=False)
    if result.returncode:
        raise ValueError('%s evidence unavailable (exit %s)' % (verb, result.returncode))
    return json.loads(result.stdout)


def _records(repo, full=False, _attempts=None):
    attempts = (_query(repo, 'agent-events', 'list', *([] if full else ['--limit', '300']))
                if _attempts is None else (_attempts if full else _attempts[-300:]))
    index = _query(repo, 'artifact-index', 'list', '1000').get('rows', [])
    if not isinstance(attempts, list) or not isinstance(index, list):
        raise ValueError('invalid result evidence')
    return attempts, index


def _read(repo, relative, maximum=MAX_RESULT, missing=False):
    try:
        return DURABLE['read_no_follow'](str(repo), relative, 'panel evidence',
                                          missing_ok=missing, max_bytes=maximum)
    except SystemExit as error:
        raise ValueError('panel evidence unavailable or unsafe') from error


def _relative(repo, value):
    if (not isinstance(value, str) or not value or len(value) > 300 or
            value.startswith(('/', '\\')) or '\\' in value or ':' in value or
            any(part in ('', '.', '..') for part in value.split('/'))):
        raise ValueError('evidence must be a safe repository-relative path')
    return {'path': value, 'sha256': hashlib.sha256(_read(repo, value, 1024 * 1024)).hexdigest()}


def _safe_summary(path, repo):
    path = Path(path).expanduser()
    if not path.is_absolute():
        path = repo / path
    root = path.parent.resolve(strict=True)
    text = _read(root, path.name, MAX_SUMMARY).decode('utf-8')
    if not text.strip() or '\0' in text:
        raise ValueError('summary is empty or contains NUL')
    screen = subprocess.run(['bash', '-c', '. "$1"; grep -Eiq "$(agent_memory_sensitive_re)"',
                             'panel-result', str(ROOT / 'scripts/lib/agent-memory-common.sh')],
                            input=text, capture_output=True, text=True, timeout=5, check=False)
    if screen.returncode != 1 or re.search(r'(?<![\w:])/(?:[^\s/]+/)*[^\s/]+|[A-Za-z]:\\', text):
        raise ValueError('summary contains sensitive-looking content or a private path')
    return sanitize_multiline(text, MAX_SUMMARY).strip()


def _indexed(repo, row, maximum=MAX_RESULT):
    path = row.get('artifact', '')
    if not isinstance(path, str) or not path.startswith('.oms/artifacts/'):
        raise ValueError('artifact is outside OMS evidence')
    data = _read(repo, path, maximum)
    if not row.get('artifact_sha256') or hashlib.sha256(data).hexdigest() != row['artifact_sha256']:
        raise ValueError('artifact digest mismatch')
    return data


def verified_patch(repo, row, maximum=4 * 1024 * 1024):
    """Bytes of a recorded patch, read bounded and without following links, only while its digest matches."""
    path = row.get('patch', '')
    if not isinstance(path, str) or not path.startswith('.oms/artifacts/'):
        raise ValueError('patch is outside OMS evidence')
    data = _read(repo, path, maximum)
    if hashlib.sha256(data).hexdigest() != row.get('patch_sha256'):
        raise ValueError('patch digest mismatch')
    return data


def _patch_name(value, prefix=True):
    if value.startswith('"'):
        # Git quotes UTF-8 bytes with C escapes, including octal escapes.
        escapes = {ord('a'): 7, ord('b'): 8, ord('t'): 9, ord('n'): 10,
                   ord('v'): 11, ord('f'): 12, ord('r'): 13,
                   ord('"'): 34, ord('\\'): 92}
        raw = value[1:-1].encode('utf-8', 'surrogateescape')
        def unescape(match):
            code = match.group(1)
            return bytes([int(code, 8) if code[:1] in b'01234567' else escapes[code[0]]])
        value = re.sub(rb'\\([0-7]{1,3}|[abtnvfr"\\])', unescape, raw).decode('utf-8', 'surrogateescape')
    if value == '/dev/null':
        return None
    return value[2:] if prefix and value[:2] in ('a/', 'b/') else value


def _patch_paths(patch_bytes_or_path):
    """All touched paths, including both rename sides, without display clipping."""
    data = (patch_bytes_or_path if isinstance(patch_bytes_or_path, bytes)
            else Path(patch_bytes_or_path).read_bytes())
    paths, fallback, names = {}, [], []
    remaining = 0
    lines = data.decode('utf-8', 'surrogateescape').split('\n')
    if lines[-1] == '':
        lines.pop()
    for line in lines:
        if line.startswith('diff --git '):
            paths.update(dict.fromkeys(names or fallback))
            names, remaining = [], 0
            rest = line[11:]
            match = re.fullmatch(r'("(?:[^"\\]|\\.)*"|a/.*?) ("(?:[^"\\]|\\.)*"|b/.*)', rest)
            if not match:
                raise ValueError('patch paths unavailable: invalid git header')
            fallback = [_patch_name(value) for value in match.groups()]
            # Unquoted names may contain spaces and even the delimiter " b/".
            if rest.startswith('a/') and len(rest) % 2 == 1:
                middle = len(rest) // 2
                if rest[middle:middle + 3] == ' b/' and rest[2:middle] == rest[middle + 3:]:
                    fallback = [rest[2:middle]]
        elif line.startswith('@@ '):
            match = re.match(r'@@ -[0-9]+(?:,([0-9]+))? \+[0-9]+(?:,([0-9]+))? @@', line)
            if not match:
                raise ValueError('patch paths unavailable: invalid hunk header')
            remaining = sum(int(value) if value is not None else 1 for value in match.groups())
        elif remaining:
            remaining -= 2 if line.startswith(' ') else 1 if line.startswith(('+', '-')) else 0
        elif line.startswith(('--- ', '+++ ')):
            name = _patch_name(line[4:].split('\t', 1)[0])
            if name is not None:
                names.append(name)
        elif line.startswith(('rename from ', 'rename to ', 'copy from ', 'copy to ')):
            names.append(_patch_name(line.split(' ', 2)[2], prefix=False))
    paths.update(dict.fromkeys(names or fallback))
    return list(paths)


def _scope_drift(paths, owns):
    return [path for path in paths if classify(path, owns, []) == 'outside'] if owns else []


def _worker_owns(repo, worker):
    refs = worker.get('refs', {})
    ident, who = refs.get('panel_room_id'), refs.get('panel_room_participant')
    if not ident or not who:
        return []
    state = room.status(repo, ident)
    member = next((p for p in state['participants'] if p['participant'] == who), None)
    if member is None:
        raise ValueError('write worker declared scope evidence unavailable')
    return member.get('owns') or []


def _diffstat(repo, row):
    """Changed files of a recorded patch, read only while its digest still matches."""
    data = verified_patch(repo, row)
    paths = _patch_paths(data)
    files = {path: [clean(path, 160), 0, 0] for path in paths}
    current, header, remaining = None, True, 0
    lines = data.decode('utf-8', 'surrogateescape').split('\n')
    if lines[-1] == '':
        lines.pop()
    for line in lines:
        if line.startswith('diff --git '):
            current = _patch_paths(line.encode('utf-8', 'surrogateescape'))[-1]
            header = True
        elif header and line.startswith('+++ '):
            name = _patch_name(line[4:].split('\t', 1)[0])
            if name is not None:
                current = name
        elif header and line.startswith('--- '):
            current = _patch_name(line[4:].split('\t', 1)[0])
        elif line.startswith('@@ '):
            header = False
            match = re.match(r'@@ -[0-9]+(?:,([0-9]+))? \+[0-9]+(?:,([0-9]+))? @@', line)
            if not match:
                raise ValueError('patch paths unavailable: invalid hunk header')
            remaining = sum(int(value) if value is not None else 1 for value in match.groups())
        elif not header:
            if current in files and line.startswith('+'):
                files[current][1] += 1
            elif current in files and line.startswith('-'):
                files[current][2] += 1
            remaining -= 2 if line.startswith(' ') else 1 if line.startswith(('+', '-')) else 0
            header = remaining == 0
    return {'files': list(files.values())[:20], 'count': len(files),
            'added': sum(f[1] for f in files.values()),
            'removed': sum(f[2] for f in files.values())}


def _admission(repo, rows, task_id, worker):
    patches = [r for r in rows if r.get('kind') == 'delegate' and r.get('task_id') == task_id
               and r.get('attempt_id') == worker.get('attempt_id') and r.get('patch_sha256')]
    if not patches or patches[-1].get('exit') != 0:
        return None
    patch = patches[-1]
    matches = [r for r in rows if r.get('kind') == 'patch-land'
               and (not r.get('task_id') or r.get('task_id') == task_id)
               and r.get('patch_sha256') == patch['patch_sha256']
               and (not patch.get('plan_id') or r.get('plan_id') == patch['plan_id']) and r.get('exit') == 0]
    if not matches:
        return None
    # Landing evidence and the delegated patch must still name the same bytes.
    try:
        if _relative(repo, patch['patch'])['sha256'] != patch['patch_sha256']:
            return None
    except (KeyError, ValueError):
        return None
    return matches[-1]


@contextlib.contextmanager
def _lock(repo, name='panel-results', timeout=10):
    if not isinstance(name, str) or not ID.fullmatch(name):
        raise ValueError('invalid panel lock name')
    try:
        path = DURABLE['canonical_repo_path'](str(repo), '.oms/artifacts/' + name + '.lock',
                                                'panel lock', True)
        ancestry = DURABLE['component_snapshot'](str(repo), path, 'panel lock')
        fd = os.open(path, os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    except (OSError, SystemExit) as error:
        raise ValueError('panel lock unavailable') from error
    with os.fdopen(fd, 'r+b') as handle:
        if not DURABLE['safe_regular'](os.fstat(handle.fileno())):
            raise ValueError('unsafe panel lock')
        if os.name == 'nt':
            import msvcrt
            if os.fstat(handle.fileno()).st_size == 0:
                try:
                    handle.write(b'\0'); handle.flush()
                except OSError as error:
                    if os.fstat(handle.fileno()).st_size == 0:
                        raise ValueError('panel lock initialization unavailable') from error
            handle.seek(0)
            acquire = lambda: msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            release = lambda: msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            acquire = lambda: fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            release = lambda: fcntl.flock(handle, fcntl.LOCK_UN)
        deadline = time.monotonic() + timeout
        while True:
            try:
                acquire(); break
            except OSError:
                if time.monotonic() >= deadline:
                    raise ValueError('panel operation is busy; retry later')
                time.sleep(0.05)
        try:
            DURABLE['components_unchanged'](ancestry, 'panel lock')
            named = os.lstat(path)
            opened = os.fstat(handle.fileno())
            if not DURABLE['safe_regular'](named) or not DURABLE['same_file'](opened, named):
                raise ValueError('panel lock identity changed')
            yield
        finally:
            release()


def _write(repo, relative, payload):
    body = (json.dumps(payload, ensure_ascii=False, sort_keys=True, allow_nan=False) + '\n').encode()
    def immutable(old):
        if old and old != body:
            raise ValueError('result artifact already contains different bytes')
        return body
    try:
        path = DURABLE['canonical_repo_path'](str(repo), relative, 'panel result', True)
        DURABLE['atomic_mutate'](path, immutable, str(repo), missing_ok=True,
                                 create_parent=True, max_output=MAX_RESULT, label='panel result')
        return path
    except SystemExit as error:
        raise ValueError('panel artifact write refused') from error


def _register(repo, owner, task_id, path, kind='panel-result'):
    env = dict(os.environ, OMS_TASK_ID=task_id)
    for name in ('OMS_INDEX_PLAN_ID', 'OMS_INDEX_ACTIVE_TASK_ID', 'OMS_INDEX_PARENT_EVENT_ID',
                 'OMS_INDEX_ATTEMPT_ID', 'OMS_ATTEMPT_ID', 'OMS_LAST_ATTEMPT_ID',
                 'OMS_OPERATION_ID', 'OMS_HARNESS_CALL_ID'):
        env.pop(name, None)
    result = subprocess.run(['bash', '-c', '. "$1"; ma_append_artifact_index "$2" "$3" "$4" 0 "$5"',
                             'panel-result', str(ROOT / 'scripts/lib/peer-common.sh'), str(repo),
                             kind, owner, str(path)], env=env, capture_output=True,
                            text=True, timeout=30, check=False)
    if result.returncode:
        raise ValueError('result artifact index registration failed')


def _load_result(repo, row):
    result = json.loads(_indexed(repo, row))
    if (not isinstance(result, dict) or result.get('schema') != 1 or result.get('kind') != 'oms-panel-result'
            or result.get('task_id') != row.get('task_id') or not REVISION.fullmatch(str(result.get('revision', '')))
            or result.get('owner') not in ('codex', 'claude') or result.get('outcome') not in ('completed', 'accepted', 'failed')
            or not isinstance(result.get('summary'), str) or not isinstance(result.get('evidence'), list)):
        raise ValueError('invalid result artifact')
    return result


def _plan_bindings(repo, task_ids):
    from dashboard_projection import read_plan

    plan, tasks = read_plan(repo)
    if not isinstance(plan.get('plan_id'), str) or not ID.fullmatch(plan['plan_id']):
        return {}
    return {task['id']: {'plan_id': plan['plan_id'],
                         'task_sha256': hashlib.sha256(json.dumps(task, sort_keys=True,
                             allow_nan=False).encode()).hexdigest()}
            for task in tasks if task['id'] in task_ids}


def completed_tasks(repo, task_ids, _shared=False, _tick=None):
    """Locally verified products of the current plan generation, without granting acceptance."""
    wanted = {ident for ident in task_ids if isinstance(ident, str) and ID.fullmatch(ident)}
    bindings = _plan_bindings(repo, wanted)
    if not bindings:
        return {}
    if _tick is not None:
        _, rows = _tick.records()
    elif _shared:
        from panel_cache import read_shared
        _, rows = read_shared(repo, 'result-records', lambda: _records(repo))
    else:
        _, rows = _records(repo)
    latest = {row['task_id']: row for row in rows
              if row.get('kind') == 'panel-result' and row.get('task_id') in bindings}
    found = {}
    for ident, row in latest.items():
        try:
            result = _load_result(repo, row)
            verified = result.get('verification') or {}
            if not isinstance(verified, dict):
                continue
            if (result.get('plan_binding') == bindings[ident]
                    and result.get('outcome') in {'completed', 'accepted'}
                    and verified.get('status') == 'passed' and type(verified.get('exit')) is int
                    and verified['exit'] == 0
                    and all(_relative(repo, ref['path']) == ref for ref in result['evidence'])):
                found[ident] = result['outcome']
        except (OSError, ValueError, KeyError, TypeError):
            continue
    return found


def _last_delivery(repo, rows, task_id, revision):
    for row in reversed(rows):
        prefix = '.oms/artifacts/panel-results/delivery-%s-%s-' % (task_id, revision)
        if (row.get('task_id') == task_id and row.get('kind') in ('panel-delivery', 'panel-delivery-pending')
                and isinstance(row.get('artifact'), str) and row['artifact'].startswith(prefix)):
            value = json.loads(_indexed(repo, row, 4096))
            if not isinstance(value, dict) or not isinstance(value.get('receipt'), dict):
                raise ValueError('invalid delivery receipt')
            if value.get('revision') == revision:
                return value
    return None


def _deliver(repo, payload, relative, rows, retry=False):
    prior = _last_delivery(repo, rows, payload['task_id'], payload['revision'])
    receipt = (prior or {}).get('receipt', {})
    if receipt.get('persisted') or (prior and not retry):
        return prior
    attempt = uuid.uuid4().hex[:16]
    pending = {'schema': 1, 'kind': 'oms-panel-delivery', 'revision': payload['revision'],
               'attempt': attempt, 'receipt': {'status': 'pending', 'delivery_unknown': True}}
    prefix = '.oms/artifacts/panel-results/delivery-%s-%s-%s' % (payload['task_id'], payload['revision'], attempt)
    path = _write(repo, prefix + '-pending.json', pending)
    _register(repo, payload['owner'], payload['task_id'], path, 'panel-delivery-pending')
    message = 'OMS result %s (%s)\n%s\nEvidence: %s' % (
        payload['task_id'], payload['outcome'], payload['summary'], relative)
    delivered = dict(pending, receipt=codex_app_notify.deliver(repo, message))
    path = _write(repo, prefix + '.json', delivered)
    _register(repo, payload['owner'], payload['task_id'], path, 'panel-delivery')
    return delivered


def _plan_hint(repo, task_id):
    # No land receipt exists here, so the plan is never finished automatically.
    try:
        shown = subprocess.run(['bash', str(ENTRY), 'agent-plan', '--repo', str(repo), 'show', '--id', task_id],
                               capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL, timeout=60)
        if not shown.returncode and json.loads(shown.stdout).get('state') not in ('done', None):
            print('plan %s is not done: after oms land run oms agent-plan finish --id %s --landed-commit SHA'
                  % (task_id, task_id), file=sys.stderr)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass


def finalize(repo, owner, task_id, summary_file, outcome, verify=None, evidence=None, notify=False):
    _owner()
    if owner not in ('codex', 'claude') or not ID.fullmatch(task_id) or outcome not in ('completed', 'accepted', 'failed'):
        raise ValueError('invalid owner result')
    summary = _safe_summary(summary_file, repo)
    refs = [_relative(repo, value) for value in (evidence or [])] if not isinstance(evidence, str) else [_relative(repo, evidence)]
    if len(refs) > 8:
        raise ValueError('use at most eight evidence references')
    with _lock(repo):
        binding = _plan_bindings(repo, {task_id}).get(task_id)
        attempts, rows = _records(repo, full=True)
        main_id = os.environ.get('OMS_PANEL_MAIN_ATTEMPT')
        if main_id:
            main = next((a for a in attempts if a.get('attempt_id') == main_id), {})
            if main.get('tool') != 'panel-main' or main.get('provider') != owner:
                raise ValueError('finalizing main identity is unavailable or does not match the owner')
        related = [a for a in attempts if a.get('task_id') == task_id
                   or (main_id and a.get('parent_attempt_id') == main_id)]
        workers = sorted([a for a in related if a.get('refs', {}).get('panel_access') == 'write'
                          and a.get('refs', {}).get('panel_role') == 'worker'], key=lambda a: a.get('created_at', ''))
        admissions = [_admission(repo, rows, a.get('task_id'), a) for a in workers]
        if outcome == 'accepted':
            for worker in workers:
                owns = _worker_owns(repo, worker)
                if not owns:
                    continue
                patches = [r for r in rows if r.get('kind') == 'delegate'
                           and r.get('attempt_id') == worker.get('attempt_id') and r.get('patch_sha256')]
                for row in patches:
                    drift = _scope_drift(_patch_paths(verified_patch(repo, row)), owns)
                    if drift:
                        raise ValueError('write worker patch has %s path(s) outside its declared scope: %s'
                                         % (len(drift), ', '.join(drift[:8])))
        if outcome == 'accepted' and any(not item for item in admissions):
            raise ValueError('write worker needs matching landed patch evidence')
        if outcome == 'accepted' and (not verify or not refs):
            raise ValueError('accepted requires an evidence reference and a passing verifier')
        verification = {'status': 'not_run'}
        if verify:
            # Output remains local; a verifier transcript is never exported as a summary.
            checked = subprocess.run(['bash', '-c', verify], cwd=str(repo),
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, timeout=300, check=False)
            verification = {'status': 'passed' if checked.returncode == 0 else 'failed',
                            'exit': checked.returncode, 'command_sha256': hashlib.sha256(verify.encode()).hexdigest()}
            if outcome == 'accepted' and checked.returncode:
                raise ValueError('accepted verifier failed (exit %s)' % checked.returncode)
            if any(_relative(repo, ref['path']) != ref for ref in refs):
                raise ValueError('evidence changed during verification')
        if binding != _plan_bindings(repo, {task_id}).get(task_id):
            raise ValueError('plan task changed during result verification')
        payload = {'schema': 1, 'kind': 'oms-panel-result', 'task_id': task_id,
                   'owner': owner, 'outcome': outcome, 'summary': summary,
                   'summary_sha256': hashlib.sha256(summary.encode()).hexdigest(),
                   'verification': verification, 'evidence': refs,
                   'main_attempt_id': main_id,
                   'admission_event_ids': [a.get('event_id') for a in admissions if a],
                   'attempt_ids': [a['attempt_id'] for a in related[-20:]]}
        if binding:
            payload['plan_binding'] = binding
        payload['revision'] = hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()[:24]
        relative = '.oms/artifacts/panel-results/result-%s-%s.json' % (task_id, payload['revision'])
        path = _write(repo, relative, payload)
        if not any(r.get('kind') == 'panel-result' and r.get('artifact') == relative for r in rows):
            _register(repo, owner, task_id, path)
        delivery = _deliver(repo, payload, relative, rows) if notify else _last_delivery(repo, rows, task_id, payload['revision'])
        if outcome == 'accepted':
            _plan_hint(repo, task_id)
        return dict(payload, artifact=relative, delivery=delivery)


def retry_delivery(repo, task_id, revision=None):
    _owner()
    if not ID.fullmatch(task_id) or (revision and not REVISION.fullmatch(revision)):
        raise ValueError('invalid saved result ID')
    with _lock(repo):
        _, rows = _records(repo)
        matching = [r for r in rows if r.get('kind') == 'panel-result' and r.get('task_id') == task_id]
        for row in reversed(matching):
            payload = _load_result(repo, row)
            if not revision or payload['revision'] == revision:
                return _deliver(repo, payload, row['artifact'], rows, retry=True)
    raise ValueError('saved result not found in retained evidence')


def outcomes(repo, task_ids, _shared=False, _tick=None):
    """The owner's recorded outcome for each of these tasks, beyond the twenty-task window results() pages."""
    wanted = {i for i in task_ids if isinstance(i, str) and ID.fullmatch(i)}
    if not wanted:
        return {}
    if _tick is not None:
        _, rows = _tick.records()
    elif _shared:
        from panel_cache import read_shared
        _, rows = read_shared(repo, 'result-records', lambda: _records(repo))
    else:
        _, rows = _records(repo)
    found = {}
    for ident in wanted:
        result_rows = [r for r in rows if r.get('task_id') == ident and r.get('kind') == 'panel-result']
        if result_rows:
            try:
                found[ident] = _load_result(repo, result_rows[-1]).get('outcome')
            except (OSError, ValueError, KeyError, TypeError):
                pass
    return found


def pending_admissions(repo, task_ids, _tick=None):
    """Recorded patches without admission; capped evidence stays conservatively pending.

    This is an inbox hint, not verified patch or acceptance evidence. No artifact
    bytes are read. At the latest-300-attempt/1000-event boundary, a task with
    no retained record rows stays pending because its evidence may be outside
    coverage. Retained non-patch rows alone do not imply a pending patch.
    """
    wanted = {ident for ident in task_ids if isinstance(ident, str) and ID.fullmatch(ident)}
    if not wanted:
        return set()
    if _tick is not None:
        attempts, rows = _tick.records()
    else:
        from panel_cache import read_shared
        attempts, rows = read_shared(repo, 'result-records', lambda: _records(repo))
    patches, admitted, seen = set(), set(), set()
    for row in rows:
        ident, kind = row.get('task_id'), row.get('kind')
        if not isinstance(ident, str) or ident not in wanted:
            continue
        seen.add(ident)
        if kind in ('patch-admit', 'patch-land'):
            admitted.add(ident)
        elif kind in ('ask', 'call', 'consult', 'delegate', 'review', 'ask-synthesis') and row.get('patch'):
            patches.add(ident)
    if len(attempts) >= 300 or len(rows) >= 1000:
        patches.update(wanted - seen)
    return patches - admitted


def results(repo, task_id=None, room_participant=None, _shared=False, _tick=None):
    if task_id and not ID.fullmatch(task_id):
        raise ValueError('invalid task ID')
    if _tick is not None:
        attempts, rows = _tick.records()
    elif _shared:
        from panel_cache import read_shared
        attempts, rows = read_shared(repo, 'result-records', lambda: _records(repo))
    else:
        attempts, rows = _records(repo)
    latest = {}
    for row in sorted(attempts + rows, key=lambda r: r.get('updated_at') or r.get('ts') or ''):
        ident = row.get('task_id')
        if isinstance(ident, str) and ID.fullmatch(ident):
            latest.pop(ident, None)
            latest[ident] = None
    ids = list(latest)
    scoped = bool(room_participant and not task_id)
    if scoped:
        # Select within this participant's retained tasks before the 20-task cap;
        # a task with no attempt of its own may only adopt a main's children.
        mine = [a for a in attempts if a.get('refs', {}).get('panel_room_participant') == room_participant]
        own = {a.get('task_id') for a in mine}
        adopting = any(a.get('parent_attempt_id') for a in mine)
        owned = {a.get('task_id') for a in attempts}
        has_result = {r.get('task_id') for r in rows if r.get('kind') == 'panel-result'}
        ids = [i for i in ids if i in own or (adopting and i not in owned and i in has_result)]
    chosen = [task_id] if task_id in ids else [] if task_id else list(ids)
    output = []
    capped = False
    for ident in reversed(chosen):
        metadata_attempts = [a for a in attempts if a.get('task_id') == ident]
        matching = metadata_attempts
        indexed = [r for r in rows if r.get('task_id') == ident]
        result_rows = [r for r in indexed if r.get('kind') == 'panel-result']
        result = delivery = None
        issue = None
        if result_rows:
            try:
                result = _load_result(repo, result_rows[-1])
                delivery = _last_delivery(repo, rows, ident, result['revision'])
            except (OSError, ValueError, KeyError, TypeError):
                issue = 'result or delivery evidence unavailable'
        if result and result.get('main_attempt_id'):
            # Only a whole-main result (no attempt of its own) adopts the main's
            # children; otherwise every task of one main would share its calls.
            matching = ([a for a in attempts if a.get('task_id') == ident] or
                        [a for a in attempts if a.get('parent_attempt_id') == result['main_attempt_id']])
            linked = {a.get('attempt_id') for a in matching}
            indexed = [r for r in rows if r.get('task_id') == ident or r.get('attempt_id') in linked]
        if room_participant:
            matching = [a for a in matching if a.get("refs", {}).get("panel_room_participant") == room_participant]
            metadata_attempts = [a for a in metadata_attempts
                                 if a.get("refs", {}).get("panel_room_participant") == room_participant]
            linked = {a.get("attempt_id") for a in matching}
            indexed = [r for r in indexed if r.get("attempt_id") in linked]
        if scoped and not matching:
            continue
        if len(output) >= 20:
            capped = True
            break
        last = metadata_attempts[-1] if metadata_attempts else {}
        metadata = last.get('refs', {})
        calls = []
        for row in indexed[-30:]:
            if row.get('kind') not in ('ask', 'call', 'consult', 'delegate', 'review', 'ask-synthesis', 'patch-admit', 'patch-land'):
                continue
            call = {k: row[k] for k in ('kind', 'exit', 'verify_exit', 'artifact', 'patch', 'patch_sha256', 'event_id',
                                       'selected_model', 'requested_model', 'served_model', 'attempt_id') if k in row}
            if (task_id or room_participant) and row.get('artifact') and row.get('kind') in ('ask', 'call', 'consult', 'delegate', 'review'):
                try:
                    answer, _ = artifact_sections_stream(io.BytesIO(_indexed(repo, row, 1024 * 1024)))
                    answer = re.split(r'(?m)^(?:model-result:|tokens used$|usage detail:|served model$|cost usd$|## Verify\s*$)', answer)[0]
                    answer = re.sub(r'(?m)^model-route:.*\n?', '', answer)
                    # Only a deliberation (Answer/Findings sections) drops the prompt echo before it;
                    # an ordinary report keeps its body ahead of a Verification heading.
                    trimmed, sections = debate_sections(answer)
                    if any(heading in ('Answer', 'Findings') for heading, _ in sections):
                        answer = trimmed
                    # Redact the whole bounded artifact before deciding whether the retained answer lost bytes.
                    maximum = max(MAX_RESULT + 1, 2 * len(answer.encode('utf-8')) + MAX_RESULT)
                    safe = codex_app_notify._safe_message(answer, repo, maximum=maximum).strip()
                    complete = sanitize_multiline(safe, maximum, maximum).strip()
                    call['answer'] = sanitize_multiline(complete, MAX_RESULT, MAX_RESULT).strip()
                    call['answer_truncated'] = call['answer'] != complete
                except (OSError, ValueError, TypeError):
                    call['answer'] = 'Answer evidence unavailable'
            if row.get('patch') and row.get('patch_sha256'):
                try:
                    call['changes'] = _diffstat(repo, row)
                    worker = next((a for a in matching if a.get('attempt_id') == row.get('attempt_id')), {})
                    if worker.get('refs', {}).get('panel_access') == 'write':
                        drift = _scope_drift(_patch_paths(verified_patch(repo, row)), _worker_owns(repo, worker))
                        call['changes']['out_of_scope'] = {'count': len(drift), 'paths': drift[:8]}
                except (OSError, ValueError, TypeError):
                    call['changes'] = {'issue': 'patch evidence unavailable'}
            calls.append(call)
        output.append({'task_id': ident, 'title': clean(metadata.get('panel_label') or
                       ((result or {}).get('summary') or '').split('\n')[0], 160),
                       'model': clean(metadata.get('panel_model'), 160), 'role': metadata.get('panel_role'),
                       'location': metadata.get('panel_location'), 'process_state': last.get('state'),
                       'outcome': result.get('outcome') if result else None, 'verification': (result or {}).get('verification'),
                       'acceptance': bool(result and result.get('outcome') == 'accepted'),
                       'summary': result.get('summary') if result else None,
                       'revision': result.get('revision') if result else None,
                       'artifact': result_rows[-1].get('artifact') if result_rows else None,
                       'evidence': result.get('evidence', []) if result else [], 'calls': calls[-8:],
                       'delivery': (delivery or {}).get('receipt', {}).get('status', 'not_requested'),
                       'delivery_unknown': (delivery or {}).get('receipt', {}).get('delivery_unknown', False),
                       'issue': issue, 'missing_result': result is None})
    return {'schema': 1, 'kind': 'oms-panel-results', 'rows': output,
            'coverage': 'latest 300 attempts and 1000 artifact events',
            'truncated': capped or (bool(task_id) and not chosen)}
