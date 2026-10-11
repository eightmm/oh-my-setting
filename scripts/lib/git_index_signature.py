"""Content signature of a Git index for the worker guard, read without running Git.

`git status` in any session rewrites the index with refreshed stat data and caches,
so a byte digest stops every running worker. The signature keeps what can stage or
hide a change (path, mode, object, stage, assume-unchanged, skip-worktree,
intent-to-add, other extensions) and drops stat fields and rebuildable caches.
"""

import hashlib
import struct

CACHES = {b"TREE", b"UNTR", b"FSMN", b"EOIE", b"IEOT"}


def signature(data, oid_size=20):
    """Return a hex digest, or None when the bytes are not an index this reader proves."""
    if len(data) < 12 + oid_size or data[:4] != b"DIRC":
        return None
    version, count = struct.unpack(">II", data[4:12])
    if version not in (2, 3, 4):
        return None
    end = len(data) - oid_size
    digest = hashlib.sha256(b"oms-index-v1\0%d\0" % version)
    at, previous = 12, b""
    for _ in range(count):
        start = at
        if at + 40 + oid_size + 2 > end:
            return None
        mode = data[at + 24:at + 28]
        oid = data[at + 40:at + 40 + oid_size]
        flags, = struct.unpack(">H", data[at + 40 + oid_size:at + 42 + oid_size])
        at += 42 + oid_size
        extended = 0
        if flags & 0x4000:
            if version < 3 or at + 2 > end:
                return None
            extended, = struct.unpack(">H", data[at:at + 2])
            at += 2
        if version == 4:
            strip, at = _prefix(data, at, end)
            if strip is None or strip > len(previous):
                return None
            stop = data.find(b"\0", at, end)
            if stop < 0:
                return None
            path = previous[:len(previous) - strip] + data[at:stop]
            at = stop + 1
        else:
            stop = data.find(b"\0", at, end)
            if stop < 0:
                return None
            path = data[at:stop]
            at = start + ((stop - start + 8) // 8) * 8
        previous = path
        digest.update(struct.pack(">HH", flags & 0xB000, extended) + mode + oid + path + b"\0")
    while at < end:
        if at + 8 > end:
            return None
        name = data[at:at + 4]
        size, = struct.unpack(">I", data[at + 4:at + 8])
        if at + 8 + size > end:
            return None
        if name not in CACHES:
            digest.update(name + struct.pack(">I", size) + data[at + 8:at + 8 + size])
        at += 8 + size
    return digest.hexdigest()


def _prefix(data, at, end):
    """Offset-encoded varint of index v4 (the same encoding as pack delta bases)."""
    if at >= end:
        return None, at
    byte = data[at]
    value = byte & 0x7F
    at += 1
    while byte & 0x80:
        if at >= end:
            return None, at
        byte = data[at]
        value = ((value + 1) << 7) | (byte & 0x7F)
        at += 1
    return value, at
