"""Optional terminal navigation; unsupported terminals retain the passive watch."""

import os
import re
import select
import subprocess
import sys
import time


class TerminalInput:
    def __init__(self, managed=False, active=True):
        self.managed = managed
        self.active = active
        self.fd = None
        self.saved = None
        self.pending = b""
        self.escape_since = None
        self.deferred = []

    def __enter__(self):
        if not self.active or os.name != "posix" or not sys.stdin.isatty() or not sys.stdout.isatty():
            return self
        try:
            import termios
            self.fd = sys.stdin.fileno()
            self.saved = termios.tcgetattr(self.fd)
            changed = list(self.saved)
            changed[6] = list(changed[6])
            changed[3] &= ~(termios.ICANON | termios.ECHO)
            changed[6][termios.VMIN] = 0
            changed[6][termios.VTIME] = 0
            termios.tcsetattr(self.fd, termios.TCSANOW, changed)
            if self.managed:
                session = os.environ.get("OMS_PANEL_SESSION", "")
                if re.fullmatch(r"oms-panel-[a-f0-9]{12}", session):
                    subprocess.run(["tmux", "set-option", "-t", session, "mouse", "on"],
                                   capture_output=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
            sys.stdout.write("\033[?1000h\033[?1006h")
            sys.stdout.flush()
        except (ImportError, OSError, ValueError, subprocess.SubprocessError):
            self.__exit__(None, None, None)
        return self

    def __exit__(self, *args):
        if self.saved is not None:
            try:
                sys.stdout.write("\033[?1000l\033[?1006l")
                sys.stdout.flush()
            finally:
                import termios
                try:
                    termios.tcsetattr(self.fd, termios.TCSANOW, self.saved)
                finally:
                    self.saved = None
                    self.fd = None

    def decode(self, data):
        self.pending = (self.pending + data)[-1024:]
        output = []
        keys = {b"\033[A": "up", b"\033[B": "down", b"\033[C": "right", b"\033[D": "left",
                b"\033OA": "up", b"\033OB": "down", b"\033OC": "right", b"\033OD": "left",
                b"\033[H": "home", b"\033[F": "end", b"\033[5~": "pageup", b"\033[6~": "pagedown"}
        while self.pending:
            mouse = re.match(rb"\x1b\[<(\d{1,4});(\d{1,4});(\d{1,4})([Mm])", self.pending)
            if mouse:
                code, x, y = [int(v) for v in mouse.groups()[:3]]
                self.pending = self.pending[mouse.end():]
                if mouse.group(4) == b"M":
                    if code == 0:
                        output.append(("click", x, y))
                    elif code in (64, 65):
                        output.append(("scroll", -3 if code == 64 else 3))
                continue
            key = next((sequence for sequence in keys if self.pending.startswith(sequence)), None)
            if key:
                output.append((keys[key],))
                self.pending = self.pending[len(key):]
                continue
            if self.pending.startswith(b"\033"):
                if self.pending == b"\033" or self.pending.startswith((b"\033[", b"\033O")) and len(self.pending) < 32:
                    self.escape_since = self.escape_since or time.monotonic()
                    break
                self.pending = self.pending[1:]
                continue
            value = self.pending[:1]
            self.pending = self.pending[1:]
            if value in (b"\r", b"\n"):
                output.append(("enter",))
            elif value == b"\t":
                output.append(("down",))
            elif value in (b" ", b"q", b"g", b"t", b"b"):
                output.append((value.decode(),))
        if not self.pending:
            self.escape_since = None
        return output

    def wait(self, seconds):
        if self.deferred:
            events, self.deferred = self.deferred, []
            return events
        if self.fd is None:
            time.sleep(seconds)
            return []
        if self.escape_since is not None:
            seconds = min(seconds, .1)
        if select.select([self.fd], [], [], max(0, seconds))[0]:
            data = os.read(self.fd, 1024)
            return self.decode(data) if data else [("eof",)]
        if self.escape_since is not None and time.monotonic() - self.escape_since >= .1:
            self.pending = b""
            self.escape_since = None
            return [("escape",)]
        return []

    def discard_clicks(self):
        if self.fd is None:
            return
        for unused in range(4):
            if not select.select([self.fd], [], [], 0)[0]:
                break
            data = os.read(self.fd, 1024)
            if not data:
                break
            self.deferred += [event for event in self.decode(data) if event[0] != "click"]
        if self.pending.startswith(b"\033[<"):
            self.pending = b""
            self.escape_since = None


def choose(event, navigation):
    """Use only the last rendered hit map, never row numbers from a new snapshot."""
    kind = event[0]
    items = navigation.get("items", [])
    selected = navigation.get("selected")
    if kind == "click":
        hit = next((h for h in navigation.get("hits", [])
                    if h["y"] == event[2] and h["x1"] <= event[1] <= h["x2"]), None)
        if hit:
            navigation["selected"] = hit["action"]
            if hit.get("fold") and event[1] <= 2:
                return ("fold", hit["action"][1])
            return hit["action"]
    elif kind in ("up", "down", "home", "end") and items:
        index = items.index(selected) if selected in items else (-1 if kind == "down" else 0)
        index = 0 if kind == "home" else len(items) - 1 if kind == "end" else (index + (1 if kind == "down" else -1)) % len(items)
        navigation["selected"] = items[index]
        visible = any(h["action"] == items[index] for h in navigation.get("hits", []))
        if not visible:
            positions = navigation.get("positions", {})
            row = positions.get(items[index], 0)
            offset = navigation.get("offset", 0)
            navigation["offset"] = row if row < offset else max(0, row - max(1, navigation.get("viewport", 1)) + 1)
    elif kind == "enter":
        return selected if selected in items else None
    elif kind == " " and selected in items and selected[0] == "chat":
        return ("fold", selected[1])
    elif kind in ("scroll", "pageup", "pagedown"):
        step = event[1] if kind == "scroll" else max(1, navigation.get("viewport", 1)) * (-1 if kind == "pageup" else 1)
        navigation["offset"] = max(0, navigation.get("offset", 0) + step)
    return None
