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

class ExecRenderTest(unittest.TestCase):
    def setUp(self):
        self.spec = load_spec("coding-change")

    def test_exec_text_lists_every_node_in_route_order(self):
        text = render.render_exec_text(self.spec)
        lines = text.splitlines()
        for node_id in self.spec["nodes"]:
            self.assertTrue(
                any(line.startswith("%s %s (" % (render.GLYPH_PENDING, node_id)) for line in lines),
                "missing node line for %s" % node_id,
            )
        positions = [
            next(index for index, line in enumerate(lines) if line.startswith("%s %s (" % (render.GLYPH_PENDING, node_id)))
            for node_id in ("inspect", "implement", "review", "land", "acceptance", "done")
        ]
        self.assertEqual(positions, sorted(positions), "entry-first topological order expected")
        self.assertIn("  └── completed → review", text)
        self.assertIn("%s" % render.REPEAT_MARK, text)
        self.assertIn("failed, unverified → implement %s" % render.REPEAT_MARK, text)
        self.assertEqual(text, render.render_exec_text(self.spec), "rendering must be deterministic")

    def test_exec_text_shows_glyphs_outcomes_and_the_next_step(self):
        projection = {
            "nodes": {
                "inspect": {"status": "finished", "outcome": "completed", "claimed_outcome": "completed", "attempts": 1},
                "implement": {"status": "finished", "outcome": "unverified", "claimed_outcome": "completed", "attempts": 1},
                "land": {"status": "active", "outcome": None, "claimed_outcome": None, "attempts": 1},
            },
            "steps": 3,
            "repeats": {},
            "gates": {},
        }
        route = {"status": "gate", "primary": "review", "alternatives": [], "reason": "implement needs review"}
        text = render.render_exec_text(self.spec, projection, route)
        self.assertIn("%s inspect (tool) [completed]" % render.GLYPH_DONE, text)
        self.assertIn("%s implement (agent) [unverified]" % render.GLYPH_FAILED, text)
        self.assertIn("%s land (agent)" % render.GLYPH_ACTIVE, text)
        self.assertIn("%s review (gate)" % render.GLYPH_GATE, text)
        self.assertIn("← next", text)
        review_line = [line for line in text.splitlines() if line.startswith(render.GLYPH_GATE)][0]
        self.assertTrue(review_line.endswith("← next"), review_line)
        self.assertIn("%s done (terminal)" % render.GLYPH_PENDING, text)

    def test_exec_mermaid_emits_one_line_per_node_and_edge(self):
        text = render.render_exec_mermaid(self.spec)
        lines = text.splitlines()
        self.assertEqual(lines[0], "flowchart TD")
        self.assertIn('    inspect["inspect\\n(tool)"]', lines)
        self.assertEqual(len([line for line in lines if line.endswith('"]')]), len(self.spec["nodes"]))
        arrows = [line for line in lines if "-->" in line or "-.->" in line]
        self.assertEqual(len(arrows), len(self.spec["edges"]))
        self.assertIn('    implement -.->|"failed, unverified"| implement', arrows)
        self.assertNotIn("classDef", text)

    def test_exec_mermaid_styles_nodes_only_with_a_projection(self):
        projection = {
            "nodes": {
                "inspect": {"status": "finished", "outcome": "completed"},
                "implement": {"status": "finished", "outcome": "failed"},
                "review": {"status": "active", "outcome": None},
            },
            "steps": 2,
            "repeats": {},
            "gates": {},
        }
        text = render.render_exec_mermaid(self.spec, projection)
        self.assertIn("classDef done", text)
        self.assertIn("    class inspect done;", text)
        self.assertIn("    class implement failed;", text)
        self.assertIn("    class review active;", text)

    def test_mermaid_ids_are_sanitised_and_unique(self):
        spec = {
            "schema": 1,
            "id": "sanitise",
            "entry": "file:a/b.py",
            "nodes": {"file:a/b.py": {"kind": "tool"}, "file-a-b-py": {"kind": "tool"}, "9lives": {"kind": "terminal"}},
            "edges": [{"from": "file:a/b.py", "to": "9lives", "outcomes": ["completed"]}],
        }
        text = render.render_exec_mermaid(spec)
        self.assertIn('    file_a_b_py["file-a-b-py\\n(tool)"]', text)
        self.assertIn('    file_a_b_py_2["file:a/b.py\\n(tool)"]', text)
        self.assertIn('    n_9lives["9lives\\n(terminal)"]', text)
        self.assertIn('    file_a_b_py_2 -->|"completed"| n_9lives', text)


