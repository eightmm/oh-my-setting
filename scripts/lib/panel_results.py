"""Owner result artifacts and delivery receipts, indexed by existing OMS state."""

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import time
import threading
import uuid

import codex_app_notify
from dashboard_projection import clean
from call_ending import call_ending
from panel_view import TERMINAL_STATES
from peer_artifacts import artifact_sections_stream, debate_sections
from work_journal import sanitize_multiline
from path_scope import classify
import room
from panel_routing import MAIN_OWNERS

ROOT = Path(__file__).resolve().parents[2]
ENTRY = ROOT / 'scripts/oms'
DURABLE = runpy.run_path(str(ROOT / 'scripts/lib/durable-jsonl.py'))
ID = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]{0,159}\Z')
REVISION = re.compile(r'[0-9a-f]{24}\Z')
MAX_SUMMARY, MAX_RESULT = 8192, 32768
MAX_DRAFT = 4096
# Landing capture objects and delegate patches go up to 16 MiB; evidence files stay at 1 MiB.
PATCH_MAX = 16 * 1024 * 1024
TERMINAL_FAILURE_STATES = TERMINAL_STATES - {'done'}


def _reason_phrase(code):
    phrases = {
        'panel_call_interrupted': 'interrupted (caller exited or was signalled)',
        'provider_failed': 'provider failed',
    }
    if not isinstance(code, str) or not code:
        return None
    safe_code = clean(code, 120) or 'unknown'
    return phrases.get(code, 'stopped (%s)' % safe_code)


MAX_VERIFY_OUTPUT = 8192
REVIEW_WAIT_SECONDS = 120


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


def _records(repo, full=False, _attempts=None, task_id=None):
    attempts = (_query(repo, 'agent-events', 'list', *([] if full else ['--limit', '300']))
                if _attempts is None else (_attempts if full else _attempts[-300:]))
    if not isinstance(attempts, list):
        raise ValueError('invalid result evidence')
    # The CLI's tail window cannot prove task coverage. Reuse its strict,
    # no-follow reader and select this task only after validating the snapshot.
    store = runpy.run_path(str(ROOT / 'scripts/lib/artifact-index-store.py'))
    try:
        body = store['read_index'](str(repo), '.oms/artifacts/index.jsonl', missing_ok=True)
    except (OSError, ValueError, SystemExit) as error:
        raise ValueError('artifact evidence coverage incomplete: strict retained-index read failed') from error
    linked = {a.get('attempt_id') for a in attempts if a.get('task_id') == task_id}
    index = [json.loads(line) for line in body.splitlines()]
    # A whole-main result has no task attempt; keep its children's rows available.
    if task_id and linked:
        index = [r for r in index if not r.get('task_id') or r.get('task_id') == task_id
                 or r.get('attempt_id') in linked]
    return attempts, index


def prior_findings(repo, task_id, brief):
    """Return bounded, untrusted findings from settled read-only call artifacts."""
    task_ids = [task_id] if isinstance(task_id, str) else list(task_id or [])
    attempts, rows = _records(repo, full=True)
    by_attempt = {row.get('attempt_id'): row for row in attempts
                  if row.get('task_id') in task_ids and row.get('state') == 'done'
                  and row.get('refs', {}).get('panel_access') == 'read'}
    results = []
    for row in rows:
        attempt = by_attempt.get(row.get('attempt_id'))
        path = row.get('artifact')
        if (not attempt or row.get('kind') not in {'ask', 'call', 'consult', 'review', 'delegate'}
                or row.get('exit') != 0 or not isinstance(path, str)
                or not path.startswith('.oms/artifacts/') or path in brief):
            continue
        try:
            content = _indexed(repo, row, 1024 * 1024).decode('utf-8')
        except (OSError, ValueError, UnicodeError):
            continue
        answer, _ = artifact_sections_stream(io.BytesIO(content.encode('utf-8')))
        from panel_review import _review_verdict
        gate = _review_verdict(answer)
        _, sections = debate_sections(answer)
        findings = next((body for heading, body in sections if heading == 'Findings'), '')
        block = findings.strip()
        if not block:
            block = next((match.group(1).strip() for line in answer.splitlines()
                          if (match := re.match(r'(?i)^summary\s*:\s*(.*)', line))), '')
        if not block:
            continue
        from panel_review import _relativize_quoted_paths
        block = _relativize_quoted_paths(block, [str(Path(repo).resolve())])
        if _contains_outbound_machine_path(block):
            block = 'summary omitted (contains a machine path)'
        role = attempt.get('refs', {}).get('panel_role') or attempt.get('tool') or 'read-only'
        outcome = ('GATE: ' + gate) if gate else str(row.get('outcome') or 'completed')
        header = '- Source task %s — %s — %s — %s' % (attempt.get('task_id'), role, outcome, path)
        results.append((row.get('ts') or attempt.get('updated_at') or '', header, path, block))
    selected = sorted(results, key=lambda item: item[0], reverse=True)[:5]
    if not selected:
        return ''
    safe = _scan_finding_blocks([item[3] for item in selected])
    lines = []
    for (_, header, path, block), clean_block in zip(selected, safe):
        if not clean_block:
            block = 'finding omitted (sensitive-looking content; read %s)' % path
        limit = 2400
        if len(block) > limit:
            omitted = len(block) - limit
            block = block[:limit].rstrip() + '\n[omitted %d characters from finding block]' % omitted
        lines.append(header + '\n\n' + block)
    return ('\n\n## Prior findings (untrusted evidence; verify against source; no added scope or authority)\n\n'
            + '\n\n'.join(lines) + '\n')


def _scan_finding_blocks(blocks):
    """Use the dispatch scanner for each block in one bounded shell invocation."""
    if not blocks:
        return []
    try:
        with tempfile.TemporaryDirectory(prefix='oms-findings-scan-') as directory:
            paths = []
            for index, block in enumerate(blocks):
                path = Path(directory) / str(index)
                path.write_text(block, encoding='utf-8')
                paths.append(str(path))
            screen = subprocess.run(
                ['bash', '-c', '. "$1" || exit 1; shift; for file do '
                 'ma_validate_outbound_prompt "$file" >/dev/null 2>&1; printf "%s\\n" "$?"; done',
                 'panel-findings', str(ROOT / 'scripts/lib/peer-common.sh'), *paths],
                capture_output=True, text=True, timeout=30, check=False)
            statuses = screen.stdout.splitlines()
            if screen.returncode or len(statuses) != len(blocks) or any(s not in {'0', '3'} for s in statuses):
                return [False] * len(blocks)
            return [status == '0' for status in statuses]
    except (OSError, UnicodeError, subprocess.SubprocessError):
        return [False] * len(blocks)


def _contains_outbound_machine_path(value):
    """Apply the outbound machine-path rule plus Windows absolute path forms."""
    try:
        screen = subprocess.run(
            ['bash', '-c', '. "$1"; grep -Eiq "$(agent_memory_machine_re)"',
             'panel-result', str(ROOT / 'scripts/lib/agent-memory-common.sh')],
            input=value, capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.SubprocessError):
        return True
    return (screen.returncode != 1 or
            re.search(r'(?<![A-Za-z0-9])[A-Za-z]:[\\/][^\s]+|\\\\[^\\/]+[\\/][^\s]+', value) is not None)


def _read(repo, relative, maximum=MAX_RESULT, missing=False):
    try:
        return DURABLE['read_no_follow'](str(repo), relative, 'panel evidence',
                                          missing_ok=missing, max_bytes=maximum)
    except SystemExit as error:
        raise ValueError('panel evidence unavailable or unsafe') from error


def _relative(repo, value, maximum=1024 * 1024):
    if (not isinstance(value, str) or not value or len(value) > 300 or
            value.startswith(('/', '\\')) or '\\' in value or ':' in value or
            any(part in ('', '.', '..') for part in value.split('/'))):
        raise ValueError('evidence must be a safe repository-relative path')
    return {'path': value, 'sha256': hashlib.sha256(_read(repo, value, maximum)).hexdigest()}


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
    if screen.returncode != 1 or re.search(r'(?<![\w:/.])/(?:[^\s/]+/)+[^\s/]+|[A-Za-z]:\\', text):
        raise ValueError('summary contains sensitive-looking content or a private path')
    return sanitize_multiline(text, MAX_SUMMARY).strip()


def _artifact_path(repo, path):
    if not isinstance(path, str) or not path.startswith('.oms/artifacts/'):
        raise ValueError('artifact is outside OMS evidence')
    try:
        artifacts = (Path(repo) / '.oms' / 'artifacts').resolve(strict=True)
        target = (Path(repo) / path).resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ValueError('artifact is outside OMS evidence') from error
    if os.path.commonpath((str(artifacts), str(target))) != str(artifacts):
        raise ValueError('artifact is outside OMS evidence')


def _indexed(repo, row, maximum=MAX_RESULT):
    path = row.get('artifact', '')
    _artifact_path(repo, path)
    data = _read(repo, path, maximum)
    if not row.get('artifact_sha256') or hashlib.sha256(data).hexdigest() != row['artifact_sha256']:
        raise ValueError('artifact digest mismatch')
    return data


def _final_attempt_output(repo, artifact):
    """Bounded, confined counterpart of peer_artifacts.final_attempt_output.

    Missing records or unusable entries retain the indexed transcript; unsafe
    reads propagate so results can report unavailable evidence.
    """
    base = artifact[:-3] if artifact.endswith('.md') else artifact
    sidecar = base + '.attempts.json'
    try:
        info = (Path(repo) / sidecar).lstat()
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(info.st_mode):
        raise ValueError('attempt record is a symlink')
    _artifact_path(repo, sidecar)
    record = json.loads(_read(repo, sidecar, 1024 * 1024))
    attempts = record.get('attempts') if isinstance(record, dict) else None
    if not isinstance(attempts, list):
        return None
    for attempt in reversed(attempts):
        name = attempt.get('output') if isinstance(attempt, dict) else None
        if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*\.out', name):
            continue
        output = str(Path(sidecar).parent / name)
        if stat.S_ISLNK((Path(repo) / output).lstat().st_mode):
            raise ValueError('attempt output is a symlink')
        _artifact_path(repo, output)
        return _read(repo, output, 1024 * 1024).decode('utf-8', errors='replace').strip()
    return None


def _review_run_id(row):
    review = row.get('review')
    recorded_run = review.get('run') if isinstance(review, dict) else None
    if isinstance(recorded_run, str) and recorded_run:
        return recorded_run
    artifact = row.get('artifact')
    if isinstance(artifact, str):
        match = re.search(r'-(\d{8}T\d{6}Z-\d+)(?:-r\d+)?(?:\.md)?$', Path(artifact).name)
        if match:
            return match.group(1)
    for field in ('run_id', 'operation_id', 'attempt_id'):
        value = row.get(field)
        if isinstance(value, str) and value:
            return value
    return row.get('event_id') or row.get('artifact') or ''


