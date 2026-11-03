"""Command-line client for structured OpenAI decisions."""
import argparse
import datetime
import decimal
import json
import math
import os
import re
from pathlib import Path
import re
import shlex
import sqlite3
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

INPUT_LIMIT = 4 * 1024 * 1024
AUTH_LIMIT = 4 * 1024
REQUEST_LIMIT = 8 * 1024 * 1024


class ClientError(Exception):
    pass


class CapError(ClientError):
    pass


def auth_path():
    config = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config')
    return config / 'oh-my-setting' / 'openai-decisions.key'


def config_path():
    return auth_path().parent / 'decide.json'


def read_config():
    try:
        data = json.loads(config_path().read_text(encoding='utf-8'))
    except FileNotFoundError:
        return {}
    if not isinstance(data, dict) or type(data.get('task-classify', False)) is not bool:
        raise ClientError('invalid automatic decisions config')
    return data


def auto_config(action):
    data = read_config()
    if action == 'status':
        try:
            get_auth()
            configured = True
        except (ClientError, OSError, UnicodeError):
            configured = False
        print('task-classify: %s; key: %s' % ('on' if data.get('task-classify', False) else 'off',
                                              'configured' if configured else 'not configured'))
        return
    data['task-classify'] = action == 'on'
    path = config_path()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.decide-', dir=str(path.parent))
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
            json.dump(data, handle)
            handle.write('\n')
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def classify_task():
    try:
        if not read_config().get('task-classify', False):
            return
        get_auth()
        from panel_routing import policy, TIERS
        roles = policy()['roles']
        questions = [
            {'name': 'role', 'type': 'choice', 'instructions': 'Choose the best panel role.',
             'choices': [{'value': role, 'description': role} for role in roles]},
            {'name': 'workload', 'type': 'choice',
             'instructions': 'Choose task complexity: light is a mechanical edit with an exact spec; routine is a bounded reversible change; deep is multi-file or unclear logic; frontier is architecture or a trust boundary.',
             'choices': [{'value': tier, 'description': tier} for tier in TIERS]},
        ]
        task = json.dumps({'title': os.environ.get('OMS_TITLE', ''),
                           'allowed_paths': [x for x in re.split(r'[,\s]+', os.environ.get('OMS_ALLOWED', '')) if x],
                           'verify': os.environ.get('OMS_VERIFY', '')})
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'questions.json'
            path.write_text(json.dumps(questions), encoding='utf-8')
            result = subprocess.run([sys.executable, __file__, '--input', task, '--questions-file', str(path),
                                     '--json', '--timeout', '10'], capture_output=True, text=True, timeout=14)
        if result.returncode:
            category = {3: 'refusal', 4: 'cap'}.get(result.returncode, 'request')
            raise ClientError(category)
        answers = {row['name']: row for row in json.loads(result.stdout)}
        role, workload = answers['role'], answers['workload']
        if role['choice'] not in roles or workload['choice'] not in TIERS:
            raise ValueError('choice')
        probabilities = [role['confidence'], workload['confidence']]
        if any(isinstance(p, bool) or not isinstance(p, (float, int)) or not math.isfinite(p) or not 0 <= p <= 1 for p in probabilities):
            raise ValueError('confidence')
        print(json.dumps({'role': role['choice'], 'workload': workload['choice'],
                          'role_probability': probabilities[0], 'workload_probability': probabilities[1],
                          'source': 'openai-decisions', 'model': 'gpt-6-luna'}))
    except subprocess.TimeoutExpired:
        print('plan: role suggestion skipped (timeout)', file=sys.stderr)
    except (ClientError, OSError, ValueError, TypeError, KeyError, UnicodeError) as exc:
        category = str(exc) if str(exc) in ('refusal', 'cap', 'request') else 'unavailable'
        print('plan: role suggestion skipped (%s)' % category, file=sys.stderr)


