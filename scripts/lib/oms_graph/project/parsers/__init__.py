"""Parser registry: one module per language, selected by file extension (W2)."""

from __future__ import annotations

from typing import Dict, Optional

from .base import Parser


def registry() -> Dict[str, Parser]:
    """Extension (with leading dot) -> parser instance."""
    from . import config, markdown, python, shell
    result: Dict[str, Parser] = {}
    for parser in (python.PythonParser(), shell.ShellParser(), markdown.MarkdownParser(), config.ConfigParser()):
        for extension in parser.extensions:
            result[extension] = parser
    return result


def parser_for(path: str) -> Optional[Parser]:
    name = str(path)
    lower = name.lower()
    for extension, parser in sorted(registry().items(), key=lambda item: -len(item[0])):
        if lower.endswith(extension):
            return parser
    return None
