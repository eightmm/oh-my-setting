"""Python parser (W2)."""

from __future__ import annotations

import ast
import builtins

from ..model import compact_summary, make_edge, make_node, node_id
from .base import ParseResult, Parser

_BUILTIN_NAMES = frozenset(dir(builtins))


def _dotted_name(node: ast.AST) -> str:
    """`a.b.c` for a Name/Attribute chain, '' for anything else (calls, subscripts)."""
    parts = []
    cursor = node
    while isinstance(cursor, ast.Attribute):
        parts.append(cursor.attr)
        cursor = cursor.value
    if isinstance(cursor, ast.Name):
        parts.append(cursor.id)
        return ".".join(reversed(parts))
    return ""


def _target_names(target: ast.AST, into: set) -> None:
    if isinstance(target, ast.Name):
        into.add(target.id)
    elif isinstance(target, (ast.Tuple, ast.List)):
        for element in target.elts:
            _target_names(element, into)
    elif isinstance(target, ast.Starred):
        _target_names(target.value, into)


def _bound_names(node: ast.AST) -> set:
    """Every name a statement subtree binds as a variable (imports excluded).

    Over-approximation is deliberate: a nested scope's binding also counts for
    the enclosing function, so a call through such a name yields no edge rather
    than a repo-wide guess by name."""
    names: set = set()
    for child in ast.walk(node):
        if isinstance(child, ast.Assign):
            for target in child.targets:
                _target_names(target, names)
        elif isinstance(child, (ast.AnnAssign, ast.AugAssign, ast.For, ast.AsyncFor, ast.comprehension, ast.NamedExpr)):
            _target_names(child.target, names)
        elif isinstance(child, (ast.With, ast.AsyncWith)):
            for item in child.items:
                if item.optional_vars is not None:
                    _target_names(item.optional_vars, names)
        elif isinstance(child, ast.ExceptHandler) and child.name:
            names.add(child.name)
        elif isinstance(child, ast.arguments):
            for arg in child.posonlyargs + child.args + child.kwonlyargs:
                names.add(arg.arg)
            for arg in (child.vararg, child.kwarg):
                if arg is not None:
                    names.add(arg.arg)
    return names


def _python_signature(node: ast.AST, qualname: str, kind: str) -> str:
    if isinstance(node, ast.ClassDef):
        bases = [name for name in (_dotted_name(item) for item in node.bases) if name]
        return "class %s%s" % (qualname, "(%s)" % ", ".join(bases) if bases else "")
    args = getattr(node, "args", None)
    if not isinstance(args, ast.arguments):
        return qualname
    positional = list(getattr(args, "posonlyargs", [])) + list(args.args)
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)
    parts = [item.arg + ("=..." if default is not None else "")
             for item, default in zip(positional, defaults)]
    posonly = len(getattr(args, "posonlyargs", []))
    if posonly:
        parts.insert(posonly, "/")
    if args.vararg is not None:
        parts.append("*" + args.vararg.arg)
    elif args.kwonlyargs:
        parts.append("*")
    for item, default in zip(args.kwonlyargs, args.kw_defaults):
        parts.append(item.arg + ("=..." if default is not None else ""))
    if args.kwarg is not None:
        parts.append("**" + args.kwarg.arg)
    prefix = "async def" if isinstance(node, ast.AsyncFunctionDef) else "def"
    return "%s %s(%s)" % (prefix, qualname, ", ".join(parts))


