from __future__ import annotations
import json
import os
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from oms_runtime.common import CoreError
from oms_runtime.context_pack import MAX_PACK_BYTES, validate_context_pack

MODULE = ROOT / "scripts" / "lib" / "oms_runtime" / "context_pack.py"


def _pack(**overrides):
    pack = {
        "task": "carry the pack to the worker",
        "entries": ["file:scripts/plan-run.sh"],
        "files": ["scripts/plan-run.sh", "scripts/peer-delegate.sh"],
        "tests": ["tests/autonomy-plan-run-smoke.sh"],
        "evidence": [{"path": "scripts/plan-run.sh", "reason": "query:file", "score": 1001}],
        "hubs": [{"id": "file:scripts/plan-run.sh", "degree": 4}],
        "pack_digest": "a" * 64,
    }
    pack.update(overrides)
    return pack


class ContextPackValidatorTest(unittest.TestCase):

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="oms-context-pack-test.")
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        self.path = Path(self.tmp.name) / "pack.json"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def write(self, value, *, text: str = "") -> Path:
        self.path.write_text(text or json.dumps(value), encoding="utf-8")
        return self.path

    def refuses(self, value, needle: str, *, text: str = "") -> None:
        self.write(value, text=text)
        with self.assertRaises(CoreError) as caught:
            validate_context_pack(self.path, self.repo)
        message = str(caught.exception)
        self.assertIn(needle, message)
        self.assertNotIn("\n", message)

    def test_valid_pack_returns_a_bounded_summary(self) -> None:
        path = self.write(_pack())
        summary = validate_context_pack(path, self.repo)
        self.assertEqual(summary["schema"], 1)
        self.assertEqual(summary["file_count"], 2)
        self.assertEqual(summary["files"], ["scripts/plan-run.sh", "scripts/peer-delegate.sh"])
        self.assertEqual(summary["tests"], ["tests/autonomy-plan-run-smoke.sh"])
        self.assertEqual(summary["evidence"], [{"path": "scripts/plan-run.sh", "reason": "query:file"}])
        self.assertEqual(summary["project_graph_revision"], "")
        self.assertEqual(summary["sha256"], __import__("hashlib").sha256(path.read_bytes()).hexdigest())

    def test_symlinked_pack_is_refused(self) -> None:
        real = Path(self.tmp.name) / "real.json"
        real.write_text(json.dumps(_pack()), encoding="utf-8")
        link = Path(self.tmp.name) / "link.json"
        os.symlink(str(real), str(link))
        with self.assertRaises(CoreError) as caught:
            validate_context_pack(link, self.repo)
        self.assertIn("regular non-symlink", str(caught.exception))

    def test_absolute_path_is_refused(self) -> None:
        # An absolute path is secret-shaped to the outbound scrubber as well;
        # either refusal is correct, but the pack must never be accepted.
        self.refuses(_pack(files=["/etc/passwd"]), "path")

    def test_parent_traversal_is_refused(self) -> None:
        self.refuses(_pack(files=["scripts/../../outside.py"]), "escapes the repository")

    def test_private_state_paths_are_refused(self) -> None:
        self.refuses(_pack(tests=[".git/config"]), "private state")
        self.refuses(_pack(files=[".oms/plan/tasks.json"]), "private state")

    def test_secret_shaped_content_is_refused(self) -> None:
        # Split like common.py's own pattern so the fixture never reads as a
        # real key to a scanner. The brief's AKIA shape is deliberately absent:
        # SECRET_VALUE_RE has no AWS rule, so it would not exercise anything.
        pem = "-----BE" "GIN RSA PRIVATE " "KEY-----"
        self.refuses(_pack(task="rotate the %s material" % pem), "secret-shaped")
        self.refuses(_pack(task="api" "_key: fixture"), "secret-shaped")
        self.refuses(_pack(task="see /etc/hosts for the mapping"), "secret-shaped")

    def test_oversized_pack_is_refused(self) -> None:
        pack = _pack(task="x" * (MAX_PACK_BYTES + 10))
        self.refuses(pack, "over the")

    def test_shape_violations_are_refused(self) -> None:
        self.refuses(_pack(pack_digest="A" * 64), "pack_digest")
        self.refuses(_pack(pack_digest="abc"), "pack_digest")
        self.refuses(_pack(files="scripts/plan-run.sh"), "files must be a list")
        self.refuses(_pack(files=[1]), "entries must be strings")
        self.refuses(_pack(files=["scripts/x.sh" for _ in range(201)]), "over the 200")
        self.refuses(_pack(evidence=[{"reason": "no path"}]), "evidence")
        self.refuses(_pack(hubs={"id": "x"}), "hubs must be a list")
        self.refuses(_pack(project_graph_revision=7), "project_graph_revision")
        self.refuses(None, "not valid JSON", text="[not json")
        self.refuses(None, "must be a JSON object", text="[]")

    def test_backslash_and_dot_slash_paths_are_refused(self) -> None:
        self.refuses(_pack(files=["scripts\\plan-run.sh"]), "backslash")
        self.refuses(_pack(files=["./scripts/plan-run.sh"]), "not normalized")

    def test_missing_pack_is_refused_without_a_traceback(self) -> None:
        with self.assertRaises(CoreError) as caught:
            validate_context_pack(Path(self.tmp.name) / "absent.json", self.repo)
        self.assertIn("cannot stat pack", str(caught.exception))

    def test_module_entrypoint_is_the_single_code_path_for_bash(self) -> None:
        path = self.write(_pack())
        ok = subprocess.run([sys.executable, str(MODULE), "--repo", str(self.repo), str(path)],
                            capture_output=True, text=True)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertEqual(json.loads(ok.stdout)["file_count"], 2)

        self.write(_pack(files=["/etc/passwd"]))
        bad = subprocess.run([sys.executable, str(MODULE), "--repo", str(self.repo), str(path)],
                             capture_output=True, text=True)
        self.assertEqual(bad.returncode, 2)
        self.assertEqual(bad.stdout, "")
        self.assertTrue(bad.stderr.strip())
        self.assertNotIn("Traceback", bad.stderr)

        usage = subprocess.run([sys.executable, str(MODULE), str(path)], capture_output=True, text=True)
        self.assertEqual(usage.returncode, 2)
        self.assertIn("usage:", usage.stderr)

    def test_validator_writes_no_state(self) -> None:
        path = self.write(_pack())
        before = sorted(item.name for item in Path(self.tmp.name).iterdir())
        validate_context_pack(path, self.repo)
        runpy.run_path(str(MODULE), run_name="not-main")
        self.assertEqual(sorted(item.name for item in Path(self.tmp.name).iterdir()), before)
        self.assertEqual(sorted(item.name for item in self.repo.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
