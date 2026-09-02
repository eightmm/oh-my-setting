"""Co-change coupling: Git history read as evidence, never as a dependency."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.build import build, load_graph
from oms_graph.project import history


class CouplingPureTest(unittest.TestCase):
    def test_degree_is_shared_over_average_revisions(self) -> None:
        sets = [{"files": ["a", "b"]}] * 6 + [{"files": ["a"]}] * 6 + [{"files": ["c", "b"]}]
        rows = history.coupling(sets, min_shared=5, min_degree=30)
        self.assertEqual(len(rows), 1)
        # a: 12 revisions, b: 7, shared 6 -> 6 / 9.5 * 100
        self.assertEqual((rows[0]["a"], rows[0]["b"], rows[0]["shared_revs"], rows[0]["revs"], rows[0]["degree"]),
                         ("a", "b", 6, [12, 7], 63.2))

    def test_thresholds_and_focus(self) -> None:
        sets = [{"files": ["a", "b", "c"]}] * 5
        self.assertEqual(len(history.coupling(sets, min_shared=6, min_degree=0)), 0)
        self.assertEqual(len(history.coupling(sets, min_shared=5, min_degree=0)), 3)
        focused = history.coupling(sets, min_shared=5, min_degree=0, focus={"c"})
        self.assertEqual(sorted((row["a"], row["b"]) for row in focused), [("a", "c"), ("b", "c")])

    def test_structural_annotation_uses_path_pairs_of_non_contains_edges(self) -> None:
        graph = {"nodes": [{"id": "file:a.py", "path": "a.py"}, {"id": "symbol:a.py::f", "path": "a.py"},
                           {"id": "module:b.py", "path": "b.py"}],
                 "edges": [{"source": "file:a.py", "target": "symbol:a.py::f", "relation": "contains"},
                           {"source": "symbol:a.py::f", "target": "module:b.py", "relation": "imports"}]}
        pairs = history.structural_pairs(graph)
        self.assertEqual(pairs, {("a.py", "b.py")})
        rows = history.annotate([{"a": "a.py", "b": "b.py"}, {"a": "a.py", "b": "conf.json"}], pairs)
        self.assertEqual([row["structural"] for row in rows], [True, False])


class CouplingRepoTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="oms-graph-history.")
        self.repo = Path(self.temp.name) / "repo"
        self.state = Path(self.temp.name) / "state"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def git(self, *args: str) -> str:
        return subprocess.run(["git", "-C", str(self.repo), *args], capture_output=True, text=True, check=True).stdout

    def commit(self, files, message="c") -> None:
        for path, text in files.items():
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def test_report_marks_coupling_the_parsers_cannot_see(self) -> None:
        self.commit({"a.py": "import b\n", "b.py": "x = 1\n", "settings.json": "{}\n"})
        for round_ in range(6):
            self.commit({"a.py": "import b\n# %d\n" % round_, "settings.json": '{"round": %d}\n' % round_,
                         "b.py": "x = %d\n" % round_})
        self.commit({"lonely.md": "# alone\n"})
        # A bulk commit couples everything: skipped, and counted as such.
        self.commit({"bulk/%02d.txt" % index: "%d\n" % index for index in range(60)})
        build(self.repo, state=self.state)
        graph = load_graph(self.repo, state=self.state)
        report = history.coupling_report(self.repo, graph, commits=100, max_changeset=50, min_shared=5, min_degree=30)
        self.assertEqual(report["skipped_bulk"], 1)
        self.assertEqual(report["commits"], 8)
        pairs = {(row["a"], row["b"]): row for row in report["pairs"]}
        self.assertIn(("a.py", "b.py"), pairs)
        self.assertTrue(pairs[("a.py", "b.py")]["structural"], "a.py imports b.py: the graph sees this coupling")
        self.assertIn(("a.py", "settings.json"), pairs)
        self.assertFalse(pairs[("a.py", "settings.json")]["structural"], "config to code: only history sees it")
        self.assertEqual(report["hidden"], sum(1 for row in report["pairs"] if not row["structural"]))
        self.assertFalse(report["truncated"])

    def test_deleted_files_and_limit(self) -> None:
        for round_ in range(6):
            self.commit({"a.py": "# %d\n" % round_, "gone.py": "# %d\n" % round_, "c.py": "# %d\n" % round_, "d.py": "# %d\n" % round_})
        (self.repo / "gone.py").unlink()
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "remove")
        report = history.coupling_report(self.repo, None, commits=100, min_shared=5, min_degree=30, limit=1)
        self.assertTrue(all("gone.py" not in (row["a"], row["b"]) for row in report["pairs"]))
        self.assertEqual(len(report["pairs"]), 1)
        self.assertTrue(report["truncated"])
        self.assertEqual(report["omitted"], 2, "a/c, a/d, c/d remain once gone.py is dropped")
        self.assertEqual(report["graph_revision"], "")
        focused = history.coupling_report(self.repo, None, commits=100, min_shared=5, min_degree=30, focus=["c.py"])
        self.assertTrue(all("c.py" in (row["a"], row["b"]) for row in focused["pairs"]))


if __name__ == "__main__":
    unittest.main()
