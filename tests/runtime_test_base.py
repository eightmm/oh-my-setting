from __future__ import annotations
import json
import os
import stat
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from oms_runtime import capsule, context, evidence, experiment, failures, profiles, release
from oms_runtime.benchmark import record_outcome as record_benchmark_outcome
from oms_runtime.benchmark import snapshot as benchmark_snapshot
from oms_runtime.common import CoreError, append_jsonl, atomic_write_bytes, atomic_write_json, canonical_json, git_head, parse_path_list, read_json, read_jsonl, sensitive_text, sha256_bytes, sha256_file, sha256_text
from oms_runtime.execution import check as check_backend
from oms_runtime.execution import run as run_backend

class RuntimeFixtureBase(unittest.TestCase):

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix='oms-runtime-test.')
        self.repo = Path(self.tmp.name) / 'repo'
        self.repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        subprocess.run(['git', '-C', str(self.repo), 'config', 'user.email', 'test@example.com'], check=True)
        subprocess.run(['git', '-C', str(self.repo), 'config', 'user.name', 'OMS Test'], check=True)
        (self.repo / 'scripts').mkdir()
        status_script = self.repo / 'scripts' / 'agent-task.sh'
        status_script.write_text('#!/usr/bin/env bash\nprintf \'%s\\n\' \'{"schema":1,"present":true,"task_id":"task-fixture","status":"verified","verification":"fresh","stale":false}\'\n', encoding='utf-8')
        status_script.chmod(493)
        (self.repo / 'PROJECT.md').write_text('# Demo\n\n## Goal\n\nShip the runtime core.\n\n## Acceptance Criteria\n\n- [id:project-api] Public API remains compatible.\n- [id:project-safe] Portable state carries no authority.\n\n## Scope\n\n- allowed_paths: scripts/, tests/\n- forbidden_paths: secrets/\n', encoding='utf-8')
        task_dir = self.repo / '.oms' / 'task'
        task_dir.mkdir(parents=True)
        (task_dir / 'current.md').write_text('# Active Agent Task\n\n- task_id: task-fixture\n- status: verified\n\n## Goal\n\nImplement the projection.\n\n## Constraints\n\n- allowed_paths: scripts/, tests/\n\n## Done Criteria\n\n- [id:task-tests] Focused tests pass.\n\n## Verify\n\npython3 -m unittest\n\n## Loop State\n\n- max_attempts: 2\n\n## Current State\n\nImplementation ready.\n\n## Next Step\n\nRun the focused gate.\n', encoding='utf-8')
        plan_dir = self.repo / '.oms' / 'plan'
        plan_dir.mkdir(parents=True)
        (plan_dir / 'tasks.json').write_text(json.dumps({'goal': 'Complete runtime integration', 'accept': 'python3 -m unittest', 'tasks': [{'id': 't1', 'title': 'Implement core', 'state': 'ready', 'allowed': ['scripts/', 'tests/'], 'verify': 'python3 -m unittest'}]}), encoding='utf-8')
        artifact_dir = self.repo / '.oms' / 'artifacts'
        artifact_dir.mkdir(parents=True)
        (artifact_dir / 'index.jsonl').write_text(json.dumps({'schema': 1, 'event_id': 'evt-api', 'kind': 'verify', 'status': 'verified', 'covers': ['project-api']}) + '\n', encoding='utf-8')
        (self.repo / 'scripts' / 'sample.py').write_text('from scripts.helper import value\n\ndef calculate(x):\n    return value + x\n', encoding='utf-8')
        (self.repo / 'scripts' / 'helper.py').write_text('value = 2\n', encoding='utf-8')
        test_dir = self.repo / 'tests'
        test_dir.mkdir()
        (test_dir / 'test_sample.py').write_text('from scripts.sample import calculate\nassert calculate(1) == 3\n', encoding='utf-8')
        subprocess.run(['git', '-C', str(self.repo), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(self.repo), 'commit', '-qm', 'fixture'], check=True)
        config_dir = self.repo / 'config'
        config_dir.mkdir(exist_ok=True)
        atomic_write_json(config_dir / 'update-channels.json', {'schema': 1, 'channels': {'stable': {'version': '1.0.0', 'ref': git_head(self.repo), 'auto_apply': False, 'policy': 'pinned'}, 'edge': {'version': 'edge', 'ref': 'master', 'auto_apply': False, 'policy': 'fast-forward'}}}, mode=420)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def _experiment(self) -> dict:
        row = experiment.template()
        row.update({'question': 'Does the treatment improve score?', 'hypothesis': 'The treatment improves the primary score.', 'prediction': 'Treatment exceeds baseline by at least 0.1.', 'baseline': {'run_id': 'baseline', 'config_digest': 'a'}, 'treatment': {'independent_change': 'enable channel', 'config_digest': 'b'}, 'controlled_variables': {'code_base': git_head(self.repo), 'dataset_split': 'fixed', 'training_steps': 10}, 'seeds': [0], 'metrics': {'primary': {'name': 'score', 'direction': 'maximize'}, 'secondary': [{'name': 'memory', 'direction': 'minimize'}]}, 'success': {'min_improvement': 0.1, 'no_regression': {'memory': {'direction': 'minimize', 'max_regression': 0.5}}}})
        return row
