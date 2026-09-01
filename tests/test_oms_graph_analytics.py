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
OPTIONAL_KEYS = {"repeats", "steps"}


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
    def test_degrees_count_every_node_and_ignore_unknown_endpoints(self):
        nodes = [node("a"), node("b"), node("c")]
        edges = [edge("a", "b"), edge("b", "c"), edge("a", "ghost"), edge("ghost", "c")]
        counts = analytics.degrees(nodes, edges)
        self.assertEqual(sorted(counts), ["a", "b", "c"])
        self.assertEqual(counts["a"], {"in": 0, "out": 1, "total": 1})
        self.assertEqual(counts["b"], {"in": 1, "out": 1, "total": 2})
        self.assertEqual(counts["c"], {"in": 1, "out": 0, "total": 1})

    def test_degrees_count_a_self_loop_on_both_sides(self):
        counts = analytics.degrees([node("a")], [edge("a", "a")])
        self.assertEqual(counts["a"], {"in": 1, "out": 1, "total": 2})

    def test_hubs_respect_limit_and_kind_filter(self):
        nodes = [node("hub", kind="module"), node("leaf1"), node("leaf2"), node("leaf3")]
        edges = [edge("hub", "leaf1"), edge("hub", "leaf2"), edge("leaf3", "hub")]
        top = analytics.hubs(nodes, edges, limit=2)
        self.assertEqual([row["id"] for row in top], ["hub", "leaf1"])
        self.assertEqual(top[0], {"id": "hub", "kind": "module", "degree": 3})
        modules = analytics.hubs(nodes, edges, limit=10, kinds=("module",))
        self.assertEqual([row["id"] for row in modules], ["hub"])
        self.assertEqual(modules[0]["degree"], 3, "kind filter must not change the measured degree")

    def test_connected_components_are_sorted_by_size_then_first_id(self):
        nodes = [node(name) for name in ("a", "aa", "b", "c", "m", "n", "z")]
        edges = [edge("b", "a"), edge("a", "c"), edge("n", "m")]
        groups = analytics.connected_components(nodes, edges)
        self.assertEqual(
            groups,
            [["a", "b", "c"], ["m", "n"], ["aa"], ["z"]],
            "larger components come first even when a singleton sorts earlier",
        )

    def test_connected_components_directed_are_strongly_connected(self):
        nodes = [node(name) for name in ("a", "b", "c", "d")]
        edges = [edge("a", "b"), edge("b", "c"), edge("c", "a"), edge("c", "d")]
        self.assertEqual(analytics.connected_components(nodes, edges), [["a", "b", "c", "d"]])
        self.assertEqual(
            analytics.connected_components(nodes, edges, undirected=False),
            [["a", "b", "c"], ["d"]],
        )

    def test_cycles_report_a_three_cycle_and_a_self_loop_once(self):
        nodes = [node(name) for name in ("a", "b", "c", "d", "s")]
        edges = [
            edge("b", "c", relation="imports"),
            edge("c", "a", relation="imports"),
            edge("a", "b", relation="imports"),
            edge("s", "s", relation="calls"),
            edge("c", "d", relation="imports"),
            edge("d", "a", relation="references"),
        ]
        found = analytics.cycles(nodes, edges)
        self.assertEqual(found, [["s"], ["a", "b", "c"]])

    def test_cycles_honour_the_relation_filter_and_the_limit(self):
        nodes = [node(name) for name in ("a", "b", "c", "d")]
        edges = [
            edge("a", "b", relation="imports"),
            edge("b", "a", relation="imports"),
            edge("c", "d", relation="imports"),
            edge("d", "c", relation="imports"),
            edge("a", "c", relation="references"),
        ]
        self.assertEqual(len(analytics.cycles(nodes, edges, limit=1)), 1)
        self.assertEqual(analytics.cycles(nodes, edges, relations=("references",)), [])

    def test_shortest_path_returns_both_endpoints(self):
        nodes = [node(name) for name in ("a", "b", "c", "d")]
        edges = [edge("a", "b"), edge("b", "c"), edge("c", "d"), edge("a", "d")]
        self.assertEqual(analytics.shortest_path(nodes, edges, "a", "d"), ["a", "d"])
        self.assertEqual(analytics.shortest_path(nodes, edges, "b", "b"), ["b"])
        self.assertEqual(
            analytics.shortest_path(nodes, edges, "b", "d", undirected=False),
            ["b", "c", "d"],
        )

    def test_shortest_path_returns_none_when_no_path_exists(self):
        nodes = [node("a"), node("b"), node("island")]
        edges = [edge("a", "b")]
        self.assertIsNone(analytics.shortest_path(nodes, edges, "a", "island"))
        self.assertIsNone(analytics.shortest_path(nodes, edges, "a", "ghost"))
        self.assertIsNone(analytics.shortest_path(nodes, edges, "b", "a", undirected=False))

    def test_communities_split_two_clusters_joined_by_one_bridge(self):
        left = ["a1", "a2", "a3", "a4"]
        right = ["b1", "b2", "b3", "b4"]
        nodes = [node(name, path="left/%s.py" % name) for name in left]
        nodes += [node(name, path="right/%s.py" % name) for name in right]
        edges = []
        for group in (left, right):
            for first in group:
                for second in group:
                    if first < second:
                        edges.append(edge(first, second))
        edges.append(edge("a4", "b4", relation="references"))
        groups = analytics.communities(nodes, edges)
        self.assertEqual([row["id"] for row in groups], ["c1", "c2"])
        self.assertEqual(sorted(row["label"] for row in groups), ["left", "right"])
        members = sorted([row["members"] for row in groups])
        self.assertEqual(members, [left, right])

    def test_communities_label_root_and_keep_singletons(self):
        nodes = [node("alone", path="README.md")]
        groups = analytics.communities(nodes, [])
        self.assertEqual(groups, [{"id": "c1", "label": "root", "members": ["alone"]}])


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
            "counts": {"kind": {"function": 12, "file": 30}, "language": {"python": 40, "shell": 2}},
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
        self.assertIn("   1. module:scripts/lib/x.py  (module)  degree 9", text)
        self.assertIn("  scripts (6): a, b, c, d, e, +1 more", text)
        self.assertIn("  tests (1): t1", text)

    def test_project_map_text_survives_an_empty_summary(self):
        text = render.render_project_map_text({})
        self.assertIn("project graph (revision unknown)", text)
        self.assertIn("nodes by kind: (none)", text)
        self.assertIn("top hubs: (none)", text)
        self.assertIn("module groups: (none)", text)

    def test_project_mermaid_is_bounded_by_limit(self):
        nodes = [node("n%02d" % index, path="pkg/n%02d.py" % index) for index in range(10)]
        edges = [edge("n00", "n%02d" % index, relation="imports") for index in range(1, 10)]
        edges.append(edge("n08", "n09", relation="calls"))
        text = render.render_project_mermaid(nodes, edges, limit=3)
        lines = text.splitlines()
        self.assertEqual(lines[0], "flowchart LR")
        node_lines = [line for line in lines if line.endswith('"]')]
        self.assertEqual(len(node_lines), 3)
        self.assertIn('    n00["n00"]', text)
        self.assertNotIn("n01", text, "nodes below the degree cut are dropped")
        arrows = [line for line in lines if "-->" in line]
        self.assertEqual(
            sorted(arrows),
            ['    n00 -->|"imports"| n08', '    n00 -->|"imports"| n09', '    n08 -->|"calls"| n09'],
            "only edges between kept nodes are drawn",
        )
        self.assertEqual(text, render.render_project_mermaid(nodes, edges, limit=3))

    def test_project_mermaid_labels_by_name_and_dedupes_edges(self):
        nodes = [node("symbol:a.py::f", name="f"), node("symbol:b.py::g", name="g")]
        edges = [edge("symbol:a.py::f", "symbol:b.py::g", relation="calls")] * 2
        text = render.render_project_mermaid(nodes, edges)
        self.assertIn('["f"]', text)
        self.assertEqual(len([line for line in text.splitlines() if "-->" in line]), 1)


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
