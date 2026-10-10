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


def _records(repo, full=False):
    attempts = _query(repo, 'agent-events', 'list', *([] if full else ['--limit', '300']))
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


def _diffstat(repo, row):
    """Changed files of a recorded patch, read only while its digest still matches."""
    data = verified_patch(repo, row)
    files, header = [], False
    for line in data.decode('utf-8', 'replace').splitlines():
        if line.startswith('diff --git '):
            # Fallback name; the header's +++/--- lines name the file without ' b/' ambiguity.
            files.append([clean(line.split(' b/', 1)[-1], 160), 0, 0])
            header = True
        elif files and header and line.startswith(('+++ ', '--- ')):
            name = line[4:].strip().strip('"')
            if name != '/dev/null' and (line.startswith('+++ ') or files[-1][0] == ''):
                files[-1][0] = clean(name[2:] if name[:2] in ('a/', 'b/') else name, 160)
        elif files and line.startswith('@@'):
            header = False
        elif files and not header and line.startswith('+'):
            files[-1][1] += 1
        elif files and not header and line.startswith('-'):
            files[-1][2] += 1
    return {'files': files[:20], 'count': len(files), 'added': sum(f[1] for f in files),
            'removed': sum(f[2] for f in files)}


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
        payload = {'schema': 1, 'kind': 'oms-panel-result', 'task_id': task_id,
                   'owner': owner, 'outcome': outcome, 'summary': summary,
                   'summary_sha256': hashlib.sha256(summary.encode()).hexdigest(),
                   'verification': verification, 'evidence': refs,
                   'main_attempt_id': main_id,
                   'admission_event_ids': [a.get('event_id') for a in admissions if a],
                   'attempt_ids': [a['attempt_id'] for a in related[-20:]]}
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


def outcomes(repo, task_ids, _shared=False):
    """The owner's recorded outcome for each of these tasks, beyond the twenty-task window results() pages."""
    wanted = {i for i in task_ids if isinstance(i, str) and ID.fullmatch(i)}
    if not wanted:
        return {}
    if _shared:
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


def results(repo, task_id=None, room_participant=None, _shared=False):
    if task_id and not ID.fullmatch(task_id):
        raise ValueError('invalid task ID')
    if _shared:
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
        matching = [a for a in attempts if a.get('task_id') == ident]
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
            linked = {a.get("attempt_id") for a in matching}
            indexed = [r for r in indexed if r.get("attempt_id") in linked]
        if scoped and not matching:
            continue
        if len(output) >= 20:
            capped = True
            break
        last = matching[-1] if matching else {}
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
            if (task_id or room_participant) and row.get('patch') and row.get('patch_sha256'):
                try:
                    call['changes'] = _diffstat(repo, row)
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
