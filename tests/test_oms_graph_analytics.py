from __future__ import annotations
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph import ROUTE_STATUSES
from oms_graph import render
from oms_graph.project import analytics

FIXTURE_DIR = ROOT / "tests" / "fixtures" / "graph-routes"
SPEC_DIR = ROOT / "config" / "graphs"
REQUIRED_KEYS = {"name", "facts", "outcomes", "gates", "expect"}
OPTIONAL_KEYS = {"repeats", "steps", "bindings"}


def node(node_id, kind="function", path="", name=""):
    return {
        "id": node_id,
        "kind": kind,
        "name": name or node_id,
        "path": path,
        "language": "python",
        "source_digest": "0" * 8,
        "summary": None,
        "crux": None,
        "metadata": {},
    }


def edge(source, target, relation="calls"):
    return {
        "source": source,
        "target": target,
        "relation": relation,
        "confidence": "EXTRACTED",
        "evidence": {"path": "", "line": 1},
    }


def load_spec(name):
    return json.loads((SPEC_DIR / ("%s.json" % name)).read_text(encoding="utf-8"))


class AnalyticsTest(unittest.TestCase):
    def test_structural_assurance_is_provider_neutral_and_counts_unresolved_sites(self):
        nodes = [
            node("file:src/covered.py", kind="file", path="src/covered.py"),
            node("symbol:src/covered.py::covered", path="src/covered.py"),
            node("symbol:src/covered.py::duplicate", path="src/covered.py", name="duplicate"),
            node("symbol:src/caller.py::caller", path="src/caller.py"),
            node("symbol:src/other.py::duplicate", path="src/other.py", name="duplicate"),
            node("symbol:src/inferred.py::inferred", path="src/inferred.py"),
            node("test:tests/test_covered.py", kind="test", path="tests/test_covered.py"),
            node("symbol:tests/test_covered.py::test_covered", path="tests/test_covered.py"),
        ]
        ambiguous_a = edge("symbol:src/caller.py::caller", "symbol:src/covered.py::duplicate")
        ambiguous_a.update({"confidence": "AMBIGUOUS", "evidence": {
            "path": "src/caller.py", "line": 7,
            "candidate_count": 2,
        }})
        ambiguous_b = edge("symbol:src/caller.py::caller", "symbol:src/other.py::duplicate")
        ambiguous_b.update({"confidence": "AMBIGUOUS", "evidence": {
            "path": "src/caller.py", "line": 7,
            "candidate_count": 2,
        }})
        inferred = edge("symbol:src/inferred.py::inferred", "symbol:src/covered.py::covered")
        inferred["confidence"] = "INFERRED"
        test_link = edge("symbol:tests/test_covered.py::test_covered", "symbol:src/covered.py::covered")
        second_test_link = edge("symbol:tests/test_covered.py::test_covered", "symbol:src/covered.py::covered")
        second_test_link["evidence"]["line"] = 2
        edges = [
            test_link,
            second_test_link,
            ambiguous_a,
            ambiguous_b,
            inferred,
        ]

        projection = analytics.structural_assurance(nodes, edges)
        states = projection["nodes"]
        self.assertEqual(projection["schema"], 1)
        self.assertEqual(projection["basis"], "structural-evidence")
        self.assertNotIn("test:tests/test_covered.py", states)
        self.assertEqual(states["symbol:src/covered.py::covered"]["assurance"], "supported")
        self.assertEqual(
            states["symbol:src/covered.py::covered"]["signals"]["extracted_test_paths"],
            ["tests/test_covered.py"],
        )
        self.assertEqual(states["symbol:src/caller.py::caller"]["assurance"], "attention")
        self.assertEqual(states["symbol:src/caller.py::caller"]["signals"]["ambiguous_sites"], 1)
        self.assertEqual(states["symbol:src/inferred.py::inferred"]["signals"]["inferred_sites"], 1)
        evidence_degree = analytics.evidence_degrees(nodes, edges)
        self.assertEqual(evidence_degree["symbol:src/caller.py::caller"]["out"], 1)
        self.assertEqual(evidence_degree["symbol:tests/test_covered.py::test_covered"]["out"], 2)
        import types
        self.assertIn("file:a.py", analytics._node_index([types.MappingProxyType({"id": "file:a.py"})]))