def _review_run_order(run_id):
    match = re.match(r'^(\d{8}T\d{6}Z)-(\d+)', run_id if isinstance(run_id, str) else '')
    return (match.group(1), int(match.group(2))) if match else ('', 0)


def verified_patch(repo, row, maximum=PATCH_MAX):
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
        if _relative(repo, patch['patch'], PATCH_MAX)['sha256'] != patch['patch_sha256']:
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


def _write(repo, relative, payload, maximum=MAX_RESULT):
    body = (json.dumps(payload, ensure_ascii=False, sort_keys=True, allow_nan=False) + '\n').encode()
    def immutable(old):
        if old and old != body:
            raise ValueError('result artifact already contains different bytes')
        return body
    try:
        path = DURABLE['canonical_repo_path'](str(repo), relative, 'panel result', True)
        DURABLE['atomic_mutate'](path, immutable, str(repo), missing_ok=True,
                                 create_parent=True, max_output=maximum, label='panel result')
        return path
    except SystemExit as error:
        raise ValueError('panel artifact write refused') from error


def write_summary_draft(repo, row, decision=''):
    """Write a bounded owner-editable summary beside existing panel result artifacts."""
    if not isinstance(row, dict) or not isinstance(row.get('task_id'), str) or not ID.fullmatch(row['task_id']):
        raise ValueError('invalid result row for summary draft')
    stages = row.get('stages') if isinstance(row.get('stages'), dict) else {}
    lines = ['# Panel result draft', '', '- Task: ' + clean(row['task_id'], 160), '',
             '## Decision', clean(decision, 1000) or 'No reason recorded.', '', '## Stages']
    for name in ('implement', 'verify', 'review'):
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
            or result.get('owner') not in MAIN_OWNERS or result.get('outcome') not in ('completed', 'accepted', 'failed', 'rejected')
            or not isinstance(result.get('summary'), str) or not isinstance(result.get('evidence'), list)):
        raise ValueError('invalid result artifact')
    superseded = result.get('superseded_attempts', [])
    if (not isinstance(superseded, list) or any(
            not isinstance(item, dict) or any(not isinstance(item.get(key), str)
                or not ID.fullmatch(item[key]) for key in ('attempt_id', 'superseded_by'))
            or item['attempt_id'] == item['superseded_by'] for item in superseded)):
        raise ValueError('invalid supersession evidence')
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


