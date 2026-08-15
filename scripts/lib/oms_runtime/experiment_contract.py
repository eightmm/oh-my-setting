"""ExperimentContract v2, run receipts, comparability checks and invariant packs."""
from __future__ import annotations
import math
import random
import re
import statistics
import uuid
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple
from . import EXPERIMENT_SCHEMA, RUNTIME_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, append_jsonl, atomic_write_json, canonical_json, read_json, read_jsonl, relative_path, sha256_bytes, sha256_file, utc_now
from .execution import run as run_execution
METRIC_DIRECTIONS = {'minimize', 'maximize', 'observe'}

def template() -> Dict[str, Any]:
    return {'schema': EXPERIMENT_SCHEMA, 'question': '', 'hypothesis': '', 'prediction': '', 'baseline': {'run_id': '', 'config_digest': ''}, 'treatment': {'independent_change': '', 'config_digest': ''}, 'controlled_variables': {'code_base': '', 'dataset_split': '', 'training_steps': None, 'parameter_count_tolerance': 0.01}, 'seeds': [0, 1, 2], 'metrics': {'primary': {'name': '', 'direction': 'minimize'}, 'secondary': []}, 'success': {'min_improvement': 0.0, 'no_regression': {}}, 'invariant_pack': []}

def _required_text(raw: Mapping[str, Any], key: str, limit: int=2000) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value.strip() or len(value) > limit or any((ch in value for ch in '\r\n\t')):
        raise CoreError('experiment %s must be one line of 1-%d characters' % (key, limit))
    return value.strip()

def _metric_spec(raw: Any, label: str, *, allow_observe: bool) -> Dict[str, str]:
    if isinstance(raw, str):
        name = raw.strip()
        direction = 'observe'
    elif isinstance(raw, dict):
        name = _required_text(raw, 'name', 160)
        direction = str(raw.get('direction', 'observe'))
    else:
        raise CoreError('%s must be a metric name or object' % label)
    if not name or not re.fullmatch('[A-Za-z0-9][A-Za-z0-9._/:+-]{0,159}', name):
        raise CoreError('%s.name must be a safe metric identifier' % label)
    allowed = METRIC_DIRECTIONS if allow_observe else {'minimize', 'maximize'}
    if direction not in allowed:
        raise CoreError('%s.direction must be %s' % (label, ' or '.join(sorted(allowed))))
    return {'name': name, 'direction': direction}

