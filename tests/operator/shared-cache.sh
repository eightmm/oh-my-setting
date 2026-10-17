# shellcheck shell=bash
PYTHONDONTWRITEBYTECODE=1 python3 - "$ROOT/scripts/lib" "$TMP" <<'PY' || fail "shared panel cache regression"
import inspect
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import time
from unittest.mock import patch

sys.path.insert(0, sys.argv[1])
import panel_cache as cache
import panel_results
import terminal_panel as panel

# Compact line views stay readable without adding room scans.
room_rows = [{"id": "room-current", "title": "Shared work", "closed": False,
              "participants": [{"participant": "main-live", "joined": True, "role": "main"},
                               {"participant": "main-idle", "joined": True, "role": "main"}],
              "messages": [{"pending_for": ["a", "b"], "ts": "2026-10-12T00:00:00+00:00"}]}]
room_text = "\n".join(panel.room_listing(room_rows, "room-current", 200, now=1791763320,
                                          live_participants={"main-live"}))
assert "* 1 room-current" in room_text and "main live" in room_text and "live mains" not in room_text and "2 unread" in room_text, room_text
assert "last 2m ago / open" in room_text, room_text
assert room_text.startswith("╭─ OMS · Rooms") and room_text.splitlines()[-1].startswith("╰"), room_text

with patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((57, 24))), \
        patch("sys.stdout", new=io.StringIO()) as provider_text:
    panel.show_providers([{"provider": "fixture", "installed": True, "access": ["read"],
                           "native_launch": False, "exact_model_override": True, "models": []}])
provider_lines = provider_text.getvalue().splitlines()
assert all(len(line) <= 57 for line in provider_lines), provider_lines
assert not any(line.endswith("supp") or line == "    lied" for line in provider_lines), provider_lines

with patch.object(panel, "terminal_style", return_value=(True, False, False)), \
        patch("sys.stdout", new=io.StringIO()) as clear_text:
    panel.clear_control_view()
assert clear_text.getvalue() == "\033[H\033[2J"
with patch.object(panel, "terminal_style", return_value=(False, False, False)), \
        patch("sys.stdout", new=io.StringIO()) as plain_text:
    panel.clear_control_view()
assert plain_text.getvalue() == ""

empty_prompts = []
with patch.object(panel, "results", return_value={"rows": []}), \
        patch.object(panel, "clear_control_view"), patch.object(panel, "render_results", return_value="empty"), \
        patch.object(panel, "read_field", side_effect=lambda label, required=False: empty_prompts.append(label) or ""):
    panel.browse_results(Path("."))
assert empty_prompts == ["Enter to return: "], empty_prompts
capacity_source = inspect.getsource(panel.interactive)
assert "Enter returns to controls; 9 refreshes" not in capacity_source
assert 'read_field("Enter to return / 9 Refresh: "' in capacity_source

project = Path(sys.argv[2]) / 'shared-panel'
(project / '.oms/hooks').mkdir(parents=True)
(project / '.oms/artifacts').mkdir()
(project / '.oms/plan').mkdir()
draft_artifact = project / '.oms/artifacts/answer.md'
draft_artifact.write_text('Answer is ready.\n', encoding='utf-8')
draft_path = panel_results.write_summary_draft(project, {
    'task_id': 'draft-task', 'next_action': 'record main decision',
    'stages': {'implement': {'state': 'passed'}, 'verify': {'state': 'failed'}},
    'calls': [{'artifact': '.oms/artifacts/answer.md'}]})
draft_bytes = (project / draft_path).read_bytes()
assert draft_path.startswith('.oms/artifacts/panel-results/summary-draft-task-')
assert len(draft_bytes) <= 4096 and b'Task: draft-task' in draft_bytes
assert b'Next action: record main decision' in draft_bytes and b'verify: failed' in draft_bytes
assert b'.oms/artifacts/answer.md' in draft_bytes
large_refs = []
for number in range(80):
    relative = '.oms/artifacts/' + ('문서' * 12) + '-%d.md' % number
    (project / relative).write_text('Evidence.\n', encoding='utf-8')
    large_refs.append({'artifact': relative})
large_draft = panel_results.write_summary_draft(project, {'task_id': 'draft-large', 'calls': large_refs})
large_bytes = (project / large_draft).read_bytes()
assert len(large_bytes) <= 4096 and large_bytes.endswith(b'\n'), len(large_bytes)
assert '문서' in large_bytes.decode('utf-8')
selected_row = {'task_id': 'selected-task', 'stages': {'implement': {'state': 'passed'}},
                'next_action': 'record main decision', 'calls': [{'attempt_id': 'selected-attempt',
                'artifact': '.oms/artifacts/answer.md'}]}
selected_labels = []
selected_calls = [0]
def selected_menu(repo, provider, previous, view, attention_only, navigation, cache):
    if not selected_calls[0]:
        cache.update(status=0, state={'collection': {'ok': True}, 'attempts': {'active_recent': []},
                                      'room_attempts': {'selected-member': {
                                          'attempt_id': 'selected-attempt', 'task_id': 'selected-task'}}})
        navigation['selected'] = ('result', 'selected-member')
        selected_calls[0] += 1
        return 'f'
    return 'q'
def selected_prompt(label, required=True):
    selected_labels.append(label)
    return ''
with patch.object(panel, 'panel_mains', return_value=['claude']), \
        patch.object(panel, 'managed_session', return_value=False), \
        patch.object(panel, 'read_choice', side_effect=selected_menu), \
        patch.object(panel, 'read_field', side_effect=selected_prompt), \
        patch.object(panel, 'results', return_value={'rows': [selected_row]}), \
        patch.object(panel.dispatch_store, 'read', return_value={'verify': 'python3 -m unittest'}), \
        patch.object(panel, 'finalize', return_value={'delivery': {'receipt': {'status': 'not_requested'}}}) as selected_finalize, \
        patch.object(panel, 'render_results', return_value='selected result'), \
        patch.object(panel, 'pending_admissions', return_value=set()), \
        patch.object(panel.sys, 'stdout', io.StringIO()):
    assert panel.interactive(project) == 0
assert selected_finalize.call_args.args[2:7] == (
    'selected-task', selected_finalize.call_args.args[3], 'completed', 'python3 -m unittest', '.oms/artifacts/answer.md')
assert selected_finalize.call_args.args[3].startswith('.oms/artifacts/panel-results/summary-selected-task-')
assert any('[selected-task]' in label for label in selected_labels), selected_labels
assert any('[completed]' in label for label in selected_labels), selected_labels
assert any('[python3 -m unittest]' in label for label in selected_labels), selected_labels
assert any('[.oms/artifacts/answer.md]' in label for label in selected_labels), selected_labels
selected_calls[0] = 0
selected_labels.clear()
def attention_menu(repo, provider, previous, view, attention_only, navigation, cache):
    if not selected_calls[0]:
        cache.update(status=0, state={"room": {"id": "selected-room", "participants": [
            {"participant": "selected-member", "role": "worker", "joined": True}]},
            "attempts": {"active_recent": [{"attempt_id": "selected-attempt", "task_id": "selected-task", "state": "review",
                "panel": {"room_id": "selected-room", "room_participant": "selected-member", "role": "worker"}}]}})
        navigation.update(selected=("chat", "unrelated-main"),
                          finalize_call={"task_id": "selected-task", "target": ("result", "selected-member")})
        selected_calls[0] += 1
        return "f"
    return "q"
with patch.object(panel, 'panel_mains', return_value=['claude']), \
        patch.object(panel, 'managed_session', return_value=False), \
        patch.object(panel, 'read_choice', side_effect=attention_menu), \
        patch.object(panel, 'read_field', side_effect=selected_prompt), \
        patch.object(panel, 'results', return_value={'rows': [selected_row]}), \
        patch.object(panel.dispatch_store, 'read', return_value={'verify': 'python3 -m unittest'}), \
        patch.object(panel, 'finalize', return_value={'delivery': {'receipt': {'status': 'not_requested'}}}) as attention_finalize, \
        patch.object(panel, 'render_results', return_value='selected result'), \
        patch.object(panel, 'pending_admissions', return_value=set()), \
        patch.object(panel.sys, 'stdout', io.StringIO()):
    assert panel.interactive(project) == 0
assert attention_finalize.call_args.args[2] == 'selected-task'
assert attention_finalize.call_args.args[4:7] == ('completed', 'python3 -m unittest', '.oms/artifacts/answer.md')
plan = project / '.oms/plan/tasks.json'
plan.write_text('first', encoding='utf-8')
counter = project / 'computations'
worker = '''
import json, sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0, sys.argv[1])
import panel_cache as cache
repo, counter = Path(sys.argv[2]), Path(sys.argv[3])
def compute():
    number = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(number))
    return {'number': number}
if len(sys.argv) > 4:
    cache.time.time = lambda: float(sys.argv[4])
try:
    print(json.dumps(cache.read_shared(repo, 'dashboard', compute)))
except ValueError:
    print(json.dumps({'pending': True}))
'''
def board(now=None):
    command = [sys.executable, '-c', worker, sys.argv[1], str(project), str(counter)]
    if now is not None:
        command.append(str(now))
    return json.loads(subprocess.check_output(command, text=True, timeout=5))

