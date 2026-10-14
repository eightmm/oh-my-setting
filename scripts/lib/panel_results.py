"""Owner result artifacts and delivery receipts, indexed by existing OMS state."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import signal
import subprocess
import sys
import time
import uuid

import codex_app_notify
from dashboard_projection import clean
from call_ending import call_ending
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
MAX_DRAFT = 4096


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
    # The room the worker joined holds its declaration, even after that room rolled over.
    state = room.status(repo, ident, exact=True)
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


def write_summary_draft(repo, row):
    """Write a bounded owner-editable summary beside existing panel result artifacts."""
    if not isinstance(row, dict) or not isinstance(row.get('task_id'), str) or not ID.fullmatch(row['task_id']):
        raise ValueError('invalid result row for summary draft')
    stages = row.get('stages') if isinstance(row.get('stages'), dict) else {}
    lines = ['# Panel result draft', '', '- Task: ' + clean(row['task_id'], 160),
             '- Next action: ' + clean(row.get('next_action') or 'unknown', 160), '', '## Stages']
    for name in ('implement', 'verify', 'review', 'main_decision'):
        stage = stages.get(name) if isinstance(stages.get(name), dict) else {}
        lines.append('- %s: %s' % (name, clean(stage.get('state') or 'unknown', 80)))
    refs = []
    for call in row.get('calls', []):
        if not isinstance(call, dict):
            continue
        for key in ('artifact', 'patch'):
            value = call.get(key)
            if value and value not in refs:
                refs.append(value)
    lines.extend(['', '## Artifacts'])
    for value in refs:
        try:
            _relative(repo, value)
        except (OSError, ValueError, TypeError):
            continue
        lines.append('- ' + value)
    body = (sanitize_multiline('\n'.join(lines), MAX_DRAFT - 1, 512).rstrip() + '\n').encode('utf-8')
    relative = '.oms/artifacts/panel-results/summary-%s-%s.md' % (row['task_id'], uuid.uuid4().hex[:12])
    path = DURABLE['canonical_repo_path'](str(repo), relative, 'panel summary draft', True)
    def immutable(old):
        if old:
            raise ValueError('summary draft already exists')
        return body
    try:
        DURABLE['atomic_mutate'](path, immutable, str(repo), missing_ok=True,
            create_parent=True, expected_exists=False, max_output=MAX_DRAFT, label='panel summary draft')
    except SystemExit as error:
        raise ValueError('panel summary draft write refused') from error
    return relative


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
            or result.get('owner') not in ('codex', 'claude') or result.get('outcome') not in ('completed', 'accepted', 'failed', 'rejected')
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


def _run_verifier(repo, verify, timeout):
    creationflags = getattr(subprocess, 'CREATE_NEW_PROCESS_GROUP', 0) if os.name == 'nt' else 0
    process = subprocess.Popen(['bash', '-c', verify], cwd=str(repo),
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, start_new_session=os.name == 'posix',
                               creationflags=creationflags)
    try:
        returncode = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        if os.name == 'posix':
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            try:
                subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=5, check=False)
            except (OSError, subprocess.SubprocessError):
                pass
            if process.poll() is None:
                process.kill()
        process.wait()
        raise ValueError('verifier timed out after %s s' % timeout) from error
    return returncode


def _finalize_snapshot(repo, owner, task_id, main_id, evidence):
    binding = _plan_bindings(repo, {task_id}).get(task_id)
    attempts, rows = _records(repo, full=True)
    if main_id:
        main = next((a for a in attempts if a.get('attempt_id') == main_id), {})
        if main.get('tool') != 'panel-main' or main.get('provider') != owner:
            raise ValueError('finalizing main identity is unavailable or does not match the owner')
    related = [a for a in attempts if a.get('task_id') == task_id
               or (main_id and a.get('parent_attempt_id') == main_id)]
    workers = sorted([a for a in related if a.get('refs', {}).get('panel_access') == 'write'
                      and a.get('refs', {}).get('panel_role') == 'worker'], key=lambda a: a.get('created_at', ''))
    admissions = [_admission(repo, rows, a.get('task_id'), a) for a in workers]
    # Only this task's records are compared across verification; the main's other calls keep moving meanwhile.
    task_attempts = [a for a in related if a.get('task_id') == task_id]
    attempt_ids = {a.get('attempt_id') for a in task_attempts}
    related_rows = [r for r in rows if r.get('task_id') == task_id or r.get('attempt_id') in attempt_ids]
    finalized = [{'row': r, 'record': _load_result(repo, r)} for r in rows
                 if r.get('kind') == 'panel-result' and r.get('task_id') == task_id]
    return {'binding': binding, 'main_attempt_id': main_id, 'related_attempts': task_attempts,
            'related_rows': related_rows, 'admissions': admissions, 'evidence': evidence,
            'finalized': finalized}, attempts, rows, related, workers, admissions


def finalize(repo, owner, task_id, summary_file, outcome, verify=None, evidence=None, notify=False,
             verify_timeout=1800):
    _owner()
    if owner not in ('codex', 'claude') or not ID.fullmatch(task_id) or outcome not in ('completed', 'accepted', 'failed', 'rejected'):
        raise ValueError('invalid owner result')
    if isinstance(verify_timeout, bool) or not isinstance(verify_timeout, int) or not 1 <= verify_timeout <= 86400:
        raise ValueError('verify timeout must be between 1 and 86400 seconds')
    summary = _safe_summary(summary_file, repo)
    refs = [_relative(repo, value) for value in (evidence or [])] if not isinstance(evidence, str) else [_relative(repo, evidence)]
    if len(refs) > 8:
        raise ValueError('use at most eight evidence references')
    main_id = os.environ.get('OMS_PANEL_MAIN_ATTEMPT')
    with _lock(repo):
        evidence_refs = [dict(ref) for ref in refs]
        snapshot, attempts, rows, related, workers, admissions = _finalize_snapshot(
            repo, owner, task_id, main_id, evidence_refs)
        binding = snapshot['binding']
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
        verify_exit = _run_verifier(repo, verify, verify_timeout)
        verification = {'status': 'passed' if verify_exit == 0 else 'failed',
                        'exit': verify_exit, 'command_sha256': hashlib.sha256(verify.encode()).hexdigest()}
        if outcome == 'accepted' and verify_exit:
            raise ValueError('accepted verifier failed (exit %s)' % verify_exit)
    with _lock(repo):
        current_evidence = [_relative(repo, ref['path']) for ref in refs]
        current, attempts, rows, related, workers, admissions = _finalize_snapshot(
            repo, owner, task_id, main_id, current_evidence)
        for field, message in (
                ('binding', 'plan task changed during result verification'),
                ('evidence', 'evidence changed during verification'),
                ('admissions', 'admission evidence changed during verification'),
                ('related_attempts', 'related attempts changed during verification'),
                ('related_rows', 'related artifact evidence changed during verification'),
                ('finalized', 'another finalize for this task landed during verification'),
                ('main_attempt_id', 'finalizing main identity changed during verification')):
            if current[field] != snapshot[field]:
                raise ValueError(message)
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


SHA256 = re.compile(r'[0-9a-f]{64}')
EMPTY_SHA256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'


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
        digest = row.get('patch_sha256')
        # A digest names exact patch bytes (the empty patch names none); rows without one match by task.
        digest = digest if isinstance(digest, str) and SHA256.fullmatch(digest) and digest != EMPTY_SHA256 else None
        if kind in ('patch-admit', 'patch-land'):
            admitted.add((ident, digest))
        elif kind in ('ask', 'call', 'consult', 'delegate', 'review', 'ask-synthesis') and row.get('patch'):
            patches.add((ident, digest))
    if len(attempts) >= 300 or len(rows) >= 1000:
        patches.update((ident, None) for ident in wanted - seen)
    return {ident for ident, digest in patches if not any(
        task == ident and (digest is None or done is None or done == digest) for task, done in admitted)}


def results(repo, task_id=None, room_participant=None, _shared=False, _tick=None, _records_data=None,
            include_preflight=False, offset=0, limit=20):
    if task_id and not ID.fullmatch(task_id):
        raise ValueError('invalid task ID')
    if not isinstance(offset, int) or offset < 0 or not isinstance(limit, int) or limit < 1:
        raise ValueError('invalid results page')
    if _records_data is not None:
        attempts, rows = _records_data
    elif _tick is not None:
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
    if not include_preflight:
        ids = [ident for ident in ids if not ident.startswith('preflight-')]
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
    chosen = list(reversed(chosen))
    # Page over the rows that survive the scope checks below, so skipped tasks never count as omitted.
    output, passed, capped = [], 0, False
    for ident in chosen:
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
        passed += 1
        if not task_id and passed <= offset:
            continue
        if not task_id and len(output) >= limit:
            capped = True
            break
        last = metadata_attempts[-1] if metadata_attempts else {}
        metadata = last.get('refs', {})
        calls = []
        for row in indexed[-30:]:
            if row.get('kind') not in ('ask', 'call', 'consult', 'delegate', 'review', 'ask-synthesis', 'patch-admit', 'patch-land'):
                continue
            call = {k: row[k] for k in ('kind', 'exit', 'verify_exit', 'verify_status', 'state', 'reason_code', 'guard',
                                       'provider_error_exit', 'artifact', 'patch', 'patch_sha256', 'event_id',
                                       'selected_model', 'requested_model', 'served_model', 'attempt_id') if k in row}
            attempt_meta = next((a for a in matching if a.get('attempt_id') == row.get('attempt_id')), {})
            alias = attempt_meta.get('refs', {}).get('panel_alias')
            if alias:
                call['alias'] = alias
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
        calls_by_stage = {'implement': [], 'verify': [], 'review': [], 'main_decision': []}
        for call in calls:
            if call.get('verify_exit') is not None:
                calls_by_stage['verify'].append(dict(call, stage_exit=call.get('verify_exit')))
            stage = ('review' if call.get('kind') == 'review' else
                     'implement' if call.get('kind') in ('delegate', 'call', 'ask', 'consult') else None)
            if stage:
                calls_by_stage[stage].append(call)
        stages = {name: ({'state': ('failed' if any(c.get('stage_exit', c.get('exit')) not in (None, 0) for c in values) else 'passed'),
                          'attempt_id': (values[-1].get('attempt_id') if values else None),
                          'exit': (values[-1].get('stage_exit', values[-1].get('exit')) if values else None)} if values else {'state': 'not_run'})
                  for name, values in calls_by_stage.items()}
        main_state = (result or {}).get('outcome') or 'pending'
        stages['main_decision'] = {'state': 'failed' if main_state == 'failed' else main_state}
        failed_stage = next((name for name in ('implement', 'verify', 'review', 'main_decision')
                             if stages[name]['state'] == 'failed'), None)
        failed_call = next((c for c in calls_by_stage.get(failed_stage, [])
                            if c.get('stage_exit', c.get('exit')) not in (None, 0)), {}) if failed_stage else {}
        ending = call_ending(failed_call) if failed_call else None
        guard = failed_call.get('guard')
        guard_name = clean(guard, 80) if isinstance(guard, str) else ''
        if ending == 'cancelled':
            representative = failed_stage + ' cancelled'
            stages[failed_stage]['state'] = 'cancelled'
            action = 'none'
        elif ending == 'guard_stop':
            representative = failed_stage + ' guard stop' + (' (' + guard_name + ')' if guard_name else '')
            stages[failed_stage]['state'] = representative[len(failed_stage) + 1:]
            action = 'inspect guard stop and salvage partial work'
        elif ending == 'timed_out':
            representative = 'timed out'
            stages[failed_stage]['state'] = 'timed out'
            action = 'continue with --continue TASK_ID'
        elif ending == 'verify_failed':
            representative, action = 'verify failed', 'retry verify'
            stages[failed_stage]['state'] = 'verify failed'
        elif ending == 'provider_error':
            representative, action = failed_stage + ' provider error', 'retry ' + failed_stage
            stages[failed_stage]['state'] = 'provider error'
        elif failed_stage:
            representative, action = failed_stage + ' failed', 'retry ' + failed_stage
        else:
            representative = stages['main_decision']['state']
            action = ('review main decision' if failed_stage == 'main_decision' else
                      'record main decision' if stages['main_decision']['state'] == 'pending' else 'none')
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
                       'issue': issue, 'missing_result': result is None,
                       'stages': stages, 'representative_state': representative, 'next_action': action})
    return {'schema': 1, 'kind': 'oms-panel-results', 'rows': output,
            'coverage': 'latest 300 attempts and 1000 artifact events',
            'truncated': capped or (bool(task_id) and not chosen), 'offset': offset,
            # A capped page knows only that more rows follow, not how many survive the scope checks.
            'omitted': None if capped else 0,
            'omitted_range': ([offset + len(output), None] if capped else None)}


def results_snapshot(repo, task_id=None, room_participant=None):
    """Collect retained result records once for a reader that also needs their event index."""
    records = _records(repo)
    report = results(repo, task_id, room_participant, _records_data=records)
    return report, records
