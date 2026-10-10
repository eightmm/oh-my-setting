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
PI_SESSION = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9._-]{0,158}[A-Za-z0-9])?\Z")
UUID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z")
MAX_ENTRIES = 8192
MAX_FILES = 4096
TAIL_BYTES = 512 * 1024
ADVISOR_MAX_AGE = 1800
ADVISOR_MODELS = (("fable", "Fable 5.1"), ("opus", "Opus 5.5"), ("sonnet", "Sonnet 5.5"))
_transcripts = {}


def parent_only():
    if os.environ.get("OMS_HARNESS_CHILD") == "1" or os.environ.get("OMS_HARNESS_DELEGATE_DEPTH", "0") != "0":
        raise ValueError("workers cannot navigate native owner chats")


def native_index(wanted, repo=None):
    """Resolve enrolled hashes from filenames and first metadata rows only."""
    found = {"codex": {}, "claude": {}, "pi": {}}
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
    if repo is not None:
        headers = pi_session_headers(repo)
        if headers is not None:
            for sid, rows in headers.items():
                consumer = hashlib.sha256(sid.encode()).hexdigest()[:32]
                if consumer in wanted and len(rows) == 1 and rows[0].get("cwd") == str(Path(repo).resolve()):
                    found["pi"].setdefault(consumer, set()).add(sid)
    return found


def pi_session_dir(repo):
    """Pi's project-scoped session directory, using canonical home and project paths."""
    home = Path.home().resolve(strict=True)
    canonical = str(Path(repo).resolve(strict=True))
    slug = "--" + re.sub(r"[^A-Za-z0-9]+", "-", canonical).strip("-") + "-" + hashlib.sha256(canonical.encode()).hexdigest()[:12] + "--"
    return home / ".pi" / "agent" / "sessions" / slug


def pi_session_storage_safe(repo, create=False):
    """Validate each owned directory before creating or descending into its child."""
    root = pi_session_dir(repo)
    home = Path.home().resolve(strict=True)
    try:
        current = home
        home_info = current.stat()
        if not stat.S_ISDIR(home_info.st_mode) or (hasattr(os, "getuid") and home_info.st_uid != os.getuid()):
            return False
        for part in (".pi", "agent", "sessions", root.name):
            child = current / part
            try:
                info = child.lstat()
            except FileNotFoundError:
                if not create:
                    return False
                try:
                    child.mkdir(mode=0o700)
                except FileExistsError:
                    pass
                info = child.lstat()
            if (stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode)
                    or hasattr(os, "getuid") and info.st_uid != os.getuid()):
                return False
            current = child
        return current == root
    except (OSError, RuntimeError, ValueError):
        return False


def pi_session_headers(repo):
    """Boundedly read safe Pi session headers from this exact project directory."""
    root = pi_session_dir(repo)
    try:
        if not pi_session_storage_safe(repo):
            return None
        result = {}
        scanned = 0
        with os.scandir(root) as entries:
            for entry in entries:
                scanned += 1
                if scanned > MAX_FILES:
                    return None
                path = Path(entry.path)
                if not entry.name.endswith(".jsonl"):
                    continue
                if not entry.is_file(follow_symlinks=False):
                    return None
                try:
                    data = _read_owned(path, 65537)
                    newline = data.find(b"\n")
                    if newline < 0 or newline > 65536:
                        return None
                    line = data[:newline]
                    if len(line) > 65536:
                        return None
                    row = json.loads(line.decode("utf-8"), parse_constant=_reject_json_constant)
                    if (not isinstance(row, dict) or row.get("type") != "session"
                            or not isinstance(row.get("id"), str) or not PI_SESSION.fullmatch(row["id"])
                            or not isinstance(row.get("cwd"), str)):
                        return None
                    result.setdefault(row["id"], []).append(row)
                except (OSError, ValueError, IndexError, RecursionError):
                    return None
        return result
    except OSError:
        return None


def _reject_json_constant(value):
    raise ValueError("non-standard JSON constant: " + value)


def pi_session_header(repo, ident):
    if not isinstance(ident, str) or not PI_SESSION.fullmatch(ident):
        return None
    headers = pi_session_headers(repo)
    if headers is None:
        return None
    rows = headers.get(ident, [])
    canonical = str(Path(repo).resolve(strict=True))
    return rows[0] if len(rows) == 1 and rows[0].get("cwd") == canonical else None


def _claude_home():
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")


