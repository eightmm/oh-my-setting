"""Explicit native-chat navigation; never mix transcripts into shared room state."""

from contextlib import closing
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import subprocess
import sys
import time
from urllib.parse import quote

import room
from dashboard_projection import clean
from panel_view import TERMINAL_STATES

SESSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,159}\Z")
UUID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z")
MAX_ENTRIES = 8192
MAX_FILES = 4096
TAIL_BYTES = 512 * 1024
ADVISOR_MAX_AGE = 1800
NATIVE_ACTIVE = 600          # a subagent or workflow row lasts this long after its file was last written
NATIVE_MAX = 32              # newest rows per kind and session
NATIVE_META_BYTES = 4096
CODEX_META_BYTES = 65537     # Codex session_meta may include base_instructions; matches native_index's header bound
WORKFLOW_BYTES = 256 * 1024  # journal tail read for display counts
USAGE_FILE_BYTES = 8 * 1024 * 1024      # appended transcript bytes read per file per refresh
USAGE_REFRESH_BYTES = 16 * 1024 * 1024  # ... and per refresh across every transcript
USAGE_LINE_BYTES = 256 * 1024           # a longer line is an api-request blob, not a message
OVERSIZED_ROLE = re.compile(rb'"(?:role|type)"\s*:\s*"assistant"')
OVERSIZED_USAGE = re.compile(rb'"usage"\s*:\s*')
OVERSIZED_ID = re.compile(rb'"id"\s*:\s*"(msg_[A-Za-z0-9_-]{1,120})"')
USAGE_KEYS = ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")
ADVISOR_MODELS = (("fable", "Fable 5.1"), ("opus", "Opus 5.5"), ("sonnet", "Sonnet 5.5"))
_transcripts = {}
_usage = {}  # transcript path -> read offset, file identity and per-message token totals
_codex_usage_cache = {}  # Codex rollout path -> read offset, file identity and last token_count total
_refresh = [0]  # counts native_subagents() calls, so entries not shown in the latest one are dropped


def parent_only():
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("workers cannot navigate native owner chats")


def native_index(wanted):
    """Resolve enrolled hashes from filenames and first metadata rows only."""
    found = {"codex": {}, "claude": {}}
    if not wanted:
        return found
    try:
        from claude_app_notify import _registry
        for row in _registry():
            sid = row["sessionId"]
            consumer = hashlib.sha256(sid.encode()).hexdigest()[:32]
            if consumer in wanted and SESSION.fullmatch(sid):
                found["claude"].setdefault(consumer, set()).add(sid)
    except (OSError, ValueError):
        pass
    codex = Path(os.environ.get("CODEX_HOME") or os.environ.get("OMS_CODEX_HOME") or Path.home() / ".codex")
    claude = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
    for provider, base, depth in (("codex", codex / "sessions", 3), ("codex", codex / "archived_sessions", 1),
                                  ("claude", claude / "projects", 1)):
        scanned = files = 0
        pending = [(base, 0)]
        while pending and scanned < MAX_ENTRIES and files < MAX_FILES:
            folder, level = pending.pop()
            try:
                if folder.is_symlink() or not folder.is_dir():
                    continue
                with os.scandir(folder) as entries:
                    for entry in entries:
                        scanned += 1
                        if scanned > MAX_ENTRIES or files >= MAX_FILES:
                            break
                        if entry.is_dir(follow_symlinks=False) and level < depth:
                            pending.append((Path(entry.path), level + 1))
                            continue
                        if not entry.name.endswith(".jsonl") or not entry.is_file(follow_symlinks=False):
                            continue
                        files += 1
                        sid = entry.name[:-6][-36:]
                        consumer = hashlib.sha256(sid.encode()).hexdigest()[:32]
                        if not UUID.fullmatch(sid) or consumer not in wanted:
                            continue
                        path = Path(entry.path)
                        info = path.lstat()
                        if info.st_nlink != 1 or hasattr(os, "getuid") and info.st_uid != os.getuid():
                            continue
                        if provider == "codex":
                            try:
                                fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
                                with os.fdopen(fd, "rb") as handle:
                                    actual = os.fstat(handle.fileno())
                                    if not stat.S_ISREG(actual.st_mode) or (actual.st_dev, actual.st_ino) != (info.st_dev, info.st_ino):
                                        continue
                                    line = handle.readline(65537)
                                if len(line) > 65536:
                                    continue
                                meta = json.loads(line)
                            except (OSError, ValueError, RecursionError):
                                continue
                            if (not isinstance(meta, dict) or meta.get("type") != "session_meta"
                                    or not isinstance(meta.get("payload"), dict) or meta["payload"].get("id") != sid):
                                continue
                        found[provider].setdefault(consumer, set()).add(sid)
            except (OSError, ValueError, RecursionError):
                continue
    return found


def _claude_home():
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")


