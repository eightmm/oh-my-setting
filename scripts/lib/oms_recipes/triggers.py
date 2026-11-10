"""Idempotent explicit ticks/events create requests, never autonomous execution."""

from __future__ import annotations

from pathlib import Path
import uuid

from oms_runtime.common import CoreError, atomic_write_json, file_lock, read_json, utc_now
from .common import (
    MAX_RECORDS, bounded_int, check_secrets, digest, history, identifier,
    object_fields, records, relative, state_path, state_root, text, timestamp, under,
)
from .definitions import bind_inputs, bundle, compile_plan
from .runner import execute


def _definition(value: dict) -> dict:
    object_fields(value, {"schema", "kind", "id", "recipe", "overlay", "inputs", "action", "ports", "companions", "schedule", "event"},
                  {"schema", "kind", "id", "recipe"}, "recipe trigger")
    if value["schema"] != 1 or type(value["schema"]) is not int or value["kind"] != "recipe-trigger":
        raise CoreError("expected recipe-trigger schema 1")
    identifier(value["id"])
    for key in ("recipe", "overlay"):
        if key in value:
            text(value[key], "trigger " + key, 1024)
            if not value[key].startswith("pack:"):
                relative(value[key], "trigger definition path")
    if ("schedule" in value) == ("event" in value):
        raise CoreError("trigger requires exactly one of schedule or event")
    if "schedule" in value:
        schedule = object_fields(value["schedule"], {"at", "every_seconds"}, {"at"}, "schedule")
        timestamp(schedule["at"])
        if "every_seconds" in schedule:
            bounded_int(schedule["every_seconds"], "every_seconds", 1, 366 * 86400)
    else:
        event = object_fields(value["event"], {"name", "match"}, {"name"}, "event filter")
        identifier(event["name"], "event name")
        filters = event.get("match", {})
        if not isinstance(filters, dict) or len(filters) > 32:
            raise CoreError("event match must be a bounded flat object")
        for name, expected in filters.items():
            identifier(name, "event field", name=True)
            if not isinstance(expected, (str, int, float, bool)) and expected is not None:
                raise CoreError("event equality filters require scalar values")
    if not isinstance(value.get("inputs", {}), dict) or not isinstance(value.get("ports", {}), dict):
        raise CoreError("trigger inputs and ports must be objects")
    if not isinstance(value.get("companions", []), list):
        raise CoreError("trigger companions must be an array")
    if "action" in value:
        identifier(value["action"], "workspace action", name=True)
    check_secrets(value)
    return value


def _plan(repo: Path, definition: dict, pins: dict | None = None) -> dict:
    loaded = bundle(repo, definition["recipe"], definition.get("overlay", ""))
    if pins and any(loaded[key] != pins[key] for key in ("definition_digest", "overlay_digest", "effective_digest")):
        raise CoreError("trigger recipe or overlay changed; register a new trigger id")
    inputs = bind_inputs(loaded["definition"].get("inputs", {}), definition.get("inputs", {}))
    return compile_plan(repo, loaded, inputs, action=definition.get("action", ""),
                        ports=definition.get("ports", {}), companions=definition.get("companions", []))


def add(repo: Path, value: dict) -> dict:
    value = _definition(value)
    plan = _plan(repo, value)
    root = state_root(repo, create=True)
    with file_lock(under(root, "trigger-coordinator")):
        path = state_path(repo, "triggers", value["id"])
        previous = read_json(path)
        if previous is not None:
            if previous.get("digest") != digest(value):
                raise CoreError("trigger definitions are immutable; disable the old id and register a new id")
            return previous
        if len(records(repo, "triggers")) >= MAX_RECORDS:
            raise CoreError("trigger capacity reached")
        row = {"schema": 1, "id": value["id"], "definition": value, "digest": digest(value), "enabled": True,
               "created_at": utc_now(), "last_index": -1,
               "pins": {key: plan[key] for key in ("definition_digest", "overlay_digest", "effective_digest")}}
        atomic_write_json(path, row)
        history(repo, "trigger-added", id=value["id"], digest=row["digest"])
    return row


def set_enabled(repo: Path, key: str, enabled: bool) -> dict:
    root = state_root(repo, create=True)
    with file_lock(under(root, "trigger-coordinator")):
        path = state_path(repo, "triggers", key)
        row = read_json(path)
        if not isinstance(row, dict) or row.get("id") != key:
            raise CoreError("trigger is missing or invalid")
        row["enabled"] = enabled
        atomic_write_json(path, row)
        history(repo, "trigger-enabled" if enabled else "trigger-disabled", id=key)
        return row


def _queue(repo: Path, trigger: dict, occurrence: str, metadata: dict) -> dict:
    definition = _definition(trigger.get("definition"))
    if digest(definition) != trigger.get("digest"):
        raise CoreError("stored trigger digest mismatch")
    key = "request-" + digest({"trigger": trigger["digest"], "occurrence": occurrence})[:32]
    path = state_path(repo, "requests", key)
    previous = read_json(path)
    if previous is not None:
        return previous
    if len(records(repo, "requests")) >= MAX_RECORDS:
        raise CoreError("recipe request capacity reached")
    row = {"schema": 1, "id": key, "trigger_id": trigger["id"], "trigger_digest": trigger["digest"],
           "definition": definition, "pins": trigger["pins"], "occurrence": occurrence, "created_at": utc_now(),
           "status": "pending", "metadata": metadata, "authority": "request only; explicit execution required"}
    atomic_write_json(path, row)
    history(repo, "recipe-requested", id=key, trigger_id=trigger["id"], occurrence=occurrence)
    return row


