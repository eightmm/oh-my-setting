"""Scoped retrieval of retained, store-time-redacted output-digest captures.

References bind repository, filename and stored-byte SHA, not authorization.
Readers cannot execute commands or recover an evicted capture.
"""

import errno
import hashlib
import os
from pathlib import Path
import re
import stat

CAPTURE_ROOT = Path(".oms/artifacts/output-digest")
CAPTURE_NAME_RE = re.compile(r"[0-9]{8}T[0-9]{6}Z-[0-9]+\.log\Z")
SHA_RE = re.compile(r"[0-9a-f]{64}\Z")
READ_CAP = 64 * 1024 * 1024
PAGE_BYTES = 16 * 1024


def capture_reference(repo: Path, name: str, sha256: str) -> str:
    """Mint a reference from an already computed stored-byte hash; no file read."""
    if not isinstance(name, str) or not CAPTURE_NAME_RE.fullmatch(name):
        raise ValueError("invalid capture filename")
    if not isinstance(sha256, str) or not SHA_RE.fullmatch(sha256):
        raise ValueError("invalid capture SHA-256")
    scope = hashlib.sha256((str(Path(repo).resolve()) + "\0" + name + "\0" + sha256).encode("utf-8")).hexdigest()
    return "output:%s:%s:%s" % (name, sha256, scope)


def capture_metadata(repo: Path, name: str, sha256: str) -> dict:
    ref = capture_reference(repo, name, sha256)
    return {"capture_ref": ref, "capture_sha256": sha256, "read_tool": "oms_output_read",
            "read_arguments": {"repo": str(Path(repo).resolve()), "capture_ref": ref}}


