"""Read-only project background evidence; recorded activity grants no authority.

The board shows recorded non-terminal autopilot stages and their age, unfinished
graph runs started less than 24 hours ago (unconfirmed), attributed queue jobs,
and a pushed land whose install remains pending. Each source gets one bounded
row, with at most three items and an overflow count. Legacy queue metadata has
no project identity and is omitted; shell metadata is never executed.
"""

import datetime
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import time

from dashboard_projection import clean

MAX_BYTES = 1024 * 1024
RECENT_SECONDS = 24 * 60 * 60
STAGES = {"proposing", "proposal-review", "approved", "driving", "reviewing", "publishing"}
RUN_ID = re.compile(r"run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}\Z")


def _read(path, limit=MAX_BYTES):
    """Reject linked, oversized or changing records, including non-regular files."""
    path = Path(path)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
        raise ValueError("invalid background record")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    flags |= getattr(os, "O_BINARY", 0)
    with os.fdopen(os.open(str(path), flags), "rb") as stream:
        opened = os.fstat(stream.fileno())
        raw = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns)
    if (not stat.S_ISREG(opened.st_mode) or len(raw) > limit
            or identity(before) != identity(opened) or identity(opened) != identity(after)
            or identity(after) != identity(path.lstat())):
        raise ValueError("background record changed")
    return raw.decode("utf-8")


def _json(path):
    value = json.loads(_read(path))
    if not isinstance(value, dict):
        raise ValueError("invalid background object")
    return value


def _directory(path):
    # Refuse linked record namespaces as well as linked leaves.
    return stat.S_ISDIR(Path(path).lstat().st_mode)


def _project_path(repo, *parts):
    path = Path(repo)
    for part in (".oms",) + parts:
        path = path / part
        if not _directory(path):
            raise ValueError("invalid project background directory")
    return path


def _age(stamp, now):
    if not isinstance(stamp, str):
        raise ValueError("invalid background timestamp")
    parsed = datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("background timestamp needs a timezone")
    age = now - parsed.timestamp()
    if age < 0:
        raise ValueError("background timestamp is in the future")
    return age


