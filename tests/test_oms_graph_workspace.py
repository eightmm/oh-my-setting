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

    def test_clean_repository_is_cacheable_and_stable(self) -> None:
        self.assertTrue(self.clean["digest"])
        self.assertEqual(self.clean["unsafe"], "")
        self.assertEqual(self.clean["dirty_count"], 0)
        self.assertTrue(cache_allowed(self.clean))
        self.assertEqual(workspace_fingerprint(self.repo)["digest"], self.clean["digest"])

    def test_uncommitted_edit_changes_the_digest_and_reverting_restores_it(self) -> None:
        self.write("tracked.txt", "two\n")
        self.assertNotEqual(self.digest(), self.clean["digest"])
        self.write("tracked.txt", "one\n")
        self.assertEqual(self.digest(), self.clean["digest"])

    def test_staging_the_same_bytes_differs_from_the_unstaged_edit(self) -> None:
        self.write("tracked.txt", "two\n")
        unstaged = self.digest()
        self.git("add", "tracked.txt")
        staged = self.digest()
        self.assertNotEqual(staged, unstaged)
        self.assertNotEqual(staged, self.clean["digest"])

    def test_staged_blobs_separate_workspaces_whose_worktree_bytes_match(self) -> None:
        """`MM` with the committed bytes restored: only `staged` tells these apart."""
        self.write("tracked.txt", "two\n")
        self.git("add", "tracked.txt")
        self.write("tracked.txt", "one\n")
        first = self.digest()
        self.write("tracked.txt", "three\n")
        self.git("add", "tracked.txt")
        self.write("tracked.txt", "one\n")
        self.assertNotEqual(self.digest(), first)

    def test_untracked_file_changes_the_digest_and_removing_it_restores_it(self) -> None:
        self.write("new.txt", "fresh\n")
        self.assertNotEqual(self.digest(), self.clean["digest"])
        (self.repo / "new.txt").unlink()
        self.assertEqual(self.digest(), self.clean["digest"])

    def test_ignored_file_is_not_workspace_content(self) -> None:
        self.write("ignored.txt", "noise\n")
        self.assertEqual(self.digest(), self.clean["digest"])

    def test_oms_state_is_not_workspace_content(self) -> None:
        self.write(".oms/graph/runs/r1/events.jsonl", '{"kind":"node-finished"}\n')
        fingerprint = workspace_fingerprint(self.repo)
        self.assertEqual(fingerprint["digest"], self.clean["digest"])
        self.assertEqual(fingerprint["dirty_count"], 0)
        self.git("add", ".oms/graph/runs/r1/events.jsonl")  # also invisible once staged
        self.assertEqual(self.digest(), self.clean["digest"])

    def test_symlink_fails_closed(self) -> None:
        os.symlink("tracked.txt", str(self.repo / "link.txt"))
        fingerprint = workspace_fingerprint(self.repo)
        self.assertTrue(str(fingerprint["unsafe"]).startswith("symlink:"), fingerprint)
        self.assertEqual(fingerprint["digest"], "")
        self.assertFalse(cache_allowed(fingerprint))

    def test_oversized_file_fails_closed(self) -> None:
        self.write("big.txt", "x" * 64)
        fingerprint = workspace_fingerprint(self.repo, max_file_bytes=16)
        self.assertTrue(str(fingerprint["unsafe"]).startswith("large-file:"), fingerprint)
        self.assertEqual(fingerprint["digest"], "")
        self.assertFalse(cache_allowed(fingerprint))

    def test_too_many_dirty_paths_fails_closed(self) -> None:
        for index in range(4):
            self.write("dirty%d.txt" % index, "%d\n" % index)
        fingerprint = workspace_fingerprint(self.repo, max_entries=3)
        self.assertTrue(str(fingerprint["unsafe"]).startswith("too-many-dirty-paths:"), fingerprint)
        self.assertEqual(fingerprint["dirty_count"], 4)
        self.assertEqual(fingerprint["digest"], "")
        self.assertFalse(cache_allowed(fingerprint))

    def test_deleted_tracked_file_changes_the_digest(self) -> None:
        (self.repo / "tracked.txt").unlink()
        fingerprint = workspace_fingerprint(self.repo)
        self.assertEqual(fingerprint["unsafe"], "")
        self.assertEqual(fingerprint["dirty_count"], 1)
        self.assertNotEqual(fingerprint["digest"], self.clean["digest"])

    def test_staged_rename_parses_and_changes_the_digest(self) -> None:
        self.git("mv", "tracked.txt", "renamed.txt")
        fingerprint = workspace_fingerprint(self.repo)
        self.assertEqual(fingerprint["unsafe"], "")
        self.assertEqual(fingerprint["dirty_count"], 1)
        self.assertNotEqual(fingerprint["digest"], self.clean["digest"])


if __name__ == "__main__":
    unittest.main()