def _read_owned(path, limit, tail=False):
    """Bytes from an own-uid regular file without following links; the last `limit` bytes when `tail`."""
    info = os.lstat(path)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or hasattr(os, "getuid") and info.st_uid != os.getuid():
        raise OSError("unsafe native file")
    fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(fd, "rb") as handle:
        actual = os.fstat(handle.fileno())
        if not stat.S_ISREG(actual.st_mode) or (actual.st_dev, actual.st_ino) != (info.st_dev, info.st_ino):
            raise OSError("native file changed")
        start = max(0, actual.st_size - limit) if tail else 0
        handle.seek(start)
        data = handle.read(limit)
    if start and b"\n" in data:
        data = data.split(b"\n", 1)[1]  # the cut may land mid-line
    return data


def _advisor_label(home):
    try:
        value = json.loads(_read_owned(home / "settings.json", 1024 * 1024)).get("advisorModel")
    except (OSError, ValueError, AttributeError, RecursionError):
        return "Advisor"
    value = str(value).lower() if isinstance(value, str) else ""
    return next((label for key, label in ADVISOR_MODELS if key in value), "Advisor")


def native_advisors(members):
    """Claude Code's built-in advisor per main participant: the call still unanswered, else the newest
    one answered within ADVISOR_MAX_AGE; in memory only.

    Reads only tool names, ids and timestamps from the tail of each main's own transcript."""
    mains = {m["consumer"]: m["participant"] for m in members
             if m.get("provider") == "claude" and m.get("role") == "main" and m.get("joined")
             and isinstance(m.get("consumer"), str) and len(m["consumer"]) == 32}
    found = {}
    if not mains:
        return found
    home = _claude_home()
    label = None
    for consumer, participant in list(mains.items())[:8]:
        try:
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


def windows(repo, ident, state=None):
    found = {}
    import panel_host
    if panel_host.herdr_active():
        return panel_host.targets(repo, ident, state if state is not None else room.status(repo, ident))
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
            if (len(fields) == 7 and fields[2] == ident and re.fullmatch(r"@[0-9]+", fields[0])
                    and re.fullmatch(r"\$[0-9]+", fields[6])
                    and re.fullmatch(r"%[0-9]+", fields[3]) and fields[3] + "\t" + fields[0] in native_panes):
                member = fields[4] or fields[1]
                if (member not in members or not room.thread_live.ID.fullmatch(fields[1])
                        or not fields[5] or Path(fields[5]).resolve() != repo):
                    continue
                candidates.append((member, fields))
        if not candidates:
            return found
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
                    or refs.get("panel_room_id") != ident
                    or refs.get("panel_room_participant", fields[1]) != member):
                continue
            found.setdefault(member, []).append({"window": fields[0], "pane": fields[3], "session": fields[6]})
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    return found


def catalog(repo, ident, who=None, terminal_first=False, native_repo=None):
    parent_only()
    state = room.status(repo, ident)
    members = [p for p in state["participants"] if who is None or p["participant"] == who]
    panes = windows(repo, ident, state)
    wanted = {p["consumer"] for p in members if p.get("consumer") and p["role"] == "main"
              and not (terminal_first and len(panes.get(p["participant"], [])) == 1)}
    index = native_index(wanted, native_repo or repo) if wanted else {}
    rows = []
    for member in members:
        who, provider = member["participant"], member["provider"]
        native = index.get(provider, {}).get(member.get("consumer"), set()) if member["role"] == "main" else set()
        sid = next(iter(native)) if len(native) == 1 else None
        locations = panes.get(who, []) if member["role"] == "main" else []
        rows.append({"participant": who, "label": member["label"], "provider": provider,
            "role": member["role"], "joined": member["joined"], "session_id": sid,
            "uri": "codex://threads/" + quote(sid, safe="") if sid and provider == "codex" else None,
            "terminal": locations[0] if len(locations) == 1 else None,
            "status": "native" if sid or len(locations) == 1 else "artifacts" if member["role"] != "main" else "unresolved",
            "reason": "ambiguous native identity" if len(native) > 1 or len(locations) > 1 else
                      "not bound or outside bounded local scan" if not sid and not locations else None})
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


def plan(repo, ident, who, surface="auto", native_repo=None):
    report = catalog(repo, ident, who=who, terminal_first=surface != "app", native_repo=native_repo)
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


def open_chat(repo, ident, who, surface="auto", dry_run=False, allowed_methods=None, native_repo=None):
    request = plan(repo, ident, who, surface, native_repo=native_repo)
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
        lines.append("%s. %s / %s / %s / %s%s" % (number, clean(row["label"], 80), row["provider"], row["role"], row["status"],
                                               "" if row["joined"] else " / left"))
        if row["reason"]:
            lines.append("   " + row["reason"])
        if row["uri"]:
            lines.append("   " + row["uri"])
        if row["session_id"] and not row["uri"]:
            if row["provider"] == "pi":
                lines.append("   Pi native terminal session: " + row["session_id"] + " / resume with --launch pi --resume")
            else:
                lines.append("   native session: " + row["session_id"] + " / Claude app sidebar")
    return "\n".join(lines)
