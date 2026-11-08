# Salvage captures raw checkout bytes with a private index/config and owner-only admission.
import contextlib
import panel_dispatch
import panel_salvage
import terminal_panel
from room_repository import harness_residue_parent

# BEGIN salvage-continuation
for residue, child in (('oh-my-setting-delegate.fixture', 'wt'),
                       ('oh-my-setting-admit.fixture', 'wt'),
                       ('oh-my-setting-scratch.fixture', 'wt'),
                       ('oh-my-setting-land.fixture', 'wt'),
                       ('oh-my-setting-agy-read.fixture', 'tree')):
    assert harness_residue_parent(temporary / residue / child) == temporary / residue
    assert harness_residue_parent(temporary / residue / (child + '-fixture')) == temporary / residue
assert harness_residue_parent(temporary / 'oh-my-setting-delegate.other' / 'wt-fixture') is None
assert harness_residue_parent(temporary / 'oh-my-setting-agy-read.fixture' / 'tree') == \
       temporary / 'oh-my-setting-agy-read.fixture'
assert harness_residue_parent(temporary / 'oh-my-setting-agy-read.fixture' / 'tree-other') is None
salvage_repo = temporary / 'salvage-repo'
salvage_execution_repo = temporary / 'salvage-execution-repo'
salvage_registry = temporary / 'salvage-registry'
salvage_tree = salvage_registry / 'oh-my-setting-delegate.fixture' / 'wt'
salvage_tree.parent.mkdir(parents=True)
subprocess.run(['git', 'init', '-q', str(salvage_repo)], check=True)
for key, value in (('user.email', 'test@example.com'), ('user.name', 'test')):
    subprocess.run(['git', '-C', str(salvage_repo), 'config', key, value], check=True)
(salvage_repo / 'unicode.txt').write_text('label: 한글 ─ café\n', encoding='utf-8')
(salvage_repo / 'deleted.txt').write_text('delete me\n')
(salvage_repo / 'binary.dat').write_bytes(b'\0old\xff')
(salvage_repo / '.gitattributes').write_text('*.txt filter=trap working-tree-encoding=UTF-8\n')
subprocess.run(['git', '-C', str(salvage_repo), 'add', '.'], check=True)
subprocess.run(['git', '-C', str(salvage_repo), 'commit', '-qm', 'base'], check=True)
subprocess.run(['git', 'clone', '-q', str(salvage_repo), str(salvage_execution_repo)], check=True)
subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'add', '--detach', str(salvage_tree)],
               check=True, stdout=subprocess.DEVNULL)
(salvage_tree.parent / '.oh-my-setting-tmp').write_text(
    'kind=oh-my-setting-temp\ntemporary=1\npid=99999999\nrepo=%s\nworktree=%s\n' %
    (salvage_execution_repo, salvage_tree))
(salvage_tree / 'unicode.txt').write_text('label:   cafe\n', encoding='utf-8')
(salvage_tree / 'deleted.txt').unlink()
(salvage_tree / 'binary.dat').write_bytes(b'\0new\xfe')
(salvage_tree / 'new.txt').write_bytes('새 파일\r\n'.encode('utf-8'))
(salvage_tree / '.oms-room-relay.ABC123').mkdir()
(salvage_tree / '.oms-room-relay.ABC123' / '.ready').write_bytes(b'')
trap_file = temporary / 'salvage-git-executed'
trap_command = 'touch ' + shlex.quote(str(trap_file))
for key in ('filter.trap.clean', 'filter.trap.smudge', 'diff.external', 'core.fsmonitor'):
    subprocess.run(['git', '-C', str(salvage_execution_repo), 'config', key, trap_command], check=True)
with patch.dict(os.environ, {'MSYSTEM': 'fixture', 'OMS_DELEGATE_WORKTREE_ROOT': str(salvage_registry)}), \
        patch.object(panel_salvage.subprocess, 'run', side_effect=AssertionError('unexpected path conversion')):
    assert panel_salvage._worktree_root() == salvage_registry

index_path = panel_salvage._git_directory(salvage_tree) / 'index'
index_before = index_path.read_bytes()
metadata_before = {p: p.read_bytes() for p in salvage_execution_repo.joinpath('.git').rglob('*')
                   if p.is_file()}
salvage_attempt = {'task_id': 'salvage-task', 'attempt_id': 'att_fixture', 'tool': 'peer-delegate',
                   'created_at': '2026-10-14T00:00:00Z', 'terminal': True, 'state': 'failed',
                   'reason_code': 'worker_authority_violation',
                   'refs': {'panel_location': 'worktree:oh-my-setting-delegate.fixture/wt',
                            'panel_room_participant': 'call-0123456789abcdef', 'panel_room_id': 'room-fixture'}}
def salvage_query(repo, verb, *args):
    return [salvage_attempt] if verb == 'agent-events' else {'rows': []}
salvage_env = {'OMS_DELEGATE_WORKTREE_ROOT': str(salvage_registry),
               'OMS_HARNESS_CHILD': '0', 'OMS_HARNESS_DELEGATE_DEPTH': '0',
               'OMS_ROOM_ID': 'room-fixture', 'OMS_ROOM_REPO': str(salvage_repo)}
