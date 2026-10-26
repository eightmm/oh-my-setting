# Salvage captures raw checkout bytes with a private index/config and owner-only admission.
import contextlib
import shutil
import panel_salvage
from room_repository import harness_residue_parent

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
# Native-path conversion handles Bash marker spellings before Python resolves them.
path_bin = temporary / 'salvage-path-bin'
path_bin.mkdir()
bash_registry = '/c/oms-fixture/registry'
bash_tree = bash_registry + '/oh-my-setting-delegate.fixture/wt'
bash_repo = '/c/oms-fixture/repo'
path_mapping = {bash_registry: str(salvage_registry), bash_tree: str(salvage_tree),
                bash_repo: str(salvage_execution_repo), '/c/oms-fixture/foreign': str(project)}
(path_bin / 'cygpath').write_text('#!%s\nimport sys\n' % sys.executable +
    'mapping = %r\nassert sys.argv[1] == "-m"\nsys.stdout.write(mapping.get(sys.argv[2], sys.argv[2]) + "\\r\\n")\n' % path_mapping)
(path_bin / 'cygpath').chmod(0o755)
path_env = {'MSYSTEM': 'MINGW64', 'PATH': str(path_bin) + os.pathsep + os.environ['PATH'],
            'OMS_DELEGATE_WORKTREE_ROOT': bash_registry}
marker_path = salvage_tree.parent / '.oh-my-setting-tmp'
marker_before = marker_path.read_bytes()
try:
    marker_path.write_text('kind=oh-my-setting-temp\ntemporary=1\npid=99999999\nrepo=%s\nworktree=%s\n' %
                           (bash_repo, bash_tree))
    with patch.dict(os.environ, path_env):
        assert panel_salvage._native_path(bash_repo) == salvage_execution_repo, 'CRLF native path not normalized'
        assert panel_salvage._managed(salvage_execution_repo, bash_tree)[0] == salvage_tree
        marker_path.write_text('kind=oh-my-setting-temp\ntemporary=1\nrepo=/c/oms-fixture/foreign\nworktree=%s\n' % bash_tree)
        try:
            panel_salvage._managed(salvage_execution_repo, bash_tree)
        except ValueError as error:
            assert 'does not belong' in str(error), error
        else:
            raise AssertionError('foreign Bash marker repository passed native identity validation')
finally:
    marker_path.write_bytes(marker_before)

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
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert str(error) == ('nothing to recover for %s; --continue scratch-task may now resume the session '
                                  'without a patch' % scratch_attempt['attempt_id']), error
        else:
            raise AssertionError('empty indexed patch was accepted')
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
        try:
            panel_salvage.salvage(salvage_repo, 'scratch-task', scratch_execution_repo)
        except ValueError as error:
            assert 'nothing to recover for %s' % scratch_attempt['attempt_id'] in str(error), error
        else:
            raise AssertionError('missing indexed patch metadata was not recorded as empty salvage')
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
        import panel_dispatch
        import terminal_panel
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

# On Windows, a newly committed executable must retain the mode recorded by HEAD.
executable_tree, executable_attempt = kept_fixture('executable')
git_executable = ['git', '-C', str(executable_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test']
(executable_tree / 'run.sh').write_text('#!/bin/sh\nprintf ok\n')
(executable_tree / 'run.sh').chmod(0o755)
subprocess.run(git_executable + ['add', 'run.sh'], check=True)
subprocess.run(git_executable + ['commit', '-qm', 'add executable'], check=True)
class WindowsOS:
    name = 'nt'

    def __getattr__(self, attribute):
        return getattr(os, attribute)

with patch.object(panel_salvage, 'os', WindowsOS()):
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
try:
    kept_salvage(clean_tree, clean_attempt, clean_record)
except ValueError as error:
    assert 'nothing to recover for att_clean' in str(error), error
else:
    raise AssertionError('clean kept worktree produced a patch')
assert clean_receipt.is_file(), 'clean kept worktree left no empty-salvage receipt'
assert drop_clean() == []
(clean_tree / 'late.md').write_text('written after inspection\n')
assert drop_clean() == clean_evidence, 'a dirtied checkout stopped counting as recovery evidence'
(clean_tree / 'late.md').unlink()
subprocess.run(['git', '-C', str(clean_tree), '-c', 'user.email=test@example.com', '-c', 'user.name=test',
                'commit', '-q', '--allow-empty', '-m', 'empty worker commit'], check=True)
assert drop_clean() == [], 'an empty worker commit changed the clean outcome'
# --continue stays refused after salvaging a dirty kept worktree; the refusal names the salvage patch.
import panel_dispatch
import terminal_panel
dirty_brief = temporary / 'dirty-salvage-brief.md'
dirty_brief.write_text('Continue safely.\n')
dirty_parent = {'attempt_id': 'dirty-parent', 'terminal': False, 'refs': {}}
dirty_worker = dict(committed_attempt, parent_attempt_id='dirty-parent',
                    refs=dict(committed_attempt['refs'], panel_access='write', panel_provider='codex',
                              native_session='fixture-session'))
def continue_dirty():
    with patch.dict(os.environ, salvage_env), patch.object(
            panel_results, '_query', side_effect=lambda repo, verb, *args:
            [committed_attempt] if verb == 'agent-events' else {'rows': [committed_record]}), \
            patch.object(terminal_panel, 'owning_attempt', return_value=[dirty_parent]), \
            patch.object(terminal_panel, 'events', return_value=json.dumps([dirty_parent, dirty_worker])), \
            patch.object(terminal_panel, '_rework_history', return_value=[dirty_worker]), \
            patch.object(terminal_panel, 'results', return_value={'rows': [{'calls': []}]}), \
            patch.object(terminal_panel, 'session_stored', return_value=True):
        try:
            panel_dispatch.continuation(
                salvage_repo, 'codex', 'main', 'committed-task', {'provider': 'codex'}, dirty_brief,
                {'OMS_ROOM_REPO': str(salvage_repo)}, work_repo=salvage_execution_repo, patch_reference={})
        except ValueError as error:
            return str(error)
    raise AssertionError('continue resumed stopped write work')
dirty_message = continue_dirty()
dirty_patch = committed['patch']
assert dirty_message.startswith('recoverable delegate evidence exists for task committed-task; run oms panel '
                                '--salvage committed-task --attempt att_committed and inspect: '), dirty_message
assert 'registered worktree: ' + str(committed_tree) in dirty_message, dirty_message
assert 'already salvaged as ' + dirty_patch in dirty_message and 'fresh dispatch' in dirty_message, dirty_message
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
