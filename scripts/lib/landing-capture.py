"""Native landing payload quota and private generation ownership.

Writers hold landings.jsonl's lock. A private, retained hardlink is the
generation witness; journal rows or matching bytes alone never authorize GC.
This is the cooperating OMS boundary, not protection from hostile same-UID code.
"""
import hashlib
import json
import math
import os
from pathlib import Path
import re
import runpy
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from contextlib import contextmanager

HERE = Path(__file__).resolve().parent
D = runpy.run_path(str(HERE / 'durable-jsonl.py'))
MAX_STREAM = 32 * 1024 * 1024
MAX_OBJECT = 16 * 1024 * 1024
DEFAULT_BYTES = 128 * 1024 * 1024
DEFAULT_COUNT = 256


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def identity(info):
    return [info.st_dev, info.st_ino]


def regular(path, maximum=MAX_STREAM, links=1):
    before = os.lstat(path)
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0) | getattr(os, 'O_BINARY', 0)
    with os.fdopen(os.open(path, flags), 'rb') as handle:
        opened = os.fstat(handle.fileno())
        if (not stat.S_ISREG(opened.st_mode) or D['is_reparse'](opened) or
                opened.st_nlink != links or identity(before) != identity(opened) or
                (hasattr(os, 'getuid') and opened.st_uid != os.getuid()) or opened.st_size > maximum):
            raise ValueError('unsafe or oversized capture input')
        raw = handle.read(maximum + 1)
        after = os.fstat(handle.fileno())
    named = os.lstat(path)
    if (len(raw) > maximum or identity(named) != identity(after) or
            identity(opened) != identity(after) or after.st_nlink != links or
            opened.st_size != after.st_size or opened.st_mtime_ns != after.st_mtime_ns or
            opened.st_ctime_ns != after.st_ctime_ns):
        raise ValueError('capture input changed while reading')
    return raw, after


def rows(repo):
    raw = D['read_no_follow'](repo, '.oms/landings.jsonl', 'landing journal', max_bytes=MAX_STREAM, missing_ok=True)
    if raw and not raw.endswith(b'\n'):
        raise ValueError('truncated landing journal')
    result = []
    for line in (raw or b'').splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError('non-object landing journal row')
        result.append(row)
    return result


def append(repo, event, reservation, **extra):
    row = dict(schema=1, event=event, landing_id=reservation['landing_id'],
               ts=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), managed_capture=reservation, **extra)
    D['append'](str(Path(repo) / '.oms/landings.jsonl'), encoded(row), 'landing journal')
    return row


def reservations(repo):
    result = {}
    for row in rows(repo):
        if row.get('event') != 'capture-reserved':
            continue
        item = row.get('managed_capture')
        if (not isinstance(item, dict) or item.get('version') != 1 or
                not re.fullmatch(r'[0-9a-f]{32}', item.get('generation', '')) or
                type(item.get('bytes')) is not int or not 0 <= item['bytes'] <= 2 * MAX_OBJECT or
                not isinstance(item.get('directory'), str) or
                item.get('namespace') != digest(os.path.realpath(repo).encode()) or
                not re.fullmatch(r'[0-9a-f]{64}', item.get('sha256', '')) or
                not isinstance(item.get('landing_id'), str) or
                not re.fullmatch(r'land-[A-Za-z0-9._-]{1,150}', item['landing_id'])):
            raise ValueError('malformed managed capture reservation')
        generation = item['generation']
        if generation in result:
            raise ValueError('duplicate managed capture reservation')
        result[generation] = item
    return result


def directory(path):
    info = os.lstat(path)
    if (not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or D['is_reparse'](info) or
            (hasattr(os, 'getuid') and info.st_uid != os.getuid()) or
            (os.name != 'nt' and stat.S_IMODE(info.st_mode) != 0o700)):
        raise ValueError('capture generation directory is not private')
    return info


def certificate(item):
    root = item['directory']
    if os.path.realpath(root) != root or os.path.basename(root) != 'oms-landing-capture-' + item['generation']:
        raise ValueError('capture directory moved or was redirected')
    info = directory(root)
    raw, _ = regular(os.path.join(root, 'owner.json'), 16384)
    value = json.loads(raw)
    if value.get('reservation') != item or value.get('directory') != identity(info):
        raise ValueError('capture generation owner mismatch')
    return value


def save(item, value):
    root = item['directory']
    info = directory(root)
    if value['directory'] != identity(info):
        raise ValueError('capture directory replaced')
    descriptor, temporary = tempfile.mkstemp(prefix='owner-', dir=root)
    try:
        with os.fdopen(descriptor, 'wb') as handle:
            handle.write(encoded(value))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, os.path.join(root, 'owner.json'))
        D['_fsync_directory_path'](root, 'capture owner')
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def limit(name, default):
    value = os.environ.get(name, str(default))
    if not re.fullmatch(r'[1-9][0-9]{0,12}', value):
        raise ValueError('invalid ' + name)
    return int(value)


