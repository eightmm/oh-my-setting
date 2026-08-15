"""CLI for the compatibility-preserving OMS runtime core."""
from __future__ import annotations
import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple
from . import CORE_VERSION
from .benchmark import compare as compare_benchmarks
from .benchmark import persist as persist_benchmark
from .benchmark import record_outcome as record_benchmark_outcome
from .benchmark import snapshot as benchmark_snapshot
from .capsule import diff as diff_capsules
from .capsule import export as export_capsule
from .capsule import import_capsule, verify as verify_capsule
from .common import CoreError, atomic_write_json, install_root, load_json_argument, repo_root
from .context import DEFAULT_CONTEXT_BYTES, plan_context
from .evidence import bind as bind_evidence
from .evidence import revoke as revoke_evidence
from .evidence import build_coverage, build_envelope
from .execution import check as check_backend
from .execution import describe as describe_backend
from .execution import run as run_backend
from .experiment import compare as compare_experiments
from .experiment import evaluate as evaluate_experiment
from .experiment import load_contract, record_run, register as register_experiment
from .experiment import run_invariant_pack, summarize as summarize_experiment, template as experiment_template, validate as validate_experiment
from .failures import catalog as failure_catalog
from .failures import classify as classify_failure
from .profiles import PROFILE_DEFS, apply as apply_profiles, check as check_profiles, current as current_profiles, install_plan
from .release import apply as apply_release
from .release import promote as promote_release
from .release import resolve as resolve_release
from .release import status as release_status

def emit(value: Any, pretty: bool=False) -> None:
    print(json.dumps(value, ensure_ascii=False, allow_nan=False, sort_keys=True, indent=2 if pretty else None, separators=None if pretty else (',', ':')))

def _path(repo: Path, raw: str) -> Path:
    path = Path(raw).expanduser()
    return path if path.is_absolute() else repo / path

def _file_reason(raw: str) -> Tuple[str, str]:
    if ':' in raw and (not (len(raw) > 2 and raw[1] == ':')):
        path, reason = raw.split(':', 1)
        return (path, reason)
    return (raw, 'explicit context')

from .cli_parser import build_parser