def validate(raw: Any) -> Dict[str, Any]:
    if not isinstance(raw, dict):
        raise CoreError('experiment contract must be a JSON object')
    if raw.get('schema') != EXPERIMENT_SCHEMA:
        raise CoreError('experiment contract schema must be %d' % EXPERIMENT_SCHEMA)
    _required_text(raw, 'question')
    _required_text(raw, 'hypothesis')
    _required_text(raw, 'prediction')
    baseline = raw.get('baseline')
    treatment = raw.get('treatment')
    controlled = raw.get('controlled_variables')
    metrics = raw.get('metrics')
    success = raw.get('success')
    if not isinstance(baseline, dict) or not baseline:
        raise CoreError('experiment baseline must be a nonempty object')
    if not isinstance(treatment, dict) or not treatment:
        raise CoreError('experiment treatment must be a nonempty object')
    independent_change = treatment.get('independent_change')
    if not isinstance(independent_change, str) or not independent_change.strip():
        raise CoreError('treatment.independent_change is required')
    if not isinstance(controlled, dict):
        raise CoreError('experiment controlled_variables must be an object')
    if not isinstance(metrics, dict) or not isinstance(metrics.get('primary'), dict):
        raise CoreError('experiment metrics.primary must be an object')
    primary = _metric_spec(metrics['primary'], 'metrics.primary', allow_observe=False)
    secondary_raw = metrics.get('secondary', [])
    if not isinstance(secondary_raw, list):
        raise CoreError('metrics.secondary must be a list')
    secondary: List[Dict[str, str]] = []
    metric_specs: Dict[str, Dict[str, str]] = {primary['name']: primary}
    for index, item in enumerate(secondary_raw):
        spec = _metric_spec(item, 'metrics.secondary[%d]' % index, allow_observe=True)
        if spec['name'] in metric_specs:
            raise CoreError('metric is declared more than once: %s' % spec['name'])
        metric_specs[spec['name']] = spec
        secondary.append(spec)
    seeds = raw.get('seeds')
    if not isinstance(seeds, list) or not seeds:
        raise CoreError('experiment seeds must be a nonempty list')
    normalized_seeds: List[int] = []
    for seed in seeds:
        if isinstance(seed, bool) or not isinstance(seed, int):
            raise CoreError('experiment seeds must contain integers')
        normalized_seeds.append(seed)
    if len(set(normalized_seeds)) != len(normalized_seeds):
        raise CoreError('experiment seeds must be unique')
    if not isinstance(success, dict) or not success:
        raise CoreError('experiment success must be a nonempty object')
    threshold = success.get('min_improvement', 0.0)
    if isinstance(threshold, bool) or not isinstance(threshold, (int, float)) or (not math.isfinite(float(threshold))):
        raise CoreError('success.min_improvement must be finite numeric')
    no_regression_raw = success.get('no_regression', {})
    if not isinstance(no_regression_raw, dict):
        raise CoreError('success.no_regression must be an object')
    no_regression: Dict[str, Dict[str, Any]] = {}
    for name, spec_raw in no_regression_raw.items():
        name = str(name)
        if name not in metric_specs:
            raise CoreError('no-regression metric is not declared: %s' % name)
        if isinstance(spec_raw, bool):
            raise CoreError('no-regression threshold must be numeric or an object: %s' % name)
        if isinstance(spec_raw, (int, float)):
            direction = metric_specs[name]['direction']
            if direction == 'observe':
                raise CoreError('no-regression metric needs minimize/maximize direction: %s' % name)
            max_regression = float(spec_raw)
        elif isinstance(spec_raw, dict):
            direction = str(spec_raw.get('direction', metric_specs[name]['direction']))
            max_regression = spec_raw.get('max_regression', 0.0)
            if direction not in ('minimize', 'maximize'):
                raise CoreError('no-regression direction must be minimize or maximize: %s' % name)
            if isinstance(max_regression, bool) or not isinstance(max_regression, (int, float)):
                raise CoreError('no-regression max_regression must be numeric: %s' % name)
            max_regression = float(max_regression)
        else:
            raise CoreError('no-regression threshold must be numeric or an object: %s' % name)
        if not math.isfinite(max_regression) or max_regression < 0:
            raise CoreError('no-regression max_regression must be finite and non-negative: %s' % name)
        no_regression[name] = {'direction': direction, 'max_regression': max_regression}
    invariant_pack = raw.get('invariant_pack', [])
    if not isinstance(invariant_pack, list):
        raise CoreError('invariant_pack must be a list')
    normalized_invariants: List[Dict[str, Any]] = []
    seen_invariants = set()
    for index, item in enumerate(invariant_pack):
        if not isinstance(item, dict) or not isinstance(item.get('name'), str) or (not isinstance(item.get('command'), list)) or (not item['command']):
            raise CoreError('invariant_pack[%d] must have name and nonempty command array' % index)
        name = item['name']
        if not re.fullmatch('[A-Za-z0-9][A-Za-z0-9._-]{0,79}', name):
            raise CoreError('invariant_pack[%d].name must be a safe identifier' % index)
        if name in seen_invariants:
            raise CoreError('duplicate invariant name: %s' % name)
        seen_invariants.add(name)
        if any((not isinstance(part, str) or not part for part in item['command'])):
            raise CoreError('invariant_pack[%d].command must contain strings' % index)
        required = item.get('required', True)
        if not isinstance(required, bool):
            raise CoreError('invariant_pack[%d].required must be boolean' % index)
        normalized_invariants.append({'name': name, 'command': list(item['command']), 'required': required})
    source = dict(raw)
    for derived in ('contract_digest', 'experiment_id', 'registered_at', 'primary_metric', 'metric_specs'):
        source.pop(derived, None)
    source['seeds'] = normalized_seeds
    source['metrics'] = {'primary': primary, 'secondary': secondary}
    source['success'] = dict(success, min_improvement=float(threshold), no_regression=no_regression)
    source['invariant_pack'] = normalized_invariants
    normalized = dict(source)
    normalized['primary_metric'] = primary['name']
    normalized['metric_specs'] = metric_specs
    normalized['contract_digest'] = sha256_bytes(canonical_json(source))
    return normalized

