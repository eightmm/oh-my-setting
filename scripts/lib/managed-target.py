#!/usr/bin/env python3
"""Ownership-safe copy fallback for platforms without reliable symlinks."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Optional

SCHEMA = 1
MARKER_SUFFIX = ".oh-my-setting-managed.json"


def marker_path(target: Path) -> Path:
    return target.parent / f".{target.name}{MARKER_SUFFIX}"


def canonical(path: Path) -> str:
    return os.path.normcase(os.path.realpath(os.fspath(path)))


def path_hash(path: Path) -> tuple[str, str]:
    digest = hashlib.sha256()
    if path.is_symlink():
        raise ValueError(f"managed copies cannot contain a symlink root: {path}")
    if path.is_file():
        digest.update(b"file\0")
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return "file", digest.hexdigest()
    if not path.is_dir():
        # Bare FileNotFoundError(path) prints as just the path, which is what
        # the first Windows CI failure reported: one line naming a directory
        # that was plainly there, with no hint that the name carried a trailing
        # carriage return. Say what was being attempted.
        raise FileNotFoundError("not a file or directory: %s" % path)

    digest.update(b"directory\0")
    for base, dirs, files in os.walk(path, followlinks=False):
        dirs.sort()
        files.sort()
        base_path = Path(base)
        for name in dirs:
            entry = base_path / name
            if entry.is_symlink():
                raise ValueError(f"managed copies cannot contain symlinks: {entry}")
            rel = entry.relative_to(path).as_posix()
            digest.update(b"D\0" + rel.encode("utf-8") + b"\0")
        for name in files:
            entry = base_path / name
            if entry.is_symlink():
                raise ValueError(f"managed copies cannot contain symlinks: {entry}")
            rel = entry.relative_to(path).as_posix()
            digest.update(b"F\0" + rel.encode("utf-8") + b"\0")
            with entry.open("rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
            digest.update(b"\0")
    return "directory", digest.hexdigest()


def read_marker(target: Path) -> Optional[dict[str, object]]:
    try:
        row = json.loads(marker_path(target).read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return None
    if (
        row.get("schema") != SCHEMA
        or row.get("kind") not in {"file", "directory"}
        or not isinstance(row.get("source"), str)
        or not isinstance(row.get("sha256"), str)
    ):
        return None
    return row


def intact_copy(target: Path, row: dict[str, object]) -> bool:
    try:
        kind, digest = path_hash(target)
    except (FileNotFoundError, OSError, ValueError):
        return False
    return kind == row["kind"] and digest == row["sha256"]


def source_matches(row: dict[str, object], source: Path) -> bool:
    return canonical(Path(str(row["source"]))) == canonical(source)


def inspect_copy(source: Path, target: Path) -> str:
    row = read_marker(target)
    if not row or not source_matches(row, source):
        return "foreign"
    if not intact_copy(target, row):
        return "modified"
    try:
        source_kind, source_digest = path_hash(source)
    except (FileNotFoundError, OSError, ValueError):
        return "owned"
    if source_kind == row["kind"] and source_digest == row["sha256"]:
        return "current"
    return "owned"


def owned_source_under(root: Path, target: Path) -> Optional[str]:
    row = read_marker(target)
    if not row or not intact_copy(target, row):
        return None
    source = canonical(Path(str(row["source"])))
    try:
        if os.path.commonpath([canonical(root), source]) == canonical(root):
            return str(row["source"])
    except ValueError:
        pass
    return None


def write_marker(source: Path, target: Path, kind: str, digest: str) -> None:
    marker = marker_path(target)
    marker.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(
        prefix=f".{marker.name}.", suffix=".tmp", dir=marker.parent
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(
                {
                    "schema": SCHEMA,
                    "source": canonical(source),
                    "kind": kind,
                    "sha256": digest,
                },
                handle,
                ensure_ascii=False,
                sort_keys=True,
            )
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_name, marker)
    except Exception:
        try:
            os.unlink(temp_name)
        except FileNotFoundError:
            pass
        raise


def install_copy(source: Path, target: Path) -> None:
    if target.exists() or target.is_symlink():
        raise FileExistsError(f"managed copy target already exists: {target}")
    source_kind, source_digest = path_hash(source)
    target.parent.mkdir(parents=True, exist_ok=True)

    if source_kind == "directory":
        temp = Path(
            tempfile.mkdtemp(prefix=f".{target.name}.oms-copy.", dir=target.parent)
        )
        try:
            shutil.copytree(source, temp, dirs_exist_ok=True)
            os.replace(temp, target)
        except Exception:
            shutil.rmtree(temp, ignore_errors=True)
            raise
    else:
        fd, temp_name = tempfile.mkstemp(
            prefix=f".{target.name}.oms-copy.", dir=target.parent
        )
        os.close(fd)
        temp = Path(temp_name)
        try:
            shutil.copy2(source, temp)
            os.replace(temp, target)
        except Exception:
            try:
                temp.unlink()
            except FileNotFoundError:
                pass
            raise

    try:
        write_marker(source, target, source_kind, source_digest)
    except Exception:
        if target.is_dir():
            shutil.rmtree(target, ignore_errors=True)
        else:
            try:
                target.unlink()
            except FileNotFoundError:
                pass
        raise


def remove_marker(target: Path) -> None:
    try:
        marker_path(target).unlink()
    except FileNotFoundError:
        pass


def usage() -> None:
    print(
        "usage: managed-target.py "
        "{inspect|source-under|copy|remove-marker} ...",
        file=sys.stderr,
    )


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        usage()
        return 2
    command = argv[1]
    try:
        if command == "inspect" and len(argv) == 4:
            print(inspect_copy(Path(argv[2]), Path(argv[3])))
            return 0
        if command == "source-under" and len(argv) == 4:
            source = owned_source_under(Path(argv[2]), Path(argv[3]))
            if source is None:
                return 1
            print(source)
            return 0
        if command == "copy" and len(argv) == 4:
            install_copy(Path(argv[2]), Path(argv[3]))
            return 0
        if command == "remove-marker" and len(argv) == 3:
            remove_marker(Path(argv[2]))
            return 0
    except (FileNotFoundError, OSError, ValueError) as exc:
        # Name the operation. Without it a failure is one unlabelled line and
        # the reader cannot tell an inspect from a copy, or a bad source from a
        # bad target.
        print(f"error: {command}: {exc}", file=sys.stderr)
        return 1
    usage()
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
