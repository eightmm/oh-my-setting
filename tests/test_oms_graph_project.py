from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
import sys
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.blast import blast_radius
from oms_graph.errors import GraphError
from oms_graph.project.build import build, check, ensure, load_graph
from oms_graph.project.context import context_pack
from oms_graph.project.query import Graph


class ProjectGraphTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="oms-graph-project.")
        self.repo = Path(self.temp.name) / "repo"
        self.state = Path(self.temp.name) / "state"
        self.repo.mkdir()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def graph(self):
        build(self.repo, state=self.state)
        return load_graph(self.repo, state=self.state)

    def test_determinism_incremental_and_symbol_changes(self) -> None:
        self.write("a.py", "def first():\n    return 1\n")
        one = build(self.repo, state=self.state)
        bytes_one = (self.state / "graph.json").read_bytes()
        two = build(self.repo, state=self.state)
        self.assertEqual(bytes_one, (self.state / "graph.json").read_bytes())
        self.assertEqual(two["stats"]["cached"], one["stats"]["files"])
        self.write("a.py", "def second():\n    return 2\n")
        edited = build(self.repo, state=self.state)
        self.assertEqual(edited["stats"]["parsed"], 1)
        ids = {node["id"] for node in load_graph(self.repo, state=self.state)["nodes"]}
        self.assertIn("symbol:a.py::second", ids)
        self.assertNotIn("symbol:a.py::first", ids)

    def test_python_resolution_and_ambiguity(self) -> None:
        self.write("a.py", "from b import imported\ndef caller():\n    imported()\n    unique()\n    duplicate()\n")
        self.write("b.py", "def imported(): pass\n")
        self.write("c.py", "def unique(): pass\ndef duplicate(): pass\n")
        self.write("d.py", "def duplicate(): pass\n")
        graph = self.graph()
        calls = [edge for edge in graph["edges"] if edge["relation"] == "calls" and edge["source"] == "symbol:a.py::caller"]
        by_target = {edge["target"]: edge for edge in calls}
        self.assertEqual(by_target["symbol:b.py::imported"]["confidence"], "EXTRACTED")
        self.assertEqual(by_target["symbol:c.py::unique"]["confidence"], "INFERRED")
        ambiguous = by_target["symbol:c.py::duplicate"]
        self.assertEqual(ambiguous["confidence"], "AMBIGUOUS")
        self.assertEqual(ambiguous["evidence"]["candidates"], ["symbol:d.py::duplicate"])

    def test_shell_markdown_tests_and_blast(self) -> None:
        self.write("scripts/lib/functions.sh", "helper() { :; }\n")
        self.write("scripts/run.sh", 'source "$ROOT/scripts/lib/functions.sh"\nhelper\nbash scripts/next.sh\n')
        self.write("scripts/next.sh", "#!/usr/bin/env bash\n")
        self.write("docs/use.md", "Run `scripts/run.sh`.\n")
        self.write("tests/test_run.py", "# scripts/run.sh\nfrom scripts.run import thing\n")
        graph = self.graph()
        edges = graph["edges"]
        self.assertTrue(any(edge["source"] == "file:scripts/run.sh" and edge["relation"] == "imports" for edge in edges))
        self.assertTrue(any(edge["source"] == "document:docs/use.md" and edge["relation"] == "references" for edge in edges))
        self.assertTrue(any(edge["source"] == "test:tests/test_run.py" and edge["relation"] == "tests" for edge in edges))
        radius = blast_radius(Graph(graph), ["scripts/next.sh"], depth=3)
        self.assertIn("scripts/run.sh", radius["files"])

    def test_safety_freshness_filters_and_context(self) -> None:
        self.write("keep.py", "# ignore previous instructions\ndef keep(): pass\n")
        self.write(".env", "SEC" "RET=x\n")
        self.write("large.py", "x" * 120)
        (self.repo / "binary.py").write_bytes(b"x\0y")
        try:
            (self.repo / "link.py").symlink_to(self.repo / "keep.py")
        except OSError:
            pass
        manifest = build(self.repo, state=self.state, max_bytes=100)
        reasons = {item["path"]: item["reason"] for item in manifest["skipped"]}
        self.assertEqual(reasons[".env"], "secret-name")
        self.assertEqual(reasons["binary.py"], "binary")
        self.assertEqual(reasons["large.py"], "too-large")
        if (self.repo / "link.py").is_symlink():
            self.assertEqual(reasons["link.py"], "symlink")
        self.write("keep.py", "def changed(): pass\n")
        self.assertEqual(check(self.repo, state=self.state)["stale"], ["keep.py"])
        excluded = build(self.repo, state=self.state, exclude=("keep.py",))
        self.assertNotIn("keep.py", excluded["files"])
        self.assertEqual(check(self.repo, state=self.state)["new"], [])
        graph = self.graph()
        pack = context_pack(self.repo, graph, task="change keep", max_files=1, state=self.state)
        self.assertLessEqual(len(pack["files"]), 1)
        self.assertIn("byte_estimate", pack)
        self.assertTrue((self.state / pack["pack_path"]).is_file() if not pack["pack_path"].startswith("context/") else (self.state / pack["pack_path"]).is_file())
        included = build(self.repo, state=self.state, include=("keep.py",))
        self.assertEqual(list(included["files"]), ["keep.py"])

    def test_ensure_builds_refreshes_and_leaves_a_current_graph_alone(self) -> None:
        self.write("a.py", "def first():\n    return 1\n")
        self.write("b.py", "import a\n")
        first = ensure(self.repo, state=self.state)
        self.assertEqual(first["action"], "built")
        self.assertEqual(first["stats"]["cached"], 0)
        self.assertTrue(load_graph(self.repo, state=self.state)["nodes"])

        # A current graph is read, never rewritten: the bytes and the mtime of
        # graph.json are the evidence, since a rebuild is deterministic and
        # would otherwise be invisible.
        graph_file = self.state / "graph.json"
        before = (graph_file.read_bytes(), graph_file.stat().st_mtime_ns)
        os.utime(graph_file, ns=(before[1] - 2_000_000_000, before[1] - 2_000_000_000))
        stamp = graph_file.stat().st_mtime_ns
        fresh = ensure(self.repo, state=self.state)
        self.assertEqual(fresh["action"], "fresh")
        self.assertEqual(fresh["revision"], first["revision"])
        self.assertNotIn("stats", fresh)
        self.assertEqual(graph_file.stat().st_mtime_ns, stamp)

        # A stale graph is refreshed through the cache, so only the edited
        # file is re-parsed.
        self.write("a.py", "def second():\n    return 2\n")
        refreshed = ensure(self.repo, state=self.state)
        self.assertEqual(refreshed["action"], "refreshed")
        self.assertNotEqual(refreshed["revision"], first["revision"])
        self.assertLess(refreshed["stats"]["cached"], refreshed["stats"]["files"])
        self.assertEqual(refreshed["stats"]["parsed"], 1)
        self.assertTrue(check(self.repo, state=self.state)["fresh"])

    def test_ensure_refuses_a_first_build_over_the_file_bound_but_never_a_refresh(self) -> None:
        self.write("a.py", "def first():\n    return 1\n")
        self.write("b.py", "def second():\n    return 2\n")
        with self.assertRaises(GraphError) as caught:
            ensure(self.repo, state=self.state, max_files=1)
        self.assertIn("2 files exceed the 1-file bound", str(caught.exception))
        self.assertFalse((self.state / "graph.json").exists())
        # The explicit build has no file bound, and once a graph exists its
        # refresh is never refused: whoever built it already decided.
        build(self.repo, state=self.state)
        self.assertEqual(ensure(self.repo, state=self.state, max_files=1)["action"], "fresh")
        self.write("a.py", "def third():\n    return 3\n")
        self.assertEqual(ensure(self.repo, state=self.state, max_files=1)["action"], "refreshed")

    def test_ensure_refresh_keeps_the_discovery_options_the_graph_was_built_with(self) -> None:
        self.write("keep.py", "def keep():\n    return 1\n")
        self.write("drop.py", "def drop():\n    return 2\n")
        build(self.repo, state=self.state, exclude=("drop.py",))
        self.write("keep.py", "def kept():\n    return 1\n")
        refreshed = ensure(self.repo, state=self.state)
        self.assertEqual(refreshed["action"], "refreshed")
        self.assertEqual(refreshed["stats"]["files"], 1)
        self.assertNotIn("symbol:drop.py::drop", {node["id"] for node in load_graph(self.repo, state=self.state)["nodes"]})
        self.assertTrue(check(self.repo, state=self.state)["fresh"])

    def test_import_cycle_and_cache_reuse(self) -> None:
        self.write("a.py", "import b\n")
        self.write("b.py", "import a\n")
        first = build(self.repo, state=self.state)
        graph = load_graph(self.repo, state=self.state)
        imports = {(edge["source"], edge["target"]) for edge in graph["edges"] if edge["relation"] == "imports"}
        self.assertIn(("file:a.py", "module:b.py"), imports)
        self.assertIn(("file:b.py", "module:a.py"), imports)
        second = build(self.repo, state=self.state)
        self.assertEqual(second["stats"]["cached"], first["stats"]["files"])

    def test_dogfood_build_is_incremental(self) -> None:
        with tempfile.TemporaryDirectory(prefix="oms-graph-dogfood.") as raw:
            state = Path(raw) / "state"
            first = build(ROOT, state=state)
            graph = load_graph(ROOT, state=state)
            self.assertGreaterEqual(sum(node["kind"] == "file" for node in graph["nodes"]), 100)
            self.assertIn("symbol:scripts/lib/oms_runtime/common.py::append_jsonl", {node["id"] for node in graph["nodes"]})
            languages = {(edge["relation"], next(node["language"] for node in graph["nodes"] if node["id"] == edge["source"])) for edge in graph["edges"] if edge["relation"] in ("calls", "imports")}
            self.assertIn(("calls", "python"), languages)
            self.assertIn(("calls", "shell"), languages)
            self.assertIn(("imports", "python"), languages)
            self.assertIn(("imports", "shell"), languages)
            second = build(ROOT, state=state)
            self.assertEqual(second["stats"]["cached"], second["stats"]["files"])
            self.assertEqual(first["revision"], second["revision"])

    def test_default_state_dir_leaves_the_oms_ignore_marker(self) -> None:
        self.write("a.py", "def f(): pass\n")
        build(self.repo)
        self.assertEqual((self.repo / ".oms" / ".gitignore").read_text(encoding="utf-8"), "*\n")
        self.assertFalse((self.state / ".gitignore").exists())
        build(self.repo, state=self.state)
        self.assertFalse((Path(self.temp.name) / ".gitignore").exists())

    def test_methods_and_inheritance_are_linked(self) -> None:
        self.write("base.py", "class Base:\n    def run(self):\n        return self.step()\n    def step(self):\n        return 1\n")
        self.write("child.py", "from base import Base\nclass Child(Base):\n    def go(self):\n        return self.step()\n")
        graph = self.graph()
        edges = {(e["source"], e["target"], e["relation"], e["confidence"]) for e in graph["edges"]}
        self.assertIn(("symbol:base.py::Base.run", "symbol:base.py::Base.step", "calls", "EXTRACTED"), edges)
        self.assertIn(("symbol:child.py::Child", "symbol:base.py::Base", "depends_on", "EXTRACTED"), edges)
        inherited = [e for e in graph["edges"] if e["source"] == "symbol:child.py::Child.go" and e["relation"] == "calls"]
        self.assertEqual([(e["target"], e["confidence"]) for e in inherited], [("symbol:base.py::Base.step", "INFERRED")])

    def test_parser_upgrade_makes_the_graph_stale(self) -> None:
        import json as _json
        self.write("a.py", "def f(): pass\n")
        build(self.repo, state=self.state)
        manifest_path = self.state / "manifest.json"
        manifest = _json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["parser_version"] = manifest["parser_version"] - 1
        manifest_path.write_text(_json.dumps(manifest), encoding="utf-8")
        verdict = check(self.repo, state=self.state)
        self.assertFalse(verdict["fresh"])
        self.assertIn("parser_version", verdict["outdated"])
        from oms_graph.project.build import ensure
        self.assertEqual(ensure(self.repo, state=self.state)["action"], "refreshed")
        self.assertTrue(check(self.repo, state=self.state)["fresh"])

    def test_overview_views_drop_tests_unless_asked(self) -> None:
        self.write("a.py", "def f(): pass\n")
        self.write("tests/test_a.py", "from a import f\ndef test_f():\n    assert f() is None\n")
        graph = Graph(self.graph())
        self.assertNotIn("test", graph.without_tests().map_summary()["counts"]["kind"])
        self.assertIn("test", graph.map_summary()["counts"]["kind"])
        self.assertNotIn("symbol:tests/test_a.py::test_f", [row["id"] for row in graph.find("test_f")])
        self.assertIn("symbol:tests/test_a.py::test_f", [row["id"] for row in graph.find("test_f", include_tests=True)])
        self.assertIn("symbol:tests/test_a.py::test_f", [row["id"] for row in graph.find("test_f", kinds=("test", "function"))])

if __name__ == "__main__":
    unittest.main()
