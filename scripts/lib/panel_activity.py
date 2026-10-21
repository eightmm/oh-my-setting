"""Bounded, read-only view of a dispatched provider's native event stream."""

import json
import math
import os
import re
import stat
import threading
import time

_CACHE = {}
_CACHE_LOCK = threading.Lock()
_MAX_CACHE = 64
_SUMMARY_MAX_BYTES = 64 * 1024 * 1024
_MAX_PENDING_TOOL_RESULTS = 4096
_UNAVAILABLE_POINTER = "unavailable"
_CACHE_MISS = object()


def _provider_home_path(path):
    real = os.path.realpath(path)
    home = os.path.realpath(os.path.expanduser("~"))
    return any(real == os.path.join(home, name) or real.startswith(os.path.join(home, name) + os.sep)
               for name in (".codex", ".claude"))


def _text(value):
    return value if isinstance(value, str) else ""


def _safe_text(value):
    value = re.sub(r"[\x00-\x1f\x7f]", " ", _text(value))
    return " ".join(value.split())[:2000]


def _summary_text(value, limit=256):
    output, size = [], 0
    for char in _text(value):
        encoded = char.encode("utf-8", "replace")
        if size + len(encoded) > limit:
            break
        output.append(char)
        size += len(encoded)
    return "".join(output)


