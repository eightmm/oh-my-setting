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
import pathlib
import sys

failed = 0
checked = 0
seen = set()
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
            ast.parse(source, filename=str(path), mode="exec", feature_version=9)
            compile(source, str(path), "exec")
        except (OSError, SyntaxError, UnicodeError, ValueError) as exc:
            failed = 1
            print("error: %s: %s" % (path, exc), file=sys.stderr)

if not checked:
    print("error: no Python helpers found to check", file=sys.stderr)
    raise SystemExit(1)
if failed:
    raise SystemExit(1)
print("python-syntax: ok (%d files)" % checked)
PY
