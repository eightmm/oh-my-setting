#!/usr/bin/env python3
"""Add or remove oh-my-setting's reversible Codex TUI status-line default."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import stat
import sys
import tempfile
from typing import List, Optional, Tuple


BEGIN = "# >>> oh-my-setting managed Codex HUD >>>"
END = "# <<< oh-my-setting managed Codex HUD <<<"
STATUS_VALUE = (
    '["model-with-reasoning", "context-remaining", "five-hour-limit", '
    '"weekly-limit", "git-branch"]'
)
STATUS_LINE = "status_line = %s" % STATUS_VALUE
MAX_CONFIG_BYTES = 8 * 1024 * 1024

TUI_TABLE_RE = re.compile(r"^\s*\[\s*(?:tui|[\"']tui[\"'])\s*\]\s*(?:#.*)?$")
ANY_TABLE_RE = re.compile(r"^\s*\[\[?.*\]\]?\s*(?:#.*)?$")
STATUS_KEY_RE = re.compile(r"^\s*(?:status_line|[\"']status_line[\"'])\s*=")
DOTTED_STATUS_RE = re.compile(
    r"^\s*(?:tui|[\"']tui[\"'])\s*\.\s*"
    r"(?:status_line|[\"']status_line[\"'])\s*="
)


class ConfigError(Exception):
    pass


def body(line: str) -> str:
    return line.rstrip("\r\n")


def newline_for(raw: bytes) -> str:
    return "\r\n" if b"\r\n" in raw else "\n"


def load(path: str) -> Tuple[bytes, str, List[str]]:
    if not os.path.isfile(path):
        return b"", "\n", []
    with open(path, "rb") as fh:
        raw = fh.read(MAX_CONFIG_BYTES + 1)
    if len(raw) > MAX_CONFIG_BYTES:
        raise ConfigError("config exceeds %d bytes" % MAX_CONFIG_BYTES)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ConfigError("config is not UTF-8 (%s)" % exc)
    validate_toml(text)
    return raw, newline_for(raw), text.splitlines(keepends=True)


def validate_toml(text: str) -> None:
    try:
        import tomllib  # type: ignore
    except ImportError:
        try:
            import tomli as tomllib  # type: ignore
        except ImportError:
            return
    try:
        tomllib.loads(text)
    except Exception as exc:
        raise ConfigError("config is not valid TOML (%s)" % exc)


def marker_block(lines: List[str]) -> Optional[Tuple[int, int, bool]]:
    begins = [index for index, line in enumerate(lines) if body(line) == BEGIN]
    ends = [index for index, line in enumerate(lines) if body(line) == END]
    if not begins and not ends:
        return None
    if len(begins) != 1 or len(ends) != 1 or ends[0] <= begins[0]:
        raise ConfigError("managed Codex HUD markers are incomplete or duplicated")
    start, finish = begins[0], ends[0]
    managed = [body(line) for line in lines[start + 1 : finish]] == [STATUS_LINE]
    return start, finish, managed


def status_key(lines: List[str]) -> Tuple[bool, Optional[int]]:
    in_tui = False
    in_root = True
    tui_header = None
    for index, line in enumerate(lines):
        stripped = body(line)
        if not stripped.strip() or stripped.lstrip().startswith("#"):
            continue
        if TUI_TABLE_RE.match(stripped):
            in_tui = True
            in_root = False
            if tui_header is None:
                tui_header = index
            continue
        if ANY_TABLE_RE.match(stripped):
            in_tui = False
            in_root = False
            continue
        if (in_tui and STATUS_KEY_RE.match(stripped)) or (
            in_root and DOTTED_STATUS_RE.match(stripped)
        ):
            return True, tui_header
    return False, tui_header


def install(lines: List[str], newline: str) -> Tuple[List[str], str, bool]:
    marked = marker_block(lines)
    if marked is not None:
        if marked[2]:
            return lines, "already current", False
        return lines, "preserved customized status line", False

    exists, tui_header = status_key(lines)
    if exists:
        return lines, "preserved user status line", False

    block = [BEGIN + newline, STATUS_LINE + newline, END + newline]
    updated = list(lines)
    if tui_header is None:
        if updated and not updated[-1].endswith(("\n", "\r")):
            updated[-1] += newline
        if updated and body(updated[-1]).strip():
            updated.append(newline)
        updated.extend(["[tui]" + newline] + block)
    else:
        if not updated[tui_header].endswith(("\n", "\r")):
            updated[tui_header] += newline
        updated[tui_header + 1 : tui_header + 1] = block
    validate_toml("".join(updated))
    return updated, "installed", True


def remove(lines: List[str]) -> Tuple[List[str], str, bool]:
    marked = marker_block(lines)
    if marked is None:
        return lines, "already absent", False
    start, finish, managed = marked
    if not managed:
        return lines, "preserved customized status line", False
    updated = list(lines)
    del updated[start : finish + 1]
    validate_toml("".join(updated))
    return updated, "removed", True


def write(path: str, original: bytes, lines: List[str]) -> None:
    parent = os.path.dirname(path) or "."
    os.makedirs(parent, exist_ok=True)
    backup = path + ".oms-bak"
    if original and not os.path.exists(backup):
        shutil.copyfile(path, backup)

    mode = 0o600
    try:
        mode = stat.S_IMODE(os.stat(path).st_mode)
    except OSError:
        pass
    fd, temporary = tempfile.mkstemp(prefix=".config.toml.", dir=parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.write("".join(lines))
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    except Exception:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def check(lines: List[str]) -> Tuple[str, int]:
    marked = marker_block(lines)
    if marked is not None and marked[2]:
        return "managed", 0
    exists, _ = status_key(lines)
    return ("user", 0) if exists else ("missing", 1)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("install", "remove", "check"))
    parser.add_argument("path")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    try:
        original, newline, lines = load(os.path.abspath(args.path))
        if args.action == "check":
            state, code = check(lines)
            print("codex-hud: %s" % state)
            return code
        if args.action == "install":
            updated, action, changed = install(lines, newline)
        else:
            updated, action, changed = remove(lines)
        if changed and not args.dry_run:
            write(os.path.abspath(args.path), original, updated)
        if changed and args.dry_run:
            action = "would %s" % ("install" if args.action == "install" else "remove")
        print("codex-hud: %s" % action)
        return 0
    except ConfigError as exc:
        sys.stderr.write("error: %s: %s\n" % (args.path, exc))
        return 2
    except OSError as exc:
        sys.stderr.write("error: could not update %s (%s)\n" % (args.path, exc))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
