"""Run budget of an experiment contract: what is used, what is reserved, and the gate.

This is OMS's own usage ledger, not a Slurm reservation. A run takes a
`reserved` row in .oms/experiments/budget-reservations.jsonl while holding the
state lock, after counting used + reserved, so runs that start at the same time
cannot all pass the remaining budget. The row is settled when the run ends: it
becomes a used run, or is released when the run never produced a record.
"""
from __future__ import annotations
import datetime as _dt
import importlib.util
import os
import socket
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Tuple
from .common import MAX_JSONL_LINE_BYTES, MAX_JSONL_BYTES, CoreError, append_jsonl, atomic_write_bytes, canonical_json, ensure_private_dir
from .common import file_lock, install_root, read_bytes, read_jsonl, utc_now, MAX_JSONL_ROWS
from .experiment_contract import run_index

# A reservation whose process is gone is only called stale once it is this old.
STALE_RESERVATION_SECONDS = 3600


def reservations_path(repo: Path) -> Path:
    return repo / '.oms' / 'experiments' / 'budget-reservations.jsonl'


def _age_seconds(created_at: Any) -> Optional[int]:
    try:
        moment = _dt.datetime.strptime(str(created_at), '%Y-%m-%dT%H:%M:%SZ').replace(tzinfo=_dt.timezone.utc)
    except ValueError:
        return None
    return max(0, int((_dt.datetime.now(_dt.timezone.utc) - moment).total_seconds()))


def _process_gone(row: Mapping[str, Any]) -> bool:
    """True only when the reserving process is known to be gone: same host and
    no such pid. Another host's pid cannot be checked, so it is never gone."""
    pid = row.get('pid')
    if row.get('host') != socket.gethostname() or isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    return False


def _runs_state(repo: Path, digest: Any) -> Dict[str, Any]:
    index_ids = set()
    for number, row in enumerate(read_jsonl(run_index(repo), limit_rows=MAX_JSONL_ROWS)):
        if isinstance(row, dict) and row.get('contract_digest') == digest:
            index_ids.add(str(row.get('run_id') or 'row-%d' % number))
    reserved: Dict[str, Dict[str, Any]] = {}
    settled: Dict[str, str] = {}
    for row in read_jsonl(reservations_path(repo), limit_rows=MAX_JSONL_ROWS):
        if not isinstance(row, dict) or row.get('contract_digest') != digest or not row.get('run_id'):
            continue
        if row.get('kind') == 'reserved':
            reserved[str(row['run_id'])] = row
        elif row.get('kind') == 'settled':
            settled[str(row['run_id'])] = str(row.get('outcome'))
    used = set(index_ids) | {run_id for run_id, outcome in settled.items() if outcome == 'used'}
    live: List[Dict[str, Any]] = []
    stale: List[Dict[str, Any]] = []
    for run_id, row in reserved.items():
        if run_id in used or run_id in settled:
            continue
        age = _age_seconds(row.get('created_at'))
        item = {'run_id': run_id, 'pid': row.get('pid'), 'host': row.get('host'), 'age_seconds': age}
        if age is not None and age > STALE_RESERVATION_SECONDS and _process_gone(row):
            item['message'] = ('stale reservation: run %s (pid %s on %s gone, %ds old) is not counted against the budget; '
                               'its row stays in .oms/experiments/budget-reservations.jsonl' % (run_id, row.get('pid'), row.get('host'), age))
            stale.append(item)
        else:
            live.append(item)
    return {'used': len(used), 'reserved': live, 'stale': stale}


def _gpu_hours_used(repo: Path, experiment_id: str) -> Dict[str, Any]:
    """Allocated GPU-hours from the run-reconcile usage rows of this experiment's
    ledgered jobs (note `experiment=ID`); null when there is nothing to sum."""
    out: Dict[str, Any] = {'used': None}
    module_path = install_root() / 'scripts' / 'lib' / 'run_usage.py'
    if not experiment_id or not module_path.is_file():
        out['note'] = 'not counted: no experiment id or run_usage.py to read run-reconcile usage with'
        return out
    spec = importlib.util.spec_from_file_location('oms_run_usage', str(module_path))
    if spec is None or spec.loader is None:
        out['note'] = 'not counted: run_usage.py could not be loaded'
        return out
    usage = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(usage)
    reconcile = Path(os.environ.get('OMS_RECONCILE_FILE') or repo / '.oms' / 'runs' / 'reconcile.jsonl')
    jobs = set(usage.experiment_jobs(str(repo / 'docs' / 'EXPERIMENTS.jsonl'), experiment_id))
    report = usage.summary(str(reconcile), jobs)
    out['jobs_reconciled'] = report['jobs']
    out['jobs_with_gpu_hours'] = report['gpu_hours_jobs']
    if report['gpu_hours'] is None:
        out['note'] = ('not counted: no reconciled job with allocated GPU-hours is ledgered for experiment %s '
                       '(oms run-reconcile apply records them; runs started by experiment launch carry no experiment id)' % experiment_id)
    else:
        out['used'] = report['gpu_hours']
        out['note'] = 'sum of run-reconcile usage over the reconciled jobs of this experiment; reported, not enforced'
    return out


