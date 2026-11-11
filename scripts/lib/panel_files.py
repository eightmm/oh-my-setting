"""Read-only worker file inventories, shared by the board and result reader."""

from collections import OrderedDict
import difflib
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import threading
import time
import unicodedata

from dashboard_projection import INVISIBLE, clean
from panel_results import PATCH_MAX, _diffstat_data, _patch_paths, verified_patch
from path_scope import has_glob, matches, normalize, validate_patterns

TTL = 3
FILE_LIMIT = 1024 * 1024
_CACHE = OrderedDict()
_WORKERS = OrderedDict()
_BOARD = OrderedDict()
_BOARD_SLOTS = threading.BoundedSemaphore(2)
_LOCK = threading.Lock()


def patch_changes(data):
    return [tuple(row) for row in _diffstat_data(data, limit=None)["files"]]


def file_diff(data, path):
    """Keep whole diff sections, including both sides of a rename."""
    sections = re.split(rb"(?m)(?=^diff --git )", data)
    return b"".join(part for part in sections if part.startswith(b"diff --git ") and path in _patch_paths(part))


def tree_rows(changes, declared=(), others=None):
    """Directory groups of scope entries and changes; overlap is display evidence only."""
    declared = validate_patterns(normalize(value) for value in declared)
    others = others or {}
    files = {path: (added, removed) for path, added, removed in changes}
    entries = sorted(set(files) | set(declared))
    root = {}
    for path in entries:
        node = root
        for part in path.rstrip("/").split("/"):
            node = node.setdefault(part, {})
        node[None] = path
    rows = []
    pending = [(key, root[key], 0) for key in sorted(root, reverse=True)]
    while pending:
        name, child, depth = pending.pop()
        path = child.get(None)
        directory = any(key is not None for key in child) or bool(path and path.endswith("/"))
        label = "  " * depth + clean(name, 4096) + ("/" if directory else "")
        overlap = []
        if path:
            for who, (scopes, changed) in others.items():
                if (path in changed or any(matches(path, scope) for scope in validate_patterns(scopes)) or
                        path == "." and bool(scopes or changed) or
                        path in declared and any(matches(other, path) for other in changed) or
                        path in declared and not has_glob(path) and any(scope.startswith(path + "/") for scope in scopes)):
                    overlap.append(who)
            if path in files:
                added, removed = files[path]
                label += " +%s -%s" % ("?" if added is None else added, "?" if removed is None else removed)
                if declared and not any(matches(path, scope) for scope in declared):
                    label += " · outside scope"
            if path in declared:
                label += " · declared"
            if overlap:
                label += " · overlap: " + ", ".join(clean(who, 120) for who in overlap)
        rows.append({"path": path if path and path != "." and not path.endswith("/") and
                     (path in files or not directory and not has_glob(path)) else None,
                     "text": label, "overlap": overlap, "changed": path in files})
        pending.extend((key, child[key], depth + 1) for key in sorted(
            (key for key in child if key is not None), reverse=True))
    return rows


def _git(worktree, arguments, deadline=None):
    """Bound output in a temporary descriptor; Git never refreshes the worker index."""
    timeout = min(2, deadline - time.monotonic()) if deadline is not None else 2
    if timeout <= 0:
        raise TimeoutError("file read timed out")
    command = ["git", "--no-optional-locks", "--literal-pathspecs", "-c", "core.fsmonitor=false", "-C", str(worktree), *arguments]
    scratch = Path(tempfile.gettempdir()).resolve()
    if scratch.is_relative_to(Path(worktree).resolve()):
        raise ValueError("temporary directory is inside the worker worktree")
    with tempfile.TemporaryFile(dir=scratch) as output:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.DEVNULL,
                                env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"), timeout=timeout, check=False)
        if result.returncode:
            raise ValueError("Git file evidence unavailable")
        output.seek(0)
        data = output.read(PATCH_MAX + 1)
    if len(data) > PATCH_MAX:
        raise ValueError("file evidence exceeds read limit")
    return data


