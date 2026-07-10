#!/usr/bin/env python3
"""Bound artifact JSONL retention without leaving resolution orphans."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Iterable


def retained_lines(lines: Iterable[bytes], keep: int) -> list[bytes]:
    source = list(lines)
    parsed = []
    by_id = {}
    resolved_targets = set()
    for index, line in enumerate(source):
        try:
            row = json.loads(line)
        except Exception:
            row = None
        parsed.append(row)
        if isinstance(row, dict) and isinstance(row.get("event_id"), str):
            by_id[row["event_id"]] = index

    for index, row in enumerate(parsed):
        if not isinstance(row, dict) or row.get("kind") != "artifact-resolution":
            continue
        target_index = by_id.get(row.get("resolves_event_id"))
        if target_index is not None and target_index < index:
            resolved_targets.add(target_index)

    selected = set()
    for index in range(len(source) - 1, -1, -1):
        if index in selected:
            continue
        if index in resolved_targets:
            continue
        row = parsed[index]
        group = [index]
        if isinstance(row, dict) and row.get("kind") == "artifact-resolution":
            target_index = by_id.get(row.get("resolves_event_id"))
            if target_index is None or target_index >= index:
                continue
            group.insert(0, target_index)
        group = [item for item in group if item not in selected]
        if len(selected) + len(group) > keep:
            continue
        selected.update(group)
        if len(selected) == keep:
            break
    return [line for index, line in enumerate(source) if index in selected]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--keep", required=True, type=int)
    args = parser.parse_args()
    if args.keep <= 0:
        parser.error("--keep must be positive")
    lines = Path(args.input).read_bytes().splitlines(keepends=True)
    Path(args.output).write_bytes(b"".join(retained_lines(lines, args.keep)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