def _read(path, max_bytes):
    if not isinstance(path, (str, os.PathLike)) or not 0 < max_bytes <= 1024 * 1024:
        return None
    try:
        if _provider_home_path(path):
            return None
        info = os.lstat(path)
        if (not stat.S_ISREG(info.st_mode) or
                hasattr(os, "getuid") and info.st_uid != os.getuid()):
            return None
        key = (os.path.abspath(path), info.st_size, info.st_mtime_ns if hasattr(info, "st_mtime_ns") else info.st_mtime)
        with _CACHE_LOCK:
            cached = _CACHE.get(key, _CACHE_MISS)
        if cached is not _CACHE_MISS:
            return cached
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            opened = os.fstat(fd)
            if not stat.S_ISREG(opened.st_mode) or opened.st_ino != info.st_ino:
                return None
            size = opened.st_size
            os.lseek(fd, max(0, size - max_bytes), os.SEEK_SET)
            chunks, remaining = [], min(size, max_bytes)
            while remaining:
                chunk = os.read(fd, min(65536, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            raw = b"".join(chunks)
        finally:
            os.close(fd)
        if size > max_bytes:
            raw = raw.split(b"\n", 1)[-1]
        result = raw.decode("utf-8", "replace")
        with _CACHE_LOCK:
            if len(_CACHE) >= _MAX_CACHE:
                _CACHE.clear()
            _CACHE[key] = result
        return result
    except (OSError, TypeError, ValueError):
        return None


def _event(kind, text, ok=None):
    text = _safe_text(text)
    return {"kind": kind, "text": text, "ok": ok} if text else None


def _command_summary(event):
    text = event["text"]
    match = re.match(r"^(.*) \((-?\d+)\)$", text)
    code = int(match.group(2)) if match else None
    return {"text": _summary_text(match.group(1) if match else text),
            "exit_code": code, "ok": event["ok"]}


def _codex(doc):
    result = []
    if doc.get("type") != "item.completed":
        return result
    item = doc.get("item")
    if not isinstance(item, dict):
        return result
    kind = item.get("type")
    if kind == "agent_message":
        event = _event("say", item.get("text"))
        return [event] if event else []
    if kind in ("reasoning", "summary"):
        event = _event("think", item.get("text") or item.get("summary"))
        return [event] if event else []
    if kind == "command_execution":
        code = item.get("exit_code")
        command = _text(item.get("command"))
        event = _event("run", command + (" (" + str(code) + ")" if type(code) is int else ""),
                       code == 0 if type(code) is int else None)
        return [event] if event else []
    if kind == "file_change":
        changes = item.get("changes")
        changes = changes if isinstance(changes, list) else []
        paths = [c.get("path") for c in changes if isinstance(c, dict) and isinstance(c.get("path"), str)]
        return [event for path in paths if (event := _event("edit", path))]
    if kind in ("mcp_tool_call", "web_search"):
        event = _event("tool", item.get("name") or item.get("tool") or kind)
        return [event] if event else []
    if isinstance(item.get("name") or item.get("tool"), str):
        event = _event("tool", item.get("name") or item.get("tool"))
        return [event] if event else []
    return result


def _claude(doc):
    result = []
    if doc.get("type") != "assistant":
        return result
    message = doc.get("message")
    subagent = bool(doc.get("parent_tool_use_id") or
                    isinstance(message, dict) and message.get("parent_tool_use_id"))
    content = message.get("content", []) if isinstance(message, dict) else doc.get("content", [])
    if not isinstance(content, list):
        return result
    for block in content:
        if not isinstance(block, dict):
            continue
        kind = block.get("type")
        if kind == "text":
            event = _event("say", block.get("text"))
        elif kind == "thinking":
            event = _event("think", block.get("thinking"))
        elif kind == "tool_use":
            name, inputs = block.get("name"), block.get("input")
            inputs = inputs if isinstance(inputs, dict) else {}
            path = inputs.get("file_path")
            if name in ("Edit", "Write", "MultiEdit") and isinstance(path, str):
                paths = [path]
                if name == "MultiEdit":
                    edits = inputs.get("edits", [])
                    edits = edits if isinstance(edits, list) else []
                    paths.extend(edit.get("file_path") for edit in edits
                                 if isinstance(edit, dict) and isinstance(edit.get("file_path"), str))
                for target in dict.fromkeys(paths):
                    event = _event("edit", target)
                    if event:
                        if subagent:
                            event["text"] = "sub: " + event["text"]
                        result.append(event)
                continue
            elif name in ("Bash", "bash"):
                event = _event("run", inputs.get("command"))
                if event and isinstance(block.get("id"), str):
                    event["_tool_use_id"] = block["id"]
            else:
                event = _event("tool", name)
        else:
            event = None
        if event:
            if subagent:
                event["text"] = "sub: " + event["text"]
            result.append(event)
    return result


def _document_events(doc):
    if doc.get("type") in ("item.completed", "turn.completed"):
        return _codex(doc), []
    if doc.get("type") == "assistant":
        return _claude(doc), []
    if doc.get("type") != "user":
        return [], []
    message = doc.get("message")
    content = message.get("content", []) if isinstance(message, dict) else doc.get("content", [])
    results = []
    for block in content if isinstance(content, list) else []:
        if (isinstance(block, dict) and block.get("type") == "tool_result" and
                isinstance(block.get("tool_use_id"), str) and type(block.get("is_error")) is bool):
            results.append((block["tool_use_id"], not block["is_error"]))
    return [], results


def events(path, max_bytes=262144):
    """Return normalized events from a safe regular file, parsing only its bounded tail."""
    raw = _read(path, max_bytes)
    if raw is None:
        return []
    output = []
    tool_calls = {}
    for line in raw.splitlines():
        try:
            doc = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(doc, dict):
            continue
        parsed, results = _document_events(doc)
        for event in parsed:
            ident = event.pop("_tool_use_id", None)
            if ident:
                tool_calls[ident] = len(output)
            output.append(event)
        for ident, ok in results:
            index = tool_calls.get(ident)
            if index is not None and output[index].get("kind") == "run":
                output[index]["ok"] = ok
    return output


def _summarize_stream(path):
    """Summarize a complete stream incrementally, with fixed-size retained state."""
    if not isinstance(path, (str, os.PathLike)) or _provider_home_path(path):
        raise OSError("unsafe activity stream")
    info = os.lstat(path)
    if (not stat.S_ISREG(info.st_mode) or
            hasattr(os, "getuid") and info.st_uid != os.getuid()):
        raise OSError("unsafe activity stream")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(fd)
        if (not stat.S_ISREG(opened.st_mode) or opened.st_ino != info.st_ino or
                hasattr(os, "getuid") and opened.st_uid != os.getuid()):
            raise OSError("activity stream changed while opening")
        stream_size = opened.st_size
        byte_limit = min(stream_size, _SUMMARY_MAX_BYTES)
        files, file_set, commands, said = [], set(), [], []
        command_counts = {"passed": 0, "failed": 0}
        pending_tools = {}
        tracking_partial = False
        recorded = False

        def add_event(event):
            nonlocal recorded, tracking_partial
            recorded = True
            kind = event["kind"]
            if kind == "edit":
                target = event["text"]
                if target not in file_set and len(files) < 64:
                    file_set.add(target)
                    files.append(_summary_text(target))
            elif kind == "run":
                row = _command_summary(event)
                if row["ok"] is True:
                    command_counts["passed"] += 1
                elif row["ok"] is False:
                    command_counts["failed"] += 1
                ident = event.get("_tool_use_id")
                if isinstance(ident, str):
                    if ident in pending_tools:
                        del pending_tools[ident]
                    elif len(pending_tools) >= _MAX_PENDING_TOOL_RESULTS:
                        pending_tools.pop(next(iter(pending_tools)))
                        tracking_partial = True
                    pending_tools[ident] = None
                if len(commands) < 64:
                    commands.append(row)
                    if isinstance(ident, str):
                        pending_tools[ident] = row
            elif kind == "say":
                said.append(_summary_text(event["text"]))
                del said[:-3]

        def update_tool(ident, ok):
            if ident not in pending_tools:
                return
            row = pending_tools.pop(ident)
            if row is not None:
                old = row.get("ok")
                if old is True:
                    command_counts["passed"] -= 1
                elif old is False:
                    command_counts["failed"] -= 1
                row["ok"] = ok
            if ok:
                command_counts["passed"] += 1
            else:
                command_counts["failed"] += 1

        with os.fdopen(fd, "rb") as source:
            fd = -1
            consumed = 0
            while consumed < byte_limit:
                line = source.readline(byte_limit - consumed)
                if not line:
                    break
                consumed += len(line)
                try:
                    doc = json.loads(line)
                except (ValueError, TypeError):
                    continue
                if not isinstance(doc, dict):
                    continue
                parsed, results = _document_events(doc)
                for event in parsed:
                    add_event(event)
                for ident, ok in results:
                    update_tool(ident, ok)
        return {
            "recorded": recorded,
            "files": files,
            "commands": commands,
            "command_counts": command_counts,
            "say": said,
            "partial": "; ".join(partial for partial in (
                ("first %d MiB" % ((_SUMMARY_MAX_BYTES + 1024 * 1024 - 1) // (1024 * 1024))
                 if stream_size > _SUMMARY_MAX_BYTES else None),
                ("tool result tracking overflow after %d ids" % _MAX_PENDING_TOOL_RESULTS
                 if tracking_partial else None)) if partial),
        }
    finally:
        if fd >= 0:
            os.close(fd)


def _summary_path(repo, call):
    if not isinstance(call, str) or not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call):
        return None
    return os.path.join(repo, ".oms", "panel-state", "dispatch-logs", call + ".activity.json")


def _dispatch_directory(repo, create=False):
    directory = os.path.join(os.path.realpath(repo), ".oms", "panel-state", "dispatch-logs")
    if create:
        os.makedirs(directory, mode=0o700, exist_ok=True)
    info = os.lstat(directory)
    if (not stat.S_ISDIR(info.st_mode) or
            os.name == "posix" and (info.st_mode & 0o077 or
            hasattr(os, "getuid") and info.st_uid != os.getuid())):
        raise OSError("unsafe dispatch output directory")
    return directory


def _write_private(path, data):
    encoded = data.encode("utf-8")
    if len(encoded) > 65536:
        raise OSError("activity summary exceeds its size limit")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        if hasattr(os, "fchmod"):
            os.fchmod(fd, 0o600)
        offset = 0
        while offset < len(encoded):
            written = os.write(fd, encoded[offset:])
            if written <= 0:
                raise OSError("short activity summary write")
            offset += written
    finally:
        os.close(fd)


def record_summary(repo, call, stream, provider="", access=""):
    """Save a compact summary before provider JSONL is converted in place."""
    path = _summary_path(repo, call)
    if path is None:
        return False
    try:
        parsed = _summarize_stream(stream)
        data = {
            "schema": 1,
            "available": not (provider == "claude" and access == "write"),
            "recorded": parsed["recorded"],
            "files": parsed["files"],
            "commands": parsed["commands"],
            "command_counts": parsed["command_counts"],
            "say": parsed["say"],
        }
        if parsed["partial"]:
            data["partial"] = parsed["partial"]
        if not data["available"]:
            data["reason"] = "Live activity not available for Claude write workers"
        encoded = json.dumps(data, ensure_ascii=False, separators=(",", ":")) + "\n"
        directory = _dispatch_directory(repo, create=True)
        _write_private(os.path.join(directory, os.path.basename(path)), encoded)
        return True
    except (OSError, TypeError, ValueError):
        try:
            _write_stream_pointer(repo, call, _UNAVAILABLE_POINTER)
        except (OSError, TypeError, ValueError):
            pass
        return False


def _write_stream_pointer(repo, call, value):
    directory = _dispatch_directory(repo, create=False)
    path = os.path.join(directory, call + ".stream")
    _write_private(path, value + "\n")


def finish_pointer(repo, call):
    """Replace a live output pointer with its retained summary or an unavailable marker."""
    if not isinstance(call, str) or not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call):
        return False
    try:
        directory = _dispatch_directory(repo, create=False)
        summary_file = os.path.join(directory, call + ".activity.json")
        try:
            info = os.lstat(summary_file)
            safe = (stat.S_ISREG(info.st_mode) and info.st_size <= 65536 and
                    (not hasattr(os, "getuid") or info.st_uid == os.getuid()))
        except OSError:
            safe = False
        if safe:
            _write_stream_pointer(repo, call, os.path.realpath(summary_file))
        else:
            _write_stream_pointer(repo, call, _UNAVAILABLE_POINTER)
        return bool(safe)
    except (OSError, TypeError, ValueError):
        try:
            _write_stream_pointer(repo, call, _UNAVAILABLE_POINTER)
        except (OSError, TypeError, ValueError):
            pass
        return False


def read_summary(repo, call):
    """Read only the retained compact summary for a finished call."""
    path = _summary_path(repo, call)
    if path is None:
        return None
    raw = _read(path, 65536)
    try:
        value = json.loads(raw) if raw is not None else None
    except (ValueError, TypeError):
        value = None
    if (not isinstance(value, dict) or value.get("schema") != 1 or
            not isinstance(value.get("available"), bool) or not isinstance(value.get("recorded"), bool)):
        return None
    files = value.get("files") if isinstance(value.get("files"), list) else []
    commands = value.get("commands") if isinstance(value.get("commands"), list) else []
    said = value.get("say") if isinstance(value.get("say"), list) else []
    value["files"] = list(dict.fromkeys(_safe_text(path) for path in files if isinstance(path, str)))[:64]
    value["commands"] = [{"text": _safe_text(row.get("text")),
                           "exit_code": row.get("exit_code") if type(row.get("exit_code")) is int else None,
                           "ok": row.get("ok") if type(row.get("ok")) is bool else None}
                          for row in commands if isinstance(row, dict)][:64]
    counts = value.get("command_counts")
    if (isinstance(counts, dict) and
            all(type(counts.get(key)) is int and counts[key] >= 0 for key in ("passed", "failed"))):
        value["command_counts"] = {key: counts[key] for key in ("passed", "failed")}
    else:
        value["command_counts"] = {
            "passed": sum(row["ok"] is True for row in value["commands"]),
            "failed": sum(row["ok"] is False for row in value["commands"]),
        }
    value["say"] = [_safe_text(line) for line in said if isinstance(line, str)][-3:]
    if isinstance(value.get("reason"), str):
        value["reason"] = _safe_text(value["reason"])
    if isinstance(value.get("partial"), str):
        value["partial"] = _safe_text(value["partial"])
    return value


def summary_lines(value):
    """Format a retained activity summary without inferring missing activity."""
    if not isinstance(value, dict):
        return ["Activity not recorded"]
    if value.get("available") is not True:
        return [value.get("reason") if isinstance(value.get("reason"), str)
                else "Live activity not available for Claude write workers"]
    if value.get("recorded") is not True:
        return ["Activity not recorded"]
    files = value.get("files") if isinstance(value.get("files"), list) else []
    commands = value.get("commands") if isinstance(value.get("commands"), list) else []
    said = value.get("say") if isinstance(value.get("say"), list) else []
    counts = value.get("command_counts") if isinstance(value.get("command_counts"), dict) else {}
    passed = counts.get("passed") if type(counts.get("passed")) is int else sum(
        isinstance(row, dict) and row.get("ok") is True for row in commands)
    failed = counts.get("failed") if type(counts.get("failed")) is int else sum(
        isinstance(row, dict) and row.get("ok") is False for row in commands)
    lines = []
    if isinstance(value.get("partial"), str):
        lines.append("partial: " + value["partial"])
    lines.extend(["Files edited: " + (", ".join(str(path) for path in files) if files else "no paths reported"),
                  "Commands run: %d passed, %d failed" % (passed, failed)])
    for row in commands:
        if not isinstance(row, dict):
            continue
        marker = "✓" if row.get("ok") is True else "✗" if row.get("ok") is False else "·"
        command = row.get("text", "")
        if type(row.get("exit_code")) is int:
            command += " (exit %d)" % row["exit_code"]
        lines.append("$ %s %s" % (marker, command))
    lines.extend("› " + str(line) for line in said[-3:])
    return lines


def _peer_timeout_seconds():
    raw = os.environ.get("OMS_PEER_TIMEOUT")
    if not isinstance(raw, str) or not raw.strip():
        return 2 * 60 * 60
    match = re.fullmatch(r"([0-9]+(?:\.[0-9]+)?)([smhd]?)", raw)
    if not match:
        return 2 * 60 * 60
    value = float(match.group(1))
    scale = {"": 1, "s": 1, "m": 60, "h": 3600, "d": 86400}[match.group(2)]
    seconds = value * scale
    return seconds if math.isfinite(seconds) and seconds > 0 else 2 * 60 * 60


def _live_stream(path, now, wall):
    if not isinstance(path, (str, os.PathLike)) or not os.path.isabs(path) or _provider_home_path(path):
        return False
    try:
        info = os.lstat(path)
        return (stat.S_ISREG(info.st_mode) and
                (not hasattr(os, "getuid") or info.st_uid == os.getuid()) and
                now - info.st_mtime <= wall)
    except (OSError, TypeError, ValueError):
        return False


def _snapshot_call(repo, call, now, wall, running_calls):
    if call not in running_calls:
        return None
    stream = stream_path(repo, call)
    if not stream or stream.endswith(".activity.json") or not _live_stream(stream, now, wall):
        return None
    parsed = events(stream)
    return parsed or None


def snapshot_call(repo, call, running_calls=()):
    """Read a previewed call's stream only while its projected attempt is live and fresh."""
    if not isinstance(call, str) or not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call):
        return []
    try:
        running_calls = frozenset(running_calls)
    except TypeError:
        running_calls = frozenset()
    parsed = _snapshot_call(repo, call, time.time(), _peer_timeout_seconds(), running_calls)
    return parsed or []


