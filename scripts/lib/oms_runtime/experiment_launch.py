"""Single-command pre-registration over the canonical run ledger."""
import json
import os
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

from .common import CoreError, install_root, read_json
from .experiment_contract import validate


def launch(repo: Path, args) -> int:
    names = ('question', 'hypothesis', 'prediction', 'baseline', 'metric', 'success', 'change')
    fields = {name: getattr(args, name) for name in names}
    if args.contract:
        path = Path(args.contract).expanduser()
        contract = validate(read_json(path if path.is_absolute() else repo / path))
        success = contract.get('success', {})
        threshold = 'min_improvement=%s' % success.get('min_improvement', 0.0)
        regressions = sorted(success.get('no_regression', {}))
        if regressions:
            threshold += '; no_regression=' + ','.join(regressions)
        derived = dict(zip(names, (
            contract['question'], contract['hypothesis'], contract['prediction'],
            json.dumps(contract['baseline'], sort_keys=True)[:300],
            '%s (%s)' % (contract['primary_metric'], contract['metrics']['primary']['direction']),
            threshold, contract['treatment']['independent_change'],
        )))
        for name, value in derived.items():
            value = ' '.join(str(value).split())
            if fields[name] and fields[name] != value:
                raise CoreError('--%s conflicts with --contract' % name)
            fields[name] = value
        fields['contract_digest'] = contract['contract_digest']
    for name in names:
        if not fields[name].strip():
            raise CoreError('--%s is required' % name)
    command = list(args.remainder)
    if command and command[0] == '--':
        command = command[1:]
    if not command:
        raise CoreError('command after -- is required')
    if args.no_gate and not args.reason.strip():
        raise CoreError('--reason (required with --no-gate) is required')
    note = ' | '.join('%s: %s' % (label, fields[name])
                      for label, name in zip('QHPBMSC', names))
    try:
        limit = int(os.environ.get('OMS_RESEARCH_NOTE_MAX_CHARS', '1600'))
        if limit <= 0:
            raise ValueError
    except ValueError:
        raise CoreError('OMS_RESEARCH_NOTE_MAX_CHARS must be a positive integer')
    if len(note.encode('utf-8')) > limit:
        raise CoreError('pre-registration note exceeds %d bytes; shorten the fields' % limit)

    scripts = install_root() / 'scripts'
    with tempfile.TemporaryDirectory(prefix='oms-experiment-') as scratch:
        note_path = Path(scratch) / 'note.txt'
        note_path.write_text(note, encoding='utf-8')
        # Keep the ledger's existing outbound-data policy, including path checks.
        scan = subprocess.run([
            'bash', '-c', '. "$1"; agent_memory_file_has_sensitive_content "$2"',
            '_', str(scripts / 'lib' / 'agent-memory-common.sh'), str(note_path),
        ], cwd=repo, check=False)
        if scan.returncode != 1:
            raise CoreError('pre-registration contains sensitive-looking content or scan failed')
        metadata = Path(scratch) / 'research.json'
        metadata.write_text(json.dumps(fields, ensure_ascii=False), encoding='utf-8')
        cmd = ['bash', str(scripts / 'run-ledger.sh'), '--note', note]
        for flag in ('file', 'metrics'):
            if getattr(args, flag):
                cmd += ['--' + flag, getattr(args, flag)]
        if args.no_gate:
            cmd += ['--no-gate', '--reason', args.reason]
        cmd += ['--'] + command
        if args.dry_run:
            print('experiment launch: dry-run\nnote: %s\ncommand: %s' % (note, shlex.join(cmd)))
            return 0
        print('experiment launch: launching registered experiment', file=sys.stderr, flush=True)
        env = dict(os.environ, OMS_RESEARCH_METADATA_FILE=str(metadata),
                   OMS_WORK_JOURNAL_EVENT_TYPE='experiment')
        rc = subprocess.run(cmd, cwd=repo, env=env, check=False).returncode
        return rc if rc >= 0 else 128 - rc
