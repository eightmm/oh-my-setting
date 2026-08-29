"""Read-only effective TaskEnvelope projection over existing OMS state."""
from __future__ import annotations
import collections
import re
from pathlib import Path
from typing import Any, Dict, List, Mapping, Sequence, Tuple
from . import ENVELOPE_SCHEMA
from .common import MAX_JSONL_ROWS, CoreError, bounded_line, canonical_json, git_branch, git_head, install_root, parse_path_list, read_json, read_jsonl, read_text, relative_path, run_json, sha256_bytes, sha256_file, sha256_text, source_descriptor, utc_now
from .failures import classify
from .markdown import bullet_items, first_nonempty, normalized_line, parse_scope, section_text, section_text_exact, sections, stable_criterion_id, strip_criterion_marker

def _project_file(repo: Path) -> Dict[str, Any]:
    path = repo / 'PROJECT.md'
    if not path.is_file() or path.is_symlink():
        return {'present': False, 'source': None, 'criteria': [], 'scope': {'allowed': [], 'forbidden': []}}
    body, metadata = sections(read_text(path))
    goal = first_nonempty(section_text(body, ['goal', 'objective', 'purpose', '목표', '목적']), 1000)
    if not goal:
        goal = first_nonempty('\n'.join(body.get('__preamble__', [])), 1000)
    criteria_text = section_text(body, ['acceptance', 'done', 'success', 'completion', '완료', '성공', '수용'])
    constraints_text = section_text(body, ['constraint', 'scope', '제약', '범위'])
    scope_lines = bullet_items(constraints_text) or constraints_text.splitlines()
    status = metadata.get('status', '') or first_nonempty(section_text(body, ['status', '상태']), 80)
    return {'present': True, 'source': source_descriptor(path, repo), 'metadata': metadata, 'status': status, 'goal': goal, 'criteria': bullet_items(criteria_text), 'constraints': [normalized_line(item) for item in scope_lines if normalized_line(item)], 'scope': parse_scope(scope_lines)}

def _task_status(repo: Path) -> Dict[str, Any]:
    # The packet's status verb belongs to the installed harness, not to the
    # repository being projected. Resolved against the target repo it answered
    # {} for every project except this checkout: verification silently read
    # 'unknown' and a stale packet never reported stale, while the fixture that
    # planted its own scripts/agent-task.sh kept the suite green.
    script = install_root() / 'scripts' / 'agent-task.sh'
    if not script.is_file() or script.is_symlink():
        raise CoreError('canonical task status command is unavailable')
    payload = run_json(['bash', str(script), '--repo', str(repo), 'status', '--json'], cwd=repo, timeout=20)
    if (not isinstance(payload, dict) or payload.get('schema') != 1 or
            payload.get('present') is not True or
            not isinstance(payload.get('status'), str) or
            not isinstance(payload.get('verification'), str) or
            not isinstance(payload.get('stale'), bool)):
        raise CoreError('canonical task status projection is unavailable or invalid')
    return payload

