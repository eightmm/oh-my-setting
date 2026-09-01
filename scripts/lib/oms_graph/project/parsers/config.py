"""Config parser (W2)."""

from __future__ import annotations

from .base import ParseResult, Parser


class ConfigParser(Parser):
    language = "config"
    extensions = (".json", ".yaml", ".yml", ".toml")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        raise NotImplementedError("config parser is not implemented yet")
