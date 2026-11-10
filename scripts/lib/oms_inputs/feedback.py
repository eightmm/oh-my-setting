"""Import already-local artifact feedback as one reviewable queued instruction."""
from __future__ import annotations

from collections import Counter

from oms_runtime.common import CoreError, canonical_json, read_json, read_jsonl, sha256_bytes
from . import attachments, queue
from .common import digest, ident, locked, plain, safe_text, state, write_state

ACTION = {"select", "accept", "reject", "uncertain", "comment"}


def batch(repo, requested):
    requested = digest(requested)
    folder = plain(repo / ".oms" / "artifact-views")
    rows = read_jsonl(folder / "feedback.jsonl", limit_rows=10000)
    found = [row for row in rows if row.get("batch_id") == requested]
    if not found:
        raise CoreError("feedback batch is not in the local artifact-view import ledger")
    fields = ("schema", "batch_id", "view_id", "data_sha256", "events", "report")
    if any(any(row.get(field) != found[0].get(field) for field in fields) for row in found[1:]):
        raise CoreError("feedback batch has conflicting import records")
    row = found[0]
    if row.get("schema") != 1:
        raise CoreError("unsupported feedback schema")
    view = ident(row.get("view_id"))
    data_sha = digest(row.get("data_sha256"))
    events, report = row.get("events"), row.get("report")
    if not isinstance(events, list) or len(events) > 1000 or not isinstance(report, dict):
        raise CoreError("invalid or oversized feedback batch")
    if set(report) - {"title", "question", "decision", "next"}:
        raise CoreError("unknown feedback report field")
    for value in report.values():
        if not isinstance(value, str) or len(value) > 12000:
            raise CoreError("feedback report fields must be strings of at most 12000 characters")
    registry = read_json(plain(folder / (view + ".json")), limit=1024 * 1024)
    if (not isinstance(registry, dict) or registry.get("schema") != 1 or registry.get("id") != view
            or registry.get("data_sha256") != data_sha or not isinstance(registry.get("row_keys"), list)
            or len(registry["row_keys"]) > 100000):
        raise CoreError("feedback does not match the registered artifact view")
    if any(not isinstance(key, str) or len(key) > 500 for key in registry["row_keys"]):
        raise CoreError("invalid registered artifact row keys")
    keys = set(registry["row_keys"])
    for event in events:
        if (not isinstance(event, dict) or set(event) - {"row_key", "action", "comment"}
                or event.get("row_key") not in keys or event.get("action") not in ACTION
                or not isinstance(event.get("comment", ""), str) or len(event.get("comment", "")) > 4000):
            raise CoreError("feedback event has an invalid row identity, action, or comment")
    canonical = {"view_id": view, "data_sha256": data_sha, "events": events, "report": report}
    encoded = canonical_json(canonical)
    if len(encoded) > attachments.MAX_FEEDBACK or sha256_bytes(encoded) != requested:
        raise CoreError("feedback batch digest or byte bound does not match")
    safe_text(encoded.decode("utf-8"), attachments.MAX_FEEDBACK)
    return row, canonical, registry


def enqueue(repo, work, requested, room_id, who, to, mode="next"):
    row, canonical, registry = batch(repo, requested)
    route = queue.binding(repo, room_id, who, to)
    counts = Counter(event["action"] for event in row["events"])
    text = "Artifact feedback for %s.\nBatch: %s\n" % (row["view_id"], requested)
    text += "Selections and decisions: " + ", ".join("%s=%d" % (key, counts[key]) for key in sorted(counts)) + "."
    if row["report"].get("next", "").strip():
        text += "\n\nRequested next step:\n" + row["report"]["next"]
    text += "\n\nRead the attached batch for every exact selected row, comment, and report field."
    # Reserving room for the immutable attachment reference prevents a long
    # next-step report from being silently truncated on the way to a worker.
    if len(text.encode()) > 2900:
        raise CoreError("feedback next step exceeds the room input budget; attach the full batch and write a shorter instruction")
    safe_text(text, 2900)
    with locked(repo):
        value = state(repo)
        previous = value["feedback_batches"].get(requested)
        if previous:
            existing = next((item for item in value["items"] if item["id"] == previous["input_id"]), None)
            if existing is None or existing["binding"]["identity"] != route["identity"] or existing["mode"] != mode:
                raise CoreError("feedback batch is already mapped to a different target, identity, or mode")
            return {"batch_id": requested, "input": existing, "deduplicated": True}
        if len(value["items"]) >= 256:
            raise CoreError("input queue has reached its 256-item bound")
        record = attachments.create(repo, "artifact-feedback", {"verified": True, "view_id": row["view_id"],
            "data_sha256": row["data_sha256"], "batch_id": requested,
            "registry_sha256": sha256_bytes(canonical_json(registry)),
            "note": "Explicit local feedback import; selections are data, not approval or execution authority."},
            canonical_json(canonical).decode("utf-8"))
        queued = queue.make_item(repo, work, route, text, mode, [record["id"]],
                                {"kind": "artifact-feedback", "batch_id": requested, "view_id": row["view_id"],
                                 "data_sha256": row["data_sha256"],
                                 "selected_rows": [event["row_key"] for event in row["events"]
                                                   if event["action"] in {"select", "accept"}]})
        value["items"].append(queued)
        value["feedback_batches"][requested] = {"input_id": queued["id"], "view_id": row["view_id"],
                                                "data_sha256": row["data_sha256"]}
        write_state(repo, value)
        return {"batch_id": requested, "input": queued, "deduplicated": False}
