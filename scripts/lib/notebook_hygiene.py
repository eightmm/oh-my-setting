#!/usr/bin/env python3
"""Notebook hygiene report for project-doctor (stdlib JSON only).

Usage: notebook_hygiene.py NOTEBOOK...

Prints one `warn<TAB>message` or `ok<TAB>message` line per finding. A notebook is
read as plain JSON; cell outputs are measured, never printed. Nothing is
rewritten or stripped. Unreadable or oversized files are reported, not guessed at.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Dict, List

MAX_BYTES = 32 * 1024 * 1024
OUTPUT_WARN_BYTES = 1024


def inspect(path: str) -> Dict[str, Any]:
    size = os.path.getsize(path)
    if size > MAX_BYTES:
        return {"skipped": "larger than %d MiB, not parsed" % (MAX_BYTES // 1024 // 1024)}
    try:
        with open(path, encoding="utf-8") as handle:
            doc = json.load(handle)
    except (OSError, ValueError) as exc:
        return {"skipped": "not readable as JSON (%s)" % type(exc).__name__}
    cells = doc.get("cells") if isinstance(doc, dict) else None
    if not isinstance(cells, list):
        return {"skipped": "no top-level cells array (nbformat 3 or not a notebook)"}
    with_outputs = 0
    output_bytes = 0
    counts: List[int] = []
    inversions: List[int] = []  # 1-based code-cell numbers whose count does not exceed the previous one
    code_index = 0
    for cell in cells:
        if not isinstance(cell, dict) or cell.get("cell_type") != "code":
            continue
        code_index += 1
        outputs = cell.get("outputs")
        if isinstance(outputs, list) and outputs:
            with_outputs += 1
            output_bytes += len(json.dumps(outputs, ensure_ascii=False).encode("utf-8"))
        count = cell.get("execution_count")
        if isinstance(count, int) and not isinstance(count, bool):
            if counts and count <= counts[-1]:
                inversions.append(code_index)
            counts.append(count)
    return {
        "with_outputs": with_outputs,
        "output_bytes": output_bytes,
        "inversions": inversions,
    }


def main(argv: List[str]) -> int:
    flagged = 0
    for path in argv:
        info = inspect(path)
        if "skipped" in info:
            print("warn\tnotebook %s: %s" % (path, info["skipped"]))
            flagged += 1
            continue
        if info["with_outputs"] and info["output_bytes"] >= OUTPUT_WARN_BYTES:
            print(
                "warn\ttracked notebook %s stores outputs in %d code cell(s) (%.1f KiB); "
                "clear outputs before committing, and take claim numbers from a ledgered script run"
                % (path, info["with_outputs"], info["output_bytes"] / 1024.0)
            )
            flagged += 1
        if info["inversions"]:
            cells = ", ".join(str(n) for n in info["inversions"][:5])
            print(
                "warn\ttracked notebook %s ran out of order (execution_count does not increase at code cell %s); "
                "restart and run all before relying on its numbers" % (path, cells)
            )
            flagged += 1
    if not flagged:
        print("ok\ttracked notebooks carry no outputs and ran in order (%d checked)" % len(argv))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
