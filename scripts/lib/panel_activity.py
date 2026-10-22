"""Bounded, read-only view of a dispatched provider's native event stream."""

import json
import math
import os
import re
import stat
import subprocess
import tempfile
import threading
import time
import unicodedata
from pathlib import Path

_CACHE = {}
_CACHE_LOCK = threading.Lock()
_MAX_CACHE = 64
_USAGE_CACHE = {}
_MAX_USAGE_CACHE = 128
_MAX_USAGE_MESSAGES = 4096
_USAGE_FILE_BYTES = 8 * 1024 * 1024
_USAGE_REFRESH_BYTES = 16 * 1024 * 1024
_USAGE_MAX_LINE_BYTES = 1024 * 1024
_MAX_USAGE_CALLS = 128
_USAGE_LOCK = threading.RLock()
_USAGE_REFRESH = threading.local()
_SUMMARY_MAX_BYTES = 64 * 1024 * 1024
_MAX_PENDING_TOOL_RESULTS = 4096
_UNAVAILABLE_POINTER = "unavailable"
_CACHE_MISS = object()
_TRANSCRIPT_BYTES = 8 * 1024 * 1024
_TRANSCRIPT_KEEP = 128
_TRANSCRIPT_DAYS = 30
_TRANSCRIPT_MARKER = b'{"oms_transcript_truncated":true}\n'


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


def _usage(doc):
    """Normalize one provider usage event without combining per-message totals."""
    payload = doc.get("payload")
    if doc.get("type") == "event_msg" and isinstance(payload, dict):
        doc = payload
    kind = doc.get("type")
    if kind == "token_count":
        info = doc.get("info")
        usage = info.get("total_token_usage") if isinstance(info, dict) else None
        usage = usage if isinstance(usage, dict) else info
        if isinstance(usage, dict) and "input_tokens" in usage and "output_tokens" in usage:
            return ("codex", {"input_tokens": usage.get("input_tokens"), "output_tokens": usage.get("output_tokens"),
                              "cache_in_input": True}, None)
    if kind == "turn.completed":
        usage = doc.get("usage")
        if isinstance(usage, dict):
            total = usage.get("total_token_usage")
            usage = total if isinstance(total, dict) else usage
            if "input_tokens" in usage and "output_tokens" in usage:
                return ("codex", {"input_tokens": usage.get("input_tokens"), "output_tokens": usage.get("output_tokens"),
                                  "cache_in_input": True}, None)
    if kind in ("assistant", "result"):
        message = doc.get("message")
        usage = message.get("usage") if isinstance(message, dict) else None
        usage = usage if isinstance(usage, dict) else doc.get("usage")
        if isinstance(usage, dict) and "input_tokens" in usage and "output_tokens" in usage:
            normalized = {"input_tokens": usage.get("input_tokens"), "output_tokens": usage.get("output_tokens"),
                          "cache_read_tokens": usage.get("cache_read_input_tokens", 0),
                          "cache_write_tokens": usage.get("cache_creation_input_tokens", 0),
                          "cache_in_input": False}
            if kind == "result":
                return ("claude-result", normalized, None)
            ident = message.get("id") if isinstance(message, dict) else None
            ident = ident or doc.get("message_id")
            return ("claude-message", normalized, ident if isinstance(ident, str) and ident else None)
    return None


def _claude_usage_total(messages):
    keys = ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens")
    totals = dict.fromkeys(keys, 0)
    for usage in messages.values():
        for key in keys:
            value = usage.get(key)
            if type(value) is not int or value < 0:
                return None
            totals[key] += value
    return dict(totals, cache_in_input=False) if messages else None


def _begin_usage_refresh():
    """Reset live usage parsing limits and generation cache for one panel render."""
    _USAGE_REFRESH.budget = [_USAGE_REFRESH_BYTES]
    _USAGE_REFRESH.files = set()
    _USAGE_REFRESH.parsed = {}


def _usage_refresh():
    if not hasattr(_USAGE_REFRESH, "budget"):
        _begin_usage_refresh()
    return _USAGE_REFRESH


def _new_usage_state(info):
    return {
        "id": (info.st_dev, info.st_ino),
        "mtime": info.st_mtime_ns if hasattr(info, "st_mtime_ns") else info.st_mtime,
        "offset": 0, "pending": b"", "discard_line": False,
        "messages": {}, "message_overflow": False, "incomplete": False, "claude_result": None,
        "codex_total_seen": False, "codex_total": None, "codex_fallback": None,
        "codex_turns": 0, "codex_turns_valid": True, "codex_turn_input": 0, "codex_turn_output": 0,
    }


