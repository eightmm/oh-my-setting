#!/usr/bin/env python3
"""Print the newest recorded task decisions for a session-handoff digest.

Reads only the active packet's `## Decisions` bullets (written by
`agent-task update --decision [--decision-reason] [--rejected]`). No model
call, no new store. Newest entries are kept first; older ones are dropped when
the entry limit or the byte cap is hit, and the dropped count is printed.

usage: handoff_decisions.py REPO [MAX_ENTRIES] [MAX_BYTES]
"""
import pathlib
import re
import sys

PACKET_LIMIT = 64 * 1024
BULLET = re.compile(r"^- (\d{4}-\d{2}-\d{2})T\S*Z \[[^\]]*\] (.+)$")


def decisions(text):
    if re.search(r"^- status: closed\s*$", text, re.M):
        return []
    match = re.search(r"^## Decisions\n(.*?)(?=^## |\Z)", text, re.M | re.S)
    rows = []
    for line in (match.group(1) if match else "").splitlines():
        found = BULLET.match(line.strip())
        if found:
            rows.append("- %s %s" % (found.group(1), found.group(2).strip()))
    return rows


def main(argv):
    repo = pathlib.Path(argv[1])
    limit = int(argv[2]) if len(argv) > 2 else 5
    cap = max(256, int(argv[3])) if len(argv) > 3 else 1024
    task = repo / ".oms/task/current.md"
    try:
        if any(p.is_symlink() for p in (task, *task.parents)):
            return 0
        with task.open("rb") as handle:
            raw = handle.read(PACKET_LIMIT + 1)
        if len(raw) > PACKET_LIMIT:
            return 0
        rows = decisions(raw.decode("utf-8"))
    except (OSError, ValueError):
        return 0
    if not rows:
        return 0
    kept, used = [], 0
    for row in reversed(rows):
        size = len(row.encode("utf-8")) + 1
        if len(kept) >= limit or used + size > cap:
            break
        kept.append(row)
        used += size
    dropped = len(rows) - len(kept)
    print("## Decisions (recorded; decision | why | rejected)\n")
    if dropped:
        print("(%d older decision(s) dropped; see .oms/task/current.md)" % dropped)
    for row in reversed(kept):
        print(row)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