def snapshot(repo, max_calls=128, running_calls=()):
    """Read fresh pointers for projected live calls, counting only those calls toward the cap."""
    if type(max_calls) is not int or max_calls <= 0:
        return {}
    try:
        running_calls = frozenset(running_calls)
    except TypeError:
        running_calls = frozenset()
    max_calls = min(max_calls, 128)
    try:
        directory = _dispatch_directory(repo)
        entries = os.scandir(directory)
    except OSError:
        return {}
    result = {}
    scanned = 0
    now = time.time()
    wall = _peer_timeout_seconds()
    with entries:
        for entry in entries:
            match = re.fullmatch(r"(call-[A-Za-z0-9][A-Za-z0-9._-]{0,159})\.stream", entry.name)
            if not match:
                continue
            if match.group(1) not in running_calls:
                continue
            stream = stream_path(repo, match.group(1))
            if not stream or stream.endswith(".activity.json"):
                continue
            if not _live_stream(stream, now, wall):
                continue
            if scanned >= max_calls:
                break
            scanned += 1
            parsed = events(stream)
            if parsed:
                result[match.group(1)] = parsed
    return result


def stream_path(repo, call):
    """Resolve a dispatch stream pointer without following links or entering provider homes."""
    if not isinstance(call, str) or not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call):
        return None
    pointer = os.path.join(repo, ".oms", "panel-state", "dispatch-logs", call + ".stream")
    raw = _read(pointer, 4096)
    if not raw:
        return None
    value = raw.strip()
    if not os.path.isabs(value) or _provider_home_path(value):
        return None
    return value


if __name__ == "__main__":
    import sys
    if len(sys.argv) == 7 and sys.argv[1] == "record":
        raise SystemExit(0 if record_summary(*sys.argv[2:]) else 1)
    if len(sys.argv) == 4 and sys.argv[1] == "finish":
        raise SystemExit(0 if finish_pointer(sys.argv[2], sys.argv[3]) else 1)
    raise SystemExit(2)
