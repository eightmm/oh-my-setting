#!/usr/bin/env python3
"""Render a bounded Claude Code status line from the JSON supplied on stdin."""

from __future__ import annotations

import json
import math
import os
import sys
import unicodedata
from typing import Any, Dict, Optional


MAX_INPUT_BYTES = 256 * 1024
BAR_WIDTH = 10
RESET = "\033[0m"


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


def render(payload: Dict[str, Any], color: bool) -> str:
    model = mapping(payload.get("model"))
    model_name = safe_text(
        model.get("display_name"), safe_text(model.get("id"), "Claude")
    )
    parts = [model_name]

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
    for key, label in (("five_hour", "5h"), ("seven_day", "7d")):
        rate = rounded_percent(mapping(rate_limits.get(key)).get("used_percentage"))
        if rate is not None:
            parts.append(colorize("%s %d%%" % (label, rate), rate, color))

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

    return " | ".join(parts)


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