def _ago(age):
    return "%dm ago" % (age // 60) if age < 3600 else "%dh ago" % (age // 3600)


def _autopilot(repo, now):
    try:
        receipt = _json(_project_path(repo, "plan") / "autopilot-run.json")
        stage = receipt.get("stage")
        if not isinstance(stage, str) or stage not in STAGES:
            return []
        return ["Autopilot: recorded %s (%s)" % (stage, _ago(_age(receipt.get("updated"), now)))]
    except (OSError, ValueError, TypeError, OverflowError):
        return []


def _summary(prefix, items):
    shown = items[:3]
    return [prefix + " / ".join(shown) + (" / +%d more" % (len(items) - 3) if len(items) > 3 else "")] if items else []


def _graphs(repo, now):
    items = []
    try:
        directory = _project_path(repo, "graph", "runs")
        for path in sorted(directory.iterdir(), reverse=True):
            if not RUN_ID.fullmatch(path.name):
                continue
            try:
                if not _directory(path):
                    continue
                rows = [json.loads(line) for line in _read(path / "events.jsonl").splitlines() if line.strip()]
                if not rows or any(not isinstance(row, dict) for row in rows):
                    continue
                started = next((row for row in rows if row.get("event") == "run_started"), {})
                age = _age(started.get("ts"), now)
                if age >= RECENT_SECONDS or any(row.get("event") == "run_finished" for row in rows):
                    continue
                items.append("%s (%s, unconfirmed)" % (clean(started.get("spec_id") or path.name, 60), _ago(age)))
            except (OSError, ValueError, TypeError, OverflowError):
                continue
    except (OSError, ValueError):
        pass
    return _summary("Graph: ", items)


def _metadata(path, repo):
    for line in _read(path).splitlines():
        if line.startswith("# oms-panel "):
            row = json.loads(line[len("# oms-panel "):])
            if (isinstance(row, dict) and isinstance(row.get("project"), str)
                    and row["project"] == str(repo)):
                return row
    return None


def _run(argv):
    return subprocess.run(argv, capture_output=True, text=True, check=False, timeout=3,
                          stdin=subprocess.DEVNULL, env=dict(os.environ, GIT_NO_LAZY_FETCH="1"))


def _queue(repo, now):
    del now
    base = Path(os.environ.get("XDG_RUNTIME_DIR") or os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    queue = Path(os.environ.get("OMS_TSP_STATE_DIR") or base / "oh-my-setting/tsp-queue")
    fallback = Path(os.environ.get("OMS_TSP_FALLBACK_DIR") or base / "oh-my-setting/tsp-fallback")
    items = []
    if (os.environ.get("OMS_TSP_FORCE_FALLBACK") != "1" and queue.is_dir()
            and not queue.is_symlink() and shutil.which("tsp")):
        try:
            result = _run(["tsp"])
            if result.returncode == 0 and _directory(queue):
                for line in result.stdout.replace("\r", "").splitlines():
                    fields = line.split()
                    if len(fields) < 2 or not fields[0].isdigit() or fields[1] not in {"queued", "running"}:
                        continue
                    try:
                        row = _metadata(queue / fields[0], repo)
                        if row:
                            items.append("#%s %s %s" % (fields[0], fields[1], clean(row.get("label") or "job", 80)))
                    except (OSError, ValueError, TypeError):
                        continue
        except (OSError, ValueError, subprocess.SubprocessError):
            pass
    try:
        if _directory(fallback):
            for path in sorted(fallback.glob("*.meta")):
                if not re.fullmatch(r"[1-9][0-9]{0,9}", path.stem):
                    continue
                try:
                    row = _metadata(path, repo)
                    if not row:
                        continue
                    status = row.get("status")
                    if not isinstance(status, str) or Path(status).name != status or (fallback / status).exists():
                        continue
                    result = _run(["ps", "-o", "stat=", "-p", path.stem])
                    state = result.stdout.strip()
                    if result.returncode == 0 and state and not state.startswith("Z"):
                        items.append("fallback #%s running (unconfirmed) %s" % (path.stem, clean(row.get("label") or "job", 80)))
                except (OSError, ValueError, TypeError, subprocess.SubprocessError):
                    continue
    except (OSError, ValueError):
        pass
    return _summary("Queue: ", items)


def _uninstalled(receipt):
    if not isinstance(receipt, dict):
        return []
    if receipt.get("active") is True:
        return []
    # Status normally selects this HEAD's receipt. The same namespace may have a newer land.
    log = receipt.get("log")
    if isinstance(log, str) and log:
        directory = Path(log).parent
        try:
            if _directory(directory):
                paths = [path for path in directory.glob("*.json")
                         if re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})-.+\.json", path.name)]
                if paths:
                    receipt = _json(max(paths, key=lambda path: path.lstat().st_mtime_ns))
        except (OSError, ValueError, TypeError):
            return []
    sha = receipt.get("sha")
    push, update = receipt.get("push"), receipt.get("update")
    if (not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", sha)
            or not isinstance(push, dict) or type(push.get("rc")) is not int or push["rc"] != 0
            or not isinstance(update, dict) or (type(update.get("rc")) is int and update["rc"] == 0)):
        return []
    install = update.get("root")
    if not install:
        config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
        try:
            install = _json(Path(os.environ.get("OMS_INSTALL_RECEIPT") or config / "oh-my-setting/install.json")).get("source_root")
        except (OSError, ValueError, TypeError):
            pass
    if isinstance(install, str) and install:
        try:
            if _run(["git", "-C", install, "merge-base", "--is-ancestor", sha, "HEAD"]).returncode == 0:
                return []
        except (OSError, ValueError, subprocess.SubprocessError):
            pass
    return ["Install pending: %s %s" % (sha[:7], clean(update.get("reason") or "reason not recorded", 80))]


def collect(repo, land=None, now=None):
    """Each optional source fails independently; no metadata is sourced or executed."""
    repo = Path(repo).resolve()
    now = time.time() if now is None else now
    rows = []
    for source in (lambda: _autopilot(repo, now), lambda: _graphs(repo, now),
                   lambda: _queue(repo, now), lambda: _uninstalled(land)):
        try:
            rows.extend(source())
        except (OSError, ValueError, TypeError, OverflowError, RecursionError, subprocess.SubprocessError):
            continue
    return rows
