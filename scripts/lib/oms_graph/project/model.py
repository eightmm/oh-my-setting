"""Project graph node/edge shapes, ids, and canonical ordering (W2)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def node_id(kind: str, path: str, qualname: str = "") -> str:
    _todo("project.model.node_id")


def make_node(kind: str, name: str, path: str, language: str, source_digest: str, *, qualname: str = "", line: Optional[int] = None, metadata: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    _todo("project.model.make_node")


def make_edge(source: str, target: str, relation: str, confidence: str, *, path: str, source_digest: str, line: Optional[int] = None, candidates: Optional[Sequence[str]] = None) -> Dict[str, Any]:
    _todo("project.model.make_edge")


def sort_nodes(nodes: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    _todo("project.model.sort_nodes")


def sort_edges(edges: Sequence[Mapping[str, Any]]) -> List[Dict[str, Any]]:
    _todo("project.model.sort_edges")
