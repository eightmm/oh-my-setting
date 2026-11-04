#!/usr/bin/env bash
set -euo pipefail

# Preview a privacy-checked OMS bug report or feature request before publication.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec python3 -I -B -c "$(cat <<'PY'
import argparse
import importlib.util
import json
import ipaddress
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import subprocess
import sys
import tempfile
from urllib.parse import urlencode


def fail(reason):
    print('report: ' + reason, file=sys.stderr)
    raise SystemExit(2)


def run(args):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, check=False)


def confirm(prompt):
    try:
        return input(prompt).strip().lower() in ('y', 'yes')
    except EOFError:
        return False


if sys.version_info < (3, 11):
    fail('Python 3.11 or newer is required')
root = Path(sys.argv[1])
p = argparse.ArgumentParser(prog='oms report', description=(
    'Preview allowlisted OMS metadata and an optional description. Descriptions '
    'come only from --message or stdin; sensitive content is refused. '
    'Publication requires --yes or an interactive y. Otherwise print a new-issue URL.'))
p.add_argument('kind', choices=('bug', 'feature'))
p.add_argument('--from-failure', metavar='FINGERPRINT')
p.add_argument('--title')
p.add_argument('--message')
p.add_argument('--dry-run', action='store_true')
p.add_argument('--yes', action='store_true')
a = p.parse_args(sys.argv[2:])
if a.kind == 'feature' and (a.title is None or not a.title.strip()):
    fail('feature requires --title')
if a.kind == 'feature' and a.from_failure is not None:
    fail('--from-failure requires bug')
if a.from_failure is not None and not re.fullmatch(r'[a-f0-9]{16}', a.from_failure):
    fail('malformed failure fingerprint')
title = a.title.replace('\r\n', '\n') if a.title is not None else 'OMS bug report'
if not title.strip() or '\n' in title or '\r' in title:
    fail('title must be a nonempty single line')
message = a.message if a.message is not None else ('' if sys.stdin.isatty() else sys.stdin.read())
message = message.replace('\r\n', '\n')

spec = importlib.util.spec_from_file_location('github_remote', root / 'scripts/lib/github-remote.py')
remote_parser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remote_parser)
if 'OMS_REPORT_REPO' in os.environ:
    value = os.environ['OMS_REPORT_REPO']
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', value):
        fail('OMS_REPORT_REPO must be owner/name')
    repo = remote_parser.validate_slug(*value.split('/'))
else:
    origin = run(['git', '-C', str(root), 'remote', 'get-url', 'origin'])
    if origin.returncode:
        fail('OMS origin unavailable; set OMS_REPORT_REPO to owner/name')
    repo = remote_parser.normalize(origin.stdout.strip())

version = (root / 'VERSION').read_text().strip()
if not re.fullmatch(r'[0-9]+(?:\.[0-9]+){1,2}(?:[-+][A-Za-z0-9.-]+)?', version):
    fail('invalid OMS version')
commit = run(['git', '-C', str(root), 'rev-parse', '--short=12', 'HEAD']).stdout.strip()
if not re.fullmatch(r'[0-9a-f]{7,40}', commit):
    commit = 'unavailable'
os_family = 'macos' if sys.platform == 'darwin' else 'linux'
if 'microsoft' in platform.release().lower():
    os_family = 'wsl'
bash_version = run(['bash', '-c', 'printf "%s.%s" "${BASH_VERSINFO[0]}" "${BASH_VERSINFO[1]}"']).stdout
if not re.fullmatch(r'[0-9]+\.[0-9]+', bash_version) or int(bash_version.split('.')[0]) < 5:
    fail('Bash 5.0 or newer is required')
lines = [f'OMS: {version} ({commit})', f'OS: {os_family}',
         f'Bash: {bash_version}', f'Python: {sys.version_info.major}.{sys.version_info.minor}']
if a.from_failure:
    result = run(['bash', str(root / 'scripts/fail-ledger.sh'), 'show',
                  '--fingerprint', a.from_failure, '--json'])
    if result.returncode:
        fail('failure record unavailable')
    row = json.loads(result.stdout)['failure']
    # Command arguments and free-form ledger text never enter the draft.
    match = re.match(r'^oms ([a-z][a-z0-9-]*)(?:\s|$)', row.get('cmd') or '')
    public = run(['bash', str(root / 'scripts/oms'), 'list', '--all'])
    if public.returncode:
        fail('public OMS tool listing unavailable')
    tools = {line.split()[0] for line in public.stdout.splitlines() if line.strip()}
    verb = 'oms ' + match.group(1) if match and match.group(1) in tools else 'unavailable'
    sys.path.insert(0, str(root / 'scripts/lib'))
    from oms_runtime.failures import codes
    code = row.get('failure_code')
    if code not in codes():
        code = 'unknown_failure'
    lines += [f'Fingerprint: {a.from_failure}', f'Verb: {verb}',
              f'Exit: {row.get("exit")}', f'Failure code: {code}', f'Repeat count: {row["count"]}']
body = '\n'.join(lines) + ('\n\n' + message if message else '') + '\n'