def _task_packet(repo: Path) -> Dict[str, Any]:
    path = repo / '.oms' / 'task' / 'current.md'
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise CoreError('task packet must be a regular file: %s' % path)
    if not path.exists():
        return {'present': False, 'source': None, 'criteria': [], 'scope': {'allowed': [], 'forbidden': []}}
    body, metadata = sections(read_text(path))
    goal = first_nonempty(section_text(body, ['goal', 'objective', '목표']), 1000)
    constraints_text = section_text(body, ['constraint', '제약'])
    criteria_text = section_text(body, ['done criteria', 'acceptance', 'completion', '완료', '성공'])
    verify_text = section_text_exact(body, ['verify', '검증 명령'])
    loop_text = section_text_exact(body, ['loop state', 'budget', '예산'])
    constraint_lines = bullet_items(constraints_text) or constraints_text.splitlines()
    budget: Dict[str, Any] = {}
    for line in loop_text.splitlines():
        match = re.match('^\\s*[-*]?\\s*([A-Za-z][A-Za-z0-9_.-]*):\\s*(.+?)\\s*$', line)
        if match:
            raw = match.group(2)
            budget[match.group(1)] = int(raw) if raw.isdigit() else bounded_line(raw, 160)
    status = _task_status(repo)
    verification = status.get('verification') if isinstance(status, dict) else None
    if verification not in ('fresh', 'stale', 'none'):
        verification = 'unknown'
    return {'present': True, 'source': source_descriptor(path, repo), 'metadata': metadata, 'task_id': bounded_line(status.get('task_id', metadata.get('task_id', '')), 160), 'status': bounded_line(status.get('status', metadata.get('status', 'active')), 40).lower(), 'goal': goal, 'constraints': [normalized_line(item) for item in constraint_lines if normalized_line(item)], 'criteria': bullet_items(criteria_text), 'verify_present': bool(verify_text.strip()), 'verify_digest': sha256_text(verify_text.strip()) if verify_text.strip() else '', 'verification': verification, 'state': first_nonempty(section_text_exact(body, ['current state', '현재 상태']), 1000), 'next': first_nonempty(section_text_exact(body, ['next step', '다음 단계']), 1000), 'scope': parse_scope(constraint_lines), 'budget': budget, 'stale': bool(status.get('stale')) if isinstance(status, dict) else False}

def _task_list(raw: Any) -> List[Dict[str, Any]]:
    if isinstance(raw, dict):
        rows: List[Dict[str, Any]] = []
        for key, value in raw.items():
            if isinstance(value, dict):
                row = dict(value); row.setdefault('id', key); rows.append(row)
        return rows
    return [dict(item) for item in raw if isinstance(item, dict)] if isinstance(raw, list) else []

def _latest_plan_retirement(repo: Path) -> Tuple[Dict[str, Any], Any]:
    path = repo / '.oms' / 'plan' / 'retirements.jsonl'
    rows = [row for row in read_jsonl(path)
            if row.get('schema') == 1 and row.get('kind') == 'plan-retirement']
    if not rows:
        return {}, None
    row = rows[-1]
    return ({key: row.get(key) for key in (
        'ts', 'retirement_id', 'plan_sha256', 'plan_id', 'disposition',
        'reason', 'head', 'git_ref', 'archive', 'acceptance_verified',
        'task_landings_claimed') if row.get(key) not in (None, '')},
        source_descriptor(path, repo))

def _plan_status(repo: Path) -> Dict[str, Any]:
    script = install_root() / 'scripts' / 'agent-plan.sh'
    if not script.is_file() or script.is_symlink():
        raise CoreError('canonical plan status command is unavailable')
    payload = run_json(['bash', str(script), '--repo', str(repo), 'status', '--json'], cwd=repo, timeout=20)
    if (not isinstance(payload, dict) or payload.get('schema') != 1 or
            payload.get('present') is not True or
            isinstance(payload.get('task_count'), bool) or
            not isinstance(payload.get('task_count'), int) or
            payload.get('task_count', -1) < 0 or
            not all(isinstance(payload.get(key), bool)
                    for key in ('nonempty', 'all_done', 'has_unfinished')) or
            not isinstance(payload.get('by_state'), dict) or
            not all(isinstance(payload.get(key), list)
                    for key in ('actionable', 'stale', 'stale_review')) or
            not isinstance(payload.get('contract'), dict) or
            not all(isinstance(payload['contract'].get(key), bool)
                    for key in ('bound', 'satisfied', 'project_present',
                                'project_healthy')) or
            not all(isinstance(payload['contract'].get(key), str)
                    for key in ('project_state', 'blocker',
                                'expected_spec_sha256',
                                'current_spec_sha256')) or
            any(not isinstance(item, str) for item in payload['actionable']) or
            len(payload['actionable']) != len(set(payload['actionable']))):
        raise CoreError('canonical plan status projection is unavailable or invalid')
    return payload

