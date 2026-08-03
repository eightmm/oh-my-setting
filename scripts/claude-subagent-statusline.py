#!/usr/bin/env python3
"""Render bounded Claude Code subagent rows from the JSON supplied on stdin."""

from __future__ import annotations

import datetime
import json
import math
import os
import re
import sys
import time
import unicodedata
from typing import Any, Dict, Optional


MAX_INPUT_BYTES = 1024 * 1024
MAX_TASKS = 128
DEFAULT_COLUMNS = 100


def mapping(value: Any) -> Dict[str, Any]:
    return value if isinstance(value, dict) else {}


def safe_text(value: Any, fallback: str, limit: int = 28) -> str:
    if not isinstance(value, str):
        return fallback
    value = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", value)
    cleaned = "".join(
        " " if unicodedata.category(char).startswith("C") else char
        for char in value
    )
    cleaned = " ".join(cleaned.split()).strip()
    if len(cleaned) > limit:
        cleaned = cleaned[: limit - 1].rstrip() + "…"
    return cleaned or fallback


def safe_id(value: Any) -> Optional[str]:
    if not isinstance(value, str) or not value or len(value) > 256:
        return None
    if any(unicodedata.category(char).startswith("C") for char in value):
        return None
    return value


def finite_number(
    value: Any, minimum: float = 0.0, maximum: float = 1_000_000_000_000.0
) -> Optional[float]:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number):
        return None
    return min(max(number, minimum), maximum)


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
    seconds = max(0, int(seconds))
    if seconds < 60:
        return "%ds" % seconds
    minutes, seconds = divmod(seconds, 60)
    if minutes < 60:
        return "%dm%02ds" % (minutes, seconds)
    hours, minutes = divmod(minutes, 60)
    if hours < 48:
        return "%dh%02dm" % (hours, minutes)
    days, hours = divmod(hours, 24)
    return "%dd%dh" % (days, hours)


def now_seconds() -> float:
    raw = os.environ.get("OMS_HUD_NOW", "").strip()
    if raw:
        try:
            value = float(raw)
            if math.isfinite(value):
                return value
        except ValueError:
            pass
    return time.time()


def start_seconds(value: Any) -> Optional[float]:
    number = finite_number(value, 0.0, 10_000_000_000_000.0)
    if number is not None:
        return number / 1000.0 if number > 10_000_000_000 else number
    if not isinstance(value, str) or len(value) > 80:
        return None
    candidate = value.strip()
    if candidate.endswith("Z"):
        candidate = candidate[:-1] + "+00:00"
    try:
        parsed = datetime.datetime.fromisoformat(candidate)
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=datetime.timezone.utc)
        return parsed.timestamp()
    except (ValueError, OverflowError, OSError):
        return None


def model_name(value: Any) -> str:
    model = safe_text(value, "", 40).lower()
    if not model:
        return ""
    if model.startswith("claude-"):
        model = model[len("claude-") :]
    model = re.sub(r"-\d{8}$", "", model)
    return safe_text(model, "", 20)


def effort_name(value: Any) -> str:
    if isinstance(value, str) and value in {"low", "medium", "high", "xhigh", "max"}:
        return value
    compact = compact_tokens(value)
    return (compact + " effort") if compact else ""


def status_name(value: Any) -> str:
    status = safe_text(value, "", 16).lower().replace("_", "-")
    aliases = {
        "completed": "done",
        "complete": "done",
        "errored": "error",
        "failed": "error",
        "interrupted": "stopped",
        "cancelled": "stopped",
        "canceled": "stopped",
    }
    return aliases.get(status, status)


def columns(value: Any) -> int:
    number = finite_number(value, 20.0, 500.0)
    return int(number) if number is not None else DEFAULT_COLUMNS


def truncate(text: str, width: int) -> str:
    if len(text) <= width:
        return text
    if width <= 1:
        return "…"[:width]
    return text[: width - 1].rstrip() + "…"


def render_task(task: Dict[str, Any], width: int, now: float) -> Optional[Dict[str, str]]:
    task_id = safe_id(task.get("id"))
    if task_id is None:
        return None

    label = safe_text(task.get("label"), "", 24)
    if not label:
        label = safe_text(task.get("name"), "", 24)
    if not label:
        label = safe_text(task.get("type"), "agent", 24)
    parts = [label]

    model = model_name(task.get("model"))
    effort = effort_name(task.get("effort"))
    if model or effort:
        parts.append("/".join(value for value in (model, effort) if value))

    used_number = finite_number(task.get("tokenCount"))
    capacity_number = finite_number(task.get("contextWindowSize"), 1.0)
    used = compact_tokens(used_number)
    capacity = compact_tokens(capacity_number)
    if used is not None:
        context = "ctx %s" % used
        if capacity is not None:
            percent = min(100, int(used_number * 100.0 / capacity_number + 0.5))
            context += "/%s %d%%" % (capacity, percent)
        parts.append(context)

    started = start_seconds(task.get("startTime"))
    if started is not None and 0 <= now - started <= 365 * 86400:
        parts.append(compact_duration(now - started))

    status = status_name(task.get("status"))
    if status:
        parts.append(status)

    return {"id": task_id, "content": truncate(" | ".join(parts), width)}


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

    width = columns(payload.get("columns"))
    now = now_seconds()
    tasks = payload.get("tasks")
    for value in tasks[:MAX_TASKS] if isinstance(tasks, list) else []:
        row = render_task(mapping(value), width, now)
        if row is not None:
            print(json.dumps(row, ensure_ascii=False, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
