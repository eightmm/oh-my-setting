"""Shell parser (W2)."""

from __future__ import annotations

from .base import ParseResult, Parser


class ShellParser(Parser):
    language = "shell"
    extensions = (".sh", ".bash")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        raise NotImplementedError("shell parser is not implemented yet")
