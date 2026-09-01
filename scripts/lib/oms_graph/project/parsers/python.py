"""Python parser (W2)."""

from __future__ import annotations

import ast

from ..model import make_edge, make_node, node_id
from .base import ParseResult, Parser


class PythonParser(Parser):
    language = "python"
    extensions = (".py",)
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        file_id = node_id("file", path)
        module_id = node_id("module", path)
        result.nodes.append(make_node("module", path.rsplit("/", 1)[-1], path, self.language, source_digest))
        result.edges.append(make_edge(file_id, module_id, "contains", "EXTRACTED", path=path, source_digest=source_digest))
        try:
            tree = ast.parse(text, filename=path)
        except (SyntaxError, ValueError):
            return result

        class Visitor(ast.NodeVisitor):
            def __init__(self) -> None:
                self.class_name = ""
                self.symbol = ""

            def _source(self) -> str:
                return self.symbol or file_id

            def _add_symbol(self, kind: str, name: str, line: int) -> str:
                qualname = (self.class_name + "." + name) if self.class_name and kind == "method" else name
                ident = node_id(kind, path, qualname)
                result.nodes.append(make_node(kind, name, path, "python", source_digest, qualname=qualname, line=line))
                parent = node_id("class", path, self.class_name) if kind == "method" else file_id
                result.edges.append(make_edge(parent, ident, "contains", "EXTRACTED", path=path, source_digest=source_digest, line=line))
                return ident

            def visit_ClassDef(self, node: ast.ClassDef) -> None:
                old_class, old_symbol = self.class_name, self.symbol
                self.class_name = node.name
                self.symbol = self._add_symbol("class", node.name, node.lineno)
                for child in node.body:
                    self.visit(child)
                self.class_name, self.symbol = old_class, old_symbol

            def _visit_function(self, node: ast.AST) -> None:
                name = getattr(node, "name")
                kind = "method" if self.class_name else "function"
                old = self.symbol
                self.symbol = self._add_symbol(kind, name, getattr(node, "lineno", 1))
                for child in getattr(node, "body", []):
                    self.visit(child)
                self.symbol = old

            visit_FunctionDef = _visit_function
            visit_AsyncFunctionDef = _visit_function

            def visit_Import(self, node: ast.Import) -> None:
                for alias in node.names:
                    result.refs.append({"from": self._source(), "relation": "imports", "kind": "module", "value": alias.name, "line": node.lineno, "binding": alias.asname or alias.name.split(".")[0]})

            def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
                module = ("." * node.level) + (node.module or "")
                for alias in node.names:
                    result.refs.append({"from": self._source(), "relation": "imports", "kind": "module", "value": module, "line": node.lineno, "binding": alias.asname or alias.name, "imported": alias.name})

            def visit_Call(self, node: ast.Call) -> None:
                value = ""
                if isinstance(node.func, ast.Name):
                    value = node.func.id
                elif isinstance(node.func, ast.Attribute):
                    parts = []
                    cursor = node.func
                    while isinstance(cursor, ast.Attribute):
                        parts.append(cursor.attr)
                        cursor = cursor.value
                    if isinstance(cursor, ast.Name):
                        parts.append(cursor.id)
                        value = ".".join(reversed(parts))
                if value:
                    result.refs.append({"from": self._source(), "relation": "calls", "kind": "name", "value": value, "line": node.lineno})
                self.generic_visit(node)

        Visitor().visit(tree)
        return result
