"""Experiment execution, aggregation, evaluation and invariant packs."""
from __future__ import annotations
import math
import random
import statistics
import uuid
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple
from . import RUNTIME_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, append_jsonl, read_json, read_jsonl, sha256_bytes, sha256_file, utc_now
from .execution import run as run_execution
from .experiment_contract import load_contract, run_index, validate

def _metric_value(metrics: Mapping[str, Any], name: str) -> float:
    value = metrics.get(name)
    if isinstance(value, bool) or not isinstance(value, (int, float)) or (not math.isfinite(float(value))):
        raise CoreError('metrics file must contain finite numeric %s' % name)
    return float(value)

def _numeric_metrics(metrics: Mapping[str, Any], names: Sequence[str]) -> Dict[str, float]:
    result: Dict[str, float] = {}
    for name in names:
        value = metrics.get(name)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            continue
        number = float(value)
        if math.isfinite(number):
            result[name] = number
    return result

def _metrics_snapshot(path: Path) -> Optional[Dict[str, Any]]:
    if not path.exists():
        return None
    if path.is_symlink() or not path.is_file():
        raise CoreError('metrics path must be a regular non-symlink file: %s' % path)
    info = path.stat()
    return {'sha256': sha256_file(path), 'size': info.st_size, 'mtime_ns': info.st_mtime_ns}

def record_run(repo: Path, experiment_id: str, arm: str, seed: int, metrics_path: Path, command: Sequence[str], *, profile: str='trusted-local', timeout_seconds: int=86400, image: str='', adapter: str='', worktree: Optional[Path]=None, allow_existing_metrics: bool=False) -> Tuple[Dict[str, Any], int]:
    contract = load_contract(repo, experiment_id)
    if arm not in ('baseline', 'treatment'):
        raise CoreError('experiment arm must be baseline or treatment')
    if seed not in contract['seeds']:
        raise CoreError('seed %s was not pre-registered' % seed)
    before = _metrics_snapshot(metrics_path)
    if before is not None and (not allow_existing_metrics):
        raise CoreError('metrics file already exists; refuse stale reuse unless --allow-existing-metrics is explicit')
    run_id = 'experiment-run-' + uuid.uuid4().hex
    receipt, rc = run_execution(profile, repo, command, timeout_seconds=timeout_seconds, image=image, adapter=adapter, worktree=worktree, log_path=repo / '.oms' / 'experiments' / 'logs' / (run_id + '.log'))
    after = _metrics_snapshot(metrics_path)
    stale_metrics = before is not None and after is not None and (before.get('sha256') == after.get('sha256')) and (before.get('size') == after.get('size'))
    metrics = None if stale_metrics else read_json(metrics_path, default=None)
    numeric: Dict[str, float] = {}
    metric_value: Optional[float] = None
    metric_error = ''
    required_metrics = [contract['primary_metric']] + sorted(contract['success'].get('no_regression', {}))
    missing_required_metrics: List[str] = []
    if stale_metrics:
        metric_error = 'metrics file content was not changed by this run'
    elif isinstance(metrics, dict):
        numeric = _numeric_metrics(metrics, list(contract['metric_specs']))
        missing_required_metrics = [name for name in required_metrics if name not in numeric]
        try:
            metric_value = _metric_value(metrics, contract['primary_metric'])
        except CoreError as exc:
            metric_error = str(exc)
        if missing_required_metrics and (not metric_error):
            metric_error = 'required metrics are missing or non-finite: %s' % ', '.join(missing_required_metrics)
    else:
        metric_error = 'metrics file is missing or invalid'
    metrics_valid = rc == 0 and (not stale_metrics) and (metric_value is not None) and (not missing_required_metrics)
    effective_exit = rc if rc != 0 else 0 if metrics_valid else 3
    row = {'schema': 1, 'run_id': run_id, 'created_at': utc_now(), 'experiment_id': experiment_id, 'contract_digest': contract['contract_digest'], 'arm': arm, 'seed': seed, 'execution_receipt': receipt.get('receipt'), 'execution_operation_id': receipt.get('operation_id'), 'exit': effective_exit, 'command_exit': rc, 'metric': contract['primary_metric'], 'metric_value': metric_value, 'metrics': numeric, 'metrics_digest': after.get('sha256') if after and (not stale_metrics) else None, 'metric_error': metric_error, 'missing_required_metrics': missing_required_metrics, 'metrics_valid': metrics_valid, 'stale_metrics': stale_metrics}
    append_jsonl(run_index(repo), row)
    return (row, effective_exit)

def _bootstrap_ci(values: Sequence[float], seed_text: str, samples: int=2000) -> Optional[List[float]]:
    if not values:
        return None
    if len(values) == 1:
        return [float(values[0]), float(values[0])]
    rng = random.Random(int(sha256_bytes(seed_text.encode('utf-8'))[:16], 16))
    estimates: List[float] = []
    for _ in range(samples):
        estimates.append(statistics.mean((rng.choice(values) for _ in values)))
    estimates.sort()
    low = estimates[int(0.025 * (samples - 1))]
    high = estimates[int(0.975 * (samples - 1))]
    return [low, high]