def tick(repo: Path, now: str) -> dict:
    moment = timestamp(now)
    root = state_root(repo, create=True)
    queued = []
    with file_lock(under(root, "trigger-coordinator")):
        for row in records(repo, "triggers"):
            definition = _definition(row.get("definition"))
            schedule = definition.get("schedule")
            if not row.get("enabled") or schedule is None:
                continue
            due = timestamp(schedule["at"])
            if moment < due:
                continue
            elapsed = int((moment - due).total_seconds())
            index = elapsed // schedule["every_seconds"] if "every_seconds" in schedule else 0
            previous = bounded_int(row.get("last_index", -1), "last schedule index", -1, 10**15)
            if index <= previous:
                continue
            # One latest request represents overdue work; ticks never launch a
            # burst of historical commands after a machine has been offline.
            request = _queue(repo, row, "schedule:" + str(index),
                             {"observed_at": now, "schedule_index": index, "coalesced_occurrences": index - previous - 1})
            queued.append(request)
            row["last_index"] = index
            atomic_write_json(state_path(repo, "triggers", row["id"]), row)
    return {"schema": 1, "at": now, "requests": queued, "executed": False}


def event(repo: Path, name: str, event_id: str, payload: dict) -> dict:
    identifier(name, "event name")
    identifier(event_id, "event id")
    if not isinstance(payload, dict) or len(payload) > 32:
        raise CoreError("event data must be a bounded flat object")
    check_secrets(payload)
    for key, item in payload.items():
        identifier(key, "event field", name=True)
        if type(item) not in (str, int, float, bool, type(None)):
            raise CoreError("event data values must be scalar")
        if isinstance(item, str):
            text(item, "event data", 1024, empty=True)
    fingerprint = digest(payload)
    root = state_root(repo, create=True)
    with file_lock(under(root, "trigger-coordinator")):
        path = state_path(repo, "events", "event-" + digest({"name": name, "id": event_id})[:32])
        prior = read_json(path)
        if prior is not None:
            if prior.get("payload_digest") != fingerprint:
                raise CoreError("event id was already used for different data")
            if prior.get("status") == "submitted":
                return prior
            selected = prior["matched"]
        else:
            if len(records(repo, "events")) >= MAX_RECORDS:
                raise CoreError("event history capacity reached")
            selected = []
            for trigger in records(repo, "triggers"):
                definition = _definition(trigger.get("definition"))
                filter_ = definition.get("event", {})
                if trigger.get("enabled") and filter_.get("name") == name and all(key in payload and type(payload[key]) is type(item) and payload[key] == item for key, item in filter_.get("match", {}).items()):
                    selected.append(trigger["id"])
            prior = {"schema": 1, "id": path.stem, "name": name, "event_id": event_id, "payload_digest": fingerprint,
                     "matched": selected, "status": "submitting", "submitted_at": utc_now(), "requests": []}
            atomic_write_json(path, prior)
        requests = []
        for key in selected:
            trigger = read_json(state_path(repo, "triggers", key))
            if not isinstance(trigger, dict):
                raise CoreError("matched trigger is missing")
            queued = _queue(repo, trigger, "event:" + name + ":" + event_id, {"event": name, "event_id": event_id, "payload_digest": fingerprint})
            requests.append(queued["id"])
        prior.update(status="submitted", requests=requests)
        atomic_write_json(path, prior)
        history(repo, "event-submitted", id=prior["id"], name=name, event_id=event_id, requests=requests)
    return prior


def request(repo: Path, key: str) -> dict:
    value = read_json(state_path(repo, "requests", key))
    if not isinstance(value, dict) or value.get("id") != key:
        raise CoreError("recipe request is missing or invalid")
    definition = _definition(value.get("definition"))
    if digest(definition) != value.get("trigger_digest"):
        raise CoreError("request definition digest mismatch")
    return value


def request_plan(repo: Path, key: str) -> dict:
    value = request(repo, key)
    return _plan(repo, value["definition"], value["pins"])


def list_requests(repo: Path) -> list[dict]:
    result = []
    for row in records(repo, "requests"):
        row = dict(row)
        if row.get("run_id"):
            run = read_json(state_path(repo, "runs", row["run_id"]))
            row["effective_status"] = run.get("status", "invalid") if isinstance(run, dict) else "uncertain"
        else:
            row["effective_status"] = row["status"]
        result.append(row)
    return result


def cancel(repo: Path, key: str) -> dict:
    path = state_path(repo, "requests", key)
    with file_lock(path):
        row = request(repo, key)
        if row["status"] != "pending":
            raise CoreError("only pending requests can be cancelled")
        row.update(status="cancelled", updated_at=utc_now())
        atomic_write_json(path, row)
        history(repo, "request-cancelled", id=key)
        return row


def run_request(repo: Path, key: str, *, retry_failed: bool = False, retry_uncertain: bool = False) -> tuple[dict, int]:
    path = state_path(repo, "requests", key)
    with file_lock(path):
        row = request(repo, key)
        if row["status"] == "cancelled":
            raise CoreError("request was cancelled")
        plan = request_plan(repo, key)
        resume = False
        if row.get("run_id"):
            run_id = row["run_id"]
            existing = read_json(state_path(repo, "runs", run_id))
            resume = existing is not None
            if not resume and not retry_uncertain:
                raise CoreError("request has no run receipt after dispatch; --retry-uncertain is required")
        else:
            run_id = "run-" + uuid.uuid4().hex
        row.update(status="running", run_id=run_id, updated_at=utc_now())
        atomic_write_json(path, row)
        history(repo, "request-dispatched", id=key, run_id=run_id)
        run, exit_code = execute(repo, plan, run_id=run_id, resume=resume,
                                 retry_failed=retry_failed, retry_uncertain=retry_uncertain, request_id=key)
        row.update(status=run["status"], updated_at=utc_now())
        atomic_write_json(path, row)
        return {"request": row, "run": run}, exit_code
