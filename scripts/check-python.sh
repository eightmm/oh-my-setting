#!/usr/bin/env bash
set -euo pipefail

# Syntax gate for every Python helper, including typed runtime packages below
# scripts/lib. Compilation stays in memory so a read-only gate never writes
# __pycache__ into the tree it is certifying.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

case "${1:-}" in -h|--help) echo "usage: check-python.sh [PATH...] (default: all Python helpers)"; exit 0 ;; esac
[ "$#" -gt 0 ] || set -- scripts templates tests

PYTHONDONTWRITEBYTECODE=1 python3 - "$@" <<'PY'
import ast
import builtins
import pathlib
import symtable
import sys


class ModuleBindings(ast.NodeVisitor):
    """Collect names that are visible at module scope without entering functions."""
    def __init__(self):
        self.names = set()
        self.dynamic = False

    def bind_target(self, target):
        if isinstance(target, ast.Name):
            self.names.add(target.id)
        elif isinstance(target, (ast.Tuple, ast.List)):
            for item in target.elts:
                self.bind_target(item)
        elif isinstance(target, ast.Starred):
            self.bind_target(target.value)

    def visit_FunctionDef(self, node):
        self.names.add(node.name)
        for decorator in node.decorator_list:
            self.visit(decorator)
        for default in node.args.defaults + node.args.kw_defaults:
            if default is not None:
                self.visit(default)

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node):
        self.names.add(node.name)
        for expr in node.decorator_list + node.bases + [kw.value for kw in node.keywords]:
            self.visit(expr)

    def visit_Import(self, node):
        self.names.update(alias.asname or alias.name.split('.')[0] for alias in node.names)

    def visit_ImportFrom(self, node):
        if any(alias.name == '*' for alias in node.names):
            self.dynamic = True
        self.names.update(alias.asname or alias.name for alias in node.names if alias.name != '*')

    def visit_Name(self, node):
        if isinstance(node.ctx, (ast.Store, ast.Param)):
            self.names.add(node.id)

    def visit_Global(self, node):
        # Function-level global writes are added from the symbol table below.
        pass

    def visit_Call(self, node):
        if (isinstance(node.func, ast.Attribute) and node.func.attr in
                ('setdefault', 'update', '__setitem__') and
                isinstance(node.func.value, ast.Call) and
                isinstance(node.func.value.func, ast.Name) and
                node.func.value.func.id in ('globals', 'vars') and not node.func.value.args and
                not node.func.value.keywords):
            self.dynamic = True
        if (isinstance(node.func, ast.Name) and node.func.id == 'setattr' and
                node.args and isinstance(node.args[0], ast.Subscript) and
                isinstance(node.args[0].value, ast.Attribute) and
                isinstance(node.args[0].value.value, ast.Name) and
                node.args[0].value.value.id == 'sys' and node.args[0].value.attr == 'modules'):
            self.dynamic = True
        self.generic_visit(node)

    def visit_Subscript(self, node):
        namespace = node.value
        if (isinstance(node.ctx, ast.Store) and isinstance(namespace, ast.Call) and
                isinstance(namespace.func, ast.Name) and namespace.func.id in ('globals', 'vars') and
                not namespace.args and not namespace.keywords):
            self.dynamic = True
        self.generic_visit(node)

    def visit_NamedExpr(self, node):
        self.bind_target(node.target)
        self.visit(node.value)

    def visit_comprehension(self, node):
        self.visit(node.iter)
        for condition in node.ifs:
            self.visit(condition)

    def visit_ExceptHandler(self, node):
        if node.name:
            self.names.add(node.name)
        for stmt in node.body:
            self.visit(stmt)


