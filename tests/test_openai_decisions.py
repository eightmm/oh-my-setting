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
        for removed in (('--auto', 'feature', 'on'), ('--auto',)):
            result = self.run_cli(*removed)
            self.assertEqual(result.returncode, 2)
            self.assertIn('unrecognized arguments', result.stderr)

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