def worktree_path(repo, location):
    if location == "repository":
        return Path(repo).resolve()
    if not isinstance(location, str) or not location.startswith("worktree:") or location.count("/") != 1:
        raise ValueError("worker worktree not recorded")
    parent, name = location[len("worktree:"):].split("/")
    data = _git(repo, ["worktree", "list", "--porcelain"])
    found = [Path(line[9:]) for line in data.decode("utf-8", "surrogateescape").splitlines()
             if line.startswith("worktree ")]
    found = [path.resolve() for path in found if path.name == name and path.parent.name == parent and path.is_dir()
             and path.resolve() != Path(repo).resolve()]
    if len(set(found)) != 1:
        raise ValueError("worker worktree unavailable or ambiguous")
    return found[0]


def _relative(path):
    # Scope normalization folds whitespace and backslashes; Git's POSIX file names must stay literal.
    if (not isinstance(path, str) or not path or path.startswith("/") or "\0" in path or
            any(part in {"", ".", ".."} for part in path.split("/"))):
        raise ValueError("file path must be repository-relative")
    return path


def _file_bytes(worktree, path, read_link=False):
    path = _relative(path)
    root = Path(worktree).resolve()
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
    try:
        parts = path.split("/")
        for part in parts[:-1]:
            next_directory = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = next_directory
        if read_link and stat.S_ISLNK(os.stat(parts[-1], dir_fd=directory, follow_symlinks=False).st_mode):
            return os.readlink(parts[-1], dir_fd=directory).encode("utf-8", "surrogateescape")
        # No symlink component can redirect a read, even when a worker replaces a directory concurrently.
        descriptor = os.open(parts[-1], os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW, dir_fd=directory)
    finally:
        os.close(directory)
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_size > FILE_LIMIT:
            raise ValueError("file is not a regular file within the read limit")
        with os.fdopen(descriptor, "rb", closefd=False) as handle:
            data = handle.read(FILE_LIMIT + 1)
        if len(data) > FILE_LIMIT:
            raise ValueError("file exceeds read limit")
        return data
    finally:
        os.close(descriptor)


def read_live(worktree, base="HEAD"):
    """A three-second inventory snapshot, including non-ignored untracked files."""
    if not isinstance(base, str):
        return {"files": [], "error": "recorded base commit is invalid"}
    key = (str(Path(worktree).resolve()), base)
    now = time.monotonic()
    with _LOCK:
        kept = _CACHE.get(key)
        if kept and now - kept[0] < TTL:
            return kept[1]
    result = {"files": [], "worktree": key[0], "base": base, "untracked": (), "skipped": [], "error": ""}
    try:
        if base != "HEAD" and not re.fullmatch(r"[0-9a-fA-F]{40,64}", base or ""):
            raise ValueError("recorded base commit is invalid")
        deadline = now + 2
        data = _git(worktree, ["diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--numstat", "-z", base, "--"], deadline)
        files = []
        for entry in data.split(b"\0"):
            if not entry:
                continue
            added, removed, path = entry.split(b"\t", 2)
            files.append((path.decode("utf-8", "surrogateescape"), int(added) if added != b"-" else 0,
                          int(removed) if removed != b"-" else 0))
        result["files"] = files
        untracked = _git(worktree, ["ls-files", "--others", "--exclude-standard", "-z"], deadline)
        paths = [path.decode("utf-8", "surrogateescape") for path in untracked.split(b"\0") if path]
        result["untracked"] = tuple(paths)
        for path in paths:
            try:
                if time.monotonic() >= deadline:
                    raise TimeoutError("file read timed out")
                data = _file_bytes(worktree, path, read_link=True)
                files.append((path, 0 if b"\0" in data else len(data.splitlines()), 0))
            except (OSError, ValueError):
                files.append((path, None, 0))
                result["skipped"].append(path)
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        result["error"] = clean(str(error), 160) or "worker files unavailable"
    with _LOCK:
        _CACHE[key] = (now, result)
        _CACHE.move_to_end(key)
        while len(_CACHE) > 64:
            _CACHE.popitem(last=False)
    return result


