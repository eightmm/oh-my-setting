"""Command-line client for structured OpenAI decisions."""
import argparse
import datetime
import decimal
import getpass
import json
import math
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request


class ClientError(Exception):
    pass


class CapError(ClientError):
    pass


def auth_path():
    config = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config')
    return config / 'oh-my-setting' / 'openai-decisions.key'


def validate_auth(value):
    if not value or not value.isascii() or any(ord(c) < 33 or ord(c) > 126 for c in value):
        raise ClientError('key must be a single non-empty printable token')
    return value


def prompt_auth():
    return getpass.getpass('Decisions key: ') if sys.stdin.isatty() else sys.stdin.read().removesuffix('\n').removesuffix('\r')


def save_auth():
    value = prompt_auth()
    validate_auth(value)
    path = auth_path()
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd, name = tempfile.mkstemp(prefix='.decisions-', dir=str(path.parent))
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
            handle.write(value + '\n')
            handle.flush()
            os.fsync(handle.fileno())
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
        mode = path.stat().st_mode
    except FileNotFoundError:
        raise ClientError('key is not set; run oms decide --set-key') from None
    if mode & 0o077:
        raise ClientError('key file must not be readable by group or others')
    return validate_auth(path.read_text(encoding='utf-8').rstrip('\r\n'))


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
    if parsed.scheme != 'https' and not (parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1', 'localhost')):
        raise ClientError('base URL requires HTTPS or loopback HTTP')
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
    parser = argparse.ArgumentParser(description='Ask the OpenAI Decisions API.')
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--set-key', **{'action': 'store_true'})
    group.add_argument('--key-status', **{'action': 'store_true'})
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
    if args.set_key:
        save_auth()
        return 0
    if args.key_status:
        path = auth_path()
        print('present %04o' % (path.stat().st_mode & 0o777) if path.exists() else 'absent')
        return 0
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise ClientError('timeout must be positive and finite')
    if args.input is None and args.input_file is None:
        raise ClientError('input is required')
    input_value = args.input
    if args.input_file is not None:
        try:
            input_value = sys.stdin.read() if args.input_file == '-' else Path(args.input_file).read_text(encoding='utf-8')
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
            question_text = Path(args.questions_file).read_text(encoding='utf-8')
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