assert board() == board() == {'number': 1}, counter.read_text()
path = project / cache.DIRECTORY / 'dashboard.json'
created = json.loads(path.read_text())['created']
with panel_results._lock(project, 'panel-cache-dashboard', timeout=0):
    start = time.monotonic()
    assert board(created + cache.INTERVAL + 1) == {'number': 1}
    assert time.monotonic() - start < 2, 'contending board blocked'
    plan.write_text('second generation', encoding='utf-8')
    # A board never replays another input generation; with the writer busy it reads for itself.
    assert board() == {'number': 2}, 'replayed a different input generation'
assert counter.read_text() == '2'
assert board() == {'number': 3}
assert board(json.loads(path.read_text())['created'] + cache.INTERVAL + 1) == {'number': 4}
path.write_text('{"schema":1,', encoding='utf-8')
assert board() == {'number': 5}, 'corrupt cache was reused'
path.write_text('{"schema":1}', encoding='utf-8')
assert board() == {'number': 6}, 'partial cache was reused'
entry = json.loads(path.read_text())
entry['value'] = {'number': 999}
path.write_text(json.dumps(entry), encoding='utf-8')
assert board() == {'number': 7}, 'damaged payload was reused'
entry = json.loads(path.read_text())
entry['created'] = 10 ** 400
path.write_text(json.dumps(entry), encoding='utf-8')
assert board() == {'number': 8}, 'overflowing cache timestamp did not trigger recollection'

# Plan reads outlive the dashboard interval, including across board processes.
plan.write_text('{"tasks": {}}', encoding='utf-8')
plan_worker = worker.replace("cache.read_shared(repo, 'dashboard', compute)",
                             "cache.read_plan_list(repo, lambda: {'tasks': [compute()]})")
def plan_board(now):
    return json.loads(subprocess.check_output(
        [sys.executable, '-c', plan_worker, sys.argv[1], str(project), str(counter), str(now)],
        text=True, timeout=5))
now = int(time.time())
assert plan_board(now) == plan_board(now + 5) == plan_board(now + 10) == {'tasks': [{'number': 9}]}
replacement = plan.with_suffix('.tmp')
replacement.write_text(plan.read_text(), encoding='utf-8')
replacement.replace(plan)
assert plan_board(now + 15) == {'tasks': [{'number': 10}]}, 'same-size replacement was missed'
os.utime(plan, ns=(plan.stat().st_atime_ns, plan.stat().st_mtime_ns + 1_000_000))
assert plan_board(now + 20) == {'tasks': [{'number': 11}]}, 'mtime change was missed'

# Measure real agent-plan invocations on fixture state only, using simulated
# five-second tick clocks so this regression needs no sleep or live repository.
plan.write_text(json.dumps({'schema': 3, 'tasks': {
    'cache-task': {'id': 'cache-task', 'state': 'ready', 'depends': [], 'title': 'fixture'}}}), encoding='utf-8')
plan_cache = project / cache.DIRECTORY / 'plan-list.json'
plan_cache.unlink()
real_run = subprocess.run
calls = []
def run_plan(command, **kwargs):
    if 'agent-plan' in command:
        started = time.perf_counter()
        result = real_run(command, **kwargs)
        calls.append((time.perf_counter() - started) * 1000)
        return result
    if command[0] == 'git' or command[:2] == ['bash', '-c']:
        return real_run(command, **kwargs)
    return subprocess.CompletedProcess(command, 0, '{}', '')
def measure(reader, repo=project, expected='cache-task', detailed=False):
    calls.clear()
    ticks, counts = [], []
    started = time.perf_counter()
    with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
            patch.object(panel.room, 'status', side_effect=lambda repo, ident: {'id': ident}), \
            patch.object(panel_results, 'completed_tasks', return_value={}), \
            patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
            patch.object(cache, 'read_plan_list', side_effect=reader), \
            patch.object(panel.subprocess, 'run', side_effect=run_plan):
        for tick in range(3):
            tick_start, tick_calls = time.perf_counter(), len(calls)
            with patch.object(cache.time, 'time', return_value=now + tick * 5):
                # A timed-out plan read is deliberately reported as unavailable;
                # the next snapshot retries it. Under load, wait for that
                # completion signal rather than indexing a partial room state.
                for attempt in range(3):
                    state, status = panel.snapshot(repo, 'board-%d' % tick)
                    if not state.get('room', {}).get('repo_tasks_unavailable'):
                        break
                assert status == 0 and state['room']['repo_tasks'][0]['id'] == expected, state
            ticks.append(round((time.perf_counter() - tick_start) * 1000, 1))
            counts.append(len(calls) - tick_calls)
    if detailed:
        return {'tick_ms': ticks, 'tick_calls': counts}
    return len(calls), (time.perf_counter() - started) * 1000
baseline = measure(lambda repo, collect: collect())
cached = measure(cache.read_plan_list)
assert baseline[0] == 3 and cached[0] == 1, (baseline, cached)
os.utime(plan, ns=(plan.stat().st_atime_ns, plan.stat().st_mtime_ns + 1_000_000))
touched = measure(cache.read_plan_list)
assert touched[0] == 1, touched
print('plan-list fixture: before=%d calls %.1f ms; after=%d calls %.1f ms; touched=%d calls %.1f ms'
      % (baseline + cached + touched))

with patch.object(cache, 'fingerprint', side_effect=AssertionError('plan cache walked the repository')):
    assert cache.read_plan_list(project, lambda: None)['tasks'][0]['id'] == 'cache-task'
def raced_plan():
    plan.write_text(plan.read_text() + ' ', encoding='utf-8')
    return {'tasks': []}
plan_cache.unlink()
assert cache.read_plan_list(project, raced_plan) == {'tasks': []}
assert not plan_cache.exists(), 'plan changed during collection was published'
completions = project / '.oms/plan/completions'
completions.mkdir()
assert cache.read_plan_list(project, lambda: {'tasks': []}) == {'tasks': []}
assert plan_cache.exists(), 'empty completion namespace prevented reuse'
completions.rmdir()

# Refresh ages locally; at the exact TTL boundary consult the engine again.
plan_cache.unlink(missing_ok=True)
anchor = int(time.time()) - 20
claim = {'id': 'claim', 'state': 'claimed', 'depends': [], 'title': 'fixture',
         'claimed_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(anchor)), 'ttl': '10',
         'claim_expired': False, 'claim_age_s': 0}
stored_claim = dict(claim)
stored_claim.pop('claim_expired')
stored_claim.pop('claim_age_s')
plan.write_text(json.dumps({'schema': 3, 'tasks': {'claim': stored_claim}}), encoding='utf-8')
with patch.object(cache.time, 'time', return_value=anchor):
    assert cache.read_plan_list(project, lambda: {'tasks': [claim]})['tasks'][0]['claim_expired'] is False
with patch.object(cache.time, 'time', return_value=anchor + 5):
    assert cache.read_plan_list(project, lambda: None)['tasks'][0]['claim_age_s'] == 5
with patch.object(cache.time, 'time', return_value=anchor + 10):
    with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
            patch.object(panel.room, 'status', return_value={'id': 'ttl'}), \
            patch.object(panel_results, 'completed_tasks', return_value={}), \
            patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
            patch.object(panel.subprocess, 'run', side_effect=run_plan) as command:
        state, status = panel.snapshot(project, 'ttl')
        assert status == 0 and state['room']['repo_tasks'][0]['claim_expired'] is True
        assert command.call_count == 2, 'expired claim did not reach agent-plan'

# Failed reads leave the room drawn and retry on the following tick.
plan_cache.unlink()
with patch.object(panel, 'dashboard', return_value=({'plan': {'present': True}}, 0)), \
        patch.object(panel.room, 'status', return_value={'id': 'retry'}), \
        patch.object(panel_results, 'completed_tasks', return_value={}), \
        patch.object(panel, 'read_shared', side_effect=lambda repo, key, collect, **kwargs: collect()), \
        patch.object(panel.subprocess, 'run', side_effect=[
            subprocess.CompletedProcess([], 0, '{}', ''), subprocess.CompletedProcess([], 1, '', ''),
            subprocess.CompletedProcess([], 0, '{}', ''), subprocess.CompletedProcess([], 0, '{"tasks": []}', '')]) as command:
    state, status = panel.snapshot(project, 'retry')
    assert status == 0 and state['room']['repo_tasks_unavailable'] is True
    assert not plan_cache.exists(), 'failed read was cached'
    state, status = panel.snapshot(project, 'retry')
    assert status == 0 and state['room']['repo_tasks'] == [] and command.call_count == 4

# Scale the same real validation path to a mixed closed plan. Synthetic receipt
# digests/proofs use the canonical codec; every measured collection runs the
# actual agent-plan validator rather than mocking completion acceptance.
closed = Path(sys.argv[2]) / 'closed-panel'
(closed / '.oms/hooks').mkdir(parents=True)
(closed / '.oms/artifacts').mkdir()
(closed / '.oms/plan/completions').mkdir(parents=True)
root = Path(sys.argv[1]).parents[1]
fixture_env = {key: value for key, value in os.environ.items() if not key.startswith('OMS_')}
fixture_env['OMS_LOCK_DIR'] = str(closed / '.oms/fixture-locks')
real_run(['git', '-C', str(closed), 'init', '-q'], check=True, env=fixture_env)
def fixture_call(script, *args):
    result = real_run(['bash', str(root / 'scripts' / script), '--repo', str(closed), *args],
                      capture_output=True, text=True, check=True, timeout=20, env=fixture_env)
    return result.stdout.strip()