def _plan_state(repo: Path) -> Dict[str, Any]:
    path = repo / '.oms' / 'plan' / 'tasks.json'
    raw = read_json(path, default=None)
    if raw is None:
        retirement, source = _latest_plan_retirement(repo)
        return {'present': False, 'source': source, 'tasks': [], 'task_count': 0,
                'nonempty': False, 'all_done': False, 'has_unfinished': False, 'counts': {},
                'latest_retirement': retirement,
                'scope': {'allowed': [], 'forbidden': []}}
    if not isinstance(raw, dict):
        raise CoreError('plan state must be a JSON object: %s' % path)
    raw_plan_id = raw.get('plan_id', '')
    plan_id = raw_plan_id if isinstance(raw_plan_id, str) and re.fullmatch(r'plan_[0-9a-f]{32}', raw_plan_id) else ''
    tasks: List[Dict[str, Any]] = []; all_allowed: List[str] = []; all_forbidden: List[str] = []
    for row in _task_list(raw.get('tasks', [])):
        allowed = parse_path_list(row.get('allowed', row.get('allowed_paths', [])))
        forbidden = parse_path_list(row.get('forbidden', row.get('forbidden_paths', [])))
        all_allowed.extend(allowed); all_forbidden.extend(forbidden)
        verify = str(row.get('verify', '') or '')
        tasks.append({'id': bounded_line(row.get('id', ''), 160), 'title': bounded_line(row.get('title', row.get('goal', '')), 300), 'state': bounded_line(row.get('state', 'ready'), 40).lower(), 'depends': [bounded_line(item, 160) for item in row.get('depends', row.get('dependencies', []))] if isinstance(row.get('depends', row.get('dependencies', [])), list) else [], 'scope': {'allowed': allowed, 'forbidden': forbidden}, 'verify_present': bool(verify), 'verify_digest': sha256_text(verify) if verify else '', 'provider': bounded_line(row.get('provider', ''), 80), 'lease_id_present': bool(row.get('lease_id')), 'artifact_present': bool(row.get('artifact')), 'patch_present': bool(row.get('patch'))})
    acceptance = str(raw.get('accept', raw.get('acceptance', '')) or '')
    status = _plan_status(repo)
    count = len(tasks)
    if status['task_count'] != count or status['nonempty'] != (count > 0):
        raise CoreError('canonical plan status disagrees with the physical plan task set')
    task_ids = {task['id'] for task in tasks}
    if any(item not in task_ids for item in status['actionable']):
        raise CoreError('canonical plan status names an unknown actionable task')
    return {'present': True, 'source': source_descriptor(path, repo), 'plan_id': plan_id, 'goal': bounded_line(raw.get('goal', ''), 1000), 'acceptance_present': bool(acceptance), 'acceptance_digest': sha256_text(acceptance) if acceptance else '', 'tasks': tasks, 'task_count': status['task_count'], 'nonempty': status['nonempty'], 'all_done': status['all_done'], 'has_unfinished': status['has_unfinished'], 'actionable': list(status['actionable']), 'contract': dict(status['contract']), 'counts': dict(sorted(collections.Counter(task['state'] for task in tasks).items())), 'scope': {'allowed': sorted(set(all_allowed)), 'forbidden': sorted(set(all_forbidden))}}

def _latest_executor(repo: Path) -> Dict[str, Any]:
    candidates: List[Tuple[Path, Dict[str, Any]]] = []
    for root in (repo / '.oms' / 'executors', repo / '.oms' / 'executor'):
        if not root.is_dir() or root.is_symlink():
            continue
        for path in root.glob('*/meta.json'):
            if path.is_file() and not path.is_symlink():
                raw = read_json(path, default={})
                if isinstance(raw, dict): candidates.append((path, dict(raw)))
    active = [(path, row) for path, row in candidates if str(row.get('state', '')).lower() in {'draft', 'frozen', 'running'}]
    if not active:
        return {'present': False, 'active': False, 'source': None, 'scope': {'allowed': [], 'forbidden': []}}
    path, raw = max(active, key=lambda item: item[0].stat().st_mtime)
    verify = str(raw.get('verify', '') or ''); state = bounded_line(raw.get('state', ''), 40).lower()
    return {'present': True, 'active': True, 'source': source_descriptor(path, repo), 'id': bounded_line(raw.get('executor_id', raw.get('id', path.parent.name)), 160), 'state': state, 'provider': bounded_line(raw.get('provider', ''), 80), 'model': bounded_line(raw.get('selected_model', raw.get('model', '')), 120), 'reasoning_effort': bounded_line(raw.get('reasoning_effort', ''), 40), 'task_id': bounded_line(raw.get('plan_task', raw.get('task_id', '')), 160), 'base_sha': bounded_line(raw.get('base_sha', ''), 80), 'scope': {'allowed': parse_path_list(raw.get('allowed_paths', raw.get('allowed', []))), 'forbidden': parse_path_list(raw.get('forbidden_paths', raw.get('forbidden', [])))}, 'verify_present': bool(verify), 'verify_digest': sha256_text(verify) if verify else '', 'frozen': state in ('frozen', 'running') and bool(raw.get('soul_sha256'))}

