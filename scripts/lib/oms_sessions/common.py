"""Session-local bounds and records; no transcript text belongs in these records."""
from __future__ import annotations

import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import time
import uuid

from oms_runtime.common import (
    CoreError, SECRET_VALUE_RE, atomic_write_bytes, atomic_write_json,
    ensure_private_dir, file_lock, install_root, read_json, sensitive_text, utc_now,
)

MAX_PATCH_BYTES = 32 * 1024 * 1024
MAX_RECORD_BYTES = 1024 * 1024
ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,159}\Z")
SECRET_RE = re.compile(r"\b(?:gh[pousr]_|github_pat_|sk-(?:proj-|ant-)?)[A-Za-z0-9_-]{12,}|"
                    r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}")


def parent_only():
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise CoreError("session exploration is parent-only; workers cannot read or launch native owner sessions")


def ident(value, label="session ID"):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise CoreError("%s must be an exact alphanumeric ID with optional '-' or '_'" % label)
    return value


def label(value):
    if (not isinstance(value, str) or len(value.encode("utf-8")) > 120
            or any(ord(ch) < 32 for ch in value) or sensitive_text(value) or SECRET_RE.search(value)):
        raise CoreError("label must be a short title without sensitive content or control characters")
    return value


def excerpt(value, start=0, size=400):
    # Preserve exact characters/offsets for ordinary text. A withheld excerpt is
    # explicitly marked, never presented as a faithful quotation.
    if SECRET_VALUE_RE.search(value) or SECRET_RE.search(value):
        return "[sensitive-content-omitted]", True
    return value[start:start + size], False


def git_env():
    env = dict(os.environ)
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR",
                "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE"):
        env.pop(key, None)
    env["GIT_OPTIONAL_LOCKS"] = "0"
    return env


def command(argv, *, cwd=None, limit=MAX_PATCH_BYTES, env=None, timeout=60):
    """Bound memory and error output even for a large Git diff or helper result."""
    process = None
    try:
        process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        data = bytearray()
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise CoreError("local command timed out")
                for key, _ in selector.select(min(remaining, 1)):
                    chunk = os.read(key.fd, min(65536, limit + 1 - len(data)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    data.extend(chunk)
                    if len(data) > limit:
                        raise CoreError("local command output exceeds the %d byte bound" % limit)
        code = process.wait(timeout=max(.01, deadline - time.monotonic()))
        if code:
            # Helper errors can quote patches or private transcript content.
            raise CoreError("%s failed (exit %d); no native model was started" % (Path(argv[0]).name, code))
        return bytes(data)
    except (OSError, subprocess.SubprocessError) as exc:
        raise CoreError("local command could not complete: %s" % type(exc).__name__) from None
    finally:
        if process is not None:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            if process.stdout is not None:
                process.stdout.close()


def git(repo, *args, limit=MAX_PATCH_BYTES):
    return command(["git", "-C", str(repo), "-c", "core.fsmonitor=false", *args],
                   limit=limit, env=git_env())


def repository(raw):
    candidate = Path(raw).expanduser().resolve(strict=True)
    if any(ord(ch) < 32 for ch in str(candidate)):
        raise CoreError("repository paths must not contain control characters")
    top = git(candidate, "rev-parse", "--show-toplevel", limit=8192).decode().rstrip("\r\n")
    repo = Path(top).resolve(strict=True)
    if not repo.is_dir() or not git(repo, "rev-parse", "--verify", "HEAD", limit=256).strip():
        raise CoreError("session exploration requires a Git repository with an initial commit")
    return repo


def plain_chain(path, *, directory=False):
    """Reject symlink traversal, including a symlink above the final file."""
    current = Path(os.path.abspath(str(path)))
    for item in (current, *current.parents):
        if item.is_symlink():
            raise CoreError("refusing a symlink in a session path")
    if directory and current.exists() and not current.is_dir():
        raise CoreError("session state path is not a directory")
    return current


def owned_bytes(path, limit):
    # The panel's audited reader checks regular file, uid, link count, inode,
    # O_NOFOLLOW, and bounded reads. Directory traversal is checked here too.
    from panel_chats import _read_owned
    path = plain_chain(path)
    try:
        info = path.lstat()
        if info.st_size > limit:
            raise CoreError("native file exceeds the %d byte bound" % limit)
        data = _read_owned(path, limit + 1, expect=(info.st_dev, info.st_ino))
        after = path.lstat()
        if len(data) > limit or (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns) != (
                after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
            raise CoreError("native file changed while reading; retry when the session is idle")
        return data
    except OSError:
        raise CoreError("native file is missing, unreadable, or not an owned regular file") from None


def state_dir(repo, create=False):
    path = plain_chain(repo / ".oms" / "sessions", directory=True)
    if create:
        if git(repo, "ls-files", "--", ".oms/sessions", limit=8192).strip():
            raise CoreError("session records must not be tracked by Git")
        ensure_private_dir(path)
        ignore = path / ".gitignore"
        if not ignore.exists():
            atomic_write_bytes(ignore, b"*\n")
        elif ignore.is_symlink():
            raise CoreError("session ignore file must not be a symlink")
        ignored = subprocess.run(["git", "-C", str(repo), "check-ignore", "-q", "--",
                                  ".oms/sessions/record.json"], env=git_env(), check=False)
        if ignored.returncode:
            raise CoreError("session records must be ignored by Git")
    return path


def new_id(kind):
    return "%s-%s" % (kind, uuid.uuid4().hex[:20])


def save(repo, kind, row):
    folder = state_dir(repo, True) / kind
    ensure_private_dir(folder)
    path = folder / (ident(row["id"], "record ID") + ".json")
    with file_lock(path):
        atomic_write_json(path, row)
    return row


def load(repo, kind, key):
    path = plain_chain(state_dir(repo) / kind / (ident(key, "record ID") + ".json"))
    row = read_json(path, limit=MAX_RECORD_BYTES)
    if not isinstance(row, dict) or row.get("schema") != 1 or row.get("id") != key or row.get("repo") != str(repo):
        raise CoreError("record is missing or belongs to a different repository")
    return row


def transition(repo, kind, key, expected, changes):
    """Claim a launch under the record lock; atomic writes alone do not exclude a second launcher."""
    path = state_dir(repo, True) / kind / (ident(key, "record ID") + ".json")
    with file_lock(path):
        row = load(repo, kind, key)
        if any(row.get(name) != value for name, value in expected.items()):
            raise CoreError("session record changed; another launch may already own this fork")
        row.update(changes)
        atomic_write_json(path, row)
        return row


def records(repo, kind):
    folder = plain_chain(state_dir(repo) / kind, directory=True)
    if not folder.exists():
        return []
    result = []
    with os.scandir(folder) as entries:
        for count, entry in enumerate(entries):
            if len(result) >= 1000 or count >= 2000:
                raise CoreError("more than 1000 session records; narrow or archive the local records")
            if entry.name.endswith(".json"):
                result.append(load(repo, kind, entry.name[:-5]))
    return sorted(result, key=lambda row: (row.get("created_at", ""), row["id"]), reverse=True)


def script(name):
    return str(install_root() / "scripts" / name)


def base_record(repo, kind):
    return {"schema": 1, "id": new_id(kind), "repo": str(repo), "created_at": utc_now()}