room_id = json.loads(fixture_call('room.sh', 'new', '--topic', 'Closed plan fixture', '--json'))['id']
main = fixture_call('agent-events.sh', 'start', '--provider', 'codex', '--tool', 'panel-main',
                    '--ref', 'panel_room_id=' + room_id, '--ref', 'panel_role=main',
                    '--then', 'starting', '--then', 'working')
fixture_call('room.sh', 'join', '--id', room_id, '--participant', main, '--provider', 'codex', '--role', 'main')
core = runpy.run_path(str(root / 'scripts/lib/plan-completion.py'))
receipt_codec = core['RECEIPT']
encoded, sha = core['encoded'], core['sha']
provenance = core['capture_prefixes'](str(closed), room_id)
namespace = core['namespace'](str(closed))
(closed / 'verify.sh').write_text(':\n', encoding='utf-8')
(closed / 'decision.md').write_text('Reviewed cancellation.\n', encoding='utf-8')
plan_id = 'plan_' + '1' * 32
closed_tasks, index_rows = {}, []
stamp = '2026-10-11T00:00:00Z'
def closed_ref(rel):
    return {'path': rel, 'sha256': sha((closed / rel).read_bytes())}
verifier_file = dict(closed_ref('verify.sh'), state='file', mode='100644')
def original_task(tid):
    return {'id': tid, 'title': 'Inspect source evidence', 'state': 'review', 'updated': stamp,
            'created': stamp, 'depends': [], 'provider': 'codex', 'role': 'analysis-worker',
            'claimed_by_participant': main, 'lease_epoch': 1, 'lease_id': 'lease_' + '2' * 32,
            'repair_count': 0, 'repair_artifact': '', 'executor_id': '', 'executor_soul_sha256': '',
            'autopilot_owner_id': '', 'verify': 'bash verify.sh', 'artifact': '.oms/artifacts/' + tid + '.md'}
def add_completion(number):
    tid = 'closed-%03d' % number
    original = original_task(tid)
    (closed / original['artifact']).write_text('Source evidence for ' + tid + '\n', encoding='utf-8')
    artifact = closed_ref(original['artifact'])
    row = dict(event_id='evt-' + tid, kind='call-import', exit=0, artifact=artifact['path'],
               artifact_sha256=artifact['sha256'], task_id=tid, plan_id=plan_id)
    index_rows.append(row)
    bundle = {'schema': 1, 'kind': 'main-research-accepted', 'plan_id': plan_id, 'task_id': tid,
              'task_sha256': receipt_codec['digest'](original), 'plan_sha256': '3' * 64,
              'state': original['state'], 'lease_id': original['lease_id'],
              'owner': {'provider': 'codex', 'participant': main, 'parent_provider': 'codex'},
              'original_contract': receipt_codec['projection'](original), 'review_summary': 'Reviewed source evidence',
              'reviewed_obligations': ['inspect'], 'requirements': [{
                  'id': 'inspect', 'obligation': original['title'], 'deliverable': 'Source findings',
                  'source_paths': ['verify.sh'], 'evidence': [artifact]}],
              'base_commit': '4' * 40, 'source_envelope': ['verify.sh'], 'source_manifest': [verifier_file],
              'verifier': {'command': 'bash verify.sh', 'files': [verifier_file],
                           'adopted': False, 'review_summary': 'Reviewed frozen verifier'},
              'reviewer': {'kind': 'native-panel-main'},
              'main_research': {'classification': 'research-only', 'provenance': 'main-reviewed-external',
                  'artifact': artifact, 'index_event_id': row['event_id'], 'original_deliverable': original['title'],
                  'read_only_contract': 'original-report-only', 'original_producer_status':
                  {key: row.get(key) for key in ('kind', 'exit', 'attempt_id', 'task_id', 'plan_id', 'base_sha')}}}
    bundle_sha = sha(encoded(bundle))
    reviewer = {'schema': 1, 'kind': 'native-panel-main', 'participant': main, 'parent_provider': 'codex',
                'room_id': room_id, 'attempt_id': main, 'bundle_sha256': bundle_sha}
    evidence = {'index': row, 'artifact': artifact, 'provenance': 'main-reviewed-external',
                'child_provenance_asserted': False, 'reviewer': reviewer}
    snapshot = {'head': '4' * 40, 'base_commit': '4' * 40,
                'manifest_sha256': sha(encoded(bundle['source_manifest'])), 'source_identities': {},
                'verifier_command_sha256': sha(b'bash verify.sh'), 'repository_sha256': namespace,
                'evidence': evidence, 'evidence_identities': {}, 'provenance': provenance}
    verification = {'exit': 0, 'command_sha256': snapshot['verifier_command_sha256'],
                    'output_sha256': sha(b''), 'output_bytes': 0}
    verification['proof_sha256'] = sha(encoded(dict(verification, snapshot_sha256=sha(encoded(snapshot)))))
    receipt = {'schema': 1, 'kind': 'oms-plan-completion', 'completion_kind': 'main-research-accepted',
               'bundle_sha256': bundle_sha, 'bundle': bundle, 'bundle_raw': encoded(bundle).decode(),
               'original_task': original, 'original_plan_sha256': bundle['plan_sha256'],
               'snapshot': snapshot, 'verification': verification, 'ts': stamp}
    rel = '.oms/plan/completions/' + sha(encoded(receipt)) + '.json'
    (closed / rel).write_bytes(encoded(receipt))
    closed_tasks[tid] = core['completion_task'](receipt, closed_ref(rel))
    return closed / rel
first_receipt = add_completion(0)
for number in range(1, 125):
    add_completion(number)
for number in range(125, 250):
    tid = 'closed-%03d' % number
    original = dict(original_task(tid), state='ready')
    cancellation = {'schema': 1, 'kind': 'operator-cancellation', 'ts': stamp, 'operator_consent': 'cancel',
                    'reason': 'Reviewed cancellation', 'state': 'ready', 'original_updated': stamp,
                    'lease_id_sha256': sha(original['lease_id'].encode()), 'plan_sha256': '3' * 64,
                    'task_sha256': sha(json.dumps(original, sort_keys=True, ensure_ascii=False,
                                                separators=(',', ':'), allow_nan=False).encode()),
                    'decision_artifact': 'decision.md', 'decision_artifact_sha256': closed_ref('decision.md')['sha256']}
    closed_tasks[tid] = dict(original, state='cancelled', cancellation=cancellation)
closed_plan = closed / '.oms/plan/tasks.json'
def save_closed():
    closed_plan.write_text(json.dumps({'schema': 3, 'plan_id': plan_id, 'tasks': closed_tasks}), encoding='utf-8')
    (closed / '.oms/artifacts/index.jsonl').write_bytes(b''.join(encoded(row) for row in index_rows))
save_closed()
readers = [('round3', cache.read_plan_list)]
if len(sys.argv) > 3:
    # Comparative timing is an explicit fixture benchmark, not a repeated CI
    # baseline. Ordinary regression runs require no historical Git objects.
    previous = {'__file__': str(Path(sys.argv[1]) / 'panel_cache.py')}
    exec(Path(sys.argv[3]).read_bytes(), previous)
    readers = [('round2', previous['read_plan_list']), ('round3', cache.read_plan_list)]
for label, reader in readers:
    (closed / cache.DIRECTORY / 'plan-list.json').unlink(missing_ok=True)
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan unchanged %s: %s' % (label, json.dumps(result)))
add_completion(250)
save_closed()
for label, reader in readers:
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan added %s: %s' % (label, json.dumps(result)))
os.utime(first_receipt, ns=(first_receipt.stat().st_atime_ns, first_receipt.stat().st_mtime_ns + 1_000_000))
for label, reader in readers:
    result = measure(reader, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == ([1, 0, 0] if label in ('round2', 'round3') else [1, 1, 1]), result
    print('closed-plan receipt-touched %s: %s' % (label, json.dumps(result)))
with patch.object(cache, '_plan_inputs', side_effect=AssertionError('dependencies reread on a cache hit')), \
        patch.object(Path, 'iterdir', side_effect=AssertionError('namespace walked on a cache hit')):
    assert len(cache.read_plan_list(closed, lambda: None)['tasks']) == 251
# External HOME/config aliases are normal on macOS and Fedora Atomic. Keep
# their resolved targets and detect retargeting without accepting repo symlinks.
outside = Path(sys.argv[2]) / 'external-home'
outside.mkdir()
(outside / '.gitconfig').write_text('', encoding='utf-8')
alias = Path(sys.argv[2]) / 'home-alias'
alias.symlink_to(outside, target_is_directory=True)
with patch.dict(os.environ, HOME=str(alias), GIT_CONFIG_GLOBAL=str(alias / '.gitconfig')):
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], result
    envelope = {'inputs': cache._plan_inputs(closed)}
    os.utime(outside / '.gitconfig', None)
    assert not cache._plan_inputs_current(envelope), 'external config touch was missed'
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], result
    envelope = {'inputs': cache._plan_inputs(closed)}
    replacement_home = Path(sys.argv[2]) / 'replacement-home'
    replacement_home.mkdir()
    (replacement_home / '.gitconfig').write_text('', encoding='utf-8')
    alias.unlink()
    alias.symlink_to(replacement_home, target_is_directory=True)
    assert not cache._plan_inputs_current(envelope), 'external alias retarget was missed'
