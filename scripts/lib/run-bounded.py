#!/usr/bin/env python3
"""Portable POSIX wall clock for provider and verifier subprocesses."""

from __future__ import annotations

import os
import re
import signal
import subprocess
import sys
import time


def seconds(value: str) -> float:
    match = re.fullmatch(r"([0-9]+(?:\.[0-9]+)?)([smhd]?)", value)
    if not match:
        raise ValueError("invalid duration: %s" % value)
    amount = float(match.group(1))
    factor = {"": 1, "s": 1, "m": 60, "h": 3600, "d": 86400}[match.group(2)]
    result = amount * factor
    if result <= 0:
        raise ValueError("duration must be greater than zero: %s" % value)
    return result


def normalized_returncode(returncode: int) -> int:
    return 128 + (-returncode) if returncode < 0 else returncode


def main() -> int:
    if len(sys.argv) < 5:
        print(
            "error: usage: run-bounded.py WALL KILL_AFTER LABEL COMMAND...",
            file=sys.stderr,
        )
        return 2
    if os.name != "posix":
        print(
            "error: portable timeout fallback requires POSIX process groups; "
            "install timeout/gtimeout on this host",
            file=sys.stderr,
        )
        return 127
    try:
        wall = seconds(sys.argv[1])
        kill_after = seconds(sys.argv[2])
    except ValueError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 2
    label = sys.argv[3]
    command = sys.argv[4:]
    try:
        # Inherit all three streams. In particular, provider prompts arrive on
        # stdin; a communicate()/PIPE wrapper would consume or buffer them.
        process = subprocess.Popen(command, start_new_session=True)
    except FileNotFoundError as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 127
    except (OSError, PermissionError) as exc:
        print("error: %s" % exc, file=sys.stderr)
        return 126

    def group_alive() -> bool:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        return True

    def stop_group(first_signal: int) -> None:
        if not group_alive():
            return
        try:
            os.killpg(process.pid, first_signal)
        except ProcessLookupError:
            return
        deadline = time.monotonic() + kill_after
        while group_alive() and time.monotonic() < deadline:
            # Reap the group leader when it exits, but keep waiting on any
            # descendant that ignored TERM before deciding on escalation.
            process.poll()
            time.sleep(0.05)
        if group_alive():
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            process.wait(timeout=max(1.0, kill_after))
        except subprocess.TimeoutExpired:
            pass

    def forward(signum: int, _frame: object) -> None:
        stop_group(signum)
        raise SystemExit(128 + signum)

    for forwarded in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(forwarded, forward)

    try:
        return normalized_returncode(process.wait(timeout=wall))
    except subprocess.TimeoutExpired:
        print("error: %s call timed out after %s" % (label, sys.argv[1]), file=sys.stderr)
        stop_group(signal.SIGTERM)
        return 124


if __name__ == "__main__":
    raise SystemExit(main())