class ProjectRenderTest(unittest.TestCase):
    def test_project_map_text_reports_counts_hubs_and_groups(self):
        summary = {
            "revision": "abc123",
            "counts": {
                "kind": {"function": 12, "file": 30},
                "language": {"python": 40, "shell": 2},
                "confidence": {"EXTRACTED": 35, "INFERRED": 5, "AMBIGUOUS": 2},
            },
            "assurance": {"schema": 1, "basis": "structural-evidence",
                          "counts": {"supported": 20, "needs-evidence": 18, "attention": 4}},
            "coverage": {"parsed": 42, "unparsed": 3,
                         "unparsed_by_extension": {".ts": 2, ".go": 1}},
            "hubs": [{"id": "module:scripts/lib/x.py", "kind": "module", "degree": 9}],
            "groups": {"scripts": ["a", "b", "c", "d", "e", "f"], "tests": ["t1"]},
        }
        text = render.render_project_map_text(summary)
        self.assertIn("project graph (revision abc123)", text)
        self.assertIn("nodes by kind:", text)
        lines = text.splitlines()
        self.assertLess(
            next(index for index, line in enumerate(lines) if line.startswith("  file ")),
            next(index for index, line in enumerate(lines) if line.startswith("  function ")),
            "counts are ordered by descending count",
        )
        self.assertIn("  file      30", lines)
        self.assertIn("  python  40", text)
        self.assertIn("edges by confidence:", text)
        self.assertIn("structural assurance:", text)
        self.assertIn("  supported       20", text)
        self.assertIn("parser coverage: parsed 42, unparsed 3", text)
        self.assertIn("unparsed extensions: .go=1, .ts=2", text)
        self.assertIn("   1. module:scripts/lib/x.py  (module)  degree 9", text)
        self.assertIn("  scripts (6): a, b, c, d, e, +1 more", text)
        self.assertIn("  tests (1): t1", text)

    def test_project_map_text_survives_an_empty_summary(self):
        text = render.render_project_map_text({})
        self.assertIn("project graph (revision unknown)", text)
        self.assertIn("nodes by kind: (none)", text)
        self.assertIn("top hubs: (none)", text)
        self.assertIn("module groups: (none)", text)