repo_alias = closed / 'unsafe-alias'
repo_alias.symlink_to(outside, target_is_directory=True)
try:
    cache._plan_input_stamp(repo_alias)
except ValueError:
    pass
else:
    raise AssertionError('repository symlink became cacheable')
repo_alias.unlink()
# Check parser parity with the engine definition without running its CLI body.
import ast
engine = ast.parse((root / 'scripts/lib/agent_plan_core.py').read_text())
parser = next(node for node in engine.body if isinstance(node, ast.FunctionDef) and node.name == 'claim_ttl_for')
for raw_default in ('', '0', '0010', '3600', '+10', ' 10', '١٠'):
    default = int(raw_default) if raw_default and all('0' <= digit <= '9' for digit in raw_default) else 3600
    namespace = {'CLAIM_TTL': default}
    exec(compile(ast.Module(body=[parser], type_ignores=[]), '<engine TTL>', 'exec'), namespace)
    with patch.dict(os.environ, OMS_PLAN_CLAIM_TTL=raw_default):
        for value in ('', '0', '0010', '١٠', '+10', ' 10', 10, None):
            task = {'ttl': value}
            assert cache._plan_claim_ttl(task) == namespace['claim_ttl_for'](task), (raw_default, value)
# Discovery pays the commit namespace resolver once and guards only the pushed
# ref chain (plus replacement refs), not unrelated refs or the POSIX index.
receipt_paths = [first_receipt, next(path for path in (closed / '.oms/plan/completions').iterdir() if path != first_receipt)]
original_receipts = [path.read_bytes() for path in receipt_paths]
land_directory = Path(sys.argv[2]) / 'land-receipts'
land_directory.mkdir()
commit = '5' * 40
(land_directory / (commit + '-fixture.json')).write_text(json.dumps({'remote': 'origin', 'target': 'main'}))
remote_ref = closed / '.git/refs/remotes/origin/main'
remote_ref.parent.mkdir(parents=True)
resolved_ref = remote_ref.with_name('accepted')
remote_ref.write_text('ref: refs/remotes/origin/accepted\n')
resolved_ref.write_text(commit + '\n')
try:
    for path, raw in zip(receipt_paths, original_receipts):
        receipt = json.loads(raw)
        receipt['completion_kind'] = 'satisfied-by'
        receipt['bundle']['successor'] = {'product_kind': 'commit', 'commit_sha': commit}
        path.write_bytes(encoded(receipt))
    from unittest.mock import Mock
    resolver = Mock(return_value=str(land_directory))
    with patch.object(cache.runpy, 'run_path', return_value={'canonical_land_directory': resolver}) as loader:
        dependencies = cache._plan_inputs(closed)
    assert loader.call_count == resolver.call_count == 1, 'commit namespace resolved per receipt'
    paths = {item[0] for item in dependencies}
    assert {str(remote_ref), str(resolved_ref), str(closed / '.git/packed-refs')}.issubset(paths)
    envelope = {'inputs': dependencies}
    os.utime(resolved_ref, ns=(resolved_ref.stat().st_atime_ns, resolved_ref.stat().st_mtime_ns + 1_000_000))
    assert not cache._plan_inputs_current(envelope), 'resolved Git ref touch was missed'
    assert str(closed / '.git/refs/heads') not in paths, 'unrelated refs were stamped'
    assert (str(closed / '.git/index') in paths) == (os.name == 'nt')
finally:
    for path, raw in zip(receipt_paths, original_receipts):
        path.write_bytes(raw)
    remote_ref.unlink()
    resolved_ref.unlink()

for changed in (closed / 'decision.md', closed / '.oms/artifacts/closed-000.md',
                closed / provenance['capture']['path'], closed / provenance['room']['path'],
                closed / 'verify.sh'):
    os.utime(changed, ns=(changed.stat().st_atime_ns, changed.stat().st_mtime_ns + 1_000_000))
    result = measure(cache.read_plan_list, closed, 'closed-000', detailed=True)
    assert result['tick_calls'] == [1, 0, 0], (changed, result)
# A changed dependency reaches validation and its rejection does not reuse the
# previous successful board value or publish a failed collection.
(closed / 'decision.md').write_text('Changed decision bytes!\n', encoding='utf-8')
def collect_closed():
    result = real_run(['bash', str(root / 'scripts/oms'), 'agent-plan', '--repo', str(closed), 'list', '--json'],
                      capture_output=True, text=True, timeout=15, env=fixture_env)
    if result.returncode:
        raise ValueError(result.stderr)
    return json.loads(result.stdout)
for tick in range(2):
    try:
        cache.read_plan_list(closed, collect_closed)
    except ValueError as error:
        assert 'cancellation decision artifact digest changed' in str(error), error
    else:
        raise AssertionError('changed decision bypassed actual validation')

# The managed patch reader's private certificate/anchor are validation inputs,
# even though their bytes live outside the repository. Probe dependency discovery
# directly; this altered receipt is never presented as an accepted completion.
import shutil
capture = core['CAPTURE']
source_patch = closed / 'fixture.patch'
source_patch.write_bytes(b'fixture patch bytes\n')
private_patch = capture['allocate'](str(closed), str(source_patch), 'land-cache-fixture', sha(source_patch.read_bytes()))
private_dir = Path(private_patch).parent
receipt_bytes = first_receipt.read_bytes()
try:
    publication = closed / '.oms/landing-patches/land-cache-fixture.patch'
    publication.parent.mkdir()
    capture['publish'](str(closed), private_patch, str(publication))
    altered = json.loads(receipt_bytes)
    altered['bundle']['requirements'][0]['evidence'].append(closed_ref('.oms/landing-patches/land-cache-fixture.patch'))
    first_receipt.write_bytes(encoded(altered))
    dependencies = cache._plan_inputs(closed)
    paths = {item[0] for item in dependencies}
    assert str(private_dir / 'owner.json') in paths and str(private_dir / 'publication.anchor') in paths, paths
    assert str(closed / '.oms/landings.jsonl') in paths
    for changed in (private_dir / 'owner.json', private_dir / 'publication.anchor'):
        envelope = {'inputs': cache._plan_inputs(closed)}
        os.utime(changed, ns=(changed.stat().st_atime_ns, changed.stat().st_mtime_ns + 1_000_000))
        assert not cache._plan_inputs_current(envelope), 'private patch generation input was missed'
finally:
    first_receipt.write_bytes(receipt_bytes)
    shutil.rmtree(private_dir)

