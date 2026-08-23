#!/usr/bin/env python3
"""Non-destructive cross-platform process liveness probe."""

from __future__ import annotations

import errno
import os
from typing import Any, Callable, Optional


SYNCHRONIZE = 0x00100000
WAIT_OBJECT_0 = 0x00000000
WAIT_TIMEOUT = 0x00000102
WAIT_FAILED = 0xFFFFFFFF
ERROR_ACCESS_DENIED = 5
ERROR_INVALID_PARAMETER = 87


def _configure_kernel32(kernel32: Any) -> None:
    import ctypes
    from ctypes import wintypes

    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel32.WaitForSingleObject.restype = wintypes.DWORD
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL


def _windows_pid_alive(
    pid: int,
    *,
    kernel32: Optional[Any] = None,
    get_last_error: Optional[Callable[[], int]] = None,
) -> bool:
    """Probe with wait rights only; ambiguous Win32 errors preserve work."""
    try:
        import ctypes

        if kernel32 is None:
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        if get_last_error is None:
            get_last_error = ctypes.get_last_error
        _configure_kernel32(kernel32)
        try:
            handle = kernel32.OpenProcess(SYNCHRONIZE, False, pid)
        except OSError as exc:
            error = int(getattr(exc, "winerror", 0) or get_last_error())
            return error != ERROR_INVALID_PARAMETER
        if not handle:
            return int(get_last_error()) != ERROR_INVALID_PARAMETER
        try:
            try:
                result = int(kernel32.WaitForSingleObject(handle, 0))
            except (OSError, ValueError):
                return True
            if result == WAIT_OBJECT_0:
                return False
            # WAIT_TIMEOUT is alive. WAIT_FAILED and unknown results are
            # ambiguous, so fail closed by preserving the worker.
            return True
        finally:
            try:
                kernel32.CloseHandle(handle)
            except (OSError, ValueError):
                pass
    except Exception:
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
    """Return liveness without ever signaling a process on Windows."""
    platform = os.name if os_name is None else os_name
    if platform == "nt":
        # Git Bash exposes an MSYS pid through $$, but OpenProcess consumes a
        # Win32 pid. Never guess or fall back across those namespaces: a
        # legacy/malformed marker is unproven and therefore preserved.
        if (
            isinstance(native_pid, bool)
            or not isinstance(native_pid, int)
            or native_pid <= 0
            or native_pid > 0xFFFFFFFF
        ):
            return True
        if (isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0
                or pid > 0x7FFFFFFF):
            return True
        return _windows_pid_alive(
            native_pid, kernel32=kernel32, get_last_error=get_last_error
        )
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