def _read_owned(path, limit, tail=False, start=0, expect=None):
    """Bytes from an own-uid regular file without following links; the last `limit` bytes when `tail`, else
    `limit` bytes from `start`. `expect` is a (device, inode) the file must still be."""
    info = os.lstat(path)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or hasattr(os, "getuid") and info.st_uid != os.getuid():
        raise OSError("unsafe native file")
    fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(fd, "rb") as handle:
        actual = os.fstat(handle.fileno())
        if (not stat.S_ISREG(actual.st_mode) or (actual.st_dev, actual.st_ino) != (info.st_dev, info.st_ino)
                or expect and (actual.st_dev, actual.st_ino) != expect):
            raise OSError("native file changed")
        if tail:
            start = max(0, actual.st_size - limit)
        handle.seek(start)
        data = handle.read(limit)
    if tail and start and b"\n" in data:
        data = data.split(b"\n", 1)[1]  # the cut may land mid-line
    return data


def _advisor_label(home):
    try:
        value = json.loads(_read_owned(home / "settings.json", 1024 * 1024)).get("advisorModel")
    except (OSError, ValueError, AttributeError, RecursionError):
        return "Advisor"
    value = str(value).lower() if isinstance(value, str) else ""
    return next((label for key, label in ADVISOR_MODELS if key in value), "Advisor")


def _claude_mains(members):
    return {m["consumer"]: m["participant"] for m in members
            if m.get("provider") == "claude" and m.get("role") == "main" and m.get("joined")
            and isinstance(m.get("consumer"), str) and len(m["consumer"]) == 32}


def _main_transcript(home, consumer):
    """The main's own transcript, resolved once per consumer."""
    path = _transcripts.get(consumer)
    if path is None:
        sids = native_index({consumer})["claude"].get(consumer) or ()
        for sid in sorted(sids)[:1]:
            with os.scandir(home / "projects") as slugs:
                for slug in list(slugs)[:MAX_FILES]:
                    candidate = Path(slug.path) / (sid + ".jsonl")
                    if slug.is_dir(follow_symlinks=False) and candidate.is_file() and not candidate.is_symlink():
                        path = _transcripts[consumer] = candidate
                        break
    return path


def native_advisors(members):
    """Claude Code's built-in advisor per main participant: the call still unanswered, else the newest
    one answered within ADVISOR_MAX_AGE; in memory only.

    Reads only tool names, ids and timestamps from the tail of each main's own transcript."""
    mains = _claude_mains(members)
    found = {}
    if not mains:
        return found
    home = _claude_home()
    label = None
    for consumer, participant in list(mains.items())[:8]:
        try:
            path = _main_transcript(home, consumer)
            if path is None:
                continue
            calls, answered = [], {}
            try:
                data = _read_owned(path, TAIL_BYTES, tail=True)
            except OSError:
                _transcripts.pop(consumer, None)
                continue
            for line in data.split(b"\n"):
                if b"advisor" not in line:
                    continue
                try:
                    row = json.loads(line)
                    content = row["message"]["content"]
                except (ValueError, KeyError, TypeError, RecursionError):
                    continue
                for part in content if isinstance(content, list) else ():
                    if not isinstance(part, dict):
                        continue
                    if part.get("type") == "server_tool_use" and part.get("name") == "advisor" and isinstance(part.get("id"), str):
                        calls.append((part["id"], row.get("timestamp")))
                    elif part.get("type") == "advisor_tool_result" and isinstance(part.get("tool_use_id"), str):
                        answered[part["tool_use_id"]] = row.get("timestamp")
            if not calls:
                continue
            call = calls[-1]
            running = call[0] not in answered
            started = datetime.fromisoformat(str(call[1]).replace("Z", "+00:00")).astimezone()
            finished = None if running else datetime.fromisoformat(
                str(answered[call[0]]).replace("Z", "+00:00")).astimezone()
            if abs(datetime.now(started.tzinfo).timestamp() - (finished or started).timestamp()) > ADVISOR_MAX_AGE:
                continue
            label = label or _advisor_label(home)
            found[participant] = {"model": label, "started": started.strftime("%H:%M"), "running": running}
            if finished:
                found[participant]["finished"] = finished.strftime("%H:%M")
        except (OSError, ValueError, TypeError, RecursionError):
            continue
    return found


def _active(folder, prefix, suffix, now, inner=None):
    """(ISO mtime, name) of own regular files `prefix*suffix` (or `inner` within such directories) written
    within NATIVE_ACTIVE seconds, newest first."""
    recent = []
    try:
        with os.scandir(folder) as entries:
            for number, entry in enumerate(entries):
                name = entry.name
                if number >= MAX_ENTRIES:
                    break
                if not (name.startswith(prefix) and name.endswith(suffix) and SESSION.fullmatch(name[len(prefix):len(name) - len(suffix)])):
                    continue
                try:
                    info = os.lstat(os.path.join(entry.path, inner)) if inner else entry.stat(follow_symlinks=False)
                    if inner and not entry.is_dir(follow_symlinks=False):
                        continue
                except OSError:
                    continue
                if (stat.S_ISREG(info.st_mode) and 0 <= now - info.st_mtime <= NATIVE_ACTIVE
                        and not (hasattr(os, "getuid") and info.st_uid != os.getuid())):
                    recent.append((info.st_mtime, name[len(prefix):len(name) - len(suffix)]))
    except OSError:
        pass
    return [(time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(mtime)), name)
            for mtime, name in sorted(recent, reverse=True)[:NATIVE_MAX]]


