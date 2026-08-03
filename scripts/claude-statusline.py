#!/usr/bin/env python3
"""Render a bounded Claude Code status line from the JSON supplied on stdin."""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import time
import unicodedata
from typing import Any, Dict, Optional


MAX_INPUT_BYTES = 256 * 1024
BAR_WIDTH = 10
RESET = "\033[0m"
GIT_CACHE_SECONDS = 5.0
MAX_GIT_OUTPUT_CHARS = 512 * 1024


def mapping(value: Any) -> Dict[str, Any]:
    return value if isinstance(value, dict) else {}


def safe_text(value: Any, fallback: str, limit: int = 24) -> str:
    if not isinstance(value, str):
        return fallback
    cleaned = "".join(
        " " if unicodedata.category(char).startswith("C") else char
        for char in value
    )
    cleaned = " ".join(cleaned.split())[:limit].strip()
    return cleaned or fallback


def finite_number(
    value: Any, minimum: float = 0.0, maximum: float = 1_000_000_000_000.0
) -> Optional[float]:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number):
        return None
    return min(max(number, minimum), maximum)


def rounded_percent(value: Any) -> Optional[int]:
    number = finite_number(value, 0.0, 100.0)
    return None if number is None else int(number + 0.5)


def compact_tokens(value: Any) -> Optional[str]:
    number = finite_number(value)
    if number is None:
        return None
    if number >= 1_000_000_000:
        return "%gb" % (number / 1_000_000_000.0)
    if number >= 1_000_000:
        return "%gm" % (number / 1_000_000.0)
    if number >= 1_000:
        return "%dk" % int(number / 1_000.0 + 0.5)
    return str(int(number + 0.5))


