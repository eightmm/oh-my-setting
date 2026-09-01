"""Text and mermaid renderers for both graphs (W-G)."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from .errors import GraphError


def _todo(name):
    raise GraphError("%s is not implemented yet" % name)


def render_exec_text(spec: Mapping[str, Any], projection: Optional[Mapping[str, Any]] = None, route: Optional[Mapping[str, Any]] = None) -> str:
    _todo("render.render_exec_text")


def render_exec_mermaid(spec: Mapping[str, Any], projection: Optional[Mapping[str, Any]] = None) -> str:
    _todo("render.render_exec_mermaid")


def render_project_map_text(summary: Mapping[str, Any]) -> str:
    _todo("render.render_project_map_text")


def render_project_mermaid(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]], *, limit: int = 200) -> str:
    _todo("render.render_project_mermaid")
