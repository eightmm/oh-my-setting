#!/usr/bin/env python3
"""Problem campaign core.

Waves: Controller runs seats through agent-call and verifies candidates locally.
validate(repo, manifest) returns the normalized projection and canonical bytes.
initialize(repo, manifest) validates and creates/resumes a campaign; it returns
its directory and projection. Ledger(directory) loads the authoritative budget
and routes from its digest-bound saved manifest; it exposes reserve, settle,
spent and remaining. Reservations count forever even if unsettled.
All ledger transitions are serialized by the campaign lock; status reads without it.
"""
import argparse
import concurrent.futures
import contextlib
import functools
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import shlex
import stat
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location('durable_jsonl', ROOT / 'scripts/lib/durable-jsonl.py')
_durable = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_durable)
KINDS = ('seat', 'consolidator', 'verifier', 'repair', 'final')
EFFORTS = ('auto', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra')


class BudgetRefused(ValueError):
    """A reservation the ledger refused; terminal means no later call can succeed."""

    def __init__(self, reason, terminal=False):
        super().__init__(reason)
        self.reason = reason
        self.terminal = terminal


def utc_now():
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())


def error(message):
    raise ValueError(message)


def obj(value, keys, required, label):
    if not isinstance(value, dict):
        error(label + ' must be an object')
    extra = set(value) - set(keys)
    if extra:
        error('unknown key in %s: %s' % (label, sorted(extra)[0]))
    if set(required) - set(value):
        error('%s missing required key: %s' % (label, sorted(set(required) - set(value))[0]))
    return value


def string(value, label, limit=4096):
    if not isinstance(value, str) or not value.strip() or len(value.encode('utf-8')) > limit or '\x00' in value:
        error(label + ' must be a nonempty bounded string')
    return value


def integer(value, lo, hi, label):
    if type(value) is not int or not lo <= value <= hi:
        error('%s must be %d..%d' % (label, lo, hi))
    return value


def digest_file(repo, path, digest, label):
    string(path, label + ' path', 1024)
    if Path(path).is_absolute() or '..' in Path(path).parts:
        error(label + ' path must be repository-relative')
    target = (repo / path).resolve()
    if os.path.commonpath((str(repo), str(target))) != str(repo) or not target.is_file():
        error(label + ' must be a regular file inside the repository')
    if not isinstance(digest, str) or not re.fullmatch('[0-9a-f]{64}', digest):
        error(label + ' sha256 must be lowercase hex')
    h = hashlib.sha256()
    with target.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    if h.hexdigest() != digest:
        error(label + ' digest mismatch')


@functools.lru_cache(maxsize=1)
def provider_names():
    command = '. "$1"; oms_provider_supported_names'
    output = subprocess.check_output(['bash', '-c', command, 'campaign', str(ROOT / 'scripts/lib/provider-registry.sh')], text=True)
    return frozenset(output.split())


