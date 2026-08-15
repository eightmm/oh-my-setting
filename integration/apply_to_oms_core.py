"""Apply the typed-runtime overlay to an exact clean OMS checkout."""
from __future__ import annotations
import argparse
import json
import subprocess
from pathlib import Path
from typing import Dict, List, Sequence
from oms_patchers import (ApplyError, COPY_PATHS, PATCHED_PATHS, copy_overlay_file, patch_changelog, patch_check, patch_components, patch_install, patch_update)
EXPECTED_BASE = 'fa1e9e2cf66ee8b9a5adb642a5ef766c1381db9b'

DEFAULT_BRANCH = 'agent/oms-core-consolidation'

def run(command: Sequence[str], *, cwd: Path, check: bool=True, capture: bool=False) -> subprocess.CompletedProcess:
    result = subprocess.run(list(command), cwd=str(cwd), check=False, text=True, stdout=subprocess.PIPE if capture else None, stderr=subprocess.PIPE if capture else None)
    if check and result.returncode != 0:
        detail = ''
        if capture:
            detail = '\n%s%s' % (result.stdout or '', result.stderr or '')
        raise ApplyError('command failed (%d): %s%s' % (result.returncode, ' '.join(command), detail))
    return result

def git(target: Path, *args: str, capture: bool=True, check: bool=True) -> str:
    result = run(['git', *args], cwd=target, capture=capture, check=check)
    return (result.stdout or '').strip()

def verify_target(target: Path, *, full_check: bool) -> List[Dict[str, object]]:
    checks: List[Sequence[str]] = [['bash', '-n', 'install.sh'], ['bash', '-n', 'scripts/oms'], ['bash', '-n', 'scripts/runtime-core.sh'], ['bash', '-n', 'scripts/install-profile.sh'], ['bash', '-n', 'scripts/update.sh'], ['bash', '-n', 'scripts/check.sh'], ['bash', 'scripts/check-python.sh'], ['bash', 'tests/runtime-core-smoke.sh'], ['bash', 'tests/runtime-core-integration-smoke.sh'], ['bash', 'tests/install-profile-smoke.sh']]
    if full_check:
        checks.append(['bash', 'scripts/check.sh', '--focused-only'])
    rows: List[Dict[str, object]] = []
    for command in checks:
        result = run(command, cwd=target, check=False, capture=True)
        rows.append({'command': list(command), 'exit': result.returncode, 'stdout_tail': (result.stdout or '')[-2000:], 'stderr_tail': (result.stderr or '')[-2000:]})
        if result.returncode != 0:
            raise ApplyError('verification failed: %s\n%s%s' % (' '.join(command), result.stdout or '', result.stderr or ''))
    return rows

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('target', help='clean oh-my-setting checkout')
    parser.add_argument('--expected-base', default=EXPECTED_BASE)
    parser.add_argument('--allow-descendant', action='store_true', help='accept a clean descendant of the expected base')
    parser.add_argument('--branch', default=DEFAULT_BRANCH)
    parser.add_argument('--no-branch', action='store_true')
    parser.add_argument('--no-commit', action='store_true')
    parser.add_argument('--full-check', action='store_true')
    parser.add_argument('--report', default='')
    return parser.parse_args()

def main() -> int:
    args = parse_args()
    source_root = Path(__file__).resolve().parents[1]
    target = Path(args.target).expanduser().resolve()
    if not target.is_dir() or target.is_symlink():
        raise ApplyError('target must be a real directory')
    top = git(target, 'rev-parse', '--show-toplevel')
    if Path(top).resolve() != target:
        raise ApplyError('target must be the repository top level: %s' % top)
    dirty = git(target, 'status', '--porcelain=v1', '--untracked-files=all')
    if dirty:
        raise ApplyError('target checkout is not clean')
    head = git(target, 'rev-parse', 'HEAD')
    if head != args.expected_base:
        if not args.allow_descendant:
            raise ApplyError('unexpected base: %s (expected %s)' % (head, args.expected_base))
        ancestor = run(['git', 'merge-base', '--is-ancestor', args.expected_base, head], cwd=target, check=False)
        if ancestor.returncode != 0:
            raise ApplyError('current HEAD is not a descendant of the expected base')
    if not args.no_branch:
        current = git(target, 'symbolic-ref', '--quiet', '--short', 'HEAD', check=False)
        if current != args.branch:
            exists = run(['git', 'show-ref', '--verify', '--quiet', 'refs/heads/%s' % args.branch], cwd=target, check=False)
            if exists.returncode == 0:
                branch_head = git(target, 'rev-parse', 'refs/heads/%s' % args.branch)
                if branch_head != head:
                    raise ApplyError('branch already exists at another commit: %s' % args.branch)
                git(target, 'checkout', args.branch, capture=False)
            else:
                git(target, 'checkout', '-b', args.branch, capture=False)
    for relative in COPY_PATHS:
        copy_overlay_file(source_root, target, relative)
    integration_patch = source_root / 'integration' / 'runtime-state-integration.patch'
    run(['git', 'apply', '--check', str(integration_patch)], cwd=target, capture=True)
    run(['git', 'apply', str(integration_patch)], cwd=target, capture=True)
    patch_check(target / 'scripts' / 'check.sh')
    patch_update(target / 'scripts' / 'update.sh')
    patch_install(target / 'install.sh')
    patch_changelog(target / 'CHANGELOG.md')
    patch_components(target / 'docs' / 'COMPONENTS.md')
    verification = verify_target(target, full_check=args.full_check)
    git(target, 'diff', '--check', capture=False)
    changed = git(target, 'status', '--short').splitlines()
    commit = None
    if not args.no_commit:
        git(target, 'add', '-A', capture=False)
        git(target, 'diff', '--cached', '--check', capture=False)
        run(['git', 'commit', '-m', 'feat: add typed OMS runtime core'], cwd=target, capture=False)
        commit = git(target, 'rev-parse', 'HEAD')
    report = {'schema': 1, 'base': head, 'branch': None if args.no_branch else args.branch, 'commit': commit, 'copied': COPY_PATHS, 'patched': PATCHED_PATHS, 'changed_before_commit': changed, 'verification': verification}
    report_path = Path(args.report).expanduser() if args.report else target / '.git' / 'oms-runtime-apply-report.json'
    report_path = report_path if report_path.is_absolute() else target / report_path
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    print(json.dumps(report, sort_keys=True))
    return 0
