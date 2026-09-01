"""Markdown parser (W2)."""

from __future__ import annotations

from .base import ParseResult, Parser


class MarkdownParser(Parser):
    language = "markdown"
    extensions = (".md", ".markdown")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        raise NotImplementedError("markdown parser is not implemented yet")
