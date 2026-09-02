#!/usr/bin/env python3
"""Inventory one checkout's .oms state, minus what the live session owns.

check.sh takes this fingerprint before and after the gate: no suite may write
into the checkout's own .oms, and a test that forgets --repo defaults to the
working directory. The inventory covers contents, child-entry modes, symlinks,
and empty directories.

Ambient entries are excluded because the live agent session writes them on its
own schedule, with no relation to the gate: hooks/ and work-journal/ on every
turn, ci.jsonl whenever the CI watcher records a run for a push, project-graph/
whenever a session starts in this checkout without a current graph and the
session-start hook refreshes it in the background, and the autopilot
shadow-judgment ledger whenever any session starts here — a green 25-minute
gate is a wide window for those to land and read as a suite defect. Everything
else stays covered, including failures.jsonl, where a forgotten --repo is
exactly the bug worth catching.

Usage: oms-state-inventory.py OMS_DIR
"""

from __future__ import annotations

import hashlib
import json
import os
import stat
import sys

AMBIENT_DIRS = {"hooks", "work-journal", "project-graph"}
# .gitignore is the constant ownership marker ("*") every harness tool drops
# when it first touches a repository's .oms; the ambient CI watcher creates it
# together with ci.jsonl, so excluding one without the other still failed a
# green gate. A suite that wrote only this marker left no state behind, and
# any real leak brings its own entries.
AMBIENT_FILES = {"ci.jsonl", ".gitignore"}
# Path-precise ambient entries below the root: the session-start hook appends
# a shadow-judgment row (autopilot) and a graph-route shadow row whenever a
# session opens this checkout mid-gate. Only the ledgers themselves are
# ambient — every other plan/ or graph/ entry (receipts, tasks, claims, runs,
# events) stays covered.
AMBIENT_PATHS = {"plan/autopilot-shadow.jsonl", "graph/shadow.jsonl"}


def _holds_only_ambient(rel: str, listed: set[str]) -> bool:
    """A directory that exists only to hold ambient entries is itself ambient.

    The session-start hook creates `graph/` for `graph/shadow.jsonl` in a
    repository that never ran the execution graph; that directory must compare
    equal to its absence, while `graph/runs/` (real run state) still lists."""
    prefix = rel + "/"
    if not any(path.startswith(prefix) for path in AMBIENT_PATHS):
        return False
    return not any(other.startswith(prefix) for other in listed)


def inventory(root: str) -> list[tuple]:
    entries: list[tuple] = []
    for base, dirs, files in os.walk(root, topdown=True, followlinks=False):
        at_root = base == root
        if at_root:
            dirs[:] = [name for name in dirs if name not in AMBIENT_DIRS]

        traversable = []
        for name in sorted(dirs):
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            try:
                info = os.lstat(path)
                mode = stat.S_IMODE(info.st_mode)
                if stat.S_ISLNK(info.st_mode):
                    entries.append((rel, "link", mode, os.readlink(path)))
                else:
                    entries.append((rel, "dir", mode, ""))
                    traversable.append(name)
            except OSError as exc:
                entries.append((rel, "error", 0, exc.__class__.__name__))
        dirs[:] = traversable

        for name in sorted(files):
            if at_root and name in AMBIENT_FILES:
                continue
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root).replace(os.sep, "/")
            if rel in AMBIENT_PATHS:
                continue
            try:
                info = os.lstat(path)
                mode = stat.S_IMODE(info.st_mode)
                if stat.S_ISLNK(info.st_mode):
                    entries.append((rel, "link", mode, os.readlink(path)))
                elif stat.S_ISREG(info.st_mode):
                    digest = hashlib.sha256()
                    with open(path, "rb") as handle:
                        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                            digest.update(chunk)
                    entries.append((rel, "file", mode, digest.hexdigest()))
                else:
                    entries.append((rel, "other", mode, ""))
            except OSError as exc:
                entries.append((rel, "error", 0, exc.__class__.__name__))
    listed = {entry[0] for entry in entries}
    return [entry for entry in entries if not (entry[1] == "dir" and _holds_only_ambient(entry[0], listed))]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write("usage: oms-state-inventory.py OMS_DIR\n")
        return 2
    root = argv[1]
    if not os.path.isdir(root):
        return 0
    for entry in sorted(inventory(os.path.realpath(root))):
        print(json.dumps(entry, ensure_ascii=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