def _read_usage(path, limit, start, expect):
    """Read appended bytes from one own-uid, unlinked regular stream without following links."""
    if _provider_home_path(path):
        raise OSError("unsafe provider stream")
    info = os.lstat(path)
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or
            hasattr(os, "getuid") and info.st_uid != os.getuid()):
        raise OSError("unsafe provider stream")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(fd, "rb") as source:
        opened = os.fstat(source.fileno())
        if (not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1 or
                (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino) or
                (opened.st_dev, opened.st_ino) != expect or
                hasattr(os, "getuid") and opened.st_uid != os.getuid()):
            raise OSError("provider stream changed while opening")
        source.seek(start)
        return source.read(limit)


def _record_usage(entry, doc):
    usage_doc = doc.get("payload") if doc.get("type") == "event_msg" else doc
    if not isinstance(usage_doc, dict):
        usage_doc = doc
    kind = usage_doc.get("type")
    event = _usage(doc)
    if kind == "token_count":
        info = usage_doc.get("info")
        total = info.get("total_token_usage") if isinstance(info, dict) else None
        if isinstance(total, dict):
            entry["codex_total_seen"] = True
            entry["codex_total"] = event[1] if event and event[0] == "codex" else None
        elif event and event[0] == "codex":
            entry["codex_fallback"] = event[1]
    elif kind == "turn.completed":
        raw = usage_doc.get("usage")
        total = raw.get("total_token_usage") if isinstance(raw, dict) else None
        if isinstance(total, dict):
            entry["codex_total_seen"] = True
            entry["codex_total"] = event[1] if event and event[0] == "codex" else None
        else:
            entry["codex_turns"] += 1
            if not event or event[0] != "codex":
                entry["codex_turns_valid"] = False
            else:
                usage = event[1]
                input_tokens, output_tokens = usage.get("input_tokens"), usage.get("output_tokens")
                if (type(input_tokens) is not int or input_tokens < 0 or
                        type(output_tokens) is not int or output_tokens < 0):
                    entry["codex_turns_valid"] = False
                else:
                    entry["codex_turn_input"] += input_tokens
                    entry["codex_turn_output"] += output_tokens
    elif event:
        provider, usage, ident = event
        if provider == "codex":
            entry["codex_fallback"] = usage
        elif provider == "claude-result":
            entry["claude_result"] = usage
        elif provider == "claude-message":
            if ident is None:
                entry["incomplete"] = True
            elif ident in entry["messages"]:
                entry["messages"][ident] = usage
            elif len(entry["messages"]) < _MAX_USAGE_MESSAGES:
                entry["messages"][ident] = usage
            else:
                entry["message_overflow"] = True


def _consume_usage(entry, data):
    if entry["discard_line"]:
        newline = data.find(b"\n")
        if newline < 0:
            return
        data = data[newline + 1:]
        entry["discard_line"] = False
    lines = (entry["pending"] + data).split(b"\n")
    entry["pending"] = lines.pop()
    for line in lines:
        if len(line) > _USAGE_MAX_LINE_BYTES:
            continue
        try:
            doc = json.loads(line)
        except (ValueError, TypeError, RecursionError):
            continue
        if isinstance(doc, dict):
            _record_usage(entry, doc)
    if len(entry["pending"]) > _USAGE_MAX_LINE_BYTES:
        entry["pending"] = b""
        entry["discard_line"] = True


def _usage_total(entry):
    if entry["claude_result"] is not None:
        return entry["claude_result"]
    if not entry["message_overflow"] and not entry["incomplete"]:
        total = _claude_usage_total(entry["messages"])
        if total is not None:
            return total
    if entry["codex_total_seen"]:
        return entry["codex_total"]
    if entry["codex_turns"] and entry["codex_turns_valid"]:
        return {"input_tokens": entry["codex_turn_input"],
                "output_tokens": entry["codex_turn_output"], "cache_in_input": True}
    if not entry["codex_turns"]:
        return entry["codex_fallback"]
    return None