# The board still selects its own room, and result queries share the same
# generation without retaining scoped answer text or skipping digest checks.
path.unlink()
with patch.object(panel, 'dashboard', return_value=({'repo': {'name': 'fixture'}}, 0)) as dashboard, \
        patch.object(panel.room, 'status', side_effect=lambda repo, ident: {'id': ident}), \
        patch.object(panel.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{}', '')):
    assert panel.snapshot(project, 'one')[0]['room']['id'] == 'one'
    assert panel.snapshot(project, 'two')[0]['room']['id'] == 'two'
    assert dashboard.call_count == 1
    with panel_results._lock(project, 'panel-cache-dashboard', timeout=0):
        plan.write_text('third generation', encoding='utf-8')
        # Inputs that change while another board collects never turn this board into "collection degraded".
        state, status = panel.snapshot(project, 'two')
        assert status == 0 and state['room']['id'] == 'two', state
    assert panel.snapshot(project, 'two')[0]['room']['id'] == 'two'
    assert dashboard.call_count == 3
with patch.object(panel_results, '_records', return_value=([], [])) as queries:
    assert panel_results.results(project, _shared=True)['rows'] == []
    assert panel_results.results(project, _shared=True)['rows'] == []
    assert queries.call_count == 1
    plan.write_text('fourth generation', encoding='utf-8')
    panel_results.results(project, _shared=True)
    assert queries.call_count == 2

# One board collection shares the full projection with D/E/F/G and the H tmux path,
# even when another writer invalidates every persistent cache during the tick.
from copy import deepcopy
import panel_chats
import panel_host
import panel_metrics

# Persist producer-shaped plan, lifecycle and artifact-index records. This also
# exercises the public text and JSON CLI surfaces without mocking readers.
import io
import hashlib
import runpy
import os
from contextlib import redirect_stdout
from contextlib import redirect_stderr
from datetime import datetime, timezone
import room as room_module
metric_repo = project / 'panel-metrics-fixture'
metric_repo.mkdir()
subprocess.run(['git', 'init', '-q', str(metric_repo)], check=True)
(metric_repo / '.oms/plan').mkdir(parents=True)
(metric_repo / '.oms/threads').mkdir()
now_metric = int(time.time())
def stamp_metric(offset):
    return datetime.fromtimestamp(now_metric + offset, timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
old_lock_dir = os.environ.get('OMS_LOCK_DIR')
os.environ['OMS_LOCK_DIR'] = str(metric_repo / '.oms/fixture-locks')
room_module.create(metric_repo, room='metric-room', title='Metrics fixture', creator='metric-main')
room_module.join(metric_repo, 'metric-room', 'metric-main', 'codex', role='main')
room_module.join(metric_repo, 'metric-room', 'metric-worker', 'codex', role='worker', parent='metric-main')
room_module.send(metric_repo, 'metric-room', 'metric-worker', 'metric-main',
                 'Follow-up task: followup', kind='handoff')
if old_lock_dir is None:
    os.environ.pop('OMS_LOCK_DIR', None)
else:
    os.environ['OMS_LOCK_DIR'] = old_lock_dir

# A ready task cancelled before claim is not started, even though claimed_at
# can be refreshed later by touch.
(metric_repo / '.oms/plan/tasks.json').write_text(json.dumps({'tasks': {
    'continued': {'id': 'continued', 'state': 'done', 'claimed_at': stamp_metric(-1), 'lease_epoch': 1},
    'recent': {'id': 'recent', 'state': 'done', 'claimed_at': stamp_metric(-1)},
    'abandoned': {'id': 'abandoned', 'state': 'running', 'claimed_at': stamp_metric(-1)},
    'followup': {'id': 'followup', 'state': 'running', 'claimed_at': stamp_metric(-1)},
    'claim-blocked': {'id': 'claim-blocked', 'state': 'blocked', 'provider': 'codex',
                      'claimed_at': stamp_metric(-1), 'lease_epoch': 1},
    'preflight-check': {'id': 'preflight-check', 'state': 'done', 'claimed_at': stamp_metric(-1),
                        'lease_epoch': 1},
    'awaiting-review': {'id': 'awaiting-review', 'state': 'review', 'claimed_at': stamp_metric(-90),
                        'updated_at': stamp_metric(-80), 'lease_epoch': 1},
    'ready-cancel': {'id': 'ready-cancel', 'state': 'cancelled', 'claimed_at': stamp_metric(-1),
                     'lease_epoch': 0, 'cancellation': {'state': 'ready', 'ts': stamp_metric(-90)}}}}), encoding='utf-8')

events_module = runpy.run_path(str(Path(sys.argv[1]) / 'agent-events.py'))
new_event = events_module['new_event']
event_rows = []
def add_attempt(attempt_id, task_id, role, access, start, end, final, reason='', parent='main-metric', extra_refs=None, tool='panel-dispatch'):
    refs = {'panel_role': role, 'panel_access': access,
            'panel_room_id': 'metric-room', 'panel_room_participant': attempt_id}
    refs.update(extra_refs or {})
    created = new_event(attempt_id, 1, 'attempt.created', run_id='run-metric', task_id=task_id,
                        parent_attempt_id=parent, provider='codex', tool=tool,
                        refs=refs, to_state='queued')
    created['ts'] = stamp_metric(start)
    event_rows.append(created)
    states = [('starting', start + 1), ('working', start + 2), (final, end)]
    previous = 'queued'
    seq = 2
    for state, offset in states:
        row = new_event(attempt_id, seq, 'attempt.state_changed', from_state=previous,
                        to_state=state, reason_code=reason if state == final else '', actor='main-metric')
        row['ts'] = stamp_metric(offset)
        event_rows.append(row)
        previous, seq = state, seq + 1

# More than the public 1000 attempt window: bounded tail projection retains the
# newest attempts and excludes old history without projecting the whole ledger.
for number in range(1005):
    add_attempt('old-%03d' % number, 'old-task-%03d' % number, 'worker', 'write', -1000-number*4,
                -999-number*4, 'done')
# Same-task continuation may have a different owning main attempt. Its recent
# timeout and successor belong to --since even though the task started earlier.
add_attempt('write-one', 'continued', 'worker', 'write', -600, -50, 'timed_out')
add_attempt('write-two', 'continued', 'worker', 'write', -45, -40, 'done', parent='main-later')
add_attempt('review-two', 'continued', 'reviewer', 'read', -35, -30, 'done', parent='main-later')
add_attempt('write-recent', 'recent', 'worker', 'write', -55, -40, 'done')
add_attempt('review-recent', 'recent', 'reviewer', 'read', -35, -30, 'done')
add_attempt('write-abandoned', 'abandoned', 'worker', 'write', -25, -15, 'timed_out')
add_attempt('clear-call', 'recent', 'worker', 'write', -35, -30, 'cancelled', 'manual_clear')
add_attempt('write-followup', 'followup', 'worker', 'write', -45, -40, 'done')
add_attempt('delegate-done', 'metric-write-done', 'worker', 'write', -12, -10, 'done', tool='peer-delegate')
add_attempt('delegate-failed', 'metric-write-failed', 'worker', 'write', -11, -9, 'failed',
            'verification_failed', extra_refs={'provider_error': 'quota', 'provider_error_exit': 125},
            tool='peer-delegate')
add_attempt('delegate-blocked', 'metric-write-blocked', 'worker', 'write', -7, -6, 'failed',
            'provider_failed', tool='peer-delegate')
add_attempt('delegate-typed-verify', 'metric-write-typed-verify', 'worker', 'write', -5, -4,
            'failed', tool='peer-delegate')
add_attempt('delegate-provider-verify', 'metric-write-provider-verify', 'worker', 'write', -5, -4,
            'failed', tool='peer-delegate')
add_attempt('delegate-review', 'metric-write-review', 'worker', 'write', -8, -7, 'review', '', tool='peer-delegate')
add_attempt('delegate-other', 'metric-write-other', 'worker', 'write', -6, -5, 'queued', '', tool='peer-delegate')
lifecycle = metric_repo / '.oms/lifecycle/events.jsonl'
lifecycle.parent.mkdir(parents=True)
lifecycle.write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in event_rows), encoding='utf-8')

metric_result_path = metric_repo / '.oms/artifacts/panel-results/failure.json'
metric_result_path.parent.mkdir(parents=True)
metric_result_path.write_text(json.dumps({'schema': 1, 'kind': 'oms-panel-result', 'outcome': 'failed'}), encoding='utf-8')
artifact_rows = []
for event_id, task_id, attempt_id, offset in (
        ('evt_review_two', 'continued', 'review-two', -20),
        ('evt_review_recent', 'recent', 'review-recent', -20)):
    artifact_rows.append({'schema': 1, 'event_id': event_id, 'operation_id': 'op_' + event_id,
                          'ts': stamp_metric(offset), 'kind': 'review-outcome', 'provider': 'local',
                          'exit': 0, 'task_id': task_id, 'attempt_id': attempt_id})
artifact_rows.append({'schema': 1, 'event_id': 'evt_panel_failure', 'operation_id': 'op_panel_failure',
                      'ts': stamp_metric(-10), 'kind': 'panel-result', 'provider': 'local', 'exit': 1,
                      'task_id': 'abandoned', 'artifact': '.oms/artifacts/panel-results/failure.json',
                      'artifact_sha256': hashlib.sha256(metric_result_path.read_bytes()).hexdigest()})
artifact_rows.extend([
    {'schema': 1, 'event_id': 'evt_delegate_failed', 'operation_id': 'op_delegate_failed',
     'ts': stamp_metric(-8), 'kind': 'delegate', 'exit': 1, 'verify_exit': 1,
     'reason_code': 'verification_failed', 'attempt_id': 'delegate-failed'},
    {'schema': 1, 'event_id': 'evt_delegate_blocked', 'operation_id': 'op_delegate_blocked',
     'ts': stamp_metric(-6), 'kind': 'delegate', 'exit': 1, 'verify_exit': 125,
     'reason_code': 'provider_failed', 'attempt_id': 'delegate-blocked'},
    {'schema': 1, 'event_id': 'evt_delegate_typed_verify', 'operation_id': 'op_delegate_typed_verify',
     'ts': stamp_metric(-4), 'kind': 'delegate', 'reason_code': 'verification_failed',
     'attempt_id': 'delegate-typed-verify'},
    {'schema': 1, 'event_id': 'evt_delegate_provider_verify', 'operation_id': 'op_delegate_provider_verify',
     'ts': stamp_metric(-4), 'kind': 'delegate', 'exit': 1, 'verify_exit': 1, 'provider_error_exit': 1,
     'attempt_id': 'delegate-provider-verify'},
    {'schema': 1, 'event_id': 'evt_delegate_review', 'operation_id': 'op_delegate_review',
     'ts': stamp_metric(-7), 'kind': 'delegate', 'exit': 0, 'attempt_id': 'delegate-review'},
])
index_path = metric_repo / '.oms/artifacts/index.jsonl'
index_path.write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in artifact_rows), encoding='utf-8')
expected_metrics = panel_metrics.aggregate(metric_repo, now=now_metric)
assert expected_metrics['completion'] == {'finished': 2, 'started': 6, 'rate': 0.3333}, expected_metrics
assert expected_metrics['review_wait_seconds'] == {'count': 2, 'median': 20.0, 'p90': 20.0}, expected_metrics
assert expected_metrics['rework'] == {'tasks': 1, 'started': 6, 'rate': 0.1667}, expected_metrics
assert expected_metrics['abandoned_calls'] == 1, expected_metrics
assert expected_metrics['endings'] == {'timed_out': 2, 'cancelled': 1,
                                       'verify_failed': 1, 'failed': 3}, expected_metrics