def live_diff(snapshot, path):
    path = _relative(path)
    if path not in snapshot.get("untracked", ()):
        data = _git(snapshot["worktree"], ["diff", "--no-ext-diff", "--no-textconv", "--no-renames",
                                          snapshot["base"], "--", path])
        return file_diff(data, path)
    data = _file_bytes(snapshot["worktree"], path, read_link=True)
    if b"\0" in data:
        return ("Binary file added: " + clean(path, 4096) + "\n").encode()
    lines = data.decode("utf-8", "replace").splitlines(keepends=True)
    return ("diff --git a/%s b/%s\n" % (path, path) + "".join(difflib.unified_diff(
        [], lines, fromfile="/dev/null", tofile="b/" + path))).encode("utf-8")


def _worker_fields(member, attempt):
    refs, meta = attempt.get("refs", {}), attempt.get("panel", {})
    return (refs.get("panel_role") or meta.get("role") or member.get("role"),
            refs.get("panel_access") or meta.get("access") or attempt.get("access") or member.get("access"),
            refs.get("panel_location") or meta.get("location") or member.get("location"),
            member.get("state") or attempt.get("state"))


def is_write_worker(member, attempt=None):
    role, access, location, _ = _worker_fields(member, attempt or {})
    return role in {"worker", "builder"} and access == "write" and isinstance(location, str) and location.startswith("worktree:")


def worker_inventory(repo, member, attempt=None, call=None):
    """A worker's patch is authoritative after exit; its worktree is only a live preview."""
    from call_ending import LIVE_STATES
    repo, attempt = Path(repo), attempt or {}
    role, access, location, state = _worker_fields(member, attempt)
    if role == "main" or access == "read" or state in LIVE_STATES and not is_write_worker(member, attempt):
        return {"files": [], "error": ""}
    key = (str(Path(repo).resolve()), member.get("participant"), member.get("attempt") or attempt.get("attempt_id"),
           state, role, access, location, attempt.get("base_sha"),
           (call or {}).get("patch"), (call or {}).get("patch_sha256"))
    now = time.monotonic()
    with _LOCK:
        kept = _WORKERS.get(key)
        if kept and now - kept[0] < TTL:
            return kept[1]
    result = _worker_inventory(repo, member, attempt, call)
    with _LOCK:
        _WORKERS[key] = (now, result)
        _WORKERS.move_to_end(key)
        while len(_WORKERS) > 64:
            _WORKERS.popitem(last=False)
    return result


def _worker_inventory(repo, member, attempt=None, call=None):
    from call_ending import LIVE_STATES
    attempt = attempt or {}
    if (member.get("state") or attempt.get("state")) in LIVE_STATES and not attempt.get("base_sha") and member.get("attempt"):
        # Dashboard projections omit base_sha; the full lifecycle record retains it.
        try:
            from panel_results import _records
            full, _ = _records(repo, full=True)
            attempt = next((row for row in full if row.get("attempt_id") == member["attempt"]), attempt)
        except (OSError, ValueError, TypeError, subprocess.SubprocessError):
            pass
    role, access, location, state = _worker_fields(member, attempt)
    if role == "main" or access == "read" or state in LIVE_STATES and not is_write_worker(member, attempt):
        return {"files": [], "error": ""}
    if state in LIVE_STATES:
        try:
            path = worktree_path(repo, location)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            return {"files": [], "error": clean(str(error), 160)}
        return read_live(path, attempt.get("base_sha") or (call or {}).get("base_sha") or "HEAD")
    try:
        if call is None:
            from panel_results import results_snapshot
            report, _ = results_snapshot(repo, room_participant=member.get("participant"))
            call = next((call for row in report.get("rows", []) for call in reversed(row.get("calls", []))
                         if call.get("attempt_id") == (member.get("attempt") or attempt.get("attempt_id"))), {})
        data = verified_patch(repo, call) if call.get("patch") else b""
        return {"files": patch_changes(data),
                "record": {key: call[key] for key in ("patch", "patch_sha256") if key in call},
                "error": "", "location": location}
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        return {"files": [], "error": "patch evidence unavailable: " + clean(str(error), 120)}