def _latest_by_seed(rows: Sequence[Mapping[str, Any]], arm: str) -> Dict[int, Mapping[str, Any]]:
    result: Dict[int, Mapping[str, Any]] = {}
    for row in rows:
        if row.get('arm') != arm:
            continue
        seed = row.get('seed')
        if isinstance(seed, int):
            result[seed] = row
    return result

def _metric_arm_summary(latest: Mapping[int, Mapping[str, Any]], seeds: Sequence[int], metric: str, digest: str) -> Dict[str, Any]:
    values: List[float] = []
    missing: List[int] = []
    for seed in seeds:
        row = latest.get(seed)
        value = row.get('metrics', {}).get(metric) if isinstance(row, Mapping) and isinstance(row.get('metrics'), dict) else None
        if not isinstance(row, Mapping) or row.get('exit') != 0 or isinstance(value, bool) or (not isinstance(value, (int, float))):
            missing.append(seed)
        else:
            values.append(float(value))
    return {'values': values, 'n': len(values), 'mean': statistics.mean(values) if values else None, 'stdev': statistics.stdev(values) if len(values) > 1 else 0.0 if values else None, 'bootstrap_ci95': _bootstrap_ci(values, digest + metric), 'missing_seeds': missing}

def _regression(baseline: float, treatment: float, direction: str) -> float:
    return treatment - baseline if direction == 'minimize' else baseline - treatment

def _latest_invariant_result(repo: Path, contract_digest: str) -> Optional[Dict[str, Any]]:
    path = repo / '.oms' / 'experiments' / 'invariants.jsonl'
    latest: Optional[Dict[str, Any]] = None
    for row in read_jsonl(path, limit_rows=MAX_JSONL_ROWS) if path.is_file() else []:
        if row.get('contract_digest') == contract_digest:
            latest = row
    return latest

def summarize(repo: Path, experiment_id: str) -> Dict[str, Any]:
    contract = load_contract(repo, experiment_id)
    rows = [row for row in read_jsonl(run_index(repo), limit_rows=MAX_JSONL_ROWS) if row.get('experiment_id') == experiment_id and row.get('contract_digest') == contract['contract_digest']]
    latest = {arm: _latest_by_seed(rows, arm) for arm in ('baseline', 'treatment')}
    primary = contract['primary_metric']
    arms = {arm: _metric_arm_summary(latest[arm], contract['seeds'], primary, contract['contract_digest'] + arm) for arm in ('baseline', 'treatment')}
    data_complete = not arms['baseline']['missing_seeds'] and (not arms['treatment']['missing_seeds'])
    improvement: Optional[float] = None
    primary_pass: Optional[bool] = None
    if arms['baseline']['mean'] is not None and arms['treatment']['mean'] is not None:
        delta = float(arms['treatment']['mean']) - float(arms['baseline']['mean'])
        improvement = -delta if contract['metrics']['primary']['direction'] == 'minimize' else delta
        if data_complete:
            primary_pass = improvement >= float(contract['success'].get('min_improvement', 0.0))
    no_regression: Dict[str, Dict[str, Any]] = {}
    no_regression_complete = True
    no_regression_pass = True
    for name, rule in contract['success'].get('no_regression', {}).items():
        baseline = _metric_arm_summary(latest['baseline'], contract['seeds'], name, contract['contract_digest'] + 'baseline')
        treatment = _metric_arm_summary(latest['treatment'], contract['seeds'], name, contract['contract_digest'] + 'treatment')
        ready = not baseline['missing_seeds'] and (not treatment['missing_seeds'])
        regression = None
        passed = None
        if ready and baseline['mean'] is not None and (treatment['mean'] is not None):
            regression = _regression(float(baseline['mean']), float(treatment['mean']), rule['direction'])
            passed = regression <= float(rule['max_regression'])
        no_regression_complete = no_regression_complete and ready
        no_regression_pass = no_regression_pass and passed is True
        no_regression[name] = {'direction': rule['direction'], 'max_regression': rule['max_regression'], 'baseline': baseline, 'treatment': treatment, 'regression': regression, 'passed': passed}
    invariant_required = bool(contract.get('invariant_pack'))
    invariant_result = _latest_invariant_result(repo, contract['contract_digest'])
    invariant_complete = not invariant_required or invariant_result is not None
    invariant_pass = not invariant_required or bool(invariant_result and invariant_result.get('passed'))
    complete = data_complete and no_regression_complete and invariant_complete
    if not complete or primary_pass is None:
        verdict = 'inconclusive'
    elif primary_pass and no_regression_pass and invariant_pass:
        verdict = 'supported'
    else:
        verdict = 'not_supported'
    return {'schema': RUNTIME_SCHEMA, 'experiment_id': experiment_id, 'contract_digest': contract['contract_digest'], 'metric': primary, 'direction': contract['metrics']['primary']['direction'], 'arms': arms, 'complete': complete, 'data_complete': data_complete, 'improvement': improvement, 'min_improvement': float(contract['success'].get('min_improvement', 0.0)), 'primary_pass': primary_pass, 'no_regression': no_regression, 'invariants': {'required': invariant_required, 'complete': invariant_complete, 'passed': invariant_pass, 'result': invariant_result}, 'verdict': verdict}

