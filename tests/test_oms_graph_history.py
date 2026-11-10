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
from oms_graph.errors import GraphError
from oms_graph.project import history, impact


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

    def test_impact_brief_joins_tests_dependents_history_and_owners(self) -> None:
        self.commit({"core.py": "def run():\n    return 1\n", "app.py": "import core\n\ndef main():\n    return core.run()\n",
                     "settings.json": "{}\n", "docs/guide.md": "# guide\n",
                     "tests/test_core.py": "import core\n\ndef test_run():\n    assert core.run()\n"})
        for round_ in range(6):
            self.commit({"core.py": "def run():\n    return %d\n" % round_, "settings.json": '{"round": %d}\n' % round_})
        self.commit({"CODEOWNERS": "# owners\n*        @everyone\n/docs/    @writers\n*.py      @py-team @lead\n"
                                   "[Section]\n!negated  @nobody\n/app.py\n"})
        base = self.git("rev-parse", "HEAD").strip()
        self.commit({"core.py": "def run():\n    return 99\n", "docs/guide.md": "# guide 2\n", "app.py": "import core\n"})
        build(self.repo, state=self.state)
        graph = load_graph(self.repo, state=self.state)
        brief = impact.impact_brief(self.repo, graph, base=base, commits=100)
        self.assertEqual((brief["kind"], brief["truncated"], brief["omitted"]), ("impact-brief", False, {}))
        self.assertIn("not evidence", brief["note"])
        self.assertEqual([row["path"] for row in brief["changes"]], ["app.py", "core.py", "docs/guide.md"])
        self.assertIn("tests/test_core.py", brief["affected"]["tests"])
        self.assertEqual(brief["documentation_paths"], ["docs/guide.md"])
        # app.py depends on core.py but is itself in the range, and tests are listed apart: neither is a dependent here.
        self.assertNotIn("app.py", brief["dependents"]["files"])
        self.assertNotIn("tests/test_core.py", brief["dependents"]["files"])
        # History, not the parsers, says settings.json moves with core.py; this range left it alone.
        partners = {row["path"]: row for row in brief["co_change"]["not_in_range"]}
        self.assertEqual(list(partners), ["settings.json"])
        self.assertEqual((partners["settings.json"]["with"], partners["settings.json"]["structural"]), ("core.py", False))
        # The last matching rule wins; a rule without owners clears them; unsupported lines are skipped.
        self.assertEqual(brief["owners"]["file"], "CODEOWNERS")
        owners = {row["path"]: (row["owners"], row["pattern"]) for row in brief["owners"]["paths"]}
        self.assertEqual(owners, {"app.py": ([], "/app.py"), "core.py": (["@py-team", "@lead"], "*.py"),
                                  "docs/guide.md": (["@writers"], "/docs/")})
        text = impact.brief_text(brief)
        self.assertTrue(text.startswith("impact brief %s..HEAD (orientation only" % base))
        self.assertIn("  settings.json  with core.py", text)
        self.assertIn("  app.py  (no owner)\n", text)

        cut = impact.impact_brief(self.repo, graph, base=base, commits=100, limit=1)
        self.assertEqual(len(cut["changes"]), 1)
        self.assertEqual((cut["omitted"]["changes"], cut["omitted"]["owners.paths"], cut["truncated"]), (2, 2, True))
        self.assertIn("omitted: changes +2 (raise --limit)", impact.brief_text(cut))
        with self.assertRaises(GraphError):
            impact.impact_brief(self.repo, graph, base=base, limit=0)

        # No CODEOWNERS, or one that is a symlink, is no owners section; the file is never followed or guessed.
        (self.repo / "CODEOWNERS").unlink()
        self.assertIsNone(impact.impact_brief(self.repo, graph, base=base, commits=100)["owners"])
        (self.repo / "CODEOWNERS").symlink_to(self.repo / "settings.json")
        self.assertEqual(impact.read_codeowners(self.repo), ("", []))

    def test_codeowners_patterns_follow_last_match_and_anchoring(self) -> None:
        rules = impact.parse_codeowners("*.js @js\n/build/logs/ @logs\ndocs/* @docs\napps/ @apps\n**/gen/** @gen\nsrc/[ab].py @skip\n")
        cases = {"lib/x.js": ["@js"], "build/logs/a/b.txt": ["@logs"], "x/build/logs/a.txt": [],
                 "docs/a.md": ["@docs"], "docs/sub/a.md": [], "deep/apps/x.py": ["@apps"], "apps": [],
                 "a/gen/b/c.py": ["@gen"], "src/a.py": []}
        self.assertEqual({path: impact.owners_for(rules, path)[0] for path in cases}, cases)



if __name__ == "__main__":
    unittest.main()
