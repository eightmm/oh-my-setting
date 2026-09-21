#!/usr/bin/env python3
"""Add or remove oh-my-setting's reversible Codex usage-efficiency defaults.

Codex validates these keys when it loads config.toml, and a rejected file
stops every session. So only keys whose effect was evidenced are managed, a
value the user set is never overridden, and anything uncertain is left alone.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import re
import sys
from typing import Dict, List, Optional, Tuple


def _sibling(name: str, filename: str):
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location(name, os.path.join(here, filename))
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# The HUD helper owns the config writer (first-touch backup, mode-preserving
# atomic replace); a second copy would let the two drift.
HUD = _sibling("codex_hud_config", "codex-hud-config.py")
ConfigError = HUD.ConfigError

TERMINAL_CEILING_MS = 900000
V2_MIN_WAIT_MS = 120000
V2_DEFAULT_WAIT_MS = 300000


class Block:
    def __init__(self, name: str, entries: List[Tuple[str, int]]):
        self.begin = "# >>> oh-my-setting managed Codex usage (%s) >>>" % name
        self.end = "# <<< oh-my-setting managed Codex usage (%s) <<<" % name
        self.entries = entries
        self.lines = ["%s = %d" % entry for entry in entries]


# Raises the ceiling of an empty background-terminal poll (Codex default
# 300000). It permits a long wait; it does not make a short request long.
ROOT = Block("root", [("background_terminal_max_timeout", TERMINAL_CEILING_MS)])
# Codex clamps a shorter requested wait up to a minimum, and rejects the
# whole file unless min <= default <= max, hence both keys or neither.
V2 = Block(
    "multi_agent_v2",
    [
        ("min_wait_timeout_ms", V2_MIN_WAIT_MS),
        ("default_wait_timeout_ms", V2_DEFAULT_WAIT_MS),
    ],
)

_KEY = r"(?:%s|\"%s\"|'%s')"
V2_TABLE_RE = re.compile(
    r"^\s*\[\s*%s\s*\.\s*%s\s*\]\s*(?:#.*)?$"
    % (_KEY % (("features",) * 3), _KEY % (("multi_agent_v2",) * 3))
)

ROOT_NOTES = {
    "managed": "already current",
    "customized": "preserved customized value",
    "user": "preserved user value",
}
V2_NOTES = {
    "managed": "already current",
    "customized": "preserved customized values",
    "user": "preserved user values",
    "disabled": "skipped (multi_agent_v2 is not enabled in this config)",
    "boolean": "preserved (enabled as a boolean; a table would conflict with it)",
    "conflict": "preserved (max_wait_timeout_ms is below the managed default)",
    "shape": "preserved (enabled in a form this helper does not edit)",
}


def toml_parser():
    try:
        import tomllib  # type: ignore

        return tomllib
    except ImportError:
        try:
            import tomli  # type: ignore

            return tomli
        except ImportError:
            return None


def read(path: str) -> Tuple[bytes, str, List[str]]:
    if not os.path.isfile(path):
        return b"", "\n", []
    with open(path, "rb") as fh:
        raw = fh.read(HUD.MAX_CONFIG_BYTES + 1)
    if len(raw) > HUD.MAX_CONFIG_BYTES:
        raise ConfigError("config exceeds %d bytes" % HUD.MAX_CONFIG_BYTES)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ConfigError("config is not UTF-8 (%s)" % exc)
    return raw, HUD.newline_for(raw), text.splitlines(keepends=True)


def parse(parser, lines: List[str]) -> Dict:
    try:
        return parser.loads("".join(lines))
    except Exception as exc:
        raise ConfigError("config is not valid TOML (%s)" % exc)


def marker_block(lines: List[str], block: Block) -> Optional[Tuple[int, int, bool]]:
    begins = [i for i, line in enumerate(lines) if HUD.body(line) == block.begin]
    ends = [i for i, line in enumerate(lines) if HUD.body(line) == block.end]
    if not begins and not ends:
        return None
    if len(begins) != 1 or len(ends) != 1 or ends[0] <= begins[0]:
        raise ConfigError("managed Codex usage markers are incomplete or duplicated")
    start, finish = begins[0], ends[0]
    managed = [HUD.body(line) for line in lines[start + 1 : finish]] == block.lines
    return start, finish, managed


def v2_table(parsed: Dict):
    features = parsed.get("features")
    return features.get("multi_agent_v2") if isinstance(features, dict) else None


def v2_header(lines: List[str]) -> Optional[int]:
    for index, line in enumerate(lines):
        if V2_TABLE_RE.match(HUD.body(line)):
            return index
    return None


def root_state(lines: List[str], parsed: Dict) -> str:
    marked = marker_block(lines, ROOT)
    if marked is not None:
        return "managed" if marked[2] else "customized"
    return "user" if ROOT.entries[0][0] in parsed else "missing"


def v2_state(lines: List[str], parsed: Dict) -> str:
    marked = marker_block(lines, V2)
    if marked is not None:
        return "managed" if marked[2] else "customized"
    table = v2_table(parsed)
    if table is True:
        return "boolean"
    if not (isinstance(table, dict) and table.get("enabled") is True):
        return "disabled"
    if any(key in table for key, _ in V2.entries):
        return "user"
    ceiling = table.get("max_wait_timeout_ms")
    if ceiling is not None and (
        isinstance(ceiling, bool)
        or not isinstance(ceiling, int)
        or ceiling < V2_DEFAULT_WAIT_MS
    ):
        return "conflict"
    return "missing" if v2_header(lines) is not None else "shape"


def fenced(block: Block, newline: str) -> List[str]:
    return [text + newline for text in [block.begin] + block.lines + [block.end]]


def install(
    parser, lines: List[str], newline: str, dry_run: bool
) -> Tuple[List[str], List[str], bool]:
    parsed = parse(parser, lines)
    root, v2 = root_state(lines, parsed), v2_state(lines, parsed)
    updated = list(lines)

    # Lower index last, so the header index stays valid.
    if v2 == "missing":
        header = v2_header(updated)
        assert header is not None
        if not updated[header].endswith(("\n", "\r")):
            updated[header] += newline
        updated[header + 1 : header + 1] = fenced(V2, newline)
    if root == "missing":
        # Root keys are only valid before the first table; the top of the
        # file is that place without having to find where tables begin.
        updated[0:0] = fenced(ROOT, newline) + ([newline] if updated else [])

    if root == "missing" or v2 == "missing":
        after = parse(parser, updated)
        table = v2_table(after)
        landed = root != "missing" or all(after.get(k) == v for k, v in ROOT.entries)
        if v2 == "missing":
            landed = landed and isinstance(table, dict) and all(
                table.get(k) == v for k, v in V2.entries
            )
        if not landed:
            raise ConfigError("managed Codex usage keys did not land where intended")

    verb = "would install" if dry_run else "installed"
    notes = [
        "terminal ceiling %s"
        % ("%s (%s)" % (verb, ROOT.lines[0]) if root == "missing" else ROOT_NOTES[root]),
        "multi_agent_v2 waits %s"
        % ("%s (%s)" % (verb, ", ".join(V2.lines)) if v2 == "missing" else V2_NOTES[v2]),
    ]
    return updated, notes, root == "missing" or v2 == "missing"


def remove(parser, lines: List[str], dry_run: bool) -> Tuple[List[str], List[str], bool]:
    updated = list(lines)
    verb = "would remove" if dry_run else "removed"
    notes = {}
    spans = []
    for block, label in ((ROOT, "terminal ceiling"), (V2, "multi_agent_v2 waits")):
        marked = marker_block(lines, block)
        if marked is None:
            notes[label] = "already absent"
        elif not marked[2]:
            notes[label] = "preserved customized value"
        else:
            notes[label] = verb
            spans.append((marked[0], marked[1]))
    # Higher index first, so deleting one block cannot shift the other.
    for start, finish in sorted(spans, reverse=True):
        # install separates the top block from the user's first line.
        if start == 0 and finish + 1 < len(updated) and not HUD.body(updated[finish + 1]).strip():
            finish += 1
        del updated[start : finish + 1]
    # Exactly the lines this helper wrote are deleted, so what remains is the
    # text that was valid before; a parser only confirms it when available.
    if spans and parser is not None:
        parse(parser, updated)
    return updated, ["%s %s" % item for item in notes.items()], bool(spans)


def check(parser, lines: List[str]) -> Tuple[str, int]:
    if parser is None:
        return "unverified (needs Python 3.11+ or tomli to read the config)", 1
    parsed = parse(parser, lines)
    root, v2 = root_state(lines, parsed), v2_state(lines, parsed)
    shown = root
    if root == "user":
        shown = "user(%s)" % parsed.get(ROOT.entries[0][0])
    return (
        "terminal-ceiling=%s v2-waits=%s scope=config-file effective=unverified"
        % (shown, v2),
        0 if root != "missing" else 1,
    )


def main() -> int:
    arguments = argparse.ArgumentParser()
    arguments.add_argument("action", choices=("install", "remove", "check"))
    arguments.add_argument("path")
    arguments.add_argument("--dry-run", action="store_true")
    args = arguments.parse_args()
    path = os.path.abspath(args.path)

    try:
        parser = toml_parser()
        original, newline, lines = read(path)
        if args.action == "check":
            state, code = check(parser, lines)
            print("codex-usage: %s" % state)
            return code
        if args.action == "install":
            if parser is None:
                # Whether the user already set a key cannot be known without
                # reading the TOML, and guessing risks overriding them.
                print("codex-usage: skipped (needs Python 3.11+ or tomli to read the config)")
                return 0
            updated, notes, changed = install(parser, lines, newline, args.dry_run)
        else:
            updated, notes, changed = remove(parser, lines, args.dry_run)
        if changed and not args.dry_run:
            HUD.write(path, original, updated)
        for note in notes:
            print("codex-usage: %s" % note)
        return 0
    except ConfigError as exc:
        sys.stderr.write("error: %s: %s\n" % (args.path, exc))
        return 2
    except OSError as exc:
        sys.stderr.write("error: could not update %s (%s)\n" % (args.path, exc))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
