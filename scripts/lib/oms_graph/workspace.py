"""Workspace fingerprint for the execution read-cache (W5).

`git.head` cannot key a `cacheable: true` inspect result on its own: an
uncommitted edit to a tracked file, a staged change, and a new untracked file
all leave HEAD unchanged while changing what the tool would read.  The
fingerprint below covers those, and fails closed -- an unsafe workspace yields
an empty digest, so the caller neither reads nor writes the cache -- instead of
degrading back to HEAD.

`.oms/` is OMS state, not workspace content: the run's own `events.jsonl`
appends must never invalidate the cache, so `.oms/` and `.git/` are excluded.
Ignored files are already absent from `git status`.
"""

from __future__ import annotations

import stat
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Mapping, Sequence, Tuple

from oms_runtime.common import canonical_json, run_output, sha256_bytes, sha256_file

from .errors import GraphError

SCHEMA = 1
GIT_TIMEOUT = 20
EXCLUDED_ROOTS = (".oms", ".git")


def workspace_fingerprint(repo: Path, *, max_entries: int = 200,
                          max_file_bytes: int = 2 * 1024 * 1024) -> Dict[str, Any]:
    """Return {"schema", "head", "digest", "unsafe", "dirty_count"} for `repo`.

    `digest` is non-empty only when the whole dirty set was hashed; otherwise
    `unsafe` names the first refusal (paths are scanned in sorted order, so the
    reason is stable) and `digest` is "".  Raises GraphError only when git
    itself cannot run.
    """
    repo = Path(repo).resolve()
    porcelain = _git(repo, ["status", "--porcelain=v1", "-z", "--untracked-files=all"])
    head = run_output(["git", "-C", str(repo), "rev-parse", "HEAD"], cwd=repo)
    if not head:
        return _result("", "", "no-head", 0)
    dirty = sorted((pair for pair in _parse_porcelain(porcelain) if not _excluded(pair[1])),
                   key=lambda pair: pair[1])
    if len(dirty) > max_entries:
        return _result(head, "", "too-many-dirty-paths:%d" % len(dirty), len(dirty))
    entries: List[List[str]] = []
    for status, path in dirty:
        content, reason = _entry_content(repo, status, path, max_file_bytes)
        if reason:
            return _result(head, "", reason, len(dirty))
        entries.append([status, path, content])
    payload = {"schema": SCHEMA, "head": head, "staged": _staged_text(repo), "entries": entries}
    return _result(head, sha256_bytes(canonical_json(payload)), "", len(dirty))


def cache_allowed(fp: Mapping[str, Any]) -> bool:
    """A fingerprint may key the cache only when nothing was refused."""
    return bool(fp.get("digest")) and not str(fp.get("unsafe") or "")


# --------------------------------------------------------------------------
# internals
# --------------------------------------------------------------------------

def _result(head: str, digest: str, unsafe: str, dirty_count: int) -> Dict[str, Any]:
    return {"schema": SCHEMA, "head": head, "digest": digest, "unsafe": unsafe,
            "dirty_count": dirty_count}


def _git(repo: Path, args: Sequence[str]) -> str:
    """Unlike `run_output`, a git failure raises here instead of reading as clean."""
    command = ["git", "-C", str(repo)] + list(args)
    try:
        completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   timeout=GIT_TIMEOUT, check=False)
    except (OSError, subprocess.SubprocessError) as exc:
        raise GraphError("workspace fingerprint could not run git %s: %s" % (args[0], exc))
    if completed.returncode != 0:
        raise GraphError("workspace fingerprint could not run git %s in %s (exit %d)"
                         % (args[0], repo, completed.returncode))
    return completed.stdout.decode("utf-8", "surrogateescape")


def _parse_porcelain(text: str) -> List[Tuple[str, str]]:
    """NUL-separated `XY path` records; a rename/copy carries the old path next."""
    records = text.split("\0")
    entries: List[Tuple[str, str]] = []
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if len(record) < 4:
            continue
        status, path = record[:2], record[3:]
        if status[0] in ("R", "C"):
            index += 1  # the source path; the entry is the new path
        entries.append((status, path))
    return entries


def _excluded(path: str) -> bool:
    return any(path == root or path.startswith(root + "/") for root in EXCLUDED_ROOTS)


def _entry_content(repo: Path, status: str, path: str, max_file_bytes: int) -> Tuple[str, str]:
    """(content marker, "") for one dirty path, or ("", refusal reason)."""
    if status[1:2] == "D" or status == "D ":
        return "deleted", ""
    target = repo / path
    try:
        info = target.lstat()
    except OSError:
        return "", "unreadable:%s" % path
    if stat.S_ISLNK(info.st_mode):
        return "", "symlink:%s" % path
    if not stat.S_ISREG(info.st_mode):
        return "", "unreadable:%s" % path
    if info.st_size > max_file_bytes:
        return "", "large-file:%s" % path
    try:
        return sha256_file(target), ""
    except OSError:
        return "", "unreadable:%s" % path


def _staged_text(repo: Path) -> str:
    """Staged blob hashes, so an `add`ed change counts even with unchanged bytes."""
    lines = []
    for line in _git(repo, ["diff-index", "--cached", "HEAD", "--"]).splitlines():
        fields = line.split("\t")
        if any(_excluded(field) for field in fields[1:]):
            continue
        lines.append(line)
    return "\n".join(lines)
