"""Private atomic input state using the shared OMS writer and lock protocol."""
from __future__ import annotations

import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import time
import uuid

from oms_runtime.common import (
    CoreError, SECRET_VALUE_RE, atomic_write_bytes, atomic_write_json, canonical_json,
    ensure_private_dir, file_lock, read_json, sha256_bytes, utc_now,
)
from room_repository import state_repository, work_repository

MAX_STATE_BYTES = 4 * 1024 * 1024
ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
TOKEN_RE = re.compile(r"\b(?:gh[pousr]_|github_pat_|sk-(?:proj-|ant-)?)[A-Za-z0-9_-]{12,}")
STRUCTURED_SECRET_RE = re.compile(r"(?i)(?:api[_-]?key|access[_-]?token|client[_-]?secret|password|private[_-]?key)[\"']?\s*[:=]")


def child():
    return os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0"


def parent_only():
    if child():
        raise CoreError("only the parent may capture attachments, edit inputs, or deliver messages")


def ident(value):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise CoreError("expected an exact local identifier")
    return value


def digest(value):
    if not isinstance(value, str) or not HEX.fullmatch(value):
        raise CoreError("expected a full SHA-256 digest")
    return value


def plain(path):
    path = Path(os.path.abspath(str(path)))
    if any(part.is_symlink() for part in (path, *path.parents)):
        raise CoreError("input paths must not traverse symbolic links")
    return path


def read_owned(path, limit):
    path = plain(path)
    try:
        info = path.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > limit
                or hasattr(os, "getuid") and info.st_uid != os.getuid()):
            raise CoreError("input must be an owned regular file within the size bound")
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as handle:
            before = os.fstat(handle.fileno())
            data = handle.read(limit + 1)
            after = os.fstat(handle.fileno())
        current = path.lstat()
        shape = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        if len(data) > limit or any(shape(s) != shape(info) for s in (before, after, current)):
            raise CoreError("input changed during capture; retry with a stable source")
        return data
    except OSError:
        raise CoreError("input file is missing, unreadable, or unsafe") from None


def safe_text(text, maximum, *, empty=False):
    if (not isinstance(text, str) or len(text.encode("utf-8")) > maximum
            or not empty and not text.strip() or "\0" in text
            or any(ord(ch) < 32 and ch not in "\r\n\t" for ch in text)):
        raise CoreError("text is empty, oversized, or contains unsupported control characters")
    if SECRET_VALUE_RE.search(text) or STRUCTURED_SECRET_RE.search(text) or TOKEN_RE.search(text):
        raise CoreError("input contains secret-like content; redact it before attachment or delivery")
    return text


def bounded_command(argv, limit, timeout=5):
    process = None
    try:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        data = bytearray()
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                if time.monotonic() >= deadline:
                    raise CoreError("local capture timed out")
                for key, _ in selector.select(min(1, max(.01, deadline - time.monotonic()))):
                    chunk = os.read(key.fd, min(65536, limit + 1 - len(data)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                    data.extend(chunk)
                    if len(data) > limit:
                        raise CoreError("local capture exceeded its byte bound")
        if process.wait(timeout=max(.01, deadline - time.monotonic())):
            raise CoreError("local capture failed")
        return bytes(data)
    except (OSError, subprocess.SubprocessError):
        raise CoreError("local capture is unavailable") from None
    finally:
        if process is not None:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            process.stdout.close()


def context(raw):
    work = work_repository(raw)
    if any(ord(ch) < 32 for ch in str(work)):
        raise CoreError("repository path contains unsupported control characters")
    result = subprocess.run(["git", "-C", str(work), "rev-parse", "--show-toplevel"],
                            capture_output=True, text=True, timeout=5, check=False)
    if result.returncode or Path(result.stdout.rstrip("\r\n")).resolve() != work:
        raise CoreError("inputs require a Git repository root")
    return work, state_repository(work)


def directory(repo, create=False):
    path = plain(repo / ".oms" / "inputs")
    if create:
        tracked = subprocess.run(["git", "-C", str(repo), "ls-files", "--", ".oms/inputs"],
                                 capture_output=True, check=True, timeout=5).stdout
        if tracked:
            raise CoreError("private input state must not be tracked by Git")
        ensure_private_dir(path)
        ignore = path / ".gitignore"
        if not ignore.exists():
            atomic_write_bytes(ignore, b"*\n")
        else:
            plain(ignore)
        checked = subprocess.run(["git", "-C", str(repo), "check-ignore", "-q", "--", ".oms/inputs/queue.json"],
                                 check=False, timeout=5)
        if checked.returncode:
            raise CoreError("private inputs must be ignored by Git")
    elif path.exists() and not path.is_dir():
        raise CoreError("input state is not a directory")
    return path


def state(repo):
    path = plain(directory(repo) / "queue.json")
    value = read_json(path, limit=MAX_STATE_BYTES)
    if value is None:
        return {"schema": 1, "repo": str(repo), "revision": 0, "items": [], "feedback_batches": {}}
    if (not isinstance(value, dict) or value.get("schema") != 1 or value.get("repo") != str(repo)
            or not isinstance(value.get("items"), list) or len(value["items"]) > 256
            or not isinstance(value.get("feedback_batches"), dict)):
        raise CoreError("invalid or oversized input queue")
    return value


def write_state(repo, value):
    value["revision"] += 1
    if len(canonical_json(value)) > MAX_STATE_BYTES:
        raise CoreError("input queue exceeds its 4 MiB bound")
    atomic_write_json(directory(repo, True) / "queue.json", value)


def locked(repo):
    return file_lock(directory(repo, True) / "queue.json")


def new_id(prefix):
    return prefix + "-" + uuid.uuid4().hex[:20]


def item(value, key):
    found = next((row for row in value["items"] if row.get("id") == ident(key)), None)
    if found is None:
        raise CoreError("input item does not exist")
    return found