def _failure_paths(repo: Path) -> List[Path]:
    oms = repo / '.oms'; canonical = oms / 'failures.jsonl'; candidates = [oms / 'fail-ledger.jsonl']
    for root_name in ('failures', 'failure'):
        root = oms / root_name
        if root.is_dir() and not root.is_symlink(): candidates.extend(root.rglob('*.jsonl'))
    paths = {path for path in candidates if path.is_file() and not path.is_symlink()}
    if canonical.exists() or canonical.is_symlink():
        paths.add(canonical)
    return sorted(paths)

def _canonical_ledger_failures(repo: Path, path: Path) -> List[Dict[str, Any]]:
    # The canonical ledger does not carry resolution in the failing row: a fix
    # is a separate `event: "resolved"` row keyed by fingerprint, and hook rows
    # retire on a read-time TTL. Reading each row's own status field saw
    # neither, so a resolved failure stayed open forever, every repeat of one
    # command counted again, and resolve_blocker outranked every other next
    # action for as long as the ledger existed. That replay already has an
    # owner -- and a comment naming every site that must agree with it -- so
    # ask fail-ledger instead of keeping a sixth copy of the predicate here.
    if path.is_symlink() or not path.is_file():
        raise CoreError('canonical failure ledger must be a regular non-symlink file')
    script = install_root() / 'scripts' / 'fail-ledger.sh'
    if not script.is_file() or script.is_symlink():
        raise CoreError('canonical failure ledger projection command is unavailable')
    payload = run_json(['bash', str(script), '--repo', str(repo), 'list', '--unresolved', '--json'], cwd=repo, timeout=20)
    if not isinstance(payload, dict) or payload.get('schema') != 1:
        raise CoreError('canonical failure ledger projection is unavailable or invalid')
    rows = payload.get('failures')
    if not isinstance(rows, list):
        raise CoreError('canonical failure ledger projection has invalid failures')
    result: List[Dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, Mapping):
            raise CoreError('canonical failure ledger projection contains a non-object row')
        if (row.get('attention') not in {'none', 'retiring', 'actionable'} or
                not isinstance(row.get('actionable'), bool) or
                not isinstance(row.get('retiring'), bool)):
            raise CoreError('canonical failure ledger projection contains invalid attention state')
        if row.get('attention') == 'retiring' or row.get('actionable') is False:
            continue
        summary = bounded_line(row.get('summary') or row.get('cmd') or '', 300)
        result.append({'id': bounded_line(row.get('fingerprint', ''), 160), 'kind': bounded_line(row.get('kind', ''), 80), 'summary': summary, 'classification': classify(summary, row.get('exit')), 'source': relative_path(path, repo)})
    return result

def failure_rows(repo: Path) -> List[Dict[str, Any]]:
    result: List[Dict[str, Any]] = []
    canonical = repo / '.oms' / 'failures.jsonl'
    for path in _failure_paths(repo):
        if path == canonical:
            result.extend(_canonical_ledger_failures(repo, path))
            continue
        for row in read_jsonl(path, limit_rows=MAX_JSONL_ROWS):
            if str(row.get('status', row.get('state', 'open'))).lower() in ('resolved', 'closed', 'superseded', 'done'): continue
            summary = bounded_line(row.get('summary', row.get('reason', row.get('message', ''))), 300)
            explicit = bounded_line(row.get('reason_code', row.get('code', '')), 80)
            result.append({'id': bounded_line(row.get('id', row.get('event_id', '')), 160), 'kind': bounded_line(row.get('kind', ''), 80), 'summary': summary, 'classification': classify(summary, row.get('exit'), explicit=explicit), 'source': relative_path(path, repo)})
    return result[-100:]

