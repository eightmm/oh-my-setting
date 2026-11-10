"""Compare retained envelope snapshots without replaying actions or approvals."""
from __future__ import annotations

from datetime import datetime

from .workflow_views import PLAN_ID, ViewError, bounded, identifier, limit_value, rows, text, validate_envelope


def _time(value):
    if not isinstance(value, str) or len(value) > 80:
        return None
    try:
        result = datetime.fromisoformat(value.replace('Z', '+00:00'))
        return result if result.utcoffset() is not None else None
    except (ValueError, OverflowError):
        return None


def _recorded_scope(before, after):
    old, new = before['plan'].get('plan_id'), after['plan'].get('plan_id')
    if isinstance(old, str) and PLAN_ID.fullmatch(old) and isinstance(new, str) and PLAN_ID.fullmatch(new):
        return 'same-recorded-plan' if old == new else 'different-recorded-plan'
    old, new = before['task'].get('task_id'), after['task'].get('task_id')
    if isinstance(old, str) and old and isinstance(new, str) and new:
        identifier(old); identifier(new)
        return 'same-recorded-task' if old == new else 'different-recorded-task'
    return 'unproven'


def _diff(old, new, fields, limit):
    changed = []
    for key in sorted(old.keys() & new.keys()):
        deltas = {}
        for field in fields:
            a, b = old[key].get(field), new[key].get(field)
            if a != b:
                # Report only bounded scalar metadata; never entire user dictionaries.
                if a is not None and not isinstance(a, str) or b is not None and not isinstance(b, str):
                    raise ViewError('invalid snapshot field for comparison')
                deltas[field] = {'before': text(a) if a is not None else None,
                                 'after': text(b) if b is not None else None}
        if deltas:
            changed.append({'id': key, 'changes': deltas})
    return {'appeared': bounded(sorted(new.keys()-old.keys()), limit),
            'no_longer_present': bounded(sorted(old.keys()-new.keys()), limit),
            'changed': bounded(changed, limit)}


def _failures(value):
    indexed, unkeyed = {}, 0
    for row in rows(value.get('failures', [])):
        key = row.get('id')
        source = row.get('source')
        if not isinstance(key, str) or not key or not isinstance(source, str) or not source:
            unkeyed += 1
            continue
        identifier(key)
        combined = (text(source, 300), key)
        # A repeated fingerprint in one projection is not several new failures.
        indexed[combined] = {'id': key, 'source': combined[0]}
    return indexed, unkeyed


def compare_envelopes(before, after, *, limit=20):
    limit_value(limit)
    old_criteria, old_tasks = validate_envelope(before)
    new_criteria, new_tasks = validate_envelope(after)
    scope = _recorded_scope(before, after)
    known_same = scope in ('same-recorded-plan', 'same-recorded-task')
    left_time, right_time = _time(before.get('generated_at')), _time(after.get('generated_at'))
    order = ('unknown' if left_time is None or right_time is None else
             'forward' if left_time < right_time else 'equal' if left_time == right_time else 'reversed')
    warnings = []
    if not known_same:
        warnings.append('Task identity continuity is not established; same ids are not treated as the same task.')
    if order != 'forward':
        warnings.append('Snapshot chronology is ' + order + '; this is not a chronological event replay.')
    if known_same:
        tasks = _diff(old_tasks, new_tasks, ('state', 'title', 'verify_digest'), limit)
    else:
        tasks = {'before_ids': bounded(sorted(old_tasks), limit), 'after_ids': bounded(sorted(new_tasks), limit)}
    # Criteria text/command changes are material even when their id is unchanged.
    criteria = _diff(old_criteria, new_criteria, ('status', 'text', 'command_digest', 'plan_id', 'active_task_id'), limit)
    failures_before, unkeyed_before = _failures(before)
    failures_after, unkeyed_after = _failures(after)
    added = [failures_after[k] for k in sorted(failures_after.keys()-failures_before.keys())]
    absent = [failures_before[k] for k in sorted(failures_before.keys()-failures_after.keys())]
    actions = []
    for row in after.get('next_actions', []):
        actions.append({key: text(row.get(key, ''), 500 if key == 'command' else 300)
                        for key in ('id', 'reason', 'authority', 'command')})
    old_contract, new_contract = before['plan'].get('contract', {}), after['plan'].get('contract', {})
    if not isinstance(old_contract, dict) or not isinstance(new_contract, dict):
        raise ViewError('invalid contract snapshot')
    changed_contract_fields = [key for key in ('bound','satisfied','expected_spec_sha256','current_spec_sha256','blocker')
                               if old_contract.get(key) != new_contract.get(key)]
    return {'schema': 1, 'kind': 'oms-return-brief', 'authority': 'historical-read-projection-only',
            'scope': scope, 'repository_identity_verified': False, 'chronology': order,
            'before_state_digest': before['state_digest'], 'after_state_digest': after['state_digest'],
            'state_changed': before['state_digest'] != after['state_digest'],
            'head_changed': before['repo'].get('head') != after['repo'].get('head'),
            'contract_fields_changed': changed_contract_fields,
            'tasks': tasks, 'criteria': criteria,
            'failures': {'appeared': bounded(added, limit), 'no_longer_present': bounded(absent, limit),
                         'before_unkeyed': unkeyed_before, 'after_unkeyed': unkeyed_after},
            'after_actions': bounded(actions, limit), 'warnings': warnings,
            'history_complete': False,
            'note': 'Compares retained endpoint snapshots only. Disappearance does not prove resolution; intermediate events may be missing. No cursor, memory, approval, task state, or provider history is changed. Saved snapshots are not revalidated or authenticated.'}
