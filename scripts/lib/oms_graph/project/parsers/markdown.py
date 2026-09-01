"""Markdown parser (W2)."""

from __future__ import annotations

from ..model import PATH_LITERAL_RE, make_node, node_id
from .base import ParseResult, Parser


class MarkdownParser(Parser):
    language = "markdown"
    extensions = (".md", ".markdown")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        result.nodes.append(make_node("document", path.rsplit("/", 1)[-1], path, self.language, source_digest))
        source = node_id("document", path)
        pattern = PATH_LITERAL_RE
        for line, content in enumerate(text.splitlines(), 1):
            for match in pattern.finditer(content):
                result.refs.append({"from": source, "relation": "references", "kind": "path", "value": match.group(1).rstrip(".,:;`"), "line": line})
        return result
