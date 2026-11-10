"""Enumerate a hyperparameter grid as named cells; no search, no submission.

A sweep file is JSON: {"grid": {name: [values...]}, "seeds": [ints],
"fixed": {name: value}}. Every combination of grid values is one cell with
an id derived from its parameters alone, so the same configuration has the
same id in every plan and a repeated one is seen as a repeat. The plan and
its emitted text are for a person or a job script to read; nothing here
runs, submits or chooses a cell.
"""
from __future__ import annotations
import itertools
import math
import re
from typing import Any, Dict, List, Mapping, Sequence
from . import RUNTIME_SCHEMA
from .common import CoreError, canonical_json, sha256_bytes

MAX_AXES = 16
MAX_VALUES = 200
MAX_CELLS = 5000
MAX_ROWS = 20000
EMIT_FORMATS = ('tsv', 'hydra-overrides', 'sbatch-array')
_NAME = re.compile(r'[A-Za-z_][A-Za-z0-9_.-]{0,79}')


def _scalar(value: Any, label: str) -> Any:
    if value is None or isinstance(value, (bool, str)):
        if isinstance(value, str) and (len(value) > 200 or any(ch in value for ch in '\r\n\t')):
            raise CoreError('%s must be one line of at most 200 characters' % label)
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float) and math.isfinite(value):
        return value
    raise CoreError('%s must be a string, number, boolean or null' % label)


def _text(value: Any) -> str:
    if value is None:
        return 'null'
    if isinstance(value, bool):
        return 'true' if value else 'false'
    return str(value)


def plan(raw: Any) -> Dict[str, Any]:
    if not isinstance(raw, dict):
        raise CoreError('sweep file must be a JSON object')
    unknown = sorted(set(raw) - {'grid', 'seeds', 'fixed'})
    if unknown:
        raise CoreError('sweep file has unknown key: %s' % unknown[0])
    grid = raw.get('grid')
    if not isinstance(grid, dict) or not grid:
        raise CoreError('sweep grid must be a nonempty object of name -> list of values')
    if len(grid) > MAX_AXES:
        raise CoreError('sweep grid has more than %d parameters' % MAX_AXES)
    fixed = raw.get('fixed', {})
    if not isinstance(fixed, dict):
        raise CoreError('sweep fixed must be an object')
    axes: List[str] = sorted(grid)
    repeated: Dict[str, List[Any]] = {}
    values: Dict[str, List[Any]] = {}
    for name in axes + sorted(fixed):
        if not isinstance(name, str) or not _NAME.fullmatch(name) or name == 'seed':
            raise CoreError('sweep parameter name is not usable: %r' % (name,))
    overlap = sorted(set(grid) & set(fixed))
    if overlap:
        raise CoreError('parameter is both swept and fixed: %s' % overlap[0])
    for name in axes:
        listed = grid[name]
        if not isinstance(listed, list) or not listed or len(listed) > MAX_VALUES:
            raise CoreError('sweep grid.%s must be a list of 1-%d values' % (name, MAX_VALUES))
        kept: List[Any] = []
        seen = set()
        for index, item in enumerate(listed):
            item = _scalar(item, 'grid.%s[%d]' % (name, index))
            key = canonical_json(item)
            if key in seen:
                repeated.setdefault(name, []).append(item)
                continue
            seen.add(key)
            kept.append(item)
        values[name] = kept
    fixed_values = {name: _scalar(fixed[name], 'fixed.%s' % name) for name in sorted(fixed)}
    seeds = raw.get('seeds', [0])
    if not isinstance(seeds, list) or not seeds:
        raise CoreError('sweep seeds must be a nonempty list of integers')
    for seed in seeds:
        if isinstance(seed, bool) or not isinstance(seed, int):
            raise CoreError('sweep seeds must contain integers')
    if len(set(seeds)) != len(seeds):
        raise CoreError('sweep seeds must be unique')
    count = 1
    for name in axes:
        count *= len(values[name])
        if count > MAX_CELLS:
            raise CoreError('sweep has more than %d cells; split it' % MAX_CELLS)
    if count * len(seeds) > MAX_ROWS:
        raise CoreError('sweep has more than %d runs; split it' % MAX_ROWS)
    cells: List[Dict[str, Any]] = []
    for combo in itertools.product(*(values[name] for name in axes)):
        params = dict(fixed_values)
        params.update(zip(axes, combo))
        cells.append({'cell_id': 'cell-' + sha256_bytes(canonical_json(params))[:12], 'params': params})
    notes: List[str] = []
    if repeated:
        notes.append('repeated grid values were listed once: ' + '; '.join(
            '%s=%s' % (name, ','.join(_text(item) for item in items)) for name, items in sorted(repeated.items())))
    if len(cells) > 1:
        notes.append('%d configurations are compared: the best of %d is expected to look better than it is; '
                     'confirm the chosen cell on data the selection did not use' % (len(cells), len(cells)))
    if len(seeds) < 3:
        notes.append('%d seed(s) per cell: a difference between cells may be seed noise' % len(seeds))
    return {'schema': RUNTIME_SCHEMA, 'kind': 'oms-experiment-sweep-plan', 'parameters': axes,
            'fixed': fixed_values, 'seeds': list(seeds), 'cells': cells, 'cell_count': len(cells),
            'run_count': len(cells) * len(seeds), 'repeated_values': {name: items for name, items in sorted(repeated.items())},
            'notes': notes}