def validate_auth(value):
    if len(value.encode('utf-8')) > AUTH_LIMIT:
        raise ClientError('key exceeds 4 KiB limit')
    if not re.fullmatch(r'[A-Za-z0-9._-]+', value):
        raise ClientError('key must contain only letters, digits, dot, underscore or hyphen')
    return value


def read_bounded(handle, limit, label):
    data = handle.read(limit + 1)
    if len(data) > limit:
        raise ClientError(label + ' exceeds ' + ('4 KiB' if limit == AUTH_LIMIT else '4 MiB') + ' limit')
    return data.decode('utf-8')


def owned(info):
    return not hasattr(os, 'getuid') or info.st_uid == os.getuid()


def file_issue(info, path):
    name = shlex.quote(str(path))
    if not stat.S_ISREG(info.st_mode):
        return 'replace %s with a regular non-symlink file owned by the current user; chmod 600 %s' % (name, name)
    if not owned(info):
        return 'change the owner of %s to the current user; chmod 600 %s' % (name, name)
    if stat.S_IMODE(info.st_mode) != 0o600:
        return 'run chmod 600 %s' % name
    return None


def check_private(info, path):
    issue = file_issue(info, path)
    if issue:
        raise ClientError(issue)


def check_directory(path, create=False):
    name = shlex.quote(str(path))
    if create:
        try:
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
        except FileExistsError:
            pass
        except OSError:
            raise ClientError('verify %s is a real directory owned by the current user; chmod 700 %s' % (name, name)) from None
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or not owned(info):
        raise ClientError('replace %s with a real directory owned by the current user; chmod 700 %s' % (name, name))
    if create and stat.S_IMODE(info.st_mode) != 0o700:
        try:
            fd = os.open(str(path), os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_DIRECTORY', 0))
        except OSError:
            raise ClientError('verify the directory %s is owned by the current user; chmod 700 %s' % (name, name)) from None
        try:
            current = os.fstat(fd)
            if not stat.S_ISDIR(current.st_mode) or not owned(current) or (current.st_dev, current.st_ino) != (info.st_dev, info.st_ino):
                raise ClientError('verify the directory %s is owned by the current user; chmod 700 %s' % (name, name))
            try:
                os.fchmod(fd, 0o700)
            except OSError:
                raise ClientError('verify the directory %s is owned by the current user; chmod 700 %s' % (name, name)) from None
        finally:
            os.close(fd)
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or not owned(info) or stat.S_IMODE(info.st_mode) != 0o700:
            raise ClientError('verify the directory %s is owned by the current user; chmod 700 %s' % (name, name))


def check_target(path):
    try:
        check_private(path.lstat(), path)
    except FileNotFoundError:
        pass


def prompt_auth():
    if sys.stdin.isatty():
        try:
            import termios
        except ImportError:
            raise ClientError('cannot disable terminal echo') from None
        try:
            fd = sys.stdin.fileno()
            original = termios.tcgetattr(fd)
            settings = original[:]
            settings[3] &= ~termios.ECHO
            termios.tcsetattr(fd, termios.TCSAFLUSH, settings)
        except (termios.error, OSError, ValueError):
            raise ClientError('cannot disable terminal echo') from None
        try:
            print('Decisions key: ', **{'end': '', 'file': sys.stderr, 'flush': True})
            data = sys.stdin.buffer.readline(AUTH_LIMIT + 3).removesuffix(b'\n').removesuffix(b'\r')
            if len(data) > AUTH_LIMIT:
                raise ClientError('key exceeds 4 KiB limit')
            return data.decode('utf-8')
        finally:
            try:
                termios.tcsetattr(fd, termios.TCSAFLUSH, original)
            except termios.error:
                raise ClientError('cannot restore terminal settings') from None
            finally:
                print(file=sys.stderr)
    # One byte past a CRLF-terminated maximal key exposes trailing input.
    data = sys.stdin.buffer.read(AUTH_LIMIT + 3)
    data = data.removesuffix(b'\n').removesuffix(b'\r')
    if len(data) > AUTH_LIMIT:
        raise ClientError('key exceeds 4 KiB limit')
    return data.decode('utf-8')


def save_auth():
    value = prompt_auth()
    validate_auth(value)
    path = auth_path()
    check_target(path)
    check_directory(path.parent, create=True)
    fd, name = tempfile.mkstemp(prefix='.decisions-', dir=str(path.parent))
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        check_directory(path.parent, create=True)
        check_target(path)
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)
    print('key saved')