with patch.dict(os.environ, salvage_env), patch.object(panel_results, '_query', side_effect=salvage_query), \
        patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
            'call': {'participant': 'call-0123456789abcdef', 'room': 'room-fixture',
                     'task_id': 'salvage-task'},
            'execution_repo': str(salvage_execution_repo)}):
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task', '--json']) == 0
    room_salvaged = json.loads(output.getvalue())
    assert Path(room_salvaged['patch_path']).is_relative_to(salvage_repo)
    newer_attempt = dict(salvage_attempt, attempt_id='att_newer_fixture', created_at='2026-10-16T00:00:00Z', refs={})
    with patch.object(panel_results, '_query', side_effect=lambda repo, verb, *args:
                      [salvage_attempt, newer_attempt] if verb == 'agent-events' else {'rows': []}):
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task']) == 2
            assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task',
                               '--attempt', salvage_attempt['attempt_id']]) == 0
            assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task',
                               '--attempt', 'att_foreign']) == 2
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert panel.main(['--repo', str(salvage_execution_repo), '--salvage', 'salvage-task', '--json']) == 0
    salvaged = json.loads(output.getvalue())
    scratch_execution_repo = temporary / 'salvage-scratch-execution'
    scratch_tree = salvage_registry / 'oh-my-setting-delegate.scratch' / 'wt-scratch'
    scratch_tree.parent.mkdir(parents=True)
    subprocess.run(['git', '-C', str(salvage_repo), 'worktree', 'add', '--detach',
                    str(scratch_execution_repo)], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(['git', '-C', str(scratch_execution_repo), 'worktree', 'add', '--detach',
                    str(scratch_tree)], check=True, stdout=subprocess.DEVNULL)
    (scratch_tree.parent / '.oh-my-setting-tmp').write_text(
        'kind=oh-my-setting-temp\ntemporary=1\nrepo=%s\nworktree=%s\n' % (salvage_repo, scratch_tree))
    (scratch_tree / 'unicode.txt').write_text('scratch change\n')
    scratch_attempt = {'task_id': 'scratch-task', 'attempt_id': 'att_scratch_fixture',
                       'tool': 'peer-delegate', 'provider': 'claude', 'created_at': '2026-10-14T00:01:00Z',
                       'terminal': True, 'reason_code': 'worker_authority_violation',
                       'refs': {'panel_location': 'worktree:oh-my-setting-delegate.scratch/wt-scratch',
                                'panel_provider': 'codex',
                                'panel_room_participant': 'call-abcdef0123456789',
                                'panel_room_id': 'room-fixture'}}
    def scratch_query(repo, verb, *args):
        return [scratch_attempt] if verb == 'agent-events' else {'rows': []}
    with patch.object(panel_results, '_query', side_effect=scratch_query), \
            patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
                'call': {'participant': 'call-abcdef0123456789', 'room': 'room-fixture',
                         'task_id': 'scratch-task'},
                'execution_repo': str(scratch_execution_repo)}):
        scratch_salvaged = panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        assert Path(scratch_salvaged['patch_path']).is_relative_to(salvage_repo)
    # A stopped worker's indexed patch remains salvageable after EXIT cleanup removed its worktree.
    recovered_patch, unused_files, unused_warnings = panel_salvage._capture(
        scratch_tree, panel_salvage._common(panel_salvage._git_directory(scratch_tree)),
        panel_salvage._managed(scratch_execution_repo, scratch_tree)[2])
    scratch_log = '.oms/artifacts/delegate/scratch-stopped.md'
    scratch_patch = '.oms/artifacts/delegate/scratch.patch'
    (salvage_repo / scratch_log).parent.mkdir(parents=True, exist_ok=True)
    (salvage_repo / scratch_log).write_text('worktree kept at %s\n' % scratch_tree)
    (salvage_repo / scratch_patch).write_bytes(recovered_patch)
    scratch_record = {'kind': 'delegate', 'task_id': 'scratch-task',
                      'attempt_id': scratch_attempt['attempt_id'], 'exit': 124,
                      'artifact': scratch_log,
                      'artifact_sha256': hashlib.sha256((salvage_repo / scratch_log).read_bytes()).hexdigest(),
                      'patch': scratch_patch,
                      'patch_sha256': hashlib.sha256(recovered_patch).hexdigest(),
                      'base_sha': panel_salvage._managed(scratch_execution_repo, scratch_tree)[2][:12]}
    def recovered_query(repo, verb, *args):
        return [scratch_attempt] if verb == 'agent-events' else {'rows': [scratch_record]}
    scratch_git_pointer = scratch_tree / '.git'
    scratch_git_before = scratch_git_pointer.read_bytes()
    try:
        scratch_git_pointer.write_bytes(b'gitdir: /missing-salvage-registration\n')
        with patch.object(panel_results, '_query', side_effect=recovered_query), \
                patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
                    'call': {'participant': 'call-abcdef0123456789', 'room': 'room-fixture',
                             'task_id': 'scratch-task'}, 'execution_repo': str(scratch_execution_repo)}):
            try:
                panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
            except FileNotFoundError:
                pass
            else:
                raise AssertionError('missing registration fell back while the worktree still existed')
    finally:
        scratch_git_pointer.write_bytes(scratch_git_before)
    other_tree = temporary / 'elsewhere/oh-my-setting-delegate.scratch/wt-scratch'
    other_tree.parent.mkdir(parents=True)
    subprocess.run(['git', '-C', str(scratch_execution_repo), 'worktree', 'add', '--detach',
                    str(other_tree)], check=True, stdout=subprocess.DEVNULL)
    shutil.rmtree(scratch_tree.parent)
    saved_patch_fields = {key: scratch_record.pop(key) for key in ('patch', 'patch_sha256')}
    with patch.object(panel_results, '_query', side_effect=recovered_query), \
            patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
                'call': {'participant': 'call-abcdef0123456789', 'room': 'room-fixture',
                         'task_id': 'scratch-task'}, 'execution_repo': str(scratch_execution_repo)}):
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert 'registered worktree:' in str(error) and str(other_tree) in str(error), error
        else:
            raise AssertionError('registered worktree at a different path was treated as gone')
    scratch_record.update(saved_patch_fields)
    subprocess.run(['git', '-C', str(scratch_execution_repo), 'worktree', 'remove', '--force',
                    str(other_tree)], check=True)
    with patch.object(panel_results, '_query', side_effect=recovered_query), \
            patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
                'call': {'participant': 'call-abcdef0123456789', 'room': 'room-fixture',
                         'task_id': 'scratch-task'}, 'execution_repo': str(scratch_execution_repo)}):
        recovered = panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        assert recovered['salvaged_from']['recovered_from_artifact'] is True
        assert recovered['salvaged_from']['stop_reason'] == 'worker_authority_violation'
        assert recovered['recovered_artifact'] == scratch_patch
        assert all(row['before_bytes'] is None and row['after_bytes'] is None
                   for row in recovered['files'])
        saved_recovery = Path(recovered['patch_path'])
        panel_salvage.admission(saved_recovery, True, True, repo=salvage_repo)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            assert panel.main(['--repo', str(salvage_repo), '--salvage', 'scratch-task']) == 0
        assert 'recovered from indexed patch %s (worktree missing)' % scratch_patch in output.getvalue()
        assert ' -> ' not in output.getvalue()
        gone_attempt, gone_record = dict(scratch_attempt, refs=dict(scratch_attempt['refs'])), dict(scratch_record)
        provenance_files = {path: path.read_bytes()
                            for path in (salvage_repo / '.oms/salvage-provenance').glob('*.json')}
        original_indexed_patch = (salvage_repo / scratch_patch).read_bytes()
        (salvage_repo / scratch_patch).write_bytes(recovered_patch + b'tampered\n')
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert 'digest' in str(error), error
        else:
            raise AssertionError('tampered indexed patch was salvaged')
        finally:
            (salvage_repo / scratch_patch).write_bytes(original_indexed_patch)
        with patch.dict(scratch_record, {'base_sha': 'f' * 12}):
            try:
                panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
            except ValueError as error:
                assert 'recorded base' in str(error), error
            else:
                raise AssertionError('missing indexed patch base was accepted')
            assert {path: path.read_bytes() for path in provenance_files} == provenance_files
        # Empty-outcome checks use an attempt without the earlier recovery's provenance.
        scratch_attempt['attempt_id'] = 'att_empty_fixture'
        scratch_record['attempt_id'] = scratch_attempt['attempt_id']
        empty_record_path = salvage_repo / panel_salvage._empty_provenance_path(
            'scratch-task', scratch_attempt['attempt_id'])
        # The indexed row's exact conventional patch may be empty; that is not recovery evidence.
        scratch_patch = '.oms/artifacts/delegate/codex-scratch-task-20261014T000100Z-4321.patch'
        scratch_record['patch'] = scratch_patch
        (salvage_repo / scratch_patch).write_bytes(b'')
        scratch_record['patch_sha256'] = hashlib.sha256(b'').hexdigest()
        scratch_record.pop('patch')
        scratch_record.pop('patch_sha256')
        # An unindexed peer-delegate patch uses provider-task-timestamp-pid.patch.
        unindexed_patch = salvage_repo / '.oms/artifacts/delegate/codex-scratch-task-20261014T000100Z-4321.patch'
        unindexed_patch.write_bytes(recovered_patch)
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert str(unindexed_patch.relative_to(salvage_repo)) in str(error), error
        else:
            raise AssertionError('unindexed attempt patch was recorded as empty salvage')
        assert not empty_record_path.exists(), 'unindexed patch created an empty-salvage receipt'
        unindexed_patch.unlink()
        unindexed_patch.write_bytes(recovered_patch)
        sibling_index_row = {'task_id': 'scratch-task', 'attempt_id': 'att_prior_fixture',
                             'patch': str(unindexed_patch.relative_to(salvage_repo))}
        def sibling_index_query(repo, verb, *args):
            return {'rows': [sibling_index_row]} if verb == 'artifact-index' else [scratch_attempt]
        with patch.object(panel_results, '_query', side_effect=sibling_index_query):
            evidence = panel_salvage._attempt_recovery_evidence(
                salvage_repo, 'scratch-task', scratch_attempt['attempt_id'], attempt_row=scratch_attempt)
        assert str(unindexed_patch.relative_to(salvage_repo)) not in evidence, evidence
        unindexed_patch.unlink()
        unindexed_patch.write_bytes(b'')
        # A malformed provenance record tied to another task must not block this attempt.
        foreign_provenance = salvage_repo / '.oms/salvage-provenance' / ('a' * 64 + '.json')
        foreign_provenance.parent.mkdir(parents=True, exist_ok=True)
        foreign_provenance.write_text(json.dumps({
            'schema': 99, 'payload_sha256': 'a' * 64,
            'records': [{'salvaged_from': {'task': 'other-task', 'attempt': 'att_other'},
                         'encoding_warnings': []}]}))
        scratch_record['patch'] = scratch_patch
        scratch_record['patch_sha256'] = hashlib.sha256(b'').hexdigest()
        empty = panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        assert empty['outcome'] == 'nothing_to_recover' and empty['reason'] == 'indexed patch is empty', empty
        assert '--continue scratch-task' in empty['next_action'], empty
        empty_record_path = salvage_repo / panel_salvage._empty_provenance_path(
            'scratch-task', scratch_attempt['attempt_id'])
        empty_record = json.loads(empty_record_path.read_text())
        outcome = next(row['salvaged_from'] for row in empty_record['records']
                       if row['salvaged_from'].get('attempt') == scratch_attempt['attempt_id'])
        assert outcome['outcome'] == 'nothing_to_recover'
        assert outcome['task'] == 'scratch-task'
        # A delegate-located missing worktree with no indexed patch metadata is also empty salvage.
        scratch_record.pop('patch')
        scratch_record.pop('patch_sha256')
        empty = panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        assert empty['outcome'] == 'nothing_to_recover' and empty['reason'] == 'no kept worktree or indexed patch', empty
        for options in ([], ['--json']):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                assert panel.main(['--repo', str(salvage_repo), '--salvage', 'scratch-task', *options]) == 0
            if options:
                assert json.loads(output.getvalue()) == empty
            else:
                assert 'nothing to recover' in output.getvalue() and '--continue scratch-task' in output.getvalue()
        foreign_provenance.unlink()
        empty_record = json.loads(empty_record_path.read_text())
        outcomes = [row['salvaged_from'] for row in empty_record['records']
                    if row['salvaged_from'].get('attempt') == scratch_attempt['attempt_id']]
        assert any(row.get('reason', '').startswith('no kept worktree or indexed patch')
                   for row in outcomes), outcomes
        assert str(panel_salvage._worktree_root().resolve(strict=True)) in outcomes[-1]['reason']
        def assert_partial_patch_evidence(task_id, attempt_id, evidence):
            attempt = dict(scratch_attempt, task_id=task_id, attempt_id=attempt_id,
                           refs=dict(scratch_attempt['refs']))
            record = dict(scratch_record, task_id=task_id, attempt_id=attempt_id,
                          base_sha=scratch_record['base_sha'], **evidence)
            receipt = salvage_repo / panel_salvage._empty_provenance_path(task_id, attempt_id)
            with patch.object(panel_results, '_query', side_effect=lambda repo, verb, *args:
                              [attempt] if verb == 'agent-events' else {'rows': [record]}), \
                    patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
                        'call': {'participant': attempt['refs']['panel_room_participant'],
                                 'room': attempt['refs']['panel_room_id'], 'task_id': task_id,
                                 'attempt_id': attempt_id},
                        'execution_repo': str(scratch_execution_repo)}):
                try:
                    panel_salvage.salvage(salvage_repo, task_id, scratch_execution_repo)
                except ValueError:
                    pass
                else:
                    raise AssertionError('partial indexed patch evidence was recorded as empty salvage')
            assert not receipt.exists(), receipt
        assert_partial_patch_evidence('partial-digest-task', 'att_partial_digest',
                                      {'patch_sha256': '1' * 64})
        assert_partial_patch_evidence('partial-external-task', 'att_partial_external',
                                      {'patch_external': {'name': 'worker.patch', 'owned': False},
                                       'patch_sha256': '2' * 64})
        def timeout_agent_events(repo, verb, *args):
            if verb == 'artifact-index':
                return {'rows': []}
            raise TimeoutError('fixture timeout')
        with patch.object(panel_results, '_query', side_effect=timeout_agent_events):
            try:
                panel_salvage._attempt_recovery_evidence(salvage_repo, 'timeout-task', 'att_timeout')
            except ValueError as error:
                assert str(error) == 'cannot verify delegate recovery evidence; refusing empty salvage', error
            else:
                raise AssertionError('agent-events timeout did not refuse empty salvage cleanly')
        worktree_attempt = dict(scratch_attempt, attempt_id='att_timeout_worktree',
                                refs=dict(scratch_attempt['refs']))
        for listing_error in (subprocess.TimeoutExpired('git worktree list', 15), OSError('git unavailable')):
            with patch.object(panel_results, '_query', return_value={'rows': []}), \
                    patch.object(panel_salvage.subprocess, 'run', side_effect=listing_error):
                try:
                    panel_salvage._attempt_recovery_evidence(
                        salvage_repo, 'scratch-task', worktree_attempt['attempt_id'],
                        attempt_row=worktree_attempt, execution=scratch_execution_repo)
                except ValueError as error:
                    assert str(error) == ('cannot verify registered delegate worktrees; refusing empty salvage'), error
                else:
                    raise AssertionError('worktree listing failure did not refuse empty salvage cleanly')
        empty_input = temporary / 'admission-empty.patch'
        empty_input.write_bytes(b'')
        panel_salvage.admission(empty_input, repo=salvage_repo)
        assert not (salvage_repo / panel_salvage._provenance_path(
            hashlib.sha256(panel_salvage._payload(b'')).hexdigest())).exists()
        # A non-delegate worktree ref without artifact path evidence remains unlocatable and refused.
        original_log = (salvage_repo / scratch_log).read_bytes()
        original_location = scratch_attempt['refs']['panel_location']
        (salvage_repo / scratch_log).write_text('worker stopped without a kept path\n')
        scratch_record['artifact_sha256'] = hashlib.sha256((salvage_repo / scratch_log).read_bytes()).hexdigest()
        scratch_attempt['refs']['panel_location'] = 'worktree:outside-temp/work'
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert 'no recorded kept worktree path' in str(error), error
        else:
            raise AssertionError('unlocated non-delegate worktree was recorded as empty salvage')
        finally:
            (salvage_repo / scratch_log).write_bytes(original_log)
            scratch_record['artifact_sha256'] = hashlib.sha256(original_log).hexdigest()
            scratch_attempt['refs']['panel_location'] = original_location
        # A valid empty receipt resumes, but later provenance invalidates it.
        continuation_brief = temporary / 'empty-salvage-brief.md'
        continuation_brief.write_text('Continue safely.\n')
        parent = {'attempt_id': 'empty-parent', 'terminal': False, 'refs': {}}
        worker = dict(scratch_attempt, parent_attempt_id='empty-parent',
                      refs=dict(scratch_attempt['refs'], panel_access='write', panel_provider='codex',
                                native_session='fixture-session'))
        with patch.object(panel_salvage.panel_results, '_query', side_effect=lambda repo, verb, *args:
                          [scratch_attempt] if verb == 'agent-events' else {'rows': [scratch_record]}), \
                patch.object(terminal_panel, 'owning_attempt', return_value=[parent]), \
                patch.object(terminal_panel, 'events', return_value=json.dumps([parent, worker])), \
                patch.object(terminal_panel, '_rework_history', return_value=[worker]), \
                patch.object(terminal_panel, 'results', return_value={'rows': [{'calls': []}]}), \
                patch.object(terminal_panel, 'session_stored', return_value=True):
            materialized, resumed, note = panel_dispatch.continuation(
                salvage_repo, 'codex', 'main', 'scratch-task', {'provider': 'codex'}, continuation_brief,
                {'OMS_ROOM_REPO': str(salvage_repo)}, work_repo=salvage_repo, patch_reference={})
            assert resumed == 'fixture-session'
            assert 'no patch transferred' in note, note
            assert 'no patch to recover' in Path(materialized).read_text()
            Path(materialized).unlink()

            unreadable_provenance = salvage_repo / '.oms/salvage-provenance' / ('c' * 64 + '.json')
            unreadable_provenance.write_text(json.dumps({
                'schema': 99, 'payload_sha256': 'c' * 64,
                'records': [{'salvaged_from': {'task': 'scratch-task',
                                                'attempt': scratch_attempt['attempt_id']},
                             'encoding_warnings': []}]}))
            try:
                panel_dispatch.continuation(
                    salvage_repo, 'codex', 'main', 'scratch-task', {'provider': 'codex'}, continuation_brief,
                    {'OMS_ROOM_REPO': str(salvage_repo)}, work_repo=salvage_repo, patch_reference={})
            except ValueError as error:
                assert 'unreadable salvage provenance file:' in str(error), error
                assert str(unreadable_provenance.relative_to(salvage_repo)) in str(error), error
            else:
                raise AssertionError('continue ignored unreadable provenance tied to this attempt')
            unreadable_provenance.unlink()

            panel_salvage.record_provenance(salvage_repo, {
                'payload_sha256': hashlib.sha256(panel_salvage._payload(recovered_patch)).hexdigest(),
                'salvaged_from': {'task': 'scratch-task', 'attempt': scratch_attempt['attempt_id']},
                'encoding_warnings': [], 'base_sha': scratch_record['base_sha']})
            provenance_files = {path: path.read_bytes()
                                for path in (salvage_repo / '.oms/salvage-provenance').glob('*.json')}
            try:
                panel_dispatch.continuation(
                    salvage_repo, 'codex', 'main', 'scratch-task', {'provider': 'codex'}, continuation_brief,
                    {'OMS_ROOM_REPO': str(salvage_repo)}, work_repo=salvage_repo, patch_reference={})
            except ValueError as error:
                assert 'recoverable delegate evidence exists' in str(error), error
                assert '.oms/salvage-provenance/' in str(error), error
                assert ('inspect the recovery, then include recovered work in a fresh dispatch brief' in str(error)), error
                assert 'run oms panel --salvage' not in str(error), error
            else:
                raise AssertionError('patchless continue trusted an empty receipt after nonempty salvage')
        # An unrelated stopped write attempt has no empty-salvage receipt and remains refused.
        worker['attempt_id'] = 'att_unrecovered'
        worker['task_id'] = 'unrecovered-task'
        try:
            with patch.object(terminal_panel, 'owning_attempt', return_value=[parent]), \
                    patch.object(terminal_panel, 'events', return_value=json.dumps([parent, worker])), \
                    patch.object(terminal_panel, '_rework_history', return_value=[worker]), \
                    patch.object(terminal_panel, 'results', return_value={'rows': [{'calls': []}]}):
                panel_dispatch.continuation(salvage_repo, 'codex', 'main', 'unrecovered-task',
                                            {'provider': 'codex'}, continuation_brief, {}, work_repo=salvage_repo,
                                            patch_reference={})
        except ValueError as error:
            assert '--salvage unrecovered-task' in str(error), error
        else:
            raise AssertionError('unrecorded stopped write attempt continued')
        assert all(path.read_bytes() == data for path, data in provenance_files.items())
        (salvage_repo / scratch_patch).write_bytes(original_indexed_patch)
        scratch_record['patch'] = scratch_patch
        scratch_record['patch_sha256'] = hashlib.sha256(original_indexed_patch).hexdigest()
    with patch.object(panel_salvage.dispatch_supervisor, 'read', return_value={
            'call': {'participant': 'call-0123456789abcdef', 'room': 'room-fixture',
                     'task_id': 'another-task'},
            'execution_repo': str(salvage_execution_repo)}):
        try:
            panel_salvage.salvage(salvage_repo, 'salvage-task', salvage_execution_repo)
        except ValueError as error:
            assert 'matching recorded execution repository' in str(error), error
        else:
            raise AssertionError('a launch anchor recorded for another task was accepted')
    assert salvaged['salvaged_from'] == {'task': 'salvage-task', 'attempt': 'att_fixture',
        'kept_path': str(salvage_tree), 'stop_reason': 'worker_authority_violation'}
    assert {f['path'] for f in salvaged['files']} == {'unicode.txt', 'deleted.txt', 'binary.dat', 'new.txt'}
    assert not any('oms-room-relay' in f['path'] for f in salvaged['files']), salvaged['files']
    flagged = {(w['file'], w['kind']) for w in salvaged['encoding_warnings']}
    assert ('unicode.txt', 'transliteration_hunk') in flagged, flagged
    assert ('unicode.txt', 'non_ascii_drop') in flagged, flagged
    saved_patch = Path(salvaged['patch_path'])
    assert saved_patch.exists() and Path(salvaged['metadata_path']).exists()
    assert saved_patch.read_bytes().startswith(panel_salvage.PREFIX)
    assert b'oms-room-relay' not in saved_patch.read_bytes(), 'relay queue residue entered the patch'
    assert saved_patch.is_relative_to(salvage_repo), 'salvaged artifact was not written to the room repository'
    assert not trap_file.exists(), 'salvage executed repository Git config'
    assert index_path.read_bytes() == index_before, 'salvage changed kept index'
    assert all(p.read_bytes() == data for p, data in metadata_before.items()), 'salvage changed Git metadata'
    for options, message in (({}, '--accept-salvaged'),
                             ({'accept_salvaged': True}, '--accept-encoding-loss')):
        try:
            panel_salvage.admission(saved_patch, repo=salvage_repo, **options)
        except ValueError as error:
            assert message in str(error), error
        else:
            raise AssertionError('salvaged patch passed without explicit acceptance')
    panel_salvage.admission(saved_patch, True, True, repo=salvage_repo)
    copy = temporary / 'copied-salvage.patch'
    copy.write_bytes(saved_patch.read_bytes())
    try:
        panel_salvage.admission(copy, repo=salvage_repo)
    except ValueError as error:
        assert '--accept-salvaged' in str(error)
    else:
        raise AssertionError('copied salvage lost admission protection')
    controller_path = salvage_repo / salvaged['provenance']
    controller_before = controller_path.read_bytes()
    original_body = saved_patch.read_bytes()
    first, unused, payload = original_body.partition(b'\n')
    display = json.loads(first[len(panel_salvage.PREFIX):])
    display.pop('encoding_warnings')
    stripped = panel_salvage.PREFIX + json.dumps(display).encode() + b'\n' + payload
    for body in (b'\n' + original_body, stripped, payload, original_body + b'\n'):
        copy.write_bytes(body)
        for options, message in (({}, '--accept-salvaged'),
                                 ({'accept_salvaged': True}, '--accept-encoding-loss')):
            try:
                panel_salvage.admission(copy, repo=salvage_repo, **options)
            except ValueError as error:
                assert message in str(error), error
            else:
                raise AssertionError('editable display provenance bypassed controller record')
        panel_salvage.admission(copy, True, True, repo=salvage_repo)
    assert controller_path.read_bytes() == controller_before
    # Recording the same payload with fewer warnings never erases earlier ones.
    panel_salvage.record_provenance(salvage_repo, dict(salvaged, encoding_warnings=[]))
    try:
        panel_salvage.admission(copy, True, repo=salvage_repo)
    except ValueError as error:
        assert '--accept-encoding-loss' in str(error)
    else:
        raise AssertionError('a repeated payload erased controller warnings')
    # An unrecorded payload is ordinary even with forged or malformed display metadata.
    unrecorded = temporary / 'unrecorded-salvage.patch'
    unrecorded.write_bytes(b'# oms-salvage: not JSON\n' + payload.replace(b'label:   cafe', b'label: unrelated'))
    panel_salvage.admission(unrecorded, repo=salvage_repo)
    with patch.dict(salvage_attempt, terminal=False), contextlib.redirect_stderr(io.StringIO()):
        assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task']) == 2
    for invalid in (['--dry-run'], ['--dispatch', 'worker'], ['--results']):
        try:
            with contextlib.redirect_stderr(io.StringIO()):
                panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task', *invalid])
        except SystemExit as error:
            assert error.code == 2
        else:
            raise AssertionError('salvage accepted conflicting operation')
    with patch.dict(os.environ, {'OMS_HARNESS_CHILD': '1'}), contextlib.redirect_stderr(io.StringIO()):
        assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task']) == 2
    pointer = salvage_tree / '.git'
    pointer_before = pointer.read_bytes()
    pointer.write_bytes((project / '.git').read_bytes() if (project / '.git').is_file() else b'gitdir: /missing\n')
    try:
        with contextlib.redirect_stderr(io.StringIO()):
            assert panel.main(['--repo', str(salvage_repo), '--salvage', 'salvage-task']) == 2
    finally:
        pointer.write_bytes(pointer_before)
