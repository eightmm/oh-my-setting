from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
import sys
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.blast import blast_radius, changed_paths
from oms_graph.project.affected import affected_plan
from oms_graph.errors import GraphError
from oms_graph import render
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
        self.write("pkg/sub.py", "def work(): pass\n")
        self.write("namespace.py", "from pkg import sub\ndef use():\n    sub.work()\n")
        graph = self.graph()
        calls = [edge for edge in graph["edges"] if edge["relation"] == "calls" and edge["source"] == "symbol:a.py::caller"]
        by_target = {edge["target"]: edge for edge in calls}
        self.assertEqual(by_target["symbol:b.py::imported"]["confidence"], "EXTRACTED")
        self.assertEqual(by_target["symbol:c.py::unique"]["confidence"], "INFERRED")
        ambiguous = {target: edge for target, edge in by_target.items() if target.endswith("::duplicate")}
        self.assertEqual(set(ambiguous), {"symbol:c.py::duplicate", "symbol:d.py::duplicate"})
        self.assertTrue(all(edge["confidence"] == "AMBIGUOUS" for edge in ambiguous.values()))
        self.assertTrue(all(edge["evidence"]["candidate_count"] == 2 for edge in ambiguous.values()))
        self.assertTrue(all("candidates" not in edge["evidence"] for edge in ambiguous.values()))
        self.assertTrue(any(
            edge["source"] == "file:namespace.py"
            and edge["target"] == "module:pkg/sub.py"
            and edge["relation"] == "imports"
            for edge in graph["edges"]
        ))
        self.assertTrue(any(
            edge["source"] == "symbol:namespace.py::use"
            and edge["target"] == "symbol:pkg/sub.py::work"
            and edge["relation"] == "calls"
            for edge in graph["edges"]
        ))

    def test_summary_file_api_source_search_and_unparsed_coverage(self) -> None:
        self.write(
            "lease.py",
            '"""Lease lifecycle utilities."""\n\n'
            "class Lease:\n"
            '    """Coordinates task leases."""\n'
            "    def recover(self, task, *, force=False):\n"
            '        """Recover an expired task lease."""\n'
            '        marker = "RECOVER_MARKER"\n'
            "        return task, force, marker\n\n"
            "def task_journal_update(task):\n"
            "    return task\n",
        )
        self.write("frontend.ts", "export function renderGraph() {}\n")
        graph = self.graph()
        index = Graph(graph)
        recover = index.nodes["symbol:lease.py::Lease.recover"]
        self.assertEqual(recover["summary"], "Recover an expired task lease.")
        self.assertEqual(index.find("recover expired lease", limit=1)[0]["id"], recover["id"])

        api = index.file_api("lease.py")
        self.assertEqual(api["path"], "lease.py")
        self.assertEqual(api["content_trust"], "untrusted-source-data")
        self.assertEqual(api["total_symbols"], 3)
        self.assertFalse(api["truncated"])
        self.assertEqual(
            [row["qualname"] for row in api["symbols"]],
            ["Lease", "Lease.recover", "task_journal_update"],
        )
        self.assertEqual(api["symbols"][1]["signature"], "def Lease.recover(self, task, *, force=...)")
        found = index.search(self.repo, "recover_marker", limit=10)
        self.assertEqual(found["total_hits"], 1)
        self.assertEqual(found["groups"][0]["id"], "symbol:lease.py::Lease.recover")
        self.assertEqual(found["groups"][0]["hits"][0]["line"], 7)

        normalized = Graph({"nodes": [{
            "id": "symbol:lib/context.py::context_pack", "kind": "function",
            "name": "context_pack", "path": "lib/context.py", "summary": None,
            "metadata": {"qualname": "context_pack"},
        }], "edges": []})
        self.assertGreaterEqual(normalized.find("context pack")[0]["score"], 70)

        status = check(self.repo, state=self.state)
        self.assertEqual(status["coverage"]["parsed"], 1)
        self.assertEqual(status["coverage"]["unparsed"], 1)
        self.assertEqual(status["coverage"]["unparsed_by_extension"], {".ts": 1})
        pack = context_pack(self.repo, graph, task="recover expired lease", max_files=2, state=self.state)
        self.assertEqual(pack["coverage"], status["coverage"])

    def test_trace_is_a_bounded_projection_with_explicit_loss(self) -> None:
        nodes = [{"id": "root", "kind": "file", "path": "root.py"}]
        nodes.extend({"id": "n%d" % position, "kind": "function", "path": "n%d.py" % position}
                     for position in range(8))
        edges = []
        for position in range(8):
            edges.append({
                "source": "root", "target": "n%d" % position, "relation": "calls",
                "confidence": "AMBIGUOUS",
                "evidence": {"path": "root.py", "line": position + 1,
                             "source_digest": "f" * 64, "candidates": ["n7"]},
            })
        traced = Graph({"nodes": nodes, "edges": edges}).trace(
            "root", depth=1, max_nodes=4, max_edges=2,
        )
        self.assertEqual(len(traced["nodes"]), 4)
        self.assertEqual(len(traced["edges"]), 2)
        self.assertTrue(traced["truncated"])
        self.assertTrue(traced["node_limit_reached"])
        self.assertEqual(traced["omitted_edges"], 6)
        self.assertEqual(traced["limits"], {"nodes": 4, "edges": 2})
        self.assertTrue(all(set(row["evidence"]) <= {"path", "line"} for row in traced["edges"]))

    def test_shell_markdown_tests_and_blast(self) -> None:
        self.write("scripts/lib/functions.sh", "helper() { :; }\n")
        self.write(
            "scripts/run.sh",
            'source "$ROOT/scripts/lib/functions.sh"\n'
            "VALUE=1 helper\n"
            "result=$(helper)\n"
            "if ! helper; then :; fi\n"
            "helper | sed -n 1p\n"
            "local_helper() { :; }\n"
            "local_helper\n"
            "bash scripts/next.sh\n",
        )
        self.write("scripts/next.sh", "#!/usr/bin/env bash\n")
        self.write("docs/use.md", "Run `scripts/run.sh`.\n")
        self.write("tests/test_run.py", "# scripts/run.sh\nfrom scripts.run import thing\n")
        self.write("test/verify_feature.py", "# scripts/next.sh\n")
        graph = self.graph()
        edges = graph["edges"]
        self.assertTrue(any(edge["source"] == "file:scripts/run.sh" and edge["relation"] == "imports" for edge in edges))
        self.assertTrue(any(edge["source"] == "document:docs/use.md" and edge["relation"] == "references" for edge in edges))
        self.assertTrue(any(edge["source"] == "test:tests/test_run.py" and edge["relation"] == "tests" for edge in edges))
        helper_lines = {
            edge["evidence"].get("line")
            for edge in edges
            if edge["source"] == "file:scripts/run.sh"
            and edge["target"] == "symbol:scripts/lib/functions.sh::helper"
            and edge["relation"] == "calls"
        }
        self.assertEqual(helper_lines, {2, 3, 4, 5})
        self.assertTrue(any(
            edge["source"] == "file:scripts/run.sh"
            and edge["target"] == "symbol:scripts/run.sh::local_helper"
            for edge in edges
        ))
        radius = blast_radius(Graph(graph), ["scripts/next.sh"], depth=3)
        self.assertIn("scripts/run.sh", radius["files"])
        self.assertIn("test/verify_feature.py", radius["tests"])

    def test_changed_paths_uses_merge_base_and_handles_an_unborn_head(self) -> None:
        subprocess.run(["git", "-C", str(self.repo), "init", "-q", "-b", "main"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "config", "user.email", "test@example.com"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "config", "user.name", "test"], check=True)
        self.write("base.py", "BASE = 1\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "base"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "switch", "-q", "-c", "feature"], check=True)
        self.write("mine.py", "MINE = 1\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "mine"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "switch", "-q", "main"], check=True)
        self.write("onlymain.py", "MAIN = 1\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "main moved"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "switch", "-q", "feature"], check=True)
        self.assertEqual(changed_paths(self.repo, base="main")["changed"], ["mine.py"])

        unborn = Path(self.temp.name) / "unborn"
        unborn.mkdir()
        subprocess.run(["git", "-C", str(unborn), "init", "-q", "-b", "main"], check=True)
        (unborn / "a.py").write_text("A = 1\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(unborn), "add", "a.py"], check=True)
        self.assertEqual(changed_paths(unborn)["changed"], ["a.py"])

    def test_blast_projects_test_paths_from_module_dependents_and_reports_unmatched(self) -> None:
        self.write("pkg/mod.py", "def fn(): return 1\n")
        self.write("tests/test_mod.py", "from pkg.mod import fn\n\ndef test_fn():\n    assert fn() == 1\n")
        radius = blast_radius(Graph(self.graph()), ["pkg/mod.py", "scripts/oms"], depth=1)
        self.assertEqual(radius["tests"], ["tests/test_mod.py"])
        self.assertNotIn("test:tests/test_mod.py", [row["id"] for row in radius["test_cases"]])
        self.assertEqual(radius["unmatched"], ["scripts/oms"])
        self.assertEqual(radius["path_coverage"], "partial")

    def test_shell_function_references_add_case_precision_without_losing_file_fallback(self) -> None:
        self.write("scripts/a.sh", "#!/usr/bin/env bash\n")
        self.write("scripts/b.sh", "#!/usr/bin/env bash\n")
        self.write(
            "tests/suite-smoke.sh",
            "test_a() {\n  bash scripts/a.sh\n}\n\n"
            "test_b() {\n  bash scripts/b.sh\n}\n",
        )
        graph = self.graph()
        sources = {
            edge["source"]
            for edge in graph["edges"]
            if edge["relation"] == "calls" and edge["target"] == "file:scripts/a.sh"
        }
        self.assertIn("symbol:tests/suite-smoke.sh::test_a", sources)
        self.assertIn("test:tests/suite-smoke.sh", sources)
        self.assertNotIn("symbol:tests/suite-smoke.sh::test_b", sources)
        radius = blast_radius(Graph(graph), ["scripts/a.sh"], depth=1)
        self.assertEqual(
            radius["test_cases"],
            [{
                "id": "symbol:tests/suite-smoke.sh::test_a",
                "language": "shell",
                "name": "test_a",
                "path": "tests/suite-smoke.sh",
            }],
        )

    def test_affected_plan_selects_existing_cases_but_fails_open_on_uncertainty(self) -> None:
        self.write("scripts/a.sh", "#!/usr/bin/env bash\n")
        self.write("scripts/middle.sh", "#!/usr/bin/env bash\nbash scripts/a.sh\n")
        self.write("tests/suite-smoke.sh", "test_a() {\n  bash scripts/middle.sh\n}\n")
        self.write("tests/fixture.json", '{"target": "scripts/a.sh"}\n')
        graph = Graph(self.graph())

        selected = affected_plan(graph, ["scripts/a.sh"])
        self.assertEqual(selected["mode"], "affected")
        self.assertEqual(selected["tests"], ["tests/suite-smoke.sh"])
        self.assertEqual([row["name"] for row in selected["test_cases"]], ["test_a"])
        self.assertEqual(selected["reasons"], [])

        bounded = affected_plan(graph, ["scripts/a.sh"], depth=1)
        self.assertEqual(bounded["mode"], "full")
        self.assertIn("depth-truncated", bounded["reasons"])

        unmatched = affected_plan(graph, ["scripts/oms"])
        self.assertEqual(unmatched["mode"], "full")
        self.assertIn("unmatched:scripts/oms", unmatched["reasons"])

        deleted = affected_plan(graph, ["scripts/a.sh"], changes=[{"path": "scripts/a.sh", "status": "D"}])
        self.assertEqual(deleted["mode"], "full")
        self.assertIn("deleted:scripts/a.sh", deleted["reasons"])

        dirty = affected_plan(graph, ["scripts/a.sh"], workspace_exact=False)
        self.assertEqual(dirty["mode"], "full")
        self.assertIn("workspace-differs-from-head", dirty["reasons"])

        self.write("lib/first.py", "def possible(): pass\n")
        self.write("lib/second.py", "def possible(): pass\n")
        self.write("tests/test_possible.py", "class PossibleTest:\n    def test_possible(self): possible()\n")
        graph = Graph(self.graph())
        possible = affected_plan(graph, ["lib/second.py"])
        self.assertEqual(possible["mode"], "full")
        self.assertIn("no-tests", possible["reasons"])
        self.assertGreaterEqual(possible["ignored_confidence_counts"]["AMBIGUOUS"], 1)

        self.write("docs/note.md", "# note\n")
        self.write(".github/workflows/test.yml", "name: test\n")
        self.write("nested/AGENTS.md", "# policy\n")
        graph = Graph(self.graph())
        self.assertEqual(affected_plan(graph, ["docs/note.md"])["mode"], "affected")
        boundary = affected_plan(graph, [".github/workflows/test.yml"])
        self.assertEqual(boundary["mode"], "full")
        self.assertIn("boundary:.github/workflows/test.yml", boundary["reasons"])
        nested_policy = affected_plan(graph, ["nested/AGENTS.md"])
        self.assertEqual(nested_policy["mode"], "full")
        self.assertIn("boundary:nested/AGENTS.md", nested_policy["reasons"])

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
        self.assertEqual(pack["project_graph_revision"], graph["revision"])
        self.assertTrue((self.state / pack["pack_path"]).is_file() if not pack["pack_path"].startswith("context/") else (self.state / pack["pack_path"]).is_file())

        self.write("src/needle.py", "def needle(): return 1\n")
        self.write("src/middle.py", "from src.needle import needle\n")
        self.write("src/orphan.py", "def orphan(): return 1\n")
        self.write("src/uncertain_a.py", "def uncertain(): return 1\n")
        self.write("src/uncertain_b.py", "def uncertain(): return 2\n")
        self.write("src/uncertain_caller.py", "def caller():\n    return uncertain()\n")
        self.write("tests/a_indirect.py", "from src.middle import needle\n\nclass IndirectTest:\n    def test_indirect(self): needle()\n")
        self.write("tests/z_direct.py", "from src.needle import needle\n\nclass DirectTest:\n    def test_direct(self): needle()\n")
        graph = self.graph()
        ranked = context_pack(self.repo, graph, task="change needle", max_files=1, state=self.state)
        self.assertEqual(ranked["tests"], ["tests/z_direct.py"])
        self.assertEqual([row["name"] for row in ranked["test_cases"]], ["test_direct"])
        self.assertEqual(len({Graph(graph).nodes[ident].get("path", ident) for ident in ranked["entries"]}),
                         len(ranked["entries"]))
        self.assertLessEqual(len(ranked["blast"]["dependents"]), 40)
        self.assertEqual(ranked["assurance"]["basis"], "structural-evidence")
        pack_assurance = {row["id"]: row for row in ranked["assurance"]["nodes"]}
        self.assertEqual(pack_assurance["symbol:src/needle.py::needle"]["assurance"], "supported")
        self.assertEqual(
            pack_assurance["symbol:src/needle.py::needle"]["signals"]["extracted_test_paths"],
            ["tests/z_direct.py"],
        )
        fragment = render.render_project_html_fragment(
            graph["nodes"], graph["edges"], revision=graph["revision"],
            focus_ids=ranked["entries"], focus_paths=ranked["files"], limit=40,
        )
        self.assertTrue(fragment.startswith("<section id=\"oms-graph-view-"))
        self.assertNotIn("<!doctype", fragment.lower())
        self.assertIn("d3@7.9.0", fragment)
        self.assertIn("Instruction draft", fragment)
        self.assertIn("Copy text", fragment)
        self.assertIn("Send to Codex", fragment)
        self.assertIn("draft.focus()", fragment)
        self.assertIn("window.openai.toolOutput", fragment)
        self.assertIn("Continue the current user request", fragment)
        self.assertIn("Refresh task context first", fragment)
        self.assertIn("Do not add a new test by default", fragment)
        self.assertIn("Focus + 1 hop", fragment)
        self.assertIn("Zoom in", fragment)
        self.assertIn("Zoom out", fragment)
        self.assertIn("Graph nodes (text alternative)", fragment)
        self.assertIn("narrowLabelIds", fragment)
        self.assertIn("EXTRACTED", fragment)
        match = re.search(r'<script id="[^"]+-data" type="application/json">(.*?)</script>', fragment, re.S)
        self.assertIsNotNone(match)
        model = json.loads(match.group(1))
        self.assertEqual(model["schema"], 3)
        self.assertEqual(model["display"]["default_scope"], "neighbors")
        self.assertGreater(model["counts"]["hidden_test_nodes"], 0)
        self.assertTrue(all(row["kind"] != "test" and not row["path"].startswith("tests/")
                            for row in model["nodes"]))
        assurance_by_id = {row["id"]: row["assurance"] for row in model["nodes"]}
        self.assertEqual(assurance_by_id["symbol:src/needle.py::needle"], "supported")
        self.assertEqual(assurance_by_id["symbol:src/orphan.py::orphan"], "needs-evidence")
        self.assertEqual(assurance_by_id["file:src/uncertain_caller.py"], "needs-evidence")
        self.assertEqual(assurance_by_id["symbol:src/uncertain_caller.py::caller"], "attention")
        self.assertEqual(assurance_by_id["symbol:src/uncertain_a.py::uncertain"], "needs-evidence")
        needle = next(row for row in model["nodes"] if row["id"] == "symbol:src/needle.py::needle")
        self.assertTrue(needle["focus"])
        self.assertEqual(needle["distance"], 0)
        self.assertEqual(needle["signals"]["extracted_test_paths"], ["tests/z_direct.py"])
        self.assertLess(len(fragment.encode("utf-8")), 1024 * 1024)
        self.assertEqual(fragment, render.render_project_html_fragment(
            graph["nodes"], graph["edges"], revision=graph["revision"],
            focus_ids=ranked["entries"], focus_paths=ranked["files"], limit=40,
        ))
        app = render.render_project_mcp_app()
        self.assertIn("text alternative", app)
        self.assertIn('"resource_template":true', app)
        self.assertIn("window.openai.toolOutput", app)
        self.assertLess(len(app.encode("utf-8")), 1024 * 1024)
        included = build(self.repo, state=self.state, include=("keep.py",))
        self.assertEqual(list(included["files"]), ["keep.py"])

    def test_context_change_overlay_separates_changed_from_extracted_impact(self) -> None:
        self.write("src/base.py", "def base(): return 1\n")
        self.write("src/dependent.py", "from src.base import base\n\ndef dependent(): return base()\n")
        subprocess.run(["git", "-C", str(self.repo), "init", "-q", "-b", "main"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "config", "user.email", "test@example.com"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "config", "user.name", "test"], check=True)
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "base"], check=True)
        self.write("src/base.py", "def base(): return 2\n")
        graph = self.graph()
        pack = context_pack(self.repo, graph, task="change base", max_files=4, base="HEAD", state=self.state)
        self.assertEqual(pack["change"]["changed"], ["src/base.py"])
        self.assertIn("src/dependent.py", pack["change"]["impacted"])
        self.assertEqual(pack["change"]["impact_basis"], "EXTRACTED reverse dependencies")
        fragment = render.render_project_html_fragment(
            graph["nodes"], graph["edges"], revision=graph["revision"],
            focus_ids=pack["entries"], focus_paths=pack["files"],
            changed_paths=pack["change"]["changed"], impacted_paths=pack["change"]["impacted"],
            limit=40,
        )
        model = json.loads(re.search(
            r'<script id="[^"]+-data" type="application/json">(.*?)</script>', fragment, re.S,
        ).group(1))
        changes = {row["change"] for row in model["nodes"] if row["path"] in ("src/base.py", "src/dependent.py")}
        self.assertEqual(changes, {"changed", "impacted"})

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
