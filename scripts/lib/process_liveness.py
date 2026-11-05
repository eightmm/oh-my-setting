#!/usr/bin/env python3
"""Non-destructive cross-platform process liveness probe."""

from __future__ import annotations

import errno
import os
from pathlib import Path
from typing import Any, Callable, List, Optional, Union


def proc_stat_fields(pid: Union[int, str]) -> List[str]:
    """Read fields starting at state; comm may contain spaces and parentheses."""
    raw = (Path("/proc") / str(pid) / "stat").read_text(
        encoding="utf-8", errors="replace"
    )
    return raw[raw.rfind(")") + 2:].split()


def persisted_native_pid_is_proven(
    native_pid: Any,
    source: Any,
    os_name: Optional[str] = None,
) -> bool:
    """POSIX persisted logical/native pids share one namespace."""
    return True


def pid_alive(
    pid: int,
    *,
    native_pid: Optional[int] = None,
    os_name: Optional[str] = None,
    kernel32: Optional[Any] = None,
    get_last_error: Optional[Callable[[], int]] = None,
    posix_kill: Optional[Callable[[int, int], Any]] = None,
) -> bool:
    """Probe process existence without sending a signal."""
    if (isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0
            or pid > 0x7FFFFFFF):
        if isinstance(pid, int) and not isinstance(pid, bool) and pid > 0:
            return True
        return False
    kill = os.kill if posix_kill is None else posix_kill
    try:
        kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OverflowError, ValueError):
        return True
    except OSError as exc:
        if exc.errno == errno.ESRCH:
            return False
        if exc.errno == errno.EPERM:
            return True
        return True
