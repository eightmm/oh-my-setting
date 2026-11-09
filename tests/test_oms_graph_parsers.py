"""Reference resolution precision: what a call can and cannot be linked to.

Every edge below is checked against the resolver's confidence contract in
docs/GRAPH-ENGINEERING.md: a name is linked by repo-wide lookup only when the
call could actually name that symbol, and never merely because the words agree.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
import warnings
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))

from oms_graph.project.build import build, coverage, load_graph
from oms_graph.project.context import select_context
from oms_graph.project.parsers.notebook import NotebookParser
from oms_graph.project.parsers.shell import ShellParser
from oms_graph.project.query import Graph


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

    @staticmethod
    def notebook(*cells, language: str = "python", **layout) -> str:
        """nbformat 4 text; a cell is (cell_type, source) or (cell_type, source, outputs)."""
        rows = []
        for cell in cells:
            row = {"cell_type": cell[0], "metadata": {}, "source": cell[1]}
            if cell[0] == "code":
                row.update({"execution_count": None, "outputs": list(cell[2]) if len(cell) > 2 else []})
            rows.append(row)
        return json.dumps({"cells": rows, "metadata": {"kernelspec": {"language": language, "name": language}},
                           "nbformat": 4, "nbformat_minor": 5}, **layout)

    def test_notebook_code_cells_become_symbols_at_their_file_lines(self) -> None:
        text = self.notebook(
            ("markdown", ["# Notes\n", "def prose_only(): pass\n"]),
            ("code", ["%matplotlib inline\n", "!pip list\n", "from pkg.util import helper\n", "\n",
                      "def load(path):\n", '    """Load one table."""\n', "    shared()\n", "    return helper(path)\n"],
             [{"output_type": "stream", "name": "stdout", "text": ["def leaked():\n", "    OUTPUT_ONLY_MARKER = 1\n"]}]),
            ("code", ["%%bash\n", "echo not python\n"]),
            ("code", "class Model:\n    def fit(self):\n        return load('x')\n"),
            ("code", ["x = = 1\n"]),
            indent=1,
        )
        self.write("nb/a.ipynb", text)
        self.write("nb/b.ipynb", self.notebook(("code", ["def shared():\n", "    pass\n"]), indent=1))
        self.write("pkg/__init__.py", "")
        self.write("pkg/util.py", "def helper(path):\n    return path\n")
        self.write("lib.py", "def shared():\n    pass\n")
        self.write("pkg/k.py", "class K:\n    def shared(self):\n        pass\n")
        self.write("other.py", "def main():\n    load('x')\n")

        calls = self.edges("calls")
        index = Graph(load_graph(self.repo, state=self.state))
        lines = text.split("\n")
        load = index.nodes["symbol:nb/a.ipynb::load"]
        self.assertEqual((load["kind"], load["language"], load["metadata"]["cell"]), ("function", "notebook", 2))
        self.assertEqual(lines[load["metadata"]["line"] - 1].strip(), json.dumps("def load(path):\n") + ",")
        self.assertEqual(lines[load["metadata"]["end_line"] - 1].strip(), json.dumps("    return helper(path)\n"))
        self.assertEqual(load["summary"], "Load one table.")
        # A string-valued source sits on one line of the file, so the whole cell is anchored there.
        fit = index.nodes["symbol:nb/a.ipynb::Model.fit"]["metadata"]
        self.assertEqual((fit["cell"], fit["line"], fit["end_line"]), (4, fit["line"], fit["line"]))
        self.assertTrue(lines[fit["line"] - 1].strip().startswith('"source": "class Model:'))
        # Markdown and outputs are never a source of symbols; magics and broken cells are counted, not guessed.
        self.assertEqual(sorted(node["id"] for node in index.nodes.values()
                                if node.get("path") == "nb/a.ipynb" and node["kind"] in ("class", "function", "method")),
                         ["symbol:nb/a.ipynb::Model", "symbol:nb/a.ipynb::Model.fit", "symbol:nb/a.ipynb::load"])
        module = index.nodes["module:nb/a.ipynb"]
        self.assertEqual((module["language"], module["metadata"]), ("notebook", {"code_cells": 4, "skipped_cells": 2}))

        def targets(source: str):
            return sorted((edge["target"], edge["confidence"]) for edge in calls if edge["source"] == source)

        imports = {(edge["source"], edge["target"], edge["evidence"]["line"]) for edge in self.edges("imports")}
        self.assertIn(("file:nb/a.ipynb", "module:pkg/util.py", load["metadata"]["line"] - 2), imports)
        # The import binding is a fact; the bare name reaches the Python function only, never the method or
        # the other notebook's definition of the same word.
        self.assertEqual(targets("symbol:nb/a.ipynb::load"),
                         [("symbol:lib.py::shared", "INFERRED"), ("symbol:pkg/util.py::helper", "EXTRACTED")])
        self.assertEqual(targets("symbol:nb/a.ipynb::Model.fit"), [("symbol:nb/a.ipynb::load", "EXTRACTED")])
        # Nothing imports a notebook: a Python caller's bare name does not land on a cell's function.
        self.assertEqual(targets("symbol:other.py::main"), [])

        self.assertEqual(coverage(self.repo, state=self.state)["unparsed_by_extension"], {})
        api = index.file_api("nb/a.ipynb")
        self.assertEqual(api["language"], "notebook")
        self.assertEqual([(row["qualname"], row["line"]) for row in api["symbols"]],
                         [("load", load["metadata"]["line"]), ("Model", fit["line"]), ("Model.fit", fit["line"])])
        # Context retrieval does not score raw notebook JSON either: an output-only word selects nothing.
        picked = select_context(self.repo, index, task="OUTPUT_ONLY_MARKER", state=self.state)
        self.assertFalse([node for node in picked["entries"] if "ipynb" in str(node)], picked["entries"])
        # Search previews raw lines, and a notebook's raw lines include its outputs.
        for needle in ("OUTPUT_ONLY_MARKER", "def load"):
            found = index.search(self.repo, needle)
            self.assertEqual((found["total_hits"], found["skipped_files"]), (0, 2), needle)

    def test_notebook_layouts_and_inputs_that_are_not_python(self) -> None:
        def parse(text: str):
            result = NotebookParser().parse("nb/a.ipynb", text, "digest")
            symbols = {node["metadata"]["qualname"]: (node["metadata"]["line"], node["metadata"]["cell"])
                       for node in result.nodes if node["kind"] in ("class", "function", "method")}
            self.assertEqual([node["id"] for node in result.nodes][:1], ["module:nb/a.ipynb"])
            return symbols, result.nodes[0]["metadata"], result

        cells = (("markdown", "intro"), ("code", ["import os\n", "def first():\n", "    return os.sep\n"]),
                 ("code", "def second():\n    return first()\n"))
        # One-line JSON has one file line; four-space and non-ASCII layouts still anchor to the real line.
        self.assertEqual(parse(self.notebook(*cells))[:2],
                         ({"first": (1, 2), "second": (1, 3)}, {"code_cells": 2, "skipped_cells": 0}))
        wide = self.notebook(("markdown", "\u00e9\u2028{[\"]}"), *cells[1:], indent=4, ensure_ascii=False)
        symbols, _, result = parse(wide)
        self.assertEqual(wide.split("\n")[symbols["first"][0] - 1].strip(), json.dumps("def first():\n") + ",")
        self.assertEqual([ref["line"] for ref in result.refs if ref["relation"] == "imports"], [symbols["first"][0] - 1])
        # Windows line endings inside a cell do not shift the lines after them.
        crlf = self.notebook(("code", ["x = 1\r\n", "\r\n", "def after():\r\n", "    pass\r\n"]), indent=1)
        self.assertEqual(crlf.split("\n")[parse(crlf)[0]["after"][0] - 1].strip(), json.dumps("def after():\r\n") + ",")

        for text, counts in (
            (self.notebook(*cells, language="R", indent=1), {"code_cells": 2, "skipped_cells": 2}),
            ("{not json", {"code_cells": 0, "skipped_cells": 0}),
            ("[]", {"code_cells": 0, "skipped_cells": 0}),
            (json.dumps({"worksheets": [{"cells": [{"cell_type": "code", "input": ["def old(): pass\n"]}]}]}),
             {"code_cells": 0, "skipped_cells": 0}),
            (json.dumps({"cells": [{"cell_type": "code", "source": 7}, {"cell_type": "code", "source": ["%%time\n", "x = 1\n"]},
                                   {"cell_type": "code", "source": ["files = !ls\n"]}, "stray"]}),
             {"code_cells": 3, "skipped_cells": 3}),
        ):
            symbols, metadata, result = parse(text)
            self.assertEqual((symbols, metadata, result.refs), ({}, counts, []), text[:40])

    def test_notebook_outputs_never_become_test_edges(self) -> None:
        out = [{"output_type": "stream", "name": "stderr", "text": ["File pkg/private.py, line 1\n"]}]
        self.write("tests/demo.ipynb", self.notebook(("code", ["x = 1\n"], out)))
        self.write("pkg/private.py", "def f():\n    pass\n")
        self.assertEqual([edge for edge in self.edges("tests") if edge["source"] == "test:tests/demo.ipynb"], [])

    def test_notebook_magics_do_not_blank_python_continuations(self) -> None:
        def refs(*lines: str):
            result = NotebookParser().parse("nb/a.ipynb", self.notebook(("code", list(lines))), "digest")
            return sorted(ref["value"] for ref in result.refs if ref["relation"] == "calls"), result.nodes[0]["metadata"]

        self.assertEqual(refs("%matplotlib inline\n", "ok = (compute(a)\n", "      != expected(b))\n"),
                         (["compute", "expected"], {"code_cells": 1, "skipped_cells": 0}))
        self.assertEqual(refs("!pip list\n", "ok = (compute(x)\n", "      % modulus(y))\n")[0], ["compute", "modulus"])
        self.assertEqual(refs("%matplotlib inline\n", "ok = (first(a)\n", " != middle(b)\n", " != last(c))\n"),
                         (["first", "last", "middle"], {"code_cells": 1, "skipped_cells": 0}))
        self.assertEqual(refs("%load_ext autoreload\n", "x = (first(a)\n", " != second(b))\n", "y = (third(c)\n",
                              " % fourth(d))\n", "!ls\n"),
                         (["first", "fourth", "second", "third"], {"code_cells": 1, "skipped_cells": 0}))
        # A comment before a cell magic does not hide it.
        self.assertEqual(refs("# timed\n", "%%time\n", "def f(): pass\n")[1], {"code_cells": 1, "skipped_cells": 1})


if __name__ == "__main__":
    unittest.main()