class ExecRenderTest(unittest.TestCase):
    def setUp(self):
        self.spec = load_spec("coding-change")

    def test_exec_text_rendering(self):
        text = render.render_exec_text(self.spec)
        lines = text.splitlines()
        for node_id in self.spec["nodes"]:
            self.assertTrue(any(line.startswith("%s %s (" % (render.GLYPH_PENDING, node_id)) for line in lines))
        positions = [
            next(index for index, line in enumerate(lines) if line.startswith("%s %s (" % (render.GLYPH_PENDING, node_id)))
            for node_id in ("inspect", "implement", "review", "land", "acceptance", "done")
        ]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("  └── completed → review", text)
        self.assertIn("failed, unverified → implement %s" % render.REPEAT_MARK, text)
        self.assertEqual(text, render.render_exec_text(self.spec))

        projection = {
            "nodes": {
                "inspect": {"status": "finished", "outcome": "completed", "claimed_outcome": "completed", "attempts": 1},
                "implement": {"status": "finished", "outcome": "unverified", "claimed_outcome": "completed", "attempts": 1},
                "land": {"status": "active", "outcome": None, "claimed_outcome": None, "attempts": 1},
            },
            "steps": 3, "repeats": {}, "gates": {},
        }
        route = {"status": "gate", "primary": "review", "alternatives": [], "reason": "implement needs review"}
        styled = render.render_exec_text(self.spec, projection, route)
        for expected in (
            "%s inspect (tool) [completed]" % render.GLYPH_DONE,
            "%s implement (agent) [unverified]" % render.GLYPH_FAILED,
            "%s land (agent)" % render.GLYPH_ACTIVE,
            "%s review (gate)" % render.GLYPH_GATE,
            "%s done (terminal)" % render.GLYPH_PENDING,
        ):
            self.assertIn(expected, styled)
        review_line = [line for line in styled.splitlines() if line.startswith(render.GLYPH_GATE)][0]
        self.assertTrue(review_line.endswith("← next"), review_line)

    def test_exec_mermaid_rendering(self):
        text = render.render_exec_mermaid(self.spec)
        lines = text.splitlines()
        self.assertEqual(lines[0], "flowchart TD")
        self.assertIn('    inspect["inspect\\n(tool)"]', lines)
        self.assertEqual(len([line for line in lines if line.endswith('"]')]), len(self.spec["nodes"]))
        arrows = [line for line in lines if "-->" in line or "-.->" in line]
        self.assertEqual(len(arrows), len(self.spec["edges"]))
        self.assertIn('    implement -.->|"failed, unverified"| implement', arrows)
        self.assertNotIn("classDef", text)

        projection = {
            "nodes": {
                "inspect": {"status": "finished", "outcome": "completed"},
                "implement": {"status": "finished", "outcome": "failed"},
                "review": {"status": "active", "outcome": None},
            },
            "steps": 2, "repeats": {}, "gates": {},
        }
        styled = render.render_exec_mermaid(self.spec, projection)
        for expected in ("classDef done", "    class inspect done;", "    class implement failed;", "    class review active;"):
            self.assertIn(expected, styled)

        spec = {
            "schema": 1, "id": "sanitise", "entry": "file:a/b.py",
            "nodes": {"file:a/b.py": {"kind": "tool"}, "file-a-b-py": {"kind": "tool"}, "9lives": {"kind": "terminal"}},
            "edges": [{"from": "file:a/b.py", "to": "9lives", "outcomes": ["completed"]}],
        }
        sanitised = render.render_exec_mermaid(spec)
        for expected in (
            '    file_a_b_py["file-a-b-py\\n(tool)"]',
            '    file_a_b_py_2["file:a/b.py\\n(tool)"]',
            '    n_9lives["9lives\\n(terminal)"]',
            '    file_a_b_py_2 -->|"completed"| n_9lives',
        ):
            self.assertIn(expected, sanitised)


