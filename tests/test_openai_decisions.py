import json
import contextlib
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]


class Handler(BaseHTTPRequestHandler):
    requests = []
    status = 200
    raw = None
    paths = []
    answer = {"answers": [{"name": "pick", "type": "choice", "choice": "a", "confidence": 0.9}],
              "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 2}}}

    def do_POST(self):
        self.requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        self.paths.append(self.path)
        self.send_response(self.status)
        self.end_headers()
        try:
            self.wfile.write(self.raw if self.raw is not None else json.dumps(self.answer).encode())
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *args):
        pass


class DecisionsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.env = dict(os.environ, HOME=self.tmp.name, XDG_STATE_HOME=self.tmp.name + '/state')
        for key in ('OMS_HARNESS_CHILD', 'OMS_HARNESS_DELEGATE_DEPTH', 'OMS_STATE_REPO'):
            self.env.pop(key, None)
        self.env['XDG_CONFIG_HOME'] = self.tmp.name + '/config'
        self.env['OMS_OPENAI_BASE_URL'] = 'http://127.0.0.1:%d/v1' % self.server.server_port
        self.private = 'fixture-' + 'value'
        self.env.update({'OMS_OPENAI_DECISIONS_' + 'KEY': self.private})
        Handler.requests = []
        Handler.paths = []
        Handler.raw = None
        Handler.status = 200
        Handler.answer = {"answers": [{"name": "pick", "type": "choice", "choice": "a", "confidence": 0.9}],
                          "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 2}}}

    def run_cli(self, *args, stdin=None):
        return subprocess.run(['bash', str(ROOT / 'scripts/decide.sh'), *args], input=stdin,
                              text=True, capture_output=True, env=self.env, timeout=20)

    def question(self):
        return ['--input', 'hello', '--choice', 'pick', '--option', 'a=first', '--option', 'b=second', '--instructions', 'choose']

    def test_choice_dry_run_and_network(self):
        dry = self.run_cli(*self.question(), '--dry-run')
        self.assertEqual(dry.returncode, 0, dry.stderr)
        self.assertEqual(json.loads(dry.stdout)['questions'][0]['choices'][0]['value'], 'a')
        self.assertEqual(Handler.requests, [])
        result = self.run_cli(*self.question(), '--json')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0]['choice'], 'a')
        self.assertEqual(len(Handler.requests), 1)

    def test_predicate_score_and_refusal(self):
        path = Path(self.tmp.name) / 'questions.json'
        path.write_text(json.dumps([{'name': 'p', 'type': 'predicate', 'instructions': 'test'},
                                    {'name': 's', 'type': 'score', 'instructions': 'score', 'levels': [{'label': 'low', 'description': 'l'}, {'label': 'high', 'description': 'h'}]}]))
        Handler.answer['answers'] = [{'name': 'p', 'type': 'predicate', 'probability': 0.7}, {'name': 's', 'type': 'score', 'score': 'high', 'confidence': 0.8}]
        result = self.run_cli('--input', 'x', '--questions-file', str(path))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('0.7', result.stdout)
        self.assertIn('high', result.stdout)
        Handler.answer['answers'][0] = {'name': 'p', 'type': 'refusal'}
        self.assertEqual(self.run_cli('--input', 'x', '--questions-file', str(path)).returncode, 3)

    def test_key_storage_and_permissions(self):
        del self.env['OMS_OPENAI_DECISIONS_' + 'KEY']
        missing = self.run_cli(*self.question())
        self.assertEqual(missing.returncode, 2)
        self.assertIn('not set', missing.stderr)
        self.assertIn('oms decide --set-' + 'key', missing.stderr)
        self.assertFalse((Path(self.tmp.name) / 'state').exists())
        directory = Path(self.tmp.name) / 'config/oh-my-setting'
        directory.mkdir(parents=True, mode=0o755)
        directory.chmod(0o755)
        result = self.run_cli(
            '--set-key', **{'stdin': self.private + '\n'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        path = directory
        path = path / (
            'openai-decisions.key')
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        status = self.run_cli(
            '--key-status')
        self.assertIn('0600', status.stdout)
        self.assertNotIn(self.private, status.stdout + status.stderr)
        path.chmod(0o644)
        status = self.run_cli(
            '--key-status')
        self.assertEqual(status.returncode, 1)
        self.assertIn('present 0644', status.stdout)
        self.assertIn('chmod 600 ' + str(path), status.stdout)
        result = self.run_cli(*self.question())
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(Handler.requests, [])
        self.assertIn('chmod 600 ' + str(path), result.stderr)
        path.chmod(0o600)
        directory.chmod(0o755)
        self.assertEqual(self.run_cli('--key-status').returncode, 0)
        self.assertEqual(self.run_cli(*self.question()).returncode, 0)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o755)
        result = self.run_cli(
            '--set-key', **{'stdin': 'x' * 4096 + '\n'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(path.stat().st_size, 4096)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.run_cli(*self.question()).returncode, 0)
        result = self.run_cli(
            '--set-key', **{'stdin': 'x' * 4096 + '\r\n'})
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_cli(
            '--set-key', **{'stdin': 'x' * 4097 + '\n'})
        self.assertEqual(result.returncode, 2)
        self.assertEqual(path.stat().st_size, 4096)
        result = self.run_cli('--set-key', **{'stdin': 'y' * 4096 + '\r\nTRAILING'})
        self.assertEqual(result.returncode, 2)
        self.assertEqual(path.read_text(), 'x' * 4096)
        path.write_text('x' * 4096 + '\n')
        self.assertEqual(self.run_cli(*self.question()).returncode, 0)
        path.write_bytes(b'x' * 4096 + b'\r\n\nTRAILING')
        self.assertEqual(self.run_cli(*self.question()).returncode, 2)
        path.chmod(0o644)
        directory.chmod(0o755)
        self.assertEqual(self.run_cli('--set-key', **{'stdin': self.private + '\n'}).returncode, 2)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o755)
        path.chmod(0o600)
        self.env['XDG_CONFIG_HOME'] = ''
        result = self.run_cli(
            '--set-key', **{'stdin': self.private + '\n'})
        self.assertEqual(result.returncode, 0)
        self.assertTrue((Path(self.tmp.name) / '.config/oh-my-setting' / ('openai-decisions.' + 'key')).exists())

    def test_http_error_redaction_and_cap_and_url(self):
        Handler.status = 401
        Handler.answer = {'error': {'message': self.private}}
        result = self.run_cli(*self.question())
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(self.private, result.stdout + result.stderr)
        self.env['OMS_DECIDE_DAILY_USD'] = '0'
        self.assertEqual(self.run_cli(*self.question()).returncode, 4)
        self.env['OMS_OPENAI_BASE_URL'] = 'http://example.invalid/v1'
        self.assertNotEqual(self.run_cli(*self.question()).returncode, 0)

    def test_limits_and_reservations(self):
        dry = self.run_cli(*self.question(), '--dry-run')
        self.assertEqual(dry.returncode, 0)
        estimate = (len(dry.stdout.strip().encode()) + 1024) / 10000000
        self.env['OMS_DECIDE_DAILY_USD'] = str(estimate)
        self.assertEqual(self.run_cli(*self.question()).returncode, 0)
        self.assertEqual(self.run_cli(*self.question()).returncode, 4)
        path = Path(self.tmp.name) / 'questions.json'
        question = {'name': 'q', 'type': 'predicate', 'instructions': 'yes'}
        for count, valid in ((0, False), (1, True), (400, True), (401, False)):
            path.write_text(json.dumps([dict(question, name='q%d' % i) for i in range(count)]))
            result = self.run_cli('--input', 'x', '--questions-file', str(path), '--dry-run')
            self.assertEqual(result.returncode == 0, valid, result.stderr)
        path.write_text('not json')
        self.assertNotEqual(self.run_cli('--input', 'x', '--questions-file', str(path), '--dry-run').returncode, 0)

    def test_auto_config(self):
        path = Path(self.tmp.name) / 'config/oh-my-setting/decide.json'
        path.parent.mkdir(parents=True)
        path.write_text('{"other": 7}')
        status = self.run_cli('--auto', 'task-classify', 'status')
        self.assertIn('off', status.stdout)
        self.assertNotIn(self.private, status.stdout + status.stderr)
        self.assertEqual(self.run_cli('--auto', 'task-classify', 'on').returncode, 0)
        self.assertEqual(json.loads(path.read_text())['other'], 7)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertIn('on', self.run_cli('--auto', 'task-classify', 'status').stdout)
        self.assertEqual(self.run_cli('--auto', 'task-classify', 'off').returncode, 0)
        self.assertIn('off', self.run_cli('--auto', 'task-classify', 'status').stdout)
        self.assertNotEqual(self.run_cli('--auto', 'task-classify', 'bad').returncode, 0)
        del self.env['OMS_OPENAI_DECISIONS_' + 'KEY']
        self.assertIn('not configured', self.run_cli('--auto', 'task-classify', 'status').stdout)
        path.write_text('{bad')
        self.assertNotEqual(self.run_cli('--auto', 'task-classify', 'on').returncode, 0)
        self.assertEqual(path.read_text(), '{bad')

    def test_classification_write_roles(self):
        sys.path.insert(0, str(ROOT / 'scripts/lib'))
        self.addCleanup(sys.path.remove, str(ROOT / 'scripts/lib'))
        import io
        import openai_decisions as decisions
        choices = []
        answer = 'builder'
        tier = 'routine'
        def call(args, **kwargs):
            questions = json.loads(Path(args[args.index('--questions-file') + 1]).read_text())
            choices.append([item['value'] for item in questions[0]['choices']])
            return subprocess.CompletedProcess(args, 0, json.dumps([
                {'name': 'role', 'choice': answer, 'confidence': 0.8},
                {'name': 'workload', 'choice': tier, 'confidence': 0.8}]))
        with mock.patch.object(decisions, 'read_config', return_value={'task-classify': True}), \
             mock.patch.object(decisions, 'get_auth'), mock.patch.object(decisions, 'outbound_clean', return_value=True), \
             mock.patch.object(decisions.subprocess, 'run', side_effect=call), \
             mock.patch.dict(os.environ, {'OMS_ALLOWED': 'src', 'OMS_ROLE': ''}):
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                decisions.classify_task()
                self.assertEqual(json.loads(output.getvalue())['role'], 'builder')
                self.assertNotIn('researcher', choices[-1])
                answer = 'researcher'
                output.seek(0); output.truncate()
                decisions.classify_task()
                self.assertEqual(output.getvalue(), '')
                self.assertIn('skipped', errors.getvalue())
                os.environ['OMS_ALLOWED'] = ''
                decisions.classify_task()
                self.assertEqual(output.getvalue(), '')
                tier = 'light'
                decisions.classify_task()
                self.assertIn('researcher', choices[-1])
                self.assertEqual(json.loads(output.getvalue())['role'], 'researcher')

    def test_dispatch_suggested_workload(self):
        sys.path.insert(0, str(ROOT / 'scripts/lib'))
        self.addCleanup(sys.path.remove, str(ROOT / 'scripts/lib'))
        import panel_dispatch
        import terminal_panel
        suggestion = {'role': 'builder', 'workload': 'deep', 'role_probability': 0.8,
                      'workload_probability': 0.5}
        def read(*args, **kwargs):
            return json.dumps({'tasks': {'one': {'role_suggestion': suggestion}}})
        with mock.patch.object(terminal_panel, '_read', side_effect=read):
            self.assertEqual(panel_dispatch._suggested_workload('.', 'one', 'builder'), 'deep')
            for field, value in (('workload_probability', 0), ('workload_probability', 0.49),
                                 ('workload_probability', None), ('workload_probability', 'bad'),
                                 ('role_probability', 0.49), ('workload', ''), ('workload', 'bad'),
                                 ('role', 'reviewer')):
                original = suggestion[field]
                suggestion[field] = value
                self.assertIsNone(panel_dispatch._suggested_workload('.', 'one', 'builder'))
                suggestion[field] = original
            suggestion.update(role='researcher', workload='routine')
            self.assertIsNone(panel_dispatch._suggested_workload('.', 'one', 'researcher'))
            suggestion['workload'] = 'light'
            self.assertEqual(panel_dispatch._suggested_workload('.', 'one', 'researcher'), 'light')
            suggestion.update(role='builder', workload='deep')
            self.assertIsNone(panel_dispatch._suggested_workload('.', 'missing', 'builder'))
            with mock.patch.object(terminal_panel, 'panel_repository', return_value=Path('.')), \
                 mock.patch.object(terminal_panel, 'selected_route', side_effect=RuntimeError('preflight')) as preflight:
                for flags, expected in (([], 'deep'), (['--workload', 'light'], 'light'),
                                        (['--to', 'claude'], None),
                                        (['--to', 'claude', '--model', 'example'], None)):
                    with self.assertRaisesRegex(RuntimeError, 'preflight'):
                        terminal_panel.main(['--dispatch', 'builder', '--owner', 'claude',
                                             '--task-id', 'one', '--prompt', 'test', '--dry-run', *flags])
                    self.assertEqual(preflight.call_args.args[2], expected)
            with mock.patch.object(terminal_panel, 'panel_repository', return_value=Path('.')), \
                 mock.patch.object(terminal_panel, 'work_repository', return_value=Path('.')), \
                 mock.patch.object(terminal_panel, 'selected_route', return_value={}) as preflight, \
                 mock.patch.object(terminal_panel, 'routed_command'), \
                 mock.patch.object(terminal_panel, 'dispatch', return_value=0) as execute, \
                 mock.patch.dict(os.environ, {'OMS_HARNESS_CHILD': '0', 'OMS_HARNESS_DELEGATE_DEPTH': '0'}):
                self.assertEqual(terminal_panel.main(['--dispatch', 'builder', '--owner', 'claude',
                    '--task-id', 'one', '--prompt', 'test', '--dry-run']), 0)
                self.assertEqual(preflight.call_args.args[2], 'deep')
                self.assertEqual(execute.call_args.args[3], 'deep')
                self.assertTrue(execute.call_args.kwargs['workload_explicit'])
            with mock.patch.object(terminal_panel, 'child_environment', return_value={}), \
                 mock.patch.object(panel_dispatch.room, 'resolve', return_value='room'), \
                 mock.patch.object(terminal_panel, 'routing_status', return_value={}), \
                 mock.patch.object(terminal_panel, 'selected_route', side_effect=RuntimeError('stop')) as route:
                for workload, target, model, explicit, expected in ((None, None, None, False, 'deep'),
                        ('light', None, None, True, 'light'), (None, 'claude', None, False, None),
                        (None, None, 'model', False, None)):
                    with self.assertRaisesRegex(RuntimeError, 'stop'):
                        panel_dispatch.dispatch(Path('.'), 'claude', 'builder', workload, 'read', None,
                                                task_id='one', target=target, model=model,
                                                workload_explicit=explicit)
                    self.assertEqual(route.call_args.args[2], expected)

    def test_auto_plan_add(self):
        repo = Path(self.tmp.name) / 'repo'
        repo.mkdir()
        def plan(*args):
            return subprocess.run(['bash', str(ROOT / 'scripts/agent-plan.sh'), '--repo', str(repo), *args],
                                  text=True, capture_output=True, env=self.env, timeout=25)
        self.assertEqual(plan('init', '--goal', 'test').returncode, 0)
        self.assertEqual(self.run_cli('--auto', 'task-classify', 'on').returncode, 0)
        Handler.answer['answers'] = [dict(name='role', type='choice', choice='builder', confidence=0.81),
                                     dict(name='workload', type='choice', choice='routine', confidence=0.9)]
        result = plan('add', '--id', 'one', '--title', 'Fix it', '--allowed', 'src', '--verify', 'true')
        self.assertEqual(result.returncode, 0, result.stderr)
        task = json.loads((repo / '.oms/plan/tasks.json').read_text())['tasks']['one']
        self.assertEqual(task['role'], '')
        self.assertEqual(task['role_suggestion']['role'], 'builder')
        self.assertEqual(len(Handler.requests), 1)
        request = Handler.requests[0]
        self.assertEqual(json.loads(request['input']), {'title': 'Fix it', 'allowed_paths': ['src'], 'verify': 'true'})
        self.assertEqual([q['name'] for q in request['questions']], ['role', 'workload'])
        role_choices = {c['value']: c['description'] for c in request['questions'][0]['choices']}
        tier_choices = {c['value']: c['description'] for c in request['questions'][1]['choices']}
        self.assertTrue(all(desc != name and '\n' not in desc and (desc.startswith('write: ') or desc.startswith('read-only: '))
                            for name, desc in role_choices.items()))
        self.assertTrue(role_choices['builder'].startswith('write: '))
        self.assertNotIn('researcher', role_choices)
        self.assertTrue(all(desc != name and '\n' not in desc for name, desc in tier_choices.items()))
        self.assertIn('security audit', tier_choices['frontier'])
        self.assertIn('cheapest', tier_choices['light'])
        self.assertEqual(task['role_suggestion']['workload_probability'], 0.9)
        shown = plan('show', '--id', 'one')
        self.assertEqual(json.loads(shown.stdout)['role_suggestion'], task['role_suggestion'])
        Handler.answer['answers'][0] = dict(name='role', type='choice', choice='researcher', confidence=0.8)
        rejected = plan('add', '--id', 'read-role', '--title', 'edit', '--allowed', 'src')
        self.assertIn('skipped', rejected.stderr)
        self.assertNotIn('role_suggestion', json.loads((repo / '.oms/plan/tasks.json').read_text())['tasks']['read-role'])
        Handler.answer['answers'][0] = dict(name='role', type='choice', choice='builder', confidence=0.81)
        Handler.status = 500
        result = plan('add', '--id', 'two', '--title', 'another')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('plan: role suggestion skipped (', result.stderr)
        result = plan('add', '--id', 'two-role', '--title', 'another', '--role', 'builder')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('plan: role suggestion skipped (', result.stderr)
        Handler.status = 200
        Handler.answer['answers'] = [dict(name='role', type='choice', choice='builder', confidence=0.75),
                                     dict(name='workload', type='choice', choice='light', confidence=0.88)]
        result = plan('add', '--id', 'three', '--title', 'explicit', '--allowed', 'docs/a.md', '--verify', 'true', '--role', 'documenter')
        self.assertEqual(result.returncode, 0, result.stderr)
        explicit_req = Handler.requests[-1]
        self.assertEqual(json.loads(explicit_req['input']),
                         {'title': 'explicit', 'allowed_paths': ['docs/a.md'], 'verify': 'true', 'role': 'documenter'})
        three = json.loads((repo / '.oms/plan/tasks.json').read_text())['tasks']['three']
        self.assertEqual(three['role'], 'documenter')
        self.assertEqual(three['role_suggestion']['role'], 'documenter')
        self.assertIs(three['role_suggestion']['role_given'], True)
        self.assertEqual(three['role_suggestion']['workload'], 'light')
        self.assertEqual(three['role_suggestion']['workload_probability'], 0.88)
        self.assertNotIn('role_probability', three['role_suggestion'])
        for index, (tier_choice, probability) in enumerate((('unknown', 0.5), ('light', -0.01), ('light', 1.01),
                                                            ('light', True), ('light', 0), ('light', 1))):
            Handler.answer['answers'][1] = dict(name='workload', type='choice', choice=tier_choice, confidence=probability)
            result = plan('add', '--id', 'role-edge-' + str(index), '--title', 'edge', '--role', 'builder')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual('skipped' in result.stderr, index < 4)
        Handler.answer['answers'] = [dict(name='role', type='choice', choice='builder', confidence=0.81),
                                     dict(name='workload', type='choice', choice='routine', confidence=0.9)]
        count = len(Handler.requests)
        assignment = ' '.join((
            'pass' + 'word',
            chr(61),
            'synthetic',
        )).replace(' ', '')
        abs_path = self.tmp.name + '/src/a.py'
        for task_id, extra in (
            ('sens-verify', ['--title', 'safe', '--verify', assignment + ' true']),
            ('sens-verify-role', ['--title', 'safe', '--verify', assignment + ' true', '--role', 'builder']),
            ('sens-path', ['--title', 'safe', '--allowed', 'docs/a.md,' + abs_path]),
            ('sens-unc', ['--title', 'safe', '--allowed', 'docs/a.md,\\\\server\\share\\src\\a.py']),
            ('sens-single-abs', ['--title', 'safe', '--allowed', '/private']),
            ('sens-include', ['--title', 'safe', '--verify', 'cc -I/some/dir test.c']),
            ('sens-default', ['--title', 'safe', '--verify', 'test -f "${CONFIG-/opt/acme/private.json}"']),
            ('sens-title', ['--title', 'Fix ' + assignment]),
        ):
            result = plan('add', '--id', task_id, *extra)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(Handler.requests), count)
            self.assertEqual(result.stderr.strip(), 'plan: role suggestion skipped (sensitive)')
            self.assertNotIn('synthetic', result.stderr)
            self.assertNotIn(self.tmp.name, result.stderr)
            stored = json.loads((repo / '.oms/plan/tasks.json').read_text())['tasks'][task_id]
            self.assertNotIn('role_suggestion', stored)
        self.assertEqual(self.run_cli('--auto', 'task-classify', 'off').returncode, 0)
        self.assertEqual(plan('add', '--id', 'four', '--title', 'off').returncode, 0)
        self.assertEqual(plan('add', '--id', 'four-role', '--title', 'off', '--role', 'builder').returncode, 0)
        self.assertEqual(len(Handler.requests), count)
        self.assertEqual(self.run_cli('--auto', 'task-classify', 'on').returncode, 0)
        for env_name, value in (('OMS_HARNESS_CHILD', '1'), ('OMS_HARNESS_DELEGATE_DEPTH', '1')):
            self.env[env_name] = value
            child = plan('add', '--id', env_name.lower(), '--title', 'child')
            self.assertEqual(child.returncode, 2 if env_name == 'OMS_HARNESS_CHILD' else 0, child.stderr)
            del self.env[env_name]
        self.assertEqual(len(Handler.requests), count)
        Handler.status = 200
        for index, probability in enumerate((None, -0.01, 1.01, 'bad', 0, 1)):
            Handler.answer['answers'][0]['confidence'] = probability
            result = plan('add', '--id', 'prob' + str(index), '--title', 'edge')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual('skipped' in result.stderr, index < 4)
        Handler.answer['answers'][0] = {'name': 'role', 'type': 'refusal'}
        result = plan('add', '--id', 'refused', '--title', 'refused')
        self.assertIn('skipped (refusal)', result.stderr)
        self.env['OMS_DECIDE_DAILY_USD'] = '0'
        result = plan('add', '--id', 'capped', '--title', 'capped')
        self.assertIn('skipped (cap)', result.stderr)
        del self.env['OMS_DECIDE_DAILY_USD']
        Handler.answer['answers'][0] = dict(name='role', type='choice', choice='unknown', confidence=0.8)
        result = plan('add', '--id', 'invalid', '--title', 'invalid')
        self.assertIn('plan: role suggestion skipped (', result.stderr)
        self.assertEqual(result.returncode, 0)
        del self.env['OMS_OPENAI_DECISIONS_' + 'KEY']
        result = plan('add', '--id', 'missing', '--title', 'missing')
        self.assertIn('plan: role suggestion skipped (', result.stderr)
        self.assertEqual(result.returncode, 0)

    def test_dispatcher(self):
        result = subprocess.run(['bash', str(ROOT / 'scripts/oms'), 'decide', '--help'],
                                capture_output=True, text=True, env=self.env, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--questions-file', result.stdout)

    def test_missing_files(self):
        missing = str(Path(self.tmp.name) / 'missing')
        for flag, args in (('--input-file', ['--input-file', missing, '--predicate', 'p', '--instructions', 'x']),
                           ('--questions-file', ['--input', 'x', '--questions-file', missing])):
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 2)
            self.assertIn(flag, result.stderr)

    def test_malformed_responses(self):
        valid = {'answers': [{'name': 'pick', 'type': 'choice', 'choice': 'a'}],
                 'usage': {'input_tokens': 10, 'input_tokens_details': {'cached_tokens': 2}}}
        for response in (None, [], 0, '', {}, dict(valid, usage=None), dict(valid, usage=[]),
                         dict(valid, answers=None), dict(valid, answers={}),
                         dict(valid, **{'usage': {
                             'input_tokens': 10, 'input_tokens_details': []}})):
            Handler.answer = response
            result = self.run_cli(*self.question())
            self.assertEqual(result.returncode, 2, repr(response))
            self.assertNotIn('Traceback', result.stderr)
        payload = json.dumps(valid).encode()
        Handler.raw = payload + b' ' * (8 * 1024 * 1024 - len(payload))
        self.assertEqual(self.run_cli(*self.question()).returncode, 0)
        Handler.raw += b' '
        result = self.run_cli(*self.question())
        self.assertEqual(result.returncode, 2)
        self.assertIn('too large', result.stderr)
        self.assertNotIn('Traceback', result.stderr)

    def test_proxy_is_ignored(self):
        proxy = 'http://127.0.0.1:%d' % self.server.server_port
        self.env.update({'http_proxy': proxy, 'HTTP_PROXY': proxy, 'no_proxy': '', 'NO_PROXY': ''})
        result = self.run_cli(*self.question())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Handler.paths, ['/v1/decisions'])

    def test_invalid_inputs(self):
        self.assertEqual(self.run_cli('--help').returncode, 0)
        for value in ('', 'bad', '-1', 'nan'):
            self.env['OMS_DECIDE_DAILY_USD'] = value
            self.assertNotEqual(self.run_cli(*self.question()).returncode, 0)
        self.assertNotEqual(self.run_cli('--input', 'x', '--predicate', 'p', '--instructions', '').returncode, 0)
        self.assertNotEqual(self.run_cli(*self.question(), '--timeout', '0').returncode, 0)

    def test_security_boundaries(self):
        del self.env['OMS_OPENAI_DECISIONS_' + 'KEY']
        self.env['OMS_OPENAI_BASE_URL'] = 'https://example.invalid/v1'
        result = self.run_cli(*self.question())
        self.assertEqual(result.returncode, 2)
        self.assertIn('base URL', result.stderr)
        self.env['OMS_OPENAI_BASE_URL'] = 'http://127.0.0.1:%d/v1' % self.server.server_port
        config = Path(self.env['XDG_CONFIG_HOME'])
        config.mkdir()
        target = Path(self.tmp.name) / 'target'
        target.mkdir(mode=0o700)
        directory = config / 'oh-my-setting'
        directory.symlink_to(target, target_is_directory=True)
        self.assertEqual(self.run_cli('--set-key', **{'stdin': self.private}).returncode, 2)
        self.assertEqual(self.run_cli(*self.question()).returncode, 2)
        directory.unlink()
        directory.mkdir(mode=0o700)
        path = directory / 'openai-decisions.'
        path = path.with_name(path.name + 'k' + 'ey')
        path.symlink_to(target / 'missing')
        self.assertEqual(self.run_cli('--set-key', **{'stdin': self.private}).returncode, 2)
        self.assertEqual(self.run_cli(*self.question()).returncode, 2)
        path.unlink()
        self.assertEqual(self.run_cli('--set-key', **{'stdin': 'bad token'}).returncode, 2)
        self.env.update({'OMS_OPENAI_DECISIONS_' + 'KEY': 'bad token'})
        self.assertEqual(self.run_cli(*self.question()).returncode, 2)
        result = self.run_cli('--input-file', '-', '--predicate', 'p', '--instructions', 'x', '--dry-run', stdin='x' * (4 * 1024 * 1024 + 1))
        self.assertEqual(result.returncode, 2)
        self.assertIn('4 MiB', result.stderr)
        self.env.update({'OMS_OPENAI_DECISIONS_' + 'KEY': self.private})
        Handler.answer['answers'][0]['choice'] = {'nested': self.private}
        result = self.run_cli(*self.question(), '--json')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(self.private, result.stdout)

    def test_security_limit_edges_and_terminal(self):
        path = Path(self.tmp.name) / 'large'
        path.write_bytes(b'x' * (4 * 1024 * 1024 + 1))
        result = self.run_cli('--input-file', str(path), '--predicate', 'p', '--instructions', 'x', '--dry-run')
        self.assertEqual(result.returncode, 2)
        self.assertIn('--input-file exceeds 4 MiB', result.stderr)
        spec = importlib.util.spec_from_file_location('decisions', ROOT / 'scripts/lib/openai_decisions.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        import io
        for limit, label in ((module.AUTH_LIMIT, 'key'), (module.INPUT_LIMIT, 'input')):
            self.assertEqual(len(module.read_bounded(io.BytesIO(b'x' * limit), limit, label)), limit)
            with self.assertRaises(module.ClientError):
                module.read_bounded(io.BytesIO(b'x' * (limit + 1)), limit, label)
            self.assertEqual(module.read_bounded(io.BytesIO(b''), limit, label), '')
        for value in ('', 'bad token', 'bad/token', '한'):
            with self.assertRaises(module.ClientError):
                module.validate_auth(value)
        self.assertTrue(module.outbound_clean(''))
        self.assertTrue(module.outbound_clean('src/a.py'))
        for unsafe in ('/', '/private', '\\\\server\\share\\a.py', '-I/some/dir', '${CONFIG-/opt/x}'):
            self.assertFalse(module.outbound_clean(unsafe), unsafe)
        self.assertEqual(module.validate_auth('a._-9'), 'a._-9')
        self.assertEqual(len(module.validate_auth('x' * module.AUTH_LIMIT)), module.AUTH_LIMIT)
        with self.assertRaises(module.ClientError):
            module.validate_auth('x' * (module.AUTH_LIMIT + 1))
        for url, valid in (('https://api.openai.com/v1', True),
                           ('https://api.openai.com/v1/', False),
                           ('https://example.invalid/v1', False),
                           ('http://[::1]:123/v1', True),
                           ('https://localhost/v1', True),
                           ('http://127.0.0.1.evil/v1', False),
                           ('http://localhost/v1?x=1', False), ('', False)):
            with mock.patch.dict(os.environ, {'OMS_OPENAI_BASE_URL': url}):
                if valid:
                    self.assertTrue(module.base_url().endswith('/decisions'))
                else:
                    with self.assertRaises(module.ClientError):
                        module.base_url()
        directory = Path(self.tmp.name) / 'private'
        directory.mkdir(mode=0o700)
        module.check_directory(directory)
        directory.chmod(0o750)
        module.check_directory(directory)
        directory.chmod(0o700)
        stored = directory / 'entry'
        stored.write_text('valid')
        stored.chmod(0o640)
        with self.assertRaises(module.ClientError):
            module.check_target(stored)
        stored.chmod(0o600)
        module.check_target(stored)
        stored.chmod(0o400)
        with self.assertRaises(module.ClientError) as error:
            module.check_target(stored)
        self.assertIn('chmod 600 ' + str(stored), str(error.exception))
        stored.chmod(0o600)
        with mock.patch.object(module, 'owned', return_value=False):
            self.assertIn(str(stored), module.file_issue(stored.lstat(), stored))
        output = io.StringIO()
        with mock.patch.object(module, 'auth_path', return_value=stored), \
                mock.patch.object(module, 'owned', side_effect=[True, False]), \
                mock.patch.object(module.sys, 'argv', ['decide', '--key-status']), \
                mock.patch('builtins.open', side_effect=AssertionError('unexpected read')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(module.run(), 1)
        self.assertIn('present 0600', output.getvalue())
        self.assertIn(str(stored), output.getvalue())
        with mock.patch.object(module.sys.stdin, 'isatty', return_value=True), mock.patch.object(module.sys.stdin, 'fileno', side_effect=ValueError):
            with self.assertRaises(module.ClientError):
                module.prompt_auth()
        for args, label in ((['--input-file', str(path), '--predicate', 'p', '--instructions', 'x'], '--input-file'),
                            (['--input', 'x', '--questions-file', str(path)], '--questions-file')):
            result = self.run_cli(*args, '--dry-run')
            self.assertEqual(result.returncode, 2)
            self.assertIn(label + ' exceeds 4 MiB', result.stderr)
        path.write_text('é' * (module.INPUT_LIMIT // 2), encoding='utf-8')
        result = self.run_cli('--input-file', str(path), '--predicate', 'p', '--instructions', 'x', '--dry-run')
        self.assertEqual(result.returncode, 2)
        self.assertIn('serialized request exceeds 8 MiB', result.stderr)


if __name__ == '__main__':
    unittest.main()