def get_auth():
    value = os.environ.get(
        'OMS_OPENAI_DECISIONS_KEY')
    if value is not None:
        return validate_auth(value)
    path = auth_path()
    try:
        check_directory(path.parent)
        check_private(path.lstat(), path)
        fd = os.open(str(path), os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    except FileNotFoundError:
        raise ClientError('key is not set at %s; run oms decide --set-key' % shlex.quote(str(path))) from None
    except OSError:
        name = shlex.quote(str(path))
        raise ClientError('verify %s is a regular non-symlink file owned by the current user; chmod 600 %s' % (name, name)) from None
    with os.fdopen(fd, 'rb') as handle:
        check_private(os.fstat(handle.fileno()), path)
        return validate_auth(handle.read(AUTH_LIMIT + 3).decode('utf-8').removesuffix('\n').removesuffix('\r'))


def question_list(value):
    if not isinstance(value, list) or not 1 <= len(value) <= 400:
        raise ClientError('questions must contain 1 to 400 entries')
    names = set()
    for item in value:
        if not isinstance(item, dict):
            raise ClientError('each question must be an object')
        name, kind, instructions = item.get('name'), item.get('type'), item.get('instructions')
        if not isinstance(name, str) or not name.strip() or name in names:
            raise ClientError('question names must be non-empty and unique')
        names.add(name)
        if kind not in ('choice', 'predicate', 'score') or not isinstance(instructions, str) or not instructions.strip():
            raise ClientError('question type or instructions are invalid')
        if kind in ('choice', 'score'):
            field, label = ('choices', 'value') if kind == 'choice' else ('levels', 'label')
            entries = item.get(field)
            if not isinstance(entries, list) or len(entries) < 2 or (kind == 'score' and len(entries) > 10):
                raise ClientError('question choices or levels are invalid')
            seen = set()
            for entry in entries:
                if not isinstance(entry, dict) or not isinstance(entry.get(label), str) or not entry[label].strip() or not isinstance(entry.get('description'), str) or not entry['description'].strip() or entry[label] in seen:
                    raise ClientError('question choices or levels are invalid')
                seen.add(entry[label])
    return value


def base_url():
    value = os.environ.get('OMS_OPENAI_BASE_URL', 'https://api.openai.com/v1')
    parsed = urllib.parse.urlsplit(value)
    if parsed.username or parsed.password or parsed.query or parsed.fragment or not parsed.hostname:
        raise ClientError('invalid base URL')
    if value != 'https://api.openai.com/v1' and not (parsed.scheme in ('http', 'https') and parsed.hostname in ('127.0.0.1', '::1', 'localhost')):
        raise ClientError('base URL must be the production API or loopback')
    return value.rstrip('/') + '/decisions'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def reserve(body, cap):
    # Request bytes plus framing allowance are an estimate, not a tokenizer guarantee.
    estimate = len(body) + 1024
    root = Path(os.environ.get('XDG_STATE_HOME') or Path.home() / '.local' / 'state') / 'oh-my-setting'
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = root / 'decisions.sqlite3'
    db = sqlite3.connect(str(path), timeout=30)
    try:
        db.execute('CREATE TABLE IF NOT EXISTS calls (day TEXT, units INTEGER, cost REAL)')
        db.execute('BEGIN IMMEDIATE')
        day = datetime.date.today().isoformat()
        spent = db.execute('SELECT COALESCE(SUM(cost), 0) FROM calls WHERE day = ?', (day,)).fetchone()[0]
        cost = estimate / 10000000
        if decimal.Decimal(str(spent)) + decimal.Decimal(str(cost)) > cap:
            raise CapError('daily cap OMS_DECIDE_DAILY_USD would be exceeded')
        row = db.execute('INSERT INTO calls VALUES (?, ?, ?)', (day, estimate, cost)).lastrowid
        db.commit()
        return db, row
    except BaseException:
        db.close()
        raise


def run():
    parser = argparse.ArgumentParser(description='Ask the OpenAI Decisions API. The daily cap is a cooperative estimate: the state directory and OMS_DECIDE_DAILY_USD are caller-controlled.')
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--set-key', **{'action': 'store_true'})
    group.add_argument('--key-status', **{'action': 'store_true'})
    group.add_argument('--auto', nargs=2, metavar=('FEATURE', 'ACTION'))
    group.add_argument('--classify-task', action='store_true', help=argparse.SUPPRESS)
    inputs = parser.add_mutually_exclusive_group()
    inputs.add_argument('--input')
    inputs.add_argument('--input-file')
    questions = parser.add_mutually_exclusive_group()
    questions.add_argument('--questions-file')
    questions.add_argument('--choice')
    questions.add_argument('--predicate')
    parser.add_argument('--option', action='append', default=[])
    parser.add_argument('--instructions')
    parser.add_argument('--json', action='store_true')
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--timeout', type=float, default=60)
    args = parser.parse_args()
    if args.auto:
        if args.auto[0] != 'task-classify' or args.auto[1] not in ('on', 'off', 'status'):
            raise ClientError('expected --auto task-classify on|off|status')
        auto_config(args.auto[1])
        return 0
    if args.classify_task:
        classify_task()
        return 0
    if args.set_key:
        save_auth()
        return 0
    if args.key_status:
        path = auth_path()
        try:
            check_directory(path.parent)
            info = path.lstat()
            issue = file_issue(info, path)
            if not stat.S_ISREG(info.st_mode):
                raise ClientError(issue)
            print('present %04o%s' % (stat.S_IMODE(info.st_mode), ' (unsafe: %s)' % issue if issue else ''))
            if issue:
                return 1
        except FileNotFoundError:
            print('absent')
        return 0
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise ClientError('timeout must be positive and finite')
    if args.input is None and args.input_file is None:
        raise ClientError('input is required')
    input_value = args.input
    if args.input_file is not None:
        try:
            if args.input_file == '-':
                input_value = read_bounded(sys.stdin.buffer, INPUT_LIMIT, 'stdin')
            else:
                with open(args.input_file, 'rb') as handle:
                    input_value = read_bounded(handle, INPUT_LIMIT, '--input-file')
        except OSError:
            raise ClientError('cannot read --input-file') from None
        try:
            parsed = json.loads(input_value)
            if isinstance(parsed, list):
                input_value = parsed
        except ValueError:
            pass
    if isinstance(input_value, list) and any(not isinstance(message, dict) or not isinstance(message.get('role'), str) or 'content' not in message for message in input_value):
        raise ClientError('input message array is invalid')
    if not input_value or not isinstance(input_value, (str, list)):
        raise ClientError('input must be a non-empty string or message array')
    if args.questions_file:
        if args.option or args.instructions is not None:
            raise ClientError('shorthand options cannot accompany questions file')
        try:
            with open(args.questions_file, 'rb') as handle:
                question_text = read_bounded(handle, INPUT_LIMIT, '--questions-file')
        except OSError:
            raise ClientError('cannot read --questions-file') from None
        rows = json.loads(question_text)
    else:
        if not (args.choice or args.predicate):
            raise ClientError('questions are required')
        row = {'name': args.choice or args.predicate, 'type': 'choice' if args.choice else 'predicate', 'instructions': args.instructions}
        if args.choice:
            choices = []
            for option in args.option:
                if '=' not in option:
                    raise ClientError('option must be VALUE=DESC')
                value, description = option.split('=', 1)
                choices.append({'value': value, 'description': description})
            row['choices'] = choices
        elif args.option:
            raise ClientError('options require a choice question')
        rows = [row]
    body = json.dumps({'model': 'gpt-6-luna', 'input': input_value, 'questions': question_list(rows)}, ensure_ascii=True).encode()
    if len(body) > REQUEST_LIMIT:
        raise ClientError('serialized request exceeds 8 MiB limit')
    if args.dry_run:
        print(body.decode())
        return 0
    url = base_url()
    try:
        cap = decimal.Decimal(os.environ.get('OMS_DECIDE_DAILY_USD', '1.00'))
        if not cap.is_finite() or cap < 0:
            raise ValueError()
    except (decimal.InvalidOperation, ValueError):
        raise ClientError('OMS_DECIDE_DAILY_USD must be a non-negative finite number') from None
    private = get_auth()
    db, rowid = reserve(body, cap)
    try:
        request = urllib.request.Request(url, data=body, headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + private}, method='POST')
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect).open(request, timeout=args.timeout) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
                if len(raw) > 8 * 1024 * 1024:
                    raise ClientError('response is too large')
                result = json.loads(raw)
                if not isinstance(result, dict):
                    raise ClientError('invalid response')
        except urllib.error.HTTPError as exc:
            try:
                message = json.loads(exc.read(65536)).get('error', {}).get('message', '')
                if not isinstance(message, str):
                    message = ''
            except (ValueError, AttributeError):
                message = ''
            raise ClientError('HTTP %d: %s' % (exc.code, message.replace(private, '[redacted]'))) from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise ClientError('request failed') from None
        usage = result.get('usage', {})
        if not isinstance(usage, dict):
            raise ClientError('invalid usage in response')
        units = usage.get(
            'input_tokens')
        details = usage.get(
            'input_tokens_details', {})
        if not isinstance(details, dict):
            raise ClientError('invalid usage in response')
        cached = details.get(
            'cached_tokens', 0)
        if not isinstance(units, int) or isinstance(units, bool) or not isinstance(cached, int) or isinstance(cached, bool) or not 0 <= cached <= units:
            raise ClientError('invalid usage in response')
        db.execute('UPDATE calls SET units = ?, cost = ? WHERE rowid = ?', (units, (units - cached) / 10000000, rowid))
        db.commit()
        answers = result.get('answers')
        expected = {item['name']: item['type'] for item in rows}
        if (not isinstance(answers, list) or len(answers) != len(expected) or
                any(not isinstance(a, dict) or a.get('name') not in expected or
                    a.get('type') not in (expected.get(a.get('name')), 'refusal') for a in answers) or
                len({a['name'] for a in answers}) != len(expected)):
            raise ClientError('invalid answers in response')
        if args.json:
            output = json.dumps(answers, ensure_ascii=True)
        else:
            lines = []
            for answer in answers:
                kind = answer['type']
                value = answer.get({'choice': 'choice', 'predicate': 'probability', 'score': 'score', 'refusal': 'type'}[kind], '')
                lines.append('%s\t%s\t%s' % (answer.get('name', ''), value, answer.get('confidence', '')))
            output = '\n'.join(lines)
        print(output.replace(private, '[redacted]'))
        return 3 if any(a['type'] == 'refusal' for a in answers) else 0
    finally:
        db.close()


def main():
    try:
        return run()
    except CapError as exc:
        print('error: ' + str(exc), file=sys.stderr)
        return 4
    except (ClientError, OSError, ValueError, sqlite3.Error, TypeError) as exc:
        # Filesystem and parser errors can contain caller-controlled data.
        message = str(exc) if isinstance(exc, ClientError) else 'invalid input or local state'
        print('error: ' + message, file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
