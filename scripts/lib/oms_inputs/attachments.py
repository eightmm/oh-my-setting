"""Immutable local snapshots with explicit provenance and pre-delivery drift checks."""
from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
from urllib.parse import parse_qsl, urlsplit

from oms_runtime.common import CoreError, atomic_write_bytes, atomic_write_json, canonical_json, ensure_private_dir, read_json, sha256_bytes, utc_now
from .common import bounded_command, directory, ident, new_id, plain, read_owned, safe_text

MAX_SOURCE = 4 * 1024 * 1024
MAX_ATTACHMENT = 64 * 1024
MAX_FEEDBACK = 1024 * 1024


def create(repo, kind, source, content):
    limit = MAX_FEEDBACK if kind == "artifact-feedback" else MAX_ATTACHMENT
    safe_text(content, limit)
    key = new_id("attachment")
    folder = directory(repo, True) / "attachments" / key
    ensure_private_dir(folder)
    encoded = content.encode("utf-8")
    record = {"schema": 1, "id": key, "repo": str(repo), "kind": kind, "source": source,
              "content_sha256": sha256_bytes(encoded), "content_bytes": len(encoded), "created_at": utc_now()}
    # Metadata is the commit marker: an interrupted content write is never
    # listed as an attachment, and an ID is never reused or overwritten.
    atomic_write_bytes(folder / "content.txt", encoded)
    atomic_write_json(folder / "meta.json", record)
    return record


def load(repo, key):
    folder = plain(directory(repo) / "attachments" / ident(key))
    record = read_json(folder / "meta.json", limit=64 * 1024)
    if not isinstance(record, dict) or record.get("schema") != 1 or record.get("id") != key or record.get("repo") != str(repo):
        raise CoreError("attachment is missing or belongs to another repository")
    data = read_owned(folder / "content.txt", MAX_FEEDBACK if record.get("kind") == "artifact-feedback" else MAX_ATTACHMENT)
    if len(data) != record.get("content_bytes") or sha256_bytes(data) != record.get("content_sha256"):
        raise CoreError("immutable attachment content has changed")
    return record, data.decode("utf-8")


def listing(repo):
    folder = plain(directory(repo) / "attachments")
    if not folder.exists():
        return []
    result = []
    for count, path in enumerate(folder.iterdir()):
        if count >= 1000:
            raise CoreError("attachment directory exceeds the 1000-entry bound")
        if (path / "meta.json").exists():
            result.append(load(repo, path.name)[0])
    return sorted(result, key=lambda row: row["created_at"])


def attach_file(repo, work, raw_path, start=None, end=None):
    path = Path(raw_path)
    if path.is_absolute():
        raise CoreError("file attachments require a repository-relative path")
    if any(part in ("..", ".git", ".oms") for part in path.parts):
        raise CoreError("file attachment escapes source scope or names private state")
    target = plain(work / path)
    try:
        relative = target.relative_to(work).as_posix()
    except ValueError:
        raise CoreError("file attachment escapes the source repository") from None
    data = read_owned(target, MAX_SOURCE)
    text = data.decode("utf-8")
    lines = text.splitlines(keepends=True)
    if start is not None or end is not None:
        first, last = start or 1, end or len(lines)
        if not 1 <= first <= last <= len(lines):
            raise CoreError("selected lines are outside the file")
        selected = "".join(lines[first - 1:last])
    else:
        first, last, selected = 1, len(lines), text
    return create(repo, "file", {"verified": True, "worktree": str(work), "path": relative,
                  "full_file_sha256": sha256_bytes(data), "full_file_bytes": len(data),
                  "start_line": first, "end_line": last}, selected)


