"""Small Markdown helpers for OMS task/project contracts."""

from __future__ import annotations

import collections
import re
from typing import Dict, Iterable, List, Mapping, Sequence, Tuple

from .common import bounded_line, sha256_text


def normalized_line(text: str) -> str:
    value = re.sub(r"\[[ xX]\]\s*", "", text.strip())
    value = re.sub(r"\s+", " ", value)
    return value.strip(" -\t")


def sections(text: str) -> Tuple[Dict[str, List[str]], Dict[str, str]]:
    result: Dict[str, List[str]] = collections.OrderedDict()
    metadata: Dict[str, str] = {}
    current = "__preamble__"
    result[current] = []
    frontmatter = False
    for index, raw in enumerate(text.splitlines()):
        line = raw.rstrip("\r")
        if index == 0 and line.strip() == "---":
            frontmatter = True
            continue
        if frontmatter:
            if line.strip() == "---":
                frontmatter = False
                continue
            match = re.match(r"^([A-Za-z][A-Za-z0-9_.-]{0,63}):\s*(.*)$", line)
            if match:
                metadata[match.group(1).lower()] = match.group(2).strip()
            continue
        heading = re.match(r"^#{1,6}\s+(.+?)\s*$", line)
        if heading:
            current = heading.group(1).strip()
            result.setdefault(current, [])
            continue
        result.setdefault(current, []).append(line)
    for line in result.get("__preamble__", [])[:40]:
        match = re.match(r"^\s*-\s*([A-Za-z][A-Za-z0-9_.-]{0,63}):\s*(.*)$", line)
        if match:
            metadata.setdefault(match.group(1).lower(), match.group(2).strip())
    return result, metadata


def section_text(mapping: Mapping[str, Sequence[str]], names: Sequence[str]) -> str:
    wanted = [name.lower() for name in names]
    body: List[str] = []
    for heading, lines in mapping.items():
        lower = heading.lower()
        if any(token in lower for token in wanted):
            body.extend(lines)
    return "\n".join(body).strip()


def section_text_exact(mapping: Mapping[str, Sequence[str]], names: Sequence[str]) -> str:
    """Return only exact normalized headings, avoiding Verify/Verification collisions."""
    wanted = {re.sub(r"\s+", " ", name.strip().lower()) for name in names}
    body: List[str] = []
    for heading, lines in mapping.items():
        normalized = re.sub(r"\s+", " ", heading.strip().lower())
        if normalized in wanted:
            body.extend(lines)
    return "\n".join(body).strip()


def bullet_items(text: str) -> List[str]:
    result: List[str] = []
    pending = ""
    for raw in text.splitlines():
        match = re.match(r"^\s*(?:[-*+]\s+|\d+[.)]\s+)(?:\[[ xX]\]\s*)?(.*)$", raw)
        if match:
            if pending:
                result.append(normalized_line(pending))
            pending = match.group(1).strip()
        elif pending and raw.startswith(("  ", "\t")):
            pending += " " + raw.strip()
        elif pending:
            result.append(normalized_line(pending))
            pending = ""
    if pending:
        result.append(normalized_line(pending))
    return [item for item in result if item]


def first_nonempty(text: str, limit: int = 500) -> str:
    for line in text.splitlines():
        value = normalized_line(line)
        if value:
            return bounded_line(value, limit)
    return ""


def stable_criterion_id(source: str, text: str) -> str:
    explicit = re.search(r"\[(?:id|criterion):\s*([A-Za-z0-9._:-]{1,80})\]", text, re.I)
    if explicit:
        return explicit.group(1)
    normalized = re.sub(r"\s+", " ", text.strip().lower())
    return "criterion-" + sha256_text(source + "\0" + normalized)[:12]


def strip_criterion_marker(text: str) -> str:
    return re.sub(r"\s*\[(?:id|criterion):\s*[A-Za-z0-9._:-]{1,80}\]\s*", " ", text, flags=re.I).strip()


def parse_scope(lines: Iterable[str]) -> Dict[str, List[str]]:
    from .common import parse_path_list
    allowed: List[str] = []
    forbidden: List[str] = []
    for raw in lines:
        line = normalized_line(raw)
        allow = re.match(r"^(allowed(?:_paths)?|include|scope\.allow)\s*:\s*(.+)$", line, re.I)
        deny = re.match(r"^(forbidden(?:_paths)?|exclude|scope\.deny)\s*:\s*(.+)$", line, re.I)
        if allow:
            allowed.extend(parse_path_list(allow.group(2)))
        elif deny:
            forbidden.extend(parse_path_list(deny.group(2)))
    return {"allowed": sorted(set(allowed)), "forbidden": sorted(set(forbidden))}