def release_ready(item, log, repo):
    candidates = [r for r in log if r.get('event') == 'capture-release-ready' and
                  r.get('managed_capture', {}).get('generation') == item['generation']]
    for row in candidates:
        proof = row.get('release_proof', {})
        if not isinstance(proof, dict):
            raise ValueError('malformed native release readiness evidence')
        terminal = [r for r in log[:log.index(row)] if digest(encoded(r)) == proof.get('terminal_event_sha256')]
        objects = proof.get('objects', {})
        if (row.get('schema') != 1 or row.get('managed_capture') != item or
                row.get('landing_id') != item['landing_id'] or proof.get('version') != 1 or
                proof.get('reservation_sha256') != digest(encoded(item)) or
                not re.fullmatch(r'[0-9a-f]{64}', proof.get('certificate_sha256', '')) or
                len(terminal) != 1 or terminal[0].get('event') != 'capture-terminal' or
                terminal[0].get('managed_capture') != item or not isinstance(objects, dict) or
                'capture' not in objects or set(objects) - {'capture', 'publication'}):
            raise ValueError('invalid native release readiness evidence')
        value = proof.get('certificate')
        if (not isinstance(value, dict) or set(value) not in (
                {'reservation', 'directory', 'objects', 'terminal'},
                {'reservation', 'directory', 'objects', 'terminal', 'admission'}) or
                value.get('reservation') != item or
                digest(encoded(value)) != proof['certificate_sha256'] or value.get('objects') != objects or
                not isinstance(value.get('directory'), list) or len(value['directory']) != 2 or
                any(type(n) is not int or n < 0 for n in value['directory'])):
            raise ValueError('release certificate inventory is unproven')
        sealed = value.get('terminal')
        if (not isinstance(sealed, dict) or set(sealed) != {'event_sha256', 'time', 'reason', 'proof'} or
                type(sealed.get('time')) not in (int, float) or sealed['time'] < 0 or
                (type(sealed['time']) is float and not math.isfinite(sealed['time'])) or
                sealed.get('event_sha256') != digest(encoded(terminal[0])) or
                terminal[0].get('reason') != sealed.get('reason') or
                terminal[0].get('native_terminal_proof') != sealed.get('proof') or
                sealed.get('proof') != terminal_proof(item, value, sealed.get('reason'), log, repo)):
            raise ValueError('release lacks its exact native terminal provenance')
        for name, obj in objects.items():
            expected_path = (os.path.join(item['directory'], item['landing_id'] + '.patch') if name == 'capture'
                             else os.path.join(repo, '.oms/landing-patches', item['landing_id'] + '.patch'))
            if (not isinstance(obj, dict) or set(obj) != {'identity', 'size', 'path', 'anchor'} or
                    obj.get('path') != expected_path or obj.get('anchor') != os.path.join(item['directory'], name + '.anchor') or
                    not isinstance(obj.get('identity'), list) or len(obj['identity']) != 2 or
                    any(type(n) is not int or n < 0 for n in obj['identity'])):
                raise ValueError('release object generation inventory is incomplete')
        for obj in objects.values():
            if (not isinstance(obj, dict) or type(obj.get('size')) is not int or
                    obj['size'] * 2 != item['bytes'] or obj['size'] < 0):
                raise ValueError('release object size contradicts reservation')
    if candidates and any(encoded(r['release_proof']) != encoded(candidates[0]['release_proof']) for r in candidates):
        raise ValueError('conflicting native release readiness')
    return candidates[-1] if candidates else None


def usage(repo):
    items, log = reservations(repo), rows(repo)
    total_count = total_bytes = 0
    for row in log:
        if row.get('event') in ('capture-release-ready', 'capture-released'):
            item = row.get('managed_capture')
            if not isinstance(item, dict) or item.get('generation') not in items:
                raise ValueError('release has no native reservation')
    for generation, item in items.items():
        charge = item['bytes']
        try:
            ready = release_ready(item, log, repo)
            released = [r for r in log if r.get('event') == 'capture-released' and
                        r.get('managed_capture', {}).get('generation') == generation]
            for row in released:
                if (not ready or row.get('schema') != 1 or row.get('managed_capture') != item or
                        row.get('landing_id') != item['landing_id'] or
                        row.get('release_ready_sha256') != digest(encoded(ready)) or
                        log.index(row) <= log.index(ready)):
                    raise ValueError('invalid native capture release evidence')
            exists = os.path.lexists(item['directory'])
            if released and not exists:
                continue
            if exists:
                value = certificate(item)
                objects = value.get('objects', {})
                if not isinstance(objects, dict) or set(objects) - {'capture', 'publication'}:
                    raise ValueError('unknown quota object inventory')
                if not objects:
                    raise ValueError('unproved quota object inventory')
                for obj in objects.values():
                    if type(obj.get('size')) is not int or obj['size'] * 2 != item['bytes']:
                        raise ValueError('quota reservation contradicts actual object size')
                    if os.path.lexists(obj['anchor']):
                        links = os.lstat(obj['anchor']).st_nlink
                        if links not in (1, 2):
                            raise ValueError('unknown quota anchor links')
                        object_bytes(item, obj, links)
                    else:
                        raise ValueError('missing quota generation anchor')
            else:
                # A crash before certificate creation is retained, never treated
                # as zero or as a caller-declared release permitting allocation.
                raise ValueError('quota generation missing without native release proof')
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            # Unknown bounded generations remain reserved at their maximum
            # peak size; they cannot waive quota or authorize collection.
            charge = 2 * MAX_OBJECT
        total_count += 1
        total_bytes += charge
    return total_count, total_bytes


