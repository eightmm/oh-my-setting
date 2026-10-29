"""Local SSH target metadata; registration grants no execution authority."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, Dict

from .common import (CoreError, MAX_STATE_BYTES, SECRET_VALUE_RE,
                     atomic_write_bytes, file_lock, pretty_json, read_json)


_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")


def _name(value: Any, label: str) -> str:
    if not isinstance(value, str) or not _NAME.fullmatch(value):
        raise CoreError("%s must be 1..64 letters, digits, dots, underscores or hyphens, starting with a letter or digit" % label)
    return value


def _record(value: Any) -> Dict[str, str]:
    if not isinstance(value, dict) or set(value) != {"ssh_alias", "workdir"}:
        raise CoreError("remote target must contain only ssh_alias and workdir")
    _name(value["ssh_alias"], "SSH alias")
    workdir = value["workdir"]
    if (not isinstance(workdir, str) or not workdir.startswith("/") or
            any(ord(char) < 32 or ord(char) == 127 for char in workdir)):
        raise CoreError("remote workdir must be an absolute POSIX path without control characters")
    if SECRET_VALUE_RE.search(workdir):
        raise CoreError("remote workdir contains sensitive-looking material")
    try:
        workdir.encode("utf-8")
    except UnicodeEncodeError:
        raise CoreError("remote workdir must be UTF-8")
    return value


def _path(repo: Path) -> Path:
    parent = repo
    for part in (".oms", "runtime"):
        parent = parent / part
        if parent.is_symlink() or (parent.exists() and not parent.is_dir()):
            raise CoreError("remote target state requires real directories")
    return parent / "targets.json"


def listing(repo: Path) -> Dict[str, Any]:
    """Read without creating state, including for harness children."""
    value = read_json(_path(repo), default={"schema": 1, "targets": {}}, limit=MAX_STATE_BYTES)
    if (not isinstance(value, dict) or set(value) != {"schema", "targets"} or
            type(value["schema"]) is not int or value["schema"] != 1 or
            not isinstance(value["targets"], dict)):
        raise CoreError("invalid remote target registry")
    for name, record in value["targets"].items():
        _name(name, "target name")
        _record(record)
    return value


def _update(repo: Path, name: str, record: Any) -> Dict[str, Any]:
    path = _path(repo)
    try:
        # Serialize the entire read/modify/replace; readers see the old or new
        # registry, and a failed write never truncates the existing one.
        with file_lock(path):
            value = listing(repo)
            entries = value["targets"]
            if record is None:
                if name not in entries:
                    raise CoreError("unknown remote target: %s" % name)
                del entries[name]
            else:
                if name in entries:
                    raise CoreError("remote target already exists: %s" % name)
                entries[name] = record
            encoded = pretty_json(value)
            if len(encoded) > MAX_STATE_BYTES:
                raise CoreError("remote target registry exceeds the state size limit")
            atomic_write_bytes(path, encoded)
    except OSError as exc:
        raise CoreError("cannot update remote target registry: %s" % exc)
    return value


def add(repo: Path, name: str, ssh_alias: str, workdir: str) -> Dict[str, Any]:
    return _update(repo, _name(name, "target name"),
                   _record({"ssh_alias": ssh_alias, "workdir": workdir}))


def remove(repo: Path, name: str) -> Dict[str, Any]:
    return _update(repo, _name(name, "target name"), None)


def check(repo: Path, name: str) -> Dict[str, Any]:
    reason = "SSH target execution is not yet supported"
    valid = False
    try:
        _name(name, "target name")
        if name not in listing(repo)["targets"]:
            raise CoreError("unknown remote target: %s" % name)
        valid = True
    except CoreError as exc:
        reason = "%s; %s" % (exc, reason)
    return {"profile": "remote", "ready": False, "target_valid": valid,
            "reason": reason, "missing": ["ssh-execution-not-supported"],
            "details": {"target": name},
            "declared": {"transport": "ssh-alias"},
            "enforced": {"filesystem": False, "network": False},
            "unknown": ["SSH alias resolution", "remote filesystem", "remote network", "remote cleanup"],
            "warning": "Target metadata is local only; no SSH connection or isolation is established."}