class RouteFixtureTest(unittest.TestCase):
    """Fixtures are data for W1's route evaluator; this guards their shape only."""

    def fixture_paths(self):
        return sorted(FIXTURE_DIR.glob("*.json"))

    def test_the_fixture_suite_covers_the_documented_routes(self):
        paths = self.fixture_paths()
        self.assertGreaterEqual(len(paths), 14, "at least 14 route fixtures are required")
        names = set()
        for path in paths:
            payload = json.loads(path.read_text(encoding="utf-8"))
            self.assertNotIn(payload["name"], names, "duplicate fixture name in %s" % path.name)
            names.add(payload["name"])
        statuses = set()
        for path in paths:
            statuses.add(json.loads(path.read_text(encoding="utf-8"))["expect"]["status"])
        for expected in ("actionable", "gate", "terminal", "exhausted", "invalid"):
            self.assertIn(expected, statuses, "no fixture exercises status %s" % expected)

    def test_every_fixture_is_structurally_valid(self):
        for path in self.fixture_paths():
            with self.subTest(fixture=path.name):
                raw = path.read_text(encoding="utf-8")
                payload = json.loads(raw)
                self.assertIsInstance(payload, dict)
                keys = set(payload)
                self.assertEqual(
                    len(keys & {"spec_ref", "spec"}), 1,
                    "exactly one of spec_ref/spec is required",
                )
                self.assertTrue(REQUIRED_KEYS <= keys, "missing keys: %s" % sorted(REQUIRED_KEYS - keys))
                extra = keys - REQUIRED_KEYS - OPTIONAL_KEYS - {"spec_ref", "spec"}
                self.assertEqual(extra, set(), "unexpected keys: %s" % sorted(extra))

                self.assertIsInstance(payload["name"], str)
                self.assertTrue(payload["name"].strip())
                for key in ("facts", "outcomes", "gates", "expect"):
                    self.assertIsInstance(payload[key], dict, "%s must be an object" % key)
                if "repeats" in payload:
                    self.assertIsInstance(payload["repeats"], dict)
                    for key, value in payload["repeats"].items():
                        self.assertIn("->", key, "repeat keys are 'from->to'")
                        self.assertIsInstance(value, int)
                if "steps" in payload:
                    self.assertIsInstance(payload["steps"], int)
                if "bindings" in payload:
                    # `name: task_id` shorthand or a projection row with task_id.
                    self.assertIsInstance(payload["bindings"], dict)
                    for name, value in payload["bindings"].items():
                        self.assertRegex(name, r"^[A-Za-z_][A-Za-z0-9_-]{0,63}$")
                        if isinstance(value, dict):
                            self.assertIsInstance(value.get("task_id"), str)
                        else:
                            self.assertIsInstance(value, str)

                if "spec_ref" in payload:
                    spec_path = SPEC_DIR / ("%s.json" % payload["spec_ref"])
                    self.assertTrue(spec_path.is_file(), "spec_ref does not resolve: %s" % payload["spec_ref"])
                    spec = json.loads(spec_path.read_text(encoding="utf-8"))
                else:
                    spec = payload["spec"]
                    self.assertIsInstance(spec, dict)
                known = set(spec.get("nodes") or {})
                self.assertTrue(known, "the referenced spec declares no nodes")

                status = payload["expect"].get("status")
                self.assertIn(status, ROUTE_STATUSES, "unknown route status %r" % status)
                for node_id in payload["outcomes"]:
                    self.assertIn(node_id, known, "outcome names an unknown node")
                for node_id in payload["gates"]:
                    self.assertIn(node_id, known, "gate names an unknown node")
                primary = payload["expect"].get("primary")
                if primary is not None:
                    self.assertIn(primary, known, "expect.primary names an unknown node")
                for node_id in payload["expect"].get("downgrades", []):
                    self.assertIn(node_id, known, "downgrade names an unknown node")

    def test_fixture_outcomes_and_gates_use_the_spec_vocabulary(self):
        from oms_graph import OUTCOMES

        for path in self.fixture_paths():
            with self.subTest(fixture=path.name):
                payload = json.loads(path.read_text(encoding="utf-8"))
                if "spec_ref" in payload:
                    spec = load_spec(payload["spec_ref"])
                else:
                    spec = payload["spec"]
                nodes = spec.get("nodes") or {}
                for node_id, outcome in payload["outcomes"].items():
                    self.assertIn(outcome, OUTCOMES, "%s claims an unknown outcome" % node_id)
                for node_id, decision in payload["gates"].items():
                    node_spec = nodes.get(node_id) or {}
                    self.assertEqual(node_spec.get("kind"), "gate", "%s is not a gate node" % node_id)
                    decisions = node_spec.get("decisions") or ["approved", "changes_requested"]
                    self.assertIn(decision, decisions, "%s records an undeclared decision" % node_id)


if __name__ == "__main__":
    unittest.main()