def undefined_globals(tree, source, filename):
    bindings = ModuleBindings()
    bindings.visit(tree)
    if '__getattr__' in bindings.names:
        bindings.dynamic = True
    table = symtable.symtable(source, filename, 'exec')

    scope_nodes = {}
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            key = ('function', node.name, node.lineno)
        elif isinstance(node, ast.ClassDef):
            key = ('class', node.name, node.lineno)
        elif isinstance(node, ast.Lambda):
            key = ('function', 'lambda', node.lineno)
        else:
            continue
        scope_nodes[key] = node

    def scopes(scope):
        yield scope
        for child in scope.get_children():
            yield from scopes(child)

    # A global assignment anywhere in the module makes that name a module
    # binding, regardless of whether its writer appears before its reader.
    for scope in scopes(table):
        for symbol in scope.get_symbols():
            if symbol.is_global() and symbol.is_assigned():
                bindings.names.add(symbol.get_name())

    class ScopeLoads(ast.NodeVisitor):
        def __init__(self, root, name):
            self.root = root
            self.name = name
            self.lines = []

        def visit_Name(self, node):
            if node.id == self.name and isinstance(node.ctx, ast.Load):
                self.lines.append(node.lineno)

        def visit_FunctionDef(self, node):
            if node is self.root:
                for stmt in node.body:
                    self.visit(stmt)
                return
            for expr in node.decorator_list + node.args.defaults + node.args.kw_defaults:
                if expr is not None:
                    self.visit(expr)

        visit_AsyncFunctionDef = visit_FunctionDef

        def visit_ClassDef(self, node):
            if node is self.root:
                for stmt in node.body:
                    self.visit(stmt)
                return
            for expr in node.decorator_list + node.bases + [kw.value for kw in node.keywords]:
                self.visit(expr)

        def visit_Lambda(self, node):
            if node is self.root:
                self.visit(node.body)

    def read_line(scope, name):
        node = scope_nodes.get((scope.get_type(), scope.get_name(), scope.get_lineno()))
        if node is not None:
            visitor = ScopeLoads(node, name)
            visitor.visit(node)
            if visitor.lines:
                return min(visitor.lines)
        return scope.get_lineno()

    def inspect(scope):
        if scope is not table:
            for symbol in scope.get_symbols():
                if (symbol.is_global() and symbol.is_referenced() and
                        not symbol.is_declared_global() and symbol.get_name() not in bindings.names):
                    yield symbol.get_name(), read_line(scope, symbol.get_name())
        for child in scope.get_children():
            yield from inspect(child)

    problems = list(inspect(table))
    if bindings.dynamic:
        return [], 'dynamic namespace'
    # The operator smoke fixtures are source fragments deliberately exec'd by
    # tests/operator/runner.py into one shared namespace, so their globals are
    # supplied by sibling fragments and the runner rather than module bindings.
    if pathlib.Path(filename).resolve().parent == pathlib.Path('tests/operator').resolve():
        return [], 'operator-fixture directory'
    allowed = set(dir(builtins)) | {'__file__', '__name__', '__package__', '__loader__',
                                    '__spec__', '__builtins__', '__doc__', '__cached__'}
    return sorted(set((name, line) for name, line in problems if name not in allowed and
                      not (name.startswith('__') and name.endswith('__')))), None

failed = 0
checked = 0
seen = set()
skipped = []
for directory in sys.argv[1:]:
    root = pathlib.Path(directory)
    if not root.exists():
        print("error: missing syntax-check path: %s" % root, file=sys.stderr)
        failed = 1
        continue
    for path in ([root] if root.is_file() else sorted(root.rglob("*.py"))):
        if "__pycache__" in path.parts:
            continue
        resolved = path.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        checked += 1
        try:
            source = path.read_text(encoding="utf-8")
            tree = ast.parse(source, filename=str(path), mode="exec", feature_version=9)
            compile(source, str(path), "exec")
            problems, skip_reason = undefined_globals(tree, source, str(path))
            for name, line in problems:
                failed = 1
                print("error: %s:%d: undefined name %s" % (path, line, name), file=sys.stderr)
            if skip_reason:
                skipped.append((str(path), skip_reason))
        except (OSError, SyntaxError, UnicodeError, ValueError) as exc:
            failed = 1
            print("error: %s: %s" % (path, exc), file=sys.stderr)

if not checked:
    print("error: no Python helpers found to check", file=sys.stderr)
    raise SystemExit(1)
if failed:
    raise SystemExit(1)
summary = "python-syntax: ok (%d files)" % checked
if skipped:
    dynamic_names = [pathlib.Path(path).name for path, reason in skipped
                     if reason == 'dynamic namespace']
    operator_count = sum(1 for _, reason in skipped
                         if reason == 'operator-fixture directory')
    names = []
    dynamic_capacity = 5 - (1 if operator_count else 0)
    if len(dynamic_names) > dynamic_capacity:
        dynamic_capacity -= 1
        names.extend(dynamic_names[:dynamic_capacity])
        names.append('…')
    else:
        names.extend(dynamic_names)
    if operator_count:
        names.append('tests/operator/* (%d)' % operator_count)
    summary = "python-syntax: ok (%d files; undefined-name rule skipped %d: %s)" % (
        checked, len(skipped), ', '.join(names))
print(summary)
PY