def allocate(repo, source, landing, expected, execution_repo=None):
    if not re.fullmatch(r'land-[A-Za-z0-9._-]{1,150}', landing):
        raise ValueError('invalid native landing identity')
    raw, _ = regular(source, MAX_OBJECT)
    if digest(raw) != expected:
        raise ValueError('source changed before quota reservation')
    if any(item['landing_id'] == landing for item in reservations(repo).values()):
        raise ValueError('native landing already reserved; use its existing recovery path')
    count, size = usage(repo)
    # Charge both private admission input and separate publication inode,
    # including crashes and failures. Hardlinked anchors need no extra bytes.
    if count + 1 > limit('OMS_LANDING_CAPTURE_COUNT', DEFAULT_COUNT) or size + 2 * len(raw) > limit('OMS_LANDING_CAPTURE_BYTES', DEFAULT_BYTES):
        raise ValueError('managed landing capture quota exhausted; retained/unknown evidence remains charged')
    execution_repo = os.path.realpath(execution_repo or repo)
    root = None
    for parent in (tempfile.gettempdir(), os.path.dirname(repo)):
        parent = os.path.realpath(parent)
        try:
            inside = False
            for tree in (repo, execution_repo):
                try:
                    inside = inside or os.path.commonpath([parent, tree]) == tree
                except ValueError:
                    pass  # Different Windows drives cannot overlap.
            if inside or os.stat(parent).st_dev != os.stat(repo).st_dev:
                continue
            root = parent
            break
        except (OSError, ValueError):
            continue
    if root is None:
        raise ValueError('no same-filesystem private capture parent outside state repository')
    generation = uuid.uuid4().hex
    item = dict(version=1, generation=generation, landing_id=landing, namespace=digest(repo.encode()),
                directory=os.path.join(root, 'oms-landing-capture-' + generation), bytes=2 * len(raw), sha256=expected)
    # Reserve before mkdir/copy: an interrupted creation is charged, never
    # rediscovered/adopted from a filename, a PID or matching contents.
    append(repo, 'capture-reserved', item)
    os.mkdir(item['directory'], 0o700)
    value = dict(reservation=item, directory=identity(directory(item['directory'])), objects={}, terminal=None)
    save(item, value)
    D['_fsync_directory_path'](root, 'capture generation parent')
    target = os.path.join(item['directory'], item['landing_id'] + '.patch')
    with open(target, 'xb') as handle:
        handle.write(raw)
        handle.flush()
        os.fsync(handle.fileno())
    anchor = os.path.join(item['directory'], 'capture.anchor')
    os.link(target, anchor)
    value['objects']['capture'] = dict(identity=identity(os.lstat(anchor)), size=len(raw), path=target, anchor=anchor)
    save(item, value)
    return target


def find(repo, capture=None, landing=None):
    found = [item for item in reservations(repo).values()
             if (capture is not None and os.path.join(item['directory'], item['landing_id'] + '.patch') == capture) or
             (landing is not None and item['landing_id'] == landing)]
    if len(found) != 1:
        raise ValueError('unique native capture reservation unavailable')
    return found[0]


def object_bytes(item, obj, links=2):
    raw, info = regular(obj['anchor'], MAX_OBJECT, links)
    if identity(info) != obj['identity'] or len(raw) != obj['size'] or digest(raw) != item['sha256']:
        raise ValueError('native generation anchor changed')
    return raw


def publish(repo, capture, target):
    item = find(repo, capture=capture)
    value = certificate(item)
    obj = value['objects']['capture']
    raw = object_bytes(item, obj)
    if identity(os.lstat(capture)) != obj['identity']:
        raise ValueError('admission input was replaced')
    target = D['canonical_repo_path'](repo, target, 'landing publication', True)
    anchor = os.path.join(item['directory'], 'publication.anchor')
    with open(anchor, 'xb') as handle:
        handle.write(raw)
        handle.flush()
        os.fsync(handle.fileno())
    value['objects']['publication'] = dict(identity=identity(os.lstat(anchor)), size=len(raw), path=target, anchor=anchor)
    save(item, value)
    os.link(anchor, target)
    D['_fsync_directory_path'](os.path.dirname(target), 'landing publication')


def admission_records(repo, report):
    relative = os.path.relpath(os.path.abspath(report), repo)
    if not relative.startswith('.oms/artifacts/'):
        raise ValueError('admission witness requires canonical internal report')
    raw = D['read_no_follow'](repo, relative, 'native admission report', max_bytes=MAX_STREAM)
    index = D['read_no_follow'](repo, '.oms/artifacts/index.jsonl', 'native admission index', max_bytes=MAX_STREAM)
    if not index.endswith(b'\n'):
        raise ValueError('incomplete native admission index')
    found = [json.loads(line) for line in index.splitlines() if line.strip()]
    found = [row for row in found if isinstance(row, dict) and row.get('artifact') == relative]
    if len(found) != 1 or found[0].get('artifact_sha256') != digest(raw):
        raise ValueError('unique unchanged native admission receipt unavailable')
    return relative, raw, found[0]


