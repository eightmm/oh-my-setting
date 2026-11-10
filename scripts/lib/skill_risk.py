#!/usr/bin/env python3
"""Where a person should read before importing a skill bundle.

A static, bounded pass over the bundle's own files that lists review points by
kind with their locations. It decides nothing: it never calls a bundle safe,
never changes whether import is refused, runs no bundle file and calls no
model. Only paths, line numbers, host names and counts are reported; the text
of a flagged line is never copied into the report.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict, List, Sequence, Tuple

MAX_LOCATIONS = 20
MAX_TEXT_BYTES = 262144
SCRIPT_SUFFIXES = (".sh", ".bash", ".zsh", ".py", ".js", ".mjs", ".cjs", ".ts", ".rb", ".pl", ".ps1", ".bat", ".cmd")
TEXT_SUFFIXES = (".md", ".mdx", ".txt", ".yaml", ".yml", ".json", ".toml")

# Sentences that tell an agent to fetch or run something.
INSTRUCTION_RE = re.compile(
    r"(?:\b(?:curl|wget)\s+[^\s|]|\|\s*(?:sudo\s+)?(?:ba|z|da)?sh\b|\b(?:ba)?sh\s+-c\b|\beval\s|\bsudo\s"
    r"|\bchmod\s+\+x\b|\b(?:pip3?|uv|npm|pnpm|yarn|gem|cargo|brew|apt(?:-get)?)\s+(?:install|add)\b"
    r"|\bnpx\s+\S|\bgit\s+clone\b|\bInvoke-WebRequest\b|\biex\b)",
    re.IGNORECASE,
)
URL_RE = re.compile(r"https?://(?:[^\s/@<>\"'`)\]]*@)?([A-Za-z0-9.-]+)", re.IGNORECASE)
COMMENT_RE = re.compile(r"<!--")


def _hidden(line: str, first_line: bool) -> bool:
    for index, char in enumerate(line):
        code = ord(char)
        if code == 0xFEFF and first_line and index == 0:
            continue  # a byte-order mark at the start of a file is not hidden text
        if (0x200B <= code <= 0x200F or 0x202A <= code <= 0x202E or 0x2060 <= code <= 0x2064
                or 0x2066 <= code <= 0x2069 or code == 0xFEFF or 0xE0000 <= code <= 0xE007F):
            return True
    return False


def _item(locations: List[str], count: int) -> Dict[str, Any]:
    row: Dict[str, Any] = {"count": count, "where": locations[:MAX_LOCATIONS]}
    if count > len(row["where"]):
        row["omitted"] = count - len(row["where"])
    return row


def summarize(entries: Sequence[Tuple[str, Path, int, int]]) -> Dict[str, Any]:
    scripts: List[str] = []
    instructions: List[str] = []
    hidden: List[str] = []
    comments: List[str] = []
    hosts: Dict[str, int] = {}
    urls = 0
    allowed_tools = ""
    unread: List[str] = []
    for relative, path, size, executable in entries:
        lowered = relative.lower()
        try:
            raw = path.read_bytes() if size <= MAX_TEXT_BYTES else b""
        except OSError:
            raw = b""
            unread.append(relative)
        if executable or lowered.endswith(SCRIPT_SUFFIXES) or raw.startswith(b"#!"):
            scripts.append(relative)
        if not lowered.endswith(TEXT_SUFFIXES) and not raw.startswith(b"#!") and not lowered.endswith(SCRIPT_SUFFIXES):
            continue
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            unread.append(relative)
            continue
        prose = lowered.endswith((".md", ".mdx", ".txt"))
        in_frontmatter = False
        for number, line in enumerate(text.splitlines(), 1):
            where = "%s:%d" % (relative, number)
            if relative == "SKILL.md":
                if number == 1 and line.strip() == "---":
                    in_frontmatter = True
                elif in_frontmatter and line.strip() == "---":
                    in_frontmatter = False
                elif in_frontmatter and not allowed_tools:
                    match = re.match(r"^allowed-tools:\s*(.*)$", line)
                    if match:
                        allowed_tools = re.sub(r"[^A-Za-z0-9_ ,:()*./\[\]\"'-]", "?", match.group(1).strip())[:200] or "(listed below the key)"
            if _hidden(line, number == 1):
                hidden.append(where)
            if prose and COMMENT_RE.search(line):
                comments.append(where)
            if prose and INSTRUCTION_RE.search(line):
                instructions.append(where)
            for match in URL_RE.finditer(line):
                urls += 1
                host = match.group(1).lower().rstrip(".")
                if host:
                    hosts[host] = hosts.get(host, 0) + 1
    host_names = sorted(hosts)
    summary: Dict[str, Any] = {
        "schema": 1,
        "note": "review points, not a verdict: an empty list does not mean the bundle is safe, and import rules are unchanged",
        "scripts": _item(scripts, len(scripts)),
        "fetch_or_run_instructions": _item(instructions, len(instructions)),
        "hidden_unicode": _item(hidden, len(hidden)),
        "html_comments": _item(comments, len(comments)),
        "external_urls": {"count": urls, "hosts": host_names[:MAX_LOCATIONS]},
        "allowed_tools": allowed_tools,
    }
    if len(host_names) > MAX_LOCATIONS:
        summary["external_urls"]["omitted"] = len(host_names) - MAX_LOCATIONS
    if unread:
        summary["unread"] = _item(unread, len(unread))
    return summary
