"""Local numeric readings for an explicitly selected room main."""

import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import time

from dashboard_projection import mapping, number
from panel_view import TERMINAL_STATES


def percent(value):
    if number(value) is None or value > 100:
        return None
    return round(value)


def cached(consumer, now):
    if not isinstance(consumer, str) or not re.fullmatch(r"[0-9a-f]{32}", consumer):
        return {}
    folder = Path(os.environ.get("OMS_HUD_CACHE_DIR") or Path(tempfile.gettempdir()) / "oh-my-setting-hud")
    path = folder / ("ctx-" + consumer[:24] + ".json")
    try:
        if folder.is_symlink() or path.is_symlink():
            return {}
        fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 8192:
                return {}
            if hasattr(os, "getuid") and info.st_uid != os.getuid():
                return {}
            raw = handle.read(8193)
        data = json.loads(raw)
        timestamp = mapping(data).get("ts")
        if number(timestamp) is None:
            return {}
        if timestamp > now:
            return {}
        if now - timestamp > 600:
            return {"source": "cache stale"}
        used = percent(data.get("used_percentage"))
        week = mapping(data.get("seven_day"))
        weekly = percent(week.get("used_percentage"))
        reset = week.get("resets_at")
        if number(reset) is not None and reset <= now:
            weekly = None
        return {"context_left": None if used is None else 100 - used,
                "weekly_used": weekly, "source": "HUD"}
    except (OSError, ValueError, RecursionError):
        return {}


def pane_reading(target, provider):
    """Mirror only numeric fields in the proven pane's visible native footer."""
    pane = mapping(target).get("pane")
    if mapping(target).get("host") == "herdr":
        try:
            import panel_host
            return numeric_reading(panel_host.read(target), provider)
        except (OSError, ValueError, KeyError, TypeError):
            return {}
    if not isinstance(pane, str) or not re.fullmatch(r"%[0-9]+", pane):
        return {}
    try:
        result = subprocess.run(["tmux", "display-message", "-p", "-t", pane,
                                 "#{pane_height}\t#{pane_dead}"], capture_output=True, text=True,
                                check=False, timeout=2, stdin=subprocess.DEVNULL)
        fields = result.stdout.replace("\r", "").strip().split("\t")
        if result.returncode or len(fields) != 2 or not fields[0].isdigit() or fields[1] != "0":
            return {}
        height = int(fields[0])
        if not 1 <= height <= 1000:
            return {}
        result = subprocess.run(["tmux", "capture-pane", "-p", "-t", pane,
                                 "-S", str(max(0, height - 8)), "-E", str(height - 1)],
                                capture_output=True, text=True, check=False, timeout=2,
                                stdin=subprocess.DEVNULL)
        if result.returncode or len(result.stdout) > 16384:
            return {}
        text = result.stdout
        return numeric_reading(text, provider)
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}


def numeric_reading(text, provider):
    # Only whole fields of the pane's last status lines count; chat text quoting "Context 5% left" does not.
    lines = [line for line in text.splitlines() if line.strip()][-2:]
    fields = [f.strip() for line in lines for f in re.split(r"\s[·|/]\s", line)]

    def found(pattern):
        hits = [re.fullmatch(pattern, f, re.I if provider == "codex" else 0) for f in fields]
        return [int(m.group(1)) for m in hits if m]

    if provider == "codex":
        context, weekly = found(r"Context\s+(\d{1,3})%\s+left"), found(r"weekly\s+(\d{1,3})%\s+left")
        reading = {"context_left": percent(context[-1]) if context else None,
                   "weekly_used": 100 - weekly[-1] if weekly and weekly[-1] <= 100 else None}
    else:
        context, weekly = found(r"ctx\s+\[[#-]+\]\s+(\d{1,3})%(?:\s.*)?"), found(r"7d\s+(\d{1,3})%(?:\s.*)?")
        used = percent(context[-1]) if context else None
        reading = {"context_left": None if used is None else 100 - used,
                   "weekly_used": percent(weekly[-1]) if weekly else None}
    return dict(reading, source="TUI") if any(v is not None for v in reading.values()) else {}


def collect(report, main_attempt=None, now=None, repo=None):
    from panel_chats import current_models, windows
    from room_view import nodes
    room = mapping(report.get("room"))
    mains = [m for m in nodes(report) if m.get("role") == "main"]
    panes = windows(repo, room["id"], room) if repo is not None and room.get("id") else {}
    now = time.time() if now is None else now
    current = current_models(repo, mains)
    output = {}
    for provider in ("claude", "codex"):
        candidates = [m for m in mains if m.get("provider") == provider]
        selected = [m for m in candidates if main_attempt and
                    (m["participant"] == main_attempt or m.get("attempt") == main_attempt)]
        if not selected:
            live = [m for m in candidates if m["state"] not in {"done", "failed", "cancelled", "timed_out", "abandoned"}]
            selected = live if live else candidates
        row = {"main_count": len(candidates), "context_left": None, "weekly_used": None,
               "source": "main ambiguous" if len(selected) > 1 else "unavailable"}
        if provider == "codex":
            row.update(native_models=current, native_model_checked=repo is not None)
        if len(selected) == 1:
            main = selected[0]
            native = current.get(main["participant"], {})
            bound_live = (provider == "codex" and repo is not None and main.get("consumer")
                          and main["state"] not in TERMINAL_STATES)
            row.update(participant=main["participant"],
                       model=native.get("model") if bound_live else main.get("model"))
            reading = cached(main.get("consumer"), now) if provider == "claude" else {}
            targets = panes.get(main["participant"], [])
            if len(targets) == 1:
                visible = pane_reading(targets[0], provider)
                if not reading or reading.get("source") == "cache stale":
                    reading = visible or reading
                else:
                    for key in ("weekly_used", "context_left"):
                        if reading.get(key) is None and visible.get(key) is not None:
                            reading[key] = visible[key]
                            reading["source"] = "HUD/TUI"
            row.update(reading)
            if main["state"] in {"done", "failed", "cancelled", "timed_out", "abandoned"}:
                row["context_left"] = None
        output[provider] = row
    return output


def header_rows(report, width=100):
    output = []
    for provider, title in (("claude", "Claude"), ("codex", "Codex")):
        row = mapping(mapping(report.get("provider_status")).get(provider))
        week, context = percent(row.get("weekly_used")), percent(row.get("context_left"))
        if width < 40:
            output.append("%s | W %s / C %s" % (title,
                "--" if week is None else "%s%%" % week,
                "--" if context is None else "%s%%" % context))
            continue
        text = "%s | week %s | main ctx %s" % (title,
            "--" if week is None else "%s%% used" % week,
            "--" if context is None else "%s%% left" % context)
        if width >= 68:
            text += " | " + (row.get("source") or "unavailable")
        output.append(text)
    return output
