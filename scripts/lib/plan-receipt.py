#!/usr/bin/env python3
"""One projection of an agent-plan task's landing receipt digest.

`patch-land` freezes the digest of the reviewed task and hands it back to
`agent-plan finish` as the compare-and-set that fences review -> landing ->
done. The two sides read the task from different places: landing hashes the
`agent-plan show` view, finish hashes the stored record. Any field one side
sees and the other does not is a permanent digest mismatch, which fails after
the patch is already applied and cannot be repaired by recovery, so both sides
compute the digest here and nowhere else.

Excluded fields are exactly those that are not part of the reviewed task
record: `state` and `updated` move on every transition the fence exists to
allow, and `claim_expired`/`claim_age_s`/`project_contract` are computed by
`show` for readers and never stored on the task. `project_contract` is
plan-level in particular: binding a plan to its PROJECT.md is the autopilot
spec CAS's job, not the task fence's. Stored fields the receipt must keep —
`patch` and `artifact` above all — stay in the projection, so evidence
swapped under a frozen receipt is still rejected.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any


VOLATILE = ("state", "updated", "claim_expired", "claim_age_s", "project_contract")


def projection(task: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in task.items() if key not in VOLATILE}


def digest(task: dict[str, Any]) -> str:
    raw = json.dumps(projection(task), sort_keys=True, separators=(",", ":"),
                     ensure_ascii=False)
    return hashlib.sha256(raw.encode()).hexdigest()
