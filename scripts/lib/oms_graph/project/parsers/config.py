"""Config parser (W2)."""

from __future__ import annotations

from ..model import make_node
from .base import ParseResult, Parser


class ConfigParser(Parser):
    language = "config"
    extensions = (".json", ".yaml", ".yml", ".toml")
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        result = ParseResult()
        result.nodes.append(make_node("config", path.rsplit("/", 1)[-1], path, self.language, source_digest))
        return result
