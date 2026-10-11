"""Read-only Python bridge to the shared native scratch-state resolver."""

import os
from pathlib import Path
import stat
import subprocess

EXECUTION_ENV = "OMS_PANEL_EXECUTION_ROOT"
MARKER_LIMIT = 16 * 1024
LIBRARY = Path(__file__).with_name("agent-memory-common.sh")

# Supply the checked snapshot instead of letting the shell reopen mutable
# marker contents. Managed paths and registration stay with the native helper:
# Bash-minted marker paths need not use native Python's Windows spelling.
STATE_QUERY = r'''
. "$1" || exit 2
declare -F oms_state_root >/dev/null || {
    printf '%s\n' 'published oms_state_root prerequisite is unavailable' >&2
    exit 2
}
room_execution=$2 room_kind=$3 room_temporary=$4 room_origin=$5 room_worktree=$6
oms_harness_read_marker_value() {
    case "$2" in
        kind) printf '%s\n' "$room_kind" ;;
        temporary) printf '%s\n' "$room_temporary" ;;
        repo) printf '%s\n' "$room_origin" ;;
        worktree) printf '%s\n' "$room_worktree" ;;
        *) return 1 ;;
    esac
}
room_shared=$(oms_state_root "$room_execution") || exit 2
# Git emits the path spelling understood by the calling native Python on
# Windows; pwd -P inside the helper remains the comparison authority.
git -C "$room_shared" rev-parse --show-toplevel
'''


def _work_repository(value):
    directory = Path(value).resolve(strict=True)
    if not directory.is_dir():
        raise ValueError("repository must be a directory")
    query = subprocess.run(["git", "-C", str(directory), "rev-parse", "--show-toplevel"],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True,
                           check=False, timeout=5)
    if query.returncode == 0:
        return Path(query.stdout.replace("\r", "").rstrip("\n")).resolve(strict=True), True
    return directory, False


def work_repository(value):
    return _work_repository(value)[0]


def _generation(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


def _snapshot(path):
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > MARKER_LIMIT:
        raise ValueError("scratch marker must be a bounded unlinked regular file")
    descriptor = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
    with os.fdopen(descriptor, "rb") as source:
        opened = os.fstat(source.fileno())
        if _generation(before) != _generation(opened):
            raise ValueError("scratch marker changed during open")
        content = source.read(MARKER_LIMIT + 1)
        if _generation(opened) != _generation(os.fstat(source.fileno())):
            raise ValueError("scratch marker changed during read")
    if _generation(opened) != _generation(path.lstat()) or len(content) > MARKER_LIMIT or not content.endswith(b"\n"):
        raise ValueError("scratch marker changed or is incomplete")
    fields = {}
    for record in content.decode("utf-8").splitlines():
        name, equals, value = record.partition("=")
        if not equals or name in fields or "\0" in value:
            raise ValueError("invalid scratch marker fields")
        fields[name] = value
    if fields.get("kind") != "oh-my-setting-temp" or fields.get("temporary") != "1":
        raise ValueError("invalid scratch marker type")
    if not fields.get("repo") or not fields.get("worktree"):
        raise ValueError("scratch marker paths are missing")
    return fields, _generation(opened)


def _state_repository(value):
    local, git_worktree = _work_repository(value)
    if not git_worktree:
        return local
    if (os.environ.get("OMS_HARNESS_CHILD") or "0") != "0" or (os.environ.get("OMS_HARNESS_DELEGATE_DEPTH") or "0") != "0":
        return local
    parent = local.parent
    try:
        if (local.name != "wt" or not parent.name.startswith("oh-my-setting-scratch.")
                or parent.is_symlink()):
            return local
        marker = parent / ".oh-my-setting-tmp"
        fields, generation = _snapshot(marker)
    except (OSError, ValueError, UnicodeError):
        return local
    query = subprocess.run(["bash", "-c", STATE_QUERY, "room-state", LIBRARY.as_posix(), local.as_posix(),
                            fields["kind"], fields["temporary"], fields["repo"], fields["worktree"]],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True,
                           check=False, timeout=15)
    if query.returncode:
        raise ValueError("room repository resolution failed: " + query.stderr[:160].strip())
    try:
        if _snapshot(marker) != (fields, generation):
            return local
        shared = Path(query.stdout.replace("\r", "").rstrip("\n")).resolve(strict=True)
        return shared if shared.is_dir() else local
    except (OSError, ValueError, UnicodeError):
        return local


def state_repository(value):
    shared = _state_repository(value)
    inherited = os.environ.get("OMS_ROOM_REPO") if os.environ.get("OMS_ROOM_ID") else None
    if inherited and _state_repository(inherited) != shared:
        raise ValueError("selected repository conflicts with the inherited room repository")
    return shared


def native_repository(shared):
    intended = os.environ.get(EXECUTION_ENV)
    if not intended:
        return shared
    working = work_repository(intended)
    if _state_repository(working) != shared:
        raise ValueError("native execution root does not belong to the selected state repository")
    return working