def main(argv: Optional[Sequence[str]]=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        repo = repo_root(args.repo)
        if args.command == 'envelope':
            value = build_envelope(repo)
            if args.action == 'write':
                output = _path(repo, args.output)
                atomic_write_json(output, value)
                value = {'written': str(output.relative_to(repo)) if output.is_relative_to(repo) else output.name, 'envelope': value}
            emit(value, args.pretty)
            return 0
        if args.command == 'evidence':
            coverage = build_coverage(repo)
            if args.evidence_action == 'show':
                value = coverage
            elif args.evidence_action == 'unbound':
                value = {'schema': coverage['schema'], 'unbound_evidence': coverage['unbound_evidence']}
            elif args.evidence_action == 'bind':
                value = bind_evidence(repo, args.criterion, args.ref, args.status, evidence_type=args.type, note=args.note, dependencies=args.depends)
            else:
                value = revoke_evidence(repo, args.binding, reason=args.reason)
            emit(value, args.pretty)
            return 0
        if args.command == 'next':
            envelope = build_envelope(repo)
            emit({'schema': 1, 'state_digest': envelope['state_digest'], 'actions': envelope['next_actions']}, args.pretty)
            return 0
        if args.command == 'failure':
            value = classify_failure(args.text, args.exit, args.code) if args.failure_action == 'classify' else failure_catalog()
            emit(value, args.pretty)
            return 0
        if args.command == 'context':
            value = plan_context(repo, target=args.target, explicit=[_file_reason(item) for item in args.file], required=args.require, max_bytes=args.max_bytes, bundle_path=_path(repo, args.bundle) if args.bundle else None, manifest_path=_path(repo, args.manifest) if args.manifest else None, allow_external=args.allow_external, phase=args.phase)
            emit(value, args.pretty)
            return 0 if value['sufficient'] else 3
        if args.command == 'profile':
            if args.profile_action == 'list':
                value = {'schema': 1, 'profiles': PROFILE_DEFS}
            elif args.profile_action == 'current':
                value = current_profiles(repo)
            elif args.profile_action == 'show':
                value = {'schema': 1, 'name': args.name, 'profile': PROFILE_DEFS[args.name]}
            elif args.profile_action == 'check':
                value = check_profiles(args.profiles)
            elif args.profile_action == 'apply':
                value = apply_profiles(repo, args.profiles, allow_missing=args.allow_missing)
            elif args.profile_action == 'install-plan':
                value = install_plan(args.profiles, args.primary_provider)
            else:
                command = ['bash', str(install_root() / 'scripts' / 'install-profile.sh')]
                for name in args.profiles:
                    command.extend(['--profile', name])
                command.extend(['--primary-provider', args.primary_provider])
                if args.upgrade:
                    command.append('--upgrade')
                if args.allow_missing:
                    command.append('--allow-missing')
                if args.receipt:
                    command.extend(['--receipt', str(_path(repo, args.receipt))])
                if args.dry_run:
                    command.append('--dry-run')
                result = subprocess.run(command, cwd=str(repo), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False)
                if result.stderr:
                    print(result.stderr.rstrip(), file=sys.stderr)
                installer_result: Any = None
                if result.stdout.strip():
                    try:
                        installer_result = json.loads(result.stdout)
                    except ValueError:
                        installer_result = {'output': result.stdout[-4000:]}
                configured = None
                if result.returncode == 0 and (not args.dry_run):
                    configured = apply_profiles(repo, args.profiles, allow_missing=args.allow_missing)
                value = {'schema': 1, 'exit': result.returncode, 'installed': result.returncode == 0, 'plan': install_plan(args.profiles, args.primary_provider), 'installer': installer_result, 'configured': configured}
                emit(value, args.pretty)
                return result.returncode
            emit(value, args.pretty)
            return 0 if value.get('ready', value.get('check', {}).get('ready', True)) else 3
        if args.command == 'release':
            source = install_root()
            if args.release_action == 'status':
                value = release_status(source)
            elif args.release_action == 'resolve':
                value = resolve_release(source, args.channel, fetch=args.fetch)
            elif args.release_action == 'apply':
                value = apply_release(source, args.channel, fetch=not args.no_fetch, no_tools=not args.tools)
            else:
                value = promote_release(source, args.commit, args.version, expected_manifest_digest=args.expected_manifest_digest)
            emit(value, args.pretty)
            return 0 if value.get('ready', value.get('applied', True)) else 3
        if args.command == 'capsule':
            if args.capsule_action == 'export':
                value = export_capsule(repo, _path(repo, args.output) if args.output else None)
            elif args.capsule_action == 'verify':
                value = verify_capsule(_path(repo, args.path))
            elif args.capsule_action == 'import':
                value = import_capsule(repo, _path(repo, args.path))
            else:
                value = diff_capsules(_path(repo, args.left), _path(repo, args.right))
            emit(value, args.pretty)
            return 0
        if args.command == 'backend':
            if args.backend_action == 'describe':
                value = describe_backend(args.profile)
                rc = 0
            elif args.backend_action == 'check':
                value = check_backend(args.profile, image=args.image, adapter=args.adapter)
                rc = 0 if value['ready'] else 3
            else:
                command = list(args.remainder)
                if command and command[0] == '--':
                    command = command[1:]
                value, rc = run_backend(args.profile, repo, command, timeout_seconds=args.timeout_seconds, image=args.image, adapter=args.adapter, worktree=_path(repo, args.worktree) if args.worktree else None, allow_network=args.allow_network, inherit_env=args.inherit_env, log_path=_path(repo, args.log) if args.log else None, receipt_path=_path(repo, args.receipt) if args.receipt else None, log_limit=args.log_limit, memory_mb=args.memory_mb, cpus=args.cpus, pids_limit=args.pids_limit, tmpfs_mb=args.tmpfs_mb)
            emit(value, args.pretty)
            return rc
        if args.command == 'experiment':
            if args.experiment_action == 'template':
                value = experiment_template()
                if args.output:
                    atomic_write_json(_path(repo, args.output), value)
            elif args.experiment_action == 'validate':
                value = validate_experiment(load_json_argument(args.contract))
            elif args.experiment_action == 'compare':
                value = compare_experiments(load_json_argument(args.left), load_json_argument(args.right))
            elif args.experiment_action == 'register':
                value = register_experiment(repo, load_json_argument(args.contract), args.id)
            elif args.experiment_action == 'show':
                value = load_contract(repo, args.id)
            elif args.experiment_action == 'run':
                command = list(args.remainder)
                if command and command[0] == '--':
                    command = command[1:]
                value, rc = record_run(repo, args.id, args.arm, args.seed, _path(repo, args.metrics), command, profile=args.profile, timeout_seconds=args.timeout_seconds, image=args.image, adapter=args.adapter, worktree=_path(repo, args.worktree) if args.worktree else None, allow_existing_metrics=args.allow_existing_metrics)
                emit(value, args.pretty)
                return rc
            elif args.experiment_action == 'summarize':
                value = summarize_experiment(repo, args.id)
            elif args.experiment_action == 'evaluate':
                value = evaluate_experiment(load_json_argument(args.contract), load_json_argument(args.results))
            else:
                value = run_invariant_pack(repo, load_json_argument(args.contract), profile=args.profile, image=args.image, adapter=args.adapter, timeout_seconds=args.timeout_seconds)
            emit(value, args.pretty)
            if isinstance(value, dict) and value.get('verdict') == 'not_supported':
                return 3
            return 0
        if args.command == 'benchmark':
            if args.benchmark_action == 'show':
                value = benchmark_snapshot(repo)
            elif args.benchmark_action == 'snapshot':
                value = persist_benchmark(repo)
            elif args.benchmark_action == 'record':
                value = record_benchmark_outcome(repo, task_id=args.task_id, status=args.status, human_corrections=args.human_corrections, escaped_defects=args.escaped_defects, reverted_lines=args.reverted_lines, false_refusals=args.false_refusals, duplicate_work=args.duplicate_work, note=args.note)
            else:
                value = compare_benchmarks(load_json_argument(args.left), load_json_argument(args.right))
            emit(value, args.pretty)
            return 0
        if args.command == 'doctor':
            envelope = build_envelope(repo)
            profile = current_profiles(repo)
            release = release_status(install_root())
            issues: List[str] = []
            if not profile.get('check', {}).get('ready'):
                issues.append('configured capability profile is not ready')
            if not envelope.get('criteria'):
                issues.append('no acceptance criteria are projected')
            if envelope.get('failures'):
                issues.append('unresolved failures exist')
            if not release.get('stable', {}).get('ready'):
                issues.append('stable channel ref is unavailable locally')
            value = {'schema': 1, 'ok': not issues, 'issues': issues, 'profile': profile, 'state_digest': envelope.get('state_digest'), 'release': release}
            emit(value, args.pretty)
            return 3 if args.strict and issues else 0
        raise CoreError('unknown command')
    except CoreError as exc:
        print('runtime-core: %s' % exc, file=sys.stderr)
        return exc.exit_code
    except KeyboardInterrupt:
        print('runtime-core: interrupted', file=sys.stderr)
        return 130
if __name__ == '__main__':
    raise SystemExit(main())