def validate_admission_scope(item, capture, attempt, raw, row, created, engine, terminal=None):
    text = raw.decode('utf-8')
    verdict = 'ADMIT' if row.get('exit') == 0 else 'REJECT'
    if (row.get('operation_id') != attempt or
            text.splitlines()[0:1] != ['# Patch admission: ' + verdict] or
            [line for line in text.splitlines() if line.startswith('- patch: ')] != ['- patch: ' + capture]):
        raise ValueError('admission producer/report scope differs from the actual local input')
    born = engine['parse_ts'](created['ts'])
    produced = engine['parse_ts'](row['ts'])
    if produced < born or (terminal is not None and produced > engine['parse_ts'](terminal['ts'])):
        raise ValueError('admission receipt is outside its actual operation lifetime')


def record_admission(repo, capture, attempt, report, result):
    # Only the actual private input publisher stamps this witness after its
    # durable report/index append. Existing public inputs never acquire it.
    matches = [i for i in reservations(repo).values()
               if os.path.join(i['directory'], i['landing_id'] + '.patch') == capture]
    if not matches:
        return
    item = matches[0]
    value = certificate(item)
    indexed_bytes(repo, capture)
    relative, raw, row = admission_records(repo, report)
    if (result not in ('0', '1') or row.get('kind') != 'patch-admit' or
            type(row.get('exit')) is not int or row['exit'] != int(result) or
            row.get('patch_sha256') != item['sha256'] or
            row.get('patch_external') != dict(name=os.path.basename(capture), owned=False, sha256=item['sha256'])):
        raise ValueError('native admission result does not match private input')
    engine = runpy.run_path(str(HERE / 'agent-events.py'))
    events = engine['read_rows'](engine['event_path'](Path(repo)))
    created = [r for r in events if r.get('attempt_id') == attempt and r.get('event_type') == 'attempt.created']
    if (len(created) != 1 or created[0].get('provider') != 'local' or
            created[0].get('tool') != 'patch-admit' or created[0].get('parent_attempt_id') or
            created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
            created[0].get('refs', {}).get('reference_input_sha256') != digest(capture.encode())):
        raise ValueError('native admission operation scope unavailable')
    validate_admission_scope(item, capture, attempt, raw, row, created[0], engine)
    proof = dict(version=1, generation=item['generation'], attempt=attempt,
                 created_sha256=digest(encoded(created[0])), report=relative,
                 report_sha256=digest(raw), index_event_id=row.get('event_id'),
                 index_row_sha256=digest(encoded(row)), exit=int(result))
    if value.get('admission') not in (None, proof):
        raise ValueError('conflicting native admission witness')
    value['admission'] = proof
    save(item, value)


def admission_proof(repo, item, value, expected):
    proof = value.get('admission')
    if (not isinstance(proof, dict) or proof.get('version') != 1 or
            proof.get('generation') != item['generation'] or proof.get('exit') != expected):
        raise ValueError('native admission outcome witness missing')
    _, raw, row = admission_records(repo, os.path.join(repo, proof['report']))
    if (digest(raw) != proof['report_sha256'] or row.get('event_id') != proof['index_event_id'] or
            digest(encoded(row)) != proof['index_row_sha256'] or row.get('kind') != 'patch-admit' or
            type(row.get('exit')) is not int or row['exit'] != expected or row.get('patch_sha256') != item['sha256']):
        raise ValueError('native admission evidence changed')
    engine = runpy.run_path(str(HERE / 'agent-events.py'))
    events = engine['read_rows'](engine['event_path'](Path(repo)))
    created = [r for r in events if r.get('attempt_id') == proof['attempt'] and r.get('event_type') == 'attempt.created']
    _, attempts = engine['load_projection'](Path(repo))
    operation = attempts.get(proof['attempt'], {})
    capture = value['objects']['capture']['path']
    if (len(created) != 1 or digest(encoded(created[0])) != proof['created_sha256'] or
            created[0].get('provider') != 'local' or created[0].get('tool') != 'patch-admit' or
            created[0].get('refs', {}).get('reference_input_sha256') != digest(capture.encode()) or
            created[0].get('refs', {}).get('reference_writer') != 'landing-reference-v1' or
            operation.get('state') != 'done' or operation.get('terminal') is not True):
        raise ValueError('actual admission publisher has not completed')
    terminal = [r for r in events if r.get('attempt_id') == proof['attempt'] and
                r.get('event_type') == 'attempt.state_changed' and r.get('to_state') == 'done']
    if len(terminal) != 1:
        raise ValueError('unique admission terminal producer event unavailable')
    validate_admission_scope(item, capture, proof['attempt'], raw, row, created[0], engine, terminal[0])
    return proof


