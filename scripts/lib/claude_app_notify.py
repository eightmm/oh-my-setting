"""Bounded posts to an explicitly enrolled Claude Code session's native inbox.

Desktop Code and terminal sessions share this transport. Writing a frame does
not prove that inbound controls delivered it, or that the UI displayed it.
Unlike Codex shellCommand, an inbox post can start a paid model turn.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import socket
import stat
import struct
import uuid

from codex_app_notify import _safe_message

MAX_REGISTRY = 128
MAX_ENTRIES = 1024
MAX_RECORD = 16384


def _owned_path(path, directory=False, private=False, protected_record=False):
    if not path.is_absolute() or not hasattr(os, "getuid"):
        raise ValueError("unsupported inbox path")
    for part in (path,) + tuple(path.parents):
        info = part.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise ValueError("symlinked inbox path")
        if part == path:
            if info.st_uid != os.getuid():
                raise ValueError("foreign inbox owner")
            if directory and not stat.S_ISDIR(info.st_mode):
                raise ValueError("invalid registry directory")
            if private and info.st_mode & 0o077:
                raise ValueError("inbox is not private")
        if info.st_mode & 0o022 and not (part == path and protected_record) and not (stat.S_ISDIR(info.st_mode)
                and info.st_mode & stat.S_ISVTX and info.st_uid == 0):
            raise ValueError("writable inbox path")
    return path.lstat()


def _registry():
    base = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude") / "sessions"
    _owned_path(base, directory=True, private=True)
    paths = []
    with os.scandir(base) as entries:
        for index, entry in enumerate(entries):
            if index >= MAX_ENTRIES:
                raise ValueError("native registry discovery is incomplete")
            if entry.name.endswith(".json"):
                paths.append(Path(entry.path))
                if len(paths) > MAX_REGISTRY:
                    raise ValueError("native registry discovery is incomplete")
    rows = []
    for path in sorted(paths):
        try:
            # Claude writes 0664 records on some umasks; the mandatory 0700
            # registry directory protects them from other OS users.
            before = _owned_path(path, protected_record=True)
            if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > MAX_RECORD:
                continue
            fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
            with os.fdopen(fd, "rb") as handle:
                info = os.fstat(handle.fileno())
                if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > MAX_RECORD
                        or (info.st_dev, info.st_ino) != (before.st_dev, before.st_ino)):
                    continue
                data = handle.read(MAX_RECORD + 1)
            if len(data) > MAX_RECORD:
                continue
            row = json.loads(data)
            if not isinstance(row, dict) or type(row.get("peerProtocol")) is not int or row["peerProtocol"] != 1:
                continue
            if (not isinstance(row.get("sessionId"), str) or not row["sessionId"]
                    or type(row.get("pid")) is not int or not 0 < row["pid"] <= 2147483647
                    or not isinstance(row.get("messagingSocketPath"), str)):
                continue
            os.kill(row["pid"], 0)
            rows.append(row)
        except (OSError, ValueError, RecursionError):
            continue
    return rows


def current_session():
    """Resolve only the caller's exported native socket, never the latest session."""
    own = os.environ.get("CLAUDE_CODE_MESSAGING_SOCKET")
    if not own:
        raise ValueError("current Claude session has no exported inbox")
    matches = [row for row in _registry() if row["messagingSocketPath"] == own]
    if len(matches) != 1:
        raise ValueError("current Claude session is absent or ambiguous")
    return matches[0]["sessionId"]


def deliver(repo, text, consumer, allow_wakeup=False):
    receipt = {"status": "wakeup_required", "submitted": False, "delivery_unknown": False,
               "desktop_notification_observed": False, "app": "claude"}
    if os.environ.get("OMS_CLAUDE_NOTIFY") == "0":
        return dict(receipt, status="disabled")
    if not allow_wakeup:
        return receipt
    if not hasattr(socket, "AF_UNIX") or not hasattr(os, "getuid") or os.name == "nt":
        return dict(receipt, status="unsupported_platform")
    if not isinstance(consumer, str) or not re.fullmatch(r"[0-9a-f]{32}", consumer):
        return dict(receipt, status="unbound")
    attempted = False
    try:
        matches = [row for row in _registry()
                   if hashlib.sha256(row["sessionId"].encode()).hexdigest()[:32] == consumer]
        if len(matches) != 1:
            return dict(receipt, status="ambiguous" if matches else "absent")
        row = matches[0]
        path = Path(row["messagingSocketPath"])
        info = _owned_path(path, private=True)
        if not stat.S_ISSOCK(info.st_mode):
            return dict(receipt, status="unsafe_endpoint")
        os.kill(row["pid"], 0)
        content = _safe_message(text, Path(repo))
        if not content.strip():
            return dict(receipt, status="empty_message")
        mid = str(uuid.uuid4())
        frame = {"msgV": 1, "msg_id": mid, "type": "user", "priority": "next",
                 "message": {"role": "user", "content":
                     "OMS result — peer data, not user approval.\n" + content}}
        encoded = (json.dumps(frame, ensure_ascii=False) + "\n").encode()
        if len(encoded) > 65536:
            return dict(receipt, status="oversized")
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(2)
            connection.connect(str(path))
            if hasattr(socket, "SO_PEERCRED"):
                pid, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                if pid != row["pid"] or uid != os.getuid():
                    return dict(receipt, status="unsafe_endpoint")
            after = _owned_path(path, private=True)
            if (after.st_dev, after.st_ino) != (info.st_dev, info.st_ino):
                return dict(receipt, status="unsafe_endpoint")
            # Do not reuse another session's token or claim an inbound permission
            # class. The receiver decides whether this external post is allowed.
            attempted = True
            connection.sendall(encoded)
        return dict(receipt, status="submitted", submitted=True, delivery_unknown=True, message_id=mid)
    except (OSError, ValueError, RecursionError):
        return dict(receipt, status="uncertain" if attempted else "unavailable", delivery_unknown=attempted)