def attach_selection(repo, source_kind, origin, content):
    safe_text(origin, 1000)
    if any(ch in origin for ch in "\r\n\t"):
        raise CoreError("selection origin must be one line")
    parsed = urlsplit(origin)
    if parsed.username or parsed.password:
        raise CoreError("selection origin must not contain URL credentials")
    if any(re.search(r"token|secret|password|credential|signature|api.?key", key, re.I)
           for key, _ in parse_qsl(parsed.query)):
        raise CoreError("selection origin must not contain a credential-bearing query")
    return create(repo, "selection", {"verified": False, "kind": source_kind, "declared_origin": origin,
                  "note": "Origin was supplied by the caller; OMS did not inspect a browser or chat."}, content)


def _tmux(argv, limit=MAX_ATTACHMENT):
    binary = shutil.which("tmux")
    if not binary:
        raise CoreError("tmux is required for explicit terminal capture")
    return bounded_command([binary, *argv], limit).decode("utf-8")


def pane_state(pane):
    if not re.fullmatch(r"%[0-9]+", pane):
        raise CoreError("terminal attachments require an exact tmux pane ID")
    raw = _tmux(["display-message", "-p", "-t", pane, "#{pane_id}\t#{pane_pid}\t#{pane_current_path}\t#{pane_dead}"], 8192)
    fields = raw.rstrip("\r\n").split("\t")
    if len(fields) != 4 or fields[0] != pane or not fields[1].isdigit() or fields[3] != "0":
        raise CoreError("terminal pane identity is unavailable or not live")
    return {"pane": pane, "pid": fields[1], "cwd": str(Path(fields[2]).resolve(strict=True))}


def attach_terminal(repo, work, pane, lines=80):
    if not 1 <= lines <= 200:
        raise CoreError("terminal capture must request 1 to 200 lines")
    before = pane_state(pane)
    cwd = Path(before["cwd"])
    if cwd != work and work not in cwd.parents:
        raise CoreError("terminal pane cwd is outside the selected repository")
    text = _tmux(["capture-pane", "-p", "-t", pane, "-S", "-" + str(lines)])
    if pane_state(pane) != before:
        raise CoreError("terminal pane identity or cwd changed during capture")
    selected = "".join(text.splitlines(keepends=True)[-lines:])
    return create(repo, "terminal", dict(before, verified=True, worktree=str(work), lines=lines,
                  note="Explicit historical terminal snapshot; no command was executed or injected."), selected)


def validate(repo, record):
    source = record["source"]
    if record["kind"] == "file":
        target = plain(Path(source["worktree"]) / source["path"])
        root = Path(source["worktree"])
        if not target.is_relative_to(root) or any(part in (".git", ".oms", "..") for part in Path(source["path"]).parts):
            raise CoreError("attachment source escapes its recorded worktree")
        if sha256_bytes(read_owned(target, MAX_SOURCE)) != source["full_file_sha256"]:
            raise CoreError("file attachment source drifted; capture a new attachment before delivery")
    elif record["kind"] == "terminal":
        current = pane_state(source["pane"])
        if any(current[key] != source[key] for key in ("pane", "pid", "cwd")):
            raise CoreError("terminal pane identity or cwd changed; capture a new attachment")
    elif record["kind"] == "artifact-feedback":
        registry = read_json(plain(repo / ".oms" / "artifact-views" / (ident(source["view_id"]) + ".json")), limit=MAX_FEEDBACK)
        if (not isinstance(registry, dict) or registry.get("id") != source["view_id"]
                or registry.get("data_sha256") != source["data_sha256"]
                or sha256_bytes(canonical_json(registry)) != source["registry_sha256"]):
            raise CoreError("feedback view source changed")
    elif record["kind"] != "selection":
        raise CoreError("unknown attachment source kind")


def retrieve(repo, key, full=False):
    record, content = load(repo, key)
    try:
        validate(repo, record)
        status = "current" if record["source"].get("verified") else "declared-unverified"
    except (CoreError, OSError, ValueError):
        status = "drifted-or-unavailable"
    excerpt = content if full else content.encode()[:8192].decode("utf-8", "ignore")
    return dict(record, content=excerpt, truncated=excerpt != content, source_status=status)