def terminal_proof(item, value, reason, log, repo=None):
    own = [row for row in log if row.get('landing_id') == item['landing_id']]
    intents = [row for row in own if row.get('event') == 'intent']
    publication = value['objects'].get('publication')
    if reason == 'publication-without-intent':
        if intents or not publication:
            raise ValueError('no-intent publication history is contradictory')
        proof = admission_proof(repo, item, value, 0)
        return {'phase': 'publication-without-intent', 'admission_sha256': digest(encoded(proof))}
    if reason == 'admission-rejected':
        if intents or publication:
            raise ValueError('rejected input was already published')
        proof = admission_proof(repo, item, value, 1)
        return {'phase': 'admission-rejected', 'admission_sha256': digest(encoded(proof))}
    if reason == 'before-admission':
        if intents or publication:
            raise ValueError('pre-publication seal contradicts landing history')
        return {'phase': 'pre-publication', 'reason': reason}
    if reason not in ('complete', 'abandoned') or len(intents) != 1 or not publication:
        raise ValueError('exact native terminal landing identity unavailable')
    intent = intents[0]
    fields = ('landing_id', 'patch', 'patch_sha', 'base_sha', 'task', 'lease',
              'plan_receipt_sha', 'plan_done_receipt_sha', 'approval', 'approval_version')
    if intent.get('schema') != 1 or any(not isinstance(intent.get(k), str) for k in fields):
        raise ValueError('malformed native landing intent')
    canonical = dict(schema=1, **{k: intent[k] for k in fields})
    receipt = digest(json.dumps(canonical, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode())
    terminals = [row for row in own if row.get('event') in ('complete', 'abandoned')]
    if (intent['patch'] != publication['path'] or intent['patch_sha'] != item['sha256'] or
            intent.get('receipt_sha') != receipt or not terminals or
            any(row.get('event') != reason or row.get('schema') != 1 or row.get('receipt_sha') != receipt or
                row.get('plan_id', '') != intent.get('plan_id', '') or
                any(row.get(k) != intent[k] for k in fields) for row in terminals)):
        raise ValueError('native terminal receipt is missing or contradictory')
    return {'phase': 'landing', 'intent_sha256': digest(encoded(intent)),
            'terminal_sha256': digest(encoded(terminals[-1])), 'receipt_sha256': receipt}


def seal(repo, landing, reason):
    matches = [i for i in reservations(repo).values() if i['landing_id'] == landing]
    if not matches:
        return  # Legacy generations never acquire ownership retrospectively.
    item = find(repo, landing=landing)
    value = certificate(item)
    log = rows(repo)
    proof = terminal_proof(item, value, reason, log, repo)
    if (value['terminal'] and value['terminal'].get('proof') == proof and
            any(digest(encoded(row)) == value['terminal']['event_sha256'] for row in log)):
        return
    for obj in value['objects'].values():
        object_bytes(item, obj)
        if identity(os.lstat(obj['path'])) != obj['identity']:
            raise ValueError('cannot seal replaced capture')
    event = append(repo, 'capture-terminal', item, reason=reason, native_terminal_proof=proof)
    value['terminal'] = dict(event_sha256=digest(encoded(event)), time=time.time(), reason=reason, proof=proof)
    save(item, value)


def read_patch(repo, path, maximum=MAX_OBJECT):
    """Designated landing reader; ordinary files keep durable's nlink=1 rule."""
    repo = os.path.realpath(repo)
    path = os.path.abspath(os.path.join(repo, path))
    rel = os.path.relpath(path, repo)
    if os.path.dirname(rel) != os.path.join('.oms', 'landing-patches'):
        return D['read_no_follow'](repo, rel, 'patch evidence', max_bytes=maximum)
    named = os.lstat(path)
    matching = [item for item in reservations(repo).values()
                if os.path.basename(path) == item['landing_id'] + '.patch']
    if not matching and named.st_nlink == 1:
        return D['read_no_follow'](repo, rel, 'legacy patch evidence', max_bytes=maximum)
    candidates = []
    for item in matching:
        value = certificate(item)
        obj = value['objects'].get('publication')
        if obj and obj['path'] == path:
            candidates.append((item, obj))
    if len(candidates) != 1:
        raise ValueError('unique native publication ownership unavailable')
    item, obj = candidates[0]
    components = D['component_snapshot'](repo, path, 'landing patch')
    raw = object_bytes(item, obj)
    if len(raw) > maximum or identity(named) != obj['identity'] or identity(os.lstat(path)) != obj['identity']:
        raise ValueError('landing publication replaced or oversized')
    D['components_unchanged'](components, 'landing patch')
    return raw


def indexed_bytes(repo, path):
    """Called under the index writer lock before recording a new payload pin."""
    path = os.path.abspath(path)
    if os.path.dirname(path) == os.path.join(os.path.realpath(repo), '.oms', 'landing-patches'):
        return read_patch(repo, path)
    if (re.fullmatch(r'land-[A-Za-z0-9._-]+\.patch', os.path.basename(path)) and
            os.path.basename(os.path.dirname(path)).startswith('oms-landing-capture-')):
        if not any(os.path.join(i['directory'], i['landing_id'] + '.patch') == path
                   for i in reservations(repo).values()):
            return None  # Legacy private captures retain their original contract.
        item = find(repo, capture=path)
        value = certificate(item)
        obj = value['objects']['capture']
        raw = object_bytes(item, obj)
        if identity(os.lstat(path)) != obj['identity']:
            raise ValueError('indexed admission input was replaced')
        return raw
    return None


def reference_spellings(repo, path):
    """Literal path grammar shared by publishers and conservative GC pins."""
    result = set()
    for value in (path, os.path.relpath(path, repo), path.replace('\\', '/')):
        result.add(value)
        result.add(value.replace('/', '\\/'))
        for ascii_only in (True, False):
            result.add(json.dumps(value, ensure_ascii=ascii_only)[1:-1])
    return result


def reference_bytes(repo, trace=None):
    """Freeze supported evidence namespaces; unreadable/unknown inputs veto GC."""
    result, total, count = [], 0, 0

    def fail_walk(error):
        raise error

    for relative in ('.oms/plan', '.oms/artifacts', '.oms/tasks', '.oms/task'):
        root = os.path.join(repo, relative)
        if not os.path.lexists(root):
            continue
        if os.path.islink(root) or not os.path.isdir(root):
            raise ValueError('unsafe reference namespace')
        for parent, dirs, files in os.walk(root, followlinks=False, onerror=fail_walk):
            for name in dirs:
                if os.path.islink(os.path.join(parent, name)):
                    raise ValueError('redirected reference directory')
            for name in files:
                path = os.path.join(parent, name)
                raw, _ = regular(path)
                count += 1
                total += len(raw)
                if count > 8192 or total > 64 * 1024 * 1024:
                    raise ValueError('reference scan limit exceeded')
                if name.endswith('.jsonl') and raw and not raw.endswith(b'\n'):
                    raise ValueError('truncated reference journal')
                if name.endswith('.json'):
                    json.loads(raw)
                elif name.endswith('.jsonl'):
                    for line in raw.splitlines():
                        if line.strip():
                            row = json.loads(line)
                            if not isinstance(row, dict):
                                raise ValueError('non-object reference journal row')
                            if name == 'index.jsonl' and (row.get('artifact_external') or row.get('source_external')):
                                raise ValueError('external report reference closure is unproven')
                if trace:
                    proof, item = trace
                    private = os.path.join(item['directory'], item['landing_id'] + '.patch')
                    tokens = [v.encode() for v in reference_spellings(repo, private)] + [item['sha256'].encode()]
                    def historical(data):
                        for token in tokens:
                            data = data.replace(token, b'')
                        return data
                    if os.path.relpath(path, repo) == proof['report'] and digest(raw) == proof['report_sha256']:
                        raw = historical(raw)
                    elif os.path.relpath(path, repo) == '.oms/artifacts/index.jsonl':
                        lines = []
                        for line in raw.splitlines(keepends=True):
                            row = json.loads(line)
                            if row.get('event_id') == proof['index_event_id'] and digest(encoded(row)) == proof['index_row_sha256']:
                                line = historical(line)
                            lines.append(line)
                        raw = b''.join(lines)
                result.append(raw)
    return b'\n'.join(result)


def collect_object(item, obj):
    """Capture the pathname first; only then decide whether its inode is ours."""
    root = item['directory']
    directory(root)
    target = os.path.join(root, 'quarantine-' + os.path.basename(obj['anchor']))
    if not os.path.lexists(obj['anchor']):
        if os.path.lexists(target) or os.path.lexists(obj['path']):
            raise ValueError('missing anchor with remaining data; preserve')
        return  # Crash after both owned links were deleted.
    links = os.lstat(obj['anchor']).st_nlink
    if links not in (1, 2):
        raise ValueError('unknown external generation links; preserve')
    object_bytes(item, obj, links)
    # The anchor stays named and open throughout capture/revalidation, so its
    # inode cannot be recycled into a same-byte replacement generation.
    flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
    with os.fdopen(os.open(obj['anchor'], flags), 'rb') as anchor:
        if identity(os.fstat(anchor.fileno())) != obj['identity']:
            raise ValueError('anchor changed before quarantine')
        if not os.path.lexists(target):
            if not os.path.lexists(obj['path']):
                if links != 1:
                    raise ValueError('unlocated generation links; preserve')
                os.unlink(obj['anchor'])
                return  # Crash after deleting the quarantined owned link.
            os.rename(obj['path'], target)
            D['_fsync_directory_path'](os.path.dirname(obj['path']), 'capture quarantine source')
            D['_fsync_directory_path'](root, 'capture quarantine')
        captured = os.lstat(target)
        if not stat.S_ISREG(captured.st_mode) or identity(captured) != obj['identity']:
            # Never replace a newer occupant. A foreign object is left safely
            # in this private quarantine if no-clobber restoration cannot work.
            try:
                os.link(target, obj['path'], follow_symlinks=False)
            except OSError:
                pass
            else:
                D['_fsync_directory_path'](os.path.dirname(obj['path']), 'restored foreign capture')
                os.unlink(target)
                D['_fsync_directory_path'](root, 'restored capture quarantine')
            raise ValueError('foreign replacement preserved; generation remains charged')
        raw, _ = regular(target, MAX_OBJECT, 2)
        if digest(raw) != item['sha256']:
            raise ValueError('mutated generation preserved in quarantine')
        # Only the owned private directory is mutated after revalidation. Its
        # pathname is not shared with external publishers.
        os.unlink(target)
        os.unlink(obj['anchor'])
        D['_fsync_directory_path'](root, 'collected capture generation')


def collect(repo, days, apply, private_only=""):
    # private_only is an internal caller scope, never permission to ignore pins.
    log = rows(repo)
    evidence = reference_bytes(repo)
    for item in reservations(repo).values():
        if private_only and item['landing_id'] != private_only:
            continue
        if not os.path.lexists(item['directory']):
            continue
        try:
            value = certificate(item)
            terminal = value['terminal']
            if not terminal or terminal['time'] > time.time() - days * 86400:
                continue
            if not any(digest(encoded(row)) == terminal['event_sha256'] for row in log):
                continue
            if terminal.get('proof') != terminal_proof(item, value, terminal['reason'], log, repo):
                continue
            objects = value['objects']
            if private_only and ('publication' in objects or terminal['reason'] != 'admission-rejected'):
                continue
            scoped_evidence = evidence
            if terminal['reason'] in ('admission-rejected', 'publication-without-intent'):
                proof = admission_proof(repo, item, value, 1 if terminal['reason'] == 'admission-rejected' else 0)
                scoped_evidence = reference_bytes(repo, (proof, item))
            paths = [obj['path'] for obj in objects.values()]
            refs = [spelling.encode() for path in paths
                    for spelling in reference_spellings(repo, path)]
            if any(ref in scoped_evidence for ref in refs) or item['sha256'].encode() in scoped_evidence:
                continue
            # An unrelated/open landing may reference this payload. The
            # writer's own terminal intent is ownership history, not a pin.
            if any(row.get('patch') in paths and row.get('landing_id') != item['landing_id'] for row in log):
                continue
            known = {'owner.json'} | {os.path.basename(obj['anchor']) for obj in objects.values()}
            known |= {os.path.basename(obj['path']) for obj in objects.values() if os.path.dirname(obj['path']) == item['directory']}
            known |= {'quarantine-' + os.path.basename(obj['anchor']) for obj in objects.values()}
            if not set(os.listdir(item['directory'])).issubset(known):
                continue
            print('- managed-landing-capture: ' + item['generation'] + (' collect' if apply else ' would collect'))
            if apply:
                ready = release_ready(item, log, repo)
                if not ready:
                    for obj in objects.values():
                        links = os.lstat(obj['anchor']).st_nlink
                        if links not in (1, 2) or obj['size'] * 2 != item['bytes']:
                            raise ValueError('release inventory differs from reservation')
                        object_bytes(item, obj, links)
                    ready = append(repo, 'capture-release-ready', item,
                                   release_proof=dict(version=1, reservation_sha256=digest(encoded(item)),
                                                      certificate_sha256=digest(encoded(value)), certificate=value,
                                                      terminal_event_sha256=terminal['event_sha256'], objects=objects))
                    log.append(ready)
                # Process publication first: a shared-name replacement cannot
                # cause the still-referenced private admission input to vanish.
                for name in sorted(objects, reverse=True):
                    collect_object(item, objects[name])
                os.unlink(os.path.join(item['directory'], 'owner.json'))
                os.rmdir(item['directory'])
                D['_fsync_directory_path'](os.path.dirname(item['directory']), 'released capture generation')
                append(repo, 'capture-released', item, release_ready_sha256=digest(encoded(ready)))
        except (OSError, ValueError, KeyError, TypeError) as error:
            print('warning: managed capture preserved: ' + str(error), file=sys.stderr)
    count, size = usage(repo)
    print('managed-landing-capture: charged count=%d bytes=%d (legacy/history excluded)' % (count, size))


def gc_quiescent(repo, days, apply, private_only=""):
    # Called only after landings -> marker-set -> plan; then lifecycle -> index.
    if os.name == 'nt':
        print('managed-landing-capture: preserved (private ACL/quarantine cleanup unsupported on this host)')
        return
    engine = runpy.run_path(str(HERE / 'agent-events.py'))
    lifecycle_path = Path(repo) / '.oms/lifecycle/events.jsonl'
    lifecycle_lock = engine['file_lock'](lifecycle_path)

    @contextmanager
    def bounded_lifecycle_lock():
        try:
            lifecycle_lock.__enter__()
        except engine['OpsError'] as error:
            if str(error) == 'could not acquire state lock for %s' % lifecycle_path:
                raise SystemExit(75)
            raise
        try:
            yield
        except BaseException:
            lifecycle_lock.__exit__(*sys.exc_info())
            raise
        else:
            lifecycle_lock.__exit__(None, None, None)

    with bounded_lifecycle_lock():
        marker_dir = Path(repo) / '.oms/delegations'
        if os.path.lexists(marker_dir):
            info = marker_dir.lstat()
            if not stat.S_ISDIR(info.st_mode) or D['is_reparse'](info):
                raise ValueError('delegation marker directory is unproven')
            if any(marker_dir.iterdir()):
                print('managed-landing-capture: preserved (delegation quiescence unproven)')
                return
        _, attempts = engine['load_projection'](Path(repo))
        if any(a.get('terminal') is not True for a in attempts.values()):
            print('managed-landing-capture: preserved (native attempt not terminal)')
            return
        index_env = dict(os.environ,
                         OMS_CAPTURE_INDEX_CODE=('import runpy,sys; runpy.run_path(sys.argv[1])'
                                                 '["collect"](sys.argv[2], int(sys.argv[3]), bool(int(sys.argv[4])), sys.argv[5])'),
                         OMS_CAPTURE_HELPER=str(Path(__file__).resolve()), OMS_CAPTURE_REPO=repo,
                         OMS_CAPTURE_DAYS=str(days), OMS_CAPTURE_APPLY=str(int(apply)), OMS_CAPTURE_PRIVATE=private_only)
        command = ['bash', '-c',
                   '. "$1"; oms_with_file_lock "$2" python3 -c "$OMS_CAPTURE_INDEX_CODE" "$OMS_CAPTURE_HELPER" "$OMS_CAPTURE_REPO" "$OMS_CAPTURE_DAYS" "$OMS_CAPTURE_APPLY" "$OMS_CAPTURE_PRIVATE"',
                   'landing-capture-gc', str(HERE / 'file-lock.sh'),
                   str(Path(repo) / '.oms/artifacts/index.jsonl')]
        result = subprocess.run(command, env=index_env)
        if result.returncode == 75:
            raise SystemExit(75)
        if result.returncode:
            raise subprocess.CalledProcessError(result.returncode, command)


def cleanup_rejected(repo, landing):
    """Parent holds landings; bound the whole remaining cleanup, not each lock."""
    if os.name == 'nt':
        print('managed-landing-capture: preserved (bounded private cleanup unsupported on this host)')
        return
    deadline = time.monotonic() + 2
    inline = ('import runpy,sys; runpy.run_path(sys.argv[1])'
              '["gc_quiescent"](sys.argv[2],0,True,sys.argv[3])')
    command = ['bash', '-c',
               'python3 "$5" seal "$6" "$7" admission-rejected || exit $?; . "$1"; oms_with_file_lock "$2" oms_with_file_lock "$3" python3 -c "$4" "$5" "$6" "$7"',
               'landing-capture-rejected', str(HERE / 'file-lock.sh'),
               str(Path(repo) / '.oms/delegations/.marker-set-lock-target'),
               str(Path(repo) / '.oms/plan/tasks.json'), inline, str(Path(__file__).resolve()), repo, landing]
    # All descendants are trusted local lock/proof helpers in this owned session.
    # A deadline can interrupt quarantine or append: existing anchors and release
    # readiness keep that crash conservative. Reap before the outer fence leaves.
    process = None
    watched = (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)
    previous = {sig: signal.getsignal(sig) for sig in watched}
    def interrupted(sig, frame):
        raise SystemExit(128 + sig)
    try:
        for sig in watched:
            signal.signal(sig, interrupted)
        process = subprocess.Popen(command, env=dict(os.environ, OMS_LOCK_TIMEOUT='2'), start_new_session=True)
        try:
            result = process.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise SystemExit(75)
        if result:
            raise SystemExit(result)
    finally:
        # Ignore repeat cancellation until this invocation's group is gone;
        # releasing the parent's outer fence with a surviving writer is unsafe.
        for sig in watched:
            signal.signal(sig, signal.SIG_IGN)
        try:
            if process is not None and process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)