def live_usage(repo, call):
    """Return live usage while parsing only bounded, newly appended stream bytes."""
    path = stream_path(repo, call)
    if not path or path.endswith(".activity.json") or not _live_stream(path, time.time(), _peer_timeout_seconds()):
        return None
    path = os.path.abspath(path)
    try:
        info = os.lstat(path)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or
                hasattr(os, "getuid") and info.st_uid != os.getuid()):
            raise OSError("unsafe provider stream")
    except (OSError, TypeError, ValueError):
        with _USAGE_LOCK:
            for old_key in [key for key in _USAGE_CACHE if key[0] == path]:
                _USAGE_CACHE.pop(old_key, None)
        return None
    ident = (info.st_dev, info.st_ino)
    mtime = info.st_mtime_ns if hasattr(info, "st_mtime_ns") else info.st_mtime
    generation = (path, ident[0], ident[1], info.st_size, mtime)
    refresh = _usage_refresh()
    with _USAGE_LOCK:
        if generation in refresh.parsed:
            return refresh.parsed[generation]
        key = (path, ident[0], ident[1])
        for old_key in [item for item in _USAGE_CACHE if item[0] == path and item != key]:
            _USAGE_CACHE.pop(old_key, None)
        entry = _USAGE_CACHE.get(key)
        if entry and (info.st_size < entry["offset"] or mtime < entry["mtime"]):
            entry = None
            _USAGE_CACHE.pop(key, None)
        if entry is None:
            if len(_USAGE_CACHE) >= _MAX_USAGE_CACHE:
                _USAGE_CACHE.pop(next(iter(_USAGE_CACHE)))
            entry = _USAGE_CACHE[key] = _new_usage_state(info)
        if key not in refresh.files and len(refresh.files) >= _MAX_USAGE_CALLS:
            result = _usage_total(entry)
            if len(refresh.parsed) < _MAX_USAGE_CACHE:
                refresh.parsed[generation] = result
            return result
        refresh.files.add(key)
        if entry["offset"] < info.st_size and refresh.budget[0] > 0:
            limit = min(_USAGE_FILE_BYTES, refresh.budget[0])
            try:
                data = _read_usage(path, limit, entry["offset"], ident)
            except (OSError, TypeError, ValueError):
                if len(refresh.parsed) < _MAX_USAGE_CACHE:
                    refresh.parsed[generation] = None
                return None
            refresh.budget[0] -= len(data)
            _consume_usage(entry, data)
            entry["offset"] += len(data)
            try:
                current = os.lstat(path)
            except OSError:
                _USAGE_CACHE.pop(key, None)
                return None
            if ((current.st_dev, current.st_ino) != ident or current.st_size < entry["offset"]):
                _USAGE_CACHE.pop(key, None)
                return None
            entry["mtime"] = (current.st_mtime_ns if hasattr(current, "st_mtime_ns") else current.st_mtime)
        result = _usage_total(entry)
        if len(refresh.parsed) < _MAX_USAGE_CACHE:
            refresh.parsed[generation] = result
        return result


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
        summary = {
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
        return summary
    finally:
        if fd >= 0:
            os.close(fd)


def _summary_path(repo, call):
    if not isinstance(call, str) or not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}", call):
        return None
    return os.path.join(repo, ".oms", "panel-state", "dispatch-logs", call + ".activity.json")


def _dispatch_directory(repo, create=False):
    directory = os.path.realpath(repo)
    for name in (".oms", "panel-state", "dispatch-logs"):
        directory = os.path.join(directory, name)
        if _provider_home_path(directory):
            raise OSError("unsafe dispatch output directory")
        if create:
            try:
                os.mkdir(directory, mode=0o700)
            except FileExistsError:
                pass
        if not stat.S_ISDIR(os.lstat(directory).st_mode):
            raise OSError("unsafe dispatch output directory")
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


def _transcript_tail(path):
    if _provider_home_path(path):
        raise OSError("unsafe transcript source")
    info = os.lstat(path)
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or
            hasattr(os, "getuid") and info.st_uid != os.getuid()):
        raise OSError("unsafe transcript source")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(fd, "rb") as source:
        opened = os.fstat(source.fileno())
        if (not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1 or
                (opened.st_dev, opened.st_ino) != (info.st_dev, info.st_ino)):
            raise OSError("transcript source changed")
        size = opened.st_size
        start = max(0, size - (_TRANSCRIPT_BYTES - len(_TRANSCRIPT_MARKER))) if size > _TRANSCRIPT_BYTES else 0
        source.seek(start)
        raw = source.read(min(size, _TRANSCRIPT_BYTES))
        if start:
            # The first retained byte may be inside a JSON document or a UTF-8 character.
            raw = _TRANSCRIPT_MARKER + raw.partition(b"\n")[2]
        return raw


