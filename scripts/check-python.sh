#!/usr/bin/env bash
set -euo pipefail

# Syntax gate for the harness's Python helpers. Every .sh here is linted, while
# the .py files had nothing, and one of them is now on the install path
# (managed-target.py decides what an installer may delete). python3 is already
# a hard requirement of this repo, so this adds no new tool.
#
# Compiled in memory on purpose: writing __pycache__ into the checkout would
# make a read-only gate mutate the tree it is certifying.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h|--help) echo "usage: check-python.sh   (syntax-check the Python helpers)"; exit 0 ;;
    *) echo "error: check-python.sh takes no arguments: $1" >&2; exit 2 ;;
  esac
fi

python3 - scripts scripts/lib templates tests <<'PY'
import ast
import pathlib
import sys

failed = 0
checked = 0
for directory in sys.argv[1:]:
    for path in sorted(pathlib.Path(directory).glob("*.py")):
        checked += 1
        try:
            source = path.read_text(encoding="utf-8")
            # CI also runs this under Python 3.9; on newer interpreters pin the
            # grammar floor so a local lint pass cannot accept newer syntax.
            ast.parse(source, filename=str(path), mode="exec", feature_version=9)
            compile(source, str(path), "exec")
        except (SyntaxError, ValueError) as exc:
            failed = 1
            print("error: %s: %s" % (path, exc), file=sys.stderr)

if not checked:
    print("error: no Python helpers found to check", file=sys.stderr)
    raise SystemExit(1)
if failed:
    raise SystemExit(1)
print("python-syntax: ok (%d files)" % checked)
PY
