"""Parser protocol and result container (W2)."""

from __future__ import annotations

from typing import Any, Dict, List, Tuple


class ParseResult:
    """nodes/edges are complete; refs are unresolved cross-file references.

    ref shape: {"from": node id, "relation": str, "kind": "module|name|path", "value": str, "line": int}
    """

    def __init__(self) -> None:
        self.nodes: List[Dict[str, Any]] = []
        self.edges: List[Dict[str, Any]] = []
        self.refs: List[Dict[str, Any]] = []

    def as_dict(self) -> Dict[str, Any]:
        return {"nodes": list(self.nodes), "edges": list(self.edges), "refs": list(self.refs)}


class Parser:
    language: str = ""
    extensions: Tuple[str, ...] = ()
    version: int = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        raise NotImplementedError