with tempfile.TemporaryDirectory(prefix='oms-report-') as tmp:
    draft = Path(tmp) / 'draft'
    text = (a.title.replace('\r\n', '\n') if a.title is not None else '') + '\n' + message
    draft.write_text(text)
    scanned = run(['bash', '-c',
                   '. "$1/scripts/lib/agent-memory-common.sh"; '
                   'if agent_memory_file_has_sensitive_content "$2"; then exit 3; fi',
                   'report-scan', str(root), str(draft)])
    if scanned.returncode:
        fail('sensitive-content scanner refused the draft')
    # Slash commands are single tokens; paths contain another component or a drive.
    if re.search(r"(?<![\w/])(?:/(?:[^\s/]+/)+[^\s]*|[A-Za-z]:[\\/]|~/|\\\\[^\s]+)", text):
        fail('absolute path in user text')
    local_host = socket.gethostname().lower()
    generic = {'oms', 'os', 'linux', 'wsl', 'macos', 'bash', 'python', 'version',
               'commit', 'fingerprint', 'verb', 'exit', 'failure', 'code', 'repeat',
               'count', 'unavailable', 'bug', 'report', 'title'}
    words = re.findall(r'[A-Za-z0-9_.:-]+', text)
    if len(local_host) >= 4 and local_host not in generic and local_host in [w.rstrip(".").removesuffix(":").lower() for w in words]:
        fail('local hostname in user text')
    # Recognize address syntax with the stdlib, so times and test ids stay prose.
    for word in words:
        candidate = word.rstrip('.')
        try:
            ipaddress.ip_address(candidate)
        except ValueError:
            continue
        if candidate != '::':
            fail('IP address in user text')
    # Arbitrary dotted identifiers and filenames are ambiguous. Refuse network
    # names in explicit URL/host contexts and common network suffixes instead.
    host = r'(?:[a-z0-9-]+\.)+(?:com|org|net|edu|gov|io|dev|local|internal|lan)'
    if re.search(r'\b' + host + r'\b', text, re.I) or re.search(
            r'(?:https?://|\b(?:host(?:name)?|server)\s*[:=]\s*)(?:[a-z0-9-]+\.)+[a-z0-9-]+', text, re.I):
        fail('hostname in user text')
    if any(ord(ch) < 32 and ch not in '\n\t' for ch in text):
        fail('control character in user text')
    print('Title: ' + title + '\n\n' + body, flush=True)
    if a.dry_run:
        raise SystemExit(0)

    # Cap only the fallback body, on character boundaries before URL encoding.
    base = f'https://github.com/{repo}/issues/new?'
    url_title = title[:200]
    url_body = body[:8000]
    suffix = '\n[Draft shortened for URL; use --yes with gh for the full draft.]'
    if len(base + urlencode({'title': url_title, 'body': suffix})) > 8000:
        fail('destination exceeds URL capacity')
    while len(base + urlencode({'title': url_title, 'body': url_body})) > 8000:
        url_body = url_body[:max(0, len(url_body) - 100)]
        if not url_body:
            break
    if url_body != body:
        while len(base + urlencode({'title': url_title, 'body': url_body + suffix})) > 8000:
            url_body = url_body[:-100]
        url_body += suffix
    url = base + urlencode({'title': url_title, 'body': url_body})
    approved = a.yes
    if not approved and sys.stdin.isatty():
        approved = confirm('Publish this draft? [y/N] ')
    if not approved:
        print('Not published: explicit --yes or interactive y required.\n' + url)
        raise SystemExit(0 if sys.stdin.isatty() else 1)
    if not shutil.which('gh') or run(['gh', 'auth', 'status']).returncode:
        print('Not published: gh unavailable or unauthenticated.\n' + url)
        raise SystemExit(0)
    target = None
    if a.from_failure:
        found = run(['gh', 'issue', 'list', '--repo', repo, '--state', 'open',
                     '--search', a.from_failure + ' in:body', '--json', 'number', '--limit', '1'])
        if found.returncode:
            fail('duplicate search failed; nothing published')
        try:
            matches = json.loads(found.stdout)
            target = matches[0]['number'] if matches else None
            if target is not None and (type(target) is not int or target <= 0):
                raise ValueError()
        except (ValueError, KeyError, TypeError):
            fail('invalid duplicate search response; nothing published')
    draft.write_text(body)
    if target is not None:
        if not a.yes and not confirm(f'Comment on existing issue #{target}? [y/N] '):
            print('Not published.\n' + url)
            raise SystemExit(0)
        command = ['gh', 'issue', 'comment', str(target), '--repo', repo, '--body-file', str(draft)]
    else:
        label = 'bug' if a.kind == 'bug' else 'enhancement'
        labels = run(['gh', 'label', 'list', '--repo', repo, '--search', label, '--json', 'name', '--limit', '100'])
        if labels.returncode:
            fail('label lookup failed; nothing published')
        try:
            has_label = any(row['name'] == label for row in json.loads(labels.stdout))
        except (ValueError, KeyError, TypeError):
            fail('invalid label response; nothing published')
        command = ['gh', 'issue', 'create', '--repo', repo, '--title', title, '--body-file', str(draft)]
        if has_label:
            command += ['--label', label]
    # Never retry a publication: an interrupted response may already have posted.
    result = run(command)
    if result.returncode:
        fail('publication failed or outcome unknown; inspect GitHub before retrying')
    print(result.stdout.strip())
PY
)" "$ROOT" "$@"
