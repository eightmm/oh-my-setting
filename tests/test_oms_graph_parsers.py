"""Reference resolution precision: what a call can and cannot be linked to.

Every edge below is checked against the resolver's confidence contract in
docs/GRAPH-ENGINEERING.md: a name is linked by repo-wide lookup only when the
call could actually name that symbol, and never merely because the words agree.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.build import build, load_graph


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

    # -- Python receivers -------------------------------------------------

    def test_shell_command_words_never_link_to_python_functions(self) -> None:
        self.write("lib/tool.py", "def git():\n    pass\n\ndef wait():\n    pass\n")
        self.write("run.sh", "#!/bin/bash\ngit status\nwait\ncommand -v x\n")
        self.assertEqual(self.targets("file:run.sh"), [])

    def test_shell_bare_word_still_reaches_a_shell_function_by_name(self) -> None:
        self.write("lib/common.sh", "helper() {\n  echo hi\n}\n")
        self.write("run.sh", "#!/bin/bash\nhelper\n")
        self.assertEqual(self.targets("file:run.sh"), [("symbol:lib/common.sh::helper", "INFERRED")])

    def test_attribute_call_on_a_parameter_or_local_yields_no_edge(self) -> None:
        self.write("pkg/query.py", "class Graph:\n    def node(self, ident):\n        return ident\n")
        self.write("pkg/other.py", "def node():\n    pass\n")
        self.write("pkg/user.py", "def use(node, spec):\n    node.get('kind')\n    spec.get('x')\n    route = {}\n    route.items()\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"), [])

    def test_attribute_call_on_a_module_variable_yields_no_edge(self) -> None:
        self.write("pkg/a2a.py", "def parser():\n    pass\n")
        self.write("pkg/cli.py", "import argparse\nparser = argparse.ArgumentParser()\nparser.add_argument('--x')\n\ndef main():\n    parser.parse_args()\n")
        self.assertEqual(self.targets("file:pkg/cli.py"), [])
        self.assertEqual(self.targets("symbol:pkg/cli.py::main"), [])

    def test_builtin_names_are_not_resolved_repo_wide(self) -> None:
        self.write("pkg/io.py", "def open():\n    pass\n\ndef format():\n    pass\n")
        self.write("pkg/user.py", "def use():\n    open('x')\n    format('y')\n    print(len([]))\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"), [])

    def test_builtin_name_shadowed_in_the_same_file_is_a_fact(self) -> None:
        self.write("pkg/user.py", "def open():\n    pass\n\ndef use():\n    open('x')\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"), [("symbol:pkg/user.py::open", "EXTRACTED")])

    def test_bare_name_never_resolves_to_a_method(self) -> None:
        self.write("pkg/route.py", "class Test:\n    def route(self):\n        pass\n")
        self.write("pkg/user.py", "def use():\n    route()\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"), [])

    def test_inherited_self_call_still_reaches_a_method_by_name(self) -> None:
        self.write("pkg/base.py", "class Base:\n    def step(self):\n        pass\n")
        self.write("pkg/child.py", "from pkg.base import Base\n\nclass Child(Base):\n    def run(self):\n        self.step()\n")
        self.assertEqual(self.targets("symbol:pkg/child.py::Child.run"), [("symbol:pkg/base.py::Base.step", "INFERRED")])

    def test_unique_and_duplicate_bare_python_names_keep_their_confidence(self) -> None:
        self.write("a.py", "def caller():\n    unique()\n    duplicate()\n")
        self.write("b.py", "def duplicate(): pass\n")
        self.write("c.py", "def unique(): pass\ndef duplicate(): pass\n")
        targets = self.targets("symbol:a.py::caller")
        self.assertIn(("symbol:c.py::unique", "INFERRED"), targets)
        self.assertTrue(any(confidence == "AMBIGUOUS" for _, confidence in targets))

    # -- import bindings --------------------------------------------------

    def test_from_package_import_submodule_binds_the_module_file(self) -> None:
        self.write("pkg/__init__.py", "")
        self.write("pkg/runner.py", "def run():\n    pass\n")
        self.write("pkg/cli.py", "from . import runner\n\ndef main():\n    runner.run()\n")
        self.write("tool.py", "from pkg import runner\n\ndef main():\n    runner.run()\n")
        self.assertEqual(self.targets("symbol:pkg/cli.py::main"), [("symbol:pkg/runner.py::run", "EXTRACTED")])
        self.assertEqual(self.targets("symbol:tool.py::main"), [("symbol:pkg/runner.py::run", "EXTRACTED")])
        imports = {(edge["source"], edge["target"]) for edge in self.edges("imports")}
        self.assertIn(("file:pkg/cli.py", "module:pkg/runner.py"), imports)
        self.assertIn(("file:tool.py", "module:pkg/runner.py"), imports)

    def test_module_alias_member_missing_from_the_module_stays_unresolved(self) -> None:
        self.write("pkg/__init__.py", "")
        self.write("pkg/events.py", "def append_event():\n    pass\n")
        self.write("pkg/other.py", "def project():\n    pass\n")
        self.write("pkg/cli.py", "from . import events\n\ndef main():\n    events.project()\n")
        self.assertEqual(self.targets("symbol:pkg/cli.py::main"), [])

    def test_imported_class_method_resolves_to_the_method(self) -> None:
        self.write("pkg/__init__.py", "")
        self.write("pkg/query.py", "class Graph:\n    @staticmethod\n    def node(ident):\n        return ident\n")
        self.write("pkg/user.py", "from pkg.query import Graph\n\ndef use():\n    Graph.node('x')\n    Graph()\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"),
                         [("symbol:pkg/query.py::Graph", "EXTRACTED"), ("symbol:pkg/query.py::Graph.node", "EXTRACTED")])

    def test_same_file_class_method_call_resolves_to_the_method(self) -> None:
        self.write("pkg/query.py", "class Graph:\n    @staticmethod\n    def node(ident):\n        return ident\n\ndef use():\n    Graph.node('x')\n")
        self.assertEqual(self.targets("symbol:pkg/query.py::use"), [("symbol:pkg/query.py::Graph.node", "EXTRACTED")])

    def test_a_local_shadowing_a_same_file_function_yields_no_edge(self) -> None:
        self.write("pkg/user.py", "def helper():\n    pass\n\ndef use(helper):\n    helper()\n\ndef other():\n    helper()\n")
        self.assertEqual(self.targets("symbol:pkg/user.py::use"), [])
        self.assertEqual(self.targets("symbol:pkg/user.py::other"), [("symbol:pkg/user.py::helper", "EXTRACTED")])

    def test_base_class_through_a_module_alias_is_a_dependency_fact(self) -> None:
        self.write("pkg/__init__.py", "")
        self.write("pkg/errors.py", "class CoreError(Exception):\n    pass\n")
        self.write("pkg/child.py", "from pkg import errors\n\nclass GraphError(errors.CoreError):\n    pass\n")
        self.assertEqual(self.targets("symbol:pkg/child.py::GraphError", "depends_on"),
                         [("symbol:pkg/errors.py::CoreError", "EXTRACTED")])


if __name__ == "__main__":
    unittest.main()