def result_matches_plan(repo, task_id, result, bindings=None):
    """Whether this result names the current plan generation for its task."""
    current = bindings if bindings is not None else _plan_bindings(repo, {task_id})
    binding = current.get(task_id)
    return isinstance(binding, dict) and result.get('plan_binding') == binding


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
            if (result_matches_plan(repo, ident, result, bindings)
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


def _plan_hint(repo, task_id, has_write_patch):
    # No land receipt exists here, so the plan is never finished automatically.
    try:
        shown = subprocess.run(['bash', str(ENTRY), 'agent-plan', '--repo', str(repo), 'show', '--id', task_id],
                               capture_output=True, text=True, check=False, stdin=subprocess.DEVNULL, timeout=60)
        if not shown.returncode and json.loads(shown.stdout).get('state') not in ('done', None):
            if has_write_patch:
                print('plan %s is not done: after oms land run oms agent-plan finish --id %s --landed-commit SHA'
                      % (task_id, task_id), file=sys.stderr)
            else:
                print('plan %s is not done: a task with no patch cannot finish by landing; close it with '
                      'oms agent-plan accept-main-research (completion bundle) or oms agent-plan block '
                      '--id %s --reason TEXT' % (task_id, task_id), file=sys.stderr)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass


def _run_verifier(repo, verify, timeout, evidence_repo=None):
    creationflags = getattr(subprocess, 'CREATE_NEW_PROCESS_GROUP', 0) if os.name == 'nt' else 0
    process = subprocess.Popen(['bash', '-c', verify], cwd=str(repo),
                               stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=os.name == 'posix',
                               creationflags=creationflags)
    output = {}
    def drain(name, pipe):
        retained, total = bytearray(), 0
        output[name] = {'data': retained, 'bytes': 0, 'complete': False}
        try:
            while True:
                chunk = pipe.read1(8192)
                if not chunk:
                    break
                retained.extend(chunk)
                del retained[:-MAX_VERIFY_OUTPUT]
                total += len(chunk)
                output[name]['bytes'] = total
            output[name]['complete'] = True
        finally:
            pipe.close()
    readers = [threading.Thread(target=drain, args=(name, pipe), daemon=True)
               for name, pipe in (('stdout', process.stdout), ('stderr', process.stderr))]
    for reader in readers:
        reader.start()
    timed_out = False
    try:
        returncode = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
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
        returncode = process.wait()
    for reader in readers:
        reader.join(timeout=1)
    result = {'exit': returncode}
    if returncode or timed_out:
        diagnostic = {'schema': 1, 'kind': 'oms-panel-verifier', 'exit': returncode,
                      'timed_out': timed_out, 'command_sha256': hashlib.sha256(verify.encode()).hexdigest()}
        for name, stream in output.items():
            data = bytes(stream['data'])
            diagnostic[name] = {'text': data.decode('utf-8', 'replace'), 'bytes': stream['bytes'],
                                'truncated': stream['bytes'] > len(data) or not stream['complete'],
                                'complete': stream['complete'], 'retained': 'tail'}
        relative = '.oms/artifacts/panel-results/verify-%s.json' % uuid.uuid4().hex
        _write(evidence_repo or repo, relative, diagnostic, maximum=16 * MAX_VERIFY_OUTPUT)
        result['diagnostics'] = _relative(evidence_repo or repo, relative)
    if timed_out:
        raise ValueError('verifier timed out after %s s; private diagnostics: %s'
                         % (timeout, result['diagnostics']['path']))
    return result


def _write_candidates(workers, admissions, attempts, owner, main_id=None):
    """Account for discarded retries without treating failure as admission."""
    from review_settlement import _participant_parents

    superseded, selected, landed, lineages = [], [], [], {}
    for index, worker in enumerate(workers):
        replacement = None
        reason = None
        if not admissions[index] and worker.get('task_id'):
            for end in range(index + 1, len(workers)):
                if not admissions[end] or workers[end].get('task_id') != worker['task_id']:
                    continue
                cursor = end
                while cursor > index:
                    previous = workers[cursor].get('refs', {}).get('panel_continues')
                    cursor = next((pos for pos in range(cursor - 1, index - 1, -1)
                                   if workers[pos].get('attempt_id') == previous
                                   and workers[pos].get('task_id') == worker['task_id']), -1)
                if cursor == index:
                    replacement, reason = workers[end], 'continued'
                    break
        # Unassigned writes and distinct aliases cannot be assumed to be retries.
        if (not replacement and not admissions[index] and worker.get('task_id') and worker.get('parent_attempt_id')
                and worker.get('state') in {'failed', 'timed_out', 'cancelled', 'abandoned'}):
            for later, admission in zip(workers[index + 1:], admissions[index + 1:]):
                parent = later.get('parent_attempt_id')
                anchor = main_id or parent
                if anchor not in lineages:
                    main = next((a for a in attempts if a.get('attempt_id') == anchor), {})
                    try:
                        lineages[anchor] = (_participant_parents(attempts, main)
                                            if main.get('provider') == owner else set())
                    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
                        lineages[anchor] = set()
                if (admission and later.get('task_id') == worker['task_id']
                        and ((parent == worker.get('parent_attempt_id') and (not main_id or parent == main_id))
                             or {parent, worker.get('parent_attempt_id')} <= lineages[anchor])
                        and later.get('refs', {}).get('panel_alias') == worker.get('refs', {}).get('panel_alias')
                        and later.get('created_at', '') > worker.get('created_at', '')):
                    replacement = later
                    break
        if replacement:
            superseded.append({'attempt_id': worker['attempt_id'],
                               'superseded_by': replacement['attempt_id'],
                               'reason': reason or worker.get('reason_code') or worker['state']})
        else:
            selected.append(worker)
            landed.append(admissions[index])
    return selected, landed, superseded


def _finalize_owns_task(repo, owner, task_id, main_id, attempts, binding):
    """Fence panel mains while preserving standalone and unclaimed plan results."""
    if not main_id:
        return
    if binding is not None:
        from panel_dispatch import PlanBinding
        from panel_state import active_mains
        try:
            plan = json.loads(_read(repo, '.oms/plan/tasks.json', 4 * 1024 * 1024),
                              object_pairs_hook=PlanBinding.unique_fields)
            task = plan.get('tasks', {}).get(task_id)
            if not isinstance(task, dict) or task.get('id') != task_id:
                raise ValueError('plan task is unavailable')
            claimant = task.get('claimed_by_participant')
            if claimant is None or claimant == '':
                return
            caller = os.environ.get('OMS_ROOM_PARTICIPANT')
            ident = os.environ.get('OMS_ROOM_ID')
            if not isinstance(claimant, str) or not caller or not ident or claimant != caller:
                raise ValueError('plan claimant does not match')
            state = room.project(room.records(repo, ident))
            main = active_mains(repo, ident, state, owner, caller=caller, attempt=main_id).get(caller)
            if state['closed'] or not main or main['attempt_id'] != main_id:
                raise ValueError('current room main is unavailable')
        except (OSError, TypeError, ValueError, AttributeError) as error:
            raise ValueError('finalize task %s is not owned by the calling main: %s'
                             % (task_id, error)) from error
        return
    task_attempts = [a for a in attempts if a.get('task_id') == task_id]
    if not task_attempts:
        return
    from panel_state import lineage_ids_from_rows
    # The snapshot validates this main; continuation evidence is needed for earlier parents.
    eligible = {main_id}
    eligible.update(lineage_ids_from_rows(attempts, owner, os.environ.get('OMS_ROOM_PARTICIPANT'), main_id))
    by_id = {a.get('attempt_id'): a for a in attempts}
    for attempt in task_attempts:
        ident, seen = attempt.get('attempt_id'), set()
        while ident not in eligible:
            if not ident or ident in seen or ident not in by_id:
                raise ValueError('finalize task %s attempt %s does not descend from the calling main'
                                 % (task_id, attempt.get('attempt_id')))
            seen.add(ident)
            ident = by_id[ident].get('parent_attempt_id')


def _finalize_snapshot(repo, owner, task_id, main_id, evidence):
    binding = _plan_bindings(repo, {task_id}).get(task_id)
    attempts, rows = _records(repo, full=True, task_id=task_id)
    if main_id:
        main = next((a for a in attempts if a.get('attempt_id') == main_id), {})
        if main.get('tool') != 'panel-main' or main.get('provider') != owner:
            raise ValueError('finalizing main identity is unavailable or does not match the owner')
    related = [a for a in attempts if a.get('task_id') == task_id
               or (main_id and a.get('parent_attempt_id') == main_id)]
    # Other tasks are accepted independently; unassigned children still belong to this main.
    workers = sorted([a for a in related if (a.get('task_id') == task_id or not a.get('task_id'))
                      and a.get('refs', {}).get('panel_access') == 'write'
                      and a.get('refs', {}).get('panel_role') == 'worker'],
                     key=lambda a: (a.get('created_at', ''), a['attempt_id']))
    admissions = [_admission(repo, rows, a.get('task_id'), a) for a in workers]
    selected, admissions, superseded = _write_candidates(workers, admissions, attempts, owner, main_id)
    # Fence this task and its unassigned write workers while other tasks keep moving.
    worker_ids = {a.get('attempt_id') for a in workers}
    task_attempts = [a for a in related if a.get('task_id') == task_id or a.get('attempt_id') in worker_ids]
    attempt_ids = {a.get('attempt_id') for a in task_attempts}
    related_rows = [r for r in rows if r.get('task_id') == task_id or r.get('attempt_id') in attempt_ids]
    finalized = [{'row': r, 'record': _load_result(repo, r)} for r in rows
                 if r.get('kind') == 'panel-result' and r.get('task_id') == task_id]
    return {'binding': binding, 'main_attempt_id': main_id, 'related_attempts': task_attempts,
            'related_rows': related_rows, 'admissions': admissions, 'evidence': evidence,
            'finalized': finalized, 'superseded_attempts': superseded}, attempts, rows, related, selected, admissions


def _live_review_read_passes(repo, execution_checkout=None):
    """Return live reviews reading the execution checkout, including uncertain sources."""
    entries = Path(repo) / '.oms/locks/canonical-sync.d'
    if not entries.exists() and not entries.is_symlink():
        if entries.parent.exists() and not os.access(str(entries.parent), os.R_OK | os.X_OK):
            return ['canonical-sync-registry']
        return []
    if entries.is_symlink() or not entries.is_dir() or not os.access(str(entries), os.R_OK | os.X_OK):
        return ['canonical-sync-registry']
    try:
        command = ['bash', '-c', '. "$1"; shift; oms_canonical_sync_live_review_entries "$@"',
                   'panel-live-reviews', str(ROOT / 'scripts/lib/canonical-sync.sh'), str(repo)]
        if execution_checkout is not None:
            command.append(str(execution_checkout))
        live = subprocess.check_output(
            command,
            stderr=subprocess.DEVNULL, text=True, timeout=30).replace('\r', '')
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return ['canonical-sync-registry']
    return sorted(name for name in live.splitlines() if name)


def _review_preflight(repo, task_id, workers, rows, admissions=None):
    """Share scope and digest-bound review checks before application and acceptance."""
    # Scope evidence is checked first, including for unlanded candidates.
    missing_admission = [workers[index].get('attempt_id', 'unknown') if index < len(workers) else 'unknown'
                         for index, admission in enumerate(admissions) if not admission] if admissions is not None else []
    missing_reviews = []
    passing_gates = []
    for index, worker in enumerate(workers):
        patches = [r for r in rows if r.get('kind') == 'delegate'
                   and r.get('attempt_id') == worker.get('attempt_id') and r.get('patch_sha256')]
        if worker.get('refs', {}).get('panel_purpose') == 'investigate':
            raise ValueError('report-only investigation findings cannot be accepted; finalize completed instead')
        owns = _worker_owns(repo, worker)
        for row in patches:
            data = verified_patch(repo, row)
            drift = _scope_drift(_patch_paths(data), owns)
            if drift:
                raise ValueError('write worker patch has %s path(s) outside its declared scope: %s'
                                 % (len(drift), ', '.join(drift[:8])))
        digest = (admissions[index].get('patch_sha256') if admissions is not None and admissions[index]
                  else patches[-1].get('patch_sha256') if patches else None)
        reviews = [r for r in rows if r.get('kind') == 'review-outcome'
                   and (r.get('task_id') == task_id or (not worker.get('task_id') and not r.get('task_id')))
                   and isinstance(r.get('review'), dict)
                   and isinstance(r['review'].get('candidate'), dict)
                   and r['review']['candidate'].get('patch_sha256') == digest]
        latest = reviews[-1] if reviews else None
        if not (latest and latest.get('exit') == 0 and latest['review'].get('gate_verify_exit') == 0
                and isinstance(latest['review'].get('seats'), list) and latest['review']['seats']
                and all(isinstance(s, dict) and s.get('verdict') == 'pass' for s in latest['review']['seats'])):
            missing_reviews.append(worker.get('attempt_id', 'unknown'))
        else:
            passing_gates.append({'event_id': latest.get('event_id'), 'patch_sha256': digest,
                                  'exit': latest.get('exit'),
                                  'gate_verify_exit': latest['review'].get('gate_verify_exit'),
                                  'seat_verdicts': [seat.get('verdict') for seat in latest['review']['seats']]})
    if missing_admission:
        raise ValueError("write worker call(s) lack matching landed patch evidence: %s; acceptance needs oms patch-land of each worker's own patch using the exact delegate patch bytes so the landing is recorded; landing evidence is read from this state repository, so a patch-land run in an independent clone does not count; land from this checkout or an oms scratch-worktree; otherwise finalize with --outcome completed"
                         % ', '.join(missing_admission))
    if missing_reviews:
        raise ValueError('write worker call(s) lack a passing review gate: %s; run the review or finalize completed instead'
                         % ', '.join(missing_reviews))
    return passing_gates


def _execution_repo(repo, execution_repo):
    from room_repository import state_repository, work_repository
    execution = work_repository(execution_repo or repo)
    if state_repository(execution) != Path(repo).resolve():
        raise ValueError('execution clone does not belong to the canonical state repository')
    return execution


def _accept_lock(repo, execution):
    # Scratch clones have no local ignored .oms directory. Keep their locks
    # with canonical evidence so locking cannot itself dirty the target tree.
    return _lock(repo, 'panel-accept-' + hashlib.sha256(os.fsencode(execution)).hexdigest()[:24])


def _plan_command(execution, *args, panel_accept=None):
    child_env = None
    if panel_accept is not None:
        child_env = dict(os.environ, OMS_PANEL_ACCEPT_RECORD_JSON=json.dumps(
            panel_accept, sort_keys=True, separators=(',', ':')))
    result = subprocess.run(['bash', str(ENTRY), 'agent-plan', '--repo', str(execution), *args],
                            env=child_env,
                            stdin=subprocess.DEVNULL, capture_output=True, text=True, check=False, timeout=60)
    if result.returncode:
        raise ValueError(result.stderr.strip() or 'plan command failed')
    return result.stdout


def _owned_task(repo, execution, owner, task_id):
    from panel_state import active_mains
    from panel_dispatch import PlanBinding
    _owner()
    if owner not in MAIN_OWNERS or not isinstance(task_id, str) or not ID.fullmatch(task_id):
        raise ValueError('invalid owner task')
    caller, main_id, ident = (os.environ.get(name) for name in
                             ('OMS_ROOM_PARTICIPANT', 'OMS_PANEL_MAIN_ATTEMPT', 'OMS_ROOM_ID'))
    if not caller or not main_id or not ident:
        raise ValueError('accept/finish requires the current owning room main')
    state = room.project(room.records(repo, ident))
    main = active_mains(repo, ident, state, owner, caller=caller, attempt=main_id).get(caller)
    if state['closed'] or not main or main['attempt_id'] != main_id:
        raise ValueError('accept/finish requires the current owning room main')
    try:
        plan = json.loads(_read(repo, '.oms/plan/tasks.json', 4 * 1024 * 1024),
                          object_pairs_hook=PlanBinding.unique_fields)
    except (OSError, TypeError, ValueError) as error:
        raise ValueError('plan task %s is unavailable: %s' % (task_id, error)) from error
    stored = plan.get('tasks', {}).get(task_id)
    if not isinstance(stored, dict):
        raise ValueError('plan task is unavailable')
    # The CLI projection adds elapsed claim age, which cannot fence mutations.
    task = dict(stored, plan_id=plan.get('plan_id'))
    if (task.get('id') != task_id or task.get('claimed_by_participant') != caller
            or task.get('provider') != owner or not re.fullmatch(r'lease_[0-9a-f]{32}', task.get('lease_id', ''))
            or not re.fullmatch(r'plan_[0-9a-f]{32}', task.get('plan_id', ''))):
        raise ValueError('plan task owner/lease does not match the current main')
    return task


def _accept_preflight(repo, execution, owner, task_id, verify, expected=None, applied=False):
    task = _owned_task(repo, execution, owner, task_id)
    if task.get('state') not in ('claimed', 'running'):
        raise ValueError('accept requires a claimed/running task; reconcile existing application or commit before retry')
    if not verify or verify.replace('\r', '') != task.get('verify', '').replace('\r', ''):
        raise ValueError('--verify does not match the plan task verify contract')
    main_id = os.environ['OMS_PANEL_MAIN_ATTEMPT']
    _, _, rows, _, workers, admissions = _finalize_snapshot(repo, owner, task_id, main_id, [])
    if len(workers) != 1 or workers[0].get('task_id') != task_id:
        raise ValueError('accept requires one unique worker candidate for this task')
    worker = workers[0]
    candidates = [r for r in rows if r.get('kind') == 'delegate' and r.get('task_id') == task_id
                  and r.get('attempt_id') == worker['attempt_id'] and r.get('patch_sha256')]
    if (len(candidates) != 1 or candidates[0].get('exit') != 0
            or worker.get('parent_attempt_id') != main_id
            or worker.get('refs', {}).get('panel_lease_id') != task['lease_id']
            or candidates[0].get('plan_id') != task['plan_id']):
        raise ValueError('candidate is ambiguous or does not match the current main/plan lease')
    candidate = candidates[0]
    if expected is not None and candidate['patch_sha256'] != expected:
        raise ValueError('candidate patch digest drift')
    passing_gates = _review_preflight(repo, task_id, workers, rows)
    verified_patch(repo, candidate)
    _indexed(repo, candidate, 1024 * 1024)
    if any(admissions) and not applied:
        raise ValueError('candidate already applied; reconcile existing Git/receipt evidence; never reapply blindly')
    return {'task': task, 'candidate': candidate, 'worker': worker, 'review_gate': passing_gates[0]}


def _accept_authority(snapshot):
    """Return only the stable plan, candidate, lease and review authorities."""
    task, candidate, worker = snapshot['task'], snapshot['candidate'], snapshot['worker']
    refs = worker.get('refs', {})
    candidate_fields = ('event_id', 'attempt_id', 'task_id', 'plan_id', 'artifact', 'patch',
                        'patch_sha256', 'exit')
    return {
        'task_hash': hashlib.sha256(json.dumps(task, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
        'lease_id': task.get('lease_id'),
        'candidate': {key: candidate.get(key) for key in candidate_fields},
        'patch_sha256': candidate.get('patch_sha256'),
        'worker': {key: worker.get(key) for key in ('attempt_id', 'task_id', 'parent_attempt_id')},
        'worker_authority': {key: refs.get(key) for key in ('panel_access', 'panel_role', 'panel_lease_id')},
        'review_gate': snapshot['review_gate'],
    }


def _accept_identity(snapshot):
    authority = _accept_authority(snapshot)
    authority.pop('review_gate')
    return authority


class _AcceptAuthorityChanged(ValueError):
    pass


def _accept_recheck(repo, execution, owner, task_id, verify, expected, baseline):
    try:
        current = _accept_preflight(repo, execution, owner, task_id, verify, expected, True)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise _AcceptAuthorityChanged('authority preflight no longer passes: %s' % error) from error
    if _accept_authority(current) != _accept_authority(baseline):
        raise _AcceptAuthorityChanged('candidate, digest, lease, task hash or review gate changed')


def _git(execution, *args, env=None):
    result = subprocess.run(['git', '-C', str(execution), *args], env=env,
                            stdin=subprocess.DEVNULL, capture_output=True, check=False, timeout=60)
    if result.returncode:
        raise ValueError('git %s failed: %s' % (args[0], result.stderr.decode('utf-8', 'replace').strip()))
    return result.stdout


class _ExactCommitPublished(ValueError):
    def __init__(self, sha, message):
        super().__init__(message)
        self.sha = sha


def _index_digest(path):
    digest = hashlib.sha256()
    flags = os.O_RDONLY | getattr(os, 'O_BINARY', 0) | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
    descriptor = os.open(path, flags)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError('Git index is not a regular file')
        source = os.fdopen(descriptor, 'rb')
        descriptor = -1
        with source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    return digest.hexdigest()


def _exact_commit_tree(execution, base, expected_tree, message, authority_check=None):
    """Publish the verified index tree with a HEAD compare-and-swap; never run commit hooks."""
    if any(os.environ.get(name) for name in ('GIT_INDEX_FILE', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR')):
        raise ValueError('custom Git repository/index overrides are unsupported for exact acceptance commits')
    if _git(execution, 'rev-parse', 'HEAD').decode().strip() != base:
        raise ValueError('HEAD changed before exact commit')
    symbolic = subprocess.run(['git', '-C', str(execution), 'symbolic-ref', '-q', 'HEAD'],
                              stdin=subprocess.DEVNULL, capture_output=True, check=False, timeout=30)
    if symbolic.returncode not in (0, 1):
        raise ValueError('cannot resolve the current HEAD ref')
    ref = symbolic.stdout.decode('utf-8', 'replace').replace('\r', '').strip() or 'HEAD'
    raw_path = _git(execution, 'rev-parse', '--git-path', 'index').decode().replace('\r', '').strip()
    if not raw_path:
        raise ValueError('Git index path is unavailable')
    index_path = Path(raw_path)
    if not index_path.is_absolute():
        index_path = Path(execution) / index_path
    index_path = Path(os.path.realpath(index_path.parent)) / index_path.name
    try:
        info = os.lstat(index_path)
    except OSError as error:
        raise ValueError('Git index is unavailable: %s' % error) from error
    if not stat.S_ISREG(info.st_mode):
        raise ValueError('Git index is not a regular file')
    lock_path = Path(str(index_path) + '.lock')
    try:
        lock_fd = os.open(lock_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as error:
        raise ValueError('cannot lock Git index: %s' % error) from error
    published = None
    try:
        source_fd = os.open(index_path, os.O_RDONLY | getattr(os, 'O_BINARY', 0) |
                            getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
        try:
            opened = os.fstat(source_fd)
            if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino):
                raise ValueError('Git index changed while acquiring its lock')
            source = os.fdopen(source_fd, 'rb')
            source_fd = -1
            try:
                target = os.fdopen(lock_fd, 'wb')
            except OSError:
                source.close()
                raise
            lock_fd = -1
            with source, target:
                for chunk in iter(lambda: source.read(1024 * 1024), b''):
                    target.write(chunk)
                target.flush()
                os.fsync(target.fileno())
        finally:
            if source_fd >= 0:
                os.close(source_fd)
            if lock_fd >= 0:
                os.close(lock_fd)
        original_digest = _index_digest(index_path)
        if _index_digest(lock_path) != original_digest:
            raise ValueError('Git index changed while acquiring its lock')
        index_env = dict(os.environ, GIT_INDEX_FILE=str(lock_path))
        if _git(execution, 'write-tree', env=index_env).decode().strip() != expected_tree:
            raise ValueError('locked Git index differs from the reviewed tree')
        if authority_check:
            authority_check()
        if (_git(execution, 'rev-parse', 'HEAD').decode().strip() != base
                or (subprocess.run(['git', '-C', str(execution), 'symbolic-ref', '-q', 'HEAD'],
                                   stdin=subprocess.DEVNULL, capture_output=True, check=False,
                                   timeout=30).stdout.decode('utf-8', 'replace').replace('\r', '').strip() or 'HEAD') != ref):
            raise ValueError('HEAD/ref changed before exact commit')
        if _index_digest(index_path) != original_digest or _index_digest(lock_path) != original_digest:
            raise ValueError('Git index changed before exact commit')
        commit = _git(execution, '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false',
                      'commit-tree', expected_tree, '-p', base, '-m', message).decode().strip()
        if (not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit)
                or _git(execution, 'rev-parse', commit + '^{tree}').decode().strip() != expected_tree
                or _git(execution, 'rev-parse', commit + '^').decode().strip() != base):
            raise ValueError('created commit object does not match the reviewed tree and parent')
        if authority_check:
            authority_check()
        if (_index_digest(index_path) != original_digest or _index_digest(lock_path) != original_digest
                or _git(execution, 'rev-parse', 'HEAD').decode().strip() != base):
            raise ValueError('Git index or HEAD changed before exact publication')
        _git(execution, '-c', 'core.hooksPath=/dev/null', 'update-ref',
             '-m', 'panel: accept reviewed patch', ref, commit, base)
        published = commit
        if (_git(execution, 'rev-parse', 'HEAD').decode().strip() != commit
                or (subprocess.run(['git', '-C', str(execution), 'symbolic-ref', '-q', 'HEAD'],
                                   stdin=subprocess.DEVNULL, capture_output=True, check=False,
                                   timeout=30).stdout.decode('utf-8', 'replace').replace('\r', '').strip() or 'HEAD') != ref
                or _git(execution, 'rev-parse', commit + '^{tree}').decode().strip() != expected_tree
                or _git(execution, 'rev-parse', commit + '^').decode().strip() != base):
            raise _ExactCommitPublished(commit, 'published HEAD does not match the exact reviewed commit')
        return commit
    except _ExactCommitPublished:
        raise
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if published:
            raise _ExactCommitPublished(published, str(error)) from error
        raise
    finally:
        try:
            os.unlink(lock_path)
        except FileNotFoundError:
            pass


def _accept_record(snapshot, base, tree, commit):
    task, candidate, worker = snapshot['task'], snapshot['candidate'], snapshot['worker']
    return {'schema': 1, 'task_id': task['id'], 'plan_id': task['plan_id'],
            'lease_id': task['lease_id'], 'candidate_event_id': candidate['event_id'],
            'candidate_attempt_id': worker['attempt_id'], 'patch_sha256': candidate['patch_sha256'],
            'artifact': candidate['artifact'], 'patch': candidate['patch'], 'commit': commit,
            'tree': tree, 'base': base, 'review_gate': snapshot['review_gate']}


def _accepted_prefix(repo, execution, landings):
    """Canonical accepts in this clone's unpublished first-parent lineage."""
    if (os.environ.get('OMS_HARNESS_CHILD') == '1' or
            os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') != '0'):
        raise ValueError('panel acceptance prefix derivation is parent-only')
    from panel_dispatch import PlanBinding
    from land_receipts import pushed_receipts
    execution = _execution_repo(repo, execution)
    def same_checkout(value):
        if not isinstance(value, str) or not value:
            return False
        if os.name == 'nt' and re.match(r'^/[A-Za-z]/', value):
            value = value[1] + ':' + value[2:]
        return (os.path.isabs(value) and
                os.path.normcase(os.path.realpath(value)) == os.path.normcase(str(execution)))

    def valid_foreign_checkout(value):
        if not isinstance(value, str) or not value:
            return False
        if os.name == 'nt' and re.match(r'^/[A-Za-z]/', value):
            value = value[1] + ':' + value[2:]
        return os.path.isabs(value) and not same_checkout(value)

    panel_intents = [row for row in landings if row.get('event') == 'intent'
                     and row.get('panel_accept') == 'true' and row.get('_has_abandoned') is not True]
    local_intents = [row for row in panel_intents if same_checkout(row.get('worktree'))]
    try:
        plan = json.loads(_read(repo, '.oms/plan/tasks.json', 4 * 1024 * 1024),
                          object_pairs_hook=PlanBinding.unique_fields)
    except (OSError, TypeError, ValueError) as error:
        task_id = next((row.get('task') for row in local_intents if row.get('task')), None)
        if task_id:
            raise ValueError('panel acceptance record unavailable for task %s: %s'
                             % (task_id, error)) from error
        raise
    if not isinstance(plan, dict):
        task_id = next((row.get('task') for row in local_intents if row.get('task')), None)
        if task_id:
            raise ValueError('panel acceptance record unavailable for task ' + str(task_id))
        raise ValueError('panel acceptance plan is malformed')
    tasks = plan.get('tasks')
    if not isinstance(tasks, dict):
        task_id = next((row.get('task') for row in local_intents if row.get('task')), None)
        if task_id:
            raise ValueError('panel acceptance record unavailable for task ' + str(task_id))
        raise ValueError('panel acceptance plan tasks are malformed')
    accepted = [(ident, task, task.get('panel_accept')) for ident, task in tasks.items()
                if isinstance(task, dict) and 'panel_accept' in task]
    if not accepted and not local_intents:
        return []

    # The shell helper owns the receipt namespace, including Git Bash path
    # spelling. A passed clone-local publication resets even unfinalized tasks.
    directory = subprocess.check_output(['bash', '-c',
        '. "$1"; panel_receipts=$(oms_land_state_dir "$2") || exit; '
        'if [ "$3" = nt ]; then cygpath -w "$panel_receipts"; '
        'else printf "%s\\n" "$panel_receipts"; fi',
        'panel-prefix', str(ROOT / 'scripts/lib/land-receipts.sh'), str(execution), os.name],
        text=True, timeout=30).replace('\r', '').rstrip('\n')
    chain_rows = _git(execution, 'rev-list', '--first-parent', '--parents', '--max-count=10000',
                      'HEAD').decode().splitlines()
    chain = [line.split()[0] for line in chain_rows if line.split()]
    parents = {parts[0]: parts[1] if len(parts) > 1 else ''
               for parts in (line.split() for line in chain_rows) if parts}
    landed = set()
    for sha, _, raw in pushed_receipts(directory, '', lambda path: _read(
            Path(path).parent, Path(path).name, 1024 * 1024)):
        receipt = json.loads(raw)
        if (receipt.get('schema') == 1 and receipt.get('state') == 'passed'
                and isinstance(receipt.get('gate'), dict) and type(receipt['gate'].get('rc')) is int
                and receipt['gate']['rc'] == 0 and isinstance(receipt.get('ci'), dict)
                and receipt['ci'].get('conclusion') in ('success', 'skipped')):
            landed.add(sha)
    boundary = next((index for index, sha in enumerate(chain) if sha in landed), len(chain))
    if boundary == len(chain) == 10000:
        raise ValueError('panel acceptance lineage exceeds replay limit')
    pending = {sha: index for index, sha in enumerate(chain[:boundary])}
    prefix = []
    replayed = set()
    for commit in chain[:boundary]:
        parent = parents.get(commit, '')
        if not parent:
            continue
        local = [row for row in local_intents if row.get('base_sha') == parent]
        recorded = [(ident, task, record) for ident, task, record in accepted
                    if isinstance(record, dict) and record.get('commit') == commit]
        handled_local = set()
        for ident, task, record in recorded:
            local_match = [row for row in local if row.get('task') == ident
                           and row.get('plan_id') == record.get('plan_id')
                           and row.get('lease') == record.get('lease_id')
                           and row.get('base_sha') == record.get('base')
                           and row.get('patch_sha') == record.get('patch_sha256')]
            foreign_match = [row for row in panel_intents
                             if valid_foreign_checkout(row.get('worktree'))
                             and row.get('task') == ident
                             and row.get('plan_id') == record.get('plan_id')
                             and row.get('lease') == record.get('lease_id')
                             and row.get('base_sha') == parent
                             and row.get('patch_sha') == record.get('patch_sha256')]
            if not local_match:
                if any(row.get('task') == ident for row in local):
                    raise ValueError('panel acceptance lineage changed: ' + ident)
                if foreign_match:
                    # Canonical task and journal evidence bind this commit to another clone.
                    continue
                raise ValueError('panel acceptance landing evidence missing for task ' + ident)
            if len(local_match) != 1:
                raise ValueError('panel acceptance landing evidence is ambiguous for task ' + ident)
            intent = local_match[0]
            handled_local.add(id(intent))
            required = {'schema', 'task_id', 'plan_id', 'lease_id', 'candidate_event_id',
                        'candidate_attempt_id', 'patch_sha256', 'artifact', 'patch', 'commit',
                        'tree', 'base', 'review_gate'}
            if (set(record) != required or record.get('schema') != 1 or record.get('task_id') != ident
                    or record.get('commit') != commit or record.get('plan_id') != plan.get('plan_id')
                    or record.get('lease_id') != task.get('lease_id')
                    or task.get('review_lease_id') != task.get('lease_id')
                    or task.get('state') not in ('review', 'done')
                    or intent.get('plan_id') != record.get('plan_id')
                    or intent.get('lease') != record.get('lease_id')
                    or intent.get('base_sha') != record.get('base')
                    or intent.get('patch_sha') != record.get('patch_sha256')
                    or record.get('base') != parent
                    or _git(execution, 'rev-parse', commit + '^{tree}').decode().strip() != record.get('tree')):
                raise ValueError('panel acceptance lineage changed for task ' + ident)
            verify = task.get('verify')
            if not isinstance(verify, str) or not verify or '\0' in verify:
                raise ValueError('invalid panel prefix verifier for task ' + ident)
            try:
                patch = _read(repo, record.get('patch'), PATCH_MAX)
                if hashlib.sha256(patch).hexdigest() != record.get('patch_sha256'):
                    raise ValueError('patch digest changed')
                _ensure_accept_tree(execution, record, patch)
            except (OSError, TypeError, ValueError, KeyError, subprocess.SubprocessError) as error:
                raise ValueError('panel acceptance patch evidence invalid for task %s: %s'
                                 % (ident, error)) from error
            prefix.append((pending[commit], dict(id=ident, verify=verify, panel_accept=record)))
            replayed.add((ident, commit))

        for intent in local:
            if id(intent) not in handled_local:
                task_id = intent.get('task') or intent.get('landing_id') or 'unknown'
                raise ValueError('panel acceptance record missing for task ' + str(task_id))

        if local and not recorded:
            intent = local[0]
            task_id = intent.get('task') or intent.get('landing_id') or 'unknown'
            raise ValueError('panel acceptance record missing for task ' + str(task_id))

    for ident, task, record in accepted:
        if not isinstance(record, dict):
            continue
        commit = record.get('commit')
        if commit not in pending or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', str(commit)):
            continue
        if (ident, commit) not in replayed:
            parent = parents.get(commit, '')
            foreign = [row for row in panel_intents if valid_foreign_checkout(row.get('worktree'))
                       and row.get('base_sha') == parent and row.get('task') == ident
                       and row.get('plan_id') == record.get('plan_id')
                       and row.get('lease') == record.get('lease_id')
                       and row.get('patch_sha') == record.get('patch_sha256')]
            if not foreign:
                raise ValueError('panel acceptance landing evidence missing for task ' + ident)
    return [item for _, item in sorted(prefix, key=lambda item: item[0], reverse=True)]


def _accepted_review_snapshot(repo, execution, owner, task_id, verify, record, landed_commit):
    task = _owned_task(repo, execution, owner, task_id)
    finished_retry = (task.get('state') == 'done' and isinstance(task.get('landing'), dict)
                      and task['landing'].get('sha') == landed_commit)
    if ((task.get('state') != 'review' and not finished_retry)
            or task.get('review_lease_id') != task.get('lease_id')
            or record.get('task_id') != task_id or record.get('plan_id') != task.get('plan_id')
            or record.get('lease_id') != task.get('lease_id')):
        raise ValueError('stored acceptance does not match the reviewed task and lease')
    if not verify or verify.replace('\r', '') != task.get('verify', '').replace('\r', ''):
        raise ValueError('--verify does not match the plan task verify contract')
    main_id = os.environ.get('OMS_PANEL_MAIN_ATTEMPT')
    _, _, rows, _, workers, admissions = _finalize_snapshot(repo, owner, task_id, main_id, [])
    if len(workers) != 1 or workers[0].get('task_id') != task_id:
        raise ValueError('accepted task no longer has one unique worker candidate')
    worker = workers[0]
    candidates = [row for row in rows if row.get('kind') == 'delegate' and row.get('task_id') == task_id
                  and row.get('attempt_id') == worker.get('attempt_id') and row.get('patch_sha256')]
    if (len(candidates) != 1 or candidates[0].get('exit') != 0
            or worker.get('parent_attempt_id') != main_id
            or worker.get('refs', {}).get('panel_lease_id') != task.get('lease_id')
            or candidates[0].get('plan_id') != task.get('plan_id')):
        raise ValueError('accepted candidate is ambiguous or no longer matches the task lease')
    candidate = candidates[0]
    gates = _review_preflight(repo, task_id, workers, rows, admissions)
    current = {'task': task, 'candidate': candidate, 'worker': worker, 'review_gate': gates[0]}
    if (candidate.get('event_id') != record.get('candidate_event_id')
            or worker.get('attempt_id') != record.get('candidate_attempt_id')
            or candidate.get('patch_sha256') != record.get('patch_sha256')
            or candidate.get('artifact') != record.get('artifact')
            or candidate.get('patch') != record.get('patch')
            or current['review_gate'] != record.get('review_gate')):
        raise ValueError('accepted candidate or digest-bound review gate changed')
    if hashlib.sha256(verified_patch(repo, candidate)).hexdigest() != record.get('patch_sha256'):
        raise ValueError('accepted candidate patch digest changed')
    return current


def _accept_recovery(snapshot, repo, execution, owner, task_id, verify, message,
                     base, expected_tree, committed):
    task, candidate = snapshot['task'], snapshot['candidate']
    git = 'git --literal-pathspecs -C %s' % shlex.quote(str(execution))
    args = {'repo': str(repo), 'execution': str(execution), 'owner': owner, 'task_id': task_id,
            'verify': verify, 'message': message, 'base': base, 'tree': expected_tree,
            'patch_sha256': candidate['patch_sha256'], 'committed': committed,
            'identity': _accept_identity(snapshot)}
    helper = ('import json,os,sys;sys.path.insert(0,os.path.dirname(sys.argv[1]));'
              'import panel_results;panel_results._accept_recover(json.loads(sys.argv[2]))')
    command = ('python3 -c %s %s %s' % (shlex.quote(helper), shlex.quote(str(ROOT / 'scripts/lib/panel_results.py')),
                                        shlex.quote(json.dumps(args, sort_keys=True, separators=(',', ':')))))
    review = 'oms agent-plan --repo %s review --id %s --lease-id %s --artifact %s --patch %s' % (
        shlex.quote(str(execution)), shlex.quote(task['id']), shlex.quote(task['lease_id']),
        shlex.quote(candidate['artifact']), shlex.quote(candidate['patch']))
    commands = []
    if not committed:
        commands.append(git + ' add -- ' + ' '.join(shlex.quote(path) for path in _patch_paths(verified_patch(repo, candidate))))
    commands.append(command + ' # records ' + review + ' after exact-tree and authority checks')
    return '\n'.join(commands)


def _accept_recover(args):
    """Finish an interrupted accept only after rechecking current authority and exact bytes."""
    repo, execution, owner, task_id = args['repo'], Path(args['execution']), args['owner'], args['task_id']
    with _accept_lock(repo, execution):
        snapshot = _accept_preflight(repo, execution, owner, task_id, args['verify'],
                                     args['patch_sha256'], applied=True)
        task, candidate = snapshot['task'], snapshot['candidate']
        expected_head = args.get('committed') or args['base']
        if (_git(execution, 'rev-parse', 'HEAD').decode().strip() != expected_head
                or _accept_identity(snapshot) != args['identity']
                or hashlib.sha256(verified_patch(repo, candidate)).hexdigest() != args['patch_sha256']):
            raise ValueError('current HEAD, lease or candidate digest differs from the interrupted accept')
        if args.get('committed'):
            commit = args['committed']
            if (_git(execution, 'rev-parse', commit + '^').decode().strip() != args['base']
                    or _git(execution, 'rev-parse', commit + '^{tree}').decode().strip() != args['tree']
                    or _git(execution, 'rev-parse', 'HEAD').decode().strip() != commit
                    or _git(execution, 'write-tree').decode().strip() != args['tree']
                    or _git(execution, 'status', '--porcelain', '--untracked-files=all')):
                raise ValueError('existing commit is not the exact interrupted acceptance')
        else:
            if _git(execution, 'write-tree').decode().strip() != args['tree']:
                raise ValueError('staged tree is not the exact reviewed candidate; recovery refused')
            if (_git(execution, 'diff', '--name-only') or _git(execution, 'ls-files', '--others', '--exclude-standard')):
                raise ValueError('recovery tree has unstaged or unrelated untracked files')
            commit = _exact_commit_tree(execution, args['base'], args['tree'], args['message'],
                authority_check=lambda: _accept_recheck(repo, execution, owner, task_id, args['verify'],
                                                        args['patch_sha256'], snapshot))
        _accept_recheck(repo, execution, owner, task_id, args['verify'], args['patch_sha256'], snapshot)
        record = _accept_record(snapshot, args['base'], args['tree'], commit)
        _plan_command(execution, 'review', '--id', task_id, '--lease-id', task['lease_id'],
                      '--artifact', candidate['artifact'], '--patch', candidate['patch'], panel_accept=record)
        return commit


def _patch_id(execution, patch):
    patch_id = subprocess.run(['git', '-C', str(execution), 'patch-id', '--stable'], input=patch,
                              capture_output=True, check=False, timeout=60)
    if patch_id.returncode:
        raise ValueError('cannot calculate patch identity for acceptance binding')
    rows = patch_id.stdout.decode('ascii', 'replace').splitlines()
    return rows[0].split()[0] if rows and rows[0].split() else ''


def _commit_patch_id(execution, commit):
    parent = _git(execution, 'rev-parse', commit + '^').decode().strip()
    diff = subprocess.run(['git', '-C', str(execution), 'diff', '--binary', '--no-ext-diff',
                           '--no-renames', parent, commit, '--'],
                          stdin=subprocess.DEVNULL, capture_output=True, check=False, timeout=60)
    if diff.returncode:
        raise ValueError('cannot inspect commit patch for acceptance binding')
    return _patch_id(execution, diff.stdout)


def _tree_patch_id(execution, base, tree):
    diff = subprocess.run(['git', '-C', str(execution), 'diff', '--binary', '--no-ext-diff',
                           '--no-renames', base, tree, '--'],
                          stdin=subprocess.DEVNULL, capture_output=True, check=False, timeout=60)
    if diff.returncode:
        raise ValueError('cannot inspect the accepted tree patch for publication binding')
    return _patch_id(execution, diff.stdout)


def _replay_patch_tree(execution, base, patch):
    _owner()
    with tempfile.TemporaryDirectory(prefix='oms-panel-accepted-tree-') as directory:
        patch_path = Path(directory) / 'candidate.patch'
        patch_path.write_bytes(patch)
        index_env = dict(os.environ, GIT_INDEX_FILE=str(Path(directory) / 'index'))
        _git(execution, 'read-tree', base, env=index_env)
        _git(execution, 'apply', '--cached', '--binary', str(patch_path), env=index_env)
        return _git(execution, 'write-tree', env=index_env).decode().strip()


def _ensure_accept_tree(execution, record, patch):
    tree = _replay_patch_tree(execution, record['base'], patch)
    if tree != record['tree']:
        raise ValueError('stored accepted tree no longer matches the reviewed patch')


def _publication_contains_acceptance(execution, landed_commit, record, patch):
    accepted = record['commit']
    _ensure_accept_tree(execution, record, patch)
    accepted_patch = _tree_patch_id(execution, record['base'], record['tree'])
    if not accepted_patch:
        raise ValueError('accepted patch has no patch identity')
    accepted_object = subprocess.run(['git', '-C', str(execution), 'cat-file', '-e', accepted + '^{commit}'],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, check=False, timeout=30)
    if accepted_object.returncode == 0:
        ancestor = subprocess.run(['git', '-C', str(execution), 'merge-base', '--is-ancestor',
                                   accepted, landed_commit], stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False, timeout=60)
        if ancestor.returncode == 0:
            return
        if ancestor.returncode not in (1,):
            raise ValueError('cannot verify accepted commit ancestry in the publication')
    elif accepted_object.returncode != 1:
        raise ValueError('cannot inspect the accepted commit object')
    paths = _patch_paths(patch)
    if not paths:
        raise ValueError('accepted patch has no paths for rebased publication validation')
    git_env = dict(os.environ, GIT_LITERAL_PATHSPECS='1', GIT_NO_LAZY_FETCH='1', GIT_ALLOW_PROTOCOL='')
    history = subprocess.run(['git', '-C', str(execution), 'log', '--no-merges', '--format=%H',
                              landed_commit, '--', *paths], env=git_env, stdin=subprocess.DEVNULL,
                             capture_output=True, check=False, timeout=60)
    if history.returncode:
        raise ValueError('cannot inspect published history for the accepted patch')
    for commit in history.stdout.decode('ascii', 'replace').splitlines():
        if commit:
            try:
                if _commit_patch_id(execution, commit) != accepted_patch:
                    continue
                parent = _git(execution, 'rev-parse', commit + '^').decode().strip()
                candidate_tree = _git(execution, 'rev-parse', commit + '^{tree}').decode().strip()
                if _replay_patch_tree(execution, parent, patch) == candidate_tree:
                    return
            except (OSError, ValueError, subprocess.SubprocessError):
                # A shallow or incomplete local history cannot prove this commit exact.
                continue
    raise ValueError('published SHA does not contain the accepted commit or an exact replay of the reviewed patch')


def _validate_landed_acceptance(repo, execution, owner, task_id, verify, landed_commit):
    task = _owned_task(repo, execution, owner, task_id)
    record = task.get('panel_accept')
    required = {'schema', 'task_id', 'plan_id', 'lease_id', 'candidate_event_id',
                'candidate_attempt_id', 'patch_sha256', 'artifact', 'patch', 'commit', 'tree', 'base',
                'review_gate'}
    if (not isinstance(record, dict) or set(record) != required or type(record.get('schema')) is not int
            or record.get('schema') != 1
            or record.get('task_id') != task_id or record.get('plan_id') != task.get('plan_id')
            or record.get('lease_id') != task.get('lease_id')
            or task.get('review_lease_id') != task.get('lease_id')):
        raise ValueError('task has no exact panel acceptance bound to its current lease')
    snapshot = _accepted_review_snapshot(repo, execution, owner, task_id, verify, record, landed_commit)
    commit = record['commit']
    if (any(not isinstance(record.get(key), str) for key in ('commit', 'tree', 'base'))
            or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit)
            or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', record['tree'])
            or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', record['base'])):
        raise ValueError('stored panel acceptance has malformed commit identity')
    patch = verified_patch(repo, snapshot['candidate'])
    accepted_object = subprocess.run(['git', '-C', str(execution), 'cat-file', '-e', commit + '^{commit}'],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, check=False, timeout=30)
    if accepted_object.returncode == 0:
        if (_git(execution, 'rev-parse', commit + '^').decode().strip() != record['base']
                or _git(execution, 'rev-parse', commit + '^{tree}').decode().strip() != record['tree']):
            raise ValueError('stored accepted commit no longer matches its recorded tree and base')
    elif accepted_object.returncode != 1:
        raise ValueError('cannot inspect the accepted commit object')
    _publication_contains_acceptance(execution, landed_commit, record, patch)
    return record


def accept(repo, owner, task_id, message, verify, expected_patch_sha256=None, execution_repo=None):
    """Apply one reviewed candidate and commit locally; publication remains explicit."""
    _owner()
    execution = _execution_repo(repo, execution_repo)
    if not message or not message.strip() or '\0' in message:
        raise ValueError('accept requires a nonblank commit message')
    if expected_patch_sha256 is not None and not re.fullmatch(r'[0-9a-f]{64}', expected_patch_sha256):
        raise ValueError('expected patch digest must be a lowercase SHA-256')
    with _accept_lock(repo, execution):
        base = _git(execution, 'rev-parse', 'HEAD').decode().strip()
        if _git(execution, 'status', '--porcelain', '--untracked-files=all'):
            raise ValueError('dirty integration tree; accept refused before application')
        snapshot = _accept_preflight(repo, execution, owner, task_id, verify, expected_patch_sha256)
        candidate = snapshot['candidate']
        stage, committed = 'application', None
        with tempfile.TemporaryDirectory(prefix='oms-panel-accept-') as directory:
            patch_path = Path(directory) / 'candidate.patch'
            patch_path.write_bytes(verified_patch(repo, candidate))
            index_env = dict(os.environ, GIT_INDEX_FILE=str(Path(directory) / 'index'))
            _git(execution, 'read-tree', base, env=index_env)
            _git(execution, 'apply', '--cached', '--binary', str(patch_path), env=index_env)
            expected_tree = _git(execution, 'write-tree', env=index_env)
            try:
                result = subprocess.run(['bash', str(ROOT / 'scripts/patch-land.sh'), '--repo', str(execution),
                    '--patch', str(patch_path), '--verify', verify, '--panel-accept',
                    json.dumps(dict(snapshot=snapshot, owner=owner, task_id=task_id, base=base))],
                    stdin=subprocess.DEVNULL, stdout=sys.stderr, check=False)
                if result.returncode:
                    raise ValueError('patch-land failed; use existing patch-land --recover for incomplete receipts')
                stage = 'commit'
                _accept_recheck(repo, execution, owner, task_id, verify, candidate['patch_sha256'], snapshot)
                if _git(execution, 'rev-parse', 'HEAD').decode().strip() != base:
                    raise ValueError('HEAD changed after application')
                _git(execution, '--literal-pathspecs', 'add', '--all', '--', *_patch_paths(patch_path))
                if _git(execution, 'write-tree') != expected_tree or _git(execution, 'diff', '--name-only'):
                    raise ValueError('applied/index changes differ from the reviewed patch')
                if _git(execution, 'ls-files', '--others', '--exclude-standard'):
                    raise ValueError('integration tree gained unrelated untracked files')
                try:
                    committed = _exact_commit_tree(execution, base, expected_tree.decode().strip(), message,
                        authority_check=lambda: _accept_recheck(repo, execution, owner, task_id, verify,
                                                                candidate['patch_sha256'], snapshot))
                except _ExactCommitPublished as error:
                    committed = error.sha
                    raise
                stage = 'plan review'
                if (_git(execution, 'rev-parse', 'HEAD^').decode().strip() != base
                        or _git(execution, 'rev-parse', 'HEAD^{tree}').strip() != expected_tree.strip()
                        or _git(execution, 'status', '--porcelain', '--untracked-files=all')):
                    raise ValueError('committed tree or HEAD differs from the reviewed application')
                _accept_recheck(repo, execution, owner, task_id, verify, candidate['patch_sha256'], snapshot)
                record = _accept_record(snapshot, base, expected_tree.decode().strip(), committed)
                try:
                    _plan_command(execution, 'review', '--id', task_id, '--lease-id', snapshot['task']['lease_id'],
                                  '--artifact', candidate['artifact'], '--patch', candidate['patch'],
                                  panel_accept=record)
                except (OSError, ValueError, subprocess.SubprocessError) as error:
                    raise _AcceptAuthorityChanged('plan review compare-and-set failed: %s' % error) from error
            except _AcceptAuthorityChanged as error:
                head = committed or _git(execution, 'rev-parse', 'HEAD').decode().strip()
                recovery = _accept_recovery(snapshot, repo, execution, owner, task_id, verify, message,
                                            base, expected_tree.decode().strip(), committed)
                raise ValueError('accept %s failed; HEAD %s; authority changed after application for reviewed '
                                 'patch SHA %s: %s\n'
                                 'Recovery after confirming the reviewed bytes are still present; do not reapply:\n%s'
                                 % (stage, head, candidate['patch_sha256'], error, recovery)) from error
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                head = committed or _git(execution, 'rev-parse', 'HEAD').decode().strip()
                raise ValueError('accept %s failed; HEAD %s; preserve state, never reapply blindly: %s'
                                 % (stage, head, error)) from error
        return {'sha': committed, 'task_id': task_id, 'patch_sha256': candidate['patch_sha256']}


def _resolve_failed_outcomes(repo, owner, task_id, relative, outcome, revision):
    """Settle this task's failed outcome rows recorded before its result row."""
    _, index = _records(repo, task_id=task_id)
    boundary = max((i for i, r in enumerate(index) if r.get('kind') == 'panel-result'
                    and r.get('task_id') == task_id and r.get('artifact') == relative), default=None)
    if boundary is None:
        return
    attempts = _query(repo, 'agent-events', 'list')
    linked = {a.get('attempt_id') for a in attempts if a.get('task_id') == task_id}
    resolved = {r.get('resolves_event_id') for r in index if r.get('kind') == 'artifact-resolution'}
    ids = [r['event_id'] for r in index[:boundary]
           if isinstance(r.get('event_id'), str) and r['event_id'] not in resolved
           and (r.get('task_id') == task_id or (not r.get('task_id') and r.get('attempt_id') in linked))
           and r.get('kind') not in ('artifact-resolution', 'panel-result')
           and type(r.get('exit')) is int and r['exit'] != 0]
    reason = 'owner finalize %s: result revision %s' % (outcome, revision)
    for start in range(0, len(ids), 100):
        command = ['bash', str(ROOT / 'scripts/artifact-index.sh'), '--repo', str(repo), 'resolve', '--reason', reason]
        for event_id in ids[start:start + 100]:
            command += ['--event-id', event_id]
        result = subprocess.run(command, capture_output=True, text=True, timeout=60, check=False,
                                stdin=subprocess.DEVNULL, env=dict(os.environ, OMS_AGENT=owner))
        if result.returncode:
            raise ValueError('artifact-index resolve failed (exit %s): %s' % (result.returncode, result.stderr.strip()))


def _finalize_result(repo, owner, task_id, summary, refs, outcome, verify, notify, verify_timeout, execution):
    main_id = os.environ.get('OMS_PANEL_MAIN_ATTEMPT')
    with _lock(repo):
        evidence_refs = [dict(ref) for ref in refs]
        snapshot, attempts, rows, related, workers, admissions = _finalize_snapshot(
            repo, owner, task_id, main_id, evidence_refs)
        binding = snapshot['binding']
        if outcome == 'accepted':
            _review_preflight(repo, task_id, workers, rows, admissions)
        if outcome == 'accepted' and (not verify or not refs):
            raise ValueError('accepted requires an evidence reference and a passing verifier')
        _finalize_owns_task(repo, owner, task_id, main_id, attempts, binding)
        if outcome == 'accepted':
            for admission in admissions:
                if not admission:
                    continue
                path = admission.get('patch')
                try:
                    if (isinstance(path, str) and path.startswith('.oms/landing-patches/')
                            and path.count('/') == 2 and path.endswith('.patch')):
                        # Landing publications are hard links owned by the capture store.
                        data = runpy.run_path(str(ROOT / 'scripts/lib/landing-capture.py'))['read_patch'](
                            str(repo), path, PATCH_MAX)
                    else:
                        if _relative(repo, path, PATCH_MAX)['sha256'] != admission.get('patch_sha256'):
                            continue
                        data = _read(repo, path, PATCH_MAX)
                    if hashlib.sha256(data).hexdigest() != admission.get('patch_sha256'):
                        continue
                    check = subprocess.run(['git', 'apply', '--check'], cwd=str(execution), input=data,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                           check=False, timeout=60)
                except (OSError, ValueError, SystemExit, subprocess.SubprocessError):
                    continue
                if check.returncode == 0:
                    raise ValueError('landed patch %s applies cleanly to the verifier checkout; finalize from '
                                     'the checkout containing the landing (--repo WORKTREE) or after landing and syncing'
                                     % path)
    verification = {'status': 'not_run'}
    if verify:
        # Output remains local; a verifier transcript is never exported as a summary.
        verified = (_run_verifier(repo, verify, verify_timeout) if execution == repo
                    else _run_verifier(execution, verify, verify_timeout, evidence_repo=repo))
        verify_exit = verified['exit']
        verification = {'status': 'passed' if verify_exit == 0 else 'failed',
                        'exit': verify_exit, 'command_sha256': hashlib.sha256(verify.encode()).hexdigest()}
        if verified.get('diagnostics'):
            verification['diagnostics'] = verified['diagnostics']
        if outcome == 'accepted' and verify_exit:
            raise ValueError('accepted verifier failed (exit %s); private diagnostics: %s'
                             % (verify_exit, verified['diagnostics']['path']))
    with _lock(repo):
        current_evidence = [_relative(repo, ref['path']) for ref in refs]
        current, attempts, rows, related, workers, admissions = _finalize_snapshot(
            repo, owner, task_id, main_id, current_evidence)
        _finalize_owns_task(repo, owner, task_id, main_id, attempts, current['binding'])
        for field, message in (
                ('binding', 'plan task changed during result verification'),
                ('evidence', 'evidence changed during verification'),
                ('admissions', 'admission evidence changed during verification'),
                ('superseded_attempts', 'supersession evidence changed during verification'),
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
                   'superseded_attempts': current['superseded_attempts'],
                   # This display window also enters the revision. Settlement
                   # changes updated_at, so retain the latest-created attempts.
                   'attempt_ids': [a['attempt_id'] for a in sorted(related, key=lambda a: (
                       a.get('created_at', ''), a['attempt_id']))[-20:]]}
        if binding:
            payload['plan_binding'] = binding
        payload['revision'] = hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()[:24]
        relative = '.oms/artifacts/panel-results/result-%s-%s.json' % (task_id, payload['revision'])
        path = _write(repo, relative, payload)
        if not any(r.get('kind') == 'panel-result' and r.get('artifact') == relative for r in rows):
            _register(repo, owner, task_id, path)
        try:
            from review_settlement import after_finalize
            settlement = after_finalize(repo, payload)
        except Exception as error:
            settlement = {'candidates': [], 'pending': [], 'applied': [], 'executes': True,
                          'errors': [{'error': clean(str(error), 240)}]}
        for error in settlement.get('errors', []):
            print('OMS review settlement error: ' + clean(str(error), 300), file=sys.stderr)
        if settlement.get('pending'):
            print('OMS review settlement: %d worker(s) remain pending' % len(settlement['pending']), file=sys.stderr)
        try:
            _resolve_failed_outcomes(repo, owner, task_id, relative, outcome, payload['revision'])
        except Exception as error:
            print('OMS finalize artifact resolution error: ' + clean(str(error), 300), file=sys.stderr)
        delivery = _deliver(repo, payload, relative, rows) if notify else _last_delivery(repo, rows, task_id, payload['revision'])
        if outcome == 'accepted':
            _plan_hint(repo, task_id, any(current['admissions']))
        return dict(payload, artifact=relative, delivery=delivery, settlement=settlement)


def finalize(repo, owner, task_id, summary_file, outcome, verify=None, evidence=None, notify=False,
             verify_timeout=1800, execution_repo=None, landed_commit=None):
    _owner()
    if owner not in MAIN_OWNERS or not ID.fullmatch(task_id) or outcome not in ('completed', 'accepted', 'failed', 'rejected'):
        raise ValueError('invalid owner result')
    if landed_commit is not None and outcome not in ('accepted', 'completed'):
        raise ValueError('--landed-commit requires --outcome accepted or completed')
    if isinstance(verify_timeout, bool) or not isinstance(verify_timeout, int) or not 1 <= verify_timeout <= 86400:
        raise ValueError('verify timeout must be between 1 and 86400 seconds')
    execution_error = None
    try:
        execution = _execution_repo(repo, execution_repo)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        execution, execution_error = None, error
    live_reviews = _live_review_read_passes(repo, execution)
    if live_reviews:
        # The verifier may write its checkout; wait for conflicting readers
        # here, before any side effect.
        print('waiting up to %ds for %d review read passes (%s)'
              % (REVIEW_WAIT_SECONDS, len(live_reviews), ', '.join(live_reviews)), file=sys.stderr)
        deadline = time.monotonic() + REVIEW_WAIT_SECONDS
        while live_reviews and time.monotonic() < deadline:
            time.sleep(min(1.0, max(0.0, deadline - time.monotonic())))
            live_reviews = _live_review_read_passes(repo, execution)
    if live_reviews:
        print('deferred: %d review read passes running (%s); finalize again after they finish'
              % (len(live_reviews), ', '.join(live_reviews)), file=sys.stderr)
        raise SystemExit(75)
    summary = _safe_summary(summary_file, repo)
    refs = [_relative(repo, value) for value in (evidence or [])] if not isinstance(evidence, str) else [_relative(repo, evidence)]
    if len(refs) > 8:
        raise ValueError('use at most eight evidence references')
    if execution_error is not None:
        raise execution_error
    if landed_commit is not None:
        if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', landed_commit):
            raise ValueError('--landed-commit must be a full lowercase commit SHA')
        with _accept_lock(repo, execution):
            task = _owned_task(repo, execution, owner, task_id)
            _validate_landed_acceptance(repo, execution, owner, task_id, verify, landed_commit)
            try:
                _plan_command(execution, 'finish', '--id', task_id, '--lease-id', task['lease_id'],
                              '--landed-commit', landed_commit)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                raise ValueError('finish failed; SHA %s; retry finish only after validating completion: %s'
                                 % (landed_commit, error)) from error
    try:
        result = _finalize_result(repo, owner, task_id, summary, refs, outcome, verify, notify,
                                  verify_timeout, execution)
        try:
            first_line = summary.splitlines()[0].strip() if summary.splitlines() else ""
            journal_summary = 'Task %s %s: %s' % (task_id, outcome, first_line[:120])
            if landed_commit:
                journal_summary += ' (landed commit %s)' % landed_commit[:7]
            verification_status = result.get('verification', {}).get('status')
            if verification_status not in ('passed', 'failed'):
                verification_status = 'not_verified'
            subprocess.Popen(
                ['bash', '-c',
                 '. "$1"; work_journal_observe "$2" panel-result "$3" '
                 '--source-id "$4" --outcome "$5" --outcome-status "$6" '
                 '--verification-status "$7"',
                 'oms-panel-result-capture',
                 str(ROOT / 'scripts' / 'lib' / 'work-journal.sh'), str(repo),
                 str(repo / result['artifact']),
                 str(task_id) + ':' + str(result.get('revision', '')),
                 journal_summary, outcome, verification_status],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, close_fds=True)
        except (OSError, ValueError, KeyError, subprocess.SubprocessError):
            pass
        return result
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        if landed_commit is not None:
            raise ValueError('finalization failed after plan finish; SHA %s; retry only --finalize after '
                             'confirming exact completion: %s' % (landed_commit, error)) from error
        raise


def retry_delivery(repo, task_id, revision=None):
    _owner()
    if not ID.fullmatch(task_id) or (revision and not REVISION.fullmatch(revision)):
        raise ValueError('invalid saved result ID')
    with _lock(repo):
        _, rows = _records(repo, full=True, task_id=task_id)
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
    bindings = _plan_bindings(repo, wanted)
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
                result = _load_result(repo, result_rows[-1])
                # Only a same-named task of the current plan supersedes the recorded generation; tasks the
                # current plan no longer holds keep their decisions after a retire.
                if (result.get('plan_binding') is not None and bindings.get(ident) is not None
                        and result.get('plan_binding') != bindings.get(ident)):
                    continue
                found[ident] = result.get('outcome')
            except (OSError, ValueError, KeyError, TypeError):
                pass
    return found


SHA256 = re.compile(r'[0-9a-f]{64}')
EMPTY_SHA256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'


def pending_admissions(repo, task_ids, _tick=None):
    """Recorded patches without admission; capped evidence stays conservatively pending.

    This is an inbox hint, not verified patch or acceptance evidence. No artifact
    bytes are read. At the latest-300-attempt or possible index-retention
    boundary (1000 events), a task with
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


def settled_pending_admissions(repo, task_ids, _tick=None, _outcomes=None):
    """pending_admissions minus tasks the owner already finalized.

    A main admitting in a clone leaves no patch-admit row here, so the owner's decision settles the hint."""
    from room_view import DECIDED
    pending = pending_admissions(repo, task_ids, _tick=_tick)
    if pending:
        pending -= {ident for ident, outcome in (_outcomes or outcomes)(repo, pending, _shared=True, _tick=_tick).items()
                    if outcome in DECIDED | {'failed'}}
    return pending


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
        attempts, rows = _records(repo, full=bool(task_id), task_id=task_id)
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
        # Select within this participant's own tasks before the 20-task cap. A task
        # without an attempt of this participant (another call's, or a main's whole-main
        # result) is never adopted: no recorded field ties it to this call.
        own = {a.get('task_id') for a in attempts
               if a.get('refs', {}).get('panel_room_participant') == room_participant}
        ids = [i for i in ids if i in own]
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
            # A whole-main result adopts only unassigned children of its main.
            matching = (metadata_attempts or
                        [a for a in attempts if a.get('parent_attempt_id') == result['main_attempt_id']
                         and not a.get('task_id')])
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
        reason_code = last.get('reason_code')
        terminal_failure = last.get('state') in TERMINAL_FAILURE_STATES
        termination_reason = (_reason_phrase(reason_code)
                              if terminal_failure and reason_code else None)
        calls = []
        for row in indexed[-30:]:
            if row.get('kind') not in ('ask', 'call', 'consult', 'delegate', 'review', 'ask-synthesis', 'patch-admit', 'patch-land'):
                continue
            call = {k: row[k] for k in ('kind', 'exit', 'verify_exit', 'verify_status', 'state', 'reason_code', 'guard',
                                       'provider_error_exit', 'artifact', 'patch', 'patch_sha256', 'event_id',
                                       'selected_model', 'requested_model', 'served_model', 'attempt_id', 'tokens',
                                       'provider_usage') if k in row}
            attempt_meta = next((a for a in matching if a.get('attempt_id') == row.get('attempt_id')), {})
            alias = attempt_meta.get('refs', {}).get('panel_alias')
            if alias:
                call['alias'] = alias
            if (task_id or room_participant) and row.get('artifact') and row.get('kind') in ('ask', 'call', 'consult', 'delegate', 'review'):
                try:
                    verified_bytes = _indexed(repo, row, 1024 * 1024)
                    answer = (_final_attempt_output(repo, row['artifact'])
                              if attempt_meta.get('refs', {}).get('panel_fallback') else None)
                    if answer is None:
                        answer, _ = artifact_sections_stream(io.BytesIO(verified_bytes))
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
                except (OSError, ValueError, TypeError, RuntimeError):
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
        superseded_ids = {item['attempt_id'] for item in (result or {}).get('superseded_attempts', [])}
        calls_by_stage = {'implement': [], 'verify': [], 'review': [], 'main_decision': []}
        for call in calls:
            if call.get('attempt_id') in superseded_ids:
                call['superseded'] = True
                continue
            if call.get('verify_exit') is not None:
                calls_by_stage['verify'].append(dict(call, stage_exit=call.get('verify_exit')))
            stage = ('review' if call.get('kind') == 'review' else
                     'implement' if call.get('kind') in ('delegate', 'call', 'ask', 'consult') else None)
            if stage:
                calls_by_stage[stage].append(call)
        review_records = [(position, row) for position, row in enumerate(indexed)
                          if row.get('kind') in ('review', 'review-outcome')
                          and row.get('attempt_id') not in superseded_ids]
        review_state = 'not_run'
        if review_records:
            latest_run = _review_run_id(max(
                review_records, key=lambda item: (_review_run_order(_review_run_id(item[1])), item[0]))[1])
            selected_review = [row for _, row in review_records if _review_run_id(row) == latest_run]
            outcomes = [row for row in selected_review if row.get('kind') == 'review-outcome'
                        and isinstance(row.get('review'), dict)]
            if outcomes:
                outcome = outcomes[-1]
                review = outcome['review']
                overall = review.get('overall')
                if overall == 2:
                    review_state = 'inconclusive'
                elif outcome.get('exit') not in (None, 0) or review.get('gate_verify_exit') not in (None, 0) or overall == 1:
                    review_state = 'failed'
                elif outcome.get('exit') == 0 and review.get('gate_verify_exit') in (None, 0) and overall == 0:
                    review_state = 'passed'
                else:
                    review_state = 'inconclusive'
            else:
                review_state = ('failed' if any(row.get('exit') not in (None, 0)
                                                for row in selected_review) else 'inconclusive')
        stages = {}
        for name, values in calls_by_stage.items():
            if name == 'review':
                stage_state = review_state
                stage_values = values
            else:
                stage_state = ('failed' if any(c.get('stage_exit', c.get('exit')) not in (None, 0)
                                               for c in values) else 'passed') if values else 'not_run'
                stage_values = values
            stages[name] = {'state': stage_state,
                            'attempt_id': (stage_values[-1].get('attempt_id') if stage_values else None),
                            'exit': (stage_values[-1].get('stage_exit', stage_values[-1].get('exit'))
                                     if stage_values else None)}
        main_state = (result or {}).get('outcome') or 'pending'
        stages['main_decision'] = {'state': 'failed' if main_state == 'failed' else main_state}
        failed_stage = next((name for name in ('implement', 'verify', 'review', 'main_decision')
                             if stages[name]['state'] == 'failed'), None)
        failed_call = next((c for c in calls_by_stage.get(failed_stage, [])
                            if c.get('stage_exit', c.get('exit')) not in (None, 0)), {}) if failed_stage else {}
        ending = call_ending(failed_call) if failed_call and failed_stage != 'review' else None
        guard = failed_call.get('guard')
        guard_name = clean(guard, 80) if isinstance(guard, str) else ''
        if ending == 'cancelled':
            representative = failed_stage + ' cancelled'
            if failed_stage != 'review':
                stages[failed_stage]['state'] = 'cancelled'
            action = 'none'
        elif ending == 'guard_stop':
            representative = failed_stage + ' guard stop' + (' (' + guard_name + ')' if guard_name else '')
            if failed_stage != 'review':
                stages[failed_stage]['state'] = representative[len(failed_stage) + 1:]
            action = 'inspect guard stop and salvage partial work'
        elif ending == 'timed_out':
            representative = 'timed out'
            if failed_stage != 'review':
                stages[failed_stage]['state'] = 'timed out'
            action = 'continue with --continue TASK_ID'
        elif ending == 'verify_failed':
            representative, action = 'verify failed', 'retry verify'
            stages[failed_stage]['state'] = 'verify failed'
        elif ending == 'verify_timed_out':
            representative, action = 'verify timed out', 'retry verify'
            stages[failed_stage]['state'] = 'verify timed out'
        elif ending == 'provider_error':
            representative, action = failed_stage + ' provider error', 'retry ' + failed_stage
            if failed_stage != 'review':
                stages[failed_stage]['state'] = 'provider error'
        elif failed_stage:
            representative, action = failed_stage + ' failed', 'retry ' + failed_stage
        elif stages['review']['state'] == 'inconclusive' and main_state == 'pending':
            representative, action = 'review inconclusive', 'retry review'
        else:
            representative = stages['main_decision']['state']
            action = ('review main decision' if failed_stage == 'main_decision' else
                      'record main decision' if stages['main_decision']['state'] == 'pending' else 'none')
        if result:
            stages['main_decision']['state'] = main_state
            representative = 'main_decision ' + main_state
            action = 'none'
        if (not result and not failed_stage and last.get('state') == 'review'
                and metadata.get('panel_purpose') == 'investigate' and metadata.get('panel_access') == 'write'):
            findings = next((c for c in reversed(calls) if c.get('kind') == 'delegate'), {})
            try:
                if findings.get('exit') == 0 and findings.get('verify_exit') == 0 and verified_patch(repo, findings) == b'':
                    representative, action = 'findings awaiting owner judgment', 'record main decision'
            except (OSError, ValueError, TypeError):
                representative, action = 'patch evidence unavailable', 'inspect result evidence'
        if termination_reason and not result and not failed_stage and stages['review']['state'] != 'inconclusive':
            representative = termination_reason
            action = {
                'cancelled': 'salvage or re-dispatch',
                'failed': 'inspect failure and retry',
                'abandoned': 'salvage or re-dispatch',
                'timed_out': 'continue with --continue TASK_ID',
            }[last['state']]
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
                       'stages': stages, 'representative_state': representative, 'next_action': action,
                       'termination_reason': termination_reason})
    return {'schema': 1, 'kind': 'oms-panel-results', 'rows': output,
            'coverage': ('all retained attempts' if task_id and not (_records_data or _tick or _shared)
                         else 'latest 300 attempts; attempt coverage may be incomplete')
                        + '; all retained artifact events; previously pruned evidence unavailable',
            'truncated': capped or (bool(task_id) and not chosen), 'offset': offset,
            # A capped page knows only that more rows follow, not how many survive the scope checks.
            'omitted': None if capped else 0,
            'omitted_range': ([offset + len(output), None] if capped else None)}


def results_snapshot(repo, task_id=None, room_participant=None):
    """Collect retained result records once for a reader that also needs their event index."""
    records = _records(repo, full=bool(task_id), task_id=task_id)
    report = results(repo, task_id, room_participant, _records_data=records)
    return report, records