class ProjectRenderTest(unittest.TestCase):
    def test_project_map_text_rendering(self):
        summary = {
            "revision": "abc123",
            "counts": {
                "kind": {"function": 12, "file": 30},
                "language": {"python": 40, "shell": 2},
                "confidence": {"EXTRACTED": 35, "INFERRED": 5, "AMBIGUOUS": 2},
            },
            "assurance": {"schema": 1, "basis": "structural-evidence",
                          "counts": {"supported": 20, "needs-evidence": 18, "attention": 4}},
            "coverage": {"parsed": 42, "unparsed": 3, "unparsed_by_extension": {".ts": 2, ".go": 1}},
            "hubs": [{"id": "module:scripts/lib/x.py", "kind": "module", "degree": 9}],
            "groups": {"scripts": ["a", "b", "c", "d", "e", "f"], "tests": ["t1"]},
        }
        text = render.render_project_map_text(summary)
        lines = text.splitlines()
        self.assertLess(
            next(index for index, line in enumerate(lines) if line.startswith("  file ")),
            next(index for index, line in enumerate(lines) if line.startswith("  function ")),
        )
        for expected in (
            "project graph (revision abc123)", "nodes by kind:", "  file      30", "  python  40",
            "edges by confidence:", "structural assurance:", "  supported       20",
            "parser coverage: parsed 42, unparsed 3", "unparsed extensions: .go=1, .ts=2",
            "   1. module:scripts/lib/x.py  (module)  degree 9",
            "  scripts (6): a, b, c, d, e, +1 more", "  tests (1): t1",
        ):
            self.assertIn(expected, text)

        empty = render.render_project_map_text({})
        for expected in ("project graph (revision unknown)", "nodes by kind: (none)", "top hubs: (none)", "module groups: (none)"):
            self.assertIn(expected, empty)


class RouteFixtureTest(unittest.TestCase):
    def test_route_fixtures_are_valid_and_cover_documented_routes(self):
        from oms_graph import OUTCOMES

        paths = sorted(FIXTURE_DIR.glob("*.json"))
        self.assertGreaterEqual(len(paths), 14)
        names = set()
        statuses = set()
        for path in paths:
            with self.subTest(fixture=path.name):
                payload = json.loads(path.read_text(encoding="utf-8"))
                self.assertIsInstance(payload, dict)
                self.assertNotIn(payload["name"], names, "duplicate fixture name in %s" % path.name)
                names.add(payload["name"])
                keys = set(payload)
                self.assertEqual(len(keys & {"spec_ref", "spec"}), 1)
                self.assertTrue(REQUIRED_KEYS <= keys)
                self.assertEqual(keys - REQUIRED_KEYS - OPTIONAL_KEYS - {"spec_ref", "spec"}, set())
                self.assertTrue(isinstance(payload["name"], str) and payload["name"].strip())
                for key in ("facts", "outcomes", "gates", "expect"):
                    self.assertIsInstance(payload[key], dict)
                if "repeats" in payload:
                    self.assertIsInstance(payload["repeats"], dict)
                    for key, value in payload["repeats"].items():
                        self.assertIn("->", key)
                        self.assertIsInstance(value, int)
                if "steps" in payload:
                    self.assertIsInstance(payload["steps"], int)
                if "bindings" in payload:
                    self.assertIsInstance(payload["bindings"], dict)
                    for name, value in payload["bindings"].items():
                        self.assertRegex(name, r"^[A-Za-z_][A-Za-z0-9_-]{0,63}$")
                        self.assertIsInstance(value.get("task_id") if isinstance(value, dict) else value, str)

                spec = load_spec(payload["spec_ref"]) if "spec_ref" in payload else payload["spec"]
                self.assertIsInstance(spec, dict)
                nodes = spec.get("nodes") or {}
                known = set(nodes)
                self.assertTrue(known)

                status = payload["expect"].get("status")
                self.assertIn(status, ROUTE_STATUSES)
                statuses.add(status)
                for node_id, outcome in payload["outcomes"].items():
                    self.assertIn(node_id, known)
                    self.assertIn(outcome, OUTCOMES)
                for node_id, decision in payload["gates"].items():
                    self.assertIn(node_id, known)
                    node_spec = nodes.get(node_id) or {}
                    self.assertEqual(node_spec.get("kind"), "gate")
                    self.assertIn(decision, node_spec.get("decisions") or ["approved", "changes_requested"])
                primary = payload["expect"].get("primary")
                if primary is not None:
                    self.assertIn(primary, known)
                for node_id in payload["expect"].get("downgrades", []):
                    self.assertIn(node_id, known)

        for expected in ("actionable", "gate", "terminal", "exhausted", "invalid"):
            self.assertIn(expected, statuses)


if __name__ == "__main__":
    unittest.main()
