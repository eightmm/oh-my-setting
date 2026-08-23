#!/usr/bin/env python3
"""Bound artifact JSONL retention without leaving resolution orphans."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Iterable, Optional, Sequence


def retained_lines(
    lines: Iterable[bytes],
    keep: int,
    max_bytes: Optional[int] = None,
    required_event_ids: Sequence[str] = (),
) -> list[bytes]:
    """Keep the newest complete lineage groups inside both retention bounds.

    A resolution and its target are indivisible. ``required_event_ids`` is
    used by the append transaction for rows that are about to become durable;
    those rows (and a resolution's target) either survive together or the
    mutation fails before replacing the ledger.
    """
    if keep <= 0:
        raise ValueError("keep must be positive")
    if max_bytes is not None and max_bytes <= 0:
        raise ValueError("max_bytes must be positive")
    source = list(lines)
    parsed = []
    id_indices = {}
    resolved_targets = set()
    for index, line in enumerate(source):
        try:
            row = json.loads(line)
        except Exception:
            row = None
        parsed.append(row)
        if isinstance(row, dict) and isinstance(row.get("event_id"), str):
            id_indices.setdefault(row["event_id"], []).append(index)

    # Ambiguous ids cannot provide lineage. Retention still keeps their rows as
    # ordinary events, but never pairs a resolution with an arbitrary twin.
    by_id = {
        event_id: indices[0]
        for event_id, indices in id_indices.items()
        if len(indices) == 1
    }

    for index, row in enumerate(parsed):
        if not isinstance(row, dict) or row.get("kind") != "artifact-resolution":
            continue
        target_index = by_id.get(row.get("resolves_event_id"))
        if target_index is not None and target_index < index:
            resolved_targets.add(target_index)

    def group_for(index):
        # Follow the entire backward target closure. Valid current rows have
        # one ordinary target, but legacy/invalid ledgers can contain a
        # resolution targeting another resolution; retaining only the direct
        # pair would leave that target's own lineage dangling.
        group = []
        seen = set()
        current = index
        while True:
            if current in seen:
                return []
            seen.add(current)
            group.append(current)
            row = parsed[current]
            if not (isinstance(row, dict) and
                    row.get("kind") == "artifact-resolution"):
                break
            target_index = by_id.get(row.get("resolves_event_id"))
            if target_index is None or target_index >= current:
                return []
            current = target_index
        return group

    def selected_bytes(indices):
        return sum(len(source[index]) for index in indices)

    selected = set()
    for event_id in required_event_ids:
        index = by_id.get(event_id)
        if index is None:
            raise ValueError("required artifact event is missing or ambiguous: %s" % event_id)
        required_group = group_for(index)
        if not required_group:
            raise ValueError("required artifact resolution has no earlier unique target: %s" % event_id)
        selected.update(required_group)
    if len(selected) > keep:
        raise ValueError("required artifact lineage exceeds row retention")
    if max_bytes is not None and selected_bytes(selected) > max_bytes:
        raise ValueError("required artifact lineage exceeds byte retention")

    for index in range(len(source) - 1, -1, -1):
        if index in selected:
            continue
        if index in resolved_targets:
            continue
        group = group_for(index)
        if not group:
            continue
        group = [item for item in group if item not in selected]
        if len(selected) + len(group) > keep:
            continue
        if (max_bytes is not None and
                selected_bytes(selected) + selected_bytes(group) > max_bytes):
            continue
        selected.update(group)
        if len(selected) == keep or (
                max_bytes is not None and selected_bytes(selected) == max_bytes):
            break
    return [line for index, line in enumerate(source) if index in selected]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--keep", required=True, type=int)
    parser.add_argument("--max-bytes", type=int)
    args = parser.parse_args()
    if args.keep <= 0:
        parser.error("--keep must be positive")
    lines = Path(args.input).read_bytes().splitlines(keepends=True)
    Path(args.output).write_bytes(
        b"".join(retained_lines(lines, args.keep, args.max_bytes))
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