def compact_duration(seconds: float) -> str:
    if seconds < 60:
        return "<1m"
    minutes = int(seconds // 60)
    if minutes < 60:
        return "%dm" % minutes
    hours, minutes = divmod(minutes, 60)
    if hours < 48:
        return "%dh%02dm" % (hours, minutes)
    days, hours = divmod(hours, 24)
    return "%dd%dh" % (days, hours)


def color_for(percent: int) -> str:
    if percent >= 80:
        return "\033[31m"
    if percent >= 60:
        return "\033[33m"
    return "\033[32m"


def colorize(text: str, percent: int, enabled: bool) -> str:
    return "%s%s%s" % (color_for(percent), text, RESET) if enabled else text


def fallback_input_tokens(context: Dict[str, Any]) -> Optional[float]:
    usage = mapping(context.get("current_usage"))
    values = [
        finite_number(usage.get("input_tokens")),
        finite_number(usage.get("cache_creation_input_tokens")),
        finite_number(usage.get("cache_read_input_tokens")),
    ]
    present = [value for value in values if value is not None]
    return sum(present) if present else None


def git_cache_path(payload: Dict[str, Any], current_dir: str) -> str:
    session_id = payload.get("session_id")
    identity = "%s\0%s" % (
        session_id if isinstance(session_id, str) else "",
        os.path.realpath(current_dir),
    )
    digest = hashlib.sha256(identity.encode("utf-8", "replace")).hexdigest()[:24]
    cache_dir = os.environ.get("OMS_HUD_CACHE_DIR", "").strip()
    if not cache_dir:
        cache_dir = os.path.join(tempfile.gettempdir(), "oh-my-setting-hud")
    return os.path.join(cache_dir, "git-%s.json" % digest)


def parse_git_status(output: str) -> Dict[str, Any]:
    lines = output.splitlines()
    if not lines or not lines[0].startswith("## "):
        return {}
    head = lines[0][3:].strip()
    for prefix in ("No commits yet on ", "Initial commit on "):
        if head.startswith(prefix):
            head = head[len(prefix) :]
            break
    if head.startswith("HEAD ") or head == "HEAD":
        branch = "detached"
    else:
        branch = head.split("...", 1)[0].split(" [", 1)[0]
    branch = safe_text(branch, "", 24)
    if not branch:
        return {}

    staged = modified = untracked = 0
    for line in lines[1:]:
        if line.startswith("??"):
            untracked += 1
            continue
        if len(line) < 2:
            continue
        if line[0] not in (" ", "?", "!"):
            staged += 1
        if line[1] not in (" ", "?", "!"):
            modified += 1
    return {
        "branch": branch,
        "staged": staged,
        "modified": modified,
        "untracked": untracked,
    }


def read_git_status(payload: Dict[str, Any], current_dir: Any) -> Dict[str, Any]:
    if not isinstance(current_dir, str) or not current_dir.strip():
        return {}
    cache_path = git_cache_path(payload, current_dir)
    now = time.time()
    try:
        if now - os.path.getmtime(cache_path) <= GIT_CACHE_SECONDS:
            with open(cache_path, encoding="utf-8") as fh:
                cached = json.load(fh)
            return cached if isinstance(cached, dict) else {}
    except (OSError, ValueError, TypeError):
        pass

    result: Dict[str, Any] = {}
    try:
        proc = subprocess.run(
            [
                "git",
                "status",
                "--porcelain=v1",
                "--branch",
                "--untracked-files=normal",
            ],
            cwd=current_dir,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            universal_newlines=True,
            timeout=2.0,
            check=False,
        )
        if proc.returncode == 0 and len(proc.stdout) <= MAX_GIT_OUTPUT_CHARS:
            result = parse_git_status(proc.stdout)
    except (OSError, subprocess.SubprocessError):
        pass

    try:
        cache_dir = os.path.dirname(cache_path)
        os.makedirs(cache_dir, mode=0o700, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=".git-", dir=cache_dir)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                json.dump(result, fh, sort_keys=True)
                fh.write("\n")
            os.replace(tmp, cache_path)
        except Exception:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError:
        pass
    return result


def render_git(payload: Dict[str, Any], current_dir: Any) -> str:
    state = read_git_status(payload, current_dir)
    branch = safe_text(state.get("branch"), "", 24)
    if not branch:
        return ""
    text = "git %s" % branch
    for key, prefix in (("staged", "+"), ("modified", "~"), ("untracked", "?")):
        value = finite_number(state.get(key), 0.0, 9999.0)
        if value:
            text += " %s%d" % (prefix, int(value))
    return text


def truncate_plain(text: str, width: int) -> str:
    if len(text) <= width:
        return text
    if width <= 1:
        return "…"[:width]
    return text[: width - 1].rstrip() + "…"


def render(payload: Dict[str, Any], color: bool) -> str:
    # Identity parts and metric parts render as one row when the terminal is
    # wide enough and split into two rows when it is not: Claude Code
    # truncates an overlong status line with an ellipsis, and the numbers on
    # the right are exactly what disappears first.
    identity: list = []
    metrics: list = []
    parts = identity

    model = mapping(payload.get("model"))
    model_name = safe_text(
        model.get("display_name"), safe_text(model.get("id"), "Claude")
    )
    parts.append(model_name)

    session_name = safe_text(payload.get("session_name"), "", 16)
    if session_name:
        parts.append(session_name)
    current_dir = mapping(payload.get("workspace")).get("current_dir")
    if isinstance(current_dir, str) and current_dir.strip():
        directory = safe_text(re.split(r"[/\\]", current_dir.rstrip("/\\"))[-1], "", 16)
        if directory:
            parts.append(directory)
    if payload.get("fast_mode") is True:
        parts.append("fast")
    git_text = render_git(payload, current_dir)
    if git_text:
        parts.append(git_text)
    parts = metrics

    context = mapping(payload.get("context_window"))
    used = rounded_percent(context.get("used_percentage"))
    if used is None:
        parts.append("ctx --")
    else:
        filled = min(BAR_WIDTH, int(used * BAR_WIDTH / 100))
        context_text = "ctx [%s%s] %d%%" % (
            "#" * filled,
            "-" * (BAR_WIDTH - filled),
            used,
        )
        input_tokens = finite_number(context.get("total_input_tokens"))
        if input_tokens is None:
            input_tokens = fallback_input_tokens(context)
        current = compact_tokens(input_tokens)
        capacity = compact_tokens(context.get("context_window_size"))
        if current is not None and capacity is not None:
            context_text += " %s/%s" % (current, capacity)
        parts.append(colorize(context_text, used, color))

    rate_limits = mapping(payload.get("rate_limits"))
    now: Optional[float] = None
    raw_now = os.environ.get("OMS_HUD_NOW", "").strip()
    if raw_now:
        try:
            now = float(raw_now)
        except ValueError:
            now = None
    if now is None or not math.isfinite(now):
        now = time.time()
    for key, label in (("five_hour", "5h"), ("seven_day", "7d")):
        window = mapping(rate_limits.get(key))
        rate = rounded_percent(window.get("used_percentage"))
        if rate is not None:
            rate_text = "%s %d%%" % (label, rate)
            resets_at = finite_number(window.get("resets_at"), 0.0, 4_000_000_000.0)
            if resets_at is not None:
                remaining = resets_at - now
                # A reset implausibly far out is bad data, not a countdown.
                if 0 < remaining <= 8 * 86400:
                    rate_text += " (%s)" % compact_duration(remaining)
            parts.append(colorize(rate_text, rate, color))

    # Cost and effort are short: they ride the identity row so the split
    # lines stay balanced instead of one long metrics row that still clips.
    parts = identity
    cost = finite_number(mapping(payload.get("cost")).get("total_cost_usd"))
    if cost is not None:
        parts.append("$%.2f" % cost)

    effort_value = safe_text(mapping(payload.get("effort")).get("level"), "", 8)
    if effort_value not in {"low", "medium", "high", "xhigh", "max"}:
        effort_value = ""
    thinking = mapping(payload.get("thinking")).get("enabled") is True
    if thinking:
        effort_value = effort_value + "+think" if effort_value else "think"
    if effort_value:
        parts.append(effort_value)

    single = " | ".join(identity + metrics)
    raw_columns = os.environ.get("COLUMNS", "").strip()
    # Claude Code sets COLUMNS to the terminal width before running the
    # script; without it, assume a conservative classic width.
    threshold = int(raw_columns) if raw_columns.isdigit() and int(raw_columns) > 0 else 80
    if len(re.sub(r"\x1b\[[0-9;]*m", "", single)) <= threshold:
        return single
    return "\n".join(
        (truncate_plain(" | ".join(identity), threshold), " | ".join(metrics))
    )


def main() -> int:
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        return 0
    try:
        payload = json.loads(raw)
    except (json.JSONDecodeError, UnicodeError):
        return 0
    if not isinstance(payload, dict):
        return 0

    use_color = "NO_COLOR" not in os.environ and os.environ.get("TERM") != "dumb"
    output = render(payload, use_color)
    if output:
        print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