# The generated binary patch restores exact bytes through Git's normal apply path.
# Standalone calls have no room launch; explicit attempt + hashed index evidence
# binds their stopped checkout to this execution repository without a room bypass.
standalone_artifact = '.oms/artifacts/delegate/stopped.md'
standalone_body = ('- worktree: ' + str(salvage_tree) + '\n').encode()
standalone_path = salvage_execution_repo / standalone_artifact
standalone_path.parent.mkdir(parents=True)
standalone_path.write_bytes(standalone_body)
standalone_attempt = dict(attempt_id='att_standalone', tool='peer-delegate', terminal=True,
                          reason_code='worker_authority_violation', refs={})
standalone_record = dict(kind='delegate', attempt_id='att_standalone', exit=1,
                        artifact=standalone_artifact,
                        artifact_sha256=hashlib.sha256(standalone_body).hexdigest(),
                        base_sha=panel_salvage._read(panel_salvage._git_directory(salvage_tree), 'HEAD').decode().strip()[:12])
def standalone_query(repo, verb, *args):
    return [standalone_attempt] if verb == 'agent-events' else {'rows': [standalone_record]}
with patch.dict(os.environ, dict(salvage_env, OMS_ROOM_ID='', OMS_ROOM_REPO='')), \
        patch.object(panel_results, '_query', side_effect=standalone_query), \
        patch.object(panel_salvage.dispatch_supervisor, 'read', side_effect=AssertionError('standalone read room launch')):
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert panel_salvage.cli_main(['--repo', str(salvage_execution_repo), '--attempt',
                                      'att_standalone', '--artifact', standalone_artifact, '--json']) == 0
    standalone = json.loads(output.getvalue())
    assert standalone['salvaged_from']['attempt'] == 'att_standalone'
    assert Path(standalone['patch_path']).is_relative_to(salvage_execution_repo)
    assert not trap_file.exists() and index_path.read_bytes() == index_before
    for mapping, values, message in (
            (standalone_attempt, {'terminal': False}, 'stopped delegate'),
            (standalone_attempt, {'refs': {'panel_room_id': 'room-fixture'}}, 'room delegates'),
            (standalone_record, {'artifact_sha256': '0' * 64}, 'digest'),
            (standalone_record, {'attempt_id': 'att_unrelated'}, 'matching failed'),
            (standalone_record, {'base_sha': '0' * 12}, 'recorded base'),
            (standalone_record, {'task_id': 'other-task'}, 'matching failed')):
        with patch.dict(mapping, values):
            try:
                panel_salvage.salvage(salvage_execution_repo, attempt_id='att_standalone',
                                      artifact=standalone_artifact)
            except ValueError as error:
                assert message in str(error), error
            else:
                raise AssertionError('unbound standalone recovery accepted: ' + message)
    with contextlib.redirect_stderr(io.StringIO()):
        assert panel_salvage.cli_main(['--repo', str(salvage_execution_repo), '--attempt', 'att_standalone']) == 2