write_metrics = expected_metrics['write_attempts']
assert write_metrics['total'] == 7 and write_metrics['reconciles'], write_metrics
assert write_metrics['state_counts'] == {'done': 1, 'failed': 4, 'review': 1, 'queued': 1}, write_metrics
assert write_metrics['states']['other_by_state'] == {'queued': 1}, write_metrics
assert write_metrics['failed_and_undecided']['reason_code'] == {'verification_failed': 1, 'provider_failed': 1, 'unknown': 3}, write_metrics
assert write_metrics['failed_and_undecided']['first_failing_stage'] == {'verify': 2, 'implement': 2, 'unknown': 1}, write_metrics
assert write_metrics['provider_failed_categories']['quota'] == 1, write_metrics
# provider_failed without metadata and a provider failure followed by verify are both counted
assert write_metrics['provider_failed_categories']['unknown'] == 2, write_metrics
assert write_metrics['telemetry']['graph_context'] == 'not_recorded', write_metrics
assert write_metrics['telemetry']['claude_fork_subagents'] == 'not_recorded', write_metrics
assert expected_metrics['window']['until'] == expected_metrics['until'], expected_metrics
assert expected_metrics['metric_scope']['completion']['coverage']['attempt_limit'] == 1000, expected_metrics
assert expected_metrics['pending']['awaiting_review']['count'] == 1, expected_metrics
assert expected_metrics['pending']['awaiting_review']['oldest_seconds'] == 80.0, expected_metrics
assert expected_metrics['pending']['awaiting_admission']['count'] >= 0, expected_metrics
assert expected_metrics['preflight']['tasks'] == 1, expected_metrics
assert expected_metrics['interventions'] == {'manual_clears': 1, 'cancels': 1,
                                              'owner_finalized_failures': 1, 'total': 3}, expected_metrics
metric_text = io.StringIO()
with redirect_stdout(metric_text):
    assert panel.main(['--repo', str(metric_repo), '--metrics']) == 0
assert 'completion: 2/6 (33.3%)' in metric_text.getvalue(), metric_text.getvalue()
assert 'awaiting review: n=' in metric_text.getvalue(), metric_text.getvalue()
assert 'coverage:' in metric_text.getvalue() and 'until ' in metric_text.getvalue(), metric_text.getvalue()
assert 'peer-delegate write attempts: 7' in metric_text.getvalue(), metric_text.getvalue()
assert 'refs.context_pack_sha256' in metric_text.getvalue(), metric_text.getvalue()
metric_json = io.StringIO()
with redirect_stdout(metric_json):
    assert panel.main(['--repo', str(metric_repo), '--metrics', '--json']) == 0
assert json.loads(metric_json.getvalue())['completion'] == expected_metrics['completion']
metric_payload = json.loads(metric_json.getvalue())
assert metric_payload['write_attempts'] == write_metrics
assert metric_payload['window']['since'] is None
assert metric_payload['window']['until'] == metric_payload['until']
metric_help = io.StringIO()
try:
    with redirect_stdout(metric_help):
        panel.main(['--help'])
except SystemExit as error:
    assert error.code == 0
assert 'follow-up task IDs have no producer linkage' in ' '.join(metric_help.getvalue().split()), metric_help.getvalue()
recent_metrics = panel_metrics.aggregate(metric_repo, since='1m', now=now_metric)
assert recent_metrics['write_attempts']['total'] == 7, recent_metrics
outside_window_metrics = panel_metrics.aggregate(metric_repo, since='1s', now=now_metric)
assert outside_window_metrics['write_attempts']['total'] == 0, outside_window_metrics
assert recent_metrics['completion'] == {'finished': 1, 'started': 3, 'rate': 0.3333}, recent_metrics
assert recent_metrics['review_wait_seconds'] == {'count': 2, 'median': 20.0, 'p90': 20.0}, recent_metrics
assert recent_metrics['rework'] == {'tasks': 1, 'started': 3, 'rate': 0.3333}, recent_metrics
assert recent_metrics['abandoned_calls'] == 1, recent_metrics
assert recent_metrics['interventions'] == {'manual_clears': 1, 'cancels': 0,
                                            'owner_finalized_failures': 1, 'total': 2}, recent_metrics
for invalid_since in ('', '0s', '-1h', '9' * 400 + 'd', 'garbage'):
    error_text = io.StringIO()
    if invalid_since == '':
        try:
            panel_metrics.aggregate(metric_repo, since=invalid_since, now=now_metric)
        except ValueError as error:
            error_text.write(str(error))
        else:
            raise AssertionError('invalid --since value was accepted: %r' % invalid_since)
    else:
        with redirect_stderr(error_text):
            assert panel.main(['--repo', str(metric_repo), '--metrics', '--since=' + invalid_since]) == 1
    assert '--since must be a positive finite duration' in error_text.getvalue(), error_text.getvalue()

# Ledger order breaks timestamp ties: the timeout is followed by a successful
# write continuation in the same recorded second.
plan_path = metric_repo / '.oms/plan/tasks.json'
metric_plan = json.loads(plan_path.read_text(encoding='utf-8'))
metric_plan['tasks']['same-second'] = {'id': 'same-second', 'state': 'running', 'lease_epoch': 1}
plan_path.write_text(json.dumps(metric_plan), encoding='utf-8')
add_attempt('same-second-timeout', 'same-second', 'worker', 'write', -10, -5, 'timed_out')
add_attempt('same-second-success', 'same-second', 'worker', 'write', -5, -3, 'done')
lifecycle.write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in event_rows), encoding='utf-8')
same_second_metrics = panel_metrics.aggregate(metric_repo, now=now_metric)
assert same_second_metrics['abandoned_calls'] == 1, same_second_metrics
assert same_second_metrics['rework']['tasks'] == 2, same_second_metrics

# Existing but corrupt plan evidence is incomplete and exposes a concise reason
# through both output formats.
plan_path.write_text('{broken json', encoding='utf-8')
corrupt_metrics = panel_metrics.aggregate(metric_repo, now=now_metric)
assert corrupt_metrics['incomplete'] is True, corrupt_metrics
assert corrupt_metrics['incomplete_reason'] == 'plan evidence malformed', corrupt_metrics
corrupt_text = io.StringIO()
with redirect_stdout(corrupt_text):
    assert panel.main(['--repo', str(metric_repo), '--metrics']) == 0
assert 'incomplete: plan evidence malformed' in corrupt_text.getvalue(), corrupt_text.getvalue()
corrupt_json = io.StringIO()
with redirect_stdout(corrupt_json):
    assert panel.main(['--repo', str(metric_repo), '--metrics', '--json']) == 0
corrupt_payload = json.loads(corrupt_json.getvalue())
assert corrupt_payload['incomplete'] is True
assert corrupt_payload['incomplete_reason'] == 'plan evidence malformed'

# Invalid task state values are excluded from counts and reported as partial
# evidence in both output formats instead of being interpreted as ready.
for invalid_state in (None, 7, False, 'unknown'):
    plan_path.write_text(json.dumps({'tasks': {
        'valid': {'id': 'valid', 'state': 'running', 'lease_epoch': 1},
        'invalid': {'id': 'invalid', 'state': invalid_state, 'lease_epoch': 1}}}), encoding='utf-8')
    bad_state_metrics = panel_metrics.aggregate(metric_repo, now=now_metric)
    assert bad_state_metrics['incomplete'] is True, bad_state_metrics
    assert 'plan task state malformed' in bad_state_metrics['incomplete_reason'], bad_state_metrics
    assert bad_state_metrics['completion']['started'] == 1, bad_state_metrics
bad_state_text = io.StringIO()
with redirect_stdout(bad_state_text):
    assert panel.main(['--repo', str(metric_repo), '--metrics']) == 0
assert 'incomplete: plan task state malformed' in bad_state_text.getvalue(), bad_state_text.getvalue()
bad_state_json = io.StringIO()
with redirect_stdout(bad_state_json):
    assert panel.main(['--repo', str(metric_repo), '--metrics', '--json']) == 0
bad_state_payload = json.loads(bad_state_json.getvalue())
assert bad_state_payload['incomplete'] is True
assert 'plan task state malformed' in bad_state_payload['incomplete_reason']