def overlaps(member, snapshot, others):
    """Name only other live workers, using both their scopes and observed changed paths."""
    from call_ending import LIVE_STATES
    return {other["participant"]: (other.get("owns") or [], [path for path, *_ in reading["files"]])
            for other, reading in others if other.get("participant") != member.get("participant")
            and is_write_worker(other, other.get("file_attempt")) and other.get("state") in LIVE_STATES}


def _refresh_board(repo, member, key, entry, slots):
    reading = {"files": [], "error": "worker file read interrupted"}
    try:
        reading = worker_inventory(repo, member, member.get("file_attempt"))
    except Exception as error:
        reading = {"files": [], "error": "worker files unavailable: " + clean(str(error), 120)}
    finally:
        with _LOCK:
            if _BOARD.get(key) is entry:
                entry.update(reading=reading, updated=time.monotonic(), pending=False)
        slots.release()


def board_summaries(repo, members):
    """Render cached readings; at most two daemon jobs refresh workers off the UI thread."""
    repo = Path(repo)
    workers = []
    for member in members:
        attempt = member.get("file_attempt") or {}
        if not is_write_worker(member, attempt):
            continue
        key = (str(repo.resolve()), member.get("participant"), member.get("attempt"),
               *_worker_fields(member, attempt), attempt.get("base_sha"))
        refresh = False
        slots = _BOARD_SLOTS
        with _LOCK:
            entry = _BOARD.setdefault(key, {"reading": {"files": [], "error": "Reading worker files"},
                                             "updated": float("-inf"), "pending": False})
            _BOARD.move_to_end(key)
            if not entry["pending"] and time.monotonic() - entry["updated"] >= TTL and slots.acquire(False):
                entry["pending"], refresh = True, True
            reading = entry["reading"]
            while len(_BOARD) > 64:
                _BOARD.popitem(last=False)
        if refresh:
            job = threading.Thread(target=_refresh_board, args=(repo, dict(member), key, entry, slots), daemon=True)
            entry["thread"] = job
            try:
                job.start()
            except RuntimeError:
                with _LOCK:
                    entry.update(pending=False, updated=time.monotonic())
                slots.release()
        workers.append((member, reading))
    summaries = {}
    for member, snapshot in workers:
        rows = tree_rows(snapshot["files"], member.get("owns") or [], overlaps(member, snapshot, workers))
        summaries[member["participant"]] = "files %s/%s%s" % (
            "?" if snapshot.get("error") else len(snapshot["files"]), len(member.get("owns") or []),
            " !" if any(row["overlap"] for row in rows) else "")
    return summaries


def pager_text(repo, spec, path):
    """Read the selected evidence again in the pager process; never invoke a worker editor."""
    from panel_view import tint
    try:
        return _pager_text(repo, spec, path)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        return tint("File unavailable: " + clean(str(error), 160), "dim") + "\n"


def _pager_text(repo, spec, path):
    from panel_reader import render_view
    _relative(path)
    if spec.get("record"):
        data = file_diff(verified_patch(repo, spec["record"]), path)
    elif spec.get("base"):
        data = live_diff(spec, path)
    else:
        data = b""
    if data:
        return "\n".join(render_view("Diff", {}, {}, patch=data, width=2000, color=True).splitlines()[3:]) + "\n"
    worktree = spec.get("worktree") or str(worktree_path(repo, spec.get("location")))
    text = _file_bytes(worktree, path, read_link=True).decode("utf-8", "replace").replace("\r\n", "\n")
    return "".join(char if char in "\n\t" or not unicodedata.category(char).startswith("C") else "?"
                   for char in text if char not in INVISIBLE)
