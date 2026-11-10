"""Single-command pre-registration over the canonical run ledger."""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

from .common import CoreError, install_root, read_json
from .experiment_budget import require_run_budget, reserve_run, settle_run
from .experiment_contract import validate

RESEARCH_NOTE_MAX_CHARS = 1600


def _ledger_has(ledger: Path, run_id: str) -> bool:
    try:
        raw = ledger.read_bytes().splitlines()
    except OSError:
        return False
    for line in raw:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and row.get('operation_id') == run_id:
            return True
    return False


def launch(repo: Path, args) -> int:
    names = ('question', 'hypothesis', 'prediction', 'baseline', 'metric', 'success', 'change')
    fields = {name: getattr(args, name) for name in names}
    contract = None
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
        # A contract whose run budget is spent needs a stated reason, as --no-gate does.
        # The ledger row keeps the reason only alongside --no-gate; here it is
        # required and echoed, and `experiment run` is the path that records it.
        override = require_run_budget(repo, contract, args.reason)
        if override:
            print('experiment launch: past the contract run budget (%s)' % override, file=sys.stderr, flush=True)
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
    if len(note.encode('utf-8')) > RESEARCH_NOTE_MAX_CHARS:
        raise CoreError('pre-registration note exceeds %d bytes; shorten the fields' % RESEARCH_NOTE_MAX_CHARS)

    scripts = install_root() / 'scripts'
    with tempfile.TemporaryDirectory(prefix='oms-experiment-') as scratch:
        note_path = Path(scratch) / 'note.txt'
        note_path.write_text(note, encoding='utf-8')
        # Keep the ledger's existing outbound-data policy, including path checks.
        scan = subprocess.run([
            'bash', '-c', '. "$1"; agent_memory_file_sensitive_status "$2"',
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
        # The preflight above reserved nothing: take the run's share of
        # budget.runs under the state lock, and settle it from the run ledger.
        run_id = 'experiment-run-' + uuid.uuid4().hex
        reserved = False
        if contract is not None:
            _, reserved = reserve_run(repo, contract, '', run_id, args.reason, 'launch')
        if reserved:
            env['OMS_OPERATION_ID'] = run_id
        try:
            rc = subprocess.run(cmd, cwd=repo, env=env, check=False).returncode
        except BaseException as exc:
            if reserved:
                settle_run(repo, contract, run_id, 'released', 'launch ended without a record: %s' % (str(exc) or type(exc).__name__))
            raise
        if reserved:
            ledger = Path(args.file).expanduser() if args.file else Path('docs') / 'EXPERIMENTS.jsonl'
            recorded = _ledger_has(ledger if ledger.is_absolute() else repo / ledger, run_id)
            settle_run(repo, contract, run_id, 'used' if recorded else 'released',
                       '' if recorded else 'the run ledger did not record this run')
        return rc if rc >= 0 else 128 - rc
