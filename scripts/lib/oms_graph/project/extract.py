"""File discovery, safety filters, per-file extraction (W2)."""

from __future__ import annotations

import fnmatch
import os
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from oms_runtime.common import run_output, sha256_bytes

from .model import PATH_LITERAL_RE, make_node, node_id
from .parsers import parser_for

DEFAULT_EXCLUDES = (".git/", ".oms/", "node_modules/", "vendor/", "dist/", "build/", "target/", "__pycache__/")
SECRET_NAME_GLOBS = (".env", ".env.*", "*.pem", "*.key", "id_rsa*", "*.p12", "*.pfx")
TEST_DIRECTORIES = {"test", "tests", "testing", "__tests__", "e2e", "integration-tests", "integration_tests"}
TEST_NAME_GLOBS = ("test_*.py", "*_test.py", "*_spec.py", "test-*.sh", "*-test.sh", "*_test.sh", "*-smoke.sh")


def _excluded_path(path: str) -> bool:
    return any(path.startswith(prefix) or ("/" + prefix) in path for prefix in DEFAULT_EXCLUDES)


def is_test_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    parts = normalized.split("/")
    return (any(part.lower() in TEST_DIRECTORIES for part in parts[:-1])
            or any(fnmatch.fnmatch(parts[-1], pattern) for pattern in TEST_NAME_GLOBS))


def discover_files(repo: Path, *, include: Sequence[str] = (), exclude: Sequence[str] = (), max_bytes: int = 2 * 1024 * 1024) -> Dict[str, Any]:
    """{"files": [relpath...], "skipped": [{"path","reason"}]} — no symlinks, no binaries, no secrets."""
    repo = Path(repo).resolve()
    candidates: List[str] = []
    listed = run_output(["git", "-C", str(repo), "ls-files", "--cached", "--others", "--exclude-standard", "-z"])
    if listed:
        candidates = [item.replace("\\", "/") for item in listed.split("\0") if item]
    else:
        for root, dirs, names in os.walk(str(repo), followlinks=False):
            root_path = Path(root)
            dirs[:] = [name for name in dirs if not _excluded_path((root_path / name).relative_to(repo).as_posix() + "/")]
            for name in names:
                candidates.append((root_path / name).relative_to(repo).as_posix())
    files: List[str] = []
    skipped: List[Dict[str, str]] = []
    for relpath in sorted(set(candidates)):
        relpath = relpath.replace("\\", "/")
        path = repo / relpath
        reason = ""
        if _excluded_path(relpath):
            reason = "excluded"
        elif any(fnmatch.fnmatch(relpath, pattern) or fnmatch.fnmatch(Path(relpath).name, pattern) for pattern in SECRET_NAME_GLOBS):
            reason = "secret-name"
        elif exclude and any(fnmatch.fnmatch(relpath, pattern) for pattern in exclude):
            reason = "excluded"
        elif include and not any(fnmatch.fnmatch(relpath, pattern) for pattern in include):
            reason = "excluded"
        elif path.is_symlink():
            reason = "symlink"
        elif not path.is_file():
            reason = "excluded"
        else:
            try:
                size = path.stat().st_size
                with path.open("rb") as handle:
                    head = handle.read(8192)
            except OSError:
                reason = "excluded"
            else:
                if size > max_bytes:
                    reason = "too-large"
                elif b"\0" in head:
                    reason = "binary"
                elif parser_for(relpath) is None:
                    reason = "unparsed"
        if reason:
            skipped.append({"path": relpath, "reason": reason})
        else:
            files.append(relpath)
    return {"files": files, "skipped": sorted(skipped, key=lambda item: (item["path"], item["reason"]))}


def extract_file(repo: Path, relpath: str) -> Dict[str, Any]:
    """{"path","sha256","bytes","parser","parser_version","nodes","edges","refs"} for one file."""
    path = Path(repo) / relpath
    data = path.read_bytes()
    digest = sha256_bytes(data)
    parser = parser_for(relpath)
    if parser is None:
        raise ValueError("no parser for %s" % relpath)
    text = data.decode("utf-8", "replace")
    parsed = parser.parse(relpath, text, digest).as_dict()
    is_test = is_test_path(relpath)
    file_kind = "test" if is_test else "file"
    file_node = make_node(file_kind, Path(relpath).name, relpath, parser.language, digest)
    if is_test:
        regular = node_id("file", relpath)
        test_id = node_id("test", relpath)
        for edge in parsed["edges"]:
            if edge.get("source") == regular:
                edge["source"] = test_id
        for ref in parsed["refs"]:
            if ref.get("from") == regular:
                ref["from"] = test_id
    parsed["nodes"] = [file_node] + parsed["nodes"]
    # Tests name implementation paths frequently without a language-specific
    # parser reference; preserve those direct assertions as test edges.
    if is_test:
        source = node_id(file_kind, relpath)
        for number, line in enumerate(text.splitlines(), 1):
            for match in PATH_LITERAL_RE.finditer(line):
                parsed["refs"].append({"from": source, "relation": "tests", "kind": "path", "value": match.group(1).rstrip(".,:;`'\""), "line": number})
    return {"path": relpath, "sha256": digest, "bytes": len(data), "parser": parser.language,
            "parser_version": parser.version, "nodes": parsed["nodes"], "edges": parsed["edges"], "refs": parsed["refs"]}
