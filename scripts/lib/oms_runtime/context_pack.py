"""Typed validation of a Project Graph context pack before it crosses a boundary.

The pack is orientation metadata a parent hands to a delegated worker: it says
where to look, never what may be written. Nothing here widens a task's
allowed_paths, writes OMS state, or reads the named source files.
"""

from __future__ import annotations

import json
import os
import stat
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

try:
    from .common import CoreError, sensitive_text, sha256_bytes
except ImportError:  # direct execution: python3 .../oms_runtime/context_pack.py
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from oms_runtime.common import CoreError, sensitive_text, sha256_bytes

MAX_PACK_BYTES = 262144
MAX_ENTRIES = 200
MAX_PATH_CHARS = 400
HEX = frozenset("0123456789abcdef")


def _check_path(raw: Any, label: str) -> str:
    if not isinstance(raw, str):
        raise CoreError("%s entries must be strings" % label)
    if not raw:
        raise CoreError("%s contains an empty path" % label)
    if len(raw) > MAX_PATH_CHARS:
        raise CoreError("%s path exceeds %d characters" % (label, MAX_PATH_CHARS))
    if raw.startswith("/") or (len(raw) > 1 and raw[1] == ":"):
        raise CoreError("%s path is absolute: %s" % (label, raw))
    if "\\" in raw:
        raise CoreError("%s path contains a backslash: %s" % (label, raw))
    if "\0" in raw or "\n" in raw or "\r" in raw:
        raise CoreError("%s path contains a control character" % label)
    if raw.startswith("./"):
        raise CoreError("%s path is not normalized: %s" % (label, raw))
    if ".." in raw.split("/"):
        raise CoreError("%s path escapes the repository: %s" % (label, raw))
    first = raw.split("/", 1)[0]
    if first in (".git", ".oms"):
        raise CoreError("%s path names private state: %s" % (label, raw))
    return raw


def _check_str_list(value: Any, label: str, limit: int) -> List[str]:
    if not isinstance(value, list):
        raise CoreError("%s must be a list" % label)
    if len(value) > limit:
        raise CoreError("%s has %d entries, over the %d cap" % (label, len(value), limit))
    return [_check_path(item, label) for item in value]


def _check_evidence(value: Any) -> List[Dict[str, str]]:
    if not isinstance(value, list):
        raise CoreError("evidence must be a list")
    if len(value) > MAX_ENTRIES:
        raise CoreError("evidence has %d entries, over the %d cap" % (len(value), MAX_ENTRIES))
    rows = []  # type: List[Dict[str, str]]
    for item in value:
        if not isinstance(item, dict):
            raise CoreError("evidence entries must be objects")
        path = _check_path(item.get("path"), "evidence")
        reason = item.get("reason", "")
        if not isinstance(reason, str):
            raise CoreError("evidence reason must be a string")
        if "\n" in reason or "\r" in reason or "\0" in reason:
            raise CoreError("evidence reason contains a control character")
        rows.append({"path": path, "reason": reason[:200]})
    return rows


def _check_test_cases(value: Any) -> List[Dict[str, str]]:
    if not isinstance(value, list):
        raise CoreError("test_cases must be a list")
    if len(value) > MAX_ENTRIES:
        raise CoreError("test_cases has %d entries, over the %d cap" % (len(value), MAX_ENTRIES))
    rows = []
    for item in value:
        if not isinstance(item, dict):
            raise CoreError("test_cases entries must be objects")
        row = {"path": _check_path(item.get("path"), "test_cases")}
        for field in ("id", "language", "name"):
            raw = item.get(field)
            if not isinstance(raw, str):
                raise CoreError("test_cases %s must be a string" % field)
            if len(raw) > MAX_PATH_CHARS or any(char in raw for char in "\0\r\n"):
                raise CoreError("test_cases %s is not a bounded line" % field)
            row[field] = raw
        rows.append(row)
    return rows


def _read_pack_bytes(path: Path, max_bytes: int) -> Tuple[bytes, str]:
    try:
        info = os.lstat(str(path))
    except OSError as exc:
        raise CoreError("cannot stat pack %s: %s" % (path.name, exc.strerror or exc))
    if not stat.S_ISREG(info.st_mode):
        raise CoreError("pack must be a regular non-symlink file")
    if info.st_size > max_bytes:
        raise CoreError("pack is %d bytes, over the %d byte cap" % (info.st_size, max_bytes))
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise CoreError("cannot read pack: %s" % (exc.strerror or exc))
    # lstat and the read are two moments; the cap is on what is actually held.
    if len(raw) > max_bytes:
        raise CoreError("pack is %d bytes, over the %d byte cap" % (len(raw), max_bytes))
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise CoreError("pack is not UTF-8: %s" % exc)
    return raw, text


def validate_context_pack(path: Path, repo: Path, *, max_bytes: int = MAX_PACK_BYTES) -> Dict[str, Any]:
    """Return a bounded summary of PATH, or raise CoreError with a one-line reason.

    REPO is the boundary the pack's relative paths are read against; the pack
    itself is never required to live inside it and no named source file is read.
    """
    path = Path(path)
    repo = Path(repo)
    raw, text = _read_pack_bytes(path, max_bytes)
    # Before any structural trust: a pack carrying secret-shaped or
    # absolute-path content must not reach a provider prompt at all.
    if sensitive_text(text):
        raise CoreError("pack contains secret-shaped or absolute-path content")
    try:
        pack = json.loads(text)
    except ValueError as exc:
        raise CoreError("pack is not valid JSON: %s" % exc)
    if not isinstance(pack, dict):
        raise CoreError("pack must be a JSON object")

    digest = pack.get("pack_digest")
    if not isinstance(digest, str) or len(digest) != 64 or not set(digest) <= HEX:
        raise CoreError("pack_digest must be 64 lowercase hex characters")

    files = _check_str_list(pack.get("files", []), "files", MAX_ENTRIES)
    tests = _check_str_list(pack.get("tests", []), "tests", MAX_ENTRIES)
    test_cases = _check_test_cases(pack.get("test_cases", []))
    evidence = _check_evidence(pack["evidence"]) if "evidence" in pack else []
    if "hubs" in pack and not isinstance(pack["hubs"], list):
        raise CoreError("hubs must be a list")
    revision = pack.get("project_graph_revision", "")
    if not isinstance(revision, str):
        raise CoreError("project_graph_revision must be a string")
    if "\n" in revision or "\r" in revision or len(revision) > MAX_PATH_CHARS:
        raise CoreError("project_graph_revision is not a single bounded line")

    return {
        "schema": 1,
        "path": str(path),
        "pack_digest": digest,
        "project_graph_revision": revision,
        "files": files,
        "tests": tests,
        "test_cases": test_cases,
        "evidence": evidence,
        "file_count": len(files),
        "sha256": sha256_bytes(raw),
    }


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    repo = ""
    target = ""
    while args:
        item = args.pop(0)
        if item == "--repo":
            if not args:
                print("--repo requires a path", file=sys.stderr)
                return 2
            repo = args.pop(0)
        elif item.startswith("-"):
            print("unknown argument: %s" % item, file=sys.stderr)
            return 2
        elif target:
            print("exactly one pack path is accepted", file=sys.stderr)
            return 2
        else:
            target = item
    if not target or not repo:
        print("usage: context_pack.py --repo REPO PATH", file=sys.stderr)
        return 2
    try:
        summary = validate_context_pack(Path(target), Path(repo))
    except CoreError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    json.dump(summary, sys.stdout, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