class PythonParser(Parser):
    language = "python"
    extensions = (".py",)
    version = 3

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
        result.nodes[0]["summary"] = compact_summary(ast.get_docstring(tree, clean=True))

        import_bindings: set = set()
        for child in ast.walk(tree):
            if isinstance(child, ast.Import):
                import_bindings.update(alias.asname or alias.name.split(".")[0] for alias in child.names)
            elif isinstance(child, ast.ImportFrom):
                import_bindings.update(alias.asname or alias.name for alias in child.names)
        # Module-level variables (not definitions, not imports): a call through
        # one of them is an object of unknown type, never a repo symbol by name.
        module_variables: set = set()
        for statement in tree.body:
            if not isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                module_variables.update(_bound_names(statement))
        module_variables -= import_bindings

        class Visitor(ast.NodeVisitor):
            def __init__(self) -> None:
                self.class_name = ""
                self.symbol = ""
                # Method names of the enclosing class bodies, so a `self.x()`
                # call resolves to a sibling method as a fact rather than by name.
                self.methods_stack = []
                # Names bound by the enclosing function bodies (arguments and
                # assignment targets); a call through one is a variable.
                self.locals_stack = []

            def _source(self) -> str:
                return self.symbol or file_id

            def _is_variable(self, name: str) -> bool:
                if name in import_bindings:
                    return False
                return name in module_variables or any(name in frame for frame in self.locals_stack)

            def _add_symbol(self, kind: str, name: str, node: ast.AST) -> str:
                qualname = (self.class_name + "." + name) if self.class_name and kind == "method" else name
                ident = node_id(kind, path, qualname)
                metadata = {
                    "signature": _python_signature(node, qualname, kind),
                    "end_line": int(getattr(node, "end_lineno", getattr(node, "lineno", 1))),
                }
                result.nodes.append(make_node(
                    kind, name, path, "python", source_digest, qualname=qualname,
                    line=int(getattr(node, "lineno", 1)),
                    summary=ast.get_docstring(node, clean=True), metadata=metadata,
                ))
                parent = node_id("class", path, self.class_name) if kind == "method" else file_id
                result.edges.append(make_edge(parent, ident, "contains", "EXTRACTED", path=path, source_digest=source_digest, line=int(getattr(node, "lineno", 1))))
                return ident

            def visit_ClassDef(self, node: ast.ClassDef) -> None:
                old_class, old_symbol = self.class_name, self.symbol
                self.class_name = node.name
                self.symbol = self._add_symbol("class", node.name, node)
                self.methods_stack.append({child.name for child in node.body if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))})
                for base in node.bases:
                    base_name = _dotted_name(base)
                    if base_name and not self._is_variable(base_name.partition(".")[0]):
                        result.refs.append({"from": self.symbol, "relation": "depends_on", "kind": "name", "value": base_name, "line": node.lineno})
                for child in node.body:
                    self.visit(child)
                self.methods_stack.pop()
                self.class_name, self.symbol = old_class, old_symbol

            def _visit_function(self, node: ast.AST) -> None:
                name = getattr(node, "name")
                kind = "method" if self.class_name else "function"
                old = self.symbol
                self.symbol = self._add_symbol(kind, name, node)
                self.locals_stack.append(_bound_names(node))
                for child in getattr(node, "body", []):
                    self.visit(child)
                self.locals_stack.pop()
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
                value = _dotted_name(node.func)
                receiver, _, member = value.partition(".")
                if receiver in ("self", "cls") and self.class_name and member and "." not in member:
                    if self.methods_stack and member in self.methods_stack[-1]:
                        result.edges.append(make_edge(self._source(), node_id("method", path, self.class_name + "." + member), "calls", "EXTRACTED", path=path, source_digest=source_digest, line=node.lineno))
                    else:
                        # Inherited or mixed-in: resolve by name across the repo,
                        # where a method is the expected kind of target.
                        result.refs.append({"from": self._source(), "relation": "calls", "kind": "name", "value": member, "line": node.lineno, "method_call": True})
                elif value and not self._is_variable(receiver):
                    # `scoped`: resolve only through this file's import bindings
                    # or its own definitions — an attribute call on a name that
                    # is not an import, or a builtin's name, is never matched to
                    # a repo symbol just because the names agree.
                    scoped = bool(member and receiver not in import_bindings) or receiver in _BUILTIN_NAMES
                    ref = {"from": self._source(), "relation": "calls", "kind": "name", "value": value, "line": node.lineno}
                    if scoped:
                        ref["scoped"] = True
                    result.refs.append(ref)
                self.generic_visit(node)

        Visitor().visit(tree)
        return result
