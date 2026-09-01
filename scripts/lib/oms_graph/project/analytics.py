"""Stdlib graph analytics: degrees, hubs, components, cycles, paths, communities (W-G)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def degrees(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]]) -> Dict[str, Dict[str, int]]:
    _todo("project.analytics.degrees")


def hubs(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, limit: int = 10, kinds: Sequence[str] = ()) -> List[Dict[str, Any]]:
    _todo("project.analytics.hubs")


def connected_components(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, undirected: bool = True) -> List[List[str]]:
    _todo("project.analytics.connected_components")


def cycles(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, relations: Sequence[str] = ("imports", "calls"), limit: int = 20) -> List[List[str]]:
    _todo("project.analytics.cycles")


def shortest_path(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], source: str, target: str, *, undirected: bool = True) -> Optional[List[str]]:
    _todo("project.analytics.shortest_path")


def communities(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, max_rounds: int = 20) -> List[Dict[str, Any]]:
    _todo("project.analytics.communities")