def _oversized_usage(line):
    """(message id, usage counts) of an assistant record too big to parse whole, read from its two ends: the id
    sits in the first bytes, the usage object near the end (after the content). None when it is not an assistant
    record; False when it is one but the ends do not yield a clean id and usage."""
    head, tail = line[:2048], line[-8192:]
    if not (OVERSIZED_ROLE.search(head) or OVERSIZED_ROLE.search(tail)):
        return None
    marks = list(OVERSIZED_USAGE.finditer(tail))
    found = OVERSIZED_ID.search(head)
    if not marks or not found:
        return False
    try:
        usage, _ = json.JSONDecoder().raw_decode(tail[marks[-1].end():].decode("utf-8", "ignore"))
        # An absent field (Claude omits zero cache counts) is zero, not damage.
        counts = tuple(usage.get(key, 0) for key in USAGE_KEYS)
    except (ValueError, AttributeError, RecursionError):
        return False
    if not all(type(count) is int and count >= 0 for count in counts):
        return False
    return found.group(1).decode("ascii"), counts


def _transcript_usage(path, budget):
    """(total tokens, output tokens, partial, counted) from one subagent transcript, or None when it cannot be
    examined at all (callers treat that as unread).

    Only assistant records holding a usage object are counted, once per message: each streamed content block
    repeats its message's usage on consecutive lines, so only the previous message is remembered and a repeat
    replaces it (measured 2026-10-20: 0 non-consecutive repeats in 4454 usage lines of 40 real transcripts); a record over USAGE_LINE_BYTES is read from its two ends instead of parsed
    whole. A refresh reads appended bytes only, within USAGE_FILE_BYTES and the shared `budget`; a replaced
    (inode), truncated or older file is read again from the start. Partial means bytes or records are
    uncounted: the caps stopped the read, the file was unreadable, or a record's usage could not be read.
    `counted` says whether any message was counted."""
    path = str(path)
    try:
        info = os.lstat(path)
    except OSError:
        _usage.pop(path, None)
        return None
    ident = (info.st_dev, info.st_ino)
    entry = _usage.get(path)
    if entry and (entry["id"] != ident or info.st_size < entry["offset"] or info.st_mtime_ns < entry["mtime"]):
        entry = None
    if entry is None:
        entry = _usage[path] = {"id": ident, "mtime": 0, "offset": 0, "last": None, "counted": False,
                                "capped": False, "over": False, "total": 0, "output": 0, "read": -1}
    entry["mtime"], entry["refresh"] = info.st_mtime_ns, _refresh[0]
    if entry["offset"] < info.st_size:
        limit = min(USAGE_FILE_BYTES, budget[0])
        data = b""
        if limit > 0:
            try:
                data = _read_owned(path, limit, start=entry["offset"], expect=ident)
            except OSError:
                data = None  # unsafe or replaced mid-read: keep what was counted, try again next refresh
                entry["capped"] = True
        if data is not None:
            budget[0] -= len(data)
            entry["read"] = _refresh[0]
            end = data.rfind(b"\n") + 1
            if not end and len(data) >= USAGE_FILE_BYTES:
                end = len(data)  # one line longer than the read cap: skipped, and the total says so
                entry["over"] = True
            for line in data[:end].split(b"\n"):
                if b'"usage"' not in line:
                    continue
                if len(line) > USAGE_LINE_BYTES:
                    big = _oversized_usage(line)
                    if big is None:
                        continue
                    if big is False:
                        entry["over"] = True
                        continue
                    key, counts = big
                else:
                    try:
                        record = json.loads(line)
                        message = record["message"]
                        usage = message["usage"]
                        if record.get("type") != "assistant":
                            continue
                        counts = tuple(usage.get(name, 0) for name in USAGE_KEYS)
                        if not all(type(count) is int and count >= 0 for count in counts):
                            # A malformed count leaves the total incomplete.
                            entry["over"] = True
                            continue
                        key = message.get("id")
                    except (ValueError, KeyError, TypeError, AttributeError, RecursionError):
                        continue
                    key = key[:128] if isinstance(key, str) and key else None
                last = entry["last"]
                if key is not None and last and last[0] == key:
                    entry["total"] -= sum(last[1])
                    entry["output"] -= last[1][3]
                entry["total"] += sum(counts)
                entry["output"] += counts[3]
                entry["last"], entry["counted"] = (key, counts), True
            entry["offset"] += end
    # Unread bytes, including a torn last line still being written, leave the total incomplete.
    entry["capped"] = entry["offset"] < info.st_size
    return entry["total"], entry["output"], entry["capped"] or entry["over"], entry["counted"]


