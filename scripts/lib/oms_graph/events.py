"""Append-only execution run storage and pure state projection."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence

from oms_runtime.common import (
    append_jsonl, atomic_write_json, bounded_line, canonical_json, read_json,
    read_jsonl, safe_id, sensitive_text, sha256_bytes, utc_now,
)

from . import EVENTS_SCHEMA, EVENT_TYPES
from .errors import GraphError
from .spec import normalize_spec, spec_digest
from .validate import validate_spec

import re
import uuid

RUN_ID_RE = re.compile(r"^run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$")


def runs_root(repo: Path) -> Path:
    return repo / ".oms" / "graph" / "runs"


def run_dir(repo: Path, run_id: str) -> Path:
    if not RUN_ID_RE.fullmatch(str(run_id or "")):
        raise GraphError("invalid run id: %s" % run_id)
    return runs_root(repo) / run_id


def new_run_id() -> str:
    stamp = utc_now().replace("-", "").replace(":", "")
    return "run-%s-%s" % (stamp, uuid.uuid4().hex[:8])


def start_run(repo: Path, spec: Mapping[str, Any], *, run_id: str = "", options: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    normalized = normalize_spec(spec)
    verdict = validate_spec(normalized)
    if not verdict["ok"]:
        raise GraphError("cannot start an invalid graph: %s" % ", ".join(item["code"] for item in verdict["errors"]))
    selected = run_id or new_run_id()
    directory = run_dir(Path(repo).resolve(), selected)
    graph_path = directory / "graph.json"
    event_path = directory / "events.jsonl"
    if graph_path.exists() or graph_path.is_symlink() or event_path.exists() or event_path.is_symlink():
        raise GraphError("graph run already exists: %s" % selected)
    atomic_write_json(graph_path, normalized)
    event = append_event(
        Path(repo).resolve(), selected, "run_started",
        idempotency_key="run:start", spec_id=normalized.get("id", ""),
        spec_digest=spec_digest(normalized), options=dict(options or {}),
    )
    return {"schema": 1, "run_id": selected, "spec_id": normalized.get("id", ""), "spec_digest": spec_digest(normalized), "event": event}


def append_event(repo: Path, run_id: str, event: str, **fields: Any) -> Dict[str, Any]:
    if event not in EVENT_TYPES:
        raise GraphError("unknown graph event: %s" % event)
    directory = run_dir(Path(repo).resolve(), run_id)
    if not (directory / "graph.json").is_file() or (directory / "graph.json").is_symlink():
        raise GraphError("graph run does not exist: %s" % run_id)
    path = directory / "events.jsonl"
    existing = read_jsonl(path)
    idempotency_key = str(fields.get("idempotency_key", ""))
    if idempotency_key:
        try:
            safe_id(idempotency_key, "idempotency key")
        except Exception as exc:
            raise GraphError(str(exc))
        if any(str(row.get("idempotency_key", "")) == idempotency_key for row in existing):
            raise GraphError("duplicate graph event idempotency key: %s" % idempotency_key)
    raw_detail = str(fields.get("detail", ""))
    if raw_detail and sensitive_text(raw_detail):
        raise GraphError("graph event detail contains sensitive text")
    detail = bounded_line(raw_detail, 500)
    reserved = {"schema", "seq", "ts", "run_id", "event"}
    collision = sorted(reserved.intersection(fields))
    if collision:
        raise GraphError("event fields cannot replace: %s" % ", ".join(collision))
    last_seq = existing[-1].get("seq", 0) if existing else 0
    if isinstance(last_seq, bool) or not isinstance(last_seq, int) or last_seq < 0:
        raise GraphError("graph event log has an invalid final sequence")
    row: Dict[str, Any] = {
        "schema": EVENTS_SCHEMA,
        "seq": last_seq + 1,
        "ts": utc_now(),
        "run_id": run_id,
        "event": event,
    }
    row.update(fields)
    if raw_detail or "detail" in fields:
        row["detail"] = detail
    append_jsonl(path, row)
    return row


def read_events(repo: Path, run_id: str) -> List[Dict[str, Any]]:
    rows = read_jsonl(run_dir(Path(repo).resolve(), run_id) / "events.jsonl")
    for row in rows:
        if row.get("schema") != EVENTS_SCHEMA or row.get("run_id") != run_id or row.get("event") not in EVENT_TYPES:
            raise GraphError("invalid event row in graph run %s" % run_id)
    return rows


def load_run_spec(repo: Path, run_id: str) -> Dict[str, Any]:
    value = read_json(run_dir(Path(repo).resolve(), run_id) / "graph.json", default=None)
    if not isinstance(value, Mapping):
        raise GraphError("graph run has no valid frozen spec: %s" % run_id)
    return normalize_spec(value)


def project(events: Sequence[Mapping[str, Any]], spec: Mapping[str, Any]) -> Dict[str, Any]:
    """Pure, idempotent fold of events into evaluator state."""
    graph = normalize_spec(spec)
    nodes: Dict[str, Dict[str, Any]] = {
        node_id: {"status": "pending", "outcome": None, "claimed_outcome": None, "attempts": 0}
        for node_id in graph["nodes"]
    }
    gates: Dict[str, str] = {}
    repeats: Dict[str, int] = {}
    # A binding is the concrete task an attempt froze; the latest row for a
    # name wins, so a repeating writer rebinds and replay reproduces it.
    bindings: Dict[str, Dict[str, Any]] = {}
    seen: set = set()
    attempts_seen: set = set()
    child_events: Dict[str, List[Dict[str, Any]]] = {}
    subgraph_nodes = {node_id for node_id, node in graph["nodes"].items() if node.get("kind") == "subgraph"}

    for raw in events:
        if not isinstance(raw, Mapping):
            continue
        key = str(raw.get("idempotency_key", ""))
        identity = ("key", key) if key else ("row", sha256_bytes(canonical_json(dict(raw))))
        if identity in seen:
            continue
        seen.add(identity)
        event = raw.get("event")
        raw_node = str(raw.get("node", ""))
        parent, separator, child = raw_node.partition(".")
        if separator and parent in subgraph_nodes:
            copy = dict(raw)
            copy["node"] = child
            child_events.setdefault(parent, []).append(copy)
            attempt = raw.get("attempt", 1)
            if event in ("node_started", "node_outcome", "gate_decision"):
                attempts_seen.add((raw_node, attempt if isinstance(attempt, int) else 1))
            continue
        if event not in EVENT_TYPES or raw_node not in nodes:
            if event == "route_evaluated":
                route = raw.get("route", raw)
                candidate = route.get("budget", {}).get("repeats", {}) if isinstance(route, Mapping) else {}
                if isinstance(candidate, Mapping):
                    for repeat_key, count in candidate.items():
                        if isinstance(count, int) and not isinstance(count, bool) and count >= 0:
                            repeats[str(repeat_key)] = max(repeats.get(str(repeat_key), 0), count)
            continue
        node = nodes[raw_node]
        attempt = raw.get("attempt")
        if isinstance(attempt, bool) or not isinstance(attempt, int) or attempt <= 0:
            attempt = node["attempts"] or 1
            if event == "node_started" and node["status"] != "active":
                attempt = node["attempts"] + 1
        # The row's sequence orders one node's latest attempt against another's:
        # the route evaluator uses it to tell a repeat that already happened
        # from one that is due.
        seq = raw.get("seq")
        if event in ("node_started", "node_outcome", "gate_decision") and isinstance(seq, int) and not isinstance(seq, bool):
            node["seq"] = max(int(node.get("seq", 0) or 0), seq)
        task_id = str(raw.get("task_id", "") or "")
        if event == "node_started":
            node["attempts"] = max(node["attempts"], attempt)
            node["status"] = "active"
            node["outcome"] = None
            node["claimed_outcome"] = None
            if task_id:
                node["task_id"] = task_id
            attempts_seen.add((raw_node, attempt))
        elif event == "node_outcome":
            node["attempts"] = max(node["attempts"], attempt)
            node["status"] = "finished"
            node["claimed_outcome"] = raw.get("claimed_outcome", raw.get("outcome"))
            node["outcome"] = raw.get("outcome", raw.get("claimed_outcome"))
            if task_id:
                node["task_id"] = task_id
            attempts_seen.add((raw_node, attempt))
        if event in ("node_started", "node_outcome") and task_id:
            name = str(raw.get("binding", "") or "")
            if name:
                bindings[name] = {"task_id": task_id, "node": raw_node, "attempt": attempt}
        elif event == "gate_decision":
            decision = str(raw.get("outcome", raw.get("decision", "")))
            if decision:
                gates[raw_node] = decision
                node["attempts"] = max(node["attempts"], attempt)
                node["status"] = "finished"
                node["claimed_outcome"] = decision
                node["outcome"] = decision
                attempts_seen.add((raw_node, attempt))

    subgraph_states: Dict[str, Any] = {}
    for parent in sorted(child_events):
        graph_name = graph["nodes"][parent].get("graph")
        child_spec = graph.get("subgraphs", {}).get(graph_name)
        if child_spec:
            subgraph_states[parent] = project(child_events[parent], child_spec)
    return {
        "nodes": nodes,
        "steps": len(attempts_seen),
        "repeats": dict(sorted(repeats.items())),
        "gates": dict(sorted(gates.items())),
        "bindings": dict(sorted(bindings.items())),
        "subgraphs": subgraph_states,
    }


def write_projection(repo: Path, run_id: str, projection: Mapping[str, Any]) -> Path:
    return atomic_write_json(run_dir(Path(repo).resolve(), run_id) / "projection.json", dict(projection))


def list_runs(repo: Path) -> List[Dict[str, Any]]:
    root = runs_root(Path(repo).resolve())
    if not root.is_dir() or root.is_symlink():
        return []
    result: List[Dict[str, Any]] = []
    for path in sorted(root.iterdir(), key=lambda item: item.name):
        if not RUN_ID_RE.fullmatch(path.name) or path.is_symlink() or not path.is_dir():
            continue
        try:
            graph = load_run_spec(repo, path.name)
            rows = read_events(repo, path.name)
            projection = project(rows, graph)
        except Exception:
            continue
        result.append({
            "schema": 1, "run_id": path.name, "spec_id": graph.get("id", ""),
            "event_count": len(rows), "steps": projection.get("steps", 0),
            "active": sorted(node_id for node_id, node in projection["nodes"].items() if node.get("status") == "active"),
        })
    return result


def latest_run_id(repo: Path) -> str:
    runs = list_runs(repo)
    return runs[-1]["run_id"] if runs else ""
