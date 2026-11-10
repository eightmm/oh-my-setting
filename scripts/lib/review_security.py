#!/usr/bin/env python3
"""Mechanical parts of the gate's optional security audit seat.

`paths` lists the risk paths of the reviewed change with the same selector
patch-admit uses for its `security: ADVISE` line, so the seat is called for
exactly what admission would advise on (exit 3: none). `verdict` reads the
seat's answer and prints pass, fail or incomplete: only the last
`SECURITY: pass|fail` line of the answer counts, and an answer without one is
incomplete, never a pass.
"""

import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

VERDICT = re.compile(r"^[*\s]*SECURITY:\s*(pass|fail)[*\s]*$")
MAX_READ = 64 * 1024 * 1024


def read_bytes(path):
    try:
        if os.path.getsize(path) > MAX_READ:
            return None
        with open(path, "rb") as handle:
            return handle.read()
    except OSError:
        return None


def paths(args):
    from panel_review import _AUTO_REVIEW_FRONTIER_PATHS, _security_audit_paths
    changed = []
    try:
        with open(args.scope, encoding="utf-8") as handle:
            changed = [path for path in json.load(handle).get("paths") or [] if isinstance(path, str)]
    except (OSError, ValueError, AttributeError):
        changed = []
    patch = read_bytes(args.diff) if args.diff else None
    found = _security_audit_paths(changed, patch, _AUTO_REVIEW_FRONTIER_PATHS)
    if not found:
        return 3
    for path in found[:50]:
        print(json.dumps(path, ensure_ascii=True))
    return 0


def verdict(args):
    raw = read_bytes(args.answer)
    text = raw.decode("utf-8", "replace") if raw is not None else ""
    # A seat artifact wraps the answer in an Output section; a routed single call holds the answer alone.
    lines = text.splitlines()
    output, inside = [], False
    for line in lines:
        if line == "## Output":
            inside = True
        elif line == "## Exit":
            inside = False
        elif inside:
            output.append(line)
    if "## Output" not in lines:
        output = lines
    found = [match.group(1) for match in (VERDICT.match(line) for line in output) if match]
    print(found[-1] if found else "incomplete")
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="review_security.py")
    sub = parser.add_subparsers(dest="action", required=True)
    one = sub.add_parser("paths")
    one.add_argument("--scope", required=True)
    one.add_argument("--diff", default="")
    one = sub.add_parser("verdict")
    one.add_argument("--answer", required=True)
    args = parser.parse_args(argv)
    return paths(args) if args.action == "paths" else verdict(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