class CaptureError(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def read_stored_capture(repo: Path, name: str, *, max_bytes: int = READ_CAP) -> bytes:
    """Open each store component relative to an owned descriptor, without links.

    Check the pathname chain again before returning. Same-user filesystem
    writers remain outside this integrity boundary, as with peer answer reads.
    """
    if not isinstance(name, str) or not CAPTURE_NAME_RE.fullmatch(name):
        raise CaptureError("invalid_reference", "invalid capture filename")
    if type(max_bytes) is not int or not 1 <= max_bytes <= READ_CAP:
        raise CaptureError("invalid_limit", "max_bytes must be an integer from 1 to %d" % READ_CAP)
    repo = Path(repo).resolve()
    descriptors = []
    links = []
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    try:
        parent = os.open(repo, flags | os.O_DIRECTORY)
        descriptors.append(parent)
        root_info = os.fstat(parent)
        for component in (*CAPTURE_ROOT.parts, name):
            is_file = component == name
            before = os.stat(component, dir_fd=parent, follow_symlinks=False)
            if ((is_file and (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1))
                    or (not is_file and not stat.S_ISDIR(before.st_mode))):
                raise CaptureError("unsafe_capture", "capture store contains an unsafe file or directory")
            if is_file and before.st_size > max_bytes:
                raise CaptureError("capture_too_large", "stored capture exceeds the %d-byte read limit" % max_bytes)
            opened = os.open(component, flags | (0 if is_file else os.O_DIRECTORY), dir_fd=parent)
            descriptors.append(opened)
            info = os.fstat(opened)
            if not os.path.samestat(before, info):
                raise CaptureError("capture_changed", "capture changed before reading")
            links.append((parent, component, info))
            parent = opened
        with os.fdopen(os.dup(parent), "rb") as handle:
            data = handle.read(max_bytes + 1)
        after = os.fstat(parent)
        if (len(data) > max_bytes or info.st_size != len(data) or after.st_nlink != 1
                or (info.st_size, info.st_mtime_ns, info.st_ctime_ns)
                != (after.st_size, after.st_mtime_ns, after.st_ctime_ns)):
            raise CaptureError("capture_changed", "capture changed while reading")
        for directory, component, original in links:
            current = os.stat(component, dir_fd=directory, follow_symlinks=False)
            if not os.path.samestat(original, current) or stat.S_IFMT(original.st_mode) != stat.S_IFMT(current.st_mode):
                raise CaptureError("unsafe_capture", "capture path changed while reading")
        if not os.path.samestat(root_info, repo.stat()):
            raise CaptureError("unsafe_capture", "repository changed while reading")
        return data
    except FileNotFoundError:
        raise CaptureError("capture_missing", "stored capture is missing or evicted; no command was rerun") from None
    except OSError as exc:
        code = "unsafe_capture" if exc.errno in (errno.ELOOP, errno.ENOTDIR) else "capture_unreadable"
        raise CaptureError(code, "capture store is unsafe or unreadable") from None
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def read_capture(repo: Path, arguments: dict) -> dict:
    ref = arguments.get("capture_ref")
    parts = ref.split(":") if isinstance(ref, str) else []
    if (len(parts) != 4 or parts[0] != "output" or not CAPTURE_NAME_RE.fullmatch(parts[1])
            or not SHA_RE.fullmatch(parts[2]) or not SHA_RE.fullmatch(parts[3])):
        raise CaptureError("invalid_reference", "capture_ref must be a returned output capture reference")
    name, sha256 = parts[1:3]
    if ref != capture_reference(repo, name, sha256):
        raise CaptureError("capture_scope_mismatch", "capture reference belongs to another repository or was altered")
    offset, limit = arguments.get("offset", 0), arguments.get("limit", PAGE_BYTES)
    for label, value, low, high in (("offset", offset, 0, READ_CAP * 3), ("limit", limit, 4, PAGE_BYTES)):
        if type(value) is not int or not low <= value <= high:
            raise CaptureError("invalid_range", "%s must be an integer from %d to %d" % (label, low, high))
    if "query" in arguments:
        query = arguments["query"]
        try:
            needle = query.encode("utf-8") if isinstance(query, str) else b""
        except UnicodeEncodeError:
            needle = b""
        if not 1 <= len(needle) <= 256 or "limit" in arguments:
            raise CaptureError("invalid_query", "query requires 1-256 UTF-8 bytes and cannot accompany limit")
    data = read_stored_capture(repo, name)
    if hashlib.sha256(data).hexdigest() != sha256:
        raise CaptureError("capture_hash_changed", "stored capture SHA-256 changed; reference is stale")
    raw = data.decode("utf-8", errors="replace").encode("utf-8")
    if offset > len(raw) or (offset < len(raw) and raw[offset] & 0xC0 == 0x80):
        raise CaptureError("invalid_range", "offset must be a UTF-8 boundary within the capture text")
    base_args = {"repo": str(repo), "capture_ref": ref}
    payload = {"capture_ref": ref, "capture_sha256": sha256, "capture_bytes": len(data),
               "content_bytes": len(raw), "offset": offset,
               "encoding": "utf-8 (invalid bytes replaced)",
               "provenance": "stored redacted capture; not verification evidence"}
    if "query" in arguments:
        matches = []
        position = raw.find(needle, offset)
        end = offset
        while position >= 0 and len(matches) < 8:
            start = max(0, position - 128)
            while start < position and raw[start] & 0xC0 == 0x80:
                start += 1
            end = position + len(needle)
            snippet = raw[start:end + 128].decode("utf-8", errors="ignore")
            matches.append({"offset": position, "end": end, "context": snippet,
                            "read_arguments": {**base_args, "offset": start}})
            position = raw.find(needle, end)
        payload.update(matches=matches, end=end, has_more=position >= 0)
        if payload["has_more"]:
            payload["next_search_arguments"] = {**base_args, "offset": end, "query": query}
    else:
        page = raw[offset:offset + limit].decode("utf-8", errors="ignore")
        end = offset + len(page.encode("utf-8"))
        payload.update(content=page, end=end, has_more=end < len(raw))
        if payload["has_more"]:
            payload["next_read_arguments"] = {**base_args, "offset": end, "limit": limit}
    return payload