def _native_agents(folder, now, budget):
    """Agent-tool subagents whose own transcript was written lately; the row never claims more than that."""
    found = []
    for mtime, agent in _active(folder / "subagents", "agent-", ".jsonl", now):
        title = "Claude Code agent"
        try:
            meta = json.loads(_read_owned(folder / "subagents" / ("agent-%s.meta.json" % agent), NATIVE_META_BYTES))
            title = next((clean(meta[k], 80) for k in ("description", "name", "agentType")
                          if isinstance(meta.get(k), str) and meta[k].strip()), title)
        except (OSError, ValueError, TypeError, AttributeError, RecursionError):
            pass  # the file is read again on every refresh, so a missing or half-written meta fills in later
        row = {"id": agent, "title": title, "active": mtime}
        used = _transcript_usage(folder / "subagents" / ("agent-%s.jsonl" % agent), budget)
        if used and used[3]:
            row.update(tokens=used[0], output=used[1], partial=used[2])
        found.append(row)
    return found


def _codex_rollouts(home):
    """Yield bounded, non-symlink Codex rollout paths under sessions and archived_sessions."""
    scanned = files = 0
    pending = [(home / "sessions", 0), (home / "archived_sessions", 0)]
    while pending and scanned < MAX_ENTRIES and files < MAX_FILES:
        folder, level = pending.pop()
        try:
            if folder.is_symlink() or not folder.is_dir():
                continue
            with os.scandir(folder) as entries:
                for entry in entries:
                    scanned += 1
                    if scanned > MAX_ENTRIES or files >= MAX_FILES:
                        break
                    if entry.is_dir(follow_symlinks=False) and level < 3:
                        pending.append((Path(entry.path), level + 1))
                    elif entry.name.endswith(".jsonl") and entry.is_file(follow_symlinks=False):
                        files += 1
                        yield Path(entry.path)
        except OSError:
            continue


def _codex_usage(path, budget):
    """Last total_token_usage.total_tokens in a Codex token_count event, cached by rollout identity."""
    path = str(path)
    try:
        info = os.lstat(path)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or hasattr(os, "getuid") and info.st_uid != os.getuid()):
            raise OSError("unsafe Codex rollout")
    except OSError:
        _codex_usage_cache.pop(path, None)
        return None
    ident = (info.st_dev, info.st_ino)
    entry = _codex_usage_cache.get(path)
    if entry and (entry["id"] != ident or info.st_size < entry["offset"] or info.st_mtime_ns < entry["mtime"]):
        entry = None
    if entry is None:
        entry = _codex_usage_cache[path] = {"id": ident, "mtime": 0, "offset": 0, "tokens": None}
    entry["mtime"], entry["refresh"] = info.st_mtime_ns, _refresh[0]
    if entry["offset"] < info.st_size and budget[0] > 0:
        limit = min(USAGE_FILE_BYTES, budget[0])
        try:
            data = _read_owned(path, limit, start=entry["offset"], expect=ident)
        except OSError:
            return entry["tokens"]
        budget[0] -= len(data)
        end = data.rfind(b"\n") + 1
        for line in data[:end].split(b"\n"):
            try:
                record = json.loads(line)
                if record.get("type") != "event_msg":
                    continue
                payload = record["payload"]
                if payload.get("type") != "token_count":
                    continue
                usage = payload["info"]["total_token_usage"]
                count = usage["total_tokens"]
                if type(count) is int and count >= 0:
                    entry["tokens"] = count
            except (ValueError, KeyError, TypeError, AttributeError, RecursionError):
                continue
        entry["offset"] += end
    return entry["tokens"]


def _codex_native_agents(home, consumer, now, budget):
    """Codex subagent rollouts whose metadata names this main thread as their parent."""
    sessions = native_index({consumer}).get("codex", {}).get(consumer) or ()
    if len(sessions) != 1:
        return []
    main_id = next(iter(sessions))
    found = []
    for path in _codex_rollouts(home):
        try:
            info = path.lstat()
            if not (0 <= now - info.st_mtime <= NATIVE_ACTIVE) or budget[0] <= 0:
                continue
            # session_meta is the first record; its size is bounded like native_index's metadata read.
            first = _read_owned(path, min(CODEX_META_BYTES, budget[0]))
            budget[0] -= len(first)
            if b"\n" not in first:
                continue
            metadata = json.loads(first.split(b"\n", 1)[0])
            if metadata.get("type") != "session_meta":
                continue
            meta = metadata["payload"]
            source = meta.get("source") if isinstance(meta.get("source"), dict) else {}
            spawned = source.get("subagent")
            if (meta.get("parent_thread_id") != main_id or not isinstance(spawned, dict)
                    or not isinstance(spawned.get("thread_spawn"), dict)):
                continue
            label = meta.get("agent_nickname") or meta.get("agent_path") or "Codex subagent"
            title = clean(label, 80) if isinstance(label, str) else "Codex subagent"
            agent_id = meta.get("id")
            if not isinstance(agent_id, str) or not agent_id:
                agent_id = path.stem
            row = {"id": agent_id, "title": title,
                   "active": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(info.st_mtime))}
            tokens = _codex_usage(path, budget)
            if tokens is not None:
                row["tokens"] = tokens
            found.append(row)
        except (OSError, ValueError, TypeError, KeyError, AttributeError, RecursionError):
            continue
    return sorted(found, key=lambda row: row["active"], reverse=True)[:NATIVE_MAX]


