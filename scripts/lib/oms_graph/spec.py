"""Loading and deterministic normalization for execution graph specs."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List, Mapping

from oms_runtime.common import canonical_json, install_root, read_json, sha256_bytes

from .errors import GraphError


def bundled_dir() -> Path:
    """Return the installed directory containing bundled graph specs."""
    return install_root() / "config" / "graphs"


def bundled_names() -> List[str]:
    return sorted(path.stem for path in bundled_dir().glob("*.json"))


def load_spec(source: Any) -> Dict[str, Any]:
    """Accept a path, bundled name, JSON text, or mapping."""
    if isinstance(source, Mapping):
        raw = dict(source)
    elif isinstance(source, Path):
        raw = read_json(source, default=None)
        if raw is None:
            raise GraphError("graph spec does not exist: %s" % source)
    elif isinstance(source, str):
        text = source.strip()
        if not text:
            raise GraphError("graph spec source is empty")
        path = Path(text)
        bundled = bundled_dir() / (text + ".json")
        if path.is_file():
            raw = read_json(path, default=None)
        elif text in bundled_names() and bundled.is_file():
            raw = read_json(bundled, default=None)
        else:
            try:
                raw = json.loads(text)
            except ValueError as exc:
                raise GraphError("graph spec is not a path, bundled name, or JSON object: %s" % exc)
    else:
        raise GraphError("graph spec source must be a path, name, JSON text, or mapping")
    if not isinstance(raw, Mapping):
        raise GraphError("graph spec must be a JSON object")
    return normalize_spec(raw)


def _string_list(value: Any, label: str) -> List[str]:
    if value is None:
        return []
    if not isinstance(value, (list, tuple)) or any(not isinstance(item, str) for item in value):
        raise GraphError("%s must be a list of strings" % label)
    return list(value)


def _normalize_graph(raw: Mapping[str, Any], graph_id: str) -> Dict[str, Any]:
    nodes_raw = raw.get("nodes", {})
    edges_raw = raw.get("edges", [])
    if not isinstance(nodes_raw, Mapping):
        raise GraphError("nodes must be an object")
    if not isinstance(edges_raw, (list, tuple)):
        raise GraphError("edges must be a list")

    nodes: Dict[str, Any] = {}
    for raw_id in sorted(nodes_raw, key=lambda item: str(item)):
        node_id = str(raw_id)
        value = nodes_raw[raw_id]
        if not isinstance(value, Mapping):
            raise GraphError("node %s must be an object" % node_id)
        node = dict(value)
        kind = node.get("kind")
        node["title"] = str(node.get("title", node_id))
        if kind != "terminal":
            node["requires"] = _string_list(node.get("requires", []), "node %s requires" % node_id)
        if kind in ("agent", "tool"):
            node["effect"] = node.get("effect", "read")
            node["proof"] = _string_list(node.get("proof", []), "node %s proof" % node_id)
        if kind == "agent" and node.get("plan_task"):
            node["mode"] = node.get("mode", "run")
        if kind == "tool":
            node["timeout"] = node.get("timeout", 600)
            node["cacheable"] = node.get("cacheable", False)
        if kind == "gate":
            node["authority"] = node.get("authority", "parent")
            node["decisions"] = _string_list(
                node.get("decisions", ["approved", "changes_requested"]),
                "node %s decisions" % node_id,
            )
        node["join"] = node.get("join", "all")
        nodes[node_id] = node

    edges: List[Dict[str, Any]] = []
    for index, value in enumerate(edges_raw):
        if not isinstance(value, Mapping):
            raise GraphError("edge %d must be an object" % index)
        edge = dict(value)
        edge["from"] = str(edge.get("from", ""))
        edge["to"] = str(edge.get("to", ""))
        edge["outcomes"] = _string_list(edge.get("outcomes", []), "edge %d outcomes" % index)
        edge["kind"] = edge.get("kind", "normal")
        edge["priority"] = edge.get("priority", 0)
        edge["fanout"] = edge.get("fanout", False)
        edge["when"] = _string_list(edge.get("when", []), "edge %d when" % index)
        edges.append(edge)

    budget_raw = raw.get("budget", {})
    if not isinstance(budget_raw, Mapping):
        raise GraphError("budget must be an object")
    result: Dict[str, Any] = dict(raw)
    result["schema"] = raw.get("schema", 1)
    result["id"] = str(raw.get("id", graph_id))
    result["entry"] = str(raw.get("entry", ""))
    result["budget"] = {
        "max_steps": budget_raw.get("max_steps", 20),
        "max_repeats": budget_raw.get("max_repeats", 3),
    }
    result["stop_facts"] = _string_list(raw.get("stop_facts", []), "stop_facts")
    result["nodes"] = nodes
    result["edges"] = edges

    subgraphs_raw = raw.get("subgraphs", {})
    if not isinstance(subgraphs_raw, Mapping):
        raise GraphError("subgraphs must be an object")
    subgraphs: Dict[str, Any] = {}
    for raw_name in sorted(subgraphs_raw, key=lambda item: str(item)):
        name = str(raw_name)
        child = subgraphs_raw[raw_name]
        if not isinstance(child, Mapping):
            raise GraphError("subgraph %s must be an object" % name)
        child_raw = dict(child)
        child_raw.setdefault("schema", result["schema"])
        child_raw.setdefault("id", name)
        subgraphs[name] = _normalize_graph(child_raw, name)
    if subgraphs or "subgraphs" in raw:
        result["subgraphs"] = subgraphs
    return result


def normalize_spec(raw: Mapping[str, Any]) -> Dict[str, Any]:
    """Fill documented defaults without consulting external state."""
    if not isinstance(raw, Mapping):
        raise GraphError("graph spec must be an object")
    return _normalize_graph(raw, str(raw.get("id", "graph")))


def spec_digest(spec: Mapping[str, Any]) -> str:
    """Return the SHA-256 of canonical normalized JSON."""
    return sha256_bytes(canonical_json(normalize_spec(spec)))
