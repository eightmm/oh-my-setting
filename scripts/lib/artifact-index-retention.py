#!/usr/bin/env python3
"""Bound artifact JSONL retention without leaving resolution orphans."""

from __future__ import annotations

import argparse
import datetime
import json
from collections import deque
from pathlib import Path
from typing import Iterable, Optional, Sequence


DECISION_RETENTION_DAYS = 30
DECISION_FUTURE_TOLERANCE_SECONDS = 5 * 60
MAX_DECISION_PIN_ROWS = 3000


def decision_pin_indices(lines, now=None, max_rows=MAX_DECISION_PIN_ROWS):
    """Return recent panel-result positions, capped at the newest append-order rows."""
    if max_rows < 0:
        raise ValueError("max_rows must not be negative")
    current = now or datetime.datetime.now(datetime.timezone.utc)
    cutoff = current - datetime.timedelta(days=DECISION_RETENTION_DAYS)
    future_limit = current + datetime.timedelta(seconds=DECISION_FUTURE_TOLERANCE_SECONDS)
    pinned = deque(maxlen=max_rows)
    for index, line in enumerate(lines):
        try:
            row = json.loads(line)
        except Exception:
            continue
        if not isinstance(row, dict) or row.get("kind") != "panel-result":
            continue
        timestamp = row.get("ts")
        if not isinstance(timestamp, str) or not timestamp:
            continue
        try:
            # datetime.fromisoformat on Python 3.9 does not accept the common Z suffix.
            normalized = timestamp[:-1] + "+00:00" if timestamp.endswith("Z") else timestamp
            stamp = datetime.datetime.fromisoformat(normalized)
            if stamp.tzinfo is None:
                stamp = stamp.replace(tzinfo=datetime.timezone.utc)
            stamp = stamp.astimezone(datetime.timezone.utc)
        except (TypeError, ValueError, OverflowError):
            continue
        if cutoff <= stamp <= future_limit:
            pinned.append(index)
    return set(pinned)


def retained_lines(
    lines: Iterable[bytes],
    keep: int,
    max_bytes: Optional[int] = None,
    required_event_ids: Sequence[str] = (),
    pinned_event_ids: Optional[Sequence[str]] = (),
    include_decision_pins: bool = True,
) -> list[bytes]:
    """Keep the newest complete lineage groups inside both retention bounds.

    A resolution and its target are indivisible. ``required_event_ids`` is
    used by the append transaction for rows that are about to become durable;
    those rows (and a resolution's target) either survive together or the
    mutation fails before replacing the ledger.

    Recent panel decisions are preferred within the remaining byte budget;
    unresolved failures and completion pins survive the ordinary-history
    floor. An unreadable pin namespace (``None``) preserves every row; the
    durable byte ceiling still applies.
    """
    if keep <= 0:
        raise ValueError("keep must be positive")
    if max_bytes is not None and max_bytes <= 0:
        raise ValueError("max_bytes must be positive")
    source = list(lines)
    parsed = []
    id_indices = {}
    resolved_targets = set()
    settled_failures = set()
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

    def complete_outcome(row):
        return (type(row.get("schema")) is int and row["schema"] == 1 and
                all(isinstance(row.get(key), str) and row[key]
                    for key in ("event_id", "operation_id", "artifact_id", "ts", "kind")) and
                isinstance(row.get("provider"), str) and
                type(row.get("exit")) is int and row["exit"] >= 0)

    for index, row in enumerate(parsed):
        if not isinstance(row, dict) or row.get("kind") != "artifact-resolution":
            continue
        target_index = by_id.get(row.get("resolves_event_id"))
        if target_index is not None and target_index < index:
            resolved_targets.add(target_index)
            target = parsed[target_index]
            # Match the attention reader's resolution predicate. A malformed
            # or foreign resolution must never authorize eviction of a failure.
            if (complete_outcome(row) and complete_outcome(target) and
                    by_id.get(row.get("event_id")) == index and
                    row.get("parent_event_id") == row.get("resolves_event_id") and
                    row.get("resolution") == "resolved" and
                    type(row.get("exit")) is int and row["exit"] == 0 and
                    target.get("schema") == 1 and target.get("kind") != "artifact-resolution" and
                    type(target.get("exit")) is int and target["exit"] > 0 and
                    row.get("operation_id") == target.get("operation_id") and
                    row.get("artifact_id") == target.get("artifact_id")):
                settled_failures.add(target_index)

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
    required_indices = set()
    for event_id in required_event_ids:
        index = by_id.get(event_id)
        if index is None:
            raise ValueError("required artifact event is missing or ambiguous: %s" % event_id)
        required_group = group_for(index)
        if not required_group:
            raise ValueError("required artifact resolution has no earlier unique target: %s" % event_id)
        selected.update(required_group)
        required_indices.update(required_group)
    if len(selected) > keep:
        raise ValueError("required artifact lineage exceeds row retention")
    if max_bytes is not None and selected_bytes(selected) > max_bytes:
        raise ValueError("required artifact lineage exceeds byte retention")

    if pinned_event_ids is None:
        selected.update(range(len(source)))
    else:
        for event_id in pinned_event_ids:
            for index in id_indices.get(event_id, ()):
                group = group_for(index)
                if not group:
                    # Uncertain lineage cannot justify deleting evidence.
                    selected.update(range(len(source)))
                    break
                selected.update(group)
    for index, row in enumerate(parsed):
        if (isinstance(row, dict) and row.get("kind") != "artifact-resolution" and
                type(row.get("exit")) is int and row["exit"] > 0 and index not in settled_failures):
            selected.add(index)

    # Decisions are a retention preference, not a reason to reject a mutation.
    # Reserve the hard-protected rows first, then add newest decisions only when
    # they fit beside required rows and hard protections.
    if include_decision_pins:
        decision_bytes = selected_bytes(selected)
        for index in sorted(decision_pin_indices(source), reverse=True):
            if index in selected:
                continue
            row_bytes = len(source[index])
            if max_bytes is not None and decision_bytes + row_bytes > max_bytes:
                continue
            selected.add(index)
            decision_bytes += row_bytes

    # ``keep`` remains the ordinary-history floor; protected rows are reserved
    # ahead of it and do not consume the slots the previous retention contract
    # gave to recent ordinary rows.
    ordinary_selected = set(required_indices)
    for index in range(len(source) - 1, -1, -1):
        if index in selected:
            continue
        if index in resolved_targets:
            continue
        group = group_for(index)
        if not group:
            continue
        group = [item for item in group if item not in selected]
        if len(ordinary_selected) + len(group) > keep:
            continue
        if (max_bytes is not None and
                selected_bytes(selected) + selected_bytes(group) > max_bytes):
            continue
        selected.update(group)
        ordinary_selected.update(group)
        if len(ordinary_selected) == keep or (
                max_bytes is not None and selected_bytes(selected) == max_bytes):
            break
    if max_bytes is not None and selected_bytes(selected) > max_bytes:
        raise ValueError("protected artifact rows exceed byte retention")
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