def _run_agents(run, now, budget):
    """Token totals and live/total counts over the agent transcripts under one workflow run directory."""
    files = []
    try:
        with os.scandir(run) as entries:
            for number, entry in enumerate(entries):
                name = entry.name
                if number >= MAX_ENTRIES:
                    break
                if not (name.startswith("agent-") and name.endswith(".jsonl") and SESSION.fullmatch(name[6:-6])):
                    continue
                try:
                    info = entry.stat(follow_symlinks=False)
                except OSError:
                    continue
                if stat.S_ISREG(info.st_mode) and not (hasattr(os, "getuid") and info.st_uid != os.getuid()):
                    files.append((info.st_mtime, entry.path))
    except OSError:
        return {}
    if not files:
        return {}
    found = {"agents": len(files), "live": sum(0 <= now - mtime <= NATIVE_ACTIVE for mtime, _ in files)}
    # Every agent is visited; the least recently read go first, so a shared byte budget that runs out on one
    # refresh reaches the rest on the next ones instead of starving them.
    files.sort(key=lambda item: (_usage.get(item[1], {}).get("read", -1), -item[0]))
    reads = [_transcript_usage(path, budget) for _, path in files]
    counted = [used for used in reads if used and used[3]]
    if counted:
        # An agent left unread or partly read (budget, caps, unreadable) keeps the run's total marked partial.
        found.update(tokens=sum(u[0] for u in counted), output=sum(u[1] for u in counted),
                     partial=any(used is None or used[2] for used in reads))
    return found


def _native_workflows(folder, now, budget):
    """Workflow runs whose journal was written lately, with approximate phase and counts when its tail parses."""
    found = []
    for mtime, name in _active(folder / "subagents" / "workflows", "wf_", "", now, "journal.jsonl"):
        run = "wf_" + name
        started = answered = 0
        phase = None
        try:
            for line in _read_owned(folder / "subagents" / "workflows" / run / "journal.jsonl", WORKFLOW_BYTES, tail=True).split(b"\n"):
                try:
                    row = json.loads(line)
                except (ValueError, RecursionError):
                    continue
                kind = row.get("type") if isinstance(row, dict) else None
                answered += kind == "result"
                if kind == "started":
                    started += 1
                    phase = clean(row["phase"], 24) if isinstance(row.get("phase"), str) else phase
        except OSError:
            pass
        title = "Workflow " + run
        if started or answered:
            title += "%s · agents ~%s started · ~%s done" % (" · phase " + phase if phase else "", started, answered)
        found.append(dict(_run_agents(folder / "subagents" / "workflows" / run, now, budget),
                          id=run, title=title, active=mtime))
    return found


def native_subagents(members):
    """Claude Code's own subagents and Workflow runs per main participant, for read-only board rows.

    {main participant: {"agents": [...], "workflows": [...]}}, in memory only; every failure drops the one
    main, file or row it touches."""
    mains = _claude_mains(members)
    codex_mains = {m["consumer"]: m["participant"] for m in members
                   if m.get("provider") == "codex" and m.get("role") == "main" and m.get("joined")
                   and isinstance(m.get("consumer"), str) and len(m["consumer"]) == 32}
    all_mains = [("claude", key, value) for key, value in mains.items()]
    all_mains.extend(("codex", key, value) for key, value in codex_mains.items())
    found = {}
    if not all_mains:
        return found
    home, now = _claude_home(), time.time()
    codex_home = Path(os.environ.get("CODEX_HOME") or os.environ.get("OMS_CODEX_HOME") or Path.home() / ".codex")
    budget = [USAGE_REFRESH_BYTES]
    _refresh[0] += 1
    for provider, consumer, participant in all_mains[:8]:
        try:
            if provider == "codex":
                agents = _codex_native_agents(codex_home, consumer, now, budget)
                if agents:
                    found[participant] = {"agents": agents, "workflows": []}
                continue
            path = _main_transcript(home, consumer)
            folder = path.with_suffix("") if path else None
            if folder is None or not UUID.fullmatch(path.stem) or folder.is_symlink() or not folder.is_dir():
                continue
            row = {"agents": [], "workflows": []}
            for key, read in (("agents", _native_agents), ("workflows", _native_workflows)):
                try:
                    row[key] = read(folder, now, budget)
                except (OSError, ValueError, TypeError, KeyError, RecursionError):
                    continue
            if row["agents"] or row["workflows"]:
                found[participant] = row
        except Exception:  # best-effort board extra: a surprise must never reach the panel tick
            continue
    for cache in (_usage, _codex_usage_cache):
        try:
            for path, entry in list(cache.items()):
                try:
                    if entry.get("refresh") != _refresh[0]:
                        del cache[path]
                except Exception:
                    continue
        except Exception:
            continue
    return found