def validate(repo, manifest, _saved_providers=None):
    repo = Path(repo).resolve(strict=True)
    obj(manifest, ('problem','base','verify','candidate_formats','groups','budget','public_data','consolidator'), ('problem','base','verify','candidate_formats','groups','budget'), 'manifest')
    if 'public_data' in manifest and type(manifest['public_data']) is not bool:
        error('public_data must be a boolean')
    p = obj(manifest['problem'], ('file','sha256'), ('file','sha256'), 'problem')
    digest_file(repo, p['file'], p['sha256'], 'problem')
    base = manifest['base']
    if not isinstance(base, str) or not re.fullmatch('[0-9a-f]{40}|[0-9a-f]{64}', base):
        error('base must be a full commit SHA')
    head = subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'], text=True).strip()
    if base != head:
        error('base must equal HEAD')
    v = obj(manifest['verify'], ('command','timeout_s','inputs'), ('command','timeout_s','inputs'), 'verify')
    string(v['command'], 'verify command'); integer(v['timeout_s'],1,3600,'verify timeout_s')
    if not isinstance(v['inputs'], list) or len(v['inputs']) > 128:
        error('verify inputs must be a list of at most 128 items')
    for item in v['inputs']:
        obj(item, ('path','sha256'), ('path','sha256'), 'verify input')
        digest_file(repo, item['path'], item['sha256'], 'verify input')
    formats = manifest['candidate_formats']
    if not isinstance(formats,list) or not formats or len(formats)>4 or any(x not in ('patch','lean','text','counterexample') for x in formats) or len(set(formats)) != len(formats):
        error('candidate_formats must be a nonempty unique subset')
    b = obj(manifest['budget'], ('max_concurrent','total_calls','deadline_s','max_waves','stall_waves','per_provider','verifier'), ('max_concurrent','total_calls','deadline_s','max_waves','stall_waves','verifier'), 'budget')
    for key, lo, hi in (('max_concurrent',1,16),('total_calls',1,200),('deadline_s',60,86400),('max_waves',1,20),('stall_waves',1,b['max_waves'] if type(b['max_waves']) is int else 0)):
        integer(b[key],lo,hi,'budget '+key)
    providers = provider_names() if _saved_providers is None else _saved_providers
    caps = b.get('per_provider', {})
    if not isinstance(caps, dict) or len(caps)>len(providers): error('per_provider must be an object')
    for name, cap in caps.items():
        if name not in providers: error('unknown provider: ' + name)
        obj(cap, ('calls','concurrency'), ('calls','concurrency'), 'provider cap')
        integer(cap['calls'],1,200,'provider calls'); integer(cap['concurrency'],1,16,'provider concurrency')
    verifier = obj(b['verifier'], ('concurrency','timeout_s','max_artifact_bytes'), ('concurrency','timeout_s','max_artifact_bytes'), 'verifier budget')
    integer(verifier['concurrency'],1,4,'verifier concurrency'); integer(verifier['timeout_s'],1,3600,'verifier timeout_s'); integer(verifier['max_artifact_bytes'],1,1073741824,'max_artifact_bytes')
    def validate_route(route):
        obj(route, ('provider','model','effort'), ('provider','model'), 'route')
        if not isinstance(route['provider'],str) or route['provider'] not in providers: error('unknown provider: %s' % route['provider'])
        string(route['model'],'model',256)
        if len(route['model']) > 160 or re.search('[\x00-\x1f\x7f]', route['model']) or route['model'] == 'provider-default':
            error('route model must be at most 160 characters, without control characters, and not provider-default')
        if route['provider'] == 'pi' and manifest.get('public_data') is not True:
            error('pi routes are public-data only: set public_data to true')
        if 'effort' in route:
            if route['provider'] == 'cursor': error('cursor routes have no effort control')
            if route['effort'] not in EFFORTS: error('route effort must be one of: ' + ', '.join(EFFORTS))
        return dict(route, cap_source='provider' if route['provider'] in caps else 'global')
    if 'consolidator' in manifest:
        validate_route(manifest['consolidator'])
    groups = manifest['groups']
    if not isinstance(groups,list) or not 1 <= len(groups) <= 8: error('groups must contain 1..8 groups')
    normalized_groups = []
    ids = set()
    for g in groups:
        obj(g, ('id','formulation','k','routes'), ('id','formulation','k','routes'), 'group')
        if not isinstance(g['id'],str) or not re.fullmatch('[a-z0-9][a-z0-9_-]{0,63}',g['id']) or g['id'] in ids: error('group id must be a unique safe slug')
        ids.add(g['id']); string(g['formulation'],'formulation'); integer(g['k'],1,8,'group k')
        if not isinstance(g['routes'],list) or not 1 <= len(g['routes']) <= 64: error('routes must contain 1..64 routes')
        routes = [validate_route(route) for route in g['routes']]
        normalized_groups.append(dict(g, routes=routes))
    canonical = json.dumps(manifest, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')
    cid = hashlib.sha256(canonical).hexdigest()[:16]
    if b['total_calls'] < b['max_waves']:
        error('total_calls must cover one consolidator per wave')
    seats = min(sum(g['k'] for g in groups)*b['max_waves'], b['total_calls'] - b['max_waves'])
    projection = dict(manifest, campaign_id=cid, groups=normalized_groups,
                      totals={'planned_seat_calls':seats, 'consolidator_calls_per_wave':1, 'remaining_headroom':b['total_calls']-seats-b['max_waves']})
    return projection, canonical


@contextlib.contextmanager
def locked(directory):
    path = directory / 'events.lock'
    dir_info = os.lstat(directory)
    if not stat.S_ISDIR(dir_info.st_mode):
        error('campaign state must be a real directory')
    dir_fd = os.open(str(directory), os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0) | getattr(os, 'O_NOFOLLOW', 0))
    if not _durable.same_file(dir_info, os.fstat(dir_fd)):
        os.close(dir_fd)
        error('campaign state directory changed')
    flags = os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0)
    try:
        fd = os.open(str(path), flags, 0o600)
    except BaseException:
        if dir_fd is not None: os.close(dir_fd)
        raise
    try:
        info = os.fstat(fd)
        named = os.lstat(path)
        if not _durable.safe_regular(info) or not _durable.safe_regular(named) or not _durable.same_file(info, named):
            error('campaign lock must be a regular non-symlink, non-hard-linked file')
        import fcntl
        fcntl.flock(fd,fcntl.LOCK_EX)
        named_dir = os.lstat(directory)
        named_lock = os.lstat(path)
        if (not stat.S_ISDIR(named_dir.st_mode) or
                not _durable.same_file(dir_info, named_dir) or
                (dir_fd is not None and not _durable.same_file(os.fstat(dir_fd), named_dir)) or
                not _durable.safe_regular(named_lock) or
                not _durable.same_file(os.fstat(fd), named_lock)):
            error('campaign lock or state directory changed while acquiring lock')
        yield
    finally:
        os.close(fd)
        if dir_fd is not None: os.close(dir_fd)


def append(directory, event):
    data = (json.dumps(event, sort_keys=True, separators=(',', ':'))+'\n').encode()
    # The writer changes cwd internally, so isolate it from callers and threads.
    subprocess.run([sys.executable, str(ROOT/'scripts/lib/durable-jsonl.py'), '--label','events.jsonl','append',str(directory/'events.jsonl')], input=data, check=True)


def durable_call(function, *args, **kwargs):
    try:
        return function(*args, **kwargs)
    except SystemExit as exc:
        raise ValueError('campaign durable storage validation failed') from exc


