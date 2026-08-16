#!/usr/bin/env bash
set -euo pipefail

# Syntax gate for every Python helper, including typed runtime packages below
# scripts/lib. Compilation stays in memory so a read-only gate never writes
# __pycache__ into the tree it is certifying.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h|--help) echo "usage: check-python.sh   (syntax-check the Python helpers recursively)"; exit 0 ;;
    *) echo "error: check-python.sh takes no arguments: $1" >&2; exit 2 ;;
  esac
fi

PYTHONDONTWRITEBYTECODE=1 python3 - scripts scripts/lib templates tests <<'PY'
import ast
import pathlib
import sys

failed = 0
checked = 0
seen = set()
for directory in sys.argv[1:]:
    root = pathlib.Path(directory)
    if not root.exists():
        continue
    for path in sorted(root.rglob("*.py")):
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