def current_models(repo, members):
    """Read model rows without SQL writes; SQLite coordinates existing WAL reader locks."""
    candidates = [m for m in members if m.get("role") == "main" and m.get("provider") == "codex"
                  and m.get("state") not in TERMINAL_STATES
                  and re.fullmatch(r"[0-9a-f]{32}", str(m.get("consumer") or ""))]
    if not candidates or repo is None:
        return {}
    identities = native_index({m["consumer"] for m in candidates}).get("codex", {})
    bound = [(m, next(iter(identities[m["consumer"]]))) for m in candidates
             if len(identities.get(m["consumer"], ())) == 1]
    if not bound:
        return {}
    home = Path(os.environ.get("CODEX_HOME") or os.environ.get("OMS_CODEX_HOME") or Path.home() / ".codex")
    try:
        databases = []
        with os.scandir(home) as entries:
            for number, entry in enumerate(entries):
                if number >= 512:
                    return {}
                version = re.fullmatch(r"state_([1-9][0-9]*)\.sqlite", entry.name)
                if version:
                    databases.append((int(version[1]), Path(entry.path)))
        if not databases:
            return {}
        # An unreadable newer database must not revive an older model setting.
        path = max(databases)[1]
        before = path.lstat()
        if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
                or hasattr(os, "getuid") and before.st_uid != os.getuid()):
            return {}
        header = _read_owned(path, 20)
        wal_mode = header[18:20] == b"\x02\x02"
        for suffix in ("-wal", "-shm"):
            sidecar = Path(str(path) + suffix)
            try:
                info = sidecar.lstat()
            except FileNotFoundError:
                if wal_mode:
                    return {}
                continue
            if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                    or hasattr(os, "getuid") and info.st_uid != os.getuid()):
                return {}
        deadline = time.monotonic() + .2
        readings = {}
        with closing(sqlite3.connect(path.absolute().as_uri() + "?mode=ro", uri=True, timeout=.05)) as database:
            database.execute("PRAGMA query_only=ON")
            database.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
            for member, sid in bound[:32]:
                if time.monotonic() > deadline:
                    break
                row = database.execute("SELECT substr(model,1,161), substr(reasoning_effort,1,16), "
                                       "substr(cwd,1,4097) FROM threads WHERE id=?", (sid,)).fetchone()
                if not row:
                    continue
                model, effort, cwd = row
                if (not isinstance(model, str) or len(model) > 160
                        or not re.fullmatch(r"[A-Za-z0-9_./:-]+", model)
                        or not isinstance(cwd, str) or len(cwd) > 4096
                        or Path(cwd).resolve() != Path(repo).resolve()):
                    continue
                readings[member["participant"]] = {"model": model, "consumer": member["consumer"],
                    "effort": effort if effort in {"low", "medium", "high", "xhigh", "max", "ultra"} else None,
                    "source": "native state"}
        after = path.lstat()
        return readings if (before.st_dev, before.st_ino) == (after.st_dev, after.st_ino) else {}
    except (OSError, ValueError, sqlite3.Error):
        return {}