for key in ('filter.trap.clean', 'filter.trap.smudge', 'diff.external', 'core.fsmonitor'):
    subprocess.run(['git', '-C', str(salvage_execution_repo), 'config', '--unset', key], check=True)
subprocess.run(['git', '-C', str(salvage_repo), 'apply', '--check', '--binary', str(saved_patch)], check=True)
subprocess.run(['git', '-C', str(salvage_repo), 'apply', '--binary', str(saved_patch)], check=True)
assert (salvage_repo / 'new.txt').read_bytes() == '새 파일\r\n'.encode()
assert (salvage_repo / 'binary.dat').read_bytes() == b'\0new\xfe'
assert (salvage_repo / 'unicode.txt').read_bytes() == b'label:   cafe\n'
assert not (salvage_repo / 'deleted.txt').exists()
# A kept worktree is captured against its recorded launch base, so worker commits are salvaged;
# a clean checkout records an empty outcome and stops counting as recovery evidence.
def kept_fixture(name):
    tree = salvage_registry / ('oh-my-setting-delegate.' + name) / ('wt-' + name)
    tree.parent.mkdir(parents=True)
    subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'add', '--detach', str(tree)],
                   check=True, stdout=subprocess.DEVNULL)
    (tree.parent / '.oh-my-setting-tmp').write_text(
        'kind=oh-my-setting-temp\ntemporary=1\nrepo=%s\nworktree=%s\n' % (salvage_execution_repo, tree))
    attempt = {'task_id': name + '-task', 'attempt_id': 'att_' + name, 'tool': 'peer-delegate',
               'created_at': '2026-10-17T00:00:00Z', 'terminal': True, 'state': 'failed',
               'reason_code': 'worker_authority_violation',
               'refs': {'panel_location': 'worktree:oh-my-setting-delegate.%s/wt-%s' % (name, name),
                        'panel_room_participant': 'call-' + hashlib.sha256(name.encode()).hexdigest()[:16],
                        'panel_room_id': 'room-fixture'}}
    return tree, attempt

