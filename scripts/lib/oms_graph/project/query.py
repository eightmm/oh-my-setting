"""Graph index: find, neighbors, trace, map summary (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)



class Graph:
    """In-memory index over a loaded graph.json."""

    def __init__(self, graph: Mapping[str, Any]) -> None:
        _todo("project.query.Graph")

    def node(self, node_id: str) -> Dict[str, Any]:
        _todo("project.query.Graph.node")

    def find(self, query: str, *, kinds: Sequence[str] = (), limit: int = 20) -> List[Dict[str, Any]]:
        _todo("project.query.Graph.find")

    def neighbors(self, node_id: str, *, relation: str = "", direction: str = "both") -> List[Dict[str, Any]]:
        _todo("project.query.Graph.neighbors")

    def trace(self, node_id: str, *, direction: str = "out", depth: int = 2, relations: Sequence[str] = ()) -> Dict[str, Any]:
        _todo("project.query.Graph.trace")

    def map_summary(self) -> Dict[str, Any]:
        _todo("project.query.Graph.map_summary")
