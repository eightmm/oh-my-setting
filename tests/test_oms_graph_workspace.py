"""Workspace fingerprint cases for the execution read-cache (W5).

Every case asserts a RELATION between fingerprints of one temporary repository
-- never a literal hash -- so the digest payload stays free to change shape as
long as the cache-invalidation contract holds.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.workspace import cache_allowed, workspace_fingerprint

HAVE_GIT = shutil.which("git") is not None


@unittest.skipUnless(HAVE_GIT, "git is required to fingerprint a workspace")
class WorkspaceFingerprintTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="oms-graph-workspace.")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self._hermetic_env()
        self.git("init", "-q")
        self.write("tracked.txt", "one\n")
        self.write(".gitignore", "ignored.txt\n")
        self.git("add", "-A")
        self.git("commit", "-qm", "base")
        self.clean = workspace_fingerprint(self.repo)

    def _hermetic_env(self) -> None:
        """The module runs git with the ambient env; pin config discovery to the fixture."""
        self._saved_env = dict(os.environ)
        self.addCleanup(self._restore_env)
        os.environ["HOME"] = self.temp.name
        os.environ["XDG_CONFIG_HOME"] = str(Path(self.temp.name) / "config")
        os.environ["GIT_CONFIG_NOSYSTEM"] = "1"

    def _restore_env(self) -> None:
        os.environ.clear()
        os.environ.update(self._saved_env)

    def git(self, *args: str) -> None:
        argv = ["git", "-C", str(self.repo), "-c", "user.email=test@example.com",
                "-c", "user.name=Test", "-c", "commit.gpgsign=false",
                "-c", "init.defaultBranch=main"] + list(args)
        subprocess.run(argv, check=True, stdout=subprocess.DEVNULL)

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def digest(self, **kwargs) -> str:
        fingerprint = workspace_fingerprint(self.repo, **kwargs)
        self.assertEqual(fingerprint["unsafe"], "", fingerprint)
        self.assertTrue(cache_allowed(fingerprint), fingerprint)
        return str(fingerprint["digest"])

    def test_clean_and_tracked_edits_and_staging(self) -> None:
        self.assertTrue(self.clean["digest"])
        self.assertEqual(self.clean["unsafe"], "")
        self.assertEqual(self.clean["dirty_count"], 0)
        self.assertTrue(cache_allowed(self.clean))
        self.assertEqual(workspace_fingerprint(self.repo)["digest"], self.clean["digest"])

        self.write("tracked.txt", "two\n")
        unstaged = self.digest()
        self.assertNotEqual(unstaged, self.clean["digest"])
        self.write("tracked.txt", "one\n")
        self.assertEqual(self.digest(), self.clean["digest"])

        self.write("tracked.txt", "two\n")
        self.git("add", "tracked.txt")
        staged = self.digest()
        self.assertNotEqual(staged, unstaged)
        self.assertNotEqual(staged, self.clean["digest"])

        # `MM` with the committed bytes restored: only `staged` tells these apart.
        self.write("tracked.txt", "one\n")
        first_mm = self.digest()
        self.write("tracked.txt", "three\n")
        self.git("add", "tracked.txt")
        self.write("tracked.txt", "one\n")
        self.assertNotEqual(self.digest(), first_mm)

    def test_untracked_ignored_oms_state_delete_and_rename(self) -> None:
        self.write("new.txt", "fresh\n")
        self.assertNotEqual(self.digest(), self.clean["digest"])
        (self.repo / "new.txt").unlink()
        self.assertEqual(self.digest(), self.clean["digest"])

        self.write("ignored.txt", "noise\n")
        self.assertEqual(self.digest(), self.clean["digest"])

        self.write(".oms/graph/runs/r1/events.jsonl", '{"kind":"node-finished"}\n')
        fingerprint = workspace_fingerprint(self.repo)
        self.assertEqual(fingerprint["digest"], self.clean["digest"])
        self.assertEqual(fingerprint["dirty_count"], 0)
        self.git("add", ".oms/graph/runs/r1/events.jsonl")
        self.assertEqual(self.digest(), self.clean["digest"])

        (self.repo / "tracked.txt").unlink()
        deleted = workspace_fingerprint(self.repo)
        self.assertEqual((deleted["unsafe"], deleted["dirty_count"]), ("", 1))
        self.assertNotEqual(deleted["digest"], self.clean["digest"])
        self.git("checkout", "-q", "--", "tracked.txt")

        self.git("mv", "tracked.txt", "renamed.txt")
        renamed = workspace_fingerprint(self.repo)
        self.assertEqual((renamed["unsafe"], renamed["dirty_count"]), ("", 1))
        self.assertNotEqual(renamed["digest"], self.clean["digest"])

    def test_fail_closed_limits_and_symlinks(self) -> None:
        os.symlink("tracked.txt", str(self.repo / "link.txt"))
        symlink_fp = workspace_fingerprint(self.repo)
        self.assertTrue(str(symlink_fp["unsafe"]).startswith("symlink:"), symlink_fp)
        self.assertEqual(symlink_fp["digest"], "")
        self.assertFalse(cache_allowed(symlink_fp))
        (self.repo / "link.txt").unlink()

        self.write("big.txt", "x" * 64)
        large_fp = workspace_fingerprint(self.repo, max_file_bytes=16)
        self.assertTrue(str(large_fp["unsafe"]).startswith("large-file:"), large_fp)
        self.assertEqual(large_fp["digest"], "")
        self.assertFalse(cache_allowed(large_fp))
        (self.repo / "big.txt").unlink()

        for index in range(4):
            self.write("dirty%d.txt" % index, "%d\n" % index)
        many_fp = workspace_fingerprint(self.repo, max_entries=3)
        self.assertTrue(str(many_fp["unsafe"]).startswith("too-many-dirty-paths:"), many_fp)
        self.assertEqual((many_fp["dirty_count"], many_fp["digest"], cache_allowed(many_fp)), (4, "", False))


if __name__ == "__main__":
    unittest.main()
