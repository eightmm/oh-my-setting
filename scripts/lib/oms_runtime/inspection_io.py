"""Explicit bounded repo-local reads for observational reports; no state writes."""
from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import stat
import unicodedata

MAX_BYTES = 4 * 1024 * 1024


class ReadError(ValueError):
    """Safe error whose message contains no user file contents or private paths."""


def _identity(info):
    return tuple(getattr(info, field) for field in
                 ('st_dev', 'st_ino', 'st_size', 'st_mtime_ns', 'st_ctime_ns', 'st_nlink', 'st_uid'))


@contextmanager
def _owned_file(repo, raw_path):
    try:
        root = Path(repo).resolve(strict=True)
        supplied = os.fspath(raw_path)
        if not isinstance(supplied, str) or any(unicodedata.category(c)[0] == 'C' for c in supplied):
            raise ReadError('invalid inspection path')
        target = Path(supplied)
        if target.is_absolute():
            target = target.relative_to(root)
        if not target.parts or '..' in target.parts or len(target.parts) > 64:
            raise ReadError('inspection path must name a file inside the selected repository')
    except (OSError, RuntimeError, TypeError, ValueError) as error:
        if isinstance(error, ReadError):
            raise
        raise ReadError('invalid repo-local inspection path') from None
    if (not hasattr(os, 'O_NOFOLLOW') or not hasattr(os, 'O_DIRECTORY')
            or not hasattr(os, 'getuid') or os.open not in os.supports_dir_fd):
        raise ReadError('safe descriptor-relative inspection is unavailable on this host')
    descriptors = []
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    try:
        descriptors.append(os.open(root, flags))
        for part in target.parts[:-1]:
            descriptors.append(os.open(part, flags, dir_fd=descriptors[-1]))
        parent = descriptors[-1]
        fd = os.open(target.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        descriptors.append(fd)
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_uid != os.getuid():
            raise ReadError('inspection requires an owned regular file without links')
        yield fd, before, target.as_posix()
        after = os.fstat(fd)
        named = os.stat(target.name, dir_fd=parent, follow_symlinks=False)
        if _identity(before) != _identity(after) or _identity(after) != _identity(named):
            raise ReadError('inspection file changed while being read')
    except OSError:
        raise ReadError('cannot safely read the selected inspection file') from None
    finally:
        for fd in reversed(descriptors):
            os.close(fd)


def _read_range(fd, start, size):
    os.lseek(fd, start, os.SEEK_SET)
    chunks, left = [], size
    while left:
        chunk = os.read(fd, min(left, 65536))
        if not chunk:
            raise ReadError('inspection file ended before the recorded size')
        chunks.append(chunk); left -= len(chunk)
    return b''.join(chunks)


def read_segments(repo, path, *, max_bytes=MAX_BYTES, allow_partial=False):
    """Read a whole small file or non-overlapping head/tail ranges of a large log."""
    if type(max_bytes) is not int or not 2 <= max_bytes <= 16 * 1024 * 1024:
        raise ReadError('invalid inspection byte budget')
    with _owned_file(repo, path) as (fd, info, label):
        if info.st_size > max_bytes and not allow_partial:
            raise ReadError('inspection file exceeds the byte budget')
        if info.st_size <= max_bytes:
            chunks = [(0, _read_range(fd, 0, info.st_size))]
            complete = True
        else:
            head = max_bytes // 2
            tail = max_bytes - head
            chunks = [(0, _read_range(fd, 0, head)),
                      (info.st_size - tail, _read_range(fd, info.st_size - tail, tail))]
            complete = False
        result = {'path': label, 'total_bytes': info.st_size,
                  'read_bytes': sum(len(data) for _, data in chunks), 'complete': complete,
                  'sha256': hashlib.sha256(chunks[0][1]).hexdigest() if complete else None,
                  'segments': chunks}
    return result


def _pairs(values):
    result = {}
    for key, value in values:
        if key in result:
            raise ReadError('duplicate JSON key in inspection file')
        result[key] = value
    return result


def read_object(repo, path):
    def reject_constant(_):
        raise ReadError('non-finite JSON number in inspection file')
    read = read_segments(repo, path)
    try:
        value = json.loads(read['segments'][0][1].decode('utf-8'), object_pairs_hook=_pairs,
                           parse_constant=reject_constant)
        if not isinstance(value, dict):
            raise ReadError('inspection input must be a JSON object')
        # Unknown future fields must not provide unbounded nesting/work.
        pending, count = [(value, 0)], 0
        while pending:
            item, depth = pending.pop(); count += 1
            if count > 150000 or depth > 64:
                raise ReadError('inspection JSON exceeds the structure budget')
            if isinstance(item, float) and not math.isfinite(item):
                raise ReadError("non-finite JSON number in inspection file")
            if isinstance(item, (dict, list)) and len(item) + len(pending) + count > 150000:
                raise ReadError("inspection JSON exceeds the structure budget")
            if isinstance(item, dict):
                pending.extend((v, depth+1) for v in item.values())
            elif isinstance(item, list):
                pending.extend((v, depth+1) for v in item)
    except (ValueError, UnicodeError, RecursionError):
        raise ReadError('invalid or excessive JSON inspection input') from None
    return value