def _rows(value: Mapping[str, Any]) -> List[Dict[str, Any]]:
    return [{'index': index, 'cell_id': cell['cell_id'], 'seed': seed, 'params': cell['params']}
            for index, (cell, seed) in enumerate(
                (cell, seed) for cell in value['cells'] for seed in value['seeds'])]


def _overrides(params: Mapping[str, Any], seed: int) -> str:
    parts = []
    for name in sorted(params):
        text = _text(params[name])
        if not re.fullmatch(r'[A-Za-z0-9_.+:/@%-]+', text):
            raise CoreError('value of %s cannot be written as a bare override: %r' % (name, text))
        parts.append('%s=%s' % (name, text))
    parts.append('seed=%d' % seed)
    return ' '.join(parts)


def emit_text(value: Mapping[str, Any], fmt: str) -> str:
    """The plan as text a person or a job script can use. Nothing is submitted."""
    if fmt not in EMIT_FORMATS:
        raise CoreError('--emit must be one of: %s' % ', '.join(EMIT_FORMATS))
    rows = _rows(value)
    names: Sequence[str] = sorted(set(value['parameters']) | set(value['fixed']))
    lines: List[str] = []
    if fmt == 'tsv':
        lines.append('\t'.join(['index', 'cell_id', 'seed'] + list(names)))
        for row in rows:
            lines.append('\t'.join([str(row['index']), row['cell_id'], str(row['seed'])]
                                   + [_text(row['params'][name]) for name in names]))
    elif fmt == 'hydra-overrides':
        for row in rows:
            lines.append(_overrides(row['params'], row['seed']))
    else:
        lines.append('# %d run(s): %d cell(s) x %d seed(s). Printed only; nothing was submitted.' % (
            len(rows), value['cell_count'], len(value['seeds'])))
        lines.append('#SBATCH --array=0-%d' % (len(rows) - 1))
        lines.append('# One line per array index: INDEX<TAB>CELL_ID<TAB>OVERRIDES. In the job script:')
        lines.append('#   line="$(awk -F\'\\t\' -v i="$SLURM_ARRAY_TASK_ID" \'$1 == i\' sweep.tsv)"')
        for row in rows:
            lines.append('%d\t%s\t%s' % (row['index'], row['cell_id'], _overrides(row['params'], row['seed'])))
    return '\n'.join(lines)