def _merge_scope(*scopes: Mapping[str, Any]) -> Dict[str, Any]:
    layers: List[Dict[str, Any]] = []; forbidden: List[str] = []; effective_allowed: List[str] = []; effective_source = 'unbounded'; names = ('project', 'task', 'plan', 'executor')
    for index, scope in enumerate(scopes):
        allowed = parse_path_list(scope.get('allowed', [])); denied = parse_path_list(scope.get('forbidden', [])); name = names[index] if index < len(names) else 'layer-%d' % index
        layers.append({'source': name, 'allowed': allowed, 'forbidden': denied}); forbidden.extend(denied)
        if allowed: effective_allowed = allowed; effective_source = name
    return {'allowed': effective_allowed, 'allowed_source': effective_source, 'forbidden': sorted(set(forbidden)), 'layers': layers, 'authoritative': False}

def _claim_criterion_id(claimed: Dict[str, str], criterion_id: str, text: str) -> bool:
    """Reserve one criterion id for one statement. Returns False for a repeat.

    The id is the join key evidence binds to, and coverage collapses ids to a
    set, so two different statements sharing one id are both marked verified by
    the first receipt: `[id:safe] the API stays compatible` and `[id:safe] no
    data is lost` reach 100% coverage when only one of them was ever checked.
    Deduplication used to be keyed by (id, text), which kept both rows and hid
    the collision instead of naming it.
    """
    previous = claimed.get(criterion_id)
    if previous is None:
        claimed[criterion_id] = text
        return True
    if previous == text:
        return False
    raise CoreError("criterion id %s names two different criteria; give each acceptance criterion its own [id:...]" % criterion_id)

def _criteria(project: Mapping[str, Any], task: Mapping[str, Any], plan: Mapping[str, Any]) -> List[Dict[str, Any]]:
    result: List[Dict[str, Any]] = []; seen: Dict[str, str] = {}
    for source, values, weight in (('project', project.get('criteria', []), 2), ('task', task.get('criteria', []), 2)):
        if not isinstance(values, list): continue
        for raw in values:
            text = strip_criterion_marker(normalized_line(str(raw)))
            if not text: continue
            criterion_id = stable_criterion_id(source, str(raw))
            if not _claim_criterion_id(seen, criterion_id, text.lower()): continue
            result.append({'id': criterion_id, 'text': bounded_line(text, 500), 'source': source, 'weight': weight})
    if plan.get('acceptance_present'):
        digest = str(plan.get('acceptance_digest', '')); result.append({'id': 'criterion-plan-acceptance-' + digest[:10], 'text': 'The plan-level acceptance command passes on the final tree.', 'source': 'plan', 'weight': 3, 'command_digest': digest})
    # Every plan task is a criterion of its own: an explicit [id:...] in the
    # title wins, otherwise the task id names it deterministically. Admission
    # receipts for the task are the evidence (see evidence.build_coverage) —
    # admission re-ran the task's verify against an isolated tree, which is a
    # stronger claim than the task row's own state field.
    for row in plan.get('tasks', []):
        task_id = str(row.get('id', ''))
        if not task_id:
            continue
        title = str(row.get('title', ''))
        explicit = re.search(r"\[(?:id|criterion):\s*([A-Za-z0-9._:-]{1,80})\]", title, re.I)
        criterion_id = explicit.group(1) if explicit else 'plan-task-' + task_id
        if not _claim_criterion_id(seen, criterion_id, task_id.lower()):
            continue
        result.append({'id': criterion_id, 'text': bounded_line(strip_criterion_marker(title) or ('Plan task %s is admitted.' % task_id), 500), 'source': 'plan-task', 'weight': 1, 'plan_task_id': task_id, 'plan_id': str(plan.get('plan_id', ''))})
    return result

