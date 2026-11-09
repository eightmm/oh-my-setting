"""Jupyter notebook parser: code cells go through the Python extractor; outputs are never read (W2)."""

from __future__ import annotations

import ast
import io
import json
import re
import tokenize
from typing import Any, Dict, List, Optional, Tuple

from .base import ParseResult, Parser
from .python import PythonParser

# One JSON lexeme: a string, a structural character, or a bare literal.
_LEXEME = re.compile(r'"(?:[^"\\]|\\.)*"|[{}\[\]:,]|[^\s{}\[\]:,"]+')
_PYTHON_SYMBOLS = ("class", "function", "method")


def _cell_fragments(text: str) -> Optional[Dict[int, Dict[str, Any]]]:
    """{cell index: {"type", "source": [(physical line, decoded string), ...]}} for the top-level `cells` array.

    A position-keeping walk over the JSON lexemes: `json` reports no source positions, and a symbol's line
    must be the line of the notebook file, whatever layout the file was written in."""
    frames: List[List[Any]] = []  # [bracket, current key, element index]
    cells: Dict[int, Dict[str, Any]] = {}
    line = 1
    cursor = 0
    try:
        for match in _LEXEME.finditer(text):
            line += text.count("\n", cursor, match.start())
            cursor = match.start()
            lexeme = match.group(0)
            if lexeme in ("{", "["):
                frames.append([lexeme, None, 0])
            elif lexeme in ("}", "]"):
                frames.pop()
            elif lexeme == ",":
                if frames and frames[-1][0] == "[":
                    frames[-1][2] += 1
                elif frames:
                    frames[-1][1] = None
            elif lexeme == ":":
                continue
            elif frames and frames[-1][0] == "{" and frames[-1][1] is None:
                frames[-1][1] = json.loads(lexeme)
            elif lexeme.startswith('"'):
                depth = len(frames)
                if (depth < 3 or frames[0][0] != "{" or frames[0][1] != "cells"
                        or frames[1][0] != "[" or frames[2][0] != "{"):
                    continue
                cell = cells.setdefault(frames[1][2], {"type": None, "source": []})
                key = frames[2][1]
                if depth == 3 and key == "cell_type":
                    cell["type"] = json.loads(lexeme)
                elif key == "source" and (depth == 3 or (depth == 4 and frames[3][0] == "[")):
                    cell["source"].append((line, json.loads(lexeme)))
    except (IndexError, TypeError, ValueError):
        return None
    return cells


def _logical_lines(fragments: List[Tuple[int, str]]) -> List[Tuple[str, int]]:
    """(source line, physical line where it starts) for one cell; only LF, CRLF and CR end a line, as in Python."""
    lines: List[Tuple[str, int]] = []
    current = ""
    start: Optional[int] = None
    for physical, fragment in fragments:
        parts = fragment.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        for index, part in enumerate(parts):
            if index:
                lines.append((current, start if start is not None else physical))
                current, start = "", None
            if part:
                current += part
                start = physical if start is None else start
    if current:
        lines.append((current, start if start is not None else 1))
    return lines


def _parses(lines: List[str]) -> bool:
    try:
        ast.parse("\n".join(lines))
    except (SyntaxError, ValueError, RecursionError, MemoryError):
        return False
    return True


def _continues(lines: List[str]) -> bool:
    """Whether the next line continues an unfinished logical line of `lines`."""
    try:
        for _ in tokenize.generate_tokens(io.StringIO("\n".join(lines) + "\n").readline):
            pass
    except tokenize.TokenError:
        return True
    except (SyntaxError, ValueError):
        return False
    return False


def _python_lines(lines: List[str]) -> Optional[List[str]]:
    """The cell's lines when they are Python, with IPython line magics and shell escapes blanked if that is
    what kept them from parsing; None for a cell magic (`%%bash`) or anything else that is not Python."""
    if _parses(lines):
        return lines
    first = next((line.lstrip() for line in lines if line.strip() and not line.lstrip().startswith("#")), "")
    if first.startswith("%%"):
        return None
    blanked: List[str] = []
    for line in lines:
        # `!= x` and `% y` inside an open bracket, string or backslash join continue Python; only a line that
        # starts a new logical line is a magic or shell escape.
        blanked.append("" if line.lstrip().startswith(("%", "!")) and not _continues(blanked) else line)
    return blanked if blanked != lines and _parses(blanked) else None


class NotebookParser(Parser):
    language = "notebook"
    extensions = (".ipynb",)
    version = 1

    def parse(self, path: str, text: str, source_digest: str) -> ParseResult:
        python = PythonParser()
        python.language = self.language
        empty = python.parse(path, "", source_digest)
        module = empty.nodes[0]
        module["metadata"].update({"code_cells": 0, "skipped_cells": 0})
        try:
            document = json.loads(text)
        except (ValueError, RecursionError):
            return empty
        cells = document.get("cells") if isinstance(document, dict) else None
        if not isinstance(cells, list):
            return empty
        code = [index for index, cell in enumerate(cells)
                if isinstance(cell, dict) and cell.get("cell_type") == "code"]
        module["metadata"].update({"code_cells": len(code), "skipped_cells": len(code)})
        metadata = document.get("metadata") if isinstance(document.get("metadata"), dict) else {}
        kernel = ""
        for section, key in (("language_info", "name"), ("kernelspec", "language")):
            value = metadata.get(section, {}).get(key) if isinstance(metadata.get(section), dict) else None
            kernel = kernel or (value.lower() if isinstance(value, str) else "")
        if kernel and not kernel.startswith(("python", "ipython")):
            return empty
        fragments = _cell_fragments(text)
        if fragments is None:
            return empty
        lines: List[str] = []
        physical: List[int] = []
        owners: List[int] = []
        parsed_cells = 0
        for index in code:
            source = cells[index].get("source", "")
            expected = "".join(source) if isinstance(source, list) and all(isinstance(item, str) for item in source) else source
            found = fragments.get(index, {"type": None, "source": []})
            # The positions are trusted only when the walk read exactly what `json` decoded.
            if (not isinstance(expected, str) or found["type"] != "code"
                    or "".join(fragment for _, fragment in found["source"]) != expected):
                continue
            logical = _logical_lines(found["source"])
            usable = _python_lines([line for line, _ in logical])
            if usable is None:
                continue
            parsed_cells += 1
            lines.extend(usable)
            physical.extend(start for _, start in logical)
            owners.extend([index + 1] * len(logical))
        if not _parses(lines):
            # Each cell parsed alone; the joined text is checked once more so the extractor below cannot raise.
            return empty
        result = python.parse(path, "\n".join(lines), source_digest)
        module = result.nodes[0]
        module["metadata"].update({"code_cells": len(code), "skipped_cells": len(code) - parsed_cells})

        def remap(line: Any) -> int:
            position = int(line) if isinstance(line, int) else 0
            return physical[position - 1] if 1 <= position <= len(physical) else 1

        for node in result.nodes:
            if node.get("kind") not in _PYTHON_SYMBOLS:
                continue
            node["language"] = self.language
            details = node["metadata"]
            position = details.get("line")
            if isinstance(position, int) and 1 <= position <= len(owners):
                details["cell"] = owners[position - 1]
            for key in ("line", "end_line"):
                if key in details:
                    details[key] = remap(details[key])
        for edge in result.edges:
            evidence = edge.get("evidence", {})
            if "line" in evidence:
                evidence["line"] = remap(evidence["line"])
        for ref in result.refs:
            if "line" in ref:
                ref["line"] = remap(ref["line"])
        return result
