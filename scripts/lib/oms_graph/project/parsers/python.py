"""Python parser (W2)."""

from __future__ import annotations

from .base import ParseResult, Parser


class PythonParser(Parser):
    language = "python"
    extensions = (".py",)
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        raise NotImplementedError("python parser is not implemented yet")
