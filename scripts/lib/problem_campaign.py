#!/usr/bin/env python3
"""Problem campaign core.

validate(repo, manifest) returns the normalized projection and canonical bytes.
initialize(repo, manifest) validates and creates/resumes a campaign; it returns
its directory and projection. Ledger(directory) loads the authoritative budget
and routes from its digest-bound saved manifest; it exposes reserve, settle,
spent and remaining. Reservations count forever even if unsettled.
All ledger transitions are serialized by the campaign lock.
"""
import argparse
import contextlib
import functools
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location('durable_jsonl', ROOT / 'scripts/lib/durable-jsonl.py')
_durable = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_durable)
KINDS = ('seat', 'consolidator', 'verifier', 'repair', 'final')


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
    obj(manifest, ('problem','base','verify','candidate_formats','groups','budget'), ('problem','base','verify','candidate_formats','groups','budget'), 'manifest')
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
    groups = manifest['groups']
    if not isinstance(groups,list) or not 1 <= len(groups) <= 8: error('groups must contain 1..8 groups')
    normalized_groups = []
    ids = set()
    for g in groups:
        obj(g, ('id','formulation','k','routes'), ('id','formulation','k','routes'), 'group')
        if not isinstance(g['id'],str) or not re.fullmatch('[a-z0-9][a-z0-9_-]{0,63}',g['id']) or g['id'] in ids: error('group id must be a unique safe slug')
        ids.add(g['id']); string(g['formulation'],'formulation'); integer(g['k'],1,8,'group k')
        if not isinstance(g['routes'],list) or not 1 <= len(g['routes']) <= 64: error('routes must contain 1..64 routes')
        routes = []
        for route in g['routes']:
            obj(route, ('provider','model','effort'), ('provider','model'), 'route')
            if not isinstance(route['provider'],str) or route['provider'] not in providers: error('unknown provider: %s' % route['provider'])
            string(route['model'],'model',256)
            if 'effort' in route: string(route['effort'],'effort',64)
            routes.append(dict(route, cap_source='provider' if route['provider'] in caps else 'global'))
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
    flags = os.O_CREAT | os.O_RDWR | getattr(os, 'O_NOFOLLOW', 0)
    fd = os.open(str(path), flags, 0o600)
    acquired = False
    try:
        info = os.fstat(fd)
        named = os.lstat(path)
        if not _durable.safe_regular(info) or not _durable.safe_regular(named) or not _durable.same_file(info, named):
            error('campaign lock must be a regular non-symlink, non-hard-linked file')
        if os.name == 'nt':
            import msvcrt
            if info.st_size == 0: os.write(fd,b'0')
            os.lseek(fd,0,0); msvcrt.locking(fd,msvcrt.LK_LOCK,1)
        else:
            import fcntl
            fcntl.flock(fd,fcntl.LOCK_EX)
        acquired = True
        yield
    finally:
        if os.name == 'nt' and acquired:
            os.lseek(fd,0,0); msvcrt.locking(fd,msvcrt.LK_UNLCK,1)
        os.close(fd)


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
            if row.get('type') == 'reservation':
                rid = row.get('id')
                if (not isinstance(rid, str) or not re.fullmatch('res_[0-9a-f]{32}', rid) or
                        rid in reservations or row.get('provider') not in self.providers or
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

    def reserve(self, provider, kind):
        with locked(self.directory):
            reservations, settled = self._view()
            if provider not in self.providers or kind not in KINDS: return 'invalid provider or kind'
            active = [r for key,r in reservations.items() if key not in settled]
            caps = self.budget.get('per_provider',{}).get(provider,{})
            if len(reservations) >= self.budget['total_calls']: return 'total_calls exhausted'
            if sum(r['provider']==provider for r in reservations.values()) >= caps.get('calls',self.budget['total_calls']): return 'provider calls exhausted'
            if len(active) >= self.budget['max_concurrent']: return 'max_concurrent exhausted'
            if sum(r['provider']==provider for r in active) >= caps.get('concurrency',self.budget['max_concurrent']): return 'provider concurrency exhausted'
            if kind == 'verifier' and sum(r['kind']=='verifier' for r in active) >= self.budget['verifier']['concurrency']: return 'verifier concurrency exhausted'
            rid = 'res_' + uuid.uuid4().hex
            append(self.directory, {'type':'reservation','id':rid,'provider':provider,'kind':kind})
            return rid

    def settle(self, reservation_id, outcome):
        string(outcome,'outcome',256)
        with locked(self.directory):
            reservations, settled = self._view()
            if reservation_id not in reservations or reservation_id in settled: return False
            append(self.directory, {'type':'settled','id':reservation_id,'outcome':outcome})
            return True


def initialize(repo, manifest):
    repo = Path(repo).resolve(strict=True)
    canonical_input = json.dumps(manifest, sort_keys=True, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode('utf-8')
    campaign_id = hashlib.sha256(canonical_input).hexdigest()[:16]
    saved_path = '.oms/plan/campaigns/' + campaign_id + '/manifest.json'
    saved = durable_call(_durable.read_no_follow, str(repo), saved_path, label='campaign manifest', missing_ok=True)
    saved_providers = None
    if saved == canonical_input:
        saved_providers = {route['provider'] for group in manifest['groups'] for route in group['routes']}
        saved_providers.update(manifest['budget'].get('per_provider', {}))
    projection, canonical = validate(repo, manifest, saved_providers)
    relative = '.oms/plan/campaigns/' + projection['campaign_id'] + '/manifest.json'
    target = Path(durable_call(_durable.canonical_repo_path, str(repo), relative, label='campaign', create_parent=True))
    directory = target.parent
    with locked(directory):
        existing = durable_call(_durable.read_no_follow, str(repo), str(target), label='campaign manifest', missing_ok=True)
        if not existing and not os.path.lexists(target):
            subprocess.run([sys.executable, str(ROOT/'scripts/lib/durable-jsonl.py'), '--label', 'campaign manifest', 'write', str(target)], input=canonical, check=True)
        elif existing != canonical:
            error('campaign manifest differs from existing bytes')
        events = directory/'events.jsonl'
        raw = durable_call(_durable.read_no_follow, str(repo), str(events), label='events.jsonl', missing_ok=True)
        if raw is None or raw == b'':
            append(directory, {'type':'campaign_started','base':manifest['base'], 'problem':manifest['problem'], 'verify':manifest['verify'], 'budget':manifest['budget']})
        else:
            Ledger(directory)._view()
            append(directory, {'type':'campaign_resumed'})
    return directory, projection


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--repo',required=True); parser.add_argument('--campaign',required=True); parser.add_argument('--dry-run',action='store_true')
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
        if args.dry_run:
            projection, _ = validate(args.repo,manifest)
            print(json.dumps(projection, sort_keys=True, indent=2))
            return 0
        initialize(args.repo,manifest)
        error('campaign waves are not implemented yet')
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print('error: %s' % exc, file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