empty_metrics_repo = project / 'panel-metrics-empty-fixture'
empty_metrics_repo.mkdir()
subprocess.run(['git', 'init', '-q', str(empty_metrics_repo)], check=True)
(empty_metrics_repo / '.oms/plan').mkdir(parents=True)
(empty_metrics_repo / '.oms/threads').mkdir()
empty_metrics = panel_metrics.aggregate(empty_metrics_repo)
assert empty_metrics['incomplete'] is False and empty_metrics['incomplete_reason'] is None, empty_metrics
assert empty_metrics['completion'] == {'finished': 0, 'started': 0, 'rate': None}
assert empty_metrics['review_wait_seconds'] == {'count': 0, 'median': None, 'p90': None}
assert empty_metrics['rework']['rate'] is None and empty_metrics['abandoned_calls'] == 0
assert empty_metrics['interventions']['total'] == 0
empty_text = io.StringIO()
with redirect_stdout(empty_text):
    assert panel.main(['--repo', str(empty_metrics_repo), '--metrics']) == 0
assert 'completion: 0/0 (none)' in empty_text.getvalue(), empty_text.getvalue()
empty_json = io.StringIO()
with redirect_stdout(empty_json):
    assert panel.main(['--repo', str(empty_metrics_repo), '--metrics', '--json']) == 0
assert json.loads(empty_json.getvalue())['review_wait_seconds']['p90'] is None
# Add the shared ending fixture only after legacy metric assertions, so it
# verifies the vocabulary without changing their established input snapshot.
event_rows = []
add_attempt('ending-guard', 'ending-guard', 'worker', 'write', -12, -11, 'failed',
            'worker_authority_violation')
add_attempt('ending-cancel', 'ending-cancel', 'worker', 'write', -10, -9, 'cancelled', 'cancelled_by_owner')
add_attempt('ending-timeout', 'ending-timeout', 'worker', 'write', -8, -7, 'timed_out')
add_attempt('ending-failure', 'ending-failure', 'worker', 'write', -6, -5, 'failed')
lifecycle.write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in event_rows), encoding='utf-8')
ending_metrics = panel_metrics.aggregate(metric_repo, now=now_metric)
assert ending_metrics['endings'] == {'guard_stop': 1, 'cancelled': 1, 'timed_out': 1, 'failed': 1}, ending_metrics
from call_ending import call_ending
attention_ending_counts = {}
for state, reason in (('failed', 'worker_authority_violation'), ('cancelled', 'cancelled_by_owner'),
                      ('timed_out', ''), ('failed', '')):
    ending = call_ending({'state': state, 'reason_code': reason})
    attention_ending_counts[ending] = attention_ending_counts.get(ending, 0) + 1
assert {key: ending_metrics['endings'].get(key, 0) for key in attention_ending_counts} == attention_ending_counts
shutil.rmtree(metric_repo)
shutil.rmtree(empty_metrics_repo)

task_ids = ['tick-write', 'tick-wait']
plan.write_text(json.dumps({'plan_id': 'tick-plan', 'tasks': [{'id': t} for t in task_ids]}))
tick_room = {'id': 'tick-room', 'participants': [
    {'participant': 'tick-main', 'role': 'main', 'provider': 'codex', 'joined': True},
    {'participant': 'tick-worker', 'role': 'worker', 'provider': 'codex', 'joined': True, 'parent': 'tick-main'}]}
main = {'attempt_id': 'tick-main', 'terminal': False, 'state': 'working', 'tool': 'panel-main', 'provider': 'codex',
        'refs': {'panel_room_id': 'tick-room', 'panel_room_participant': 'tick-main', 'panel_role': 'main',
                 'panel_host': 'herdr', 'panel_socket_digest': 'fixture',
                 'panel_pane_id': 'tab:p1', 'panel_terminal_id': 'terminal'}}
write = {'attempt_id': 'tick-worker', 'task_id': 'tick-write', 'terminal': True, 'state': 'done',
         'refs': {'panel_room_id': 'tick-room', 'panel_room_participant': 'tick-worker', 'panel_role': 'worker'}}
listed = [main] + [{'attempt_id': 'old-%s' % i, 'terminal': True} for i in range(300)] + [write]
artifacts = [{'kind': 'delegate', 'task_id': 'tick-write', 'attempt_id': 'tick-worker', 'patch': 'change.patch'},
             {'kind': 'panel-result', 'task_id': 'tick-wait'}]
(project / '.oms/artifacts/index.jsonl').write_text(
    ''.join(json.dumps(row) + '\n' for row in artifacts), encoding='utf-8')
tick_state = {'repo': {'name': 'fixture'}, 'plan': {'present': True}, 'attempts': {'recent': [
    dict(write, panel={'room_id': 'tick-room', 'room_participant': 'tick-worker', 'role': 'worker', 'access': 'write'}),
    {'task_id': 'tick-wait', 'state': 'failed'}]}}
counts = {'events': 0, 'index': 0}
failure = [False]
run_path = runpy.run_path
tick_store = run_path(str(panel_results.ROOT / 'scripts/lib/artifact-index-store.py'))
def tick_index(repo, index, **kwargs):
    counts['index'] += 1
    body = tick_store['read_index'](repo, index, **kwargs)
    plan.write_text(plan.read_text() + '\n')
    return body
def tick_module(path, *args, **kwargs):
    # Observe the strict retained-index reader, including its real validation.
    if Path(path).name == 'artifact-index-store.py':
        return dict(tick_store, read_index=tick_index)
    return run_path(path, *args, **kwargs)
def tick_run(command, **kwargs):
    value = {}
    if 'agent-events' in command:
        counts['events'] += 1
        assert command[-2:] == ['list', '--json'], command
        if failure[0]:
            raise subprocess.TimeoutExpired(command, 5)
        value = listed
        plan.write_text(plan.read_text() + '\n')
    elif 'agent-plan' in command:
        value = {'tasks': [{'id': t} for t in task_ids]}
    elif command[:2] == ['tmux', 'list-windows']:
        return subprocess.CompletedProcess(command, 0, f'@1\ttick-main\ttick-room\t%1\ttick-main\t{project.resolve()}\t$1\n', '')
    elif command[:2] == ['tmux', 'list-panes']:
        return subprocess.CompletedProcess(command, 0, '%1\t@1\n', '')
    return subprocess.CompletedProcess(command, 0, json.dumps(value), '')
def tick_result(repo, row):
    return {'outcome': 'accepted', 'revision': 'a' * 24, 'evidence': [],
            'plan_binding': panel_results._plan_bindings(repo, task_ids)['tick-wait'],
            'verification': {'status': 'passed', 'exit': 0}}
with patch.object(panel, 'dashboard', side_effect=lambda *a, **k: (deepcopy(tick_state), 0)), \
        patch.object(panel.room, 'status', side_effect=lambda *a: deepcopy(tick_room)), \
        patch.object(panel, 'managed_session', return_value=False), \
        patch.object(panel.subprocess, 'run', side_effect=tick_run), \
        patch.object(panel_results.runpy, 'run_path', side_effect=tick_module), \
        patch.object(panel_chats.shutil, 'which', return_value='/fixture/tmux'), \
        patch.object(panel_chats, 'current_models', return_value={}), \
        patch.object(panel_results, '_load_result', side_effect=tick_result), \
        patch.object(panel_results, 'completed_tasks', wraps=panel_results.completed_tasks) as completed, \
        patch.object(panel, 'outcomes', wraps=panel.outcomes) as outcomes, \
        patch.object(panel_results, 'pending_admissions', wraps=panel_results.pending_admissions) as pending, \
        patch.object(panel_chats, 'windows', wraps=panel_chats.windows) as windows, \
        patch.dict(os.environ, {'OMS_ROOM_ID': 'tick-room', 'TMUX': 'fixture'}):
    for read in (lambda: panel.snapshot(project, 'tick-room')[0],
                 lambda: panel.read_panel(project, 'codex', None, False)[0]):
        counts.update(events=0, index=0)
        report = read()
        assert counts == {'events': 1, 'index': 1}, counts
        if 'dead_mains' in report:
            assert [row['attempt'] for row in report['dead_mains']['unverifiable']] == ['tick-main']
        assert report['completed_tasks'].keys() == {'tick-wait'}, report
        assert report['finalized'] == {'tick-wait': 'accepted'}, report
        assert report['awaiting_admission'] == ['tick-worker'], report
        assert report['room_attempts']['tick-worker']['state'] == 'done', report
    assert completed.call_count == outcomes.call_count == pending.call_count == 2
    tick = windows.call_args.kwargs['_tick']
    assert windows(project, 'tick-room', tick_room, _tick=tick) == {
        'tick-main': [{'window': '@1', 'pane': '%1', 'session': '$1'}]}
    assert tick.records()[0] == listed[-300:], 'results lost their latest-300 coverage'
    # The Herdr H reader uses the same full rows, including the old active main.
    with patch.object(panel_host, 'herdr_active', return_value=True), \
            patch.object(panel_host, 'socket_digest', return_value='fixture'), \
            patch.object(panel_host, 'pane', return_value={
                'pane_id': 'tab:p1', 'terminal_id': 'terminal', 'workspace_id': 'workspace',
                'tab_id': 'tab', 'cwd': str(project.resolve())}):
        assert panel_host.targets(project, 'tick-room', tick_room, _tick=tick).get('tick-main')
    assert counts['events'] == 1
    # Action-time navigation has no tick: it still probes --active for itself.
    def action_run(command, **kwargs):
        if 'agent-events' in command:
            return subprocess.CompletedProcess(command, 0, json.dumps([main]), '')
        return tick_run(command, **kwargs)
    with patch.object(panel_chats.subprocess, 'run', side_effect=action_run) as action:
        assert panel_chats.windows(project, 'tick-room', tick_room).get('tick-main')
        assert any('--active' in c.args[0] for c in action.call_args_list)
    # A failed common read stays unknown for every consumer and is retried next tick.
    failure[0] = True
    for i in range(2):
        plan.write_text(plan.read_text() + '\n')
        counts.update(events=0, index=0)
        report = panel.read_panel(project, 'codex', None, False)[0]
        assert counts['events'] == 1 and counts['index'] == 0, counts
        assert report['completed_tasks'] == {} and 'room_attempts' not in report, report
        assert report['awaiting_admission'] == [], report