def budget_status(repo: Path, contract: Mapping[str, Any], experiment_id: str='') -> Optional[Dict[str, Any]]:
    """What the contract's budget allows, what the run index and the reservations
    show spent, and what is held by runs in progress. None when no budget is set."""
    budget = contract.get('budget')
    if not isinstance(budget, dict):
        return None
    status: Dict[str, Any] = {}
    if 'runs' in budget:
        state = _runs_state(repo, contract.get('contract_digest'))
        used, held = state['used'], len(state['reserved'])
        status['runs'] = {'limit': budget['runs'], 'used': used, 'reserved': held,
                          'remaining': max(0, budget['runs'] - used - held),
                          'exceeded': used + held >= budget['runs'], 'stale': state['stale']}
        if state['reserved']:
            status['runs']['reserved_runs'] = state['reserved']
    if 'gpu_hours' in budget:
        gpu = _gpu_hours_used(repo, experiment_id)
        gpu['limit'] = budget['gpu_hours']
        gpu['exceeded'] = None if gpu['used'] is None else gpu['used'] >= budget['gpu_hours']
        status['gpu_hours'] = gpu
    if 'final_evals' in budget:
        status['final_evals'] = {'limit': budget['final_evals'], 'used': None,
                                 'note': 'not counted: this branch records no evaluation on a final split; the run-ledger '
                                         'test-split-count change (branch oms/run-ledger-test-split-count) counts them'}
    return status


def _refusal(runs: Mapping[str, Any]) -> CoreError:
    held = int(runs['reserved'])
    text = 'run budget spent: %d of %d run(s) recorded for this contract' % (runs['used'] + held, runs['limit'])
    if held:
        text = 'run budget spent: %d of %d run(s) recorded or reserved for this contract (%d reserved by a run in progress)' % (
            runs['used'] + held, runs['limit'], held)
    return CoreError(text + '; pass --reason to run past it')


def require_run_budget(repo: Path, contract: Mapping[str, Any], reason: str='') -> str:
    """Preflight, reserving nothing: refuse one more run past budget.runs unless a
    reason is given; returns the reason to record, or ''."""
    status = budget_status(repo, contract)
    runs = (status or {}).get('runs')
    if not runs or not runs['exceeded']:
        return ''
    if not reason.strip():
        raise _refusal(runs)
    return ' '.join(reason.split())[:300]


def _append_locked(path: Path, row: Mapping[str, Any]) -> None:
    encoded = canonical_json(dict(row)) + b'\n'
    if len(encoded) > MAX_JSONL_LINE_BYTES:
        raise CoreError('refusing JSONL row larger than 1 MiB')
    existing = b''
    if path.exists() or path.is_symlink():
        existing = read_bytes(path, MAX_JSONL_BYTES)
        if existing and not existing.endswith(b'\n'):
            raise CoreError('append-only JSONL has a partial final row: %s' % path)
    atomic_write_bytes(path, existing + encoded)


def reserve_run(repo: Path, contract: Mapping[str, Any], experiment_id: str, run_id: str, reason: str='', source: str='run') -> Tuple[str, bool]:
    """Take one run of budget.runs before the run starts. Under the state lock the
    runs used and reserved are counted again, so only as many concurrent starts as
    the budget has left get a reservation. Returns (override reason or '', whether
    a reservation was taken); a contract without budget.runs reserves nothing and
    writes nothing."""
    budget = contract.get('budget')
    if not isinstance(budget, dict) or 'runs' not in budget:
        return ('', False)
    path = reservations_path(repo)
    ensure_private_dir(path.parent)
    with file_lock(path):
        status = budget_status(repo, contract)
        runs = (status or {}).get('runs') or {}
        override = ''
        if runs.get('exceeded'):
            if not reason.strip():
                raise _refusal(runs)
            override = ' '.join(reason.split())[:300]
        row = {'schema': 1, 'kind': 'reserved', 'run_id': run_id, 'experiment_id': experiment_id,
               'contract_digest': contract.get('contract_digest'), 'source': source, 'created_at': utc_now(),
               'pid': os.getpid(), 'host': socket.gethostname()}
        if override:
            row['budget_override_reason'] = override
        _append_locked(path, row)
    return (override, True)


def settle_run(repo: Path, contract: Mapping[str, Any], run_id: str, outcome: str, reason: str='') -> None:
    """Close a reservation: `used` keeps the run counted, `released` frees it.
    Never raises: the run index stays authoritative for a run that was recorded,
    and an unsettled reservation is reported as stale once its process is gone."""
    row = {'schema': 1, 'kind': 'settled', 'run_id': run_id, 'contract_digest': contract.get('contract_digest'),
           'outcome': outcome, 'created_at': utc_now()}
    if reason:
        row['reason'] = reason[:300]
    try:
        append_jsonl(reservations_path(repo), row)
    except CoreError:
        pass