def _objective(project: Mapping[str, Any], task: Mapping[str, Any], plan: Mapping[str, Any]) -> Dict[str, str]:
    for source, value in (('task', task.get('goal')), ('plan', plan.get('goal')), ('project', project.get('goal'))):
        if value: return {'text': bounded_line(value, 1000), 'source': source}
    return {'text': '', 'source': 'none'}

def _actions(project: Mapping[str, Any], task: Mapping[str, Any], plan: Mapping[str, Any], evidence: Mapping[str, Any], failures: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    result: List[Dict[str, Any]] = []
    def add(action_id: str, priority: int, authority: str, reason: str, command: str) -> None:
        result.append({'id': action_id, 'priority': priority, 'authority': authority, 'reason': bounded_line(reason, 300), 'command': command})
    if not project.get('present'): add('initialize_project', 100, 'repo_write', 'PROJECT.md is absent.', 'oms init')
    if failures:
        top = failures[-1]; add('resolve_blocker', 95, 'read', 'An unresolved %s failure is recorded.' % top.get('classification', {}).get('code', 'unknown'), 'oms fail-ledger list')
    tasks = plan.get('tasks', []) if isinstance(plan.get('tasks'), list) else []
    empty_plan = bool(plan.get('present')) and not bool(plan.get('nonempty'))
    if empty_plan: add('repair_empty_plan', 88, 'append', 'The active plan has no tasks, so completion cannot be proven.', 'oms plan-from-spec --repo .')
    landing = [row for row in tasks if row.get('state') == 'landing']; review = [row for row in tasks if row.get('state') == 'review']; stored_ready = [row for row in tasks if row.get('state') == 'ready']; running = [row for row in tasks if row.get('state') in ('claimed', 'running')]; active_plan = bool(landing or review or stored_ready or running)
    actionable_ids = set(plan.get('actionable', [])) if isinstance(plan.get('actionable'), list) else set()
    claimable = [row for row in tasks if row.get('id') in actionable_ids]
    contract = plan.get('contract', {}) if isinstance(plan.get('contract'), dict) else {}
    contract_blocked = contract.get('bound') is True and contract.get('satisfied') is False
    if landing: add('finish_landing', 90, 'repo_write', 'A task is inside the landing transaction.', 'oms patch-land --recover')
    elif review:
        task_id = bounded_line(review[0].get('id', ''), 160); add('review_or_land_patch', 85, 'repo_write', 'A reviewed task has a frozen patch awaiting the parent decision.', 'oms plan-run --id %s --land' % task_id if task_id else 'oms plan-run --next --land')
    elif claimable: add('execute_ready_task', 80, 'worktree_write', '%d plan task(s) are ready.' % len(claimable), 'oms plan-run --next')
    elif running: add('inspect_active_attempt', 75, 'read', 'A plan task is claimed or running.', 'oms state')
    if contract_blocked and stored_ready:
        blocker = bounded_line(contract.get('blocker', 'unknown'), 80) or 'unknown'
        add('inspect_plan_contract', 89, 'read', 'The reviewed plan is blocked by PROJECT.md contract state: %s.' % blocker, 'oms agent-plan --repo . status --json')
    if task.get('present') and not evidence.get('complete'):
        if task.get('verification') != 'fresh': add('verify_active_task', 70, 'read', 'Acceptance evidence is incomplete and the active task gate is not fresh.', 'oms agent-task verify')
        else: add('resolve_evidence_gaps', 68, 'read', 'The task gate is fresh but one or more declared criteria still lack current evidence.', 'oms runtime evidence show')
    if evidence.get('complete') and not active_plan and not empty_plan:
        if task.get('present'):
            add('record_verified_completion', 50, 'append', 'Every declared criterion has current evidence and no plan task remains active.', 'oms agent-task close')
        elif plan.get('present') and plan.get('all_done'):
            add('inspect_completed_plan_retirement', 50, 'read', 'Every declared criterion has current evidence and the completed plan is still active.', 'oms agent-plan --repo . retire --check')
    if not result: add('orient', 10, 'read', 'No stronger deterministic transition is available.', 'oms inbox')
    return sorted(result, key=lambda item: (-int(item['priority']), str(item['id'])))

def build_base_envelope(repo: Path) -> Dict[str, Any]:
    project = _project_file(repo); task = _task_packet(repo); plan = _plan_state(repo); executor = _latest_executor(repo); failures = failure_rows(repo)
    scope = _merge_scope(project.get('scope', {}), task.get('scope', {}), plan.get('scope', {}), executor.get('scope', {}))
    sources = [item for item in (project.get('source'), task.get('source'), plan.get('source'), executor.get('source')) if item]
    head = git_head(repo); state_digest = sha256_bytes(canonical_json({'head': head, 'sources': sources, 'failures': failures})); warnings: List[str] = []; criteria = _criteria(project, task, plan)
    if not criteria: warnings.append('No explicit acceptance criteria were found; completion coverage cannot be proven.')
    if plan.get('present') and not plan.get('acceptance_present'): warnings.append('The active plan has no plan-level acceptance command.')
    if plan.get('present') and not plan.get('nonempty'): warnings.append('The active plan has no tasks; completion is gated until plan work is defined.')
    if executor.get('present') and executor.get('base_sha') and head and executor.get('base_sha') != head: warnings.append('The latest executor was frozen against a different Git commit.')
    if failures: warnings.append('%d unresolved failure record(s) require attention.' % len(failures))
    return {'schema': ENVELOPE_SCHEMA, 'generated_at': utc_now(), 'repo': {'head': head or None, 'branch': git_branch(repo) or None}, 'state_digest': state_digest, 'sources': sources, 'objective': _objective(project, task, plan), 'scope': scope, 'criteria': criteria, 'budget': task.get('budget', {}), 'task': {key: value for key, value in task.items() if key not in ('criteria', 'source', 'constraints')}, 'plan': {key: value for key, value in plan.items() if key != 'source'}, 'executor': {key: value for key, value in executor.items() if key != 'source'}, 'authority': {'repo_write': 'external_parent_decision', 'remote_create': 'external_parent_decision', 'authority_transferable_by_capsule': False}, 'failures': failures, 'warnings': warnings}

def _completion_state(task: Mapping[str, Any], evidence: Mapping[str, Any]) -> str:
    """Derived completion judgment: evidence decides, never a model's confidence.

    completed_verified only when every declared criterion carries current
    verified evidence; a closed task with anything less is completed with
    unverified items, which is a different promise to the caller.
    """
    status = str(task.get('status', '') or '').lower()
    if status in ('blocked', 'failed', 'cancelled'):
        return status
    counts = evidence.get('counts', {}) if isinstance(evidence.get('counts'), Mapping) else {}
    failing = int(counts.get('failed', 0) or 0)
    if status in ('closed', 'done', 'verified', 'completed'):
        if failing:
            return 'failed'
        return 'completed_verified' if evidence.get('complete') else 'completed_with_unverified_items'
    return 'active' if task.get('present') else 'none'


def finalize_envelope(base: Dict[str, Any], evidence: Mapping[str, Any]) -> Dict[str, Any]:
    base = dict(base)
    effective_evidence = dict(evidence)
    plan = base.get('plan', {}) if isinstance(base.get('plan'), dict) else {}
    if plan.get('present') and not plan.get('nonempty'):
        effective_evidence['complete'] = False
        effective_evidence['completion_blocker'] = 'empty-plan'
    base['criteria'] = effective_evidence.get('criteria', []); base['evidence'] = {key: value for key, value in effective_evidence.items() if key != 'criteria'}
    if isinstance(base.get('task'), dict):
        base['task'] = dict(base['task'], completion=_completion_state(base['task'], effective_evidence))
    base['next_actions'] = _actions({'present': any(source.get('path') == 'PROJECT.md' for source in base.get('sources', []))}, base.get('task', {}), base.get('plan', {}), effective_evidence, base.get('failures', []))
    base['state_digest'] = sha256_bytes(canonical_json({'contract': base['state_digest'], 'evidence': base['evidence']}))
    return base