def compare(left: Mapping[str, Any], right: Mapping[str, Any]) -> Dict[str, Any]:
    a = validate(left)
    b = validate(right)
    issues: List[Dict[str, Any]] = []
    controlled_keys = sorted(set(a['controlled_variables']) | set(b['controlled_variables']))
    tolerance = max(float(a['controlled_variables'].get('parameter_count_tolerance', 0.0) or 0.0), float(b['controlled_variables'].get('parameter_count_tolerance', 0.0) or 0.0))
    for key in controlled_keys:
        left_value = a['controlled_variables'].get(key)
        right_value = b['controlled_variables'].get(key)
        if key == 'parameter_count' and isinstance(left_value, (int, float)) and isinstance(right_value, (int, float)):
            scale = max(abs(float(left_value)), abs(float(right_value)), 1.0)
            if abs(float(left_value) - float(right_value)) / scale <= tolerance:
                continue
        if left_value != right_value:
            issues.append({'field': 'controlled_variables.%s' % key, 'left': left_value, 'right': right_value})
    if a['metrics'] != b['metrics']:
        issues.append({'field': 'metrics', 'left': a['metrics'], 'right': b['metrics']})
    if a['seeds'] != b['seeds']:
        issues.append({'field': 'seeds', 'left': a['seeds'], 'right': b['seeds']})
    return {'schema': RUNTIME_SCHEMA, 'comparable': not issues, 'issues': issues, 'left_digest': a['contract_digest'], 'right_digest': b['contract_digest']}

def _experiment_id(value: str) -> str:
    if not re.fullmatch('[A-Za-z0-9][A-Za-z0-9._-]{0,159}', value or ''):
        raise CoreError('invalid experiment id')
    return value

def contract_dir(repo: Path) -> Path:
    return repo / '.oms' / 'experiments' / 'contracts'

def run_index(repo: Path) -> Path:
    return repo / '.oms' / 'experiments' / 'runs.jsonl'

def register(repo: Path, raw: Mapping[str, Any], experiment_id: str='') -> Dict[str, Any]:
    contract = validate(raw)
    if not experiment_id:
        experiment_id = 'experiment-' + contract['contract_digest'][:16]
    experiment_id = _experiment_id(experiment_id)
    path = contract_dir(repo) / (experiment_id + '.json')
    existing = read_json(path, default=None)
    if existing is not None and isinstance(existing, dict) and (existing.get('contract_digest') != contract['contract_digest']):
        raise CoreError('experiment id already names a different contract')
    row = dict(contract, experiment_id=experiment_id, registered_at=utc_now())
    atomic_write_json(path, row)
    return {'schema': RUNTIME_SCHEMA, 'experiment_id': experiment_id, 'contract_digest': contract['contract_digest'], 'path': relative_path(path, repo)}

def load_contract(repo: Path, experiment_id: str) -> Dict[str, Any]:
    experiment_id = _experiment_id(experiment_id)
    path = contract_dir(repo) / (experiment_id + '.json')
    row = read_json(path, default=None)
    if not isinstance(row, dict):
        raise CoreError('experiment contract not found: %s' % experiment_id)
    return validate(row)
