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
                    try:
                        # Best effort: keyboard input works without tmux mouse reporting.
                        subprocess.run(["tmux", "set-option", "-t", session, "mouse", "on"],
                                       capture_output=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
                    except (OSError, subprocess.SubprocessError):
                        pass
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
                b"\033[H": "home", b"\033[F": "end", b"\033OH": "home", b"\033OF": "end",
                b"\033[1~": "home", b"\033[4~": "end", b"\033[7~": "home", b"\033[8~": "end",
                b"\033[5~": "pageup", b"\033[6~": "pagedown"}
        while self.pending:
            mouse = re.match(rb"\x1b\[<(\d{1,4});(\d{1,4});(\d{1,4})([Mm])", self.pending)
            if mouse:
                code, x, y = [int(v) for v in mouse.groups()[:3]]
                self.pending = self.pending[mouse.end():]
                if mouse.group(4) == b"M":
                    if code == 0:
                        output.append(("click", x, y))
                    elif code in (64, 65):
                        output.append(("scroll", -3 if code == 64 else 3, x, y))
                continue
            key = next((sequence for sequence in keys if self.pending.startswith(sequence)), None)
            if key:
                output.append((keys[key],))
                self.pending = self.pending[len(key):]
                continue
            if self.pending.startswith(b"\033"):
                # A complete sequence nobody binds (Delete, F-keys, Shift-Tab, Ctrl-arrows) is dropped, never Esc.
                unknown = re.match(rb"\x1b(?:\[[0-?]*[ -/]*[@-~]|O[@-~])", self.pending)
                if unknown:
                    self.pending = self.pending[unknown.end():]
                    continue
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
                output.append(("tab",))
            elif value in (b" ", b"q", b"g", b"t", b"b", b"v", b"a", b"w", b"f", b"?", b"n", b"1", b"2"):
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
            bare = self.pending == b"\033"
            self.pending = b""
            self.escape_since = None
            return [("escape",)] if bare else []
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
            target = hit["action"]
            if target[0] in ("pin", "tab", "message", "seat"):
                return target
            navigation["selected"] = target
            if hit.get("fold") and event[1] <= 2:
                return ("fold", target[1])
            # A first click selects a main tab or previews a call; repeating it opens.
            if hit.get("select") and navigation.get("primary") != target:
                navigation["preview"] = {"target": target}
                return None
            if hit.get("preview") and target[0] != "chat":
                # A call's content stays in the board's detail area; no separate screen opens.
                navigation["preview"] = {"target": target}
                return None
            if hit.get("preview") and (navigation.get("preview") or {}).get("target") != target:
                navigation["preview"] = {"target": target}
                return None
            return target
    elif kind in ("up", "down", "home", "end") and items:
        index = items.index(selected) if selected in items else (-1 if kind == "down" else 0)
        index = 0 if kind == "home" else len(items) - 1 if kind == "end" else (index + (1 if kind == "down" else -1)) % len(items)
        navigation["selected"] = items[index]
        if navigation.get("surface") == "graph":
            navigation["preview"] = {"target": items[index]}
        visible = any(h["action"] == items[index] for h in navigation.get("hits", []))
        if not visible:
            positions = navigation.get("positions", {})
            row = positions.get(items[index], 0)
            offset = navigation.get("offset", 0)
            navigation["offset"] = row if row < offset else max(0, row - max(1, navigation.get("viewport", 1)) + 1)
    elif kind == "enter":
        if navigation.get("surface") == "graph" and selected in items and selected[0] in ("result", "debate"):
            navigation["preview"] = {"target": selected}
            return None
        return selected if selected in items else None
    elif kind == "f":
        target = navigation.get("full_result")
        if (navigation.get("surface") == "graph" and target in items
                and target[0] in ("result", "debate")
                and (navigation.get("preview") or {}).get("target") == target):
            return target
    elif kind in ("left", "right"):
        mains = [item for item in items if item[0] == "chat"]
        current = next((item for item in (selected, navigation.get("primary")) if item in mains), None)
        if mains:
            index = mains.index(current) + (1 if kind == "right" else -1) if current else 0
            navigation["selected"] = mains[index % len(mains)]
            if navigation.get("surface") == "graph":
                # The detail follows an explicit main change and starts at its top.
                navigation["preview"] = {"target": navigation["selected"]}
                navigation.setdefault("band_offsets", {})["detail"] = 0
    elif kind == " " and selected in items and selected[0] == "chat":
        return ("pin" if navigation.get("surface") == "graph" else "fold", selected[1])
    elif kind == "scroll" and len(event) > 3 and not navigation.get("detail") and any(
            b["y1"] <= event[3] <= b["y2"] and b["x1"] <= event[2] <= b["x2"] for b in navigation.get("bands", [])):
        band = next(b for b in navigation["bands"] if b["y1"] <= event[3] <= b["y2"] and b["x1"] <= event[2] <= b["x2"])
        offsets = navigation.setdefault("band_offsets", {})
        step = band.get("step", 1)
        offsets[band["name"]] = max(0, offsets.get(band["name"], 0) + (step if event[1] > 0 else -step))
    elif kind in ("pageup", "pagedown") and navigation.get("bands") and not navigation.get("detail"):
        # Graph pages: the box scrolls when it shows a detail or the plan, otherwise every call band moves one page.
        names = [b["name"] for b in navigation["bands"]]
        offsets = navigation.setdefault("band_offsets", {})
        for name in ["detail"] if "detail" in names and (navigation.get("box") or {}).get("pages") else names:
            step = 6 if name == "detail" else 3
            offsets[name] = max(0, offsets.get(name, 0) + (step if kind == "pagedown" else -step))
    elif kind in ("scroll", "pageup", "pagedown"):
        step = event[1] if kind == "scroll" else max(1, navigation.get("viewport", 1)) * (-1 if kind == "pageup" else 1)
        navigation["offset"] = max(0, navigation.get("offset", 0) + step)
    return None
