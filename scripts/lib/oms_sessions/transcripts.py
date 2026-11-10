"""Bounded, repository-scoped native JSONL discovery and exact local origins."""
from __future__ import annotations

import json
import os
from pathlib import Path

from oms_runtime.common import CoreError, sha256_bytes
from .common import (
    base_record, excerpt, ident, label, load, owned_bytes, plain_chain, save,
)

PROVIDERS = ("claude", "codex", "pi")
MAX_ENTRIES = 8192
MAX_FILES = 4096
MAX_HEADER_BYTES = 64 * 1024
MAX_TRANSCRIPT_BYTES = 32 * 1024 * 1024
MAX_SEARCH_BYTES = 64 * 1024 * 1024
MAX_LINE_BYTES = 1024 * 1024


def roots(provider="all", supplied=()):
    wanted = PROVIDERS if provider == "all" else (provider,)
    if supplied:
        result = []
        for value in supplied:
            if "=" in value and value.split("=", 1)[0] in PROVIDERS:
                owner, raw = value.split("=", 1)
            elif provider != "all":
                owner, raw = provider, value
            else:
                raise CoreError("with --provider all, use --root PROVIDER=PATH")
            if owner not in wanted:
                raise CoreError("--root provider conflicts with --provider")
            path = Path(raw).expanduser().absolute()
            if path.resolve() in (Path(path.anchor), Path.home().resolve()):
                raise CoreError("choose a transcript directory, not HOME or the filesystem root")
            plain_chain(path, directory=True)
            if not path.is_dir():
                raise CoreError("explicit transcript root is not a directory")
            result.append((owner, path, 4))
        return result
    codex = Path(os.environ.get("CODEX_HOME") or os.environ.get("OMS_CODEX_HOME") or Path.home() / ".codex")
    claude = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
    pi = Path(os.environ.get("PI_CODING_AGENT_SESSION_DIR") or
              Path(os.environ.get("PI_CODING_AGENT_DIR") or Path.home() / ".pi" / "agent") / "sessions")
    return [(owner, path.expanduser().absolute(), depth) for owner, path, depth in (
        ("claude", claude / "projects", 1), ("codex", codex / "sessions", 3),
        ("codex", codex / "archived_sessions", 3), ("pi", pi, 1),
    ) if owner in wanted]


def _metadata(data, provider):
    for raw in data.splitlines()[:32]:
        if len(raw) > MAX_HEADER_BYTES:
            break
        try:
            row = json.loads(raw)
        except (ValueError, UnicodeError, RecursionError):
            continue
        if not isinstance(row, dict):
            continue
        if provider == "codex":
            meta = row.get("payload") if row.get("type") == "session_meta" else None
            if not isinstance(meta, dict):
                continue
            sid, cwd = meta.get("id"), meta.get("cwd")
        elif provider == "pi":
            if row.get("type") != "session":
                continue
            sid, cwd = row.get("id"), row.get("cwd")
        else:
            sid, cwd = row.get("sessionId"), row.get("cwd")
        try:
            sid = ident(sid)
            if (not isinstance(cwd, str) or len(cwd) > 4096 or any(ord(ch) < 32 for ch in cwd)
                    or not Path(cwd).is_absolute()):
                continue
            return sid, Path(cwd).resolve()
        except (CoreError, OSError, ValueError):
            continue
    return None


def discover(repo, selected_roots):
    """Read metadata before any messages; a filename alone never binds a repo."""
    from panel_chats import _read_owned
    found, seen = [], set()
    stats = {"entries": 0, "files": 0, "matched_sessions": 0, "skipped": 0, "truncated": False}
    for provider, root, depth in selected_roots:
        try:
            plain_chain(root, directory=True)
        except CoreError:
            stats["skipped"] += 1
            continue
        pending = [(root, 0)]
        while pending:
            folder, level = pending.pop()
            try:
                with os.scandir(folder) as entries:
                    for entry in entries:
                        stats["entries"] += 1
                        if stats["entries"] > MAX_ENTRIES or stats["files"] >= MAX_FILES:
                            stats["truncated"] = True
                            return found, stats
                        if entry.is_dir(follow_symlinks=False) and level < depth:
                            pending.append((Path(entry.path), level + 1))
                        elif entry.name.endswith(".jsonl") and entry.is_file(follow_symlinks=False):
                            stats["files"] += 1
                            path = Path(entry.path)
                            if (provider, str(path)) in seen:
                                continue
                            seen.add((provider, str(path)))
                            try:
                                plain_chain(path)
                                info = path.lstat()
                                if info.st_size > MAX_TRANSCRIPT_BYTES:
                                    stats["skipped"] += 1
                                    continue
                                head = _read_owned(path, MAX_HEADER_BYTES, expect=(info.st_dev, info.st_ino))
                                meta = _metadata(head, provider)
                            except (OSError, CoreError, ValueError):
                                stats["skipped"] += 1
                                continue
                            if meta and meta[1] == repo:
                                found.append({"provider": provider, "session_id": meta[0], "path": str(path),
                                              "root": str(root), "repo": str(repo), "bytes": info.st_size,
                                              "mtime_ns": info.st_mtime_ns})
                                stats["matched_sessions"] += 1
            except FileNotFoundError:
                continue
            except OSError:
                stats["skipped"] += 1
    return sorted(found, key=lambda item: (-item["mtime_ns"], item["path"])), stats


def select(repo, provider, session, supplied=()):
    sid = ident(session)
    items, stats = discover(repo, roots(provider, supplied))
    matches = [item for item in items if item["session_id"] == sid]
    if stats["truncated"]:
        raise CoreError("session discovery is incomplete; select a narrower --root")
    if len(matches) != 1:
        raise CoreError("native session is missing or ambiguous in this repository scope")
    source = matches[0]
    raw = read_source(source)
    source["sha256"] = sha256_bytes(raw)
    return source


