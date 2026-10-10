"""Parent-reviewed local completions; legacy landing remains a separate contract."""
import datetime
import contextlib
import hashlib
import json
import math
import os
from pathlib import Path
import re
import runpy
import stat
import shlex
import threading
import subprocess
import sys
import unicodedata

HERE = Path(__file__).resolve().parent
DURABLE = runpy.run_path(str(HERE / 'durable-jsonl.py'))
RECEIPT = runpy.run_path(str(HERE / 'plan-receipt.py'))
SCOPE = runpy.run_path(str(HERE / 'path_scope.py'))
MAX_JSON = 4 * 1024 * 1024
KINDS = {'accept-research': 'research-accepted', 'satisfy': 'satisfied-by'}
HEX = re.compile(r'[0-9a-f]{64}\Z')
_STREAMS = {}
_ATTEMPTS = {}
_COMMIT_PROOF = None
_HISTORICAL = {}
_PREFIXES = {}
_PREFIX_ATTEMPTS = {}
_ROOM_STATES = {}
MAX_STREAM = 16 * 1024 * 1024
MAX_PREFIX_CACHE = 64 * 1024 * 1024


def fail(message):
    raise ValueError('completion: ' + message)


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def encoded(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=False,
                       separators=(',', ':'), allow_nan=False) + '\n').encode('utf-8')