class Ledger:
    def __init__(self, directory):
        self.directory = Path(directory).resolve(strict=True)
        self._load_manifest()

    def _load_manifest(self):
        raw = durable_call(_durable.read_no_follow, str(self.directory),
                           str(self.directory / 'manifest.json'), label='campaign manifest')
        manifest = json.loads(raw)
        canonical = json.dumps(manifest, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')
        if raw != canonical or hashlib.sha256(raw).hexdigest()[:16] != self.directory.name:
            error('campaign manifest digest mismatch')
        self.budget = manifest['budget']
        self.providers = {route['provider'] for group in manifest['groups'] for route in group['routes']}
        if 'consolidator' in manifest: self.providers.add(manifest['consolidator']['provider'])

    def _rows(self):
        path = self.directory / 'events.jsonl'
        raw = durable_call(_durable.read_no_follow, str(self.directory), str(path), label='events.jsonl', missing_ok=False)
        if not raw.endswith(b'\n'):
            error('campaign events contain an incomplete row')
        rows = [json.loads(line) for line in raw.splitlines()]
        if not rows or not isinstance(rows[0], dict) or rows[0].get('type') != 'campaign_started':
            error('campaign events missing start')
        if rows[0].get('budget') != self.budget:
            error('campaign start budget differs from manifest')
        return rows

    def _view(self):
        self._load_manifest()
        reservations, settled = {}, set()
        for row in self._rows():
            if not isinstance(row, dict): error('invalid campaign event')
            if not isinstance(row.get('type'), str): error('invalid campaign event type')
            if row.get('type') == 'reservation':
                rid = row.get('id')
                if (not isinstance(rid, str) or not re.fullmatch('res_[0-9a-f]{32}', rid) or
                        rid in reservations or not isinstance(row.get('provider'), str) or
                        row.get('provider') not in self.providers or not isinstance(row.get('kind'), str) or
                        row.get('kind') not in KINDS):
                    error('invalid reservation event')
                reservations[rid] = row
            elif row.get('type') == 'settled':
                rid = row.get('id')
                if not isinstance(rid, str) or rid not in reservations or rid in settled:
                    error('invalid settlement event')
                settled.add(rid)
        return reservations, settled

    def spent(self):
        with locked(self.directory):
            reservations, _ = self._view()
            return {'total_calls':len(reservations), 'per_provider':{p:sum(r['provider']==p for r in reservations.values()) for p in set(r['provider'] for r in reservations.values())}}

    def remaining(self):
        spent = self.spent()
        return {'total_calls': max(0,self.budget['total_calls']-spent['total_calls']), 'per_provider':{p:max(0,c['calls']-spent['per_provider'].get(p,0)) for p,c in self.budget.get('per_provider',{}).items()}}

    def reserve(self, provider, kind, limit=None):
        """Return {'id': ...} or raise BudgetRefused; limit caps lifetime reservations."""
        with locked(self.directory):
            reservations, settled = self._view()
            if provider not in self.providers or kind not in KINDS: error('invalid provider or kind')
            active = [r for key,r in reservations.items() if key not in settled]
            caps = self.budget.get('per_provider',{}).get(provider,{})
            if (len(reservations) >= self.budget['total_calls'] or
                    (limit is not None and sum(r['kind'] == kind for r in reservations.values()) >= limit)): raise BudgetRefused('total_calls exhausted', True)
            if sum(r['provider']==provider for r in reservations.values()) >= caps.get('calls',self.budget['total_calls']): raise BudgetRefused('provider calls exhausted')
            if len(active) >= self.budget['max_concurrent']: raise BudgetRefused('max_concurrent exhausted')
            if sum(r['provider']==provider for r in active) >= caps.get('concurrency',self.budget['max_concurrent']): raise BudgetRefused('provider concurrency exhausted')
            if kind == 'verifier' and sum(r['kind']=='verifier' for r in active) >= self.budget['verifier']['concurrency']: raise BudgetRefused('verifier concurrency exhausted')
            rid = 'res_' + uuid.uuid4().hex
            append(self.directory, {'type':'reservation','id':rid,'provider':provider,'kind':kind})
            return {'id': rid}

    def events(self):
        with locked(self.directory):
            self._view()
            return self._rows()

    def emit(self, kind, **fields):
        with locked(self.directory):
            self._view()
            append(self.directory, dict(fields, type=kind, ts=utc_now()))

    def abandon_unsettled(self):
        """A killed controller leaves open reservations: they stay spent but stop holding concurrency."""
        with locked(self.directory):
            reservations, settled = self._view()
            for rid in reservations:
                if rid not in settled: append(self.directory, {'type':'settled','id':rid,'outcome':'abandoned'})

    def settle(self, reservation_id, outcome):
        string(outcome,'outcome',256)
        with locked(self.directory):
            reservations, settled = self._view()
            if reservation_id not in reservations or reservation_id in settled: return False
            append(self.directory, {'type':'settled','id':reservation_id,'outcome':outcome})
            return True


def initialize(repo, manifest, state_repo=None):
    repo = Path(repo).resolve(strict=True)
    state = Path(state_repo or repo).resolve(strict=True)
    canonical_input = json.dumps(manifest, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')
    campaign_id = hashlib.sha256(canonical_input).hexdigest()[:16]
    saved_path = '.oms/plan/campaigns/' + campaign_id + '/manifest.json'
    saved = durable_call(_durable.read_no_follow, str(state), saved_path, label='campaign manifest', missing_ok=True)
    saved_providers = None
    if saved == canonical_input:
        saved_providers = {route['provider'] for group in manifest['groups'] for route in group['routes']}
        saved_providers.update(manifest['budget'].get('per_provider', {}))
        if 'consolidator' in manifest: saved_providers.add(manifest['consolidator']['provider'])
    projection, canonical = validate(repo, manifest, saved_providers)
    relative = '.oms/plan/campaigns/' + projection['campaign_id'] + '/manifest.json'
    target = Path(durable_call(_durable.canonical_repo_path, str(state), relative, label='campaign', create_parent=True))
    directory = target.parent
    with locked(directory):
        existing = durable_call(_durable.read_no_follow, str(state), str(target), label='campaign manifest', missing_ok=True)
        if not existing and not os.path.lexists(target):
            subprocess.run([sys.executable, str(ROOT/'scripts/lib/durable-jsonl.py'), '--label', 'campaign manifest', 'write', str(target)], input=canonical, check=True)
        elif existing != canonical:
            error('campaign manifest differs from existing bytes')
        events = directory/'events.jsonl'
        raw = durable_call(_durable.read_no_follow, str(state), str(events), label='events.jsonl', missing_ok=True)
        now = utc_now()
        if raw is None or raw == b'':
            append(directory, {'type':'campaign_started','base':manifest['base'], 'problem':manifest['problem'], 'verify':manifest['verify'], 'budget':manifest['budget'], 'started_at':now, 'ts':now})
        else:
            Ledger(directory)._view()
            append(directory, {'type':'campaign_resumed','ts':now})
    return directory, projection


CANDIDATE_CAP = 4
DIGEST_CAP = 16 * 1024
CLAIM_CAP = 32
CLAIM_TEXT_CAP = 16 * 1024
TAIL_CAP = 2048
SEAT_WALL = 1200
EXTENSIONS = {'patch': 'patch', 'lean': 'lean', 'text': 'txt', 'counterexample': 'txt'}
_OUTBOUND = re.compile(r'^SKIPPED: outbound provider context contains sensitive-looking content', re.M)
_STREAM_CUT = re.compile(r'^stop-reason: provider=antigravity reason=stream_truncated', re.M)


def epoch(text):
    import calendar
    return calendar.timegm(time.strptime(text, '%Y-%m-%dT%H:%M:%SZ'))


def route_key(route):
    return (route['provider'], route['model'], route.get('effort', ''))


def parse_candidates(text, formats):
    """Fenced blocks tagged with a format; an unclosed block is dropped."""
    found, current = [], None
    for line in text.splitlines():
        if current is None:
            match = re.fullmatch(r'```([A-Za-z]+)[ \t]*', line)
            if match: current = (match.group(1).lower(), [])
        elif line.rstrip() == '```':
            found.append((current[0], '\n'.join(current[1]) + '\n')); current = None
        else:
            current[1].append(line)
    return found


def patch_paths(data):
    """Every path a patch touches, from git's own parse of it."""
    out = subprocess.run(['git', 'apply', '--numstat', '-z', '-'], input=data, capture_output=True, cwd=tempfile.gettempdir())
    if out.returncode != 0: error('patch does not parse')
    tokens, paths, i = out.stdout.split(b'\0'), [], 0
    while i < len(tokens) and tokens[i]:
        parts = tokens[i].split(b'\t', 2)
        if len(parts) != 3: error('patch does not parse')
        if parts[2] == b'':
            paths += tokens[i + 1:i + 3]; i += 3
        else:
            paths.append(parts[2]); i += 1
    return [os.path.normpath(p.decode('utf-8', 'replace')).casefold() for p in paths]


@contextlib.contextmanager
def running(directory):
    fd = os.open(str(directory / 'controller.lock'), os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    try:
        try:
            import fcntl
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            error('campaign is already running')
        yield
    finally:
        os.close(fd)


class Controller:
    def __init__(self, repo, state, directory, manifest):
        self.repo, self.state, self.manifest = repo, state, manifest
        self.ledger = Ledger(directory)
        self.cid = directory.name
        self.budget = manifest['budget']
        self.formats = manifest['candidate_formats']
        self.mu = threading.Lock()
        self.dead, self.dead_providers = set(), set()
        caps = self.budget.get('per_provider', {})
        providers = {r['provider'] for g in manifest['groups'] for r in g['routes']}
        if 'consolidator' in manifest: providers.add(manifest['consolidator']['provider'])
        self.sems = {p: threading.BoundedSemaphore(caps.get(p, {}).get('concurrency', self.budget['max_concurrent'])) for p in providers}
        self.seat_limit = self.budget['total_calls'] - self.budget['max_waves']
        self.stop = threading.Event()
        self.stop_reason = None

    def put(self, relative, data):
        path = durable_call(_durable.canonical_repo_path, str(self.state), relative, label='campaign artifact', create_parent=True)
        subprocess.run([sys.executable, str(ROOT/'scripts/lib/durable-jsonl.py'), '--label', 'campaign artifact', 'write', path], input=data, check=True)
        return Path(path)

    def halt(self, reason):
        with self.mu:
            if self.stop_reason is None: self.stop_reason = reason
        self.stop.set()

    def remaining(self, started):
        return started + self.budget['deadline_s'] - time.time()

    def live(self, group):
        with self.mu:
            return [r for r in group['routes'] if route_key(r) not in self.dead and r['provider'] not in self.dead_providers]

    def digest(self, rows, gid):
        lines, ids = [], []
        verdicts = {r['id']: r for r in rows if r.get('type') == 'candidate_verdict'}
        for r in rows:
            if r.get('type') == 'candidate_recorded' and r.get('group') == gid:
                v = verdicts.get(r['id'], {})
                lines.append('%s wave=%s route=%s/%s format=%s verdict=%s exit=%s\n' % (r['id'], r['wave'], r['route'][0], r['route'][1], r['format'], v.get('verdict', 'none'), v.get('exit_code', '-')) + 'receipt=%s\n' % v.get('receipt_sha256', '-'))
                ids.append(r['id'])
        while lines and len(''.join(lines).encode()) > DIGEST_CAP:
            lines.pop(0); ids.pop(0)
        return ''.join(lines), ids

    def prompt(self, group, rows):
        problem = (self.repo / self.manifest['problem']['file']).read_text(encoding='utf-8', errors='replace')
        digest, parents = self.digest(rows, group['id'])
        text = ['# Problem', problem, '', '# Approach', group['formulation'], '', '# Reply format',
                'Reply with up to %d candidates, each in one fenced block whose tag is its format. Allowed formats: %s.' % (CANDIDATE_CAP, ', '.join(self.formats)),
                'Use a unified diff against the repository root for patch and lean candidates, for example a block opened with three backticks and the word patch.']
        if digest: text += ['', '# Earlier candidates of this approach', digest]
        notes = [r for r in rows if r.get('type') == 'consolidation_recorded']
        if notes and isinstance(notes[-1].get('claims'), list):
            bounded = []
            for claim in notes[-1]['claims']:
                try:
                    payload = json.dumps(bounded + [claim], ensure_ascii=False)
                    if len(payload.encode('utf-8')) <= DIGEST_CAP: bounded.append(claim)
                except (TypeError, ValueError, UnicodeError, RecursionError):
                    continue
            if bounded: text += ['', '# Unverified notes with citations', json.dumps(bounded, ensure_ascii=False)]
        return '\n'.join(text) + '\n', parents

    def call(self, route, seat_dir, prompt_file, started):
        cmd = ['bash', str(ROOT/'scripts/agent-call.sh'), '--repo', str(self.repo), '--artifact-dir', str(seat_dir), '--to', route['provider'], '--model', route['model']]
        if route.get('effort'): cmd += ['--reasoning-effort', route['effort']]
        cmd += ['--prompt-file', str(prompt_file)]
        env = dict(os.environ, OMS_PEER_TIMEOUT='%ds' % max(1, int(min(self.remaining(started), SEAT_WALL))))
        proc = subprocess.run(cmd, capture_output=True, text=True, env=env, stdin=subprocess.DEVNULL)
        paths = [l[10:].strip() for l in proc.stdout.splitlines() if l.startswith('artifact: ')]
        text, unreadable = '', False
        if len(paths) == 1:
            spec = importlib.util.spec_from_file_location('peer_artifacts', ROOT / 'scripts/lib/peer_artifacts.py')
            module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
            try:
                text = module.artifact_sections(Path(paths[0]))[0]
            except OSError:
                unreadable = True
        if proc.returncode == 0:
            if len(paths) != 1 or unreadable: return 'failed', 'agent-call left no readable artifact'
            if _STREAM_CUT.search(text): return 'failed', 'provider stopped at its print timeout'
            return 'ok', text
        if proc.returncode == 3 and _OUTBOUND.search(text): return 'refused', 'outbound scrubber refused the prompt'
        if proc.returncode == 124: return 'failed', 'timeout'
        return 'failed', ('exit %d: %s' % (proc.returncode, (text or proc.stderr).strip()))[:500]

    def seat(self, wave, group, index, started, rows):
        while not self.stop.is_set():
            live = self.live(group)
            if not live: return []
            route = live[((wave - 1) * group['k'] + index) % len(live)]
            with self.sems[route['provider']]:
                if self.stop.is_set(): return []
                if self.remaining(started) <= 0: self.halt('deadline'); return []
                try:
                    rid = self.ledger.reserve(route['provider'], 'seat', self.seat_limit)['id']
                except BudgetRefused as refusal:
                    if refusal.terminal:
                        self.ledger.emit('budget_exhausted', wave=wave, reason=refusal.reason); self.halt('budget'); return []
                    if refusal.reason != 'provider calls exhausted': error('controller bug: ' + refusal.reason)
                    with self.mu: self.dead_providers.add(route['provider'])
                    self.ledger.emit('route_exhausted', provider=route['provider'], group=group['id'], wave=wave)
                    continue
                try:
                    return self.run_seat(wave, group, index, route, rid, started, rows)
                except BaseException:
                    self.ledger.settle(rid, 'error'); raise
        return []

    def run_seat(self, wave, group, index, route, rid, started, rows):
        base = '.oms/artifacts/campaigns/%s/seats/w%d-%s-%d' % (self.cid, wave, group['id'], index)
        prompt, parents = self.prompt(group, rows)
        prompt_file = self.put(base + '/prompt.md', prompt.encode('utf-8'))
        status, text = self.call(route, prompt_file.parent, prompt_file, started)
        label = [route['provider'], route['model'], route.get('effort', '')]
        if status == 'refused':
            self.ledger.settle(rid, 'refused'); self.halt('outbound_refused'); return []
        if status == 'failed':
            with self.mu: self.dead.add(route_key(route))
            self.ledger.emit('seat_failed', wave=wave, group=group['id'], route=label, error=text, reservation=rid)
            self.ledger.settle(rid, 'failed'); return []
        made, blocks = [], parse_candidates(text, self.formats)
        limit = min(self.budget['verifier']['max_artifact_bytes'], 1024 * 1024 - 1)
        if not blocks: self.ledger.emit('candidate_rejected', wave=wave, group=group['id'], route=label, reason='no candidate block')
        for n, (fmt, body) in enumerate(blocks):
            data = body.encode('utf-8')
            reason = ('too many candidates' if n >= CANDIDATE_CAP else 'format not allowed: ' + fmt if fmt not in self.formats else
                      'empty candidate' if not body.strip() else 'candidate too large' if len(data) > limit else '')
            if reason:
                self.ledger.emit('candidate_rejected', wave=wave, group=group['id'], route=label, reason=reason); continue
            digest = hashlib.sha256(data).hexdigest()
            self.put('.oms/artifacts/campaigns/%s/%s.%s' % (self.cid, digest, EXTENSIONS[fmt]), data)
            cand = {'id': 'cand_' + uuid.uuid4().hex[:16], 'group': group['id'], 'wave': wave, 'route': label, 'parents': parents, 'format': fmt, 'sha256': digest, 'size': len(data)}
            self.ledger.emit('candidate_recorded', **cand); made.append(cand)
        self.ledger.settle(rid, 'ok')
        return made

    def consolidate(self, wave, started):
        route = self.manifest.get('consolidator')
        if not route or self.remaining(started) <= 0: return
        label = [route['provider'], route['model'], route.get('effort', '')]
        try:
            rid = self.ledger.reserve(route['provider'], 'consolidator')['id']
        except BudgetRefused as refusal:
            self.ledger.emit('seat_failed', wave=wave, group='consolidator', route=label, error=refusal.reason)
            return
        try:
            rows = self.ledger.events()
            digests = [self.digest(rows, g['id'])[0] for g in self.manifest['groups']]
            prompt = ('# Wave digest\n' + ''.join(digests) +
                      '\nReply with exactly one fenced json block containing a list of claims with text, candidates and receipts arrays. Cite matching candidate ids and receipt digests.\n')
            path = self.put('.oms/artifacts/campaigns/%s/consolidator/w%d/prompt.md' % (self.cid, wave), prompt.encode())
            status, text = self.call(route, path.parent, path, started)
            if status != 'ok':
                self.ledger.emit('seat_failed', wave=wave, group='consolidator', route=label, error=text, reservation=rid)
                self.ledger.settle(rid, status)
                if status == 'refused': self.halt('outbound_refused')
                return
            blocks = parse_candidates(text, ['json'])
            try:
                claims = json.loads(blocks[0][1]) if len(blocks) == 1 and blocks[0][0] == 'json' else None
            except (ValueError, TypeError, RecursionError):
                claims = None
            if not isinstance(claims, list): claims = [None]
            candidates = {r['id'] for r in rows if r.get('type') == 'candidate_recorded'}
            recorded = {r['id']: r.get('receipt_sha256') for r in rows if r.get('type') == 'candidate_verdict' and r.get('id') in candidates}
            accepted, rejected, text_bytes = [], 0, 0
            for claim in claims:
                try:
                    valid = (isinstance(claim, dict) and set(claim) == {'text', 'candidates', 'receipts'} and
                             isinstance(claim['text'], str) and claim['text'].strip() and
                             isinstance(claim['candidates'], list) and claim['candidates'] and
                             isinstance(claim['receipts'], list) and len(claim['candidates']) == len(claim['receipts']) and
                             all(isinstance(cid, str) and isinstance(receipt, str) and recorded.get(cid) == receipt and receipt is not None
                                 for cid, receipt in zip(claim['candidates'], claim['receipts'])))
                    size = len(claim['text'].encode('utf-8')) if valid else 0
                    payload_size = len(json.dumps(accepted + [claim], ensure_ascii=False).encode('utf-8')) if valid else 0
                    valid = (valid and size <= 4096 and len(accepted) < CLAIM_CAP and
                             text_bytes + size <= CLAIM_TEXT_CAP and payload_size <= DIGEST_CAP)
                except (TypeError, ValueError, UnicodeError, RecursionError):
                    valid = False
                if valid:
                    accepted.append(claim)
                    text_bytes += size
                else: rejected += 1
            self.ledger.emit('consolidation_recorded', wave=wave, claims=accepted, rejected_count=rejected)
            self.ledger.settle(rid, 'ok')
        except Exception as exc:
            self.ledger.emit('seat_failed', wave=wave, group='consolidator', route=label, error=type(exc).__name__, reservation=rid)
            self.ledger.settle(rid, 'error')

    def evaluate(self, cand):
        if cand['format'] not in ('patch', 'lean'): return {'verdict': 'unverified'}
        rel = '.oms/artifacts/campaigns/%s/%s.%s' % (self.cid, cand['sha256'], EXTENSIONS[cand['format']])
        data = durable_call(_durable.read_no_follow, str(self.state), rel, label='campaign artifact')
        if hashlib.sha256(data).hexdigest() != cand['sha256']: return {'verdict': 'error', 'reason': 'artifact digest mismatch'}
        verify, pinned = self.manifest['verify'], [self.manifest['problem']] + self.manifest['verify']['inputs']
        try:
            touched = patch_paths(data)
        except ValueError as exc:
            return {'verdict': 'error', 'reason': str(exc)}
        protected = {os.path.normpath(x['file' if 'file' in x else 'path']).casefold() for x in pinned}
        if protected & set(touched): return {'verdict': 'error', 'reason': 'verifier-tamper'}
        scripts = ROOT / 'scripts/scratch-worktree.sh'
        scratch = subprocess.check_output(['bash', str(scripts), 'add', '--repo', str(self.repo), '--base', self.manifest['base'], '--owner-pid', str(os.getpid())], text=True).strip()
        try:
            for step in (['--check'], []):
                done = subprocess.run(['git', '-C', scratch, 'apply'] + step + ['-'], input=data, capture_output=True)
                if done.returncode != 0: return {'verdict': 'error', 'reason': 'patch does not apply'}
            try:
                for x in pinned:
                    digest_file(Path(scratch).resolve(), x['file' if 'file' in x else 'path'], x['sha256'], 'pinned input')
            except ValueError as exc:
                return {'verdict': 'error', 'reason': 'verifier-tamper' if 'digest mismatch' in str(exc) else str(exc)}
            return self.run_verify(scratch, min(verify['timeout_s'], self.budget['verifier']['timeout_s']))
        finally:
            subprocess.run(['bash', str(scripts), 'remove', '--repo', str(self.repo), '--force', scratch], capture_output=True)

    def run_verify(self, scratch, timeout):
        # A file, not a pipe: a background child holding the pipe cannot stall the read.
        with tempfile.TemporaryFile() as sink:
            proc = subprocess.Popen(['bash', '-c', self.manifest['verify']['command']], cwd=scratch, stdout=sink, stderr=subprocess.STDOUT, start_new_session=True)
            verdict = None
            try:
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                verdict = 'timeout'
            try: os.killpg(proc.pid, signal.SIGKILL)
            except OSError: pass
            proc.wait()
            size = sink.seek(0, 2)
            sink.seek(max(0, size - TAIL_CAP))
            out = sink.read()
        return {'verdict': verdict or ('pass' if proc.returncode == 0 else 'fail'), 'exit_code': proc.returncode, 'tail': out.decode('utf-8', 'replace')}

    def wave(self, wave, started):
        self.ledger.emit('wave_started', wave=wave)
        rows = self.ledger.events()
        order = [(g, i) for i in range(max(g['k'] for g in self.manifest['groups'])) for g in self.manifest['groups'] if i < g['k']]
        with concurrent.futures.ThreadPoolExecutor(max_workers=self.budget['max_concurrent']) as pool:
            made = [f.result() for f in [pool.submit(self.seat, wave, g, i, started, rows) for g, i in order]]
        cands = [c for part in made for c in part]
        with concurrent.futures.ThreadPoolExecutor(max_workers=self.budget['verifier']['concurrency']) as pool:
            verdicts = list(pool.map(self.evaluate, cands))
        winner = None
        for cand, v in zip(cands, verdicts):
            receipt = dict(v, candidate=cand['id'], sha256=cand['sha256'], base=self.manifest['base'])
            receipt_sha256 = hashlib.sha256(json.dumps(receipt, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
            self.ledger.emit('candidate_verdict', id=cand['id'], receipt=receipt, receipt_sha256=receipt_sha256, **v)
            if v['verdict'] == 'pass' and winner is None:
                final = self.evaluate(cand)
                if final['verdict'] == 'pass': winner = (cand, receipt, dict(final, candidate=cand['id'], sha256=cand['sha256'], base=self.manifest['base']))
                else: self.ledger.emit('final_verify_failed', id=cand['id'], receipt=final)
        if self.stop_reason is None: self.ledger.emit('wave_completed', wave=wave, candidates=len(cands))
        return winner

    def finish(self, kind, **fields):
        self.ledger.emit(kind, **fields)

    def summary(self, rows, code):
        spent, left = self.ledger.spent(), self.ledger.remaining()
        counts = {}
        for r in rows:
            if r.get('type') == 'candidate_verdict': counts[r['verdict']] = counts.get(r['verdict'], 0) + 1
        solved = next((r for r in rows if r.get('type') == 'campaign_solved'), None)
        stopped = next((r for r in reversed(rows) if r.get('type') == 'campaign_stopped'), None)
        print('campaign %s: %s waves=%d calls=%d/%d remaining=%d verdicts=%s winner=%s' % (
            self.cid, 'solved' if solved else 'stopped (%s)' % stopped['reason'], sum(r.get('type') == 'wave_completed' for r in rows),
            spent['total_calls'], self.budget['total_calls'], left['total_calls'],
            ','.join('%s:%d' % kv for kv in sorted(counts.items())) or '-', solved['candidate'] if solved else '-'))
        if solved and solved.get('command'): print(solved['command'])
        return code


def run_campaign(repo, state, directory, manifest, now):
    ctl = Controller(Path(repo).resolve(), Path(state).resolve(), directory, manifest)
    with running(directory):
        rows = ctl.ledger.events()
        if any(r.get('type') == 'campaign_solved' for r in rows): return ctl.summary(rows, 0)
        stopped = next((r for r in rows if r.get('type') == 'campaign_stopped'), None)
        if stopped: return ctl.summary(rows, 2 if stopped.get('reason') == 'outbound_refused' else 3)
        ctl.ledger.abandon_unsettled()
        for r in rows:
            if r.get('type') == 'seat_failed' and r.get('group') != 'consolidator': ctl.dead.add(tuple(r['route']))
            elif r.get('type') == 'route_exhausted': ctl.dead_providers.add(r['provider'])
        started = epoch(rows[0]['started_at']) if rows[0].get('started_at') else now
        wave = sum(r.get('type') == 'wave_completed' for r in rows) + 1
        budget = manifest['budget']
        reason = 'max_waves'
        while wave <= budget['max_waves']:
            if ctl.remaining(started) <= 0: reason = 'deadline'; break
            if not any(ctl.live(g) for g in manifest['groups']): reason = 'routes_exhausted'; break
            ctl.stop, ctl.stop_reason = threading.Event(), None
            winner = ctl.wave(wave, started)
            if winner:
                cand, receipt, final_receipt = winner
                artifact = ctl.state / ('.oms/artifacts/campaigns/%s/%s.%s' % (ctl.cid, cand['sha256'], EXTENSIONS[cand['format']]))
                command = 'oms patch-admit --repo %s --patch %s --verify %s' % tuple(shlex.quote(str(x)) for x in (ctl.repo, artifact, manifest['verify']['command'])) if cand['format'] == 'patch' else ''
                handoff = {'candidate': str(artifact), 'sha256': cand['sha256'], 'base': manifest['base'], 'verify': manifest['verify']['command'], 'receipt': receipt, 'final_receipt': final_receipt, 'command': command}
                ctl.put('.oms/plan/campaigns/%s/handoff.json' % ctl.cid, (json.dumps(handoff, sort_keys=True) + '\n').encode())
                ctl.finish('campaign_solved', candidate=cand['id'], sha256=cand['sha256'], receipt=receipt, final_receipt=final_receipt, command=command)
                return ctl.summary(ctl.ledger.events(), 0)
            if ctl.stop_reason: reason = ctl.stop_reason; break
            if wave >= budget['max_waves']: break
            if wave >= budget['stall_waves']: reason = 'stall'; break
            if ctl.remaining(started) <= 0: reason = 'deadline'; break
            reservations = [r for r in ctl.ledger.events() if r.get('type') == 'reservation']
            route = manifest.get('consolidator')
            seat_room = sum(r.get('kind') == 'seat' for r in reservations) < ctl.seat_limit
            call_room = len(reservations) + 1 < budget['total_calls']
            available = False
            if route and seat_room and call_room:
                caps = budget.get('per_provider', {})
                for group in manifest['groups']:
                    for candidate_route in ctl.live(group):
                        provider = candidate_route['provider']
                        spent = sum(r['provider'] == provider for r in reservations)
                        spent += int(provider == route['provider'])
                        if spent < caps.get(provider, {}).get('calls', budget['total_calls']): available = True
            if available: ctl.consolidate(wave, started)
            if ctl.stop_reason: reason = ctl.stop_reason; break
            wave += 1
        ctl.finish('campaign_stopped', reason=reason)
        return ctl.summary(ctl.ledger.events(), 2 if reason == 'outbound_refused' else 3)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--repo',required=True); parser.add_argument('--campaign',required=True); parser.add_argument('--dry-run',action='store_true'); parser.add_argument('--status',action='store_true'); parser.add_argument('--state-repo')
    args = parser.parse_args()
    try:
        with open(args.campaign, encoding='utf-8') as stream:
            data = stream.read(1024*1024+1)
        if len(data.encode('utf-8')) > 1024*1024: error('manifest exceeds size limit')
        def pairs(items):
            result = {}
            for key,value in items:
                if key in result: error('duplicate key: '+key)
                result[key] = value
            return result
        manifest = json.loads(data, object_pairs_hook=pairs)
        if args.status:
            if args.dry_run: error('--dry-run and --status are mutually exclusive')
            if not args.state_repo: error('--state-repo is required')
            canonical = json.dumps(manifest, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')
            cid = hashlib.sha256(canonical).hexdigest()[:16]
            directory = Path(args.state_repo).resolve() / '.oms/plan/campaigns' / cid
            ledger = Ledger(directory)
            for attempt in range(3):
                try:
                    rows = ledger._rows()
                    break
                except ValueError as exc:
                    if attempt == 2 or 'durable storage validation failed' not in str(exc): raise
            reservations = [r for r in rows if r.get('type') == 'reservation']
            solved = next((r for r in rows if r.get('type') == 'campaign_solved'), None)
            stopped = next((r for r in reversed(rows) if r.get('type') == 'campaign_stopped'), None)
            state = 'solved' if solved else 'stopped (%s)' % stopped['reason'] if stopped else 'running'
            print('campaign %s: %s waves=%d calls=%d/%d remaining=%d last_event=%s winner=%s' %
                  (cid, state, sum(r.get('type') == 'wave_completed' for r in rows), len(reservations),
                   ledger.budget['total_calls'], max(0, ledger.budget['total_calls'] - len(reservations)),
                   rows[-1]['type'], solved['candidate'] if solved else '-'))
            for provider in sorted(ledger.providers):
                count = sum(r.get('provider') == provider for r in reservations)
                cap = ledger.budget.get('per_provider', {}).get(provider, {}).get('calls', ledger.budget['total_calls'])
                print('provider %s: calls=%d/%d remaining=%d' % (provider, count, cap, max(0, cap-count)))
            if solved and solved.get('command'): print(solved['command'])
            return 0
        if args.dry_run:
            projection, _ = validate(args.repo,manifest)
            print(json.dumps(projection, sort_keys=True, indent=2))
            return 0
        if os.environ.get('OMS_HARNESS_CHILD') == '1' or os.environ.get('OMS_HARNESS_DELEGATE_DEPTH', '0') not in ('', '0'):
            error('a harness child cannot run campaign waves; the owner runs them')
        if not args.state_repo: error('--state-repo is required')
        now = time.time()  # deadline origin for core-era ledgers without started_at
        directory, _ = initialize(args.repo, manifest, args.state_repo)
        return run_campaign(args.repo, args.state_repo, directory, manifest, now)
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError) as exc:
        print('error: %s' % exc, file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