def read_source(source):
    provider = source.get("provider")
    if provider not in PROVIDERS:
        raise CoreError("invalid native provider")
    path = plain_chain(Path(source["path"]))
    root = plain_chain(Path(source["root"]), directory=True)
    try:
        path.relative_to(root)
    except ValueError:
        raise CoreError("native transcript escapes its recorded root") from None
    raw = owned_bytes(path, MAX_TRANSCRIPT_BYTES)
    metadata = _metadata(raw[:MAX_HEADER_BYTES], provider)
    if not metadata or metadata != (ident(source["session_id"]), Path(source["repo"])):
        raise CoreError("native transcript identity or repository binding changed")
    return raw


def _parts(row, provider):
    prefix = "/message"
    if provider == "codex":
        if row.get("type") != "response_item":
            return
        message = row.get("payload")
        if not isinstance(message, dict) or message.get("type") != "message":
            return
        prefix = "/payload"
    else:
        if row.get("type") not in (("user", "assistant") if provider == "claude" else ("message",)):
            return
        message = row.get("message")
    if not isinstance(message, dict) or message.get("role") not in ("user", "assistant"):
        return
    content = message.get("content")
    if isinstance(content, str):
        yield 0, prefix + "/content", message["role"], content
    elif isinstance(content, list):
        for index, part in enumerate(content):
            if isinstance(part, dict) and part.get("type") in ("text", "input_text", "output_text"):
                if isinstance(part.get("text"), str):
                    yield index, prefix + "/content/%d/text" % index, message["role"], part["text"]


def messages(source, raw):
    current_cwd = source["repo"]
    for number, line in enumerate(raw.splitlines(), 1):
        if len(line) > MAX_LINE_BYTES:
            continue
        try:
            row = json.loads(line)
        except (ValueError, UnicodeError, RecursionError):
            continue
        if not isinstance(row, dict):
            continue
        if source["provider"] == "codex" and row.get("type") == "turn_context":
            payload = row.get("payload")
            if isinstance(payload, dict) and isinstance(payload.get("cwd"), str):
                current_cwd = payload["cwd"]
        cwd = row.get("cwd", current_cwd)
        if not isinstance(cwd, str) or Path(cwd).resolve() != Path(source["repo"]):
            continue
        if source["provider"] == "claude" and row.get("sessionId", source["session_id"]) != source["session_id"]:
            continue
        for part, pointer, role, text in _parts(row, source["provider"]):
            yield {"line": number, "part": part, "json_pointer": pointer, "role": role,
                   "text": text, "line_sha256": sha256_bytes(line)}


def origin(source, message, start=0, end=None):
    text, redacted = excerpt(message["text"], start)
    return {"provider": source["provider"], "session_id": source["session_id"],
            "path": source["path"], "line": message["line"], "part": message["part"],
            "json_pointer": message["json_pointer"], "role": message["role"],
            "excerpt": text, "excerpt_start": start, "excerpt_end": min(len(message["text"]), start + 400),
            "match_end": end, "redacted": redacted, "line_sha256": message["line_sha256"]}


def search(repo, query, provider="all", supplied=(), limit=20):
    if not query or len(query) > 200:
        raise CoreError("query must contain 1 to 200 characters")
    sources, stats = discover(repo, roots(provider, supplied))
    matches, consumed, unread = [], 0, 0
    for source in sources:
        if consumed + source["bytes"] > MAX_SEARCH_BYTES:
            stats["truncated"] = True
            break
        try:
            raw = read_source(source)
        except CoreError:
            unread += 1
            continue
        consumed += len(raw)
        for message in messages(source, raw):
            # Literal matching preserves character offsets; no regex, stemming,
            # embeddings, or lowercase expansion can change their meaning.
            start = message["text"].find(query)
            if start < 0:
                continue
            if len(matches) >= limit:
                stats["truncated"] = True
                break
            matches.append(origin(source, message, start, start + len(query)))
        if len(matches) >= limit:
            stats["truncated"] = True
            break
    return {"schema": 1, "kind": "oms-session-search", "scope": str(repo), "matches": matches,
            "scan": dict(stats, bytes=consumed, unread_sessions=unread),
            "note": "Local user/assistant text only. Secret-like excerpts are withheld; offsets refer to original text."}


def show(source, line=1, limit=20):
    raw = read_source(source)
    items = []
    for item in messages(source, raw):
        if item["line"] >= line:
            items.append(origin(source, item))
            if len(items) > limit:
                break
    return {"schema": 1, "kind": "oms-session-open", "provider": source["provider"],
            "session_id": source["session_id"], "messages": items[:limit], "truncated": len(items) > limit}


def bookmark(repo, source, line, part=0, title=""):
    raw = read_source(source)
    found = next((item for item in messages(source, raw) if item["line"] == line and item["part"] == part), None)
    if found is None:
        raise CoreError("bookmark origin must be a readable user/assistant text part")
    row = dict(base_record(repo, "bookmark"), label=label(title), source=source, line=line, part=part,
               line_sha256=found["line_sha256"])
    return save(repo, "bookmarks", row)


def bookmarked(repo, key):
    row = load(repo, "bookmarks", key)
    source = row["source"]
    if source.get("repo") != str(repo):
        raise CoreError("bookmark scope changed")
    raw = read_source(source)
    found = next((item for item in messages(source, raw)
                  if item["line"] == row["line"] and item["part"] == row["part"]), None)
    if found is None or found["line_sha256"] != row["line_sha256"]:
        raise CoreError("bookmark is stale: its original line changed; select a new origin")
    return source, origin(source, found)