def windows(repo, ident, state=None, _tick=None):
    found = {}
    import panel_host
    if panel_host.herdr_active():
        return panel_host.targets(repo, ident, state if state is not None else room.status(repo, ident), _tick=_tick)
    if not os.environ.get("TMUX") or not shutil.which("tmux"):
        return found
    try:
        repo = Path(repo).resolve()
        state = state if state is not None else room.status(repo, ident)
        if not isinstance(state, dict) or not isinstance(state.get("participants"), list):
            return found
        members = {p["participant"]: p for p in state["participants"] if p["role"] == "main"}
        result = subprocess.run(["tmux", "list-windows", "-a", "-F",
            "#{window_id}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room}\t#{@oms_panel_native_pane}\t#{@oms_panel_room_participant}\t#{@oms_panel_repo}\t#{session_id}"],
            capture_output=True, text=True, check=False, timeout=3)
        if result.returncode:
            return found
        live = subprocess.run(["tmux", "list-panes", "-a", "-F", "#{pane_id}\t#{window_id}"],
                              capture_output=True, text=True, check=False, timeout=3)
        if live.returncode:
            return found
        native_panes = {line.replace("\r", "") for line in live.stdout.splitlines()}
        candidates = []
        for line in result.stdout.splitlines():
            fields = line.replace("\r", "").split("\t")
            if (len(fields) == 7 and room.same_room(repo, fields[2], ident) and re.fullmatch(r"@[0-9]+", fields[0])
                    and re.fullmatch(r"\$[0-9]+", fields[6])
                    and re.fullmatch(r"%[0-9]+", fields[3]) and fields[3] + "\t" + fields[0] in native_panes):
                member = fields[4] or fields[1]
                if (member not in members or not room.thread_live.ID.fullmatch(fields[1])
                        or not fields[5] or Path(fields[5]).resolve() != repo):
                    continue
                candidates.append((member, fields))
        if not candidates:
            return found
        if _tick is not None:
            attempts = [row for row in _tick.attempts() if isinstance(row, dict) and not row.get('terminal')]
        else:
            evidence = subprocess.run(["bash", str(room.ENTRY), "agent-events", "--repo", str(repo),
                                       "list", "--active", "--json"], capture_output=True, text=True,
                                      check=False, timeout=5, stdin=subprocess.DEVNULL)
            if evidence.returncode:
                return found
            attempts = json.loads(evidence.stdout)
        if not isinstance(attempts, list):
            return found
        attempts = {a.get("attempt_id"): a for a in attempts
                    if isinstance(a, dict) and isinstance(a.get("attempt_id"), str)}
        for member, fields in candidates:
            attempt = attempts.get(fields[1], {})
            refs = attempt.get("refs", {})
            if (not isinstance(refs, dict) or attempt.get("terminal") is not False
                    or attempt.get("tool") != "panel-main" or refs.get("panel_role") != "main"
                    or attempt.get("provider") != members[member]["provider"]
                    or not room.same_room(repo, refs.get("panel_room_id"), ident)
                    or refs.get("panel_room_participant", fields[1]) != member):
                continue
            found.setdefault(member, []).append({"window": fields[0], "pane": fields[3], "session": fields[6]})
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError, subprocess.SubprocessError):
        pass
    return found


def catalog(repo, ident, who=None, terminal_first=False):
    parent_only()
    state = room.status(repo, ident)
    members = [p for p in state["participants"] if who is None or p["participant"] == who]
    panes = windows(repo, ident, state)
    wanted = {p["consumer"] for p in members if p.get("consumer") and p["role"] == "main"
              and not (terminal_first and len(panes.get(p["participant"], [])) == 1)}
    index = native_index(wanted) if wanted else {}
    rows = []
    for member in members:
        who, provider = member["participant"], member["provider"]
        native = index.get(provider, {}).get(member.get("consumer"), set()) if member["role"] == "main" else set()
        sid = next(iter(native)) if len(native) == 1 else None
        locations = panes.get(who, []) if member["role"] == "main" else []
        title = clean(member.get("label"), 80)
        if title in {None, "", "Native task not recorded"}:
            title = "%s %s (no task title)" % (provider, member["role"])
        rows.append({"participant": who, "label": member["label"], "provider": provider,
            "role": member["role"], "joined": member["joined"], "session_id": sid,
            "uri": "codex://threads/" + quote(sid, safe="") if sid and provider == "codex" else None,
            "terminal": locations[0] if len(locations) == 1 else None,
            "status": "native" if sid or len(locations) == 1 else "artifacts" if member["role"] != "main" else "unresolved",
            "reason": "ambiguous native identity" if len(native) > 1 or len(locations) > 1 else
                      "no native session recorded for this main" if not sid and not locations and not member.get("consumer") else
                      "session not found in the recent local history (older sessions are not scanned)"
                      if not sid and not locations else None,
            "title": title})
    return {"schema": 1, "kind": "oms-panel-chats", "room": ident, "rows": rows,
            "authority": "navigation only; no prompt submission or transcript forwarding"}