def _retain_transcript(repo, call, stream):
    raw = _transcript_tail(stream)
    directory = _dispatch_directory(repo, create=True)
    target = os.path.join(directory, call + ".transcript")
    try:
        old = os.lstat(target)
    except FileNotFoundError:
        old = None
    if old is not None and (not stat.S_ISREG(old.st_mode) or old.st_nlink != 1 or
                           hasattr(os, "getuid") and old.st_uid != os.getuid()):
        raise OSError("unsafe retained transcript")
    fd, temporary = tempfile.mkstemp(prefix=".transcript-", dir=directory)
    try:
        with os.fdopen(fd, "wb") as destination:
            destination.write(raw)
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    prune_transcripts(repo)


def prune_transcripts(repo):
    """Bound completed captures only; live pointers and retained summaries are untouched."""
    directory = _dispatch_directory(repo)
    rows = []
    with os.scandir(directory) as entries:
        for entry in entries:
            if not re.fullmatch(r"call-[A-Za-z0-9][A-Za-z0-9._-]{0,159}\.transcript", entry.name):
                continue
            try:
                info = entry.stat(follow_symlinks=False)
            except FileNotFoundError:
                continue
            if (stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and
                    (not hasattr(os, "getuid") or info.st_uid == os.getuid())):
                rows.append((info.st_mtime, entry.path))
    cutoff = time.time() - _TRANSCRIPT_DAYS * 86400
    for index, (stamp, path) in enumerate(sorted(rows, reverse=True)):
        if index >= _TRANSCRIPT_KEEP or stamp < cutoff:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def _transcript_text(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "\n".join(_transcript_text(item.get("text", "")) for item in value if isinstance(item, dict))
    return json.dumps(value, ensure_ascii=False) if value is not None else ""


def _transcript_documents(raw):
    """Reuse provider event classification while retaining the bodies omitted from activity cards."""
    lines, tools = [], {}
    for raw_line in raw.decode("utf-8", "replace").splitlines():
        try:
            doc = json.loads(raw_line)
        except ValueError:
            continue
        if not isinstance(doc, dict):
            continue
        if doc.get("oms_transcript_truncated"):
            lines.append("[transcript truncated: retained last 8 MiB; earlier events unavailable]")
            continue
        parsed, unused_results = _document_events(doc)
        item = doc.get("item") or {}
        if doc.get("type") == "item.completed" and isinstance(item, dict):
            kind = item.get("type")
            if kind in ("agent_message", "reasoning", "summary"):
                lines.extend(["Assistant:" if kind == "agent_message" else "Reasoning:",
                              _transcript_text(item.get("text") or item.get("summary"))])
            elif kind == "command_execution":
                lines.extend(["Command: " + _text(item.get("command")) + " / exit " + str(item.get("exit_code", "unknown")),
                              _transcript_excerpt(item.get("aggregated_output"))])
            elif kind == "file_change":
                for change in item.get("changes", []) if isinstance(item.get("changes"), list) else []:
                    if isinstance(change, dict):
                        lines.extend(["Edit: " + _text(change.get("path")), _transcript_text(change.get("diff") or change.get("patch"))])
            else:
                for event in parsed:
                    lines.append("Tool: " + event["text"] + " / status " + str(item.get("status", "unknown")))
                if parsed:
                    lines.extend([_transcript_text(item.get("arguments")), _transcript_excerpt(item.get("result") or item.get("error"))])
        elif doc.get("type") in ("assistant", "user"):
            message = doc.get("message")
            content = message.get("content", []) if isinstance(message, dict) else doc.get("content", [])
            for block in content if isinstance(content, list) else []:
                if not isinstance(block, dict):
                    continue
                kind = block.get("type")
                if kind in ("text", "thinking") and doc.get("type") == "assistant":
                    lines.extend(["Assistant:" if kind == "text" else "Reasoning:", _text(block.get("text") or block.get("thinking"))])
                elif kind == "tool_use":
                    inputs = block.get("input")
                    inputs = inputs if isinstance(inputs, dict) else {}
                    name = block.get("name")
                    prefix = "Command: " + _text(inputs.get("command")) if name in ("Bash", "bash") else "Tool: " + str(name)
                    tools[block.get("id")] = len(lines)
                    lines.append(prefix + " / status pending")
                    if name in ("Edit", "Write", "MultiEdit"):
                        lines.append("Edit: " + _text(inputs.get("file_path")))
                        edits = inputs.get("edits", []) if name == "MultiEdit" else [inputs]
                        for edit in edits if isinstance(edits, list) else []:
                            if not isinstance(edit, dict):
                                continue
                            if edit.get("file_path"):
                                lines.append("Edit: " + _text(edit["file_path"]))
                            if name == "Write":
                                lines.append(_text(edit.get("content")))
                            else:
                                lines.extend(["@@ edit @@", "\n".join("-" + line for line in _text(edit.get("old_string")).splitlines()),
                                              "\n".join("+" + line for line in _text(edit.get("new_string")).splitlines())])
                    elif name not in ("Bash", "bash"):
                        lines.append(_transcript_text(inputs))
                elif kind == "tool_result":
                    index = tools.get(block.get("tool_use_id"))
                    result = doc.get("tool_use_result")
                    code = result.get("exit_code") if isinstance(result, dict) else None
                    status = ("exit " + str(code) if type(code) is int else
                              "status failed" if block.get("is_error") is True else
                              "status succeeded" if block.get("is_error") is False else "status unknown")
                    if index is not None:
                        lines[index] = lines[index].removesuffix(" / status pending") + " / " + status
                    lines.extend(["Tool result: " + status, _transcript_excerpt(block.get("content"))])
        elif doc.get("type") == "result" and isinstance(doc.get("result"), str):
            lines.extend(["Final report:", doc["result"]])
    return "\n".join(lines)


def _transcript_excerpt(value):
    text = _transcript_text(value)
    raw = text.encode("utf-8", "replace")
    return raw[:8192].decode("utf-8", "ignore") + ("\n[output excerpt truncated]" if len(raw) > 8192 else "")


def _redact_transcript(text):
    scanner = str(Path(__file__).with_name("agent-memory-common.sh"))
    # Use the outbound scanner's canonical secret patterns; a scanner failure hides the display.
    result = subprocess.run(["bash", "-c", '\n'.join((
        '. "$1" || exit 2',
        'pattern=$(agent_memory_secret_re) || exit 2',
        'matches=$(LC_ALL=C grep -nEi "$pattern")',
        'status=$?',
        '[ "$status" -le 1 ] || exit 2',
        'printf "%s\\n" "$matches" | cut -d: -f1',
    )), "panel-transcript", scanner], input=text, text=True, capture_output=True, timeout=5, check=False)
    if result.returncode:
        raise OSError("transcript secret scanner failed")
    masked = {int(line) for line in result.stdout.splitlines() if line.isdigit()}
    lines, private_block = [], False
    for index, line in enumerate(text.splitlines(), 1):
        if index in masked and ("-----" + "BEGIN") in line:
            private_block = True
        lines.append("[redacted]" if index in masked or private_block else line)
        if private_block and ("-----" + "END") in line:
            private_block = False
    return lines


def transcript_lines(repo, call):
    """Read only an OMS-retained private capture, never a provider home or a live pointer."""
    if _summary_path(repo, call) is None:
        return ["transcript not captured"]
    try:
        path = os.path.join(_dispatch_directory(repo), call + ".transcript")
        info = os.lstat(path)
        if os.name == "posix" and info.st_mode & 0o077:
            raise OSError("unsafe transcript permissions")
        raw = _transcript_tail(path)
        text = _transcript_documents(raw)
        safe = "\n".join("".join("?" if char != "\t" and unicodedata.category(char)[0] == "C" else char
                                for char in line) for line in text.splitlines())
        try:
            return _redact_transcript(safe) or ["No transcript events recorded"]
        except (OSError, subprocess.SubprocessError):
            return ["transcript unavailable: secret scanner failed"]
    except (OSError, ValueError, TypeError, subprocess.SubprocessError):
        return ["transcript not captured"]


def record_summary(repo, call, stream, provider="", access=""):
    """Save a compact summary before provider JSONL is converted in place."""
    path = _summary_path(repo, call)
    if path is None:
        return False
    try:
        parsed = _summarize_stream(stream)
        _retain_transcript(repo, call, stream)
        data = {
            "schema": 1,
            "available": True,
            "recorded": parsed["recorded"],
            "files": parsed["files"],
            "commands": parsed["commands"],
            "command_counts": parsed["command_counts"],
            "say": parsed["say"],
        }
        if parsed["partial"]:
            data["partial"] = parsed["partial"]
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
    if isinstance(value.get("partial"), str):
        value["partial"] = _safe_text(value["partial"])
    return value


def summary_lines(value):
    """Format a retained activity summary without inferring missing activity."""
    if not isinstance(value, dict):
        return ["Activity not recorded"]
    if value.get("available") is not True:
        return ["Activity not recorded"]
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