def kept_salvage(tree, attempt, record):
    def query(repo, verb, *args):
        return [attempt] if verb == 'agent-events' else {'rows': [record] if record else []}
    anchor = {'call': {'participant': attempt['refs']['panel_room_participant'], 'room': 'room-fixture',
                       'task_id': attempt['task_id']}, 'execution_repo': str(salvage_execution_repo)}
    with patch.dict(os.environ, salvage_env), patch.object(panel_results, '_query', side_effect=query), \
            patch.object(panel_salvage.dispatch_supervisor, 'read', return_value=anchor):
        return panel_salvage.salvage(salvage_repo, attempt['task_id'], salvage_execution_repo)

launch_base = subprocess.run(['git', '-C', str(salvage_tree), 'rev-parse', 'HEAD'], check=True,
                             capture_output=True, text=True).stdout.strip()
committed_tree, committed_attempt = kept_fixture('committed')
git_worker = ['git', '-C', str(committed_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
(committed_tree / 'committed.md').write_text('committed by the worker\n')
subprocess.run(git_worker + ['add', 'committed.md'], check=True)
subprocess.run(git_worker + ['commit', '-qm', 'worker commit'], check=True)
(committed_tree / 'dirty.md').write_text('left uncommitted\n')
committed_record = {'kind': 'delegate', 'task_id': 'committed-task', 'attempt_id': 'att_committed',
                    'exit': 124, 'base_sha': launch_base[:12]}
committed = kept_salvage(committed_tree, committed_attempt, committed_record)
assert committed['base_sha'] == launch_base, committed['base_sha']
assert {row['path'] for row in committed['files']} == {'committed.md', 'dirty.md'}, committed['files']

# A committed gitlink must reach the same refusal as one tracked in the launch base.
gitlink_tree, gitlink_attempt = kept_fixture('gitlink')
git_gitlink = ['git', '-C', str(gitlink_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
subprocess.run(['git', 'init', '-q', str(temporary / 'gitlink-source')], check=True)
(temporary / 'gitlink-source' / 'payload').write_text('gitlink target\n')
subprocess.run(['git', '-C', str(temporary / 'gitlink-source'), 'add', 'payload'], check=True)
subprocess.run(['git', '-C', str(temporary / 'gitlink-source'), '-c', 'user.email=test@example.com',
                '-c', 'user.name=test', 'commit', '-qm', 'target'], check=True)
gitlink_oid = subprocess.run(['git', '-C', str(temporary / 'gitlink-source'), 'rev-parse', 'HEAD'],
                             check=True, capture_output=True, text=True).stdout.strip()
subprocess.run(git_gitlink + ['update-index', '--add', '--cacheinfo', '160000', gitlink_oid, 'vendor'], check=True)
subprocess.run(git_gitlink + ['commit', '-qm', 'add gitlink'], check=True)
try:
    kept_salvage(gitlink_tree, gitlink_attempt, {'kind': 'delegate', 'task_id': 'gitlink-task',
                 'attempt_id': 'att_gitlink', 'exit': 124, 'base_sha': launch_base[:12]})
except ValueError as error:
    assert 'salvage of submodule worktrees requires owner inspection' in str(error), error
else:
    raise AssertionError('committed gitlink was silently omitted from salvage')

# A newly committed executable retains its mode.
executable_tree, executable_attempt = kept_fixture('executable')
git_executable = ['git', '-C', str(executable_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
(executable_tree / 'run.sh').write_text('#!/bin/sh\nprintf ok\n')
(executable_tree / 'run.sh').chmod(0o755)
subprocess.run(git_executable + ['add', 'run.sh'], check=True)
subprocess.run(git_executable + ['commit', '-qm', 'add executable'], check=True)
executable_patch, executable_files, _ = panel_salvage._capture(
    executable_tree, Path(subprocess.run(['git', '-C', str(executable_tree), 'rev-parse', '--git-common-dir'],
                                         check=True, capture_output=True, text=True).stdout.strip()),
    launch_base, subprocess.run(['git', '-C', str(executable_tree), 'rev-parse', 'HEAD'],
                                check=True, capture_output=True, text=True).stdout.strip())
assert executable_files and 'run.sh' in {row['path'] for row in executable_files}, executable_files
assert b'new file mode 100755' in executable_patch, executable_patch
for broken, expected in (({'base_sha': ''}, 'valid recorded launch base'), ({'base_sha': 'f' * 12}, 'recorded launch base')):
    try:
        kept_salvage(committed_tree, committed_attempt, dict(committed_record, **broken))
    except ValueError as error:
        assert expected in str(error), error
    else:
        raise AssertionError('salvage diffed against the kept HEAD without a usable recorded base')

# A force-added commit of an ignored path is part of the work even when the checkout is otherwise clean.
ignored_tree, ignored_attempt = kept_fixture('ignored')
git_ignored = ['git', '-C', str(ignored_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
(ignored_tree / '.gitignore').write_text('*.ign\n')
(ignored_tree / 'forced.ign').write_text('ignored but committed\n')
subprocess.run(git_ignored + ['add', '.gitignore'], check=True)
subprocess.run(git_ignored + ['add', '-f', 'forced.ign'], check=True)
subprocess.run(git_ignored + ['commit', '-qm', 'force-added ignored file'], check=True)
ignored = kept_salvage(ignored_tree, ignored_attempt, {
    'kind': 'delegate', 'task_id': 'ignored-task', 'attempt_id': 'att_ignored', 'exit': 124,
    'base_sha': launch_base[:12]})
assert {row['path'] for row in ignored['files']} == {'.gitignore', 'forced.ign'}, ignored['files']
assert not (salvage_repo / panel_salvage._empty_provenance_path('ignored-task', 'att_ignored')).exists()

# Without an indexed record nothing names the launch base, so a clean-looking checkout is no empty outcome.
norecord_tree, norecord_attempt = kept_fixture('norecord')
git_norecord = ['git', '-C', str(norecord_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
(norecord_tree / 'early.md').write_text('committed before indexing\n')
subprocess.run(git_norecord + ['add', 'early.md'], check=True)
subprocess.run(git_norecord + ['commit', '-qm', 'early commit'], check=True)
try:
    kept_salvage(norecord_tree, norecord_attempt, None)
except ValueError as error:
    assert 'no delegate artifact-index record names its launch base' in str(error), error
else:
    raise AssertionError('committed worktree without an indexed record was salvaged as a patch or empty')
assert not (salvage_repo / panel_salvage._empty_provenance_path('norecord-task', 'att_norecord')).exists()
with patch.dict(os.environ, salvage_env), patch.object(
        panel_results, '_query', side_effect=lambda repo, verb, *args: {'rows': []}):
    assert panel_salvage._without_clean_worktrees(
        salvage_repo, 'norecord-task', 'att_norecord', salvage_execution_repo,
        ['registered worktree: ' + str(norecord_tree)]) == ['registered worktree: ' + str(norecord_tree)]

clean_tree, clean_attempt = kept_fixture('clean')
clean_record = {'kind': 'delegate', 'task_id': 'clean-task', 'attempt_id': 'att_clean', 'exit': 124,
                'base_sha': launch_base[:12]}
clean_receipt = salvage_repo / panel_salvage._empty_provenance_path('clean-task', 'att_clean')
clean_evidence = ['registered worktree: ' + str(clean_tree)]
def drop_clean():
    with patch.dict(os.environ, salvage_env), patch.object(
            panel_results, '_query', side_effect=lambda repo, verb, *args: {'rows': [clean_record]}):
        return panel_salvage._without_clean_worktrees(
            salvage_repo, 'clean-task', 'att_clean', salvage_execution_repo, clean_evidence)
empty = kept_salvage(clean_tree, clean_attempt, clean_record)
assert empty['outcome'] == 'nothing_to_recover' and empty['reason'] == 'kept worktree is clean at its recorded base', empty
assert clean_receipt.is_file(), 'clean kept worktree left no empty-salvage receipt'
assert drop_clean() == []
(clean_tree / 'late.md').write_text('written after inspection\n')
assert drop_clean() == clean_evidence, 'a dirtied checkout stopped counting as recovery evidence'
(clean_tree / 'late.md').unlink()
subprocess.run(['git', '-C', str(clean_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test',
                'commit', '-q', '--allow-empty', '-m', 'empty worker commit'], check=True)
assert drop_clean() == [], 'an empty worker commit changed the clean outcome'
# Continuation materializes verified salvage; changed checkouts still require another salvage.
dirty_brief = temporary / 'dirty-salvage-brief.md'
dirty_brief.write_text('Continue safely.\n')
dirty_parent = {'attempt_id': 'dirty-parent', 'terminal': False, 'refs': {}}
def continue_dirty(attempt=committed_attempt, record=committed_record, reference=None, materialize=True):
    worker = dict(attempt, parent_attempt_id='dirty-parent',
                  refs=dict(attempt['refs'], panel_access='write', panel_provider='codex',
                            native_session='fixture-session'))
    with patch.dict(os.environ, salvage_env), patch.object(
            panel_results, '_query', side_effect=lambda repo, verb, *args:
            [attempt] if verb == 'agent-events' else {'rows': [record] if record else []}), \
            patch.object(terminal_panel, 'owning_attempt', return_value=[dirty_parent]), \
            patch.object(terminal_panel, 'events', return_value=json.dumps([dirty_parent, worker])), \
            patch.object(terminal_panel, '_rework_history', return_value=[worker]), \
            patch.object(terminal_panel, 'results', return_value={'rows': [{'calls': []}]}), \
            patch.object(terminal_panel, 'session_stored', return_value=True):
        try:
            result = panel_dispatch.continuation(
                salvage_repo, 'codex', 'main', attempt['task_id'], {'provider': 'codex'}, dirty_brief,
                {'OMS_ROOM_REPO': str(salvage_repo)},
                work_repo=salvage_execution_repo,
                patch_reference=(reference if reference is not None else {}) if materialize else None)
        except ValueError as error:
            return str(error)
    if not materialize:
        try:
            assert '# oms-salvage:' not in result[0].read_text(), result[0].read_text()
        finally:
            result[0].unlink()
    if reference is not None:
        return result
    raise AssertionError('continue resumed stopped write work')
dirty_patch = committed['patch']
ref = {}
result = continue_dirty(reference=ref)
assert ref == {'patch': dirty_patch, 'sha256': committed['patch_sha256'], 'base': launch_base,
               'attempt': committed_attempt['attempt_id']}, ref
assert 'verified patch carried forward' in result[2], result
result[0].unlink()
committed_head = subprocess.run(git_worker + ['rev-parse', 'HEAD'], check=True,
                                capture_output=True, text=True).stdout.strip()
reset_reference = {}
try:
    subprocess.run(git_worker + ['reset', '--hard', launch_base], check=True, stdout=subprocess.DEVNULL)
    (committed_tree / 'dirty.md').unlink()
    reset_result = continue_dirty(reference=reset_reference)
    if not isinstance(reset_result, str):
        reset_result[0].unlink()
    assert isinstance(reset_result, str), reset_result
    assert not reset_reference, reset_reference
    assert reset_result.startswith('already salvaged as ' + dirty_patch), reset_result
    assert 'registered worktree: ' + str(committed_tree) in reset_result, reset_result
    assert '--salvage committed-task --attempt att_committed' in reset_result, reset_result
finally:
    subprocess.run(git_worker + ['reset', '--hard', committed_head], check=True, stdout=subprocess.DEVNULL)
    (committed_tree / 'dirty.md').write_text('left uncommitted\n')
(committed_tree / 'dirty.md').write_text('changed after salvage\n')
dirty_message = continue_dirty()
assert dirty_message.startswith('already salvaged as ' + dirty_patch), dirty_message
assert 'registered worktree: ' + str(committed_tree) in dirty_message, dirty_message
assert 'already salvaged as ' + dirty_patch in dirty_message and 'fresh dispatch' in dirty_message, dirty_message
assert dirty_message.index('fresh dispatch') < dirty_message.index('registered worktree: '), dirty_message
# A second capture supersedes the first by recorded time, independent of filesystem timestamps.
first_patch = dirty_patch
first_sidecar = salvage_repo / (first_patch + '.json')
first_metadata = json.loads(first_sidecar.read_bytes())
earlier_patch = '.oms/artifacts/delegate/codex-committed-earlier.patch'
earlier_body = panel_salvage._payload((salvage_repo / first_patch).read_bytes())
(salvage_repo / earlier_patch).write_bytes(earlier_body)
committed_record.update(patch=earlier_patch, patch_sha256=hashlib.sha256(earlier_body).hexdigest())
first_ns = 2_000_000_000_000_000_000
os.utime(first_sidecar, ns=(first_ns, first_ns))
committed = kept_salvage(committed_tree, committed_attempt, committed_record)
dirty_patch = committed['patch']
assert isinstance(committed.get('salvaged_at'), str), committed
assert committed['salvaged_at'] > first_metadata['salvaged_at'], committed
new_sidecar = salvage_repo / (dirty_patch + '.json')
os.utime(new_sidecar, ns=(0, 0))
assert panel_dispatch._newest_salvage_patch(
    salvage_repo, committed_attempt['task_id'], committed_attempt['attempt_id']) == dirty_patch
for other_attempt in ('', 0, 'att_elsewhere'):
    assert panel_dispatch._newest_salvage_patch(
        salvage_repo, committed_attempt['task_id'], other_attempt) is None
saved_first_sidecar = first_sidecar.read_bytes()
saved_new_sidecar = new_sidecar.read_bytes()
saved_bodies = {path: (salvage_repo / path).read_bytes() for path in (first_patch, dirty_patch)}
legacy_paths = {first_patch: '.oms/artifacts/delegate/salvage-committed-task-legacy-z.patch',
                dirty_patch: '.oms/artifacts/delegate/salvage-committed-task-legacy-a.patch'}
saved_kept_dirty = (committed_tree / 'dirty.md').read_bytes()
kept_removed = False
try:
    tied_metadata = dict(first_metadata, salvaged_at=committed['salvaged_at'])
    first_sidecar.write_text(json.dumps(tied_metadata))
    os.utime(first_sidecar, ns=(first_ns, first_ns))
    ordered = sorted((first_sidecar, new_sidecar), reverse=True)
    with patch.object(Path, 'glob', return_value=iter(ordered)):
        tied = panel_dispatch._newest_salvage_patch(
            salvage_repo, committed_attempt['task_id'], committed_attempt['attempt_id'])
    assert tied == json.loads(ordered[0].read_bytes())['patch'], tied
    first_sidecar.write_bytes(saved_first_sidecar)
    for invalid in ('', 0, None, [], 'not-a-time', '2026-02-30T00:00:00.000000Z'):
        invalid_metadata = dict(json.loads(saved_new_sidecar), salvaged_at=invalid)
        new_sidecar.write_text(json.dumps(invalid_metadata))
        assert panel_dispatch._newest_salvage_patch(
            salvage_repo, committed_attempt['task_id'], committed_attempt['attempt_id']) is None
    new_sidecar.write_bytes(saved_new_sidecar)
    subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'remove', '--force', str(committed_tree)], check=True)
    kept_removed = True
    assert not committed_tree.exists()
    with patch.dict(os.environ, salvage_env):
        dated = panel_dispatch._verified_salvage_continuation(
            salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'],
            [first_metadata['provenance'], committed['provenance']])
    assert dated['patch'] == dirty_patch, dated['patch']
    assert dated['payload_sha256'] == committed['payload_sha256']
    legacy = json.loads(saved_first_sidecar)
    legacy.pop('salvaged_at')
    first_sidecar.write_text(json.dumps(legacy))
    legacy = json.loads(saved_new_sidecar)
    legacy.pop('salvaged_at')
    new_sidecar.write_text(json.dumps(legacy))
    assert panel_dispatch._newest_salvage_patch(
        salvage_repo, committed_attempt['task_id'], committed_attempt['attempt_id']) == \
        json.loads(max(first_sidecar, new_sidecar).read_bytes())['patch']
    for path, body in saved_bodies.items():
        header, payload = body.split(b'\n', 1)
        legacy_header = json.loads(header[len(panel_salvage.PREFIX):])
        legacy_header.pop('salvaged_at')
        legacy_body = panel_salvage.PREFIX + json.dumps(legacy_header).encode() + b'\n' + payload
        (salvage_repo / path).write_bytes(legacy_body)
        sidecar = salvage_repo / (path + '.json')
        legacy = json.loads(sidecar.read_bytes())
        legacy['patch_sha256'] = hashlib.sha256(legacy_body).hexdigest()
        sidecar.write_text(json.dumps(legacy))
    for path, alias in legacy_paths.items():
        (salvage_repo / path).rename(salvage_repo / alias)
        (salvage_repo / (path + '.json')).rename(salvage_repo / (alias + '.json'))
        sidecar = salvage_repo / (alias + '.json')
        legacy = json.loads(sidecar.read_bytes())
        legacy['patch'] = alias
        sidecar.write_text(json.dumps(legacy))
    old_alias, new_alias = legacy_paths[first_patch], legacy_paths[dirty_patch]
    os.utime(salvage_repo / (old_alias + '.json'), ns=(0, 0))
    os.utime(salvage_repo / (new_alias + '.json'), ns=(first_ns, first_ns))
    assert old_alias > new_alias
    assert panel_dispatch._newest_salvage_patch(
        salvage_repo, committed_attempt['task_id'], committed_attempt['attempt_id']) == old_alias
    assert first_metadata['payload_sha256'] != committed['payload_sha256']
    retained = {salvage_repo / (alias + suffix): (salvage_repo / (alias + suffix)).read_bytes()
                for alias in legacy_paths.values() for suffix in ('', '.json')}
    with patch.dict(os.environ, salvage_env):
        ambiguous = panel_dispatch._verified_salvage_continuation(
            salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'],
            [first_metadata['provenance'], committed['provenance']])
        assert ambiguous is None, ambiguous['patch']
    legacy_ref = {}
    legacy_result = continue_dirty(reference=legacy_ref)
    if not isinstance(legacy_result, str):
        legacy_result[0].unlink()
    assert isinstance(legacy_result, str), legacy_result
    assert not legacy_ref, legacy_ref
    assert '--salvage committed-task --attempt att_committed' in legacy_result, legacy_result
    assert all(path.read_bytes() == body for path, body in retained.items())
    for suffix in ('', '.json'):
        path = salvage_repo / (new_alias + suffix)
        path.rename(path.with_name(path.name + '.hidden'))
    with patch.dict(os.environ, salvage_env):
        single = panel_dispatch._verified_salvage_continuation(
            salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'],
            [first_metadata['provenance']])
    assert single['patch'] == old_alias
    assert single['payload_sha256'] == first_metadata['payload_sha256']
    for suffix in ('', '.json'):
        path = salvage_repo / (new_alias + suffix)
        path.with_name(path.name + '.hidden').rename(path)
    (salvage_repo / new_alias).write_bytes((salvage_repo / old_alias).read_bytes())
    identical_metadata = dict(json.loads((salvage_repo / (old_alias + '.json')).read_bytes()), patch=new_alias)
    (salvage_repo / (new_alias + '.json')).write_text(json.dumps(identical_metadata))
    with patch.dict(os.environ, salvage_env):
        identical = panel_dispatch._verified_salvage_continuation(
            salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'],
            [first_metadata['provenance']])
    assert identical['patch'] == old_alias
    assert identical['payload_sha256'] == first_metadata['payload_sha256']
finally:
    for alias in legacy_paths.values():
        for suffix in ('', '.json', '.hidden', '.json.hidden'):
            path = salvage_repo / (alias + suffix)
            if path.exists():
                path.unlink()
    for path, body in saved_bodies.items():
        (salvage_repo / path).write_bytes(body)
    first_sidecar.write_bytes(saved_first_sidecar)
    new_sidecar.write_bytes(saved_new_sidecar)
    os.utime(first_sidecar, ns=(first_ns, first_ns))
    os.utime(new_sidecar, ns=(0, 0))
    if kept_removed:
        subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'add', '--detach',
                        str(committed_tree), committed_head], check=True, stdout=subprocess.DEVNULL)
        (committed_tree / 'dirty.md').write_bytes(saved_kept_dirty)
ref = {}
with patch.object(panel_salvage, '_launch_base', wraps=panel_salvage._launch_base) as resolve_base:
    result = continue_dirty(reference=ref)
assert any(call.args == (salvage_execution_repo, {'base_sha': launch_base})
           for call in resolve_base.call_args_list), resolve_base.call_args_list
assert ref == {'patch': dirty_patch, 'sha256': committed['patch_sha256'], 'base': launch_base,
               'attempt': committed_attempt['attempt_id']}, ref
assert 'verified patch carried forward' in result[2], result
result[0].unlink()
inline_message = continue_dirty(materialize=False)
assert inline_message.startswith('already salvaged as ' + dirty_patch), inline_message
assert 'fresh dispatch' in inline_message, inline_message
with patch.dict(os.environ, dict(salvage_env, GIT_DIR=str(temporary / 'missing-git-directory'))):
    assert panel_dispatch._verified_salvage_continuation(
        salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'],
        [committed['provenance'], first_metadata['provenance'], earlier_patch,
         'registered worktree: ' + str(committed_tree)])
# Without a salvage record the refusal is unchanged.
for path in (salvage_repo / '.oms/salvage-provenance').glob('*.json'):
    path.rename(path.with_suffix('.hidden'))
try:
    plain_message = continue_dirty()
finally:
    for path in (salvage_repo / '.oms/salvage-provenance').glob('*.hidden'):
        path.rename(path.with_suffix('.json'))
assert 'already salvaged' not in plain_message and 'fresh dispatch' not in plain_message, plain_message
assert plain_message.endswith('registered worktree: ' + str(committed_tree)), plain_message
# A worktree-missing salvage from the indexed patch leaves no provenance; --continue still says it was salvaged.
gone_sidecars = list((salvage_repo / '.oms/artifacts/delegate').glob('salvage-scratch-task-*.patch.json'))
assert gone_sidecars, 'worktree-missing salvage published no sidecar'
gone_provenance = list((salvage_repo / '.oms/salvage-provenance').glob('*.json'))
for path in gone_provenance:
    path.rename(path.with_suffix('.hidden'))
try:
    gone_message = continue_dirty(gone_attempt, gone_record)
    for path in gone_sidecars:
        path.rename(path.with_suffix('.hidden'))
    try:
        unsalvaged_message = continue_dirty(gone_attempt, gone_record)
    finally:
        for path in gone_sidecars:
            path.with_suffix('.hidden').rename(path)
finally:
    for path in gone_provenance:
        path.with_suffix('.hidden').rename(path)
assert 'run oms panel --salvage scratch-task' in unsalvaged_message, unsalvaged_message
assert 'already salvaged' not in unsalvaged_message and 'fresh dispatch' not in unsalvaged_message, unsalvaged_message
assert 'registered worktree: ' not in gone_message, gone_message
gone_patch = panel_dispatch._newest_salvage_patch(salvage_repo, 'scratch-task', gone_attempt['attempt_id'])
assert gone_patch and 'already salvaged as ' + gone_patch in gone_message and 'fresh dispatch' in gone_message, gone_message
assert gone_message.startswith('already salvaged as ' + gone_patch), gone_message
assert gone_message.index('fresh dispatch') < gone_message.index('recoverable delegate evidence'), gone_message
# The same verified salvage remains usable after the kept checkout is removed.
subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'remove', '--force', str(committed_tree)], check=True)
ref = {}
result = continue_dirty(reference=ref)
assert ref == {'patch': dirty_patch, 'sha256': committed['patch_sha256'], 'base': launch_base,
               'attempt': committed_attempt['attempt_id']}, ref
assert 'verified patch carried forward' in result[2], result
result[0].unlink()
provenance_item = committed['provenance']
evidence = [provenance_item, first_metadata['provenance']]
def refuse_stale(attempt=committed_attempt, record=committed_record):
    reference = {}
    result = continue_dirty(attempt, record, reference=reference)
    if not isinstance(result, str):
        result[0].unlink()
        raise AssertionError('continued with superseded patch: %r' % reference)
    assert not reference, reference
    assert 'recoverable delegate evidence' in result, result
    return result

prefixed_tree, prefixed_attempt = kept_fixture('committed-task-extra')
(prefixed_tree / 'other.md').write_text('another task with a shared prefix\n')
kept_salvage(prefixed_tree, prefixed_attempt, {'kind': 'delegate', 'task_id': prefixed_attempt['task_id'],
             'attempt_id': prefixed_attempt['attempt_id'], 'exit': 124, 'base_sha': launch_base})
prefixed_ref = {}
prefixed_result = continue_dirty(reference=prefixed_ref)
assert prefixed_ref.get('patch') == dirty_patch, prefixed_result
prefixed_result[0].unlink()

clean_continuation = continue_dirty(clean_attempt, clean_record, reference={})
assert isinstance(clean_continuation, tuple), clean_continuation
clean_continuation[0].unlink()
orphan = salvage_repo / '.oms/artifacts/delegate/salvage-clean-task-unpaired.patch'
orphan.write_bytes(b'interrupted artifact\n')
try:
    refuse_stale(clean_attempt, clean_record)
finally:
    orphan.unlink()

# Recovery must inspect the published salvage files, without injected artifact-index rows.
try:
    for invalid in (b'', b'0', b'[]', b'{'):
        new_sidecar.write_bytes(invalid)
        refuse_stale()
    for invalid in ('', 0, None, [], 'not-a-time', '2026-02-30T00:00:00.000000Z'):
        new_sidecar.write_text(json.dumps(dict(json.loads(saved_new_sidecar), salvaged_at=invalid)))
        refuse_stale()
    missing_time = json.loads(saved_new_sidecar)
    missing_time.pop('salvaged_at')
    new_sidecar.write_text(json.dumps(missing_time))
    refuse_stale()
    new_sidecar.write_text(json.dumps(dict(json.loads(saved_new_sidecar),
                                          salvaged_at=first_metadata['salvaged_at'])))
    refuse_stale()
    for invalid in ({'patch': first_patch}, {'salvaged_from': {}},
                    {'salvaged_from': {'task': committed_attempt['task_id'], 'attempt': 0}},
                    {'salvaged_from': {'task': '', 'attempt': committed_attempt['attempt_id']}},
                    {'salvaged_from': {'task': 'other-task', 'attempt': committed_attempt['attempt_id']}},
                    {'salvaged_from': {'task': committed_attempt['task_id'], 'attempt': ''}}):
        new_sidecar.write_text(json.dumps(dict(json.loads(saved_new_sidecar), **invalid)))
        refuse_stale()
finally:
    new_sidecar.write_bytes(saved_new_sidecar)
try:
    for invalid in (b'no salvage header\n', panel_salvage.PREFIX + b'0\n', panel_salvage.PREFIX + b'{\n'):
        damaged = invalid + panel_salvage._payload(saved_bodies[dirty_patch])
        (salvage_repo / dirty_patch).write_bytes(damaged)
        new_sidecar.write_text(json.dumps(dict(json.loads(saved_new_sidecar),
                                              patch_sha256=hashlib.sha256(damaged).hexdigest())))
        refuse_stale()
finally:
    (salvage_repo / dirty_patch).write_bytes(saved_bodies[dirty_patch])
    new_sidecar.write_bytes(saved_new_sidecar)
for path in (salvage_repo / dirty_patch, salvage_repo / first_patch):
    saved = path.read_bytes()
    try:
        for invalid in (b'', b'corrupted patch\n'):
            path.write_bytes(invalid)
            refuse_stale()
    finally:
        path.write_bytes(saved)
for path in (new_sidecar, salvage_repo / dirty_patch, first_sidecar, salvage_repo / first_patch):
    hidden = path.with_name(path.name + '.hidden')
    path.rename(hidden)
    try:
        refuse_stale()
        path.symlink_to(hidden.name)
        refuse_stale()
    finally:
        if path.is_symlink():
            path.unlink()
        hidden.rename(path)

interrupted_tree, interrupted_attempt = kept_fixture('repeat-cut')
(interrupted_tree / 'edit.md').write_text('first capture\n')
interrupted_record = {'kind': 'delegate', 'task_id': interrupted_attempt['task_id'],
                      'attempt_id': interrupted_attempt['attempt_id'], 'exit': 124, 'base_sha': launch_base}
kept_salvage(interrupted_tree, interrupted_attempt, interrupted_record)
(interrupted_tree / 'edit.md').write_text('second capture\n')
publish = panel_results.DURABLE['atomic_mutate']
def interrupt_resalvage(*args, **kwargs):
    if kwargs.get('label') == 'salvage patch':
        raise OSError('interrupted second capture before publishing its patch')
    return publish(*args, **kwargs)
with patch.dict(panel_results.DURABLE, {'atomic_mutate': interrupt_resalvage}):
    try:
        kept_salvage(interrupted_tree, interrupted_attempt, interrupted_record)
    except OSError:
        pass
    else:
        raise AssertionError('second salvage survived interrupted publication')
subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'remove', '--force', str(interrupted_tree)], check=True)
refuse_stale(interrupted_attempt, interrupted_record)

with patch.dict(os.environ, salvage_env):
    select = lambda items: panel_dispatch._verified_salvage_continuation(
        salvage_repo, salvage_execution_repo, committed_attempt['task_id'], committed_attempt['attempt_id'], items)
    assert select(evidence)
    assert select([]) is None
    assert select(evidence + ['unreadable provenance: invalid']) is None
    assert select(evidence + ['unexpected evidence']) is None
    assert select(evidence + [clean_receipt.relative_to(salvage_repo).as_posix()]) is None
    assert select(evidence + ['registered worktree: ' + str(committed_tree)]) is None
    assert select(evidence + [first_metadata['provenance'], earlier_patch])
    assert select(evidence + [first_metadata['provenance'], first_patch])
    assert select([provenance_item, first_patch]) is None
    try:
        for invalid in (b'', b'0', b'[]', b'{'):
            first_sidecar.write_bytes(invalid)
            assert select(evidence + [first_metadata['provenance'], first_patch]) is None
        for invalid in ({'salvaged_from': {}}, {'patch_sha256': '0' * 64}, {'payload_sha256': '0' * 64}):
            first_sidecar.write_text(json.dumps(dict(first_metadata, **invalid)))
            assert select(evidence + [first_metadata['provenance'], first_patch]) is None
    finally:
        first_sidecar.write_bytes(saved_first_sidecar)
    historical = salvage_repo / first_metadata['provenance']
    saved_historical = historical.read_bytes()
    try:
        for invalid in (b'', b'0', b'{'):
            historical.write_bytes(invalid)
            assert select(evidence + [first_metadata['provenance']]) is None
        foreign = json.loads(saved_historical)
        for row in foreign['records']:
            row['salvaged_from']['task'] = 'another-task'
        historical.write_text(json.dumps(foreign))
        assert select(evidence + [first_metadata['provenance']]) is None
    finally:
        historical.write_bytes(saved_historical)
    sidecar = salvage_repo / (dirty_patch + '.json')
    saved_sidecar = sidecar.read_bytes()
    meta = json.loads(saved_sidecar)
    meta['salvaged_from']['recovered_from_artifact'] = True
    sidecar.write_text(json.dumps(meta))
    assert select(evidence + [earlier_patch]) is None
    assert select(evidence + [dirty_patch])
    for invalid in ('', '0', 'g' * 40, 'a' * 6, 'a' * 65):
        meta = json.loads(saved_sidecar)
        meta['base_sha'] = invalid
        sidecar.write_text(json.dumps(meta))
        assert select(evidence) is None, invalid
    sidecar.write_bytes(saved_sidecar)

# END salvage-continuation

# A salvage that died after its provenance but before its patch leaves evidence no patch backs: --continue
# says so and points at a re-run, and names a fresh dispatch once the kept worktree is gone.
cut_tree, cut_attempt = kept_fixture('cut')
(cut_tree / 'cut.md').write_text('uncommitted work\n')
cut_record = {'kind': 'delegate', 'task_id': 'cut-task', 'attempt_id': 'att_cut', 'exit': 124,
              'base_sha': launch_base[:12]}
real_mutate = panel_results.DURABLE['atomic_mutate']
def cut_mutate(*args, **kwargs):
    if kwargs.get('label') == 'salvage patch':
        raise OSError('killed before the patch was published')
    return real_mutate(*args, **kwargs)
with patch.dict(panel_results.DURABLE, {'atomic_mutate': cut_mutate}):
    try:
        kept_salvage(cut_tree, cut_attempt, cut_record)
    except OSError:
        pass
    else:
        raise AssertionError('salvage survived a failed patch write')
assert list((salvage_repo / '.oms/salvage-provenance').glob('*.json')), 'provenance was not written first'
subprocess.run(['git', '-C', str(salvage_execution_repo), 'worktree', 'remove', '--force', str(cut_tree)], check=True)
cut_message = continue_dirty(cut_attempt, cut_record)
assert '.oms/salvage-provenance/' in cut_message and 'may have been interrupted' in cut_message, cut_message
assert '`oms panel --salvage cut-task --attempt att_cut`' in cut_message and 'fresh dispatch' in cut_message, cut_message
assert 'nothing recoverable' not in cut_message and 'already salvaged' not in cut_message, cut_message
# A clean kept worktree with no delegate record cannot be certified, so --salvage is not offered.
bare_tree, bare_attempt = kept_fixture('bare')
bare_receipt = salvage_repo / panel_salvage._empty_provenance_path('bare-task', 'att_bare')
try:
    kept_salvage(bare_tree, bare_attempt, {})
except ValueError as error:
    assert 'no delegate artifact-index record names its launch base' in str(error), error
else:
    raise AssertionError('unindexed clean worktree was certified')
assert not bare_receipt.exists(), 'unindexed clean worktree left an empty-salvage receipt'
bare_message = continue_dirty(bare_attempt, {})
assert 'no recorded launch base' in bare_message and 'fresh dispatch is required' in bare_message, bare_message
assert 'run oms panel --salvage' not in bare_message, bare_message
import difflib
encoding_cases = [
    ('café 한글\n', 'cafe ??\n', True),
    ('café café\n', 'cafe caf?\n', True),
    ('naïve\n', 'na�ve\n', True),
    ('café\n', 'café'.encode().decode('latin-1') + '\n', True),
    ('price €\n', 'price €'.encode().decode('cp1252') + '\n', True),
    ('# old comment — details\n', '# rewritten with different words\n', False),
    ('café\n', 'café?\n', False),
    ('', 'AMANHÃ\n', False),
    ('ASCII before\n', 'ASCII after\n', False),
    ('한글\n', '', False),
    ('café\n', 'cafe\nordinary insertion\n', True),
    ('café\n', 'cafe ordinary insertion\n', False),
    ('café\n漢字\n', 'cafe\n', True),
    ('café\nASCII before\n', 'cafe\nASCII after\n', True),
    ('漢字\n', '??\n', True),
    ('café\n', 'caf\n', True),
    ('café\n', 'cafX\n', False),
    ('--café\n', '--cafe\n', True),
    ('++café\n', '++cafe\n', True),
]
for before, after, lossy in encoding_cases:
    diff = ''.join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                       fromfile='a/text.txt', tofile='b/text.txt')).encode()
    payload = b'diff --git a/text.txt b/text.txt\n' + diff
    for warnings in (panel_salvage.encoding_warnings('text.txt', before.encode(), after.encode(), diff),
                     panel_salvage.patch_encoding_warnings(payload)):
        assert bool(warnings) == lossy, (before, after, warnings)
        assert all(row['file'] == 'text.txt' and row.get('hunk', '@@').startswith('@@') for row in warnings)
# Equal lines inside replacement blocks anchor pairing; hunks never cross-pair.
for fragment in ('@@ -1,2 +1,2 @@\n-café\n-anchor\n+anchor\n+cafe\n',
                 '@@ -1 +0,0 @@\n-café\n@@ -9,0 +9 @@\n+cafe\n'):
    diff = fragment.encode()
    assert not panel_salvage.encoding_warnings('text.txt', 'café\n'.encode(), b'cafe\n', diff)
    assert not panel_salvage.patch_encoding_warnings(b'diff --git a/text.txt b/text.txt\n' + diff)
# Latin-1 mojibake can include U+0085, which is content rather than a diff newline.
diff = ('@@ -1 +1 @@\n-Å\n+' + 'Å'.encode().decode('latin-1') + '\n').encode()
assert panel_salvage.encoding_warnings('text.txt', 'Å\n'.encode(), b'', diff)
assert panel_salvage.patch_encoding_warnings(b'diff --git a/text.txt b/text.txt\n' + diff)