def _result_values(results: Mapping[str, Any], arm: str, metric: str, primary: str) -> List[float]:
    raw_arm = results.get(arm)
    raw = raw_arm if metric == primary and isinstance(raw_arm, list) else raw_arm.get(metric) if isinstance(raw_arm, dict) else None
    if not isinstance(raw, list):
        return []
    return [float(value) for value in raw if isinstance(value, (int, float)) and (not isinstance(value, bool)) and math.isfinite(float(value))]

def evaluate(contract_raw: Mapping[str, Any], results: Mapping[str, Any]) -> Dict[str, Any]:
    contract = validate(contract_raw)
    if not isinstance(results, dict):
        raise CoreError('experiment results must be a JSON object')
    primary = contract['primary_metric']
    baseline = _result_values(results, 'baseline', primary, primary)
    treatment = _result_values(results, 'treatment', primary, primary)
    if not baseline or not treatment:
        raise CoreError('results must contain finite numeric baseline and treatment values')
    delta = statistics.mean(treatment) - statistics.mean(baseline)
    improvement = -delta if contract['metrics']['primary']['direction'] == 'minimize' else delta
    primary_complete = len(baseline) == len(contract['seeds']) and len(treatment) == len(contract['seeds'])
    threshold = float(contract['success'].get('min_improvement', 0.0))
    checks: Dict[str, Any] = {}
    regression_complete = True
    regression_pass = True
    for name, rule in contract['success'].get('no_regression', {}).items():
        left = _result_values(results, 'baseline', name, primary)
        right = _result_values(results, 'treatment', name, primary)
        ready = len(left) == len(contract['seeds']) and len(right) == len(contract['seeds'])
        regression = _regression(statistics.mean(left), statistics.mean(right), rule['direction']) if ready else None
        passed = regression <= float(rule['max_regression']) if regression is not None else None
        regression_complete = regression_complete and ready
        regression_pass = regression_pass and passed is True
        checks[name] = {'regression': regression, 'max_regression': rule['max_regression'], 'direction': rule['direction'], 'passed': passed}
    required_invariants = [item['name'] for item in contract.get('invariant_pack', []) if item.get('required', True)]
    raw_invariants = results.get('invariants', {})
    invariant_status: Dict[str, Optional[bool]] = {}
    for name in required_invariants:
        raw_value = raw_invariants.get(name) if isinstance(raw_invariants, dict) else None
        if isinstance(raw_value, bool):
            invariant_status[name] = raw_value
        elif isinstance(raw_value, dict) and isinstance(raw_value.get('passed'), bool):
            invariant_status[name] = bool(raw_value['passed'])
        elif isinstance(raw_value, dict) and isinstance(raw_value.get('exit'), int):
            invariant_status[name] = int(raw_value['exit']) == 0
        else:
            invariant_status[name] = None
    invariant_complete = all((value is not None for value in invariant_status.values()))
    invariant_pass = all((value is True for value in invariant_status.values()))
    if not primary_complete or not regression_complete or (not invariant_complete):
        verdict = 'inconclusive'
    elif improvement >= threshold and regression_pass and invariant_pass:
        verdict = 'supported'
    else:
        verdict = 'not_supported'
    return {'schema': RUNTIME_SCHEMA, 'metric': primary, 'baseline_mean': statistics.mean(baseline), 'treatment_mean': statistics.mean(treatment), 'delta': delta, 'improvement': improvement, 'expected_n': len(contract['seeds']), 'baseline_n': len(baseline), 'treatment_n': len(treatment), 'no_regression': checks, 'invariants': {'required': required_invariants, 'status': invariant_status, 'complete': invariant_complete, 'passed': invariant_pass}, 'verdict': verdict}

def run_invariant_pack(repo: Path, contract_raw: Mapping[str, Any], *, profile: str='trusted-local', image: str='', adapter: str='', timeout_seconds: int=1200) -> Dict[str, Any]:
    contract = validate(contract_raw)
    invariant_run_id = 'invariant-run-' + uuid.uuid4().hex
    results: List[Dict[str, Any]] = []
    for item in contract.get('invariant_pack', []):
        command = [str(part) for part in item['command']]
        receipt, rc = run_execution(profile, repo, command, timeout_seconds=timeout_seconds, image=image, adapter=adapter, log_path=repo / '.oms' / 'experiments' / 'invariants' / invariant_run_id / ('%s.log' % item['name']))
        results.append({'name': item['name'], 'required': bool(item.get('required', True)), 'exit': rc, 'passed': rc == 0, 'receipt': receipt.get('receipt'), 'operation_id': receipt.get('operation_id')})
    required_results = [item for item in results if item['required']]
    row = {'schema': RUNTIME_SCHEMA, 'invariant_run_id': invariant_run_id, 'created_at': utc_now(), 'contract_digest': contract['contract_digest'], 'passed': all((item['passed'] for item in required_results)), 'results': results}
    append_jsonl(repo / '.oms' / 'experiments' / 'invariants.jsonl', row)
    return row
