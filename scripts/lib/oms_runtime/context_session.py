"""Bounded, read-only accounting of what filled a session: OMS's own hook
injections (from the hook event ledger) and one transcript's tool results.

Sizes are UTF-8 bytes. Nothing here is a tokenizer, a bill or the provider's
view of its context, and no injected or transcript text is returned."""
from __future__ import annotations

import json
import os
from pathlib import Path
import stat

from .context_inspect import InspectionError, _bounded

MAX_LEDGER_BYTES = 64 * 1024 * 1024
MAX_TRANSCRIPT_BYTES = 256 * 1024 * 1024
MAX_LINE_BYTES = 16 * 1024 * 1024
MAX_KEYS = 4096
# Hooks whose output reaches the model without passing the ledger today.
UNCOUNTED = ["skill-router state hints", "fail-ledger hook advice", "syntax-guard hook findings",
             "git-move-guard refusals", "provider-native context (rules, skills, tool output)"]


def _limit(limit):
    if type(limit) is not int or not 1 <= limit <= 100:
        raise InspectionError("--limit must be between 1 and 100")
    return limit


def _lines(path, cap, label):
    """Yield the lines of a regular file no larger than `cap`, opened without following a final symlink."""
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0))
    except OSError:
        raise InspectionError("%s is missing or unreadable" % label) from None
    with os.fdopen(fd, "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise InspectionError("%s is not a regular file" % label)
        if info.st_size > cap:
            raise InspectionError("%s is larger than %d bytes" % (label, cap))
        while True:
            line = handle.readline(MAX_LINE_BYTES + 1)
            if not line:
                return
            if len(line) > MAX_LINE_BYTES:
                # An oversized line is counted as unread; skip to its end.
                while line and not line.endswith(b"\n"):
                    line = handle.readline(MAX_LINE_BYTES + 1)
                yield None
                continue
            yield line


def _count(value):
    return value if type(value) is int and 0 <= value <= (1 << 40) else None


def _label(value, limit=80):
    if not isinstance(value, str) or not value or len(value) > limit:
        return None
    return value if all(ch.isalnum() or ch in "_.:+() /-" for ch in value) else None


def inspect_injections(repo, *, session=None, limit=10):
    """What OMS hooks printed into sessions of this repository, by hook kind and by session."""
    limit = _limit(limit)
    if session is not None and _label(session, 64) is None:
        raise InspectionError("--session-hash must be a recorded session hash")
    path = Path(repo) / ".oms" / "hooks" / "events.jsonl"
    kinds, sessions, sources = {}, {}, {}
    rows = invalid = 0
    if path.exists() or path.is_symlink():
        for line in _lines(str(path), MAX_LEDGER_BYTES, "hook event ledger"):
            try:
                row = json.loads(line) if line is not None else None
            except (TypeError, ValueError):
                row = None
            if not isinstance(row, dict):
                invalid += 1
                continue
            if row.get("action") != "injection":
                continue
            kind, size, owner = _label(row.get("status"), 40), _count(row.get("bytes")), _label(row.get("session"), 64)
            if kind is None or not size:
                invalid += 1
                continue
            if session is not None and owner != session:
                continue
            rows += 1
            for table, key in ((kinds, kind), (sessions, owner or "unknown"),
                               (sources, _label(row.get("source"), 20) or "unrecorded")):
                if key not in table and len(table) >= MAX_KEYS:
                    key = "other"
                entry = table.setdefault(key, {"calls": 0, "bytes": 0, "largest": 0})
                entry["calls"] += 1
                entry["bytes"] += size
                entry["largest"] = max(entry["largest"], size)

    def ranked(table, name):
        return _bounded([dict({name: key}, **value) for key, value in
                         sorted(table.items(), key=lambda item: (-item[1]["bytes"], item[0]))], limit)

    return {
        "schema": 1, "kind": "oms-context-injections", "unit": "utf-8 bytes",
        "ledger": ".oms/hooks/events.jsonl", "session": session,
        "injections": rows, "invalid_rows": invalid,
        "bytes": sum(value["bytes"] for value in kinds.values()),
        "by_hook": ranked(kinds, "hook"),
        "by_session_start_source": ranked(sources, "source"),
        "by_session": ranked(sessions, "session"),
        "not_counted": UNCOUNTED,
        "note": ("Bytes OMS hooks printed for the model, recorded without their text. "
                 "Not tokens, not a bill and not the provider's context window; "
                 "rows older than the hook ledger's retention are gone."),
    }


def _result_bytes(content):
    if isinstance(content, str):
        return len(content.encode("utf-8", errors="replace"))
    total = 0
    if isinstance(content, list):
        for item in content:
            if isinstance(item, dict):
                text = item.get("text")
                if isinstance(text, str):
                    total += len(text.encode("utf-8", errors="replace"))
                elif item.get("type") == "image":
                    total += 0  # binary payloads are not text the summary keeps; counted as a result only
            elif isinstance(item, str):
                total += len(item.encode("utf-8", errors="replace"))
    return total


def _repo_label(repo, raw):
    """A path the tool read, as a repository-relative label; None when it is outside the repository."""
    if not isinstance(raw, str) or not raw or len(raw) > 4096 or any(ord(ch) < 32 for ch in raw):
        return None
    try:
        root = os.path.realpath(repo)
        full = os.path.realpath(raw if os.path.isabs(raw) else os.path.join(root, raw))
        relative = os.path.relpath(full, root)
    except (OSError, ValueError):
        return None
    if relative == os.pardir or relative.startswith(os.pardir + os.sep) or os.path.isabs(relative):
        return None
    return relative.replace(os.sep, "/")


def inspect_session(repo, transcript, *, limit=10):
    """Per-tool result bytes and repeated file reads in one Claude Code JSONL transcript."""
    limit = _limit(limit)
    if not isinstance(transcript, str) or not transcript:
        raise InspectionError("--session needs a transcript path")
    calls, tools, reads = {}, {}, {}
    outside = lines = unread = results = unmatched = 0
    for line in _lines(os.path.expanduser(transcript), MAX_TRANSCRIPT_BYTES, "transcript"):
        lines += 1
        try:
            row = json.loads(line) if line is not None else None
        except (TypeError, ValueError):
            row = None
        message = row.get("message") if isinstance(row, dict) else None
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(row, dict):
            unread += 1
            continue
        if not isinstance(content, list):
            continue
        for item in content:
            if not isinstance(item, dict):
                continue
            if item.get("type") == "tool_use" and isinstance(item.get("id"), str) and len(calls) < 1 << 20:
                name = _label(item.get("name")) or "unnamed"
                target = item.get("input") if isinstance(item.get("input"), dict) else {}
                calls[item["id"]] = name
                if name in ("Read", "NotebookRead"):
                    label = _repo_label(repo, target.get("file_path") or target.get("notebook_path"))
                    if label is None:
                        outside += 1
                    elif label in reads or len(reads) < MAX_KEYS:
                        reads[label] = reads.get(label, 0) + 1
            elif item.get("type") == "tool_result":
                results += 1
                name = calls.get(item.get("tool_use_id"))
                if name is None:
                    unmatched += 1
                    name = "unmatched"
                size = _result_bytes(item.get("content"))
                if name not in tools and len(tools) >= MAX_KEYS:
                    name = "other"
                entry = tools.setdefault(name, {"results": 0, "bytes": 0, "largest": 0})
                entry["results"] += 1
                entry["bytes"] += size
                entry["largest"] = max(entry["largest"], size)
    recognized = bool(calls) or bool(results)
    repeated = sorted(((count, path) for path, count in reads.items() if count > 1), key=lambda item: (-item[0], item[1]))
    return {
        "schema": 1, "kind": "oms-context-session", "unit": "utf-8 bytes",
        "format": "claude-jsonl" if recognized else "unknown",
        "lines": lines, "unread_lines": unread,
        "tool_calls": len(calls), "tool_results": results, "unmatched_results": unmatched,
        "result_bytes": sum(value["bytes"] for value in tools.values()),
        "by_tool": _bounded([dict({"tool": key}, **value) for key, value in
                             sorted(tools.items(), key=lambda item: (-item[1]["bytes"], item[0]))], limit),
        "file_reads": {"distinct_in_repository": len(reads), "outside_repository": outside,
                       "repeated": _bounded([{"path": path, "reads": count} for count, path in repeated], limit)},
        "note": ("Tool-result text as it was recorded, in bytes; the provider may have cleared or summarized "
                 "it since. Not tokens, not a bill and not the current context window. Only Claude Code "
                 "JSONL transcripts are read; any other file reports format unknown."),
    }
