"""Reference resolution precision: what a call can and cannot be linked to.

Every edge below is checked against the resolver's confidence contract in
docs/GRAPH-ENGINEERING.md: a name is linked by repo-wide lookup only when the
call could actually name that symbol, and never merely because the words agree.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
import warnings
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.build import build, load_graph
from oms_graph.project.parsers.shell import ShellParser


class ResolutionPrecisionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="oms-graph-parsers.")
        self.repo = Path(self.temp.name) / "repo"
        self.state = Path(self.temp.name) / "state"
        self.repo.mkdir()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write(self, path: str, text: str) -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def edges(self, relation: str = "calls"):
        build(self.repo, state=self.state)
        return [edge for edge in load_graph(self.repo, state=self.state)["edges"] if edge["relation"] == relation]

    def targets(self, source: str, relation: str = "calls"):
        return sorted((edge["target"], edge["confidence"]) for edge in self.edges(relation) if edge["source"] == source)

    def shell_refs(self, text: str):
        return ShellParser().parse("tests/example.sh", text, "digest").refs

    def test_case_patterns_do_not_become_calls_but_same_line_bodies_do(self) -> None:
        refs = self.shell_refs(
            "case $path in\n"
            "  scripts/check.sh|scripts/lib/tool.py) bash scripts/run.sh ;;\n"
            "esac\n"
        )
        calls = [ref["value"] for ref in refs if ref["relation"] == "calls" and ref["kind"] == "path"]
        self.assertEqual(calls, ["scripts/run.sh"])

    def test_uncertain_case_patterns_keep_path_edges(self) -> None:
        cases = (
            ("case $path in\n  'scripts/check.sh'|scripts/lib/tool.py) : ;;\nesac\n", "scripts/check.sh"),
            ("case $path in\n  $(scripts/check.sh)|scripts/lib/tool.py) : ;;\nesac\n", "scripts/check.sh"),
            ("case $path in\n  scripts/check.sh|\\\n    scripts/lib/tool.py) : ;;\nesac\n", "scripts/check.sh"),
            ("scripts/check.sh|scripts/lib/tool.py) : ;;\n", "scripts/check.sh"),
            (
                "case x in\n  x)\n    (\n      scripts/real.sh)\n    ;;\n esac\n",
                "scripts/real.sh",
            ),
            (
                "case x in\n  x) : ;; esac\n (\n  scripts/real.sh)\n",
                "scripts/real.sh",
            ),
            (
                "case x in\n  x) case y in\n    scripts/check.sh) : ;;\n  esac ;;\nesac\n",
                "scripts/check.sh",
            ),
            (
                "case x in\n  x)\n    printf 'case esac'\n    scripts/check.sh)\n    ;;\nesac\n",
                "scripts/check.sh",
            ),
        )
        for text, expected in cases:
            with self.subTest(text=text):
                self.assertTrue(any(
                    ref.get("value") == expected and ref.get("relation") == "calls"
                    for ref in self.shell_refs(text)
                ))

    def test_python_stdin_heredoc_emits_top_level_module_imports(self) -> None:
        refs = self.shell_refs(
            "run() {\n"
            "  PYTHONDONTWRITEBYTECODE=1 python3 - \"$ROOT/scripts/lib\" <<'PY'\n"
            "import panel_metrics, oms_graph.project.build as build\n"
            "from oms_runtime.common import canonical_json as canon\n"
            "def nested():\n"
            "    import ignored\n"
            "PY\n"
            "}\n"
        )
        imports = [ref for ref in refs if ref["relation"] == "imports"]
        self.assertEqual(
            {(ref["value"], ref.get("imported", ""), ref["binding"], ref["line"], ref["from"])
             for ref in imports},
            {
                ("panel_metrics", "", "panel_metrics", 3, "file:tests/example.sh"),
                ("oms_graph.project.build", "", "build", 3, "file:tests/example.sh"),
                ("oms_runtime.common", "canonical_json", "canon", 4, "file:tests/example.sh"),
                ("panel_metrics", "", "panel_metrics", 3, "symbol:tests/example.sh::run"),
                ("oms_graph.project.build", "", "build", 3, "symbol:tests/example.sh::run"),
                ("oms_runtime.common", "canonical_json", "canon", 4, "symbol:tests/example.sh::run"),
            },
        )
        tabbed = self.shell_refs(
            "python3 - <<-'PY'\n"
            "\timport panel_metrics\n"
            "PY\n"
        )
        self.assertTrue(any(ref.get("value") == "panel_metrics" and ref.get("line") == 2 for ref in tabbed))
        long_line = "#" + ("x" * 4100)
        after_long_line = self.shell_refs(
            "python3 - <<'PY'\n" + long_line + "\nimport panel_metrics\nPY\n"
        )
        self.assertTrue(any(
            ref.get("value") == "panel_metrics" and ref.get("line") == 3
            for ref in after_long_line
        ))
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            self.shell_refs('python3 <<PY\nprint("\\$HOME")\nPY\n')
        self.assertFalse(any(issubclass(item.category, SyntaxWarning) for item in caught))
        self.assertFalse(any(
            ref["relation"] == "imports" and ref["kind"] == "module"
            for ref in self.shell_refs("cat <<EOF\nimport panel_metrics\nEOF\n")
        ))

    def test_shell_words_link_to_shell_functions_not_python(self) -> None:
        self.write("scripts/lib/tool.py", "def git():\n    pass\n\ndef wait():\n    pass\n")
        self.write("lib/common.sh", "helper() {\n  echo hi\n}\n")
        self.write(
            "run.sh",
            "#!/bin/bash\n"
            "python3 - <<'PY'\n"
            "import tool\n"
            "from tool import git\n"
            "PY\n"
            "git status\nwait\ncommand -v x\n",
        )
        self.write("call_helper.sh", "#!/bin/bash\nhelper\n")
        self.assertEqual(self.targets("file:run.sh"), [])
        self.assertTrue(any(
            edge["source"] == "file:run.sh"
            and edge["target"] == "module:scripts/lib/tool.py"
            and edge["relation"] == "imports"
            for edge in self.edges("imports")
        ))
        self.assertEqual(self.targets("file:call_helper.sh"), [("symbol:lib/common.sh::helper", "INFERRED")])

    def test_python_receiver_and_builtin_resolution(self) -> None:
        self.write("pkg/query.py", "class Graph:\n    def node(self, ident):\n        return ident\n")
        self.write("pkg/other.py", "def node():\n    pass\n")
        self.write("pkg/user.py", "def use(node, spec):\n    node.get('kind')\n    spec.get('x')\n    route = {}\n    route.items()\n")
        self.write("pkg/a2a.py", "def parser():\n    pass\n")
        self.write("pkg/cli.py", "import argparse\nparser = argparse.ArgumentParser()\nparser.add_argument('--x')\n\ndef main():\n    parser.parse_args()\n")
        self.write("pkg/io.py", "def open():\n    pass\n\ndef format():\n    pass\n")
        self.write("pkg/builtins_user.py", "def use():\n    open('x')\n    format('y')\n    print(len([]))\n")
        self.write("pkg/shadowed.py", "def open():\n    pass\n\ndef use():\n    open('x')\n")
        self.write("pkg/route.py", "class Test:\n    def route(self):\n        pass\n")
        self.write("pkg/bare_method.py", "def use():\n    route()\n")
        self.write("pkg/base.py", "class Base:\n    def run(self):\n        return self.step()\n    def step(self):\n        pass\n")
        self.write("pkg/child.py", "from pkg.base import Base\n\nclass Child(Base):\n    def run(self):\n        self.step()\n")
        for src in ("symbol:pkg/user.py::use", "file:pkg/cli.py", "symbol:pkg/cli.py::main",
                    "symbol:pkg/builtins_user.py::use", "symbol:pkg/bare_method.py::use"):
            self.assertEqual(self.targets(src), [], src)
        self.assertEqual(self.targets("symbol:pkg/shadowed.py::use"), [("symbol:pkg/shadowed.py::open", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:pkg/base.py::Base.run"), [("symbol:pkg/base.py::Base.step", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:pkg/child.py::Child.run"), [("symbol:pkg/base.py::Base.step", "INFERRED")])
        self.assertEqual(self.targets("symbol:pkg/child.py::Child", "depends_on"), [("symbol:pkg/base.py::Base", "EXTRACTED")])

    def test_import_bindings_and_class_methods(self) -> None:
        self.write("pkg/__init__.py", "")
        self.write("pkg/runner.py", "def run():\n    pass\n")
        self.write("pkg/cli.py", "from . import runner\n\ndef main():\n    runner.run()\n")
        self.write("tool.py", "from pkg import runner\n\ndef main():\n    runner.run()\n")
        self.write("pkg/events.py", "def append_event():\n    pass\n")
        self.write("pkg/other.py", "def project():\n    pass\n")
        self.write("pkg/missing_member.py", "from . import events\n\ndef main():\n    events.project()\n")
        self.write("pkg/query.py", "class Graph:\n    @staticmethod\n    def node(ident):\n        return ident\n\ndef same_file():\n    Graph.node('x')\n")
        self.write("pkg/user.py", "from pkg.query import Graph\n\ndef use():\n    Graph.node('x')\n    Graph()\n")
        self.write("pkg/shadow_local.py", "def helper():\n    pass\n\ndef use(helper):\n    helper()\n\ndef other():\n    helper()\n")
        self.write("pkg/errors.py", "class CoreError(Exception):\n    pass\n")
        self.write("pkg/child.py", "from pkg import errors\n\nclass GraphError(errors.CoreError):\n    pass\n")

        self.assertEqual(self.targets("symbol:pkg/cli.py::main"), [("symbol:pkg/runner.py::run", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:tool.py::main"), [("symbol:pkg/runner.py::run", "EXTRACTED")])
        imports = {(edge["source"], edge["target"]) for edge in self.edges("imports")}
        self.assertIn(("file:pkg/cli.py", "module:pkg/runner.py"), imports)
        self.assertIn(("file:tool.py", "module:pkg/runner.py"), imports)
        self.assertEqual(self.targets("symbol:pkg/missing_member.py::main"), [])
        self.assertEqual(self.targets("symbol:pkg/user.py::use"),
                         [("symbol:pkg/query.py::Graph", "EXTRACTED"), ("symbol:pkg/query.py::Graph.node", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:pkg/query.py::same_file"), [("symbol:pkg/query.py::Graph.node", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:pkg/shadow_local.py::use"), [])
        self.assertEqual(self.targets("symbol:pkg/shadow_local.py::other"), [("symbol:pkg/shadow_local.py::helper", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:pkg/child.py::GraphError", "depends_on"),
                         [("symbol:pkg/errors.py::CoreError", "EXTRACTED")])


if __name__ == "__main__":
    unittest.main()
