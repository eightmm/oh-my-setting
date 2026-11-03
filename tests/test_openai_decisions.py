import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
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
        result = self.run_cli(
            '--set-key', **{'stdin': self.private + '\n'})
        self.assertEqual(result.returncode, 0, result.stderr)
        path = Path(self.tmp.name) / 'config/oh-my-setting'
        path = path / (
            'openai-decisions.key')
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        status = self.run_cli(
            '--key-status')
        self.assertIn('0600', status.stdout)
        self.assertNotIn(self.private, status.stdout + status.stderr)
        path.chmod(0o644)
        result = self.run_cli(*self.question())
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(Handler.requests, [])
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


if __name__ == '__main__':
    unittest.main()