def gc(repo, days, apply):
    # Every supported CLI path takes landings -> marker-set -> plan before the
    # lifecycle and index locks. Keep this ordering in the native collector,
    # not only in the gc.sh caller, so direct helper invocation is safe too.
    if os.name == 'nt':
        print('managed-landing-capture: preserved (private ACL/quarantine cleanup unsupported on this host)')
        return
    lock_env = dict(os.environ)
    try:
        configured_timeout = int(lock_env.get('OMS_LOCK_TIMEOUT', '300'))
    except ValueError:
        configured_timeout = 300
    lock_env['OMS_LOCK_TIMEOUT'] = str(max(1, min(configured_timeout, 2)))
    inline = ('import runpy,sys; runpy.run_path(sys.argv[1])'
              '["gc_quiescent"](sys.argv[2], int(sys.argv[3]), bool(int(sys.argv[4])))')
    command = ['bash', '-c',
               '. "$1"; oms_try_file_lock "$2" oms_with_file_lock "$3" oms_with_file_lock "$4" python3 -c "$5" "$6" "$7" "$8" "$9"',
               'landing-capture-gc', str(HERE / 'file-lock.sh'),
               str(Path(repo) / '.oms/landings.jsonl'),
               str(Path(repo) / '.oms/delegations/.marker-set-lock-target'),
               str(Path(repo) / '.oms/plan/tasks.json'), inline,
               str(Path(__file__).resolve()), repo, str(days), str(int(apply))]
    result = subprocess.run(command, env=lock_env)
    if result.returncode == 75:
        raise SystemExit(75)
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command)


def main(args):
    if os.environ.get('OMS_HARNESS_CHILD') == '1' or os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') != '0':
        raise ValueError('native capture management is parent-only')
    action, repo, *args = args
    repo = os.path.realpath(repo)
    if action == 'allocate':
        print(allocate(repo, *args))
    elif action == 'publish':
        publish(repo, *args)
    elif action == 'seal':
        seal(repo, *args)
    elif action == 'gc':
        if len(args) != 2 or not re.fullmatch(r'(?:0|[1-9][0-9]{0,4})', args[0]):
            raise ValueError('gc requires non-negative DAYS <= 36500 and APPLY 0|1')
        days = int(args[0])
        if days > 36500 or args[1] not in ('0', '1'):
            raise ValueError('gc requires non-negative DAYS <= 36500 and APPLY 0|1')
        gc(repo, days, args[1] == '1')
    else:
        raise ValueError('unknown capture action')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print('error: landing capture: ' + str(exc), file=sys.stderr)
        sys.exit(2)