def terminal_commands(target):
    caller = os.environ.get("TMUX_PANE", "")
    if not re.fullmatch(r"%[0-9]+", caller):
        raise ValueError("current tmux pane is unavailable; select the original terminal")
    current = subprocess.run(["tmux", "display-message", "-p", "-t", caller, "#{session_id}"],
                             capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    if current.returncode or not isinstance(current.stdout, str):
        raise ValueError("current tmux session is unavailable; native session was preserved")
    session = current.stdout.replace("\r", "").strip()
    if not re.fullmatch(r"\$[0-9]+", session):
        raise ValueError("current tmux session is unavailable; native session was preserved")
    commands = [["tmux", "select-window", "-t", target["window"]],
                ["tmux", "select-pane", "-t", target["pane"]]]
    if session != target["session"]:
        result = subprocess.run(["tmux", "list-clients", "-F", "#{client_name}\t#{session_id}"],
                                capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        if result.returncode or len(result.stdout) > 65536:
            raise ValueError("tmux client evidence is unavailable; select the original terminal")
        clients = []
        for line in result.stdout.replace("\r", "").splitlines():
            fields = line.split("\t")
            if (len(fields) == 2 and fields[1] == session and fields[0]
                    and len(fields[0]) <= 256 and not fields[0].startswith("-")
                    and clean(fields[0], 256) == fields[0]):
                clients.append(fields[0])
        if len(clients) != 1:
            raise ValueError("current tmux client is missing or ambiguous; select the original terminal")
        commands.append(["tmux", "switch-client", "-c", clients[0], "-t", target["session"]])
    return commands


def plan(repo, ident, who, surface="auto"):
    report = catalog(repo, ident, who=who, terminal_first=surface != "app")
    row = next((r for r in report["rows"] if r["participant"] == who), None)
    if row is None:
        raise ValueError("choose a participant in the selected room")
    if surface not in {"auto", "terminal", "app"}:
        raise ValueError("invalid chat surface")
    commands = []
    if surface in {"auto", "terminal"} and row["terminal"]:
        target = row["terminal"]
        if target.get("host") == "herdr":
            return dict(row, method="existing-terminal", host="herdr", target=target, commands=[])
        commands = terminal_commands(target)
        method = "existing-terminal"
    elif surface != "terminal" and row["uri"]:
        launcher = "open" if sys.platform == "darwin" else "xdg-open"
        if os.name == "nt":
            method = "windows-uri"
        elif shutil.which(launcher):
            if sys.platform == "linux":
                if not shutil.which("xdg-mime"):
                    raise ValueError("Codex app handler cannot be verified: xdg-mime is unavailable")
                try:
                    handler = subprocess.run(
                        ["xdg-mime", "query", "default", "x-scheme-handler/codex"],
                        capture_output=True, text=True, check=False, timeout=3,
                        stdin=subprocess.DEVNULL)
                except (OSError, UnicodeError, subprocess.SubprocessError):
                    raise ValueError("Codex app handler cannot be verified")
                value = handler.stdout if isinstance(handler.stdout, str) else ""
                if (handler.returncode or len(value) > 256
                        or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,239}\.desktop\r?\n?", value)):
                    raise ValueError("Codex app handler is unavailable or invalid")
            commands.append([launcher, row["uri"]])
            method = "app-uri"
        else:
            raise ValueError("no local app URI opener; use the existing terminal or native app sidebar")
    elif surface != "terminal" and row["provider"] == "claude" and row["session_id"] and sys.platform in {"darwin", "win32"}:
        launcher = shutil.which("claude")
        if not launcher:
            raise ValueError("Claude CLI is unavailable; use the native app sidebar")
        help_result = subprocess.run([launcher, "--help"], capture_output=True, text=True,
                                     check=False, timeout=3, stdin=subprocess.DEVNULL)
        if help_result.returncode or "--desktop" not in help_result.stdout:
            raise ValueError("installed Claude does not advertise --desktop; use the native app sidebar")
        commands.append([launcher, "--desktop", "--resume", row["session_id"]])
        method = "claude-desktop"
    elif row["status"] == "artifacts":
        method = "artifacts"
    else:
        raise ValueError(row["reason"] or "native chat is not navigable here; use the native app sidebar or an existing terminal")
    return {"schema": 1, "kind": "oms-panel-chat-open", "room": ident, "participant": who,
            "provider": row["provider"], "method": method, "commands": commands, "uri": row["uri"],
            "frontend_authority": "none", "ui_observed": False}


def open_chat(repo, ident, who, surface="auto", dry_run=False, allowed_methods=None):
    request = plan(repo, ident, who, surface)
    if allowed_methods is not None and request["method"] not in allowed_methods:
        raise ValueError("no existing terminal or exact app link; choose the original chat in its app")
    if dry_run or request["method"] == "artifacts":
        return request
    if request.get("host") == "herdr":
        import panel_host
        panel_host.focus(request["target"])
        return dict(request, status="navigation_requested")
    if request["method"] == "windows-uri":
        os.startfile(request["uri"])
    else:
        for command in request["commands"]:
            result = subprocess.run(command, check=False, timeout=5, stdin=subprocess.DEVNULL)
            if result.returncode:
                raise ValueError("chat navigation failed; session was preserved")
    return dict(request, status="navigation_requested")


def text(report):
    lines = ["ROOM CHATS / " + report["room"]]
    for number, row in enumerate(report["rows"], 1):
        lines.append("%s. %s / %s / %s / %s%s" % (number, row["title"], row["provider"], row["role"], row["status"],
                                               "" if row["joined"] else " / left"))
        if row["reason"]:
            lines.append("   " + row["reason"])
        if row["uri"]:
            lines.append("   " + row["uri"])
        if row["session_id"] and not row["uri"]:
            lines.append("   native session: " + row["session_id"] + " / Claude app sidebar")
    return "\n".join(lines)