def duplicate(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            fail('duplicate JSON key')
        result[key] = value
    return result


def constant(value):
    fail('nonfinite JSON')


def parsed(raw):
    value = json.loads(raw.decode('utf-8') if isinstance(raw, bytes) else raw,
                       object_pairs_hook=duplicate, parse_constant=constant)
    def finite(item):
        if isinstance(item, float) and not math.isfinite(item):
            fail('nonfinite JSON')
        if isinstance(item, dict):
            for val in item.values():
                finite(val)
        elif isinstance(item, list):
            for val in item:
                finite(val)
    finite(value)
    return value


def keys(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        fail('malformed or unsupported object fields: ' + ','.join(required))


def text(value, label, maximum=1000):
    if (not isinstance(value, str) or not value.strip() or len(value) > maximum or
            any(unicodedata.category(c) in ('Cc', 'Cf', 'Cs') for c in value)):
        fail('invalid ' + label)
    return value


def digest(value):
    if not isinstance(value, str) or not HEX.fullmatch(value):
        fail('invalid SHA-256')
    return value


def relative(value):
    text(value, 'repo-relative path', 300)
    if (value.startswith(('/', '\\')) or '\\' in value or ':' in value or
            any(p in ('', '.', '..') for p in value.split('/'))):
        fail('evidence paths must be normalized repo-relative paths')
    return value


def legacy_relative(repo, value):
    # Existing patch-land intents store physical absolute paths. Resolve only
    # this historical spelling through the strict same-repository reader;
    # every new evidence reference remains repository-relative.
    if not isinstance(value, str):
        fail('invalid historical evidence path')
    if os.path.isabs(value):
        target = DURABLE['canonical_repo_path'](repo, value, 'historical landing evidence')
        value = os.path.relpath(target, repo).replace(os.sep, '/')
    return relative(value)


def read(repo, rel, maximum=MAX_JSON, missing=False):
    return DURABLE['read_no_follow'](repo, relative(rel), 'completion evidence',
                                    max_bytes=maximum, missing_ok=missing)


def reference(repo, value, maximum=MAX_JSON):
    keys(value, ('path', 'sha256'))
    raw = read(repo, value['path'], maximum)
    if sha(raw) != digest(value['sha256']):
        fail('evidence digest changed: ' + value['path'])
    return raw


def rows(repo, rel):
    key = (repo, rel)
    if key in _HISTORICAL:
        return _HISTORICAL[key]
    if key in _STREAMS:
        return _STREAMS[key]
    raw = read(repo, rel, MAX_STREAM)
    result = stream_rows(raw)
    _STREAMS[key] = (result, sha(raw))
    return _STREAMS[key]


def stream_rows(raw):
    if not raw.endswith(b'\n'):
        fail('incomplete evidence stream')
    result = []
    for line in raw.splitlines():
        if not line or len(line) > 1024 * 1024:
            fail('invalid evidence row size')
        item = parsed(line)
        if not isinstance(item, dict):
            fail('evidence row must be an object')
        result.append(item)
    return result


def immutable_object(repo, directory, raw, suffix, maximum):
    ref = {'path': directory + '/' + sha(raw) + suffix, 'sha256': sha(raw)}
    def unchanged(old):
        if old and old != raw:
            fail('conflicting immutable completion object')
        return raw
    DURABLE['atomic_mutate'](ref['path'], unchanged, repo, missing_ok=True,
                             create_parent=True, max_input=maximum, max_output=maximum,
                             label='completion provenance')
    return ref


def capture_prefixes(repo, room):
    # Called only inside the real thread and lifecycle writer locks. Each
    # no-follow reader consumes the complete EOF, never a caller-picked cutoff.
    proof = {'schema': 1, 'repository_sha256': namespace(repo), 'room_id': room}
    for kind, path in (('room', '.oms/threads/' + room + '.jsonl'),
                       ('lifecycle', '.oms/lifecycle/events.jsonl')):
        raw = read(repo, path, MAX_STREAM)
        records = stream_rows(raw)
        proof[kind] = dict(immutable_object(repo, '.oms/plan/completion-prefixes', raw,
                                           '.jsonl', MAX_STREAM),
                           schema=1, kind=kind, source=path, bytes=len(raw), rows=len(records))
    # This separate immutable capture binds the references to the trusted EOF
    # capture. Rehashing a shortened stream/ref cannot replace its checkpoint.
    capture = immutable_object(repo, '.oms/plan/completion-captures', encoded(proof), '.json', MAX_JSON)
    proof['capture'] = capture
    load_prefixes(repo, proof)
    return proof


def room_records(records, room, content_sha=None):
    key = (room, content_sha)
    if content_sha is not None and key in _ROOM_STATES:
        return _ROOM_STATES[key]
    for sequence, row in enumerate(records, 1):
        if (type(row.get('schema')) is not int or row['schema'] != 1 or row.get('thread') != room or
                type(row.get('seq')) is not int or row['seq'] != sequence):
            fail('malformed native room history')
        stamp = row.get('ts', '')
        if (not isinstance(stamp, str) or
                datetime.datetime.strptime(stamp, '%Y-%m-%dT%H:%M:%SZ').strftime('%Y-%m-%dT%H:%M:%SZ') != stamp):
            fail('noncanonical native room timestamp')
    state = runpy.run_path(str(HERE / 'room.py'))['project'](records)
    if state['id'] != room:
        fail('foreign native room history')
    if content_sha is not None:
        _ROOM_STATES[key] = state
    return state


def load_prefixes(repo, proof):
    keys(proof, ('schema', 'repository_sha256', 'room_id', 'room', 'lifecycle', 'capture'))
    if (type(proof['schema']) is not int or proof['schema'] != 1 or
            proof['repository_sha256'] != namespace(repo) or
            not isinstance(proof['room_id'], str) or not re.fullmatch(r'room-[0-9a-f]{12}', proof['room_id'])):
        fail('unsupported or foreign captured provenance')
    capture = proof['capture']
    keys(capture, ('path', 'sha256'))
    if capture['path'] != '.oms/plan/completion-captures/' + digest(capture['sha256']) + '.json':
        fail('noncanonical provenance capture')
    captured = parsed(reference(repo, capture))
    if captured != {key: value for key, value in proof.items() if key != 'capture'}:
        fail('prefix references differ from trusted complete capture')
    needed = 0
    for kind in ('room', 'lifecycle'):
        item = proof[kind]
        keys(item, ('schema', 'kind', 'source', 'path', 'sha256', 'bytes', 'rows'))
        if type(item['bytes']) is not int or not 0 < item['bytes'] <= MAX_STREAM:
            fail('invalid captured stream size')
        if (repo, item['path'], item['sha256']) not in _PREFIXES:
            needed += item['bytes']
    if sum(len(value[0]) for value in _PREFIXES.values()) + needed > MAX_PREFIX_CACHE:
        _PREFIXES.clear()
        _PREFIX_ATTEMPTS.clear()
        _ROOM_STATES.clear()
    result = {}
    for kind in ('room', 'lifecycle'):
        item = proof[kind]
        keys(item, ('schema', 'kind', 'source', 'path', 'sha256', 'bytes', 'rows'))
        expected = '.oms/threads/' + proof['room_id'] + '.jsonl' if kind == 'room' else '.oms/lifecycle/events.jsonl'
        if (type(item['schema']) is not int or item['schema'] != 1 or item['kind'] != kind or
                item['source'] != expected or
                item['path'] != '.oms/plan/completion-prefixes/' + digest(item['sha256']) + '.jsonl' or
                type(item['bytes']) is not int or not 0 < item['bytes'] <= MAX_STREAM or
                type(item['rows']) is not int or not 0 < item['rows'] <= MAX_STREAM):
            fail('invalid captured stream boundary or identity')
        key = (repo, item['path'], item['sha256'])
        if key not in _PREFIXES:
            raw = reference(repo, {'path': item['path'], 'sha256': item['sha256']}, MAX_STREAM)
            _PREFIXES[key] = (raw, stream_rows(raw))
        raw, records = _PREFIXES[key]
        if len(raw) != item['bytes'] or len(records) != item['rows']:
            fail('captured byte/row boundary mismatch')
        if kind == 'room':
            room_records(records, proof['room_id'], item['sha256'])
        elif key not in _PREFIX_ATTEMPTS:
            _PREFIX_ATTEMPTS[key] = runpy.run_path(str(HERE / 'agent-events.py'))['project_attempts'](records)
        result[(repo, expected)] = (records, item['sha256'])
    return result


@contextlib.contextmanager
def historical_prefixes(repo, proof):
    global _HISTORICAL, _ATTEMPTS
    saved, attempts = _HISTORICAL, _ATTEMPTS
    _HISTORICAL = load_prefixes(repo, proof)
    item = proof['lifecycle']
    _ATTEMPTS = {repo: (_HISTORICAL[(repo, item['source'])][0],
                       _PREFIX_ATTEMPTS[(repo, item['path'], item['sha256'])])}
    try:
        yield
    finally:
        _HISTORICAL, _ATTEMPTS = saved, attempts


def live_prefixes(repo, proof):
    load_prefixes(repo, proof)
    for kind in ('room', 'lifecycle'):
        item = proof[kind]
        raw = read(repo, item['source'], MAX_STREAM)
        frozen = _PREFIXES[(repo, item['path'], item['sha256'])][0]
        if not raw.startswith(frozen):
            fail('captured canonical stream prefix changed')
        # Validate even unrelated appended records; valid appends need not
        # equal the old entire-stream digest.
        records = stream_rows(raw)
        if kind == 'room':
            room_records(records, proof['room_id'], sha(raw))
        else:
            runpy.run_path(str(HERE / 'agent-events.py'))['project_attempts'](records)


def reset_streams():
    _STREAMS.clear()
    _ATTEMPTS.clear()
    _PREFIXES.clear()
    _PREFIX_ATTEMPTS.clear()
    _ROOM_STATES.clear()


def attempts_for(repo):
    if repo not in _ATTEMPTS:
        events, _ = rows(repo, '.oms/lifecycle/events.jsonl')
        engine = runpy.run_path(str(HERE / 'agent-events.py'))
        _ATTEMPTS[repo] = (events, engine['project_attempts'](events))
    return _ATTEMPTS[repo]


def native_participant(attempt):
    refs = attempt.get('refs', {})
    if not isinstance(refs, dict):
        fail('malformed native participant refs')
    # Initial native enrollment deliberately uses its attempt ID. An explicit
    # ref is authoritative even when invalid; it must never trigger fallback.
    value = refs['panel_room_participant'] if 'panel_room_participant' in refs else attempt.get('attempt_id')
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._:-]{0,159}', value):
        fail('malformed explicit native participant identity')
    return value


def native_binding(attempt, who, provider, room):
    refs = attempt.get('refs', {})
    if (native_participant(attempt) != who or attempt.get('tool') != 'panel-main' or
            attempt.get('provider') != provider or refs.get('panel_role') != 'main' or
            refs.get('panel_room_id') != room):
        fail('contradictory native participant history')


def native_binding_history(own_events, who, provider, room):
    if not own_events or own_events[0].get('event_type') != 'attempt.created':
        fail('native participant creation is unproven')
    original = own_events[0]
    current = {'attempt_id': original['attempt_id'], 'provider': original.get('provider'),
               'tool': original.get('tool'), 'refs': {}, 'state': 'queued'}
    for event in own_events:
        refs = event.get('refs', {})
        if (not isinstance(refs, dict) or event.get('attempt_id') != current['attempt_id'] or
                any(key in event and event[key] != current[key] for key in ('provider', 'tool'))):
            fail('malformed or contradictory native participant history')
        current['refs'].update(refs)
        native_binding(current, who, provider, room)
        if event['event_type'] == 'attempt.state_changed':
            current['state'] = event['to_state']
    current['terminal'] = current['state'] in ('done', 'failed', 'cancelled', 'timed_out', 'abandoned')
    return current


def related_native_bindings(events, who, provider, room):
    related = set()
    # Association precedes every provider/room/tool filter, including refs
    # superseded later in the same attempt's history.
    for event in events:
        refs = event.get('refs', {})
        if not isinstance(refs, dict):
            fail('malformed native history refs')
        if event['attempt_id'] == who or refs.get('panel_room_participant') == who:
            related.add(event['attempt_id'])
    return {ident: native_binding_history([e for e in events if e['attempt_id'] == ident],
                                         who, provider, room) for ident in related}


def membership_epoch(repo, room, who, provider, read_window=None):
    records, content_sha = rows(repo, '.oms/threads/' + room + '.jsonl')
    state = room_records(records, room, content_sha)
    related = [r for r in records if r.get('room_event', {}).get('participant') == who]
    joins = [r for r in related if r['room_event'].get('kind') == 'join']
    if not joins or any(joins[0]['room_event'].get(k) != v for k, v in
                        (('provider', provider), ('role', 'main'))):
        fail('original native room membership is unproven')
    leaves = [r for r in related if r['room_event'].get('kind') == 'leave']
    if read_window is not None:
        started, ended = read_window
        if joins[0]['ts'] > started:
            fail('READ parent was not enrolled at the evidence point')
        # Independent streams have only second-resolution timestamps. A
        # departure/closure in that same second cannot establish an epoch;
        # reject ambiguity instead of attributing an old read to a rejoin.
        boundaries = leaves + [r for r in records if r.get('role') == 'closed']
        if any(r['ts'] <= ended for r in boundaries):
            fail('historical room membership boundary is unproven or ambiguous')
        if any(r['ts'] <= ended for r in joins[1:]):
            fail('historical READ crosses recycled room membership')
        return
    member = next((m for m in state['participants'] if m['participant'] == who), {})
    if (state['id'] != room or state['closed'] or len(joins) != 1 or leaves or
            not member.get('joined') or member.get('role') != 'main' or member.get('provider') != provider):
        fail('actual parent room context is unproven or recycled')


def caller_owner(repo, bundle, task):
    who = os.environ.get('OMS_ROOM_PARTICIPANT', '')
    room = os.environ.get('OMS_ROOM_ID', '')
    aid = os.environ.get('OMS_PANEL_MAIN_ATTEMPT', '')
    if (who != task.get('claimed_by_participant') or who != bundle['owner']['participant'] or
            not re.fullmatch(r'room-[0-9a-f]{12}', room)):
        fail('actual parent caller differs from current declared claim owner')
    events, attempts = attempts_for(repo)
    provider = bundle['owner']['parent_provider']
    bindings = related_native_bindings(events, who, provider, room)
    active = [attempts[ident] for ident in bindings if attempts[ident].get('terminal') is not True]
    if (len(active) != 1 or active[0].get('attempt_id') != aid or
            active[0].get('terminal') is not False or active[0].get('state') not in
            ('starting', 'working', 'waiting_input', 'waiting_approval', 'verifying', 'review')):
        fail('current native panel-main owner binding is unproven or ambiguous')
    membership_epoch(repo, room, who, provider)
    return {'participant': who, 'parent_provider': provider, 'room_id': room}


def read_parent(repo, events, parent, child_events, who, provider):
    if not child_events or child_events[0].get('event_type') != 'attempt.created':
        fail('READ creation boundary is unproven')
    created = child_events[0]
    boundary = next(i for i, event in enumerate(events) if event['event_id'] == created['event_id'])
    historical = [e for e in events[:boundary] if e.get('attempt_id') == parent.get('attempt_id')]
    room = historical[0].get('refs', {}).get('panel_room_id') if historical else None
    if not isinstance(room, str) or not re.fullmatch(r'room-[0-9a-f]{12}', room):
        fail('exact historical native room is unproven')
    bindings = related_native_bindings(events[:boundary], who, provider, room)
    active = [binding for binding in bindings.values() if not binding['terminal']]
    if (len(active) != 1 or active[0]['attempt_id'] != parent.get('attempt_id') or
            active[0]['state'] not in ('starting', 'working', 'waiting_input', 'waiting_approval', 'verifying', 'review')):
        fail('READ parent was not the unique active native owner at creation')
    binding = active[0]
    terminal = [e for e in child_events if e.get('event_type') == 'attempt.state_changed' and e.get('to_state') == 'done']
    if len(terminal) != 1:
        fail('READ successful terminal boundary is unproven')
    membership_epoch(repo, room, who, provider, (created['ts'], terminal[0]['ts']))
    return binding


def git_environment():
    env = dict(os.environ, GIT_NO_LAZY_FETCH='1', GIT_ALLOW_PROTOCOL='', GIT_OPTIONAL_LOCKS='0')
    for name in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR',
                 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE'):
        env.pop(name, None)
    return env


def git(repo, *args):
    env = git_environment()
    result = subprocess.run(['git', '-c', 'core.fsmonitor=false', '-C', repo] + list(args),
                            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, timeout=30, check=False)
    if result.returncode or len(result.stdout) > 4 * 1024 * 1024:
        fail('local Git proof unavailable')
    return result.stdout


def namespace(repo):
    common = git(repo, 'rev-parse', '--git-common-dir').decode().strip().replace('\r', '')
    return sha(os.path.realpath(os.path.join(repo, common)).encode())


def file_identity(repo, rel):
    target = DURABLE['canonical_repo_path'](repo, relative(rel), 'frozen completion input')
    try:
        info = os.lstat(target)
    except FileNotFoundError:
        return None
    return {'dev': info.st_dev, 'ino': info.st_ino, 'size': info.st_size,
            'mode': stat.S_IMODE(info.st_mode),
            'mtime_ns': info.st_mtime_ns, 'ctime_ns': info.st_ctime_ns}


def file_mode(repo, rel, info):
    if os.name == 'nt':
        # Windows has no POSIX executable bit; Git's tracked mode is the
        # product contract. Untracked inputs have the ordinary file mode.
        listing = git(repo, 'ls-files', '--stage', '-z', '--', rel)
        if listing:
            entries = listing.split(b'\0')
            if len(entries) != 2:
                fail('ambiguous Windows source mode')
            metadata, path = entries[0].split(b'\t', 1)
            mode, _, stage = metadata.decode('ascii').split()
            if stage != '0' or path.decode('utf-8') != rel or mode not in ('100644', '100755'):
                fail('unsupported Windows source mode')
            return mode
        return '100644'
    return '100755' if info.st_mode & stat.S_IXUSR else '100644'


def source(repo, bundle, task):
    base = bundle['base_commit']
    if not isinstance(base, str) or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', base):
        fail('full base commit required')
    if git(repo, 'rev-parse', base + '^{commit}').decode().strip() != base:
        fail('base must be a local commit')
    git(repo, 'merge-base', '--is-ancestor', base, 'HEAD')
    manifest = bundle['source_manifest']
    envelope = bundle['source_envelope']
    if (not isinstance(manifest, list) or not 1 <= len(manifest) <= 256 or
            not isinstance(envelope, list) or not 1 <= len(envelope) <= 256):
        fail('bounded explicit source envelope and manifest required')
    names = [relative(p) for p in envelope]
    if len(set(names)) != len(names):
        fail('duplicate source envelope path')
    seen = set()
    for entry in manifest:
        keys(entry, ('path', 'state', 'mode', 'sha256'))
        rel = relative(entry['path'])
        if rel in seen or rel.startswith('.oms/'):
            fail('duplicate or state-directory source')
        seen.add(rel)
        target = DURABLE['canonical_repo_path'](repo, rel, 'completion source')
        if entry['state'] == 'absent':
            if entry['mode'] is not None or entry['sha256'] is not None or os.path.lexists(target):
                fail('source deletion/absence changed: ' + rel)
        elif entry['state'] == 'file':
            raw = read(repo, rel, 8 * 1024 * 1024)
            info = os.lstat(target)
            mode = file_mode(repo, rel, info)
            if mode != entry['mode'] or sha(raw) != digest(entry['sha256']):
                fail('source content/mode changed: ' + rel)
        else:
            fail('unsupported source state')
    if set(names) != seen:
        fail('source manifest must cover the complete reviewed source envelope')
    # Include deleted base paths as well as current tracked/untracked inputs.
    paths = set(git(repo, 'ls-tree', '-r', '--name-only', '-z', base).decode('utf-8').split('\0'))
    paths.update(git(repo, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').decode('utf-8').split('\0'))
    allowed = task.get('allowed_paths', [])
    if not isinstance(allowed, list):
        fail('invalid task source envelope')
    patterns = SCOPE['validate_patterns']([SCOPE['normalize'](p) for p in allowed])
    required = {p for p in paths if p and any(SCOPE['matches'](p, pattern) for pattern in patterns)}
    if not required.issubset(seen):
        fail('source manifest omits task allowed_paths content/deletions')
    return {'head': git(repo, 'rev-parse', 'HEAD').decode().strip(),
            'base_commit': base, 'manifest_sha256': sha(encoded(manifest)),
            'source_identities': {item['path']: file_identity(repo, item['path']) for item in manifest}}


def verifier(repo, value, task, current=True):
    keys(value, ('command', 'files', 'adopted', 'review_summary'))
    command = text(value['command'], 'verifier command', 4096)
    text(value['review_summary'], 'verifier review')
    if type(value['adopted']) is not bool:
        fail('verifier adoption must be boolean')
    if task.get('verify'):
        if command != task['verify']:
            fail('verifier differs from the actual task command')
    elif value['adopted'] is not True:
        fail('legacy empty verifier requires explicit reviewed adoption')
    files = value['files']
    if not isinstance(files, list) or not 1 <= len(files) <= 64:
        fail('frozen verifier file manifest required')
    seen = set()
    for item in files:
        keys(item, ('path', 'state', 'mode', 'sha256'))
        if item['path'] in seen or item['state'] != 'file':
            fail('invalid verifier manifest')
        seen.add(item['path'])
        relative(item['path'])
        digest(item['sha256'])
        if item['mode'] not in ('100644', '100755'):
            fail('invalid verifier mode')
        if not current:
            continue
        reference(repo, {'path': item['path'], 'sha256': item['sha256']}, 8 * 1024 * 1024)
        info = os.lstat(DURABLE['canonical_repo_path'](repo, relative(item['path']), 'verifier'))
        if item['mode'] != file_mode(repo, item['path'], info):
            fail('verifier mode changed')
    verifier_argv(command, seen)
    return sha(command.encode())


def verifier_argv(command, files):
    # Literal file guards and loops preserve existing verifier contracts.
    # Opaque expansion, comments and redirection cannot supply a frozen pin.
    if command == 'git diff --check':
        return [['git', 'diff', '--check']]
    lexer = shlex.shlex(command, posix=True, punctuation_chars=';&|')
    lexer.whitespace_split = True
    lexer.commenters = ''
    words = list(lexer)
    commands, used, connectors, at = [], set(), set(), 0
    while at < len(words):
        if len(commands) >= 16:
            fail('too many verifier invocations')
        if words[at] == 'if':
            tail = words[at:at + 10]
            if (len(tail) != 10 or tail[:3] != ['if', 'test', '-f'] or
                    tail[4:7] != [';', 'then', 'bash'] or tail[8:] != [';', 'fi'] or
                    tail[3] != tail[7] or tail[3] not in files or tail[3].startswith('-')):
                fail('unsupported conditional verifier')
            commands.append(tail)
            used.add(tail[3])
            at += 10
        elif words[at] == 'for':
            if at + 3 >= len(words) or not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', words[at + 1]) or words[at + 2] != 'in':
                fail('unsupported verifier loop')
            end = next((i for i in range(at + 3, len(words)) if words[i] == ';'), -1)
            paths = words[at + 3:end] if end >= 0 else []
            variable = '$' + words[at + 1]
            tail = [';', 'do', 'if', 'test', '-f', variable, ';', 'then', 'bash', variable,
                    '||', 'exit', ';', 'fi', ';', 'done']
            if (not paths or len(paths) > 64 or any(path not in files or path.startswith('-') for path in paths) or
                    words[end:end + len(tail)] != tail):
                fail('unsupported verifier loop body')
            commands.append(words[at:end + len(tail)])
            used.update(paths)
            at = end + len(tail)
        else:
            end = next((i for i in range(at, len(words)) if words[i] in (';', '&&')), len(words))
            argv = words[at:end]
            if not argv or any(not re.fullmatch(r'[A-Za-z0-9_./:=,+@%-]+', word) for word in argv):
                fail('unsupported verifier shell grammar')
            if (len(argv) < 2 or argv[0] not in ('bash', 'sh', 'python3') or
                    argv[1] not in files or argv[1].startswith('-')):
                fail('verifier must execute a frozen repository-relative script, not an interpreter option')
            used.add(argv[1])
            commands.append(argv)
            at = end
        if at < len(words):
            if words[at] not in (';', '&&'):
                fail('unsupported verifier connector')
            connectors.add(words[at])
            if len(connectors) > 1:
                fail('mixed verifier connectors can conceal a skipped verification')
            at += 1
            if at == len(words):
                fail('empty verifier invocation')
    if not used:
        fail('verifier must execute at least one frozen script')
    return commands


def requirements(repo, bundle):
    mapping = bundle['requirements']
    if not isinstance(mapping, list) or not 1 <= len(mapping) <= 64:
        fail('complete reviewed requirement/deliverable map required')
    ids = set()
    for item in mapping:
        keys(item, ('id', 'obligation', 'deliverable', 'source_paths', 'evidence'))
        name = text(item['id'], 'requirement id', 80)
        if name in ids:
            fail('duplicate requirement')
        ids.add(name)
        text(item['obligation'], 'requirement obligation')
        text(item['deliverable'], 'requirement deliverable')
        if not isinstance(item['source_paths'], list) or not item['source_paths']:
            fail('each obligation needs specific source coverage')
        if not set(item['source_paths']).issubset(bundle['source_envelope']):
            fail('obligation source missing from manifest')
        evidence = item['evidence']
        if not isinstance(evidence, list) or not 1 <= len(evidence) <= 16:
            fail('each obligation needs nonempty specific evidence')
        for ref in evidence:
            reference(repo, ref)
    reviewed = bundle['reviewed_obligations']
    if not isinstance(reviewed, list) or set(reviewed) != ids or len(reviewed) != len(ids):
        fail('partial requirement map')


def research(repo, bundle, task, plan_id):
    keys(bundle['research'], ('classification', 'artifact', 'index_event_id', 'attempt_id', 'parent_attempt_id'))
    info = bundle['research']
    if info['classification'] != 'research-only' or task.get('patch') or task.get('landing') or task.get('landed_commit'):
        fail('research requires explicitly reviewed research-only work without code landing')
    artifact = info['artifact']
    reference(repo, artifact)
    if task.get('artifact') != artifact['path']:
        fail('research artifact differs from original task evidence')
    index, _ = rows(repo, '.oms/artifacts/index.jsonl')
    matched = [row for row in index if row.get('event_id') == info['index_event_id']]
    if len(matched) != 1:
        fail('indexed artifact identity unavailable')
    row = matched[0]
    if (row.get('artifact') != artifact['path'] or row.get('artifact_sha256') != artifact['sha256'] or
            type(row.get('exit')) is not int or row['exit'] != 0 or
            row.get('attempt_id') != info['attempt_id'] or row.get('task_id') != task['id']):
        fail('artifact is not the immutable successful indexed read result')
    ancillary = None
    if row.get('patch'):
        ancillary = {'path': relative(row['patch']), 'sha256': digest(row.get('patch_sha256'))}
        if not ancillary['path'].endswith('.patch') or reference(repo, ancillary, 8 * 1024 * 1024) != b'':
            fail('read producer ancillary patch must be exactly empty')
    elif row.get('patch_sha256'):
        fail('ancillary patch digest without a path')
    events, attempts = attempts_for(repo)
    attempt = attempts.get(info['attempt_id'], {})
    parent = attempts.get(info['parent_attempt_id'], {})
    own_events = [e for e in events if e.get('attempt_id') == info['attempt_id']]
    if (attempt.get('state') != 'done' or attempt.get('terminal') is not True or
            attempt.get('task_id') != task['id'] or
            attempt.get('parent_attempt_id') != info['parent_attempt_id'] or
            attempt.get('provider') != row.get('provider') or
            not own_events or own_events[0].get('refs', {}).get('panel_access') != 'read' or
            attempt.get('refs', {}).get('panel_access') != 'read' or
            any(e.get('refs', {}).get('panel_access', 'read') != 'read' for e in own_events)):
        fail('exact terminal successful read-access attempt provenance is unproven')
    parent_binding = read_parent(repo, events, parent, own_events, task['claimed_by_participant'],
                                 bundle['owner']['parent_provider'])
    binding = (row.get('plan_id') == plan_id and row.get('base_sha') == bundle['base_commit'] and
               attempt.get('refs', {}).get('plan_id') == plan_id and
               attempt.get('refs', {}).get('task_sha256') == bundle['task_sha256'] and
               attempt.get('refs', {}).get('source_manifest_sha256') == sha(encoded(bundle['source_manifest'])))
    if not binding:
        adoption = bundle.get('legacy_adoption')
        keys(adoption, ('artifact', 'original_contract', 'review_summary'))
        if adoption['artifact'] != artifact or adoption['original_contract'] != bundle['original_contract']:
            fail('legacy adoption must bind exact original artifact and task obligation')
        text(adoption['review_summary'], 'current legacy adoption review')
        # Present producer bindings may never be overridden by legacy adoption.
        recorded_base = row.get('base_sha')
        if recorded_base:
            if not isinstance(recorded_base, str) or not re.fullmatch(r'[0-9a-f]{7,64}', recorded_base):
                fail('invalid legacy producer base')
            if git(repo, 'rev-parse', recorded_base + '^{commit}').decode().strip() != bundle['base_commit']:
                fail('conflicting legacy producer source base')
        for actual, expected in ((row.get('plan_id'), plan_id),
                                 (attempt.get('refs', {}).get('plan_id'), plan_id),
                                 (attempt.get('refs', {}).get('task_sha256'), bundle['task_sha256']),
                                 (attempt.get('refs', {}).get('source_manifest_sha256'), sha(encoded(bundle['source_manifest'])))):
            if actual and actual != expected:
                fail('conflicting producer binding cannot be adopted')
    if not any(artifact in item['evidence'] for item in bundle['requirements']):
        fail('research artifact is absent from requirement map')
    return {'index': row, 'attempt': attempt,
            'parent': {'attempt_id': parent['attempt_id'], 'provider': parent['provider'],
                       'tool': parent['tool'], 'participant': native_participant(parent_binding)},
            'attempt_events_sha256': sha(encoded(own_events)), 'ancillary_patch': ancillary}


def satisfaction(repo, bundle, tasks, plan_id):
    successor = bundle['successor']
    common = ('plan_id', 'task_id', 'task_sha256', 'original_contract', 'product_kind')
    if successor.get('product_kind') == 'patch-land':
        keys(successor, common + ('landing_id', 'admission_event_id', 'land_event_id'))
    elif successor.get('product_kind') == 'commit':
        keys(successor, common + ('commit_sha', 'land_receipt_sha256'))
    else:
        fail('unsupported successor product kind')
    if successor['plan_id'] != plan_id:
        fail('archived/cross-plan successors are unsupported; no receipt transplantation')
    tid = successor['task_id']
    if tid == bundle['task_id'] or tid not in tasks:
        fail('successor cycle or unknown identity')
    task = tasks[tid]
    if (task.get('state') != 'done' or task.get('completion_kind') or
            RECEIPT['digest'](task) != successor['task_sha256'] or
            RECEIPT['projection'](task) != successor['original_contract']):
        fail('successor must independently complete its exact actual landing contract')
    # Explicitly prohibit dependency cycles and chains of satisfied-by receipts.
    pending, visited = [tid], set()
    while pending:
        current = pending.pop()
        if current == bundle['task_id']:
            fail('successor depends on original task')
        if current in visited:
            continue
        visited.add(current)
        pending.extend(tasks.get(current, {}).get('depends', []))
    if successor['product_kind'] == 'commit':
        return commit_product(repo, bundle, task)
    if task.get('landing') or task.get('landed_commit'):
        fail('patch successor has contradictory commit landing fields')
    log, _ = rows(repo, '.oms/landings.jsonl')
    entries = [row for row in log if row.get('landing_id') == successor['landing_id']]
    intents = [row for row in entries if row.get('event') == 'intent']
    if len(intents) != 1:
        fail('exact immutable landing intent unavailable')
    intent = intents[0]
    fields = ('landing_id', 'patch', 'patch_sha', 'base_sha', 'task', 'lease',
              'plan_receipt_sha', 'plan_done_receipt_sha', 'approval', 'approval_version')
    canonical = {'schema': 1}
    for field in fields:
        if not isinstance(intent.get(field), str):
            fail('malformed landing receipt')
        canonical[field] = intent[field]
    receipt_hash = sha(json.dumps(canonical, sort_keys=True, ensure_ascii=False,
                                 separators=(',', ':')).encode())
    completed = [row for row in entries if row.get('event') == 'complete' and
                 row.get('schema') == 1 and row.get('plan_id') == plan_id and
                 all(row.get(field) == intent[field] for field in fields) and
                 row.get('receipt_sha') == receipt_hash]
    if (not completed or any(row.get('event') == 'abandoned' for row in entries) or
            intent.get('plan_id') != plan_id or intent['task'] != tid or
            intent['lease'] != task.get('lease_id') or intent['plan_done_receipt_sha'] != RECEIPT['digest'](task) or
            intent['patch'] != task.get('patch') or intent['base_sha'] != bundle['base_commit']):
        fail('successor lacks exact terminal successful patch-land receipt')
    patch_ref = {'path': legacy_relative(repo, intent['patch']), 'sha256': digest(intent['patch_sha'])}
    patch = reference(repo, patch_ref, 8 * 1024 * 1024)
    if not patch.strip():
        fail('no-op patch cannot satisfy obligations')
    index, _ = rows(repo, '.oms/artifacts/index.jsonl')
    def indexed(event, kind):
        matches = [r for r in index if r.get('event_id') == event]
        if len(matches) != 1:
            fail('successor indexed receipt missing')
        row = matches[0]
        if (row.get('kind') != kind or type(row.get('exit')) is not int or row['exit'] != 0 or
                row.get('plan_id') != plan_id or row.get('task_id') != tid or
                row.get('patch_sha256') != patch_ref['sha256']):
            fail('successor admission/land index mismatch')
        if kind == 'patch-admit' and 'patch_external' in row:
            # Admission inspects a private capture before canonical publication.
            # Its digest links that inspection to the independently retained
            # intent/land product; the diagnostic name grants no path authority.
            external = row['patch_external']
            keys(external, ('name', 'owned', 'sha256'))
            text(external['name'], 'external patch name')
            if ('patch' in row or external['owned'] is not False or
                    digest(external['sha256']) != patch_ref['sha256']):
                fail('successor admission external patch mismatch')
        elif ('patch_external' in row or
              legacy_relative(repo, row.get('patch')) != patch_ref['path']):
            fail('successor admission/land index mismatch')
        return row
    admission = indexed(successor['admission_event_id'], 'patch-admit')
    landed = indexed(successor['land_event_id'], 'patch-land')
    report = {'path': admission.get('artifact'), 'sha256': admission.get('artifact_sha256')}
    report_raw = reference(repo, report)
    if not report_raw.startswith(b'# Patch admission: ADMIT\n'):
        fail('successor admission verdict is not successful')
    # Prove product bytes independently of labels by applying the frozen patch
    # to a private index seeded from its actual base, then compare every source
    # path (including modes and deletions) with that product tree.
    import tempfile
    private_directory = tempfile.TemporaryDirectory(prefix='oms-completion-product-')
    private = os.path.join(private_directory.name, 'index')
    objects = git(repo, 'rev-parse', '--git-path', 'objects').decode('utf-8').strip()
    objects = os.path.realpath(os.path.join(repo, objects))
    private_objects = os.path.join(private_directory.name, 'objects')
    os.mkdir(private_objects)
    git_env = dict(git_environment(), GIT_INDEX_FILE=private,
                   GIT_OBJECT_DIRECTORY=private_objects,
                   GIT_ALTERNATE_OBJECT_DIRECTORIES=json.dumps(objects.replace('\\', '/'), ensure_ascii=False))
    try:
        def product(*args, data=None):
            result = subprocess.run(['git', '-C', repo] + list(args), env=git_env,
                                    input=data, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                    timeout=30, check=False)
            if result.returncode or len(result.stdout) > 8 * 1024 * 1024:
                fail('cannot prove actual successor product')
            return result.stdout
        product('read-tree', intent['base_sha'])
        product('apply', '--cached', '--binary', '-', data=patch)
        changed = product('diff', '--cached', '--name-only', '-z', intent['base_sha'])
        changed_paths = [p for p in changed.decode('utf-8').split('\0') if p]
        if not changed_paths or not set(changed_paths).issubset(bundle['source_envelope']):
            fail('successor patch is empty or product manifest omits changed paths')
        for item in bundle['source_manifest']:
            listing = product('ls-files', '--stage', '-z', '--', item['path'])
            if item['state'] == 'absent':
                if listing:
                    fail('source deletion differs from successor product')
            else:
                entries = listing.split(b'\0')
                if len(entries) != 2 or not entries[0]:
                    fail('source is absent or ambiguous in successor product')
                metadata, rel = entries[0].split(b'\t', 1)
                mode, oid, stage = metadata.decode('ascii').split()
                if rel.decode('utf-8') != item['path'] or stage != '0' or mode != item['mode'] or sha(product('cat-file', 'blob', oid)) != item['sha256']:
                    fail('source content/mode differs from successor product')
    finally:
        private_directory.cleanup()
    if not any(report in item['evidence'] or patch_ref in item['evidence'] for item in bundle['requirements']):
        fail('actual successor evidence missing from obligation map')
    return {'intent': intent, 'complete': completed[-1], 'admission': admission, 'land': landed}


def canonical_land_directory(repo):
    # Match the existing shell land-state resolver, including Git Bash's
    # physical path spelling used by cksum and native Python conversion.
    script = r'''set -eu
common_dir="$(git -C "$1" rev-parse --git-common-dir | tr -d '\r')"
case "$common_dir" in /*|[A-Za-z]:/*) ;; *) common_dir="$1/$common_dir" ;; esac
common_dir="$(cd "$common_dir" && pwd -P)"
common_root="$(cd "$common_dir/.." && pwd -P)"
name="$(basename "$common_root")"
name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '_')"
name="${name//../_}"
case "$name" in ''|.|..) name=repo ;; esac
checksum="$(printf '%s' "$common_root" | cksum | awk '{print $1 "-" $2}')"
case "$checksum" in ''|*[!0-9-]*) exit 2 ;; esac
value="${XDG_STATE_HOME:-$HOME/.local/state}/oh-my-setting/land/$name-$checksum"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) value="$(cygpath -m "$value")" ;; esac
printf '%s\n' "$value"
'''
    result = subprocess.run(['bash', '-c', script, 'completion-land-state', repo],
                            env=git_environment(), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            timeout=30, check=False)
    if result.returncode or len(result.stdout) > 4096:
        fail('canonical land receipt directory unavailable')
    return result.stdout.decode('utf-8').strip().replace('\r', '')


def commit_product(repo, bundle, task):
    successor = bundle['successor']
    commit = successor['commit_sha']
    if not isinstance(commit, str) or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit):
        fail('successor full commit identity required')
    wanted = {'kind': 'commit', 'sha': commit,
              'receipt_sha256': digest(successor['land_receipt_sha256'])}
    if task.get('landing') != wanted or (task.get('landed_commit') and task['landed_commit'] != commit):
        fail('successor exact terminal commit landing mismatch')
    directory = canonical_land_directory(repo)
    declared = os.environ.get('OMS_LAND_RECEIPTS_DIR')
    if declared and os.path.normcase(os.path.abspath(declared)) != os.path.normcase(os.path.abspath(directory)):
        fail('foreign canonical land receipt namespace')
    # Strictly freeze the exact raw canonical receipt, not a bundle-supplied path.
    root = os.path.splitdrive(os.path.abspath(directory))[0] + os.path.sep
    rel_dir = os.path.relpath(directory, root)
    DURABLE['canonical_repo_path'](root, os.path.join(rel_dir, 'probe'), 'canonical commit receipts')
    names = sorted(os.listdir(directory))
    if len(names) > 4096:
        fail('too many canonical commit receipts')
    matched = []
    for name in names:
        if not name.startswith(commit + '-') or not name.endswith('.json'):
            continue
        raw = DURABLE['read_no_follow'](root, os.path.join(rel_dir, name),
                                        'canonical commit receipt', max_bytes=1024 * 1024)
        value = parsed(raw)
        if sha(raw) == wanted['receipt_sha256']:
            matched.append((name, raw, value))
    if len(matched) != 1:
        fail('exact immutable canonical commit receipt unavailable')
    name, raw, receipt = matched[0]
    if (type(receipt.get('schema')) is not int or receipt['schema'] != 1 or
            receipt.get('sha') != commit or receipt.get('state') != 'passed' or
            type(receipt.get('gate', {}).get('rc')) is not int or receipt['gate']['rc'] != 0 or
            type(receipt.get('push', {}).get('rc')) is not int or receipt['push']['rc'] != 0 or
            receipt.get('ci', {}).get('conclusion') != 'success'):
        fail('canonical commit receipt lacks passed gate/push/CI')
    # Existing commit-finish owns gate/push/CI and local pushed-ref authority.
    # Readers resolve this same canonical directory even outside a satisfy call.
    if _COMMIT_PROOF is None:
        fail('commit landing authority callback unavailable')
    previous = os.environ.get('OMS_LAND_RECEIPTS_DIR')
    os.environ['OMS_LAND_RECEIPTS_DIR'] = directory
    try:
        if _COMMIT_PROOF(commit, wanted['receipt_sha256']) != wanted:
            fail('successor commit receipt differs from existing landing proof')
    finally:
        if previous is None:
            os.environ.pop('OMS_LAND_RECEIPTS_DIR', None)
        else:
            os.environ['OMS_LAND_RECEIPTS_DIR'] = previous
    git(repo, 'merge-base', '--is-ancestor', bundle['base_commit'], commit)
    changed = git(repo, 'diff', '--name-only', '-z', bundle['base_commit'], commit)
    paths = [p for p in changed.decode('utf-8').split('\0') if p]
    if not paths or not set(paths).issubset(bundle['source_envelope']):
        fail('commit product is empty, unrelated, or incompletely covered')
    for item in bundle['source_manifest']:
        raw_entry = git(repo, 'ls-tree', '-z', commit, '--', item['path'])
        if item['state'] == 'absent':
            if raw_entry:
                fail('source deletion differs from committed product')
        else:
            entries = raw_entry.split(b'\0')
            if len(entries) != 2 or not entries[0]:
                fail('committed product source absent or ambiguous')
            metadata, path = entries[0].split(b'\t', 1)
            mode, kind, oid = metadata.decode('ascii').split()
            if (path.decode('utf-8') != item['path'] or kind != 'blob' or mode != item['mode'] or
                    sha(git(repo, 'cat-file', 'blob', oid)) != item['sha256']):
                fail('source content/mode differs from committed product')
    artifact = {'path': legacy_relative(repo, task.get('artifact', '')),
                'sha256': sha(read(repo, legacy_relative(repo, task.get('artifact', ''))))}
    if not any(artifact in requirement['evidence'] for requirement in bundle['requirements']):
        fail('actual committed successor evidence missing from obligation map')
    return {'product_kind': 'commit', 'landing': wanted, 'receipt_name': name,
            'receipt': receipt, 'receipt_raw_sha256': sha(raw), 'artifact': artifact}


def review_contract(state_repo, bundle, task, plan_id, action):
    required = ('schema', 'kind', 'plan_id', 'task_id', 'task_sha256', 'plan_sha256', 'state',
                'lease_id', 'owner', 'original_contract', 'review_summary', 'reviewed_obligations',
                'requirements', 'base_commit', 'source_envelope', 'source_manifest', 'verifier')
    keys(bundle, required + (('research',) if action == 'accept-research' else ('successor',)), ('legacy_adoption',))
    if type(bundle['schema']) is not int or bundle['schema'] != 1 or bundle['kind'] != KINDS[action]:
        fail('unsupported completion bundle version/kind')
    if not isinstance(bundle['plan_id'], str) or not re.fullmatch(r'plan_[0-9a-f]{32}', bundle['plan_id']):
        fail('exact immutable plan lineage required')
    if not isinstance(bundle['lease_id'], str) or not re.fullmatch(r'lease_[0-9a-f]{32}', bundle['lease_id']):
        fail('exact current claim lease required')
    digest(bundle['plan_sha256'])
    if not isinstance(bundle['base_commit'], str) or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', bundle['base_commit']):
        fail('full base commit identity required')
    if bundle['plan_id'] != plan_id or bundle['task_id'] != task['id']:
        fail('plan/task identity mismatch')
    if (bundle['state'] != task['state'] or bundle['lease_id'] != task.get('lease_id') or
            not task.get('lease_id') or task['state'] not in ('claimed', 'running', 'review', 'blocked')):
        fail('original exact contract/state/lease mismatch')
    if (RECEIPT['digest'](task) != digest(bundle['task_sha256']) or
            RECEIPT['projection'](task) != bundle['original_contract']):
        fail('original full task contract mismatch')
    keys(bundle['owner'], ('provider', 'participant', 'parent_provider'))
    text(bundle['owner']['parent_provider'], 'native parent provider', 80)
    text(bundle['owner']['provider'], 'owner provider', 80)
    text(bundle['owner']['participant'], 'owner participant', 160)
    if (not task.get('provider') or not task.get('claimed_by_participant') or
            bundle['owner']['provider'] != task['provider'] or
            bundle['owner']['participant'] != task['claimed_by_participant']):
        fail('current declared claim owner mismatch; owner inference is forbidden')
    text(bundle['review_summary'], 'parent review summary')
    screen = subprocess.run(['bash', '-c', '. "$1"; grep -Eiq "$(agent_memory_secret_re)"',
                             'completion-screen', str(HERE / 'agent-memory-common.sh')],
                            input=encoded(bundle), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            timeout=5, check=False)
    if screen.returncode != 1:
        fail('review bundle contains sensitive-looking persistent content')
    if task.get('completion_kind') or task.get('completion_receipt'):
        fail('original already contains completion fields')
    if task.get('landing') or task.get('landed_commit'):
        fail('original code landing cannot be relabelled as a local completion')
    requirements(state_repo, bundle)
    manifest = bundle['source_manifest']
    envelope = bundle['source_envelope']
    if (not isinstance(manifest, list) or not 1 <= len(manifest) <= 256 or
            not isinstance(envelope, list) or not 1 <= len(envelope) <= 256):
        fail('invalid reviewed source manifest')
    paths = [relative(p) for p in envelope]
    seen = set()
    for item in manifest:
        keys(item, ('path', 'state', 'mode', 'sha256'))
        rel = relative(item['path'])
        if rel in seen or rel.startswith('.oms/'):
            fail('invalid source manifest path')
        seen.add(rel)
        if item['state'] == 'file':
            digest(item['sha256'])
            if item['mode'] not in ('100644', '100755'):
                fail('invalid source manifest mode')
        elif item['state'] != 'absent' or item['mode'] is not None or item['sha256'] is not None:
            fail('invalid source manifest state')
    if seen != set(paths) or len(set(paths)) != len(paths):
        fail('source envelope and manifest differ')
    verifier(state_repo, bundle['verifier'], task, current=False)
    if any(item not in manifest for item in bundle['verifier']['files']):
        fail('frozen verifier is outside the reviewed source manifest')
    covered = {p for requirement in bundle['requirements'] for p in requirement['source_paths']}
    covered.update(item['path'] for item in bundle['verifier']['files'])
    if covered != set(paths):
        fail('reviewed obligation/verifier map omits declared source envelope')


def check_bundle(repo, state_repo, bundle, task, tasks, plan_id, action, provenance):
    review_contract(state_repo, bundle, task, plan_id, action)
    snapshot = source(repo, bundle, task)
    command_sha = verifier(repo, bundle['verifier'], task)
    snapshot['verifier_command_sha256'] = command_sha
    # Source and evidence namespace must belong to the same local Git repository.
    if namespace(repo) != namespace(state_repo):
        fail('foreign execution/state repository')
    snapshot['repository_sha256'] = namespace(state_repo)
    live_prefixes(state_repo, provenance)
    snapshot['provenance'] = provenance
    if action == 'accept-research':
        current = research(state_repo, bundle, task, plan_id)
        with historical_prefixes(state_repo, provenance):
            frozen = research(state_repo, bundle, task, plan_id)
        # Telemetry may advance sequence/time/usage; exact access, producer
        # refs and terminal child/ancestor identity must still agree now.
        immutable = ('attempt_id', 'provider', 'tool', 'task_id', 'parent_attempt_id', 'base_sha',
                     'state', 'terminal', 'refs')
        if (any(current['attempt'].get(k) != frozen['attempt'].get(k) for k in immutable) or
                any(current[k] != frozen[k] for k in ('index', 'parent', 'ancillary_patch'))):
            fail('relevant READ provenance changed during verification')
        live_events, _ = attempts_for(state_repo)
        for event in live_events[provenance['lifecycle']['rows']:]:
            if (event.get('attempt_id') == bundle['research']['attempt_id'] and
                    any(frozen['attempt']['refs'].get(k) != value for k, value in event.get('refs', {}).items())):
                fail('contradictory READ ref history')
        snapshot['evidence'] = frozen
    else:
        snapshot['evidence'] = satisfaction(state_repo, bundle, tasks, plan_id)
    evidence_refs = [ref for item in bundle['requirements'] for ref in item['evidence']]
    if action == 'accept-research':
        evidence_refs.append(bundle['research']['artifact'])
        if snapshot['evidence']['ancillary_patch']:
            evidence_refs.append(snapshot['evidence']['ancillary_patch'])
    elif bundle['successor']['product_kind'] == 'patch-land':
        proof = snapshot['evidence']
        evidence_refs.extend([{'path': legacy_relative(state_repo, proof['intent']['patch']),
                               'sha256': proof['intent']['patch_sha']},
                              {'path': proof['admission']['artifact'], 'sha256': proof['admission']['artifact_sha256']}])
    snapshot['evidence_identities'] = {ref['path']: file_identity(state_repo, ref['path']) for ref in evidence_refs}
    return snapshot


def completion_task(receipt, ref):
    task = dict(receipt['original_task'])
    history = list(task.get('history', []))
    history.append({'schema': 1, 'kind': receipt['completion_kind'], 'ts': receipt['ts'],
                    'original_state': task['state'], 'completion_receipt': ref})
    task.update(state='done', updated=receipt['ts'], history=history,
                completion_kind=receipt['completion_kind'], completion_receipt=ref)
    return task


def checked_receipt(repo, ref):
    keys(ref, ('path', 'sha256'))
    relative(ref.get('path', ''))
    if ref['path'] != '.oms/plan/completions/' + digest(ref.get('sha256')) + '.json':
        fail('completion receipt must be content-addressed in canonical namespace')
    receipt = parsed(reference(repo, ref))
    keys(receipt, ('schema', 'kind', 'completion_kind', 'bundle_sha256', 'bundle', 'original_task',
                   'original_plan_sha256', 'snapshot', 'verification', 'ts', 'bundle_raw'))
    if type(receipt['schema']) is not int or receipt['schema'] != 1 or receipt['kind'] != 'oms-plan-completion':
        fail('unsupported completion receipt')
    if not isinstance(receipt['original_task'], dict):
        fail('invalid receipt original task')
    keys(receipt['snapshot'], ('head', 'base_commit', 'manifest_sha256', 'source_identities',
                              'verifier_command_sha256', 'repository_sha256', 'evidence', 'evidence_identities',
                              'provenance'))
    load_prefixes(repo, receipt['snapshot']['provenance'])
    datetime.datetime.strptime(receipt['ts'], '%Y-%m-%dT%H:%M:%SZ')
    bundle = receipt['bundle']
    if not isinstance(bundle, dict):
        fail('invalid receipt bundle')
    if receipt['completion_kind'] not in KINDS.values() or bundle.get('kind') != receipt['completion_kind']:
        fail('completion receipt kind mismatch')
    verification = receipt['verification']
    validate_verification(verification, receipt['snapshot'])
    if (type(verification['exit']) is not int or verification['exit'] != 0 or
            verification['command_sha256'] != sha(bundle['verifier']['command'].encode()) or
            receipt['original_plan_sha256'] != bundle['plan_sha256'] or
            receipt['snapshot']['verifier_command_sha256'] != verification['command_sha256'] or
            receipt['snapshot']['base_commit'] != bundle['base_commit']):
        fail('completion receipt lacks exact successful verification')
    if (not isinstance(receipt['bundle_raw'], str) or
            sha(receipt['bundle_raw'].encode('utf-8')) != digest(receipt['bundle_sha256']) or
            parsed(receipt['bundle_raw']) != bundle):
        fail('receipt reviewed bundle bytes mismatch')
    return receipt


def strict_plan(repo, path):
    if os.path.normcase(os.path.abspath(path)) != os.path.normcase(os.path.join(repo, '.oms/plan/tasks.json')):
        fail('completion references belong only to their canonical active plan')
    plan = parsed(read(repo, '.oms/plan/tasks.json'))
    if not isinstance(plan, dict) or not isinstance(plan.get('tasks'), dict):
        fail('invalid completion plan')
    return plan


def validate_done(repo, plan, task, commit_proof=None):
    global _COMMIT_PROOF
    if commit_proof is not None:
        _COMMIT_PROOF = commit_proof
    if task.get('completion_kind') not in KINDS.values():
        if 'completion_kind' in task or 'completion_receipt' in task:
            fail('unknown completion fields')
        return
    if task.get('state') != 'done':
        fail('completion reference on nonterminal task')
    receipt = checked_receipt(repo, task.get('completion_receipt', {}))
    if (receipt['bundle'].get('plan_id') != plan.get('plan_id') or
            receipt['bundle'].get('task_id') != task.get('id') or
            receipt['snapshot'].get('repository_sha256') != namespace(repo) or
            completion_task(receipt, task['completion_receipt']) != task):
        fail('forged, foreign or unreferenced task completion receipt')
    # Revalidate original contract, immutable provenance and frozen verifier files.
    # Product/source may evolve after acceptance; a receipt proves the frozen
    # source at acceptance, whereas a new satisfy invocation checks current bytes.
    original = receipt['original_task']
    bundle = receipt['bundle']
    action = next(name for name, kind in KINDS.items() if kind == receipt['completion_kind'])
    review_contract(repo, bundle, original, plan.get('plan_id'), action)
    if receipt['snapshot'].get('manifest_sha256') != sha(encoded(bundle['source_manifest'])):
        fail('receipt source manifest digest mismatch')
    if receipt['completion_kind'] == 'research-accepted':
        with historical_prefixes(repo, receipt['snapshot']['provenance']):
            actual = research(repo, bundle, original, plan.get('plan_id'))
        recorded = receipt['snapshot'].get('evidence', {})
        if any(actual[k] != recorded.get(k) for k in ('index', 'attempt', 'parent', 'attempt_events_sha256', 'ancillary_patch')):
            fail('immutable research provenance changed')
    else:
        successor = bundle['successor']
        historical = dict(successor['original_contract'], state='done')
        actual = satisfaction(repo, bundle, {successor['task_id']: historical}, plan.get('plan_id'))
        if actual != receipt['snapshot'].get('evidence'):
            fail('immutable successor provenance changed')
    if RECEIPT['digest'](original) != receipt['bundle']['task_sha256']:
        fail('completion receipt original task digest mismatch')
    if receipt['bundle']['original_contract'] != RECEIPT['projection'](original):
        fail('completion receipt original obligation mismatch')


def prior_receipt(repo, bundle_sha):
    directory = DURABLE['canonical_repo_path'](repo, '.oms/plan/completions/probe', 'completion receipts')
    directory = os.path.dirname(directory)
    if not os.path.exists(directory):
        return None
    entries = list(os.scandir(directory))
    if len(entries) > 4096:
        fail('too many completion receipts')
    matched = []
    for entry in entries:
        if not re.fullmatch(r'[0-9a-f]{64}\.json', entry.name):
            fail('malformed completion receipt namespace')
        ref = {'path': '.oms/plan/completions/' + entry.name, 'sha256': entry.name[:-5]}
        receipt = checked_receipt(repo, ref)
        if receipt['bundle_sha256'] == bundle_sha:
            matched.append((receipt, ref))
    if len(matched) > 1:
        fail('conflicting completion receipts for bundle')
    return matched[0] if matched else None


def validate_verification(value, snapshot):
    keys(value, ('exit', 'command_sha256', 'output_sha256', 'output_bytes', 'proof_sha256'))
    digest(value['output_sha256'])
    if (type(value['exit']) is not int or value['exit'] != 0 or
            type(value['output_bytes']) is not int or not 0 <= value['output_bytes'] <= 1024 * 1024 or
            value['command_sha256'] != snapshot['verifier_command_sha256']):
        fail('actual bounded verifier did not pass')
    proof = dict(value)
    proof.pop('proof_sha256')
    proof['snapshot_sha256'] = sha(encoded(snapshot))
    if digest(value['proof_sha256']) != sha(encoded(proof)):
        fail('verifier proof digest mismatch')


def capture_verifier(repo, command, snapshot):
    execution_env = git_environment() if command == 'git diff --check' else dict(os.environ, GIT_OPTIONAL_LOCKS='0')
    proc = subprocess.Popen(['bash', '-c', command], cwd=repo, env=execution_env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=os.name != 'nt')
    count, output, errors = [0], hashlib.sha256(), []
    def stop():
        try:
            if os.name != 'nt':
                import signal
                os.killpg(proc.pid, signal.SIGKILL)
            elif proc.poll() is None:
                proc.kill()
        except ProcessLookupError:
            pass
    def drain():
        try:
            while True:
                chunk = os.read(proc.stdout.fileno(), 65536)
                if not chunk:
                    return
                count[0] += len(chunk)
                if count[0] > 1024 * 1024:
                    errors.append('verifier output exceeds bounded capture')
                    stop()
                    return
                output.update(chunk)
        except OSError:
            errors.append('verifier output capture unavailable')
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        try:
            rc = proc.wait(timeout=300)
        except subprocess.TimeoutExpired:
            stop()
            fail('verifier execution timed out')
        reader.join(timeout=5)
        if reader.is_alive():
            stop()
            fail('verifier left an unclosed output stream')
        if errors:
            fail(errors[0])
        if rc:
            fail('verifier failed (exit %s)' % rc)
    finally:
        if proc.poll() is None:
            stop()
        proc.wait(timeout=10)
        if not reader.is_alive():
            proc.stdout.close()
        else:
            os.close(proc.stdout.fileno())
    result = {'exit': rc, 'command_sha256': sha(command.encode()),
              'output_sha256': output.hexdigest(), 'output_bytes': count[0]}
    proof = dict(result, snapshot_sha256=sha(encoded(snapshot)))
    result['proof_sha256'] = sha(encoded(proof))
    return result


def transaction_snapshot():
    filename = os.path.abspath(os.environ['OMS_COMPLETION_SNAPSHOT_FILE'])
    root = os.path.realpath(os.path.dirname(filename))
    return parsed(DURABLE['read_no_follow'](root, os.path.basename(filename),
                                           'completion transaction', max_bytes=MAX_JSON))


def run(context):
    if os.environ.get('OMS_HARNESS_CHILD') == '1' or os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') != '0':
        fail('parent-only operation')
    global _COMMIT_PROOF
    _COMMIT_PROOF = context['commit_proof']
    action = context['action']
    repo = os.path.realpath(os.environ['OMS_REPO'])
    state_repo = os.path.realpath(os.environ.get('OMS_STATE_REPO') or repo)
    expected_path = os.path.join(state_repo, '.oms', 'plan', 'tasks.json')
    if os.path.normcase(os.path.abspath(context['path'])) != os.path.normcase(expected_path):
        fail('completion requires canonical active plan')
    plan_raw = read(state_repo, '.oms/plan/tasks.json')
    # Strictly validate the raw plan as well as the engine's normalized row.
    parsed(plan_raw)
    plan = context['plan']
    task = plan['tasks'].get(os.environ.get('OMS_ID'))
    if not task:
        fail('unknown task')
    bundle_raw = read(state_repo, relative(os.environ.get('OMS_COMPLETION_BUNDLE', '')))
    bundle_sha = digest(os.environ.get('OMS_EXPECTED_COMPLETION_BUNDLE_SHA256', ''))
    if sha(bundle_raw) != bundle_sha:
        fail('bundle bytes changed')
    bundle = parsed(bundle_raw)
    reset_streams()
    if namespace(repo) != namespace(state_repo):
        fail('foreign execution/state repository')
    owner = caller_owner(state_repo, bundle, task)
    prior = prior_receipt(state_repo, bundle_sha)
    if task.get('state') == 'done':
        validate_done(state_repo, plan, task)
        if not prior or prior[1] != task.get('completion_receipt') or prior[0]['bundle'] != bundle:
            fail('conflicting replay')
        original = prior[0]['original_task']
    else:
        original = task
    for name, actual in (('OMS_EXPECTED_PLAN_SHA256', bundle.get('plan_sha256')),
                         ('OMS_EXPECTED_TASK_SHA256', bundle.get('task_sha256')),
                         ('OMS_EXPECTED_STATE', bundle.get('state')),
                         ('OMS_LEASE_ID', bundle.get('lease_id'))):
        if not os.environ.get(name) or os.environ[name] != actual:
            fail('expected CLI CAS differs from reviewed bundle')
    phase = os.environ.get('OMS_COMPLETION_PHASE', '')
    if task.get('state') == 'done':
        receipt, ref = prior
        if receipt['completion_kind'] != KINDS[action]:
            fail('replay action differs from accepted completion kind')
        checkpoint = {'bundle_sha256': bundle_sha, 'snapshot': receipt['snapshot'], 'resume': True}
        if phase == 'preflight':
            print(json.dumps(checkpoint))
        elif phase == 'finalize' and transaction_snapshot() == checkpoint:
            print('plan: completion already applied (' + task['id'] + ')')
        else:
            fail('conflicting completed replay')
        return
    if task.get('state') != 'done' and sha(plan_raw) != digest(bundle['plan_sha256']):
        fail('active plan bytes changed')
    markers = context['markers']()
    exact = [m for m in markers if m.get('task_id') == original['id'] and m.get('lease_id') == original.get('lease_id')]
    if any(not context['typed'](m) or context['alive'](m) for m in exact):
        fail('live or unproven exact worker veto')
    review_contract(state_repo, bundle, original, plan.get('plan_id'), action)
    if prior:
        provenance = prior[0]['snapshot']['provenance']
    elif phase == 'preflight':
        provenance = capture_prefixes(state_repo, owner['room_id'])
    elif phase == 'finalize':
        provenance = transaction_snapshot()['snapshot']['provenance']
    else:
        fail('invalid transaction phase')
    if provenance['room_id'] != owner['room_id']:
        fail('current caller room differs from captured authority')
    snapshot = check_bundle(repo, state_repo, bundle, original, plan['tasks'], plan.get('plan_id'), action, provenance)
    if prior:
        receipt, ref = prior
        if (receipt['bundle'] != bundle or receipt['original_task'] != original or
                receipt['snapshot'] != snapshot or receipt['completion_kind'] != KINDS[action]):
            fail('prior receipt preconditions changed')
    if phase == 'preflight':
        print(json.dumps({'bundle_sha256': bundle_sha, 'snapshot': snapshot, 'resume': bool(prior)}))
        return
    if phase != 'finalize':
        fail('invalid transaction phase')
    checkpoint = transaction_snapshot()
    if (checkpoint.get('bundle_sha256') != bundle_sha or checkpoint.get('snapshot') != snapshot or
            type(checkpoint.get('resume')) is not bool or (checkpoint['resume'] and not prior)):
        fail('evidence/source/verifier changed during execution')
    if not prior:
        verification = parsed(os.environ['OMS_COMPLETION_VERIFICATION'])
        validate_verification(verification, snapshot)
        receipt = {'schema': 1, 'kind': 'oms-plan-completion', 'completion_kind': KINDS[action],
                   'bundle_sha256': bundle_sha, 'bundle': bundle, 'bundle_raw': bundle_raw.decode('utf-8'),
                   'original_task': original,
                   'original_plan_sha256': sha(plan_raw), 'snapshot': snapshot,
                   'verification': verification, 'ts': context['ts']}
        raw = encoded(receipt)
        ref = {'path': '.oms/plan/completions/' + sha(raw) + '.json', 'sha256': sha(raw)}
        def immutable(old):
            if old and old != raw:
                fail('conflicting immutable completion receipt')
            return raw
        DURABLE['atomic_mutate'](ref['path'], immutable, state_repo, missing_ok=True,
                                 create_parent=True, max_output=MAX_JSON, label='completion receipt')
    plan['tasks'][task['id']] = completion_task(receipt, ref)
    new_plan = json.dumps(plan, indent=2, ensure_ascii=False, allow_nan=False).encode()
    def publish(old):
        if old != plan_raw:
            fail('plan CAS drift before publication')
        return new_plan
    DURABLE['atomic_mutate']('.oms/plan/tasks.json', publish, state_repo,
                             max_output=MAX_JSON, label='completion plan')
    print('plan: %s %s' % (task['id'], KINDS[action]))


def execute(state_repo, bundle_path, expected, commit_proof):
    global _COMMIT_PROOF
    _COMMIT_PROOF = commit_proof
    reset_streams()
    raw = read(state_repo, bundle_path)
    if sha(raw) != digest(expected):
        fail('bundle changed before verifier')
    bundle = parsed(raw)
    repo = os.path.realpath(os.environ['OMS_REPO'])
    current = strict_plan(state_repo, os.environ['OMS_PLAN_FILE'])
    if sha(read(state_repo, '.oms/plan/tasks.json')) != bundle['plan_sha256']:
        fail('plan changed before actual verifier execution')
    original = dict(bundle['original_contract'], state=bundle['state'])
    action = next(name for name, kind in KINDS.items() if kind == bundle['kind'])
    caller_owner(state_repo, bundle, original)
    provenance = transaction_snapshot()['snapshot']['provenance']
    before = check_bundle(repo, state_repo, bundle, original, current['tasks'], current.get('plan_id'), action, provenance)
    if transaction_snapshot()['snapshot'] != before:
        fail('frozen inputs changed before actual verifier execution')
    verification = capture_verifier(repo, bundle['verifier']['command'], before)
    reset_streams()
    caller_owner(state_repo, bundle, original)
    after = check_bundle(repo, state_repo, bundle, original, current['tasks'], current.get('plan_id'), action, provenance)
    if before != after:
        fail('source/evidence/verifier changed during actual execution')
    print(json.dumps(verification))


def locked_plan(arguments):
    import signal
    engine = runpy.run_path(str(HERE / 'agent-events.py'))
    repo = os.path.realpath(os.environ.get('OMS_STATE_REPO') or os.environ['OMS_REPO'])
    path = DURABLE['canonical_repo_path'](repo, '.oms/lifecycle/events.jsonl', 'completion lifecycle')
    if not arguments or os.path.realpath(arguments[0]) != str(HERE / 'agent-plan-engine.py'):
        fail('invalid locked plan entrypoint')
    sys.argv = arguments
    def interrupted(signum, frame):
        raise SystemExit(128 + signum)
    for name in ('SIGTERM', 'SIGHUP', 'SIGINT'):
        if hasattr(signal, name):
            signal.signal(getattr(signal, name), interrupted)
    try:
        # Hold the actual writer primitive across engine load, receipt write
        # and atomic plan publication; none of these paths append lifecycle.
        with engine['file_lock'](Path(path)):
            runpy.run_path(arguments[0], run_name='__main__')
    except engine['OpsError'] as exc:
        fail(str(exc))


if __name__ == '__main__':
    try:
        if len(sys.argv) > 2 and sys.argv[1] == '--locked-plan':
            locked_plan(sys.argv[2:])
        elif sys.argv[1:] == ['--resume']:
            print(1 if transaction_snapshot()['resume'] else 0)
        else:
            fail('verifier phase must run through agent-plan authority context')
    except (ValueError, KeyError, TypeError, OSError, subprocess.TimeoutExpired) as exc:
        print('error: completion verification refused: %s' % exc, file=sys.stderr)
        sys.exit(2)