# An owner decision older than results()' twenty-task window still reaches the board through outcomes().
many = [{'kind': 'panel-result', 'task_id': 'task-%02d' % n} for n in range(30)]
with patch.object(panel_results, '_records', return_value=([], many)), \
        patch.object(panel_results, '_load_result', side_effect=lambda repo, row: {'outcome': 'completed'}):
    assert panel_results.outcomes(project, {'task-00', 'task-29', 'missing', '../bad'}) == {'task-00': 'completed', 'task-29': 'completed'}
    assert panel_results.outcomes(project, set()) == {}

# Recorded owner decisions are scoped to their plan task generation when bound.
current_binding = {'plan_id': 'replacement-plan', 'task_sha256': 'current-task'}
outcome_rows = [
    {'kind': 'panel-result', 'task_id': 'reused-task'},
    {'kind': 'panel-result', 'task_id': 'same-task'},
    {'kind': 'panel-result', 'task_id': 'legacy-task'},
    {'kind': 'panel-result', 'task_id': 'retired-task'},
]
result_payloads = {
    'reused-task': {'outcome': 'rejected', 'plan_binding': {'plan_id': 'retired-plan', 'task_sha256': 'old-task'}},
    'same-task': {'outcome': 'rejected', 'plan_binding': current_binding},
    'legacy-task': {'outcome': 'rejected'},
    'retired-task': {'outcome': 'completed', 'plan_binding': {'plan_id': 'retired-plan', 'task_sha256': 'gone'}},
}
with patch.object(panel_results, '_records', return_value=([], outcome_rows)), \
        patch.object(panel_results, '_plan_bindings', return_value={
            'reused-task': current_binding, 'same-task': current_binding}), \
        patch.object(panel_results, '_load_result', side_effect=lambda repo, row: result_payloads[row['task_id']]):
    assert panel_results.outcomes(project, {'reused-task', 'same-task', 'legacy-task', 'retired-task'}) == {
        'same-task': 'rejected', 'legacy-task': 'rejected', 'retired-task': 'completed'}

def changing():
    plan.write_text('changed while collecting', encoding='utf-8')
    return {'number': 42}
# A read that overlapped a write is this board's answer, but it is not published for the others.
assert cache.read_shared(project, 'race', changing) == {'number': 42}
assert not (project / cache.DIRECTORY / 'race.json').exists()
# Mains' activity heartbeats change all the time and are read directly; they do not invalidate shared reads.
before = cache.fingerprint(project)
(project / '.oms/hooks/panel-activity').mkdir(parents=True, exist_ok=True)
(project / '.oms/hooks/panel-activity/main.json').write_text('{}', encoding='utf-8')
assert cache.fingerprint(project) == before
# Watch loop and control window keep the room board when a later read fails.
good = ({'repo': {'name': 'fixture'}, 'room': {'id': 'one', 'participants': []}}, 0)
bad = ({'repo': {'name': 'fixture'}, 'collection': {'ok': False}}, 1)
held = {}
assert panel.hold_state(held, *good, 'one') == (good[0], 0, None)
held['at'] -= 12
state, status, notice = panel.hold_state(held, *bad, 'one')
assert state is good[0] and status == 0 and notice.startswith('State refresh failed; showing data from 1') and notice.endswith('s ago'), notice
panel.hold_state(held, *bad, 'one')
assert 'keeps failing (3 in a row)' in panel.hold_state(held, *bad, 'one')[2]
assert panel.hold_state(held, *bad, 'two') == (bad[0], 1, None), 'another room drew the held board'
assert panel.hold_state({}, *bad, 'one') == (bad[0], 1, None), 'a board that never read kept today fallback'
# A failed first read or room switch says the room is unknown, never an empty dashboard.
from panel_view import render as draw
from dashboard_projection import display_width
healthy = {'repo': {'name': 'fixture'}, 'collection': {'ok': True}, 'room': {'id': 'one', 'participants': []}}
for view in ('auto', 'graph', 'tree'):
    for menu in (False, True):
        for width, height in ((100, 28), (38, 40), (24, 18), (12, 10), (2, 8)):
            stale = {'selected': ('chat', 'old'), 'primary': ('chat', 'old'), 'preview': {'target': ('result', 'r')},
                     'surface': 'graph', 'hits': [{'y': 1, 'action': ('chat', 'old')}], 'items': [('chat', 'old')],
                     'selected_row': 3, 'row_ids': ['old'], 'detail': 'old', 'room_id': 'one'}
            drawn = draw(bad[0], 'codex', width, height, menu=menu, view=view, navigation=stale)
            assert len(drawn.splitlines()) <= height - int(menu) and all(display_width(line) <= width for line in drawn.splitlines()), drawn
            assert any(word in drawn for word in ('unavailable', 'unknown', 'Unknown')) or width < 12 or height - int(menu) < 2, (view, menu, width, height, drawn)
            assert not any(word in drawn for word in ('Collection degraded', 'REPO ACCEPT', 'No active', 'Goal:', 'Joined mains', 'Messages:', 'Nothing needs you')), drawn
            assert not any(stale.get(key) for key in ('selected', 'primary', 'preview', 'surface', 'hits', 'items', 'selected_row', 'row_ids', 'detail')), stale
            assert stale['room_id'] == 'one'
            assert '[1] Claude' not in drawn and 'Press 1' not in drawn, drawn
assert 'o selects a room, 9 retries' in draw(bad[0], 'codex', 100, 28, menu=True)
with patch.dict(os.environ, {}, clear=True):
    assert '\033' in draw(bad[0], 'codex', 100, 28, color=True)
assert 'unavailable' not in draw(healthy, 'codex', 100, 28, menu=True).split('Actions')[0]
assert 'Collection degraded' in draw(dict(bad[0], room={'id': 'one', 'participants': [], 'error': 'x'}), 'codex', 100, 28)
assert bad[0] == {'repo': {'name': 'fixture'}, 'collection': {'ok': False}}
# The full reader must yield to an unknown-room screen on first failure.
with patch.object(panel, 'terminal_style', return_value=(False, False, False)), patch.object(panel, 'managed_session', return_value=False), patch.object(panel.os, 'get_terminal_size', return_value=os.terminal_size((100, 28))):
    reader_nav = {'detail': 'old-result', 'selected_row': 'old-row', 'row_ids': ['old-row']}
    unknown_text, unused, capable = panel.frame_view(project, 'codex', bad[0], navigation=reader_nav, view='tree')
    assert 'Room state unavailable' in unknown_text and not reader_nav.get('detail'), (unknown_text, reader_nav)

assert 'Room state unavailable' not in draw(good[0], 'codex', 100, 28, navigation={'selected': ('chat', 'old')})
board, held_nav = {}, {}
panel.hold_state(board, *good, 'one')
shown, unused, hint = panel.hold_state(board, *bad, 'two')
assert shown is bad[0] and hint is None and 'Room state unavailable' in draw(shown, 'codex', 100, 28, navigation=held_nav)
assert panel.hold_state(board, *good, 'two')[0] is good[0] and 'Room state unavailable' not in draw(good[0], 'codex', 100, 28, navigation=held_nav)
navigation = {}
panel.stale_notice(navigation, notice)
assert navigation['notice'] == notice
panel.stale_notice(navigation, None)
assert 'notice' not in navigation
with patch.object(panel, 'snapshot', side_effect=[good, bad]), patch.object(panel, 'frame_view', return_value=('', os.terminal_size((80, 24)), False)) as frame:
    shown, navigation = {}, {}
    for unused in range(2):
        shown.pop('state', None)
        panel.show(project, 'codex', navigation=navigation, cache=shown)
    assert frame.call_args[0][2] is good[0] and navigation['notice'].startswith('State refresh failed'), 'control window dropped the room'
for relative in ('.oms/lifecycle/events.jsonl', '.oms/hooks/events.jsonl', '.oms/threads/room.jsonl'):
    source = project / relative
    source.parent.mkdir(exist_ok=True)
    before = cache.fingerprint(project)
    source.write_text('record', encoding='utf-8')
    assert cache.fingerprint(project) != before, relative
git_dir = project / 'worktree-git'
git_dir.mkdir()
(project / '.git').write_text('gitdir: worktree-git\n', encoding='utf-8')
before = cache.fingerprint(project)
(git_dir / 'HEAD').write_text('ref: refs/heads/fixture\n', encoding='utf-8')
assert cache.fingerprint(project) != before, 'worktree HEAD was omitted'
PY
